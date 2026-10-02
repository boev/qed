use crate::attest::{Attestation, AttestationStore};
use crate::check::CheckResult;
use crate::discovery::{
    DurableBoardStore, FeaturedPool, FeaturedStatus, Leaderboard, PriceSnapshot, RegistrySnapshot,
};
use crate::pool::PoolReader;
use crate::registry::Registry;
use bytes::Bytes;
use chrono::{SecondsFormat, Utc};
use ed25519_dalek::SigningKey;
use moka::future::Cache;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock as StdRwLock, Weak};
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

pub struct AdminAuth {
    configured: bool,
    username: Box<[u8]>,
    password: Box<[u8]>,
}

const MAX_ADMIN_CREDENTIAL_BYTES: usize = 256;

impl AdminAuth {
    pub fn new(username: Option<&str>, password: Option<&str>) -> Self {
        let (Some(username), Some(password)) =
            (username.filter(|value| !value.is_empty()), password.filter(|value| !value.is_empty()))
        else {
            return Self {
                configured: false,
                username: Box::new([]),
                password: Box::new([]),
            };
        };
        if username.len() > MAX_ADMIN_CREDENTIAL_BYTES
            || password.len() > MAX_ADMIN_CREDENTIAL_BYTES
            || username.as_bytes().contains(&b':')
        {
            return Self {
                configured: false,
                username: Box::new([]),
                password: Box::new([]),
            };
        }
        Self {
            configured: true,
            username: username.as_bytes().to_vec().into_boxed_slice(),
            password: password.as_bytes().to_vec().into_boxed_slice(),
        }
    }

    pub(crate) fn matches(&self, username: &[u8], password: &[u8]) -> bool {
        let username_matches = constant_time_eq(&self.username, username);
        let password_matches = constant_time_eq(&self.password, password);
        self.configured && username_matches && password_matches
    }
}

fn constant_time_eq(expected: &[u8], candidate: &[u8]) -> bool {
    let mut difference = expected.len() ^ candidate.len();
    for index in 0..MAX_ADMIN_CREDENTIAL_BYTES {
        difference |= usize::from(
            expected.get(index).copied().unwrap_or_default()
                ^ candidate.get(index).copied().unwrap_or_default(),
        );
    }
    difference == 0
}
pub struct UsageStats {
    started_at: String,
    started: Instant,
    total_requests: AtomicU64,
    html_page_views: AtomicU64,
    api_requests: AtomicU64,
    checks: AtomicU64,
    wallet_requests: AtomicU64,
    admin_requests: AtomicU64,
    health_requests: AtomicU64,
    static_asset_requests: AtomicU64,
    responses_2xx: AtomicU64,
    responses_3xx: AtomicU64,
    responses_4xx: AtomicU64,
    responses_5xx: AtomicU64,
}
#[derive(Debug, Serialize)]
pub struct UsageSnapshot {
    pub scope: &'static str,
    pub started_at: String,
    pub uptime_seconds: u64,
    pub total_requests: u64,
    pub html_page_views: u64,
    pub api_requests: u64,
    pub checks: u64,
    pub wallet_requests: u64,
    pub admin_requests: u64,
    pub health_requests: u64,
    pub static_asset_requests: u64,
    pub responses: UsageResponseCounts,
}

#[derive(Debug, Serialize)]
pub struct UsageResponseCounts {
    #[serde(rename = "2xx")]
    pub class_2xx: u64,
    #[serde(rename = "3xx")]
    pub class_3xx: u64,
    #[serde(rename = "4xx")]
    pub class_4xx: u64,
    #[serde(rename = "5xx")]
    pub class_5xx: u64,
}

impl UsageStats {
    pub fn new() -> Self {
        Self {
            started_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
            started: Instant::now(),
            total_requests: AtomicU64::new(0),
            html_page_views: AtomicU64::new(0),
            api_requests: AtomicU64::new(0),
            checks: AtomicU64::new(0),
            wallet_requests: AtomicU64::new(0),
            admin_requests: AtomicU64::new(0),
            health_requests: AtomicU64::new(0),
            static_asset_requests: AtomicU64::new(0),
            responses_2xx: AtomicU64::new(0),
            responses_3xx: AtomicU64::new(0),
            responses_4xx: AtomicU64::new(0),
            responses_5xx: AtomicU64::new(0),
        }
    }
    pub(crate) fn record_request(
        &self,
        api: bool,
        check: bool,
        wallet: bool,
        admin: bool,
        health: bool,
        static_asset: bool,
    ) {
        self.total_requests.fetch_add(1, Ordering::Relaxed);
        if api {
            self.api_requests.fetch_add(1, Ordering::Relaxed);
        }
        if check {
            self.checks.fetch_add(1, Ordering::Relaxed);
        }
        if wallet {
            self.wallet_requests.fetch_add(1, Ordering::Relaxed);
        }
        if admin {
            self.admin_requests.fetch_add(1, Ordering::Relaxed);
        }
        if health {
            self.health_requests.fetch_add(1, Ordering::Relaxed);
        }
        if static_asset {
            self.static_asset_requests.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub(crate) fn record_mcp_tool(&self, name: &str) {
        if matches!(name, "qed_check" | "qed_powers" | "qed_verify") {
            self.checks.fetch_add(1, Ordering::Relaxed);
        } else if name == "qed_wallet" {
            self.wallet_requests.fetch_add(1, Ordering::Relaxed);
        }
    }
    pub(crate) fn record_html_page_view(&self) {
        self.html_page_views.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_response(&self, status: u16) {
        match status / 100 {
            2 => self.responses_2xx.fetch_add(1, Ordering::Relaxed),
            3 => self.responses_3xx.fetch_add(1, Ordering::Relaxed),
            4 => self.responses_4xx.fetch_add(1, Ordering::Relaxed),
            5 => self.responses_5xx.fetch_add(1, Ordering::Relaxed),
            _ => 0,
        };
    }

    pub(crate) fn snapshot(&self) -> UsageSnapshot {
        UsageSnapshot {
            scope: "instance",
            started_at: self.started_at.clone(),
            uptime_seconds: self.started.elapsed().as_secs(),
            total_requests: self.total_requests.load(Ordering::Relaxed),
            html_page_views: self.html_page_views.load(Ordering::Relaxed),
            api_requests: self.api_requests.load(Ordering::Relaxed),
            checks: self.checks.load(Ordering::Relaxed),
            wallet_requests: self.wallet_requests.load(Ordering::Relaxed),
            admin_requests: self.admin_requests.load(Ordering::Relaxed),
            health_requests: self.health_requests.load(Ordering::Relaxed),
            static_asset_requests: self.static_asset_requests.load(Ordering::Relaxed),
            responses: UsageResponseCounts {
                class_2xx: self.responses_2xx.load(Ordering::Relaxed),
                class_3xx: self.responses_3xx.load(Ordering::Relaxed),
                class_4xx: self.responses_4xx.load(Ordering::Relaxed),
                class_5xx: self.responses_5xx.load(Ordering::Relaxed),
            },
        }
    }
}

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
    pub source_http: reqwest::Client,
    pub check_cache: Cache<String, CachedCheckResult>,
    pub powers_cache: Cache<PowersCacheKey, crate::powers::PowersRecord>,
    pub powers_retry_cache: Cache<PowersCacheKey, crate::powers::PowersRecord>,
    pub powers_failure_cache: Cache<PowersCacheKey, ()>,
    pub powers_locks: Cache<PowersCacheKey, Arc<tokio::sync::Mutex<()>>>,
    pub powers_prefetching: Arc<tokio::sync::Mutex<HashSet<PowersCacheKey>>>,
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
    pub admin_auth: Arc<AdminAuth>,
    pub usage_stats: Arc<UsageStats>,
    pub rate_limiter: Arc<RateLimiter>,
    pub expensive_concurrency: Arc<tokio::sync::Semaphore>,
    pub powers_prefetch_concurrency: Arc<tokio::sync::Semaphore>,
    pub wallet_concurrency: Arc<tokio::sync::Semaphore>,
    pub registry_api_concurrency: Arc<tokio::sync::Semaphore>,
}

pub type PowersCacheKey = (crate::chain::Chain, String, u64);

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
    #[test]
    fn admin_auth_rejects_missing_or_wrong_credentials_and_accepts_exact_pair() {
        let unavailable = AdminAuth::new(None, Some("ignored"));
        assert!(!unavailable.matches(b"admin", b"ignored"));

        let auth = AdminAuth::new(Some("admin"), Some("replacement-only"));
        assert!(auth.matches(b"admin", b"replacement-only"));
        assert!(!auth.matches(b"admin", b"wrong"));
        assert!(!auth.matches(b"other", b"replacement-only"));
        assert!(!AdminAuth::new(Some("ad:min"), Some("replacement-only"))
            .matches(b"ad:min", b"replacement-only"));
        assert!(!AdminAuth::new(Some(&"x".repeat(257)), Some("replacement-only"))
            .matches(&vec![b'x'; 257], b"replacement-only"));
    }

    #[test]
    fn usage_stats_are_bounded_and_reset_with_a_new_instance() {
        let stats = UsageStats::new();
        stats.record_request(true, true, false, false, false, false);
        stats.record_request(true, false, true, false, false, false);
        stats.record_request(false, false, false, true, false, false);
        stats.record_request(false, false, false, false, true, false);
        stats.record_request(false, false, false, false, false, true);
        stats.record_html_page_view();
        stats.record_response(200);
        stats.record_response(302);
        stats.record_response(404);
        stats.record_response(503);

        let snapshot = stats.snapshot();
        assert_eq!(snapshot.total_requests, 5);
        assert_eq!(snapshot.scope, "instance");
        assert_eq!(snapshot.html_page_views, 1);
        assert_eq!(snapshot.api_requests, 2);
        assert_eq!(snapshot.checks, 1);
        assert_eq!(snapshot.wallet_requests, 1);
        assert_eq!(snapshot.health_requests, 1);
        assert_eq!(snapshot.static_asset_requests, 1);
        assert_eq!(snapshot.admin_requests, 1);
        assert_eq!(snapshot.responses.class_2xx, 1);
        assert_eq!(snapshot.responses.class_3xx, 1);
        assert_eq!(snapshot.responses.class_4xx, 1);
        assert_eq!(snapshot.responses.class_5xx, 1);
        assert!(!snapshot.started_at.is_empty());

        let reset = UsageStats::new().snapshot();
        assert_eq!(reset.total_requests, 0);
        assert_eq!(reset.html_page_views, 0);
        assert_eq!(reset.admin_requests, 0);
    }
}
