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
    ports::{
        Cache as CachePort, RPC_BUDGET_REJECTED, RPC_CRAWLER_BLOCKED, current_rpc_request_context,
    },
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
#[cfg(test)]
use std::collections::HashSet;
use std::collections::{BTreeMap, HashMap};
use std::hash::Hash;
use std::net::IpAddr;
use std::path::PathBuf;
#[cfg(test)]
use std::sync::RwLock as StdRwLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{Notify, RwLock};

const DEFAULT_RPC_DAILY_CU_LIMIT: u64 = 1_500_000;
const DEFAULT_RPC_DAILY_CU_LIMIT_ON_DEMAND: u64 = 500_000;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct RpcMethodUsage {
    pub(crate) chain: String,
    pub(crate) method: String,
    pub(crate) calls: u64,
    pub(crate) est_cu: u64,
    pub(crate) on_demand_calls: u64,
    pub(crate) on_demand_est_cu: u64,
    pub(crate) background_calls: u64,
    pub(crate) background_est_cu: u64,
    pub(crate) budget_rejections: u64,
}

#[derive(Debug, Serialize)]
pub(crate) struct RpcUsageSnapshot {
    pub(crate) day: String,
    pub(crate) total_limit_cu: u64,
    pub(crate) on_demand_limit_cu: u64,
    pub(crate) est_cu_total: u64,
    pub(crate) on_demand: u64,
    pub(crate) background: u64,
    pub(crate) rejected: u64,
    pub(crate) by_chain_method: Vec<RpcMethodUsage>,
}

#[derive(Default)]
struct RpcMethodCounter {
    calls: u64,
    est_cu: u64,
    on_demand_calls: u64,
    on_demand_est_cu: u64,
    background_calls: u64,
    background_est_cu: u64,
    budget_rejections: u64,
}

struct RpcBudgetState {
    day: String,
    est_cu_total: u64,
    on_demand: u64,
    background: u64,
    rejected: u64,
    warned_for_day: bool,
    by_chain_method: BTreeMap<(String, String), RpcMethodCounter>,
}

pub struct RpcBudget {
    total_limit_cu: u64,
    on_demand_limit_cu: u64,
    state: Mutex<RpcBudgetState>,
}

impl RpcBudget {
    pub fn new(total_limit_cu: u64, on_demand_limit_cu: u64) -> Self {
        Self {
            total_limit_cu,
            on_demand_limit_cu,
            state: Mutex::new(RpcBudgetState {
                day: Utc::now().date_naive().to_string(),
                est_cu_total: 0,
                on_demand: 0,
                background: 0,
                rejected: 0,
                warned_for_day: false,
                by_chain_method: BTreeMap::new(),
            }),
        }
    }

    pub fn debit_methods<'a, I>(&self, chain: Chain, methods: I) -> bool
    where
        I: Iterator<Item = &'a str> + Clone,
    {
        let context = current_rpc_request_context();
        if let Some(context) = &context
            && context.crawler
        {
            context.mark(RPC_CRAWLER_BLOCKED);
            return false;
        }
        let on_demand = context.as_ref().is_some_and(|context| context.on_demand);
        let estimated_cu = methods.clone().map(alchemy_cu_weight).sum::<u64>();
        if estimated_cu == 0 {
            return true;
        }
        let method_count = methods.clone().count() as u64;
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let day = Utc::now().date_naive().to_string();
        if state.day != day {
            *state = RpcBudgetState {
                day,
                est_cu_total: 0,
                on_demand: 0,
                background: 0,
                rejected: 0,
                warned_for_day: false,
                by_chain_method: BTreeMap::new(),
            };
        }

        let total_exceeded = state.est_cu_total.saturating_add(estimated_cu) > self.total_limit_cu;
        let on_demand_exceeded =
            on_demand && state.on_demand.saturating_add(estimated_cu) > self.on_demand_limit_cu;
        if total_exceeded || on_demand_exceeded {
            state.rejected = state.rejected.saturating_add(method_count);
            for method in methods {
                state
                    .by_chain_method
                    .entry((chain.to_string(), method.to_owned()))
                    .or_default()
                    .budget_rejections += 1;
            }
            if !on_demand && !state.warned_for_day {
                state.warned_for_day = true;
                tracing::warn!(
                    day = %state.day,
                    total_limit_cu = self.total_limit_cu,
                    on_demand_limit_cu = self.on_demand_limit_cu,
                    "daily RPC read budget exhausted; background reads are stopped for the day"
                );
            }
            if let Some(context) = context {
                context.mark(RPC_BUDGET_REJECTED);
            }
            return false;
        }

        state.est_cu_total = state.est_cu_total.saturating_add(estimated_cu);
        if on_demand {
            state.on_demand = state.on_demand.saturating_add(estimated_cu);
        } else {
            state.background = state.background.saturating_add(estimated_cu);
        }
        for method in methods {
            let cost = alchemy_cu_weight(method);
            let counter =
                state.by_chain_method.entry((chain.to_string(), method.to_owned())).or_default();
            counter.calls += 1;
            counter.est_cu = counter.est_cu.saturating_add(cost);
            if on_demand {
                counter.on_demand_calls += 1;
                counter.on_demand_est_cu = counter.on_demand_est_cu.saturating_add(cost);
            } else {
                counter.background_calls += 1;
                counter.background_est_cu = counter.background_est_cu.saturating_add(cost);
            }
        }
        true
    }

    pub(crate) fn snapshot(&self) -> RpcUsageSnapshot {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let day = Utc::now().date_naive().to_string();
        if state.day != day {
            *state = RpcBudgetState {
                day,
                est_cu_total: 0,
                on_demand: 0,
                background: 0,
                rejected: 0,
                warned_for_day: false,
                by_chain_method: BTreeMap::new(),
            };
        }
        RpcUsageSnapshot {
            day: state.day.clone(),
            total_limit_cu: self.total_limit_cu,
            on_demand_limit_cu: self.on_demand_limit_cu,
            est_cu_total: state.est_cu_total,
            on_demand: state.on_demand,
            background: state.background,
            rejected: state.rejected,
            by_chain_method: state
                .by_chain_method
                .iter()
                .map(|((chain, method), counts)| RpcMethodUsage {
                    chain: chain.clone(),
                    method: method.clone(),
                    calls: counts.calls,
                    est_cu: counts.est_cu,
                    on_demand_calls: counts.on_demand_calls,
                    on_demand_est_cu: counts.on_demand_est_cu,
                    background_calls: counts.background_calls,
                    background_est_cu: counts.background_est_cu,
                    budget_rejections: counts.budget_rejections,
                })
                .collect(),
        }
    }
}

pub(crate) fn alchemy_cu_weight(method: &str) -> u64 {
    match method {
        "eth_call" => 26,
        "eth_getLogs" => 60,
        "eth_getCode" | "eth_getStorageAt" => 20,
        "eth_blockNumber" | "eth_chainId" => 10,
        "getAccountInfo" => 10,
        "getMultipleAccounts"
        | "getProgramAccounts"
        | "getTokenSupply"
        | "getTokenLargestAccounts" => 20,
        "getTokenAccountsByOwner" => 10,
        "getSignaturesForAddress" => 40,
        _ => 26,
    }
}

#[derive(Debug, thiserror::Error)]
#[error("QED_DAILY_RPC_BUDGET_REACHED")]
pub(crate) struct RpcDailyBudgetExceeded;

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
        if matches!(name, "qed_check" | "qed_verify") {
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

pub struct RpcRateLimiter {
    rate: f64,
    state: Mutex<(Instant, f64)>,
    chain: Chain,
    budget: Arc<RpcBudget>,
}

impl RpcRateLimiter {
    #[cfg(test)]
    pub fn new(requests_per_second: u32) -> Self {
        Self::new_with_budget(
            requests_per_second,
            Chain::Base,
            Arc::new(RpcBudget::new(
                DEFAULT_RPC_DAILY_CU_LIMIT,
                DEFAULT_RPC_DAILY_CU_LIMIT_ON_DEMAND,
            )),
        )
    }

    fn new_with_budget(requests_per_second: u32, chain: Chain, budget: Arc<RpcBudget>) -> Self {
        let rate = f64::from(requests_per_second.max(1));
        Self { rate, state: Mutex::new((Instant::now(), 1.0)), chain, budget }
    }

    pub fn debit(&self, method: &str) -> bool {
        self.debit_methods(std::iter::once(method))
    }

    pub fn debit_methods<'a, I>(&self, methods: I) -> bool
    where
        I: Iterator<Item = &'a str> + Clone,
    {
        self.budget.debit_methods(self.chain, methods)
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
    pub fn new(solana_rps: u32, evm_rps: u32, budget: Arc<RpcBudget>) -> Self {
        Self {
            solana: Arc::new(RpcRateLimiter::new_with_budget(
                solana_rps,
                Chain::Solana,
                Arc::clone(&budget),
            )),
            robinhood: Arc::new(RpcRateLimiter::new_with_budget(
                evm_rps,
                Chain::RobinhoodChain,
                Arc::clone(&budget),
            )),
            base: Arc::new(RpcRateLimiter::new_with_budget(
                evm_rps,
                Chain::Base,
                Arc::clone(&budget),
            )),
            ethereum: Arc::new(RpcRateLimiter::new_with_budget(
                evm_rps,
                Chain::Ethereum,
                Arc::clone(&budget),
            )),
            bnb: Arc::new(RpcRateLimiter::new_with_budget(evm_rps, Chain::Bnb, budget)),
        }
    }
}

#[derive(Clone)]
pub struct RegistryApiCache {
    pub registry_hash: String,
    pub body: Arc<Bytes>,
}

#[derive(Clone)]
pub struct StatsSnapshotCache {
    pub html: Bytes,
    pub json: Bytes,
    pub csv: Bytes,
    pub api_pages: Vec<Bytes>,
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
    pub stats_snapshot: Arc<RwLock<Option<Arc<StatsSnapshotCache>>>>,
    pub stats_snapshot_refresh: Arc<tokio::sync::Mutex<()>>,
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

        let publisher_catalog_complete = entries
            .iter()
            .filter(|entry| !entry.official_deployments.is_empty())
            .map(|entry| entry.issuer.clone())
            .collect::<HashSet<_>>();
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
        let publisher_metadata_cache = Cache::builder()
            .max_capacity(10_000)
            .time_to_live(crate::app::guard::PUBLISHER_METADATA_CACHE_TTL)
            .build();
        let guard_cache = Cache::builder()
            .max_capacity(256)
            .time_to_live(crate::app::guard::GUARD_CACHE_TTL)
            .build();
        let guard_locks = Cache::builder().max_capacity(10_000).build();
        let no_code_cache = Cache::builder().max_capacity(4_096).build();
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
            history: crate::adapters::history::HistoryStore::memory(),
            readers,
            check_cache: Arc::new(check_cache.clone()),
            guard_cache: Arc::new(guard_cache.clone()),
            statement_cache: Arc::new(statement_cache.clone()),
            powers_cache: Arc::new(powers_cache.clone()),
            publisher_metadata_cache: Arc::new(publisher_metadata_cache.clone()),
            powers_retry_cache: Arc::new(powers_retry_cache.clone()),
            powers_failure_cache: Arc::new(powers_failure_cache.clone()),
            powers_locks: Arc::new(powers_locks.clone()),
            guard_locks: Arc::new(guard_locks.clone()),
            no_code_cache: Arc::new(no_code_cache.clone()),
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
            publisher_catalog_complete: Arc::new(StdRwLock::new(publisher_catalog_complete)),
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
            stats_snapshot: Arc::new(RwLock::new(None)),
            stats_snapshot_refresh: Arc::new(tokio::sync::Mutex::new(())),
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
    on_demand_requests: Mutex<(Instant, u32)>,
    on_demand_request_limit: u32,
    rpc_buckets: Arc<RpcBuckets>,
    rpc_budget: Arc<RpcBudget>,
    pruner_started: AtomicBool,
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::with_rpc_limits(
            4,
            8,
            DEFAULT_RPC_DAILY_CU_LIMIT,
            DEFAULT_RPC_DAILY_CU_LIMIT_ON_DEMAND,
            300,
        )
    }
}

impl RateLimiter {
    pub fn with_rpc_limits(
        solana_rps: u32,
        evm_rps: u32,
        daily_cu_limit: u64,
        daily_cu_limit_on_demand: u64,
        on_demand_request_limit: u32,
    ) -> Self {
        let rpc_budget = Arc::new(RpcBudget::new(daily_cu_limit, daily_cu_limit_on_demand));
        Self {
            requests: Arc::new(Mutex::new(HashMap::new())),
            on_demand_requests: Mutex::new((Instant::now(), 0)),
            on_demand_request_limit,
            rpc_buckets: Arc::new(RpcBuckets::new(solana_rps, evm_rps, Arc::clone(&rpc_budget))),
            rpc_budget,
            pruner_started: AtomicBool::new(false),
        }
    }

    pub fn rpc_buckets(&self) -> Arc<RpcBuckets> {
        Arc::clone(&self.rpc_buckets)
    }

    pub fn rpc_budget(&self) -> Arc<RpcBudget> {
        Arc::clone(&self.rpc_budget)
    }
    pub fn allow_on_demand_request(&self) -> bool {
        let now = Instant::now();
        let mut window =
            self.on_demand_requests.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if now.duration_since(window.0) >= REQUEST_WINDOW {
            *window = (now, 0);
        }
        if window.1 >= self.on_demand_request_limit {
            return false;
        }
        window.1 += 1;
        true
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

    #[tokio::test]
    async fn rpc_budget_caps_on_demand_reads_and_tracks_method_usage() {
        let budget = RpcBudget::new(100, 50);
        let (_, outcome) = crate::ports::with_rpc_request_context(
            crate::ports::RpcRequestContext::new(true, false),
            async {
                assert!(budget.debit_methods(Chain::Base, ["eth_call"].into_iter()));
                assert!(!budget.debit_methods(Chain::Base, ["eth_getLogs"].into_iter()));
            },
        )
        .await;

        assert!(outcome.budget_rejected());
        let usage = budget.snapshot();
        assert_eq!(usage.est_cu_total, 26);
        assert_eq!(usage.on_demand, 26);
        assert_eq!(usage.rejected, 1);
        assert_eq!(usage.by_chain_method[0].method, "eth_call");
        assert_eq!(usage.by_chain_method[1].method, "eth_getLogs");
        assert_eq!(usage.by_chain_method[1].budget_rejections, 1);
    }

    #[tokio::test]
    async fn on_demand_request_limit_is_admission_only_not_provider_cap() {
        let limiter = RateLimiter::with_rpc_limits(4, 8, 10_000, 10_000, 1);
        assert!(limiter.allow_on_demand_request());
        assert!(!limiter.allow_on_demand_request());

        let base = limiter.rpc_buckets().base.clone();
        let (_, outcome) = crate::ports::with_rpc_request_context(
            crate::ports::RpcRequestContext::new(true, false),
            async {
                assert!(base.debit("eth_call"));
                assert!(base.debit("eth_call"));
            },
        )
        .await;

        assert!(!outcome.budget_rejected());
        assert_eq!(limiter.rpc_budget().snapshot().est_cu_total, 52);
    }

    #[tokio::test]
    async fn crawler_rpc_attempts_are_blocked_before_provider_budgeting() {
        let limiter = RateLimiter::with_rpc_limits(4, 8, 10_000, 10_000, 1);
        let base = limiter.rpc_buckets().base.clone();
        let (_, outcome) = crate::ports::with_rpc_request_context(
            crate::ports::RpcRequestContext::new(true, true),
            async {
                assert!(!base.debit("eth_call"));
            },
        )
        .await;
        assert!(outcome.crawler_blocked());
        assert_eq!(limiter.rpc_budget().snapshot().est_cu_total, 0);

        let (admitted, _) = crate::ports::with_rpc_request_context(
            crate::ports::RpcRequestContext::new(true, false),
            async { base.debit("eth_call") },
        )
        .await;
        assert!(admitted);
    }
    #[test]
    fn usage_stats_count_check_mcp_calls_as_checks() {
        let stats = UsageStats::new();
        stats.record_mcp_tool("qed_check");
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
