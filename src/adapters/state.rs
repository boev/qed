#[cfg(test)]
use crate::ports::ChainReader;
use crate::{
    adapters::{
        discovery::{
            DurableBoardStore, FeaturedPool, FeaturedStatus, Leaderboard, PriceSnapshot,
            RegistrySnapshot,
        },
        registry::RegistryEndpoints,
    },
    app::context::Context,
    domain::{chain::Chain, check::CheckResult, registry::Registry},
    ports::Cache as CachePort,
};

#[cfg(test)]
use crate::app::context::PowersCacheKey;

use async_trait::async_trait;
use bytes::Bytes;
use chrono::{SecondsFormat, Utc};
#[cfg(test)]
use ed25519_dalek::SigningKey;
use moka::future::Cache;
use serde::Serialize;
use std::collections::HashMap;
#[cfg(test)]
use std::collections::HashSet;
use std::hash::Hash;
use std::net::IpAddr;
use std::path::PathBuf;
#[cfg(test)]
use std::sync::RwLock as StdRwLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{Notify, RwLock};

pub struct AdminAuth {
    configured: bool,
    username: Box<[u8]>,
    password: Box<[u8]>,
}

const MAX_ADMIN_CREDENTIAL_BYTES: usize = 256;

impl AdminAuth {
    pub fn new(username: Option<&str>, password: Option<&str>) -> Self {
        let (Some(username), Some(password)) = (
            username.filter(|value| !value.is_empty()),
            password.filter(|value| !value.is_empty()),
        ) else {
            return Self { configured: false, username: Box::new([]), password: Box::new([]) };
        };
        if username.len() > MAX_ADMIN_CREDENTIAL_BYTES
            || password.len() > MAX_ADMIN_CREDENTIAL_BYTES
            || username.as_bytes().contains(&b':')
        {
            return Self { configured: false, username: Box::new([]), password: Box::new([]) };
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
        if matches!(name, "qed_check" | "qed_guard" | "qed_powers" | "qed_verify") {
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

#[derive(Clone)]
pub struct RegistryApiCache {
    pub registry_hash: String,
    pub body: Arc<Bytes>,
}

#[derive(Clone)]
pub struct AppState {
    pub app: Arc<Context>,
    pub registry: Arc<RwLock<Arc<Registry>>>,
    pub registry_status: Arc<RwLock<RegistrySnapshot>>,
    pub registry_endpoints: Arc<RegistryEndpoints>,
    pub registry_path: Arc<PathBuf>,
    pub powers_warm_notify: Arc<Notify>,
    pub http: reqwest::Client,
    pub featured: Arc<RwLock<Vec<FeaturedPool>>>,
    pub featured_status: Arc<RwLock<FeaturedStatus>>,
    pub leaderboard: Arc<RwLock<Leaderboard>>,
    pub prices: Arc<RwLock<PriceSnapshot>>,
    pub leaderboard_check_cache: Cache<(Chain, String), CheckResult>,
    pub board_store: Arc<DurableBoardStore>,
    pub registry_api_cache: Arc<RwLock<Option<RegistryApiCache>>>,
    pub public_url: Arc<String>,
    pub admin_auth: Arc<AdminAuth>,
    pub usage_stats: Arc<UsageStats>,
    pub rate_limiter: Arc<RateLimiter>,
    pub expensive_concurrency: Arc<tokio::sync::Semaphore>,
    pub wallet_concurrency: Arc<tokio::sync::Semaphore>,
    pub registry_api_concurrency: Arc<tokio::sync::Semaphore>,
}

#[cfg(test)]
impl AppState {
    pub(crate) fn for_tests(
        entries: Registry,
        readers: Vec<Box<dyn ChainReader>>,
        dev_signer: bool,
    ) -> Self {
        use crate::{
            adapters::{attestation::Ed25519Signer, discovery::DiscoveryPoolIndex},
            ports::{Clock, IssuerRegistry},
        };

        let registry = Arc::new(RwLock::new(Arc::new(entries)));
        let readers = Arc::new(readers);
        let http = reqwest::Client::new();
        let check_cache = Cache::builder().max_capacity(10_000).build();
        let statement_cache = Cache::builder()
            .max_capacity(10_000)
            .time_to_live(std::time::Duration::from_secs(24 * 60 * 60))
            .weigher(|_, statement: &crate::domain::statement::Statement| {
                u32::try_from(statement.assets.len().saturating_add(1)).unwrap_or(u32::MAX)
            })
            .build();
        let powers_cache = Cache::builder()
            .max_capacity(10_000)
            .time_to_live(crate::app::warm::POWERS_CACHE_TTL)
            .build();
        let powers_retry_cache = Cache::builder().max_capacity(10_000).build();
        let powers_failure_cache = Cache::builder().max_capacity(10_000).build();
        let powers_locks = Cache::builder().max_capacity(10_000).build();
        let powers_prefetching =
            Arc::new(tokio::sync::Mutex::new(HashSet::<PowersCacheKey>::new()));
        let leaderboard_check_cache = Cache::builder().max_capacity(10_000).build();
        let check_inflight = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
        let featured = Arc::new(RwLock::new(Vec::new()));
        let featured_status = Arc::new(RwLock::new(FeaturedStatus::default()));
        let leaderboard = Arc::new(RwLock::new(Leaderboard::default()));
        let prices = Arc::new(RwLock::new(PriceSnapshot::default()));
        let attestations = Arc::new(StdRwLock::new(HashMap::new()));
        let signing_key = Arc::new(SigningKey::from_bytes(&[7; 32]));
        let attest_store =
            Arc::new(crate::app::context::test_support::MemoryAttestationStore::default());
        let registry_hash = Arc::new(StdRwLock::new(String::new()));
        let registry_version = Arc::new(AtomicU64::new(0));
        let powers_prefetch_concurrency = Arc::new(tokio::sync::Semaphore::new(2));
        let clock: Arc<dyn Clock> = Arc::new(crate::SystemClock);
        let signer = Arc::new(Ed25519Signer::new(Arc::clone(&signing_key)));
        let trusted_signers = HashSet::new();
        let issuer_registry: Arc<dyn IssuerRegistry> = Arc::new(Arc::clone(&registry));
        let app = Arc::new(Context {
            registry: issuer_registry,
            readers,
            check_cache: Arc::new(check_cache.clone()),
            statement_cache: Arc::new(statement_cache.clone()),
            powers_cache: Arc::new(powers_cache.clone()),
            powers_retry_cache: Arc::new(powers_retry_cache.clone()),
            powers_failure_cache: Arc::new(powers_failure_cache.clone()),
            powers_locks: Arc::new(powers_locks.clone()),
            powers_prefetching,
            check_inflight,
            pool_index: Arc::new(DiscoveryPoolIndex::new(
                Arc::clone(&featured),
                Arc::clone(&leaderboard),
            )),
            attestations,
            signer,
            trusted_signers: Arc::new(trusted_signers),
            dev_signer,
            attest_store,
            registry_hash,
            registry_version,
            source_verifier: Arc::new(crate::app::context::test_support::TestSourceVerifier),
            clock,
            powers_prefetch_concurrency,
        });
        Self {
            app,
            registry,
            registry_status: Arc::new(RwLock::new(RegistrySnapshot::default())),
            registry_endpoints: Arc::new(RegistryEndpoints {
                xstocks: String::new(),
                ondo: String::new(),
                ondo_api_key: None,
                robinhood: String::new(),
            }),
            registry_path: Arc::new(PathBuf::new()),
            powers_warm_notify: Arc::new(Notify::new()),
            http,
            featured,
            featured_status,
            leaderboard,
            prices,
            leaderboard_check_cache,
            board_store: Arc::new(DurableBoardStore::default()),
            registry_api_cache: Arc::new(RwLock::new(None)),
            public_url: Arc::new("http://localhost:3000".to_owned()),
            admin_auth: Arc::new(AdminAuth::new(Some("test-admin"), Some("test-password"))),
            usage_stats: Arc::new(UsageStats::new()),
            rate_limiter: Arc::new(RateLimiter::default()),
            expensive_concurrency: Arc::new(tokio::sync::Semaphore::new(32)),
            wallet_concurrency: Arc::new(tokio::sync::Semaphore::new(1)),
            registry_api_concurrency: Arc::new(tokio::sync::Semaphore::new(4)),
        }
    }
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

#[async_trait]
impl<K, V> CachePort<K, V> for moka::future::Cache<K, V>
where
    K: Clone + Eq + Hash + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    async fn get(&self, key: &K) -> Option<V> {
        moka::future::Cache::get(self, key).await
    }

    async fn insert(&self, key: K, value: V) {
        moka::future::Cache::insert(self, key, value).await;
    }

    async fn invalidate(&self, key: &K) {
        moka::future::Cache::invalidate(self, key).await;
    }
    fn invalidate_all(&self) {
        moka::future::Cache::invalidate_all(self);
    }

    async fn get_or_insert(&self, key: K, value: V) -> V {
        moka::future::Cache::get_with(self, key, async { value }).await
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn usage_stats_count_guard_mcp_calls_as_checks() {
        let stats = UsageStats::new();
        stats.record_mcp_tool("qed_guard");
        stats.record_mcp_tool("qed_wallet");

        let snapshot = stats.snapshot();
        assert_eq!(snapshot.checks, 1);
        assert_eq!(snapshot.wallet_requests, 1);
    }
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
        assert!(
            !AdminAuth::new(Some("ad:min"), Some("replacement-only"))
                .matches(b"ad:min", b"replacement-only")
        );
        assert!(
            !AdminAuth::new(Some(&"x".repeat(257)), Some("replacement-only"))
                .matches(&vec![b'x'; 257], b"replacement-only")
        );
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
