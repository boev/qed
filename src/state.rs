use crate::attest::{Attestation, AttestationStore};
use crate::check::CheckResult;
use crate::discovery::{
    DurableBoardStore, FeaturedPool, FeaturedStatus, Leaderboard, PriceSnapshot, RegistrySnapshot,
};
use crate::pool::PoolReader;
use crate::registry::Registry;
use bytes::Bytes;
use ed25519_dalek::SigningKey;
use moka::future::Cache;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock as StdRwLock, Weak};
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

#[derive(Debug)]
pub struct RpcRateLimiter {
    rate: f64,
    state: Mutex<(Instant, f64)>,
}

impl RpcRateLimiter {
    pub fn new(requests_per_second: u32) -> Self {
        let rate = f64::from(requests_per_second.max(1));
        Self { rate, state: Mutex::new((Instant::now(), 1.0)) }
    }

    pub async fn acquire(&self) {
        loop {
            let wait = {
                let mut state = match self.state.lock() {
                    Ok(state) => state,
                    Err(poisoned) => poisoned.into_inner(),
                };
                let now = Instant::now();
                let elapsed = now.duration_since(state.0).as_secs_f64();
                state.0 = now;
                state.1 = (state.1 + elapsed * self.rate).min(1.0);
                if state.1 >= 1.0 {
                    state.1 -= 1.0;
                    None
                } else {
                    Some(Duration::from_secs_f64((1.0 - state.1) / self.rate))
                }
            };
            let Some(wait) = wait else { return };
            tokio::time::sleep(wait).await;
        }
    }
}

#[derive(Clone)]
pub struct RpcBuckets {
    pub solana: Arc<RpcRateLimiter>,
    pub robinhood: Arc<RpcRateLimiter>,
    pub base: Arc<RpcRateLimiter>,
    pub ethereum: Arc<RpcRateLimiter>,
    pub bnb: Arc<RpcRateLimiter>,
}

impl RpcBuckets {
    pub fn new(solana_rps: u32, evm_rps: u32) -> Self {
        Self {
            solana: Arc::new(RpcRateLimiter::new(solana_rps)),
            robinhood: Arc::new(RpcRateLimiter::new(evm_rps)),
            base: Arc::new(RpcRateLimiter::new(evm_rps)),
            ethereum: Arc::new(RpcRateLimiter::new(evm_rps)),
            bnb: Arc::new(RpcRateLimiter::new(evm_rps)),
        }
    }
}

static REGISTRY_VERSION: AtomicU64 = AtomicU64::new(0);

#[derive(Clone)]
pub struct CachedCheckResult {
    pub registry_version: u64,
    pub result: CheckResult,
}

pub(crate) fn current_registry_version() -> u64 {
    REGISTRY_VERSION.load(Ordering::Acquire)
}

pub(crate) fn advance_registry_version() {
    REGISTRY_VERSION.fetch_add(1, Ordering::AcqRel);
}

#[derive(Clone)]
pub struct RegistryApiCache {
    pub registry_hash: String,
    pub body: Arc<Bytes>,
}

pub type CheckFlight = Weak<tokio::sync::watch::Sender<Option<CheckResult>>>;
pub type CheckInFlight = tokio::sync::Mutex<HashMap<String, CheckFlight>>;

#[derive(Clone)]
pub struct AppState {
    pub registry: Arc<RwLock<Registry>>,
    pub registry_status: Arc<RwLock<RegistrySnapshot>>,
    pub readers: Arc<Vec<Box<dyn PoolReader>>>,
    pub http: reqwest::Client,
    pub check_cache: Cache<String, CachedCheckResult>,
    pub leaderboard_check_cache: Cache<(crate::chain::Chain, String), CheckResult>,
    pub check_inflight: Arc<CheckInFlight>,
    pub featured: Arc<RwLock<Vec<FeaturedPool>>>,
    pub featured_status: Arc<RwLock<FeaturedStatus>>,
    pub leaderboard: Arc<RwLock<Leaderboard>>,
    pub prices: Arc<RwLock<PriceSnapshot>>,
    pub attestations: Arc<StdRwLock<HashMap<String, Attestation>>>,
    pub signing_key: Arc<SigningKey>,
    pub dev_signer: bool,
    pub attest_store: Arc<dyn AttestationStore>,
    pub board_store: Arc<DurableBoardStore>,
    pub registry_hash: Arc<StdRwLock<String>>,
    pub registry_api_cache: Arc<RwLock<Option<RegistryApiCache>>>,
    pub public_url: Arc<String>,
    pub rate_limiter: Arc<RateLimiter>,
    pub expensive_concurrency: Arc<tokio::sync::Semaphore>,
    pub wallet_concurrency: Arc<tokio::sync::Semaphore>,
    pub registry_api_concurrency: Arc<tokio::sync::Semaphore>,
}

const REQUEST_WINDOW: Duration = Duration::from_secs(60);
const REQUEST_PRUNE_INTERVAL: Duration = Duration::from_secs(10);
const MAX_REQUEST_ENTRIES: usize = 10_000;

pub struct RateLimiter {
    requests: Arc<Mutex<HashMap<IpAddr, (Instant, u32)>>>,
    rpc_buckets: Arc<RpcBuckets>,
    pruner_started: AtomicBool,
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::with_rpc_rps(4, 8)
    }
}

impl RateLimiter {
    pub fn with_rpc_rps(solana_rps: u32, evm_rps: u32) -> Self {
        Self {
            requests: Arc::new(Mutex::new(HashMap::new())),
            rpc_buckets: Arc::new(RpcBuckets::new(solana_rps, evm_rps)),
            pruner_started: AtomicBool::new(false),
        }
    }

    pub fn rpc_buckets(&self) -> Arc<RpcBuckets> {
        Arc::clone(&self.rpc_buckets)
    }

    fn start_pruner(&self) {
        if tokio::runtime::Handle::try_current().is_err()
            || self
                .pruner_started
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return;
        }
        let requests = Arc::clone(&self.requests);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(REQUEST_PRUNE_INTERVAL);
            loop {
                interval.tick().await;
                let now = Instant::now();
                let mut requests = match requests.lock() {
                    Ok(requests) => requests,
                    Err(poisoned) => poisoned.into_inner(),
                };
                requests.retain(|_, (started, _)| now.duration_since(*started) < REQUEST_WINDOW);
            }
        });
    }

    #[cfg(test)]
    fn prune_expired_at(&self, now: Instant) {
        let mut requests = match self.requests.lock() {
            Ok(requests) => requests,
            Err(poisoned) => poisoned.into_inner(),
        };
        requests.retain(|_, (started, _)| now.duration_since(*started) < REQUEST_WINDOW);
    }

    pub fn allow(&self, ip: IpAddr) -> bool {
        self.start_pruner();
        let now = Instant::now();
        let mut requests = match self.requests.lock() {
            Ok(requests) => requests,
            Err(poisoned) => poisoned.into_inner(),
        };
        requests.retain(|_, (started, _)| now.duration_since(*started) < REQUEST_WINDOW);
        if requests.len() >= MAX_REQUEST_ENTRIES
            && !requests.contains_key(&ip)
            && let Some(oldest) = requests
                .iter()
                .min_by_key(|(_, (started, _))| *started)
                .map(|(address, _)| *address)
        {
            requests.remove(&oldest);
        }
        let entry = requests.entry(ip).or_insert((now, 0));
        if now.duration_since(entry.0) >= REQUEST_WINDOW {
            *entry = (now, 0);
        }
        if entry.1 >= 60 {
            return false;
        }
        entry.1 += 1;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rate_limit_prune_removes_expired_ips_without_new_traffic() {
        let limiter = RateLimiter::default();
        let now = Instant::now();
        let expired: IpAddr = "192.0.2.1".parse().unwrap();
        let fresh: IpAddr = "192.0.2.2".parse().unwrap();
        {
            let mut requests = limiter.requests.lock().unwrap();
            requests.insert(expired, (now - REQUEST_WINDOW - Duration::from_secs(1), 1));
            requests.insert(fresh, (now, 1));
        }

        limiter.prune_expired_at(now);

        let requests = limiter.requests.lock().unwrap();
        assert!(!requests.contains_key(&expired));
        assert!(requests.contains_key(&fresh));
    }
}
