use crate::{
    adapters::{net, state::AppState},
    app::check,
    domain::{
        attestation::Attestation,
        chain::Chain,
        check::{CheckReadIssue, CheckResult, Verdict},
        pool::{IndexedPool, PoolInfo},
        registry::{self, Registry},
    },
    ports::PoolIndex,
};
use async_trait::async_trait;
#[cfg(feature = "s3")]
use aws_sdk_s3::Client as S3Client;
#[cfg(feature = "s3")]
use aws_sdk_s3::primitives::ByteStream;
#[cfg(feature = "s3")]
use tokio::io::{AsyncRead, AsyncReadExt};

const DURABLE_BOARD_PREFIX: &str = "discovery/";
#[cfg(feature = "s3")]
const MAX_DURABLE_BOARD_BYTES: usize = 2 * 1024 * 1024;

#[cfg(feature = "s3")]
async fn read_bounded_body<R>(reader: R, content_length: Option<i64>) -> std::io::Result<Vec<u8>>
where
    R: AsyncRead + Unpin,
{
    let expected_length = content_length.and_then(|length| usize::try_from(length).ok());
    if expected_length.is_some_and(|length| length > MAX_DURABLE_BOARD_BYTES) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "durable board object exceeds the size limit",
        ));
    }
    let capacity = expected_length.unwrap_or_default().min(MAX_DURABLE_BOARD_BYTES);
    let mut bytes = Vec::with_capacity(capacity);
    let mut reader = reader.take((MAX_DURABLE_BOARD_BYTES + 1) as u64);
    reader.read_to_end(&mut bytes).await?;
    if bytes.len() > MAX_DURABLE_BOARD_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "durable board object exceeds the size limit",
        ));
    }
    Ok(bytes)
}

#[derive(Clone, Default)]
pub struct DurableBoardStore {
    #[cfg(feature = "s3")]
    client: Option<S3Client>,
    #[cfg(feature = "s3")]
    bucket: Option<String>,
}

impl DurableBoardStore {
    pub async fn new(bucket: Option<String>) -> Self {
        #[cfg(feature = "s3")]
        {
            let Some(bucket) = bucket.filter(|bucket| !bucket.is_empty()) else {
                return Self::default();
            };
            let config = aws_config::defaults(aws_config::BehaviorVersion::latest()).load().await;
            return Self { client: Some(S3Client::new(&config)), bucket: Some(bucket) };
        }
        #[cfg(not(feature = "s3"))]
        {
            let _ = bucket;
            Self::default()
        }
    }

    #[cfg(feature = "s3")]
    async fn read<T: DeserializeOwned>(&self, key: &str) -> Option<T> {
        let (Some(client), Some(bucket)) = (&self.client, &self.bucket) else {
            return None;
        };
        let output = match client.get_object().bucket(bucket).key(key).send().await {
            Ok(output) => output,
            Err(error)
                if error.as_service_error().is_some_and(|error| error.is_no_such_key())
                    || error
                        .raw_response()
                        .is_some_and(|response| response.status().as_u16() == 404) =>
            {
                return None;
            }
            Err(error) => {
                warn!(key, error = %error, "reading durable board failed");
                return None;
            }
        };
        let content_length = output.content_length();
        let bytes = match read_bounded_body(output.body.into_async_read(), content_length).await {
            Ok(bytes) => bytes,
            Err(error) => {
                warn!(key, error = %error, "reading durable board body failed");
                return None;
            }
        };
        match serde_json::from_slice(&bytes) {
            Ok(value) => Some(value),
            Err(error) => {
                warn!(key, error = %error, "durable board is unreadable");
                None
            }
        }
    }

    #[cfg(not(feature = "s3"))]
    async fn read<T: DeserializeOwned>(&self, key: &str) -> Option<T> {
        let _ = key;
        None
    }

    #[cfg(feature = "s3")]
    async fn write<T: Serialize>(&self, key: &str, value: &T) {
        let (Some(client), Some(bucket)) = (&self.client, &self.bucket) else {
            return;
        };
        let bytes = match serde_json::to_vec(value) {
            Ok(bytes) => bytes,
            Err(error) => {
                warn!(key, error = %error, "serializing durable board failed");
                return;
            }
        };
        if bytes.len() > MAX_DURABLE_BOARD_BYTES {
            warn!(key, "refusing to persist an oversized durable board");
            return;
        }
        if let Err(error) = client
            .put_object()
            .bucket(bucket)
            .key(key)
            .content_type("application/json")
            .body(ByteStream::from(bytes))
            .send()
            .await
        {
            warn!(key, error = %error, "persisting durable board failed");
        }
    }

    #[cfg(not(feature = "s3"))]
    async fn write<T: Serialize>(&self, key: &str, value: &T) {
        let _ = (key, value);
    }

    pub async fn load_leaderboard(&self) -> Option<Leaderboard> {
        self.read(&format!("{DURABLE_BOARD_PREFIX}{LEADERBOARD_CACHE_FILE}"))
            .await
            .map(normalize_leaderboard)
            .filter(|board| {
                !board.entries.is_empty() && board.entries.len() <= MAX_LEADERBOARD_CANDIDATES
            })
    }

    pub async fn persist_leaderboard(&self, board: &Leaderboard) {
        if !safe_board_replacement(0, board.entries.len())
            || board.entries.len() > MAX_LEADERBOARD_CANDIDATES
        {
            return;
        }
        if let Some(previous) = self.load_leaderboard().await
            && !safe_leaderboard_replacement(&previous.entries, &board.entries)
        {
            warn!(
                previous = previous.entries.len(),
                next = board.entries.len(),
                "refusing to replace durable leaderboard with an incomplete pool set"
            );
            return;
        }
        self.write(&format!("{DURABLE_BOARD_PREFIX}{LEADERBOARD_CACHE_FILE}"), board).await;
    }

    pub async fn load_featured(&self) -> Option<FeaturedSnapshot> {
        self.read(&format!("{DURABLE_BOARD_PREFIX}{FEATURED_CACHE_FILE}")).await.filter(
            |snapshot: &FeaturedSnapshot| {
                !snapshot.pools.is_empty() && snapshot.pools.len() <= MAX_FEATURED_POOLS
            },
        )
    }

    pub async fn persist_featured(&self, snapshot: &FeaturedSnapshot) {
        if !safe_board_replacement(0, snapshot.pools.len())
            || snapshot.pools.len() > MAX_FEATURED_POOLS
        {
            return;
        }
        if let Some(previous) = self.load_featured().await
            && !safe_board_replacement(previous.pools.len(), snapshot.pools.len())
        {
            warn!(
                previous = previous.pools.len(),
                next = snapshot.pools.len(),
                "refusing to replace durable featured board with a much smaller board"
            );
            return;
        }
        self.write(&format!("{DURABLE_BOARD_PREFIX}{FEATURED_CACHE_FILE}"), snapshot).await;
    }
}

use chrono::{DateTime, SecondsFormat, Utc};
use reqwest::StatusCode;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::path::Path;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};
use thiserror::Error;
use tokio::sync::Mutex;
use tokio::time::sleep;
use tracing::{info, warn};

const DEXSCREENER_API: &str = "https://api.dexscreener.com";
pub const DEXSCREENER_BATCH_SIZE: usize = 30;
const REQUEST_INTERVAL: Duration = Duration::from_millis(200);
const MAX_IMPOSTOR_SEARCHES_PER_REFRESH: usize = 50;
/// DexScreener search answers single-character queries with HTTP 400, so a one-character
/// ticker is searched only in its ticker+x product form.
const MIN_DEXSCREENER_SEARCH_BYTES: usize = 2;
const MAX_IMPOSTOR_CANDIDATES_PER_REFRESH: usize = 50;
const MAX_IMPOSTOR_RECHECKS_PER_REFRESH: usize = MAX_IMPOSTOR_CANDIDATES_PER_REFRESH / 2;
const IMPOSTOR_HOT_TICKERS_PER_REFRESH: usize = 10;
const MAX_STORED_IMPOSTORS: usize = 128;
const MAX_STORED_UNSUPPORTED_CANDIDATES: usize = 256;
pub(crate) const MAX_IMPOSTOR_LABEL_BYTES: usize = 64;
const MAX_IMPOSTOR_CHAIN_ID_BYTES: usize = 32;
pub(crate) const MAX_IMPOSTOR_REASON_BYTES: usize = 256;
const MAX_IMPOSTOR_TIMESTAMP_BYTES: usize = 64;
const MAX_IMPOSTOR_ADDRESS_BYTES: usize = 128;
const MAX_IMPOSTOR_READS: usize = 4;
pub(crate) const MAX_IMPOSTOR_READ_PARAMS_BYTES: usize = 512;
pub(crate) const MAX_IMPOSTOR_READ_RESULT_BYTES: usize = 512;
const MAX_IMPOSTOR_READ_METHOD_BYTES: usize = 64;
const MAX_IMPOSTOR_URL_BYTES: usize = 200;
const MAX_LEADERBOARD_LABEL_BYTES: usize = 64;
pub const MIN_LIQUIDITY_USD: f64 = 1_000.0;
pub const MAX_FEATURED_POOLS: usize = 12;
pub const MAX_LEADERBOARD_CANDIDATES: usize = 300;
pub const LEADERBOARD_PAGE_SIZE: usize = 50;
pub const DISCOVERY_REFRESH_SECS: u64 = 6 * 60 * 60;
pub const PRICE_REFRESH_SECS: u64 = 5 * 60;
pub const PRICE_RETRY_SECS: u64 = 60;
pub const REGISTRY_REFRESH_SECS: u64 = 60 * 60;
const LEADERBOARD_SOURCE: &str = "DexScreener / GeckoTerminal + on-chain reads";
const DEX_BUDGET_WINDOW: Duration = Duration::from_secs(60);
const DEX_GLOBAL_REQUESTS_PER_MINUTE: usize = 240;
const DEX_ENDPOINT_BACKOFF: Duration = Duration::from_secs(120);
const DEX_CANARY_QUERY: &str = "USDC";
const DEX_CANARY_TTL: Duration = Duration::from_secs(5 * 60);
const GECKOTERMINAL_API: &str = "https://api.geckoterminal.com/api/v2";
const GECKOTERMINAL_CALLS_PER_MINUTE: usize = 10;
const GECKOTERMINAL_WINDOW: Duration = Duration::from_secs(60);
const GECKOTERMINAL_BACKOFF: Duration = Duration::from_secs(60);
const GECKOTERMINAL_TIMEOUT: Duration = Duration::from_secs(10);
const GECKOTERMINAL_BATCH_SIZE: usize = 30;
const GECKOTERMINAL_WATCH_NETWORKS: [&str; 7] =
    ["robinhood", "solana", "eth", "base", "bsc", "arc", "ton"];
const GECKOTERMINAL_REGISTRY_REQUESTS_PER_REFRESH: usize = 30;
const GECKOTERMINAL_REGISTRY_POOL_IDS_PER_REFRESH: usize = 300;
const GECKOTERMINAL_REGISTRY_TOKEN_REQUESTS_PER_CHAIN: usize = 4;
const GECKOTERMINAL_REGISTRY_POOL_REQUESTS_PER_CHAIN: usize = 2;
const GECKOTERMINAL_REGISTRY_POOL_IDS_PER_CHAIN: usize = 60;
const GECKOTERMINAL_WATCH_SEARCHES_PER_REFRESH: usize = 70;

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MarketSource {
    #[default]
    Dexscreener,
    Geckoterminal,
}

impl MarketSource {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Dexscreener => "DexScreener",
            Self::Geckoterminal => "GeckoTerminal",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DexEndpoint {
    TokenPairs,
    Pairs,
    Search,
}

impl DexEndpoint {
    const fn limit(self) -> usize {
        match self {
            Self::TokenPairs | Self::Pairs => 300,
            Self::Search => 60,
        }
    }
}

impl fmt::Display for DexEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::TokenPairs => "token-pairs",
            Self::Pairs => "pairs",
            Self::Search => "search",
        };
        formatter.write_str(label)
    }
}

#[derive(Debug)]
struct DexBudget {
    all_requests: VecDeque<Instant>,
    endpoint_requests: HashMap<DexEndpoint, VecDeque<Instant>>,
    backoff_until: HashMap<DexEndpoint, Instant>,
}

impl DexBudget {
    fn try_acquire(&mut self, endpoint: DexEndpoint) -> Result<(), DexBudgetError> {
        let now = Instant::now();
        self.all_requests.retain(|request| now.duration_since(*request) < DEX_BUDGET_WINDOW);
        let requests = self.endpoint_requests.entry(endpoint).or_default();
        requests.retain(|request| now.duration_since(*request) < DEX_BUDGET_WINDOW);
        if self.backoff_until.get(&endpoint).is_some_and(|until| *until > now) {
            return Err(DexBudgetError::Backoff(endpoint));
        }
        if self.all_requests.len() >= DEX_GLOBAL_REQUESTS_PER_MINUTE
            || requests.len() >= endpoint.limit()
        {
            return Err(DexBudgetError::Exhausted(endpoint));
        }
        self.all_requests.push_back(now);
        requests.push_back(now);
        Ok(())
    }

    fn backoff(&mut self, endpoint: DexEndpoint) {
        let now = Instant::now();
        let already_backed_off =
            self.backoff_until.get(&endpoint).is_some_and(|until| *until > now);
        self.backoff_until.insert(endpoint, now + DEX_ENDPOINT_BACKOFF);
        if !already_backed_off {
            warn!(%endpoint, seconds = DEX_ENDPOINT_BACKOFF.as_secs(), "DexScreener endpoint backed off");
        }
    }
}

#[derive(Debug, Error)]
enum DexBudgetError {
    #[error("DexScreener {0} endpoint is backed off")]
    Backoff(DexEndpoint),
    #[error("DexScreener request budget is exhausted for {0}")]
    Exhausted(DexEndpoint),
}

static GLOBAL_DEX_BUDGET: LazyLock<Arc<Mutex<DexBudget>>> = LazyLock::new(|| {
    Arc::new(Mutex::new(DexBudget {
        all_requests: VecDeque::new(),
        endpoint_requests: HashMap::new(),
        backoff_until: HashMap::new(),
    }))
});

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RegistrySnapshot {
    pub entries: usize,
    pub issuers: usize,
    pub updated_at: String,
    pub next_refresh_at: String,
    #[serde(default)]
    pub restored: bool,
    #[serde(default)]
    pub refreshing: bool,
}

impl Default for RegistrySnapshot {
    fn default() -> Self {
        let now = now_rfc3339();
        Self {
            entries: 0,
            issuers: 0,
            updated_at: now,
            next_refresh_at: timestamp_after(REGISTRY_REFRESH_SECS),
            restored: false,
            refreshing: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LeaderboardEntry {
    pub rank: usize,
    pub chain: String,
    pub chain_label: String,
    pub dex: String,
    pub pool: String,
    pub base_symbol: String,
    pub quote_symbol: String,
    #[serde(default)]
    pub source: MarketSource,
    pub issuer: Option<String>,
    pub ticker: Option<String>,
    #[serde(default)]
    pub issuer_on_base: Option<bool>,
    pub verdict: String,
    #[serde(default = "checked_read_status")]
    pub read_status: String,
    #[serde(default)]
    pub read_reason: Option<String>,
    pub price_usd: Option<f64>,
    pub change_24h_pct: Option<f64>,
    pub volume_24h_usd: Option<f64>,
    pub liquidity_usd: Option<f64>,
    pub txns_24h: Option<u64>,
    pub detail_url: String,
    /// Canonical external market page for this exact pair.
    pub trade_url: String,
    #[serde(default)]
    pub explorer_url: String,
    pub attestation_id: Option<String>,
    pub checked_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ImpostorEntry {
    pub chain: String,
    pub chain_label: String,
    pub ticker: String,
    pub publisher: String,
    pub symbol: String,
    pub name: String,
    pub address: String,
    pub first_seen_at: String,
    pub last_seen_at: String,
    pub volume_24h_usd: Option<f64>,
    #[serde(default)]
    pub source: MarketSource,
    pub guard_url: String,
    pub reason: String,
    #[serde(default)]
    pub reads: Vec<crate::domain::attestation::Read>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_chain_symbol: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_chain_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher_catalog_snapshot_hash: Option<String>,
    #[serde(default)]
    pub evidence_truncated: bool,
    // Accepted only to migrate old durable boards; never emitted in stats or public responses.
    #[serde(default, skip_serializing)]
    pub guard_document: Option<crate::domain::guard::GuardDocument>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct UnsupportedImpostorCandidate {
    pub dex_chain_id: String,
    pub ticker: String,
    pub publisher: String,
    pub symbol: String,
    pub name: String,
    pub address: String,
    pub first_seen_at: String,
    pub last_seen_at: String,
    pub volume_24h_usd: Option<f64>,
    #[serde(default)]
    pub source: MarketSource,
    #[serde(default)]
    pub evidence_truncated: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ImpostorSnapshot {
    #[serde(default)]
    pub scanned_at: String,
    /// Start of the current impostor-search source outage: every search failed or returned
    /// no pairs. Absent after a successful scan; retained results and `scanned_at` stay from
    /// the last successful scan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_unavailable_since: Option<String>,
    #[serde(default)]
    pub next_ticker_offset: usize,
    #[serde(default)]
    pub next_entry_offset: usize,
    #[serde(default)]
    pub unsupported_seen: usize,
    #[serde(default)]
    pub official_on_unsupported_chain: usize,
    #[serde(default)]
    pub evicted_entries: usize,
    #[serde(default)]
    pub evicted_unsupported_candidates: usize,
    #[serde(default)]
    pub rejected_oversize_entries: usize,
    #[serde(default)]
    pub rejected_oversize_unsupported_candidates: usize,
    #[serde(default)]
    pub entries: Vec<ImpostorEntry>,
    #[serde(default)]
    pub unsupported_candidates: Vec<UnsupportedImpostorCandidate>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Leaderboard {
    pub updated_at: String,
    pub next_refresh_at: String,
    pub source: String,
    pub registry: RegistrySnapshot,
    #[serde(default)]
    pub total: usize,
    pub entries: Vec<LeaderboardEntry>,
    #[serde(default)]
    pub restored: bool,
    #[serde(default)]
    pub refreshing: bool,
    #[serde(default)]
    pub impostors: ImpostorSnapshot,
    #[serde(default)]
    pub empty_successful: bool,
}

impl Default for Leaderboard {
    fn default() -> Self {
        let now = now_rfc3339();
        Self {
            updated_at: now,
            next_refresh_at: timestamp_after(DISCOVERY_REFRESH_SECS),
            source: LEADERBOARD_SOURCE.to_owned(),
            registry: RegistrySnapshot::default(),
            total: 0,
            entries: Vec::new(),
            restored: false,
            refreshing: false,
            empty_successful: false,
            impostors: ImpostorSnapshot::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PricePoint {
    pub chain: String,
    pub pool: String,
    pub price_usd: Option<f64>,
    pub change_24h_pct: Option<f64>,
    pub volume_24h_usd: Option<f64>,
    pub liquidity_usd: Option<f64>,
    #[serde(default)]
    pub source: MarketSource,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PriceSnapshot {
    pub updated_at: String,
    pub prices: Vec<PricePoint>,
}

impl Default for PriceSnapshot {
    fn default() -> Self {
        Self { updated_at: now_rfc3339(), prices: Vec::new() }
    }
}

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

pub fn timestamp_after(seconds: u64) -> String {
    (Utc::now() + chrono::Duration::seconds(i64::try_from(seconds).unwrap_or(i64::MAX)))
        .to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Return the bounded delay before a restored non-empty board reaches its
/// six-hour discovery age. Invalid, empty, stale, and future boards need an
/// immediate discovery instead.
pub fn restored_discovery_delay(
    updated_at: &str,
    has_content: bool,
    now: DateTime<Utc>,
) -> Option<Duration> {
    if !has_content {
        return None;
    }
    let checked_at = DateTime::parse_from_rfc3339(updated_at).ok()?.with_timezone(&Utc);
    let age = now.signed_duration_since(checked_at);
    let window =
        chrono::Duration::seconds(i64::try_from(DISCOVERY_REFRESH_SECS).unwrap_or(i64::MAX));
    if age < chrono::Duration::zero() || age >= window {
        return None;
    }
    (window - age).to_std().ok().map(|delay| delay.min(Duration::from_secs(DISCOVERY_REFRESH_SECS)))
}

/// Start the first complete discovery immediately after restoring both boards.
/// Subsequent refreshes use the regular six-hour interval.
pub fn first_discovery_delay(
    leaderboard: Option<Duration>,
    featured: Option<Duration>,
) -> Option<Duration> {
    leaderboard.zip(featured).map(|_| Duration::ZERO)
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FeaturedPool {
    pub chain: Chain,
    pub dex: String,
    pub pool: String,
    pub base_symbol: String,
    pub base_address: String,
    pub quote_symbol: String,
    pub quote_address: String,
    pub issuer: Option<String>,
    pub ticker: Option<String>,
    pub verdict: String,
    pub quote_balance: Option<String>,
    pub quote_share_of_supply: Option<f64>,
    pub volume_24h_usd: Option<f64>,
    pub liquidity_usd: Option<f64>,
    pub curated: bool,
    pub note: Option<String>,
    pub updated_at: String,
}

pub(crate) struct DiscoveryPoolIndex {
    featured: Arc<tokio::sync::RwLock<Vec<FeaturedPool>>>,
    leaderboard: Arc<tokio::sync::RwLock<Leaderboard>>,
}

impl DiscoveryPoolIndex {
    pub(crate) fn new(
        featured: Arc<tokio::sync::RwLock<Vec<FeaturedPool>>>,
        leaderboard: Arc<tokio::sync::RwLock<Leaderboard>>,
    ) -> Self {
        Self { featured, leaderboard }
    }
}

#[async_trait]
impl PoolIndex for DiscoveryPoolIndex {
    async fn candidate_pools(
        &self,
        chain: Chain,
        token: &str,
        attestations: &HashMap<String, Attestation>,
    ) -> Vec<String> {
        let token = token.to_ascii_lowercase();
        let featured = self.featured.read().await;
        let leaderboard = self.leaderboard.read().await;
        let mut candidates = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for pool in featured.iter().filter(|pool| pool.chain == chain) {
            if !pool.pool.is_empty()
                && (pool.base_address.eq_ignore_ascii_case(&token)
                    || pool.quote_address.eq_ignore_ascii_case(&token))
                && seen.insert(pool.pool.to_ascii_lowercase())
            {
                candidates.push(pool.pool.clone());
            }
        }
        let chain_slug = chain_slug(chain);
        for entry in leaderboard.entries.iter().filter(|entry| entry.chain == chain_slug) {
            let Some(attestation_id) = entry.attestation_id.as_deref() else { continue };
            let Some(attestation) = attestations.get(attestation_id) else { continue };
            if !entry.pool.is_empty()
                && (attestation.pool.base.address.eq_ignore_ascii_case(&token)
                    || attestation.pool.quote.address.eq_ignore_ascii_case(&token))
                && seen.insert(entry.pool.to_ascii_lowercase())
            {
                candidates.push(entry.pool.clone());
            }
        }
        candidates
    }

    async fn known_pools(&self, attestations: &HashMap<String, Attestation>) -> Vec<IndexedPool> {
        let leaderboard = self.leaderboard.read().await;
        let mut pools = Vec::new();
        for entry in &leaderboard.entries {
            let Some(chain) = Chain::parse(&entry.chain) else { continue };
            let attestation =
                entry.attestation_id.as_ref().and_then(|id| attestations.get(id)).or_else(|| {
                    attestations.values().find(|value| {
                        value.chain == chain
                            && value.pool.chain == chain
                            && same_pool_path(chain, &value.pool.pool, &entry.pool)
                    })
                });
            let Some(attestation) = attestation.filter(|attestation| {
                attestation.chain == chain
                    && attestation.pool.chain == chain
                    && same_pool_path(chain, &attestation.pool.pool, &entry.pool)
            }) else {
                continue;
            };
            let trade_url = canonical_market_url_for_source(
                chain,
                &entry.pool,
                Some(entry.trade_url.as_str()),
                entry.source,
            );
            for token in [&attestation.pool.base, &attestation.pool.quote] {
                pools.push(IndexedPool {
                    chain,
                    token_address: token.address.clone(),
                    symbol: token.symbol.clone(),
                    pool: entry.pool.clone(),
                    pool_url: entry.detail_url.clone(),
                    trade_url: trade_url.clone(),
                    venue: attestation.pool.dex.clone(),
                    quote_address: attestation.pool.quote.address.clone(),
                    quote_symbol: attestation.pool.quote.symbol.clone(),
                    verdict: match &attestation.verdict {
                        Verdict::Verified { .. } => "verified",
                        Verdict::Mismatch { .. } => "mismatch",
                        Verdict::NoMatch => "no_match",
                        Verdict::Unknown { .. } => "unknown",
                    }
                    .to_owned(),
                    observed_at: attestation.checked_at.clone(),
                });
            }
        }
        pools
    }

    async fn warm_tickers(&self) -> Vec<String> {
        let featured = self.featured.read().await;
        let leaderboard = self.leaderboard.read().await;
        let mut ordered = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut append = |ticker: &str| {
            let ticker = ticker.trim().to_ascii_uppercase();
            if !ticker.is_empty() && seen.insert(ticker.clone()) {
                ordered.push(ticker);
            }
        };
        for ticker in featured.iter().filter_map(|pool| pool.ticker.as_deref()) {
            append(ticker);
        }
        let mut entries = leaderboard.entries.iter().collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.rank);
        for ticker in entries.iter().filter_map(|entry| entry.ticker.as_deref()) {
            append(ticker);
        }
        ordered
    }
}

const LEADERBOARD_CACHE_FILE: &str = "leaderboard.json";
const FEATURED_CACHE_FILE: &str = "featured.json";

/// The featured pools as last published, carrying the time the board was
/// built so a restart can tell a usable cache from a stale one.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FeaturedSnapshot {
    pub updated_at: String,
    pub pools: Vec<FeaturedPool>,
    #[serde(default)]
    pub next_refresh_at: String,
    #[serde(default)]
    pub restored: bool,
    #[serde(default)]
    pub refreshing: bool,
    #[serde(default)]
    pub empty_successful: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FeaturedStatus {
    pub updated_at: String,
    pub next_refresh_at: String,
    pub restored: bool,
    pub refreshing: bool,
    pub empty_successful: bool,
}

impl Default for FeaturedStatus {
    fn default() -> Self {
        let now = now_rfc3339();
        Self {
            updated_at: now,
            next_refresh_at: timestamp_after(DISCOVERY_REFRESH_SECS),
            restored: false,
            refreshing: false,
            empty_successful: false,
        }
    }
}

/// Write through a temporary file in the same directory and rename, so a
/// reader never sees a half-written board and a crash cannot truncate one.
fn store_board(path: &Path, value: &impl Serialize) {
    let temp = path.with_extension("json.tmp");
    let written = serde_json::to_vec(value)
        .map_err(std::io::Error::other)
        .and_then(|bytes| std::fs::write(&temp, bytes))
        .and_then(|()| std::fs::rename(&temp, path));
    if let Err(error) = written {
        warn!(path = %path.display(), error = %error, "persisting board failed");
        let _ = std::fs::remove_file(&temp);
    }
}

fn read_board<T: DeserializeOwned>(path: &Path) -> Option<T> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => {
            if error.kind() != std::io::ErrorKind::NotFound {
                warn!(path = %path.display(), error = %error, "reading persisted board failed");
            }
            return None;
        }
    };
    match serde_json::from_slice(&bytes) {
        Ok(value) => Some(value),
        Err(error) => {
            warn!(path = %path.display(), error = %error, "persisted board is unreadable");
            None
        }
    }
}

fn safe_board_replacement(previous: usize, next: usize) -> bool {
    next > 0 && (previous == 0 || (next as u128).saturating_mul(2) >= previous as u128)
}

fn safe_leaderboard_replacement(previous: &[LeaderboardEntry], next: &[LeaderboardEntry]) -> bool {
    if !safe_board_replacement(previous.len(), next.len()) {
        return false;
    }
    if !next.iter().any(|entry| entry.source == MarketSource::Geckoterminal) {
        return true;
    }
    previous.iter().all(|old| {
        next.iter().any(|new| {
            old.chain.eq_ignore_ascii_case(&new.chain)
                && match chain_from_dex_id(&old.chain) {
                    Some(chain) => same_pool_path(chain, &new.pool, &old.pool),
                    None => new.pool.eq_ignore_ascii_case(&old.pool),
                }
        })
    })
}

pub fn save_leaderboard(data_dir: &Path, leaderboard: &Leaderboard) {
    let path = data_dir.join(LEADERBOARD_CACHE_FILE);
    if !safe_board_replacement(0, leaderboard.entries.len()) {
        warn!("refusing to persist an empty leaderboard");
        return;
    }
    if let Some(previous) = read_board::<Leaderboard>(&path)
        && !safe_leaderboard_replacement(&previous.entries, &leaderboard.entries)
    {
        warn!(
            previous = previous.entries.len(),
            next = leaderboard.entries.len(),
            "refusing to replace persisted leaderboard with an incomplete pool set"
        );
        return;
    }
    store_board(&path, leaderboard);
}

pub fn load_leaderboard(data_dir: &Path) -> Option<Leaderboard> {
    read_board::<Leaderboard>(&data_dir.join(LEADERBOARD_CACHE_FILE))
        .map(normalize_leaderboard)
        .filter(|leaderboard| !leaderboard.entries.is_empty())
}

fn normalize_impostor_snapshot(mut snapshot: ImpostorSnapshot) -> ImpostorSnapshot {
    let mut rejected_entries = 0usize;
    let mut entries = std::mem::take(&mut snapshot.entries);
    entries.retain_mut(|entry| {
        let Some(chain) = chain_from_dex_id(&entry.chain) else {
            rejected_entries = rejected_entries.saturating_add(1);
            tracing::debug!(
                rejection = ?InvalidWatchObservation::InvalidChain,
                "rejected persisted publisher-watch entry"
            );
            return false;
        };
        let address = entry.address.as_str();
        if !valid_token_address(chain, address) {
            rejected_entries = rejected_entries.saturating_add(1);
            tracing::debug!(
                rejection = ?InvalidWatchObservation::InvalidAddress,
                "rejected persisted publisher-watch entry"
            );
            return false;
        }
        if entry
            .publisher_catalog_snapshot_hash
            .as_deref()
            .is_some_and(|hash| !valid_publisher_catalog_snapshot_hash(hash))
        {
            rejected_entries = rejected_entries.saturating_add(1);
            tracing::debug!(
                rejection = ?InvalidWatchObservation::InvalidCatalogHash,
                "rejected persisted publisher-watch entry"
            );
            return false;
        }
        let guard_url = format!("/guard/{}/{}", chain_slug(chain), address);
        let legacy_guard = entry.guard_document.take();
        let matching_legacy_guard = legacy_guard
            .as_ref()
            .filter(|guard| guard.chain == chain && guard.address == entry.address);
        let (reads, evidence_truncated) = compact_watch_reads(
            entry
                .reads
                .iter()
                .chain(matching_legacy_guard.into_iter().flat_map(|guard| guard.reads.iter())),
            chain,
            address,
        );
        entry.chain = chain_slug(chain).to_owned();
        entry.chain_label = chain_label(chain).to_owned();
        entry.guard_url = guard_url;
        entry.reads = reads;
        entry.evidence_truncated |= evidence_truncated
            || legacy_guard.is_some()
            || entry.publisher_catalog_snapshot_hash.is_none();
        if let Some(guard) = matching_legacy_guard {
            if entry.on_chain_symbol.is_none()
                && let Some(symbol) = guard.identity.observed_symbol.clone()
            {
                entry.on_chain_symbol = Some(symbol);
            }
            if entry.on_chain_name.is_none()
                && let Some(name) = guard.identity.observed_name.clone()
            {
                entry.on_chain_name = Some(name);
            }
            if entry.reason.is_empty()
                && let Some(reason) = guard.reasons.first()
            {
                entry.reason = reason.detail.clone();
            }
        }
        clip_impostor_entry_text(entry);
        true
    });
    entries.sort_by(|left, right| {
        right
            .last_seen_at
            .cmp(&left.last_seen_at)
            .then_with(|| left.chain.cmp(&right.chain))
            .then_with(|| left.address.cmp(&right.address))
    });
    let evicted_entries = entries.len().saturating_sub(MAX_STORED_IMPOSTORS);
    entries.truncate(MAX_STORED_IMPOSTORS);
    snapshot.evicted_entries = snapshot.evicted_entries.saturating_add(evicted_entries);
    snapshot.rejected_oversize_entries =
        snapshot.rejected_oversize_entries.saturating_add(rejected_entries);

    let mut rejected_unsupported = 0usize;
    snapshot.unsupported_candidates.retain_mut(|candidate| {
        if let Err(rejection) =
            validate_unsupported_candidate(&candidate.dex_chain_id, &candidate.address)
        {
            rejected_unsupported = rejected_unsupported.saturating_add(1);
            tracing::debug!(
                rejection = ?rejection,
                "rejected persisted unsupported-chain watch candidate"
            );
            return false;
        }
        clip_unsupported_candidate_text(candidate);
        true
    });
    snapshot.unsupported_candidates.sort_by(|left, right| {
        right
            .last_seen_at
            .cmp(&left.last_seen_at)
            .then_with(|| left.dex_chain_id.cmp(&right.dex_chain_id))
            .then_with(|| left.address.cmp(&right.address))
    });
    let evicted_unsupported =
        snapshot.unsupported_candidates.len().saturating_sub(MAX_STORED_UNSUPPORTED_CANDIDATES);
    snapshot.unsupported_candidates.truncate(MAX_STORED_UNSUPPORTED_CANDIDATES);
    snapshot.evicted_unsupported_candidates =
        snapshot.evicted_unsupported_candidates.saturating_add(evicted_unsupported);
    snapshot.rejected_oversize_unsupported_candidates =
        snapshot.rejected_oversize_unsupported_candidates.saturating_add(rejected_unsupported);
    snapshot.unsupported_seen = snapshot.unsupported_candidates.len();
    if let Some(since) = &mut snapshot.source_unavailable_since {
        truncate_text_bytes(since, MAX_IMPOSTOR_TIMESTAMP_BYTES);
    }
    snapshot.entries = entries;
    snapshot
}

pub(crate) fn normalize_leaderboard(mut leaderboard: Leaderboard) -> Leaderboard {
    for entry in &mut leaderboard.entries {
        if let Some(chain) = chain_from_dex_id(&entry.chain) {
            let slug = chain_slug(chain);
            entry.chain = slug.to_owned();
            entry.chain_label = chain_label(chain).to_owned();
            entry.detail_url = format!("/validated/{slug}/{}", entry.pool);
            entry.trade_url = canonical_market_url_for_source(
                chain,
                &entry.pool,
                Some(&entry.trade_url),
                entry.source,
            );
            entry.explorer_url = explorer_url(chain, &entry.pool);
        }
    }
    leaderboard.impostors = normalize_impostor_snapshot(leaderboard.impostors);
    leaderboard
}

pub fn save_featured(data_dir: &Path, featured: &FeaturedSnapshot) {
    let path = data_dir.join(FEATURED_CACHE_FILE);
    if !safe_board_replacement(0, featured.pools.len()) {
        warn!("refusing to persist an empty featured board");
        return;
    }
    if let Some(previous) = read_board::<FeaturedSnapshot>(&path)
        && !safe_board_replacement(previous.pools.len(), featured.pools.len())
    {
        warn!(
            previous = previous.pools.len(),
            next = featured.pools.len(),
            "refusing to replace persisted featured board with a much smaller board"
        );
        return;
    }
    store_board(&path, featured);
}

pub fn load_featured_snapshot(data_dir: &Path) -> Option<FeaturedSnapshot> {
    read_board::<FeaturedSnapshot>(&data_dir.join(FEATURED_CACHE_FILE))
        .filter(|snapshot| !snapshot.pools.is_empty())
}

#[cfg(test)]
pub fn load_featured(data_dir: &Path) -> Option<Vec<FeaturedPool>> {
    load_featured_snapshot(data_dir).map(|snapshot| snapshot.pools)
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct DexToken {
    #[serde(default)]
    pub address: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub symbol: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct DexLiquidity {
    #[serde(default)]
    pub usd: Option<f64>,
    #[serde(default)]
    pub base: Option<f64>,
    #[serde(default)]
    pub quote: Option<f64>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct DexVolume {
    #[serde(default)]
    pub h24: Option<f64>,
    #[serde(default)]
    pub h6: Option<f64>,
    #[serde(default)]
    pub h1: Option<f64>,
    #[serde(default)]
    pub m5: Option<f64>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct DexPair {
    #[serde(rename = "chainId")]
    pub chain_id: String,
    #[serde(rename = "dexId")]
    pub dex_id: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(rename = "pairAddress")]
    pub pair_address: String,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(rename = "baseToken")]
    pub base_token: DexToken,
    #[serde(rename = "quoteToken")]
    pub quote_token: DexToken,
    #[serde(rename = "priceUsd", default, deserialize_with = "deserialize_optional_f64")]
    pub price_usd: Option<f64>,
    #[serde(default)]
    pub volume: Option<DexVolume>,
    #[serde(rename = "priceChange", default)]
    pub price_change: Option<DexPriceChange>,
    #[serde(default)]
    pub txns: Option<DexTransactions>,
    #[serde(default)]
    pub liquidity: Option<DexLiquidity>,
    #[serde(skip)]
    pub source: MarketSource,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct DexPriceChange {
    #[serde(default, deserialize_with = "deserialize_optional_f64")]
    pub h24: Option<f64>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct DexTxnWindow {
    #[serde(default)]
    pub buys: Option<u64>,
    #[serde(default)]
    pub sells: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct DexTransactions {
    #[serde(default)]
    pub h24: Option<DexTxnWindow>,
}

fn deserialize_optional_f64<'de, D>(deserializer: D) -> Result<Option<f64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    match value {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Number(number)) => number
            .as_f64()
            .ok_or_else(|| serde::de::Error::custom("number is not representable as f64"))
            .map(Some),
        Some(serde_json::Value::String(value)) if value.trim().is_empty() => Ok(None),
        Some(serde_json::Value::String(value)) => value
            .parse::<f64>()
            .map(Some)
            .map_err(|error| serde::de::Error::custom(format!("invalid number: {error}"))),
        Some(value) => {
            Err(serde::de::Error::custom(format!("expected a number or null, got {value}")))
        }
    }
}

#[derive(Debug, Error)]
pub enum DiscoveryError {
    #[error("DexScreener does not support {0}")]
    UnsupportedChain(Chain),
    #[error("DexScreener request failed with status {0}")]
    HttpStatus(StatusCode),
    #[error("DexScreener {0} endpoint is backed off")]
    EndpointBackoff(DexEndpoint),
    #[error("DexScreener request budget is exhausted for {0}")]
    BudgetExhausted(DexEndpoint),
    #[error("DexScreener discovery failed for {failed}/{attempted} chain requests")]
    ChainRequestsFailed { attempted: usize, failed: usize },
    #[error("DexScreener request failed")]
    Http(#[from] reqwest::Error),
    #[error("DexScreener response body failed limits: {0}")]
    Body(String),
    #[error("GeckoTerminal request failed with status {0}")]
    GeckoHttpStatus(StatusCode),
    #[error("GeckoTerminal request failed")]
    GeckoHttp(reqwest::Error),
    #[error("GeckoTerminal response body failed limits: {0}")]
    GeckoBody(String),
    #[error("could not parse GeckoTerminal response: {0}")]
    GeckoJson(serde_json::Error),
    #[error("GeckoTerminal refresh request budget is exhausted")]
    GeckoRefreshBudgetExhausted,
    #[error("could not read curated pools")]
    Io(#[from] std::io::Error),
    #[error("could not parse DexScreener response: {0}")]
    Json(#[from] serde_json::Error),
}

fn is_dex_blocked(error: &DiscoveryError) -> bool {
    matches!(
        error,
        DiscoveryError::HttpStatus(StatusCode::TOO_MANY_REQUESTS)
            | DiscoveryError::EndpointBackoff(_)
            | DiscoveryError::BudgetExhausted(_)
    )
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct DexPairResponse {
    #[serde(default)]
    pub pairs: Option<Vec<DexPair>>,
    #[serde(default)]
    pub pair: Option<DexPair>,
}

#[derive(Clone)]
pub struct DexScreenerClient {
    http: reqwest::Client,
    base_url: String,
}

impl DexScreenerClient {
    pub fn new(http: reqwest::Client) -> Self {
        Self { http, base_url: DEXSCREENER_API.to_owned() }
    }

    #[cfg(test)]
    fn with_base_url(http: reqwest::Client, base_url: String) -> Self {
        Self { http, base_url }
    }

    /// Query up to thirty token addresses in one request. DexScreener's
    /// multi-token endpoint returns the same pair shape as token-pairs.
    pub async fn tokens(
        &self,
        chain: Chain,
        token_addresses: &[String],
    ) -> Result<Vec<DexPair>, DiscoveryError> {
        let chain_id = dex_chain_id(chain).ok_or(DiscoveryError::UnsupportedChain(chain))?;
        let mut pairs = Vec::new();
        for chunk in token_addresses.chunks(DEXSCREENER_BATCH_SIZE) {
            if chunk.is_empty() {
                continue;
            }
            let addresses = chunk.join(",");
            let url = format!("{}/tokens/v1/{chain_id}/{addresses}", self.base_url);
            let mut chunk_pairs: Vec<DexPair> =
                self.get_json(&url, DexEndpoint::TokenPairs).await?;
            pairs.append(&mut chunk_pairs);
        }
        Ok(pairs)
    }

    /// Query up to thirty pool addresses in one request. A 429 is returned
    /// unchanged so the caller can skip the complete ticker cycle.
    pub async fn pairs(
        &self,
        chain: Chain,
        pool_addresses: &[String],
    ) -> Result<Vec<DexPair>, DiscoveryError> {
        let chain_id = dex_chain_id(chain).ok_or(DiscoveryError::UnsupportedChain(chain))?;
        let mut pairs = Vec::new();
        for chunk in pool_addresses.chunks(DEXSCREENER_BATCH_SIZE) {
            if chunk.is_empty() {
                continue;
            }
            let addresses = chunk.join(",");
            let url = format!("{}/latest/dex/pairs/{chain_id}/{addresses}", self.base_url);
            let response: DexPairResponse = self.get_json(&url, DexEndpoint::Pairs).await?;
            if let Some(mut chunk_pairs) = response.pairs {
                pairs.append(&mut chunk_pairs);
            } else if let Some(pair) = response.pair {
                pairs.push(pair);
            }
        }
        Ok(pairs)
    }

    pub async fn pair(
        &self,
        chain: Chain,
        pool_address: &str,
    ) -> Result<Option<DexPair>, DiscoveryError> {
        let chain_id = dex_chain_id(chain).ok_or(DiscoveryError::UnsupportedChain(chain))?;
        let url = format!("{}/latest/dex/pairs/{chain_id}/{pool_address}", self.base_url);
        let response: DexPairResponse = self.get_json(&url, DexEndpoint::Pairs).await?;
        Ok(response.pair.or_else(|| response.pairs.and_then(|mut pairs| pairs.pop())))
    }

    pub async fn search(&self, query: &str) -> Result<Vec<DexPair>, DiscoveryError> {
        let mut url = reqwest::Url::parse(&format!("{}/latest/dex/search", self.base_url))
            .expect("DexScreener search endpoint is a valid URL");
        url.query_pairs_mut().append_pair("q", query);
        let response: DexPairResponse = self.get_json(url.as_str(), DexEndpoint::Search).await?;
        let mut pairs = response.pairs.unwrap_or_default();
        if let Some(pair) = response.pair {
            pairs.push(pair);
        }
        Ok(pairs)
    }

    async fn get_json<T: for<'de> Deserialize<'de>>(
        &self,
        url: &str,
        endpoint: DexEndpoint,
    ) -> Result<T, DiscoveryError> {
        {
            let mut budget = GLOBAL_DEX_BUDGET.lock().await;
            match budget.try_acquire(endpoint) {
                Ok(()) => {}
                Err(DexBudgetError::Backoff(endpoint)) => {
                    return Err(DiscoveryError::EndpointBackoff(endpoint));
                }
                Err(DexBudgetError::Exhausted(endpoint)) => {
                    return Err(DiscoveryError::BudgetExhausted(endpoint));
                }
            }
        }
        let response = self.http.get(url).send().await?;
        let status = response.status();
        if status == StatusCode::TOO_MANY_REQUESTS {
            GLOBAL_DEX_BUDGET.lock().await.backoff(endpoint);
        }
        if !status.is_success() {
            return Err(DiscoveryError::HttpStatus(status));
        }
        let body = net::body(response).await.map_err(DiscoveryError::Body)?;
        Ok(serde_json::from_slice(&body)?)
    }
}

#[derive(Debug)]
struct GeckoBudget {
    requests: VecDeque<Instant>,
    backoff_until: Option<Instant>,
    #[cfg(test)]
    request_limit: usize,
    #[cfg(test)]
    backoff_duration: Duration,
}

impl Default for GeckoBudget {
    fn default() -> Self {
        Self {
            requests: VecDeque::new(),
            backoff_until: None,
            #[cfg(test)]
            request_limit: GECKOTERMINAL_CALLS_PER_MINUTE,
            #[cfg(test)]
            backoff_duration: GECKOTERMINAL_BACKOFF,
        }
    }
}

impl GeckoBudget {
    fn delay_or_acquire(&mut self, now: Instant) -> Option<Duration> {
        self.requests.retain(|request| now.duration_since(*request) < GECKOTERMINAL_WINDOW);
        if let Some(until) = self.backoff_until
            && until > now
        {
            return Some(until.duration_since(now));
        }
        self.backoff_until = None;
        #[cfg(test)]
        let request_limit = self.request_limit;
        #[cfg(not(test))]
        let request_limit = GECKOTERMINAL_CALLS_PER_MINUTE;
        if self.requests.len() >= request_limit {
            return self
                .requests
                .front()
                .map(|oldest| (*oldest + GECKOTERMINAL_WINDOW).saturating_duration_since(now));
        }
        self.requests.push_back(now);
        None
    }

    fn backoff(&mut self, now: Instant) {
        #[cfg(test)]
        let duration = self.backoff_duration;
        #[cfg(not(test))]
        let duration = GECKOTERMINAL_BACKOFF;
        self.backoff_until = Some(now + duration);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GeckoRegistryRequest {
    Tokens,
    Pools,
}

#[derive(Debug, Default)]
struct GeckoRegistryBudget {
    requests: usize,
    pool_ids: HashSet<String>,
    token_requests_by_chain: [usize; 5],
    pool_requests_by_chain: [usize; 5],
    pool_ids_by_chain: [usize; 5],
}

impl GeckoRegistryBudget {
    fn try_acquire(&mut self, chain: Chain, request: GeckoRegistryRequest) -> bool {
        let chain_index = gecko_chain_budget_index(chain);
        let used = match request {
            GeckoRegistryRequest::Tokens => self.token_requests_by_chain[chain_index],
            GeckoRegistryRequest::Pools => self.pool_requests_by_chain[chain_index],
        };
        let limit = match request {
            GeckoRegistryRequest::Tokens => GECKOTERMINAL_REGISTRY_TOKEN_REQUESTS_PER_CHAIN,
            GeckoRegistryRequest::Pools => GECKOTERMINAL_REGISTRY_POOL_REQUESTS_PER_CHAIN,
        };
        if self.requests >= GECKOTERMINAL_REGISTRY_REQUESTS_PER_REFRESH || used >= limit {
            return false;
        }
        match request {
            GeckoRegistryRequest::Tokens => self.token_requests_by_chain[chain_index] += 1,
            GeckoRegistryRequest::Pools => self.pool_requests_by_chain[chain_index] += 1,
        }
        self.requests += 1;
        true
    }

    fn try_add_pool_id(&mut self, chain: Chain, network: &str, address: &str) -> bool {
        let id = format!("{network}_{}", address.to_ascii_lowercase());
        if self.pool_ids.contains(&id) {
            return true;
        }
        let chain_index = gecko_chain_budget_index(chain);
        if self.pool_ids.len() >= GECKOTERMINAL_REGISTRY_POOL_IDS_PER_REFRESH
            || self.pool_ids_by_chain[chain_index] >= GECKOTERMINAL_REGISTRY_POOL_IDS_PER_CHAIN
        {
            return false;
        }
        self.pool_ids.insert(id);
        self.pool_ids_by_chain[chain_index] += 1;
        true
    }
}

#[derive(Debug, Default)]
struct GeckoWatchBudget {
    requests: usize,
}

impl GeckoWatchBudget {
    fn try_acquire(&mut self) -> bool {
        if self.requests >= GECKOTERMINAL_WATCH_SEARCHES_PER_REFRESH {
            return false;
        }
        self.requests += 1;
        true
    }
}

enum GeckoRequestBudget<'a> {
    Registry(&'a mut GeckoRegistryBudget, Chain, GeckoRegistryRequest),
    Watch(&'a mut GeckoWatchBudget),
}

impl GeckoRequestBudget<'_> {
    fn try_acquire(&mut self) -> bool {
        match self {
            Self::Registry(budget, chain, request) => budget.try_acquire(*chain, *request),
            Self::Watch(budget) => budget.try_acquire(),
        }
    }
}
static GLOBAL_GECKOTERMINAL_BUDGET: LazyLock<Arc<Mutex<GeckoBudget>>> =
    LazyLock::new(|| Arc::new(Mutex::new(GeckoBudget::default())));

#[derive(Debug, Default)]
struct DexAvailability {
    checked_at: Option<Instant>,
    available: bool,
}

static GLOBAL_DEX_AVAILABILITY: LazyLock<Arc<Mutex<DexAvailability>>> =
    LazyLock::new(|| Arc::new(Mutex::new(DexAvailability::default())));

#[derive(Debug, Deserialize)]
struct GeckoApiResponse {
    #[serde(default)]
    data: Vec<GeckoEntity>,
    #[serde(default)]
    included: Vec<GeckoEntity>,
}

#[derive(Debug, Deserialize)]
struct GeckoEntity {
    id: String,
    #[serde(default)]
    attributes: serde_json::Value,
    #[serde(default)]
    relationships: serde_json::Value,
}

#[derive(Clone)]
struct GeckoTerminalClient {
    http: reqwest::Client,
    base_url: String,
    budget: Arc<Mutex<GeckoBudget>>,
}

impl GeckoTerminalClient {
    fn new(http: reqwest::Client) -> Self {
        Self {
            http,
            base_url: GECKOTERMINAL_API.to_owned(),
            budget: Arc::clone(&GLOBAL_GECKOTERMINAL_BUDGET),
        }
    }

    #[cfg(test)]
    fn with_base_url(http: reqwest::Client, base_url: String) -> Self {
        Self { http, base_url, budget: Arc::new(Mutex::new(GeckoBudget::default())) }
    }

    async fn acquire(&self) {
        loop {
            let delay = self.budget.lock().await.delay_or_acquire(Instant::now());
            if let Some(delay) = delay {
                sleep(delay).await;
            } else {
                return;
            }
        }
    }

    async fn get_json<T: DeserializeOwned>(
        &self,
        url: &str,
        mut request_budget: Option<GeckoRequestBudget<'_>>,
    ) -> Result<T, DiscoveryError> {
        for attempt in 0..2 {
            if let Some(budget) = request_budget.as_mut()
                && !budget.try_acquire()
            {
                return Err(DiscoveryError::GeckoRefreshBudgetExhausted);
            }
            self.acquire().await;
            let response = self
                .http
                .get(url)
                .header(reqwest::header::ACCEPT, "application/json")
                .timeout(GECKOTERMINAL_TIMEOUT)
                .send()
                .await
                .map_err(DiscoveryError::GeckoHttp)?;
            let status = response.status();
            if status == StatusCode::TOO_MANY_REQUESTS && attempt == 0 {
                self.budget.lock().await.backoff(Instant::now());
                continue;
            }
            if !status.is_success() {
                return Err(DiscoveryError::GeckoHttpStatus(status));
            }
            let body = net::body(response).await.map_err(DiscoveryError::GeckoBody)?;
            return serde_json::from_slice(&body).map_err(DiscoveryError::GeckoJson);
        }
        unreachable!("GeckoTerminal request retries are bounded")
    }

    async fn search(
        &self,
        query: &str,
        network: &str,
        budget: Option<&mut GeckoWatchBudget>,
    ) -> Result<Vec<DexPair>, DiscoveryError> {
        let mut url = reqwest::Url::parse(&format!("{}/search/pools", self.base_url))
            .expect("GeckoTerminal search endpoint is a valid URL");
        url.query_pairs_mut()
            .append_pair("query", query)
            .append_pair("network", network)
            .append_pair("include", "base_token,quote_token,dex");
        let request_budget = budget.map(GeckoRequestBudget::Watch);
        let response: GeckoApiResponse = self.get_json(url.as_str(), request_budget).await?;
        Ok(gecko_pools_to_pairs(response, network))
    }

    async fn pools(
        &self,
        network: &str,
        pool_addresses: &[String],
    ) -> Result<Vec<DexPair>, DiscoveryError> {
        let mut pairs = Vec::new();
        for chunk in pool_addresses.chunks(GECKOTERMINAL_BATCH_SIZE) {
            if chunk.is_empty() {
                continue;
            }
            let addresses = chunk.join(",");
            let mut url = reqwest::Url::parse(&format!(
                "{}/networks/{network}/pools/multi/{addresses}",
                self.base_url
            ))
            .expect("GeckoTerminal pool endpoint is a valid URL");
            url.query_pairs_mut().append_pair("include", "base_token,quote_token,dex");
            let response: GeckoApiResponse = self.get_json(url.as_str(), None).await?;
            pairs.extend(gecko_pools_to_pairs(response, network));
        }
        Ok(pairs)
    }

    async fn registry_pools(
        &self,
        chain: Chain,
        addresses: &[String],
        registry: &Registry,
        budget: &mut GeckoRegistryBudget,
    ) -> Result<Vec<DexPair>, DiscoveryError> {
        let network = gecko_network_id(chain).ok_or(DiscoveryError::UnsupportedChain(chain))?;
        let mut pool_addresses = Vec::new();
        let mut seen_pool_addresses = HashSet::new();
        for chunk in addresses.chunks(GECKOTERMINAL_BATCH_SIZE) {
            if chunk.is_empty() {
                continue;
            }
            let mut url = reqwest::Url::parse(&format!(
                "{}/networks/{network}/tokens/multi/{}",
                self.base_url,
                chunk.join(",")
            ))
            .expect("GeckoTerminal token endpoint is a valid URL");
            url.query_pairs_mut().append_pair("include", "top_pools");
            let response: GeckoApiResponse = match self
                .get_json(
                    url.as_str(),
                    Some(GeckoRequestBudget::Registry(budget, chain, GeckoRegistryRequest::Tokens)),
                )
                .await
            {
                Ok(response) => response,
                Err(DiscoveryError::GeckoRefreshBudgetExhausted) => {
                    warn!(%chain, "GeckoTerminal registry lookup reached its request budget");
                    break;
                }
                Err(error) => return Err(error),
            };
            for token in response.data {
                if !chunk.iter().any(|address| {
                    gecko_attribute_string(&token, "address")
                        .is_some_and(|token_address| token_address.eq_ignore_ascii_case(address))
                }) {
                    continue;
                }
                let Some(top_pools) = token.relationships["top_pools"]["data"].as_array() else {
                    continue;
                };
                for pool in top_pools {
                    let Some(id) = pool["id"].as_str() else { continue };
                    let Some(address) = gecko_relationship_address(id, network) else { continue };
                    if !valid_gecko_pool_address(chain, address)
                        || !seen_pool_addresses.insert(address.to_ascii_lowercase())
                        || !budget.try_add_pool_id(chain, network, address)
                    {
                        continue;
                    }
                    pool_addresses.push(address.to_owned());
                }
            }
        }
        let mut pairs = Vec::new();
        for chunk in pool_addresses.chunks(GECKOTERMINAL_BATCH_SIZE) {
            if chunk.is_empty() {
                continue;
            }
            let addresses = chunk.join(",");
            let mut url = reqwest::Url::parse(&format!(
                "{}/networks/{network}/pools/multi/{addresses}",
                self.base_url
            ))
            .expect("GeckoTerminal pool endpoint is a valid URL");
            url.query_pairs_mut().append_pair("include", "base_token,quote_token,dex");
            let response: GeckoApiResponse = match self
                .get_json(
                    url.as_str(),
                    Some(GeckoRequestBudget::Registry(budget, chain, GeckoRegistryRequest::Pools)),
                )
                .await
            {
                Ok(response) => response,
                Err(DiscoveryError::GeckoRefreshBudgetExhausted) => {
                    warn!(%chain, "GeckoTerminal pool lookup reached its request budget");
                    break;
                }
                Err(error) => return Err(error),
            };
            pairs.extend(gecko_pools_to_pairs(response, network));
        }
        pairs.retain(|pair| {
            let base = pair.base_token.address.as_deref();
            let quote = pair.quote_token.address.as_deref();
            let matched = base
                .and_then(|address| {
                    registry::lookup(registry, chain, address)
                        .map(|entry| (entry, pair.base_token.symbol.as_deref()))
                })
                .or_else(|| {
                    quote.and_then(|address| {
                        registry::lookup(registry, chain, address)
                            .map(|entry| (entry, pair.quote_token.symbol.as_deref()))
                    })
                });
            matched.is_some_and(|(entry, symbol)| {
                symbol.is_some_and(|symbol| symbol_matches_search_ticker(symbol, &entry.ticker))
            })
        });
        Ok(pairs)
    }
}

fn gecko_attribute_string<'a>(entity: &'a GeckoEntity, field: &str) -> Option<&'a str> {
    entity.attributes.get(field)?.as_str()
}

fn gecko_relationship_id<'a>(entity: &'a GeckoEntity, field: &str) -> Option<&'a str> {
    entity.relationships.get(field)?.get("data")?.get("id")?.as_str()
}

fn gecko_relationship_address<'a>(id: &'a str, network: &str) -> Option<&'a str> {
    id.strip_prefix(network)?.strip_prefix('_')
}

fn gecko_numeric(value: Option<&serde_json::Value>) -> Option<f64> {
    let value = value?;
    value.as_f64().or_else(|| value.as_str()?.parse::<f64>().ok())
}

fn gecko_token(
    pool: &GeckoEntity,
    included: &HashMap<&str, &GeckoEntity>,
    field: &str,
    network: &str,
) -> DexToken {
    let id = gecko_relationship_id(pool, field);
    let entity = id.and_then(|id| included.get(id).copied());
    DexToken {
        address: entity
            .and_then(|entity| gecko_attribute_string(entity, "address"))
            .map(str::to_owned)
            .or_else(|| {
                id.and_then(|id| gecko_relationship_address(id, network)).map(str::to_owned)
            }),
        name: entity.and_then(|entity| gecko_attribute_string(entity, "name")).map(str::to_owned),
        symbol: entity
            .and_then(|entity| gecko_attribute_string(entity, "symbol"))
            .map(str::to_owned),
    }
}

fn gecko_pools_to_pairs(response: GeckoApiResponse, network: &str) -> Vec<DexPair> {
    let included = response
        .included
        .iter()
        .map(|entity| (entity.id.as_str(), entity))
        .collect::<HashMap<_, _>>();
    response
        .data
        .into_iter()
        .filter_map(|pool| {
            let address = gecko_attribute_string(&pool, "address")?.to_owned();
            let dex = gecko_relationship_id(&pool, "dex").and_then(|id| included.get(id).copied());
            let dex_identifier = dex.and_then(|dex| gecko_attribute_string(dex, "identifier"));
            let dex_name = dex.and_then(|dex| gecko_attribute_string(dex, "name"));
            let is_uniswap_v4 = dex_identifier.is_some_and(|identifier| {
                identifier.get(..10).is_some_and(|prefix| prefix.eq_ignore_ascii_case("uniswap-v4"))
            }) || dex_name.is_some_and(|name| {
                name.get(..10).is_some_and(|prefix| prefix.eq_ignore_ascii_case("Uniswap V4"))
            });
            let (dex_id, labels) = if is_uniswap_v4 {
                ("uniswap".to_owned(), vec!["v4".to_owned()])
            } else {
                (dex_name.or(dex_identifier).unwrap_or("unknown").to_owned(), Vec::new())
            };
            let volume =
                gecko_numeric(pool.attributes.get("volume_usd").and_then(|value| value.get("h24")));
            let reserve = gecko_numeric(pool.attributes.get("reserve_in_usd"));
            let price = gecko_numeric(pool.attributes.get("base_token_price_usd"));
            let volume =
                volume.map(|h24| DexVolume { h24: Some(h24), h6: None, h1: None, m5: None });
            let liquidity =
                reserve.map(|usd| DexLiquidity { usd: Some(usd), base: None, quote: None });
            Some(DexPair {
                chain_id: network.to_owned(),
                dex_id,
                url: Some(format!("https://www.geckoterminal.com/{network}/pools/{address}")),
                pair_address: address,
                labels,
                base_token: gecko_token(&pool, &included, "base_token", network),
                quote_token: gecko_token(&pool, &included, "quote_token", network),
                price_usd: price,
                volume,
                price_change: None,
                txns: None,
                liquidity,
                source: MarketSource::Geckoterminal,
            })
        })
        .collect()
}

#[derive(Clone)]
struct MarketDataClient {
    dex: DexScreenerClient,
    gecko: GeckoTerminalClient,
    availability: Arc<Mutex<DexAvailability>>,
}

impl MarketDataClient {
    fn new(http: reqwest::Client) -> Self {
        Self {
            dex: DexScreenerClient::new(http.clone()),
            gecko: GeckoTerminalClient::new(http),
            availability: Arc::clone(&GLOBAL_DEX_AVAILABILITY),
        }
    }

    #[cfg(test)]
    fn with_clients(dex: DexScreenerClient, gecko: GeckoTerminalClient, available: bool) -> Self {
        Self {
            dex,
            gecko,
            availability: Arc::new(Mutex::new(DexAvailability {
                checked_at: Some(Instant::now()),
                available,
            })),
        }
    }

    async fn dex_available(&self) -> bool {
        #[cfg(debug_assertions)]
        if std::env::var("QED_FORCE_DEXSCREENER_UNAVAILABLE").is_ok_and(|value| value == "1") {
            return false;
        }
        let mut availability = self.availability.lock().await;
        if availability.checked_at.is_some_and(|checked_at| checked_at.elapsed() < DEX_CANARY_TTL) {
            return availability.available;
        }
        let available = match self.dex.search(DEX_CANARY_QUERY).await {
            Ok(pairs) => !pairs.is_empty(),
            Err(error) => {
                warn!(error = %error, "DexScreener availability canary failed; using GeckoTerminal");
                false
            }
        };
        availability.checked_at = Some(Instant::now());
        availability.available = available;
        if !available {
            warn!(
                seconds = DEX_CANARY_TTL.as_secs(),
                "DexScreener USDC canary returned no pairs; using GeckoTerminal for five minutes"
            );
        }
        available
    }

    async fn registry_pools(
        &self,
        chain: Chain,
        addresses: &[String],
        registry: &Registry,
        budget: &mut GeckoRegistryBudget,
    ) -> Result<Vec<DexPair>, DiscoveryError> {
        if self.dex_available().await {
            match self.dex.tokens(chain, addresses).await {
                Ok(pairs) => return Ok(pairs),
                Err(error) => {
                    warn!(%chain, error = %error, "DexScreener discovery failed; falling back to GeckoTerminal")
                }
            }
        }
        self.gecko.registry_pools(chain, addresses, registry, budget).await
    }

    async fn pairs(
        &self,
        chain: Chain,
        pool_addresses: &[String],
    ) -> Result<Vec<DexPair>, DiscoveryError> {
        if self.dex_available().await {
            match self.dex.pairs(chain, pool_addresses).await {
                Ok(pairs) => return Ok(pairs),
                Err(error) => {
                    warn!(%chain, error = %error, "DexScreener price lookup failed; falling back to GeckoTerminal")
                }
            }
        }
        let network = gecko_network_id(chain).ok_or(DiscoveryError::UnsupportedChain(chain))?;
        self.gecko.pools(network, pool_addresses).await
    }

    async fn pair(
        &self,
        chain: Chain,
        pool_address: &str,
    ) -> Result<Option<DexPair>, DiscoveryError> {
        if self.dex_available().await {
            match self.dex.pair(chain, pool_address).await {
                Ok(pair) => return Ok(pair),
                Err(error) => {
                    warn!(%chain, error = %error, "DexScreener featured lookup failed; falling back to GeckoTerminal")
                }
            }
        }
        let network = gecko_network_id(chain).ok_or(DiscoveryError::UnsupportedChain(chain))?;
        let pools = self.gecko.pools(network, &[pool_address.to_owned()]).await?;
        Ok(pools.into_iter().find(|pair| pair.pair_address.eq_ignore_ascii_case(pool_address)))
    }

    async fn gecko_search(
        &self,
        query: &str,
        network: &str,
        budget: &mut GeckoWatchBudget,
    ) -> Result<Vec<DexPair>, DiscoveryError> {
        self.gecko.search(query, network, Some(budget)).await
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct CuratedPool {
    pub chain: Chain,
    pub dex: String,
    pub pool: String,
    pub note: String,
    pub source_url: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiscoveryCandidate {
    pub chain: Chain,
    pub dex: String,
    pub pool: String,
    pub base_symbol: String,
    pub base_address: String,
    pub quote_symbol: String,
    pub quote_address: String,
    pub issuer: Option<String>,
    pub ticker: Option<String>,
    pub volume_24h_usd: Option<f64>,
    pub liquidity_usd: Option<f64>,
    pub curated: bool,
    pub note: Option<String>,
}
#[derive(Debug, Clone, PartialEq)]
struct LeaderboardCandidate {
    chain: Chain,
    dex: String,
    pool: String,
    base_symbol: String,
    quote_symbol: String,
    issuer: Option<String>,
    ticker: Option<String>,
    issuer_on_base: bool,
    price_usd: Option<f64>,
    change_24h_pct: Option<f64>,
    volume_24h_usd: Option<f64>,
    liquidity_usd: Option<f64>,
    txns_24h: Option<u64>,
    trade_url: String,
    source: MarketSource,
}

struct DiscoveryBatch {
    featured: Vec<DiscoveryCandidate>,
    leaderboard: Vec<LeaderboardCandidate>,
}

fn dex_chain_id(chain: Chain) -> Option<&'static str> {
    match chain {
        Chain::Solana => Some("solana"),
        Chain::RobinhoodChain => Some("robinhood"),
        Chain::Base => Some("base"),
        Chain::Ethereum => Some("ethereum"),
        Chain::Bnb => Some("bsc"),
    }
}
pub fn chain_from_dex_id(chain_id: &str) -> Option<Chain> {
    match chain_id.to_ascii_lowercase().as_str() {
        "solana" => Some(Chain::Solana),
        "robinhood" | "robinhood-chain" | "robinhoodchain" | "4663" => Some(Chain::RobinhoodChain),
        "base" | "8453" => Some(Chain::Base),
        "ethereum" | "eth" | "1" => Some(Chain::Ethereum),
        "bsc" | "bnb" | "56" => Some(Chain::Bnb),
        _ => None,
    }
}
pub fn chain_slug(chain: Chain) -> &'static str {
    match chain {
        Chain::Solana => "solana",
        Chain::RobinhoodChain => "robinhood",
        Chain::Base => "base",
        Chain::Ethereum => "ethereum",
        Chain::Bnb => "bnb",
    }
}

pub fn chain_label(chain: Chain) -> &'static str {
    match chain {
        Chain::Solana => "Solana",
        Chain::RobinhoodChain => "Robinhood Chain",
        Chain::Base => "Base",
        Chain::Ethereum => "Ethereum",
        Chain::Bnb => "BNB Chain",
    }
}
pub fn explorer_url(chain: Chain, pool: &str) -> String {
    let base = match chain {
        Chain::Solana => "https://solscan.io/account/",
        Chain::RobinhoodChain => "https://robinhoodchain.blockscout.com/address/",
        Chain::Base => "https://basescan.org/address/",
        Chain::Ethereum => "https://etherscan.io/address/",
        Chain::Bnb => "https://bscscan.com/address/",
    };
    format!("{base}{pool}")
}

pub fn dex_pair_url(chain: Chain, pool: &str) -> String {
    let chain_id = dex_chain_id(chain).unwrap_or_default();
    format!("https://dexscreener.com/{chain_id}/{pool}")
}

fn gecko_network_id(chain: Chain) -> Option<&'static str> {
    match chain {
        Chain::Solana => Some("solana"),
        Chain::RobinhoodChain => Some("robinhood"),
        Chain::Base => Some("base"),
        Chain::Ethereum => Some("eth"),
        Chain::Bnb => Some("bsc"),
    }
}
fn gecko_chain_budget_index(chain: Chain) -> usize {
    match chain {
        Chain::Solana => 0,
        Chain::RobinhoodChain => 1,
        Chain::Base => 2,
        Chain::Ethereum => 3,
        Chain::Bnb => 4,
    }
}

fn valid_token_address(chain: Chain, address: &str) -> bool {
    match chain {
        Chain::Solana => Chain::decode_solana_address(address).is_some(),
        Chain::RobinhoodChain | Chain::Base | Chain::Ethereum | Chain::Bnb => {
            Chain::is_evm_address(address)
        }
    }
}

fn valid_gecko_pool_address(chain: Chain, address: &str) -> bool {
    match chain {
        Chain::Solana => valid_token_address(chain, address),
        Chain::RobinhoodChain | Chain::Base | Chain::Ethereum | Chain::Bnb => {
            Chain::is_evm_address(address) || Chain::is_v4_pool_id(address)
        }
    }
}

fn valid_publisher_catalog_snapshot_hash(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_pair_address(chain: Chain, pair: &DexPair) -> bool {
    valid_token_address(chain, &pair.pair_address)
        || (chain != Chain::Solana
            && pair.dex_id.eq_ignore_ascii_case("uniswap")
            && Chain::is_v4_pool_id(&pair.pair_address)
            && pair.labels.iter().any(|label| label.trim().eq_ignore_ascii_case("v4")))
}
fn same_pool_path(chain: Chain, path: &str, pool: &str) -> bool {
    if chain == Chain::Solana { path == pool } else { path.eq_ignore_ascii_case(pool) }
}

fn trusted_market_url(url: &str, chain: Chain, pool: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else { return false };
    let authority =
        url.split_once("://").and_then(|(_, rest)| rest.split('/').next()).unwrap_or_default();
    let authority_host_port = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
    let has_explicit_userinfo = authority.contains('@');
    let has_explicit_port = authority_host_port.contains(':');
    if parsed.scheme() != "https"
        || has_explicit_port
        || has_explicit_userinfo
        || parsed.username() != ""
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return false;
    }
    let Some(host) = parsed.host_str() else { return false };
    let segments = parsed.path_segments().map(|segments| segments.collect::<Vec<_>>());
    match host {
        "dexscreener.com" | "www.dexscreener.com" => {
            let Some(segments) = segments else { return false };
            segments.len() == 2
                && segments[0].eq_ignore_ascii_case(dex_chain_id(chain).unwrap_or_default())
                && same_pool_path(chain, segments[1], pool)
        }
        "geckoterminal.com" | "www.geckoterminal.com" => {
            let Some(network) = gecko_network_id(chain) else { return false };
            let Some(segments) = segments else { return false };
            segments.len() == 3
                && segments[0].eq_ignore_ascii_case(network)
                && segments[1].eq_ignore_ascii_case("pools")
                && same_pool_path(chain, segments[2], pool)
        }
        _ => false,
    }
}

/// Return a source URL only when it is an exact, HTTPS market page for the
/// supplied chain and pool. Old or malformed persisted URLs fall back to the
/// canonical DexScreener pair page.
pub fn canonical_market_url(chain: Chain, pool: &str, source_url: Option<&str>) -> String {
    canonical_market_url_for_source(chain, pool, source_url, MarketSource::Dexscreener)
}

pub(crate) fn canonical_market_url_for_source(
    chain: Chain,
    pool: &str,
    source_url: Option<&str>,
    source: MarketSource,
) -> String {
    source_url.filter(|url| trusted_market_url(url, chain, pool)).map(str::to_owned).unwrap_or_else(
        || match source {
            MarketSource::Dexscreener => dex_pair_url(chain, pool),
            MarketSource::Geckoterminal => format!(
                "https://www.geckoterminal.com/{}/pools/{pool}",
                gecko_network_id(chain).unwrap_or_default()
            ),
        },
    )
}

pub fn load_curated(path: impl AsRef<Path>) -> Result<Vec<CuratedPool>, DiscoveryError> {
    let bytes = std::fs::read(path)?;
    Ok(serde_json::from_slice(&bytes)?)
}
fn pair_metadata_is_bounded(pair: &DexPair) -> bool {
    pair.dex_id.len() <= MAX_LEADERBOARD_LABEL_BYTES
        && pair
            .base_token
            .symbol
            .as_deref()
            .is_none_or(|symbol| symbol.len() <= MAX_LEADERBOARD_LABEL_BYTES)
        && pair
            .quote_token
            .symbol
            .as_deref()
            .is_none_or(|symbol| symbol.len() <= MAX_LEADERBOARD_LABEL_BYTES)
        && pair
            .base_token
            .name
            .as_deref()
            .is_none_or(|name| name.len() <= MAX_LEADERBOARD_LABEL_BYTES)
        && pair
            .quote_token
            .name
            .as_deref()
            .is_none_or(|name| name.len() <= MAX_LEADERBOARD_LABEL_BYTES)
}
fn candidate_from_pair(pair: &DexPair, registry: &Registry) -> Option<DiscoveryCandidate> {
    let chain = chain_from_dex_id(&pair.chain_id)?;
    let base_address = pair.base_token.address.clone()?;
    let quote_address = pair.quote_token.address.clone()?;
    if !pair_metadata_is_bounded(pair)
        || !valid_pair_address(chain, pair)
        || !valid_token_address(chain, &base_address)
        || !valid_token_address(chain, &quote_address)
    {
        return None;
    }
    let quote_entry = registry::lookup(registry, chain, &quote_address);
    let base_entry = registry::lookup(registry, chain, &base_address);
    let matched = quote_entry.or(base_entry)?;
    if matched.issuer.len() > MAX_LEADERBOARD_LABEL_BYTES
        || matched.ticker.len() > MAX_LEADERBOARD_LABEL_BYTES
    {
        return None;
    }
    let liquidity_usd = pair.liquidity.as_ref().and_then(|liquidity| liquidity.usd);
    if liquidity_usd.is_none_or(|liquidity| liquidity <= MIN_LIQUIDITY_USD) {
        return None;
    }
    Some(DiscoveryCandidate {
        chain,
        dex: pair.dex_id.clone(),
        pool: pair.pair_address.clone(),
        base_symbol: pair.base_token.symbol.clone().unwrap_or_default(),
        base_address,
        quote_symbol: pair.quote_token.symbol.clone().unwrap_or_default(),
        quote_address,
        issuer: Some(matched.issuer.clone()),
        ticker: Some(matched.ticker.clone()),
        volume_24h_usd: pair.volume.as_ref().and_then(|volume| volume.h24),
        liquidity_usd,
        curated: false,
        note: None,
    })
}

/// Convert recorded DexScreener pairs to candidates, requiring a registry
/// address on either side and a strictly positive discovery liquidity floor.
#[cfg(test)]
fn candidates_from_pairs(
    registry: &Registry,
    pairs: impl IntoIterator<Item = DexPair>,
) -> Vec<DiscoveryCandidate> {
    pairs.into_iter().filter_map(|pair| candidate_from_pair(&pair, registry)).collect()
}

fn leaderboard_candidate_from_pair(
    pair: &DexPair,
    registry: &Registry,
) -> Option<LeaderboardCandidate> {
    let chain = chain_from_dex_id(&pair.chain_id)?;
    let base_address = pair.base_token.address.as_deref()?;
    let quote_address = pair.quote_token.address.as_deref()?;
    if !pair_metadata_is_bounded(pair)
        || !valid_pair_address(chain, pair)
        || !valid_token_address(chain, base_address)
        || !valid_token_address(chain, quote_address)
    {
        return None;
    }
    let quote_entry = registry::lookup(registry, chain, quote_address);
    let base_entry = registry::lookup(registry, chain, base_address);
    let (matched, issuer_on_base) = match quote_entry {
        Some(matched) => (matched, false),
        None => (base_entry?, true),
    };
    if matched.issuer.len() > MAX_LEADERBOARD_LABEL_BYTES
        || matched.ticker.len() > MAX_LEADERBOARD_LABEL_BYTES
    {
        return None;
    }
    let trade_url = canonical_market_url_for_source(
        chain,
        &pair.pair_address,
        pair.url.as_deref(),
        pair.source,
    );
    if trade_url.len() > MAX_IMPOSTOR_URL_BYTES {
        return None;
    }
    let txns_24h = pair
        .txns
        .as_ref()
        .and_then(|txns| txns.h24.as_ref())
        .and_then(|window| window.buys?.checked_add(window.sells?));
    Some(LeaderboardCandidate {
        chain,
        dex: pair.dex_id.clone(),
        pool: pair.pair_address.clone(),
        base_symbol: pair.base_token.symbol.clone().unwrap_or_default(),
        quote_symbol: pair.quote_token.symbol.clone().unwrap_or_default(),
        issuer: Some(matched.issuer.clone()),
        ticker: Some(matched.ticker.clone()),
        issuer_on_base,
        price_usd: pair.price_usd,
        change_24h_pct: pair.price_change.as_ref().and_then(|change| change.h24),
        volume_24h_usd: pair.volume.as_ref().and_then(|volume| volume.h24),
        liquidity_usd: pair.liquidity.as_ref().and_then(|liquidity| liquidity.usd),
        txns_24h,
        trade_url,
        source: pair.source,
    })
}

#[cfg(test)]
fn leaderboard_candidates_from_pairs(
    registry: &Registry,
    pairs: impl IntoIterator<Item = DexPair>,
) -> Vec<LeaderboardCandidate> {
    pairs.into_iter().filter_map(|pair| leaderboard_candidate_from_pair(&pair, registry)).collect()
}

fn dedup_leaderboard_candidates(
    candidates: impl IntoIterator<Item = LeaderboardCandidate>,
) -> Vec<LeaderboardCandidate> {
    let mut by_pool: HashMap<(Chain, String), LeaderboardCandidate> = HashMap::new();
    for candidate in candidates {
        let key = (candidate.chain, candidate.pool.to_ascii_lowercase());
        match by_pool.get_mut(&key) {
            None => {
                by_pool.insert(key, candidate);
            }
            Some(existing) => {
                let candidate_volume = candidate.volume_24h_usd.unwrap_or_default();
                let existing_volume = existing.volume_24h_usd.unwrap_or_default();
                let candidate_liquidity = candidate.liquidity_usd.unwrap_or_default();
                let existing_liquidity = existing.liquidity_usd.unwrap_or_default();
                if (candidate_volume, candidate_liquidity) > (existing_volume, existing_liquidity) {
                    *existing = candidate;
                }
            }
        }
    }
    by_pool.into_values().collect()
}

fn truncate_leaderboard_candidates(
    mut candidates: Vec<LeaderboardCandidate>,
) -> Vec<LeaderboardCandidate> {
    candidates.sort_by(|left, right| match (left.volume_24h_usd, right.volume_24h_usd) {
        (Some(left_volume), Some(right_volume)) => right_volume
            .total_cmp(&left_volume)
            .then_with(|| left.pool.to_ascii_lowercase().cmp(&right.pool.to_ascii_lowercase())),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => left.pool.to_ascii_lowercase().cmp(&right.pool.to_ascii_lowercase()),
    });
    candidates.truncate(MAX_LEADERBOARD_CANDIDATES);
    candidates
}

fn candidate_from_curated(pool: CuratedPool) -> DiscoveryCandidate {
    DiscoveryCandidate {
        chain: pool.chain,
        dex: pool.dex,
        pool: pool.pool,
        base_symbol: String::new(),
        base_address: String::new(),
        quote_symbol: String::new(),
        quote_address: String::new(),
        issuer: None,
        ticker: None,
        volume_24h_usd: None,
        liquidity_usd: None,
        curated: true,
        note: Some(pool.note),
    }
}

/// Deduplicate by chain and pool address. Curated candidates retain their
/// featured-file order and always precede discovered candidates; discovered
/// candidates are sorted by descending liquidity into the remaining slots.
pub fn dedup_and_order(
    candidates: impl IntoIterator<Item = DiscoveryCandidate>,
) -> Vec<DiscoveryCandidate> {
    let mut by_pool: HashMap<(Chain, String), DiscoveryCandidate> = HashMap::new();
    let mut curated_keys = Vec::new();
    for candidate in candidates {
        let key = (candidate.chain, candidate.pool.to_ascii_lowercase());
        match by_pool.get_mut(&key) {
            None => {
                if candidate.curated {
                    curated_keys.push(key.clone());
                }
                by_pool.insert(key, candidate);
            }
            Some(existing) if candidate.curated && !existing.curated => {
                *existing = candidate;
                curated_keys.push(key.clone());
            }
            Some(existing) if existing.curated || candidate.curated => {}
            Some(existing)
                if existing.liquidity_usd.unwrap_or_default()
                    >= candidate.liquidity_usd.unwrap_or_default() => {}
            Some(existing) => {
                *existing = candidate;
            }
        }
    }

    let mut ordered =
        curated_keys.into_iter().filter_map(|key| by_pool.remove(&key)).collect::<Vec<_>>();
    let mut discovered = by_pool.into_values().collect::<Vec<_>>();
    discovered.sort_by(|left, right| {
        right
            .liquidity_usd
            .unwrap_or_default()
            .total_cmp(&left.liquidity_usd.unwrap_or_default())
            .then_with(|| left.pool.to_ascii_lowercase().cmp(&right.pool.to_ascii_lowercase()))
    });
    discovered.truncate(MAX_FEATURED_POOLS.saturating_sub(ordered.len()));
    ordered.extend(discovered);
    ordered
}

fn verdict_label(verdict: &Verdict) -> &'static str {
    match verdict {
        Verdict::Verified { .. } => "verified",
        Verdict::Mismatch { .. } => "mismatch",
        Verdict::NoMatch => "nomatch",
        Verdict::Unknown { .. } => "unknown",
    }
}
fn checked_read_status() -> String {
    "checked".to_owned()
}

fn leaderboard_read_status(issue: Option<CheckReadIssue>) -> (&'static str, Option<&'static str>) {
    match issue {
        Some(CheckReadIssue::UnsupportedVenue) => ("unsupported_venue", Some("unsupported_venue")),
        Some(CheckReadIssue::RpcLimit) => ("not_read_yet", Some("rpc_limit")),
        Some(CheckReadIssue::Transient) => ("not_read_yet", Some("transient")),
        Some(CheckReadIssue::Unsupported) => ("not_read_yet", Some("unsupported")),
        None => ("checked", None),
    }
}
#[cfg(test)]
mod read_status_tests {
    use super::*;

    #[test]
    fn read_status_uses_typed_issues_and_separates_unsupported_venues() {
        assert_eq!(
            leaderboard_read_status(Some(CheckReadIssue::UnsupportedVenue)),
            ("unsupported_venue", Some("unsupported_venue"))
        );
        assert_eq!(
            leaderboard_read_status(Some(CheckReadIssue::RpcLimit)),
            ("not_read_yet", Some("rpc_limit"))
        );
        assert_eq!(
            leaderboard_read_status(Some(CheckReadIssue::Transient)),
            ("not_read_yet", Some("transient"))
        );
        assert_eq!(
            leaderboard_read_status(Some(CheckReadIssue::Unsupported)),
            ("not_read_yet", Some("unsupported"))
        );
        assert_eq!(leaderboard_read_status(None), ("checked", None));
    }
}

fn side_values(
    pool: Option<&PoolInfo>,
    candidate: &DiscoveryCandidate,
) -> (String, String, String, String) {
    let base_symbol = pool
        .and_then(|pool| pool.base.symbol.clone())
        .unwrap_or_else(|| candidate.base_symbol.clone());
    let base_address = pool
        .map(|pool| pool.base.address.clone())
        .unwrap_or_else(|| candidate.base_address.clone());
    let quote_symbol = pool
        .and_then(|pool| pool.quote.symbol.clone())
        .unwrap_or_else(|| candidate.quote_symbol.clone());
    let quote_address = pool
        .map(|pool| pool.quote.address.clone())
        .unwrap_or_else(|| candidate.quote_address.clone());
    (base_symbol, base_address, quote_symbol, quote_address)
}

fn featured_from_check(candidate: DiscoveryCandidate, result: CheckResult) -> FeaturedPool {
    let pool = result.pool.as_ref();
    let quote_balance = pool.and_then(|pool| {
        format_balance(
            pool.quote.balance.as_deref()?,
            pool.quote.decimals,
            pool.quote.symbol.as_deref(),
        )
    });
    let (base_symbol, base_address, quote_symbol, quote_address) = side_values(pool, &candidate);
    let note =
        candidate.note.filter(|note| !note.trim().is_empty()).or_else(|| match &result.verdict {
            Verdict::Unknown { reason } => Some(reason.clone()),
            _ => None,
        });
    FeaturedPool {
        chain: candidate.chain,
        dex: pool.map(|pool| pool.dex.clone()).unwrap_or(candidate.dex),
        pool: candidate.pool,
        base_symbol,
        base_address,
        quote_symbol,
        quote_address,
        issuer: candidate.issuer,
        ticker: candidate.ticker,
        verdict: verdict_label(&result.verdict).to_owned(),
        quote_balance,
        quote_share_of_supply: result.quote_share_of_supply,
        volume_24h_usd: candidate.volume_24h_usd,
        liquidity_usd: candidate.liquidity_usd,
        curated: candidate.curated,
        note,
        updated_at: result.checked_at,
    }
}

fn should_retry_empty_discovery(attempted: usize, failed: usize, candidates: usize) -> bool {
    candidates == 0 && attempted > 0 && failed > 0
}

async fn discover_registry(
    state: &AppState,
    registry: &Registry,
) -> Result<DiscoveryBatch, DiscoveryError> {
    let client = MarketDataClient::new(state.http.clone());
    let mut gecko_budget = GeckoRegistryBudget::default();
    let mut featured = Vec::new();
    let mut leaderboard = Vec::new();
    let mut attempted = 0;
    let mut succeeded = 0;
    let mut failed = 0;
    for chain in [Chain::Solana, Chain::RobinhoodChain, Chain::Base, Chain::Ethereum, Chain::Bnb] {
        let mut addresses = Vec::new();
        for entry in
            registry.iter().filter(|entry| entry.chain == chain && registry::matchable(entry))
        {
            if !addresses
                .iter()
                .any(|address: &String| address.eq_ignore_ascii_case(&entry.contract))
            {
                addresses.push(entry.contract.clone());
            }
        }
        if addresses.is_empty() {
            info!(chain = %chain, candidates = 0usize, "shared pool discovery");
            continue;
        }
        attempted += 1;
        match client.registry_pools(chain, &addresses, registry, &mut gecko_budget).await {
            Ok(pairs) => {
                succeeded += 1;
                let mut chain_featured = pairs
                    .iter()
                    .filter_map(|pair| candidate_from_pair(pair, registry))
                    .collect::<Vec<_>>();
                let mut chain_leaderboard = pairs
                    .iter()
                    .filter_map(|pair| leaderboard_candidate_from_pair(pair, registry))
                    .collect::<Vec<_>>();
                info!(
                    chain = %chain,
                    featured = chain_featured.len(),
                    leaderboard = chain_leaderboard.len(),
                    "shared pool discovery"
                );
                featured.append(&mut chain_featured);
                leaderboard.append(&mut chain_leaderboard);
            }
            Err(error) if is_dex_blocked(&error) => return Err(error),
            Err(error) => {
                failed += 1;
                warn!(%chain, error = %error, "market-data registry discovery failed");
            }
        }
    }
    let candidates = featured.len() + leaderboard.len();
    if should_retry_empty_discovery(attempted, failed, candidates) {
        warn!(attempted, succeeded, failed, "all discovered pools were unavailable");
        return Err(DiscoveryError::ChainRequestsFailed { attempted, failed });
    }
    Ok(DiscoveryBatch {
        featured,
        leaderboard: truncate_leaderboard_candidates(dedup_leaderboard_candidates(leaderboard)),
    })
}

async fn defer_featured_refresh(state: &AppState, reason: &str, delay_secs: u64) {
    let mut status = state.featured_status.write().await;
    status.next_refresh_at = timestamp_after(delay_secs);
    status.refreshing = false;
    warn!(reason, "keeping previous featured board after an unsuccessful refresh");
}

async fn refresh_featured(
    state: &AppState,
    data_dir: &Path,
    discovered: Vec<DiscoveryCandidate>,
    shared_checks: &HashMap<(Chain, String), CheckResult>,
) {
    let mut candidates = match load_curated("registry/featured.json") {
        Ok(curated) => curated.into_iter().map(candidate_from_curated).collect(),
        Err(error) => {
            warn!(error = %error, "could not load curated featured pools");
            Vec::new()
        }
    };
    candidates.extend(discovered);
    let client = MarketDataClient::new(state.http.clone());
    let mut enriched = Vec::new();
    for mut candidate in dedup_and_order(candidates) {
        if candidate.liquidity_usd.is_none() {
            match client.pair(candidate.chain, &candidate.pool).await {
                Ok(Some(pair)) => {
                    if let Some(liquidity) =
                        pair.liquidity.as_ref().and_then(|liquidity| liquidity.usd)
                    {
                        candidate.liquidity_usd = Some(liquidity);
                    }
                    candidate.volume_24h_usd = pair.volume.and_then(|volume| volume.h24);
                    if candidate.base_symbol.is_empty() {
                        candidate.base_symbol = pair.base_token.symbol.clone().unwrap_or_default();
                    }
                    if candidate.base_address.is_empty() {
                        candidate.base_address =
                            pair.base_token.address.clone().unwrap_or_default();
                    }
                    if candidate.quote_symbol.is_empty() {
                        candidate.quote_symbol =
                            pair.quote_token.symbol.clone().unwrap_or_default();
                    }
                    if candidate.quote_address.is_empty() {
                        candidate.quote_address =
                            pair.quote_token.address.clone().unwrap_or_default();
                    }
                }
                Ok(None) => {}
                Err(error) if is_dex_blocked(&error) => {
                    defer_featured_refresh(
                        state,
                        "DexScreener request budget unavailable",
                        DISCOVERY_REFRESH_SECS,
                    )
                    .await;
                    return;
                }
                Err(error) => {
                    warn!(error = %error, "DexScreener featured pool lookup failed")
                }
            }
        }
        enriched.push(candidate);
    }

    let mut featured = Vec::new();
    for (index, candidate) in dedup_and_order(enriched).into_iter().enumerate() {
        if index > 0 {
            sleep(REQUEST_INTERVAL).await;
        }
        let key = (candidate.chain, candidate.pool.to_ascii_lowercase());
        let result = match shared_checks.get(&key) {
            Some(result) => result.clone(),
            None => check::check(&state.app, &candidate.pool).await,
        };
        featured.push(featured_from_check(candidate, result));
    }
    let previous_count = state.featured.read().await.len();
    if featured.is_empty() {
        if previous_count > 0 {
            defer_featured_refresh(
                state,
                "discovery returned an empty board",
                DISCOVERY_REFRESH_SECS,
            )
            .await;
            return;
        }
        let updated_at = now_rfc3339();
        *state.featured_status.write().await = FeaturedStatus {
            updated_at,
            next_refresh_at: timestamp_after(DISCOVERY_REFRESH_SECS),
            restored: false,
            refreshing: false,
            empty_successful: true,
        };
        return;
    }
    if !safe_board_replacement(previous_count, featured.len()) {
        defer_featured_refresh(state, "discovery returned too few pools", DISCOVERY_REFRESH_SECS)
            .await;
        return;
    }
    let updated_at = now_rfc3339();
    let snapshot = FeaturedSnapshot {
        updated_at: updated_at.clone(),
        pools: featured,
        next_refresh_at: timestamp_after(DISCOVERY_REFRESH_SECS),
        restored: false,
        refreshing: false,
        empty_successful: false,
    };
    save_featured(data_dir, &snapshot);
    state.board_store.persist_featured(&snapshot).await;
    *state.featured.write().await = snapshot.pools;
    *state.featured_status.write().await = FeaturedStatus {
        updated_at,
        next_refresh_at: snapshot.next_refresh_at,
        restored: false,
        refreshing: false,
        empty_successful: false,
    };
}

async fn cached_leaderboard_check(
    state: &AppState,
    candidate: &LeaderboardCandidate,
    force_refresh: bool,
) -> CheckResult {
    let key = (candidate.chain, candidate.pool.to_ascii_lowercase());
    if force_refresh {
        state.leaderboard_check_cache.invalidate(&key).await;
        state.app.check_cache.invalidate(&crate::domain::check::cache_key(&candidate.pool)).await;
    }
    if let Some(result) = state.leaderboard_check_cache.get(&key).await {
        return result;
    }
    let result = check::check(&state.app, &candidate.pool).await;
    state.leaderboard_check_cache.insert(key, result.clone()).await;
    result
}

fn provisional_leaderboard_entry(
    rank: usize,
    candidate: &LeaderboardCandidate,
) -> LeaderboardEntry {
    LeaderboardEntry {
        rank,
        chain: chain_slug(candidate.chain).to_owned(),
        chain_label: chain_label(candidate.chain).to_owned(),
        dex: candidate.dex.clone(),
        pool: candidate.pool.clone(),
        base_symbol: candidate.base_symbol.clone(),
        quote_symbol: candidate.quote_symbol.clone(),
        issuer: candidate.issuer.clone(),
        ticker: candidate.ticker.clone(),
        issuer_on_base: Some(candidate.issuer_on_base),
        verdict: "unknown".to_owned(),
        read_status: "not_read_yet".to_owned(),
        read_reason: None,
        price_usd: candidate.price_usd,
        change_24h_pct: candidate.change_24h_pct,
        volume_24h_usd: candidate.volume_24h_usd,
        liquidity_usd: candidate.liquidity_usd,
        txns_24h: candidate.txns_24h,
        source: candidate.source,
        detail_url: format!("/validated/{}/{}", chain_slug(candidate.chain), candidate.pool),
        trade_url: candidate.trade_url.clone(),
        explorer_url: explorer_url(candidate.chain, &candidate.pool),
        attestation_id: None,
        checked_at: None,
    }
}

fn leaderboard_entry(
    rank: usize,
    candidate: LeaderboardCandidate,
    result: CheckResult,
) -> LeaderboardEntry {
    let pool = candidate.pool.clone();
    let (read_status, read_reason) = leaderboard_read_status(result.read_issue);
    LeaderboardEntry {
        rank,
        chain: chain_slug(candidate.chain).to_owned(),
        chain_label: chain_label(candidate.chain).to_owned(),
        dex: candidate.dex,
        pool,
        base_symbol: candidate.base_symbol,
        quote_symbol: candidate.quote_symbol,
        issuer: candidate.issuer,
        ticker: candidate.ticker,
        issuer_on_base: Some(candidate.issuer_on_base),
        verdict: verdict_label(&result.verdict).to_owned(),
        read_status: read_status.to_owned(),
        read_reason: read_reason.map(str::to_owned),
        price_usd: candidate.price_usd,
        change_24h_pct: candidate.change_24h_pct,
        volume_24h_usd: candidate.volume_24h_usd,
        liquidity_usd: candidate.liquidity_usd,
        txns_24h: candidate.txns_24h,
        detail_url: format!("/validated/{}/{}", chain_slug(candidate.chain), candidate.pool),
        trade_url: candidate.trade_url.clone(),
        explorer_url: explorer_url(candidate.chain, &candidate.pool),
        source: candidate.source,
        attestation_id: result.attestation_id,
        checked_at: Some(result.checked_at),
    }
}

fn rank_leaderboard_candidates(
    mut candidates: Vec<LeaderboardCandidate>,
) -> Vec<LeaderboardCandidate> {
    candidates.sort_by(|left, right| match (left.volume_24h_usd, right.volume_24h_usd) {
        (Some(left_volume), Some(right_volume)) => right_volume
            .total_cmp(&left_volume)
            .then_with(|| left.pool.to_ascii_lowercase().cmp(&right.pool.to_ascii_lowercase())),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => left.pool.to_ascii_lowercase().cmp(&right.pool.to_ascii_lowercase()),
    });
    candidates.truncate(MAX_LEADERBOARD_CANDIDATES);
    candidates
}
fn symbol_matches_search_ticker(symbol: &str, ticker: &str) -> bool {
    let symbol = symbol.trim();
    let symbol_bytes = symbol.as_bytes();
    let ticker_bytes = ticker.as_bytes();
    symbol.eq_ignore_ascii_case(ticker)
        || (symbol_bytes.len() == ticker_bytes.len() + 1
            && symbol_bytes[..ticker_bytes.len()].eq_ignore_ascii_case(ticker_bytes)
            && symbol_bytes.last().is_some_and(|byte| byte.eq_ignore_ascii_case(&b'x')))
}

fn active_search_tickers(registry: &Registry) -> Vec<String> {
    let mut tickers = registry
        .iter()
        .filter(|entry| registry::matchable(entry))
        .map(|entry| entry.ticker.clone())
        .filter(|ticker| !ticker.trim().is_empty() && ticker.len() <= MAX_IMPOSTOR_LABEL_BYTES)
        .collect::<Vec<_>>();
    tickers.sort_by_key(|ticker| ticker.to_ascii_lowercase());
    tickers.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
    tickers
}
fn registry_product_names<'a>(registry: &'a Registry, ticker: &str) -> Vec<&'a str> {
    let mut names = Vec::new();
    for entry in registry
        .iter()
        .filter(|entry| registry::matchable(entry) && entry.ticker.eq_ignore_ascii_case(ticker))
    {
        let name = entry.name.trim();
        if name.is_empty()
            || name.len() > MAX_IMPOSTOR_LABEL_BYTES
            || names.iter().any(|existing: &&str| existing.eq_ignore_ascii_case(name))
        {
            continue;
        }
        names.push(name);
    }
    names
}

async fn search_watch_products(
    client: &MarketDataClient,
    ticker: &str,
    product_names: &[&str],
    budget: &mut GeckoWatchBudget,
    searches_made: &mut usize,
    failed_queries: &mut usize,
) -> Vec<DexPair> {
    let mut pairs = Vec::new();
    'products: for product_name in product_names {
        for network in GECKOTERMINAL_WATCH_NETWORKS {
            if budget.requests >= GECKOTERMINAL_WATCH_SEARCHES_PER_REFRESH {
                break 'products;
            }
            *searches_made += 1;
            match client.gecko_search(product_name, network, budget).await {
                Ok(mut found) => pairs.append(&mut found),
                Err(DiscoveryError::GeckoRefreshBudgetExhausted) => break 'products,
                Err(error) => {
                    *failed_queries += 1;
                    warn!(ticker, %network, error = %error, "GeckoTerminal impostor search failed; continuing with the remaining networks");
                }
            }
        }
    }
    pairs
}

fn prioritize_search_tickers(registry: &Registry, board: &[LeaderboardEntry]) -> Vec<String> {
    let mut tickers = active_search_tickers(registry);
    tickers.sort_by(|left, right| {
        let volume = |ticker: &str| {
            board
                .iter()
                .filter(|entry| {
                    symbol_matches_search_ticker(&entry.base_symbol, ticker)
                        || symbol_matches_search_ticker(&entry.quote_symbol, ticker)
                })
                .filter_map(|entry| entry.volume_24h_usd)
                .max_by(f64::total_cmp)
        };
        match (volume(left), volume(right)) {
            (Some(left), Some(right)) => right.total_cmp(&left),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => left.to_ascii_lowercase().cmp(&right.to_ascii_lowercase()),
        }
    });
    tickers
}

fn rotating_impostor_search_terms(
    tickers: &[String],
    rotation_offset: usize,
) -> (Vec<(String, String)>, usize) {
    let hot_ticker_count = tickers.len().min(IMPOSTOR_HOT_TICKERS_PER_REFRESH);
    let hot_search_count = hot_ticker_count * 2;
    let tail_tickers = &tickers[hot_ticker_count..];
    let tail_search_count = tail_tickers.len() * 2;
    let tail_budget = MAX_IMPOSTOR_SEARCHES_PER_REFRESH.saturating_sub(hot_search_count);
    let tail_query_count = tail_budget.min(tail_search_count);
    let start = if tail_search_count == 0 { 0 } else { rotation_offset % tail_search_count };
    let next_offset =
        if tail_search_count == 0 { 0 } else { (start + tail_query_count) % tail_search_count };
    let mut searches = Vec::with_capacity(hot_search_count + tail_query_count);
    for ticker in &tickers[..hot_ticker_count] {
        if ticker.len() >= MIN_DEXSCREENER_SEARCH_BYTES {
            searches.push((ticker.clone(), ticker.clone()));
        }
        searches.push((ticker.clone(), format!("{ticker}x")));
    }
    for offset in 0..tail_query_count {
        let term_index = (start + offset) % tail_search_count;
        let ticker = &tail_tickers[term_index / 2];
        if term_index % 2 == 0 && ticker.len() < MIN_DEXSCREENER_SEARCH_BYTES {
            continue;
        }
        let search = if term_index % 2 == 0 { ticker.clone() } else { format!("{ticker}x") };
        searches.push((ticker.clone(), search));
    }
    (searches, next_offset)
}

fn merge_unsupported_candidates(
    mut entries: Vec<UnsupportedImpostorCandidate>,
    detected: Vec<UnsupportedImpostorCandidate>,
) -> (Vec<UnsupportedImpostorCandidate>, usize) {
    for mut detected in detected {
        if let Some(existing) = entries.iter_mut().find(|existing| {
            existing.dex_chain_id.eq_ignore_ascii_case(&detected.dex_chain_id)
                && existing.address.eq_ignore_ascii_case(&detected.address)
        }) {
            detected.first_seen_at.clone_from(&existing.first_seen_at);
            *existing = detected;
        } else {
            entries.push(detected);
        }
    }
    entries.sort_by(|left, right| {
        right
            .last_seen_at
            .cmp(&left.last_seen_at)
            .then_with(|| left.dex_chain_id.cmp(&right.dex_chain_id))
            .then_with(|| left.address.cmp(&right.address))
    });
    let evicted = entries.len().saturating_sub(MAX_STORED_UNSUPPORTED_CANDIDATES);
    entries.truncate(MAX_STORED_UNSUPPORTED_CANDIDATES);
    (entries, evicted)
}

fn impostor_address_key(chain: Chain, address: &str) -> String {
    if chain == Chain::Solana { address.to_owned() } else { address.to_ascii_lowercase() }
}

fn merge_impostor_entries(
    mut entries: Vec<ImpostorEntry>,
    detected: Vec<ImpostorEntry>,
) -> (Vec<ImpostorEntry>, usize) {
    for mut detected in detected {
        if let Some(existing) = entries.iter_mut().find(|existing| {
            existing.chain == detected.chain
                && if existing.chain == "solana" {
                    existing.address == detected.address
                } else {
                    existing.address.eq_ignore_ascii_case(&detected.address)
                }
        }) {
            detected.first_seen_at.clone_from(&existing.first_seen_at);
            *existing = detected;
        } else {
            entries.push(detected);
        }
    }
    entries.sort_by(|left, right| {
        right
            .last_seen_at
            .cmp(&left.last_seen_at)
            .then_with(|| left.chain.cmp(&right.chain))
            .then_with(|| left.address.cmp(&right.address))
    });
    let evicted = entries.len().saturating_sub(MAX_STORED_IMPOSTORS);
    entries.truncate(MAX_STORED_IMPOSTORS);
    (entries, evicted)
}

fn rank_impostor_candidates(
    mut candidates: Vec<(Chain, usize, DexPair)>,
) -> Vec<(Chain, usize, DexPair)> {
    candidates.sort_by(|left, right| {
        left.1
            .cmp(&right.1)
            .then_with(|| {
                let left_volume = left.2.volume.as_ref().and_then(|volume| volume.h24);
                let right_volume = right.2.volume.as_ref().and_then(|volume| volume.h24);
                match (left_volume, right_volume) {
                    (Some(left), Some(right)) => right.total_cmp(&left),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => std::cmp::Ordering::Equal,
                }
            })
            .then_with(|| left.2.pair_address.cmp(&right.2.pair_address))
    });
    candidates.truncate(MAX_IMPOSTOR_CANDIDATES_PER_REFRESH);
    candidates
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InvalidWatchObservation {
    InvalidChain,
    InvalidAddress,
    InvalidCatalogHash,
}

fn compact_watch_reads<'a>(
    reads: impl IntoIterator<Item = &'a crate::domain::attestation::Read>,
    chain: Chain,
    address: &str,
) -> (Vec<crate::domain::attestation::Read>, bool) {
    let mut compact = Vec::with_capacity(MAX_IMPOSTOR_READS);
    let mut evidence_truncated = false;
    for source in reads {
        if !is_identity_read(source, chain, address) {
            evidence_truncated = true;
            continue;
        }
        if compact.iter().any(|read: &crate::domain::attestation::Read| {
            read.method == source.method
                && read.params == source.params
                && read.result_hash == source.result_hash
                && read.block == source.block
                && read.slot == source.slot
        }) {
            evidence_truncated = true;
            continue;
        }
        if compact.len() == MAX_IMPOSTOR_READS {
            evidence_truncated = true;
            continue;
        }
        let mut read = source.clone();
        if read.method.len() > MAX_IMPOSTOR_READ_METHOD_BYTES {
            truncate_text_bytes(&mut read.method, MAX_IMPOSTOR_READ_METHOD_BYTES);
            evidence_truncated = true;
        }
        if read.result_hash.len() > 64 {
            truncate_text_bytes(&mut read.result_hash, 64);
            evidence_truncated = true;
        }
        if truncate_json_value(&mut read.params, MAX_IMPOSTOR_READ_PARAMS_BYTES) {
            evidence_truncated = true;
        }
        if let Some(raw_result) = &mut read.raw_result
            && truncate_json_value(raw_result, MAX_IMPOSTOR_READ_RESULT_BYTES)
        {
            evidence_truncated = true;
        }
        compact.push(read);
    }
    (compact, evidence_truncated)
}

fn is_identity_read(read: &crate::domain::attestation::Read, chain: Chain, address: &str) -> bool {
    let Some(params) = read.params.as_array() else { return false };
    match (chain, read.method.as_str()) {
        (Chain::Solana, "getAccountInfo") => params
            .first()
            .and_then(serde_json::Value::as_str)
            .is_some_and(|target| target == address),
        (Chain::Solana, "getProgramAccounts") => {
            params.first().and_then(serde_json::Value::as_str)
                == Some(crate::adapters::solana::METAPLEX_METADATA_PROGRAM)
                && params
                    .get(1)
                    .and_then(|config| config.get("filters"))
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|filters| {
                        filters.iter().any(|filter| {
                            filter.pointer("/memcmp/offset").and_then(serde_json::Value::as_u64)
                                == Some(33)
                                && filter
                                    .pointer("/memcmp/bytes")
                                    .and_then(serde_json::Value::as_str)
                                    == Some(address)
                        })
                    })
        }
        (chain, "eth_call") if chain != Chain::Solana => {
            params
                .first()
                .and_then(serde_json::Value::as_str)
                .is_some_and(|target| target.eq_ignore_ascii_case(address))
                && matches!(
                    params.get(1).and_then(serde_json::Value::as_str),
                    Some("symbol()" | "name()")
                )
        }
        _ => false,
    }
}

fn truncate_text_bytes(value: &mut String, max_bytes: usize) {
    let mut end = value.len().min(max_bytes);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
}

fn truncate_watch_text(value: &mut String, max_bytes: usize, evidence_truncated: &mut bool) {
    if value.len() > max_bytes {
        truncate_text_bytes(value, max_bytes);
        *evidence_truncated = true;
    }
}

/// Clips overlong observation text to its storage cap. Text length never rejects a
/// valid observation; clipping is recorded in `evidence_truncated`.
fn clip_impostor_entry_text(entry: &mut ImpostorEntry) {
    let labels = [&mut entry.ticker, &mut entry.publisher, &mut entry.symbol, &mut entry.name]
        .into_iter()
        .chain(entry.on_chain_symbol.as_mut())
        .chain(entry.on_chain_name.as_mut());
    for label in labels {
        truncate_watch_text(label, MAX_IMPOSTOR_LABEL_BYTES, &mut entry.evidence_truncated);
    }
    for timestamp in [&mut entry.first_seen_at, &mut entry.last_seen_at] {
        truncate_watch_text(timestamp, MAX_IMPOSTOR_TIMESTAMP_BYTES, &mut entry.evidence_truncated);
    }
    truncate_watch_text(
        &mut entry.reason,
        MAX_IMPOSTOR_REASON_BYTES,
        &mut entry.evidence_truncated,
    );
}

fn clip_unsupported_candidate_text(candidate: &mut UnsupportedImpostorCandidate) {
    for label in [
        &mut candidate.ticker,
        &mut candidate.publisher,
        &mut candidate.symbol,
        &mut candidate.name,
    ] {
        truncate_watch_text(label, MAX_IMPOSTOR_LABEL_BYTES, &mut candidate.evidence_truncated);
    }
    for timestamp in [&mut candidate.first_seen_at, &mut candidate.last_seen_at] {
        truncate_watch_text(
            timestamp,
            MAX_IMPOSTOR_TIMESTAMP_BYTES,
            &mut candidate.evidence_truncated,
        );
    }
}

/// An unsupported-chain candidate is rejected only when its DexScreener chain id or
/// token address is not a well-formed identifier; its text is clipped instead.
fn validate_unsupported_candidate(
    dex_chain_id: &str,
    address: &str,
) -> Result<(), InvalidWatchObservation> {
    if dex_chain_id.is_empty()
        || dex_chain_id.len() > MAX_IMPOSTOR_CHAIN_ID_BYTES
        || !dex_chain_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(InvalidWatchObservation::InvalidChain);
    }
    if address.is_empty()
        || address.len() > MAX_IMPOSTOR_ADDRESS_BYTES
        || !address.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err(InvalidWatchObservation::InvalidAddress);
    }
    Ok(())
}

fn json_value_len(value: &serde_json::Value) -> usize {
    serde_json::to_vec(value).map_or(usize::MAX, |bytes| bytes.len())
}

fn json_array_len(items: &[serde_json::Value]) -> usize {
    items.iter().fold(2usize.saturating_add(items.len().saturating_sub(1)), |len, item| {
        len.saturating_add(json_value_len(item))
    })
}

fn json_object_len(items: &serde_json::Map<String, serde_json::Value>) -> usize {
    items.iter().fold(2usize.saturating_add(items.len().saturating_sub(1)), |len, (key, item)| {
        let key_len = serde_json::to_vec(key).map_or(usize::MAX, |bytes| bytes.len());
        len.saturating_add(key_len).saturating_add(1).saturating_add(json_value_len(item))
    })
}

fn truncate_json_value(value: &mut serde_json::Value, max_bytes: usize) -> bool {
    if json_value_len(value) <= max_bytes {
        return false;
    }
    match value {
        serde_json::Value::String(text) => {
            truncate_text_bytes(text, max_bytes.saturating_sub(2));
            while serde_json::to_vec(text).is_ok_and(|bytes| bytes.len() > max_bytes) {
                text.pop();
            }
        }
        serde_json::Value::Array(items) => {
            while json_array_len(items) > max_bytes {
                let Some(index) = items.len().checked_sub(1) else { break };
                let reduced = {
                    let item = &mut items[index];
                    let previous_len = json_value_len(item);
                    truncate_json_value(item, previous_len.saturating_div(2).max(2));
                    json_value_len(item) < previous_len
                };
                if !reduced {
                    items.pop();
                }
            }
        }
        serde_json::Value::Object(items) => {
            while json_object_len(items) > max_bytes {
                let Some(key) = items
                    .iter()
                    .max_by_key(|(key, item)| key.len().saturating_add(json_value_len(item)))
                    .map(|(key, _)| key.clone())
                else {
                    break;
                };
                let reduced = {
                    let Some(item) = items.get_mut(&key) else { break };
                    let previous_len = json_value_len(item);
                    truncate_json_value(item, previous_len.saturating_div(2).max(2));
                    json_value_len(item) < previous_len
                };
                if !reduced {
                    items.remove(&key);
                }
            }
        }
        _ => {}
    }
    if json_value_len(value) > max_bytes {
        *value = serde_json::Value::Null;
    }
    true
}

fn catalog_absent_entry(
    chain: Chain,
    scanned_at: &str,
    symbol: String,
    name: String,
    volume_24h_usd: Option<f64>,
    source: MarketSource,
    publisher_catalog_snapshot_hash: Option<String>,
    mut guard: crate::domain::guard::GuardDocument,
) -> Result<Option<ImpostorEntry>, InvalidWatchObservation> {
    if guard.verdict != crate::domain::guard::GuardVerdict::Deny
        || guard.identity.status != crate::domain::guard::IdentityStatus::Mismatch
    {
        return Ok(None);
    }
    let Some(reason_index) = guard
        .reasons
        .iter()
        .position(|reason| reason.code == "claims_unpublished_publisher_product")
    else {
        return Ok(None);
    };
    let (Some(publisher), Some(ticker)) =
        (guard.identity.publisher.take(), guard.identity.ticker.take())
    else {
        return Ok(None);
    };
    if !valid_token_address(chain, &guard.address) {
        return Err(InvalidWatchObservation::InvalidAddress);
    }
    if publisher_catalog_snapshot_hash
        .as_deref()
        .is_some_and(|hash| !valid_publisher_catalog_snapshot_hash(hash))
    {
        return Err(InvalidWatchObservation::InvalidCatalogHash);
    }
    let (reads, reads_truncated) = compact_watch_reads(guard.reads.iter(), chain, &guard.address);
    let evidence_truncated =
        guard.reads_truncated || reads_truncated || publisher_catalog_snapshot_hash.is_none();
    let guard_url = format!("/guard/{}/{}", chain_slug(chain), guard.address);
    let mut entry = ImpostorEntry {
        chain: chain_slug(chain).to_owned(),
        chain_label: chain_label(chain).to_owned(),
        ticker,
        publisher,
        symbol,
        name,
        address: std::mem::take(&mut guard.address),
        first_seen_at: scanned_at.to_owned(),
        last_seen_at: scanned_at.to_owned(),
        volume_24h_usd,
        source,
        guard_url,
        reason: guard.reasons.swap_remove(reason_index).detail,
        reads,
        on_chain_symbol: guard.identity.observed_symbol.take(),
        on_chain_name: guard.identity.observed_name.take(),
        publisher_catalog_snapshot_hash,
        evidence_truncated,
        guard_document: None,
    };
    clip_impostor_entry_text(&mut entry);
    Ok(Some(entry))
}

/// Records that the impostor search source returned no usable data. The last successful
/// results, scan time and search rotation stay unchanged; the outage start is kept until a
/// scan succeeds.
fn impostor_source_unavailable(
    mut previous: ImpostorSnapshot,
    observed_at: &str,
) -> ImpostorSnapshot {
    previous.source_unavailable_since.get_or_insert_with(|| observed_at.to_owned());
    previous
}

fn compact_network_id_eq(left: &str, right: &str) -> bool {
    left.bytes()
        .filter(u8::is_ascii_alphanumeric)
        .map(|byte| byte.to_ascii_lowercase())
        .eq(right.bytes().filter(u8::is_ascii_alphanumeric).map(|byte| byte.to_ascii_lowercase()))
}

fn watch_network_ids_equal(left: &str, right: &str) -> bool {
    let left_is_ton =
        compact_network_id_eq(left, "ton") || compact_network_id_eq(left, "theopennetwork");
    let right_is_ton =
        compact_network_id_eq(right, "ton") || compact_network_id_eq(right, "theopennetwork");
    if left_is_ton || right_is_ton {
        left_is_ton && right_is_ton
    } else {
        compact_network_id_eq(left, right)
    }
}

fn normalized_watch_network_id(network: &str) -> String {
    if compact_network_id_eq(network, "ton") || compact_network_id_eq(network, "theopennetwork") {
        "ton".to_owned()
    } else {
        network
            .bytes()
            .filter(u8::is_ascii_alphanumeric)
            .map(|byte| byte.to_ascii_lowercase())
            .map(char::from)
            .collect()
    }
}

fn watch_addresses_equal(left: &str, right: &str) -> bool {
    if left.starts_with("0x") && right.starts_with("0x") {
        left.eq_ignore_ascii_case(right)
    } else {
        left == right
    }
}

fn watch_address_key(address: &str) -> String {
    if address.starts_with("0x") { address.to_ascii_lowercase() } else { address.to_owned() }
}

fn official_unsupported_deployment_matches(
    registry: &Registry,
    network: &str,
    address: &str,
) -> bool {
    registry.iter().filter(|entry| registry::matchable(entry)).any(|entry| {
        entry.official_deployments.iter().any(|deployment| {
            watch_network_ids_equal(network, &deployment.network)
                && [
                    Some(deployment.address.as_str()),
                    deployment.wrapper_address.as_deref(),
                    deployment.wrapper_address_v2.as_deref(),
                ]
                .into_iter()
                .flatten()
                .any(|official_address| watch_addresses_equal(official_address, address))
        })
    })
}

fn official_unsupported_deployment_key(network: &str, address: &str) -> (String, String) {
    (normalized_watch_network_id(network), watch_address_key(address))
}

async fn refresh_impostor_watch(
    state: &AppState,
    client: &MarketDataClient,
    registry: &Registry,
    publisher_catalog_snapshot_hash: Option<String>,
    mut previous: ImpostorSnapshot,
) -> ImpostorSnapshot {
    let tickers = {
        let board = state.leaderboard.read().await;
        prioritize_search_tickers(registry, &board.entries)
    };
    let (searches, next_ticker_offset) =
        rotating_impostor_search_terms(&tickers, previous.next_ticker_offset);
    if searches.is_empty() {
        return previous;
    }
    let query_count = searches.len();
    let mut source_pairs = 0usize;
    let mut failed_queries = 0usize;
    let scanned_at = now_rfc3339();
    let mut candidates = Vec::<(Chain, usize, DexPair)>::new();
    let mut candidate_keys = HashSet::new();
    let mut unsupported_keys = HashSet::new();
    let mut official_unsupported_keys = HashSet::new();
    let mut official_on_unsupported_chain = 0usize;
    let mut unsupported_detected = Vec::new();
    let mut rejected_entries = 0usize;
    let mut rejected_unsupported_candidates = 0usize;
    let dex_available = client.dex_available().await;
    let hot_ticker_count = tickers.len().min(IMPOSTOR_HOT_TICKERS_PER_REFRESH);
    let mut fallback_tickers = HashSet::new();
    let mut watch_budget = GeckoWatchBudget::default();
    let mut searches_made = 0usize;
    for offset in 0..query_count {
        if offset > 0 {
            sleep(REQUEST_INTERVAL).await;
        }
        let (ticker, search) = &searches[offset];
        let ticker_rank =
            tickers.iter().position(|priority| priority == ticker).unwrap_or(usize::MAX);
        let mut pairs = Vec::new();
        if dex_available {
            searches_made += 1;
            match client.dex.search(search).await {
                Ok(found) => pairs = found,
                Err(error) => {
                    failed_queries += 1;
                    warn!(ticker, query = search, error = %error, "DexScreener impostor search failed; checking GeckoTerminal");
                    if ticker_rank < hot_ticker_count && fallback_tickers.insert(ticker.clone()) {
                        let product_names = registry_product_names(registry, ticker);
                        let mut found = search_watch_products(
                            client,
                            ticker,
                            &product_names,
                            &mut watch_budget,
                            &mut searches_made,
                            &mut failed_queries,
                        )
                        .await;
                        pairs.append(&mut found);
                    }
                }
            }
        } else if ticker_rank < hot_ticker_count && fallback_tickers.insert(ticker.clone()) {
            let product_names = registry_product_names(registry, ticker);
            let mut found = search_watch_products(
                client,
                ticker,
                &product_names,
                &mut watch_budget,
                &mut searches_made,
                &mut failed_queries,
            )
            .await;
            pairs.append(&mut found);
        }
        source_pairs = source_pairs.saturating_add(pairs.len());
        for pair in pairs {
            let Some(symbol) = pair.base_token.symbol.as_deref() else { continue };
            if !symbol_matches_search_ticker(symbol, ticker) {
                continue;
            }
            let Some(address) = pair.base_token.address.as_deref() else { continue };
            let Some(chain) = chain_from_dex_id(&pair.chain_id) else {
                if let Err(rejection) = validate_unsupported_candidate(&pair.chain_id, address) {
                    tracing::debug!(
                        rejection = ?rejection,
                        "rejected invalid unsupported-chain watch candidate"
                    );
                    rejected_unsupported_candidates =
                        rejected_unsupported_candidates.saturating_add(1);
                    continue;
                }
                if official_unsupported_deployment_matches(registry, &pair.chain_id, address) {
                    if official_unsupported_keys
                        .insert(official_unsupported_deployment_key(&pair.chain_id, address))
                    {
                        official_on_unsupported_chain =
                            official_on_unsupported_chain.saturating_add(1);
                    }
                    continue;
                }
                let key = (pair.chain_id.to_ascii_lowercase(), address.to_ascii_lowercase());
                if unsupported_keys.insert(key) {
                    let publisher = registry
                        .iter()
                        .find(|entry| {
                            registry::matchable(entry) && entry.ticker.eq_ignore_ascii_case(ticker)
                        })
                        .map(|entry| entry.issuer.as_str())
                        .unwrap_or_default();
                    let mut candidate = UnsupportedImpostorCandidate {
                        dex_chain_id: pair.chain_id.clone(),
                        ticker: ticker.clone(),
                        publisher: publisher.to_owned(),
                        symbol: symbol.to_owned(),
                        name: pair.base_token.name.clone().unwrap_or_default(),
                        address: address.to_owned(),
                        first_seen_at: scanned_at.clone(),
                        last_seen_at: scanned_at.clone(),
                        volume_24h_usd: pair.volume.as_ref().and_then(|volume| volume.h24),
                        source: pair.source,
                        evidence_truncated: false,
                    };
                    clip_unsupported_candidate_text(&mut candidate);
                    unsupported_detected.push(candidate);
                }
                continue;
            };
            if !valid_token_address(chain, address) {
                tracing::debug!(
                    %chain,
                    rejection = ?InvalidWatchObservation::InvalidAddress,
                    "rejected invalid publisher-watch candidate"
                );
                continue;
            }
            if registry::lookup(registry, chain, address).is_some() {
                continue;
            }
            let key = (chain, impostor_address_key(chain, address));
            if candidate_keys.insert(key) {
                candidates.push((chain, ticker_rank, pair));
            }
        }
    }
    if source_pairs == 0 {
        warn!(
            searches = searches_made,
            gecko_requests = watch_budget.requests,
            failed_queries,
            "market-data impostor searches returned no pairs; keeping last successful watch results"
        );
        return impostor_source_unavailable(previous, &scanned_at);
    }
    let candidates = rank_impostor_candidates(candidates);
    let current_candidate_keys = candidates
        .iter()
        .filter_map(|(chain, _, pair)| {
            pair.base_token
                .address
                .as_deref()
                .map(|address| (*chain, impostor_address_key(*chain, address)))
        })
        .collect::<HashSet<_>>();

    let mut previous_entries = std::mem::take(&mut previous.entries);
    previous_entries.retain(|entry| {
        Chain::from_network_name(&entry.chain)
            .is_none_or(|chain| registry::lookup(registry, chain, &entry.address).is_none())
    });
    let eligible_previous = previous_entries
        .iter()
        .filter(|entry| {
            let Some(chain) = Chain::from_network_name(&entry.chain) else { return false };
            !current_candidate_keys.contains(&(chain, impostor_address_key(chain, &entry.address)))
        })
        .cloned()
        .collect::<Vec<_>>();
    let recheck_count = eligible_previous.len().min(MAX_IMPOSTOR_RECHECKS_PER_REFRESH);
    let recheck_start = if eligible_previous.is_empty() {
        0
    } else {
        previous.next_entry_offset % eligible_previous.len()
    };
    let next_entry_offset = if eligible_previous.is_empty() {
        0
    } else {
        (recheck_start + recheck_count) % eligible_previous.len()
    };
    let mut detected = Vec::new();
    let mut confirmed_official = HashSet::new();
    let mut guard_checks = 0;
    for offset in 0..recheck_count {
        if guard_checks > 0 {
            sleep(REQUEST_INTERVAL).await;
        }
        guard_checks += 1;
        let previous_entry = &eligible_previous[(recheck_start + offset) % eligible_previous.len()];
        let Some(chain) = Chain::from_network_name(&previous_entry.chain) else { continue };
        let guard = match crate::app::guard::create(
            &state.app,
            &previous_entry.address,
            chain,
            None,
        )
        .await
        {
            Ok(guard) => guard,
            Err(error) => {
                warn!(%chain, address = %previous_entry.address, error = %error, "Guard could not recheck impostor candidate");
                continue;
            }
        };
        if guard.identity.status == crate::domain::guard::IdentityStatus::Match {
            confirmed_official
                .insert((chain, impostor_address_key(chain, &previous_entry.address)));
            continue;
        }
        match catalog_absent_entry(
            chain,
            &scanned_at,
            previous_entry.symbol.clone(),
            previous_entry.name.clone(),
            previous_entry.volume_24h_usd,
            previous_entry.source,
            publisher_catalog_snapshot_hash.clone(),
            guard,
        ) {
            Ok(Some(updated)) => detected.push(updated),
            Err(rejection) => {
                tracing::debug!(%chain, rejection = ?rejection, "rejected invalid publisher-watch entry");
                rejected_entries = rejected_entries.saturating_add(1);
            }
            Ok(None) => {}
        }
    }
    previous_entries.retain(|entry| {
        let Some(chain) = Chain::from_network_name(&entry.chain) else { return true };
        !confirmed_official.contains(&(chain, impostor_address_key(chain, &entry.address)))
    });

    let new_candidate_limit = MAX_IMPOSTOR_CANDIDATES_PER_REFRESH.saturating_sub(guard_checks);
    let judged_count = recheck_count + candidates.len().min(new_candidate_limit);
    for (chain, _ticker_rank, pair) in candidates.into_iter().take(new_candidate_limit) {
        if guard_checks > 0 {
            sleep(REQUEST_INTERVAL).await;
        }
        guard_checks += 1;
        let Some(address) = pair.base_token.address.as_deref() else { continue };
        let guard = match crate::app::guard::create(&state.app, address, chain, None).await {
            Ok(guard) => guard,
            Err(error) => {
                warn!(%chain, address, error = %error, "Guard could not read impostor candidate");
                continue;
            }
        };
        match catalog_absent_entry(
            chain,
            &scanned_at,
            pair.base_token.symbol.unwrap_or_default(),
            pair.base_token.name.unwrap_or_default(),
            pair.volume.as_ref().and_then(|volume| volume.h24),
            pair.source,
            publisher_catalog_snapshot_hash.clone(),
            guard,
        ) {
            Ok(Some(entry)) => detected.push(entry),
            Err(rejection) => {
                tracing::debug!(%chain, rejection = ?rejection, "rejected invalid publisher-watch entry");
                rejected_entries = rejected_entries.saturating_add(1);
            }
            Ok(None) => {}
        }
    }
    let (entries, evicted_entries) = merge_impostor_entries(previous_entries, detected);
    let (unsupported_candidates, evicted_unsupported_candidates) = merge_unsupported_candidates(
        std::mem::take(&mut previous.unsupported_candidates),
        unsupported_detected,
    );
    let unsupported_seen = unsupported_candidates.len();
    info!(
        searches = searches_made,
        gecko_requests = watch_budget.requests,
        judged = judged_count,
        impostors = entries.len(),
        unsupported_seen,
        evicted_entries,
        official_on_unsupported_chain,
        evicted_unsupported_candidates,
        rejected_entries,
        rejected_unsupported_candidates,
        failed_queries,
        "market-data impostor watch refresh complete"
    );
    previous.scanned_at = scanned_at;
    previous.source_unavailable_since = None;
    previous.next_ticker_offset = next_ticker_offset;
    previous.next_entry_offset = next_entry_offset;
    previous.unsupported_seen = unsupported_seen;
    previous.official_on_unsupported_chain = official_on_unsupported_chain;
    previous.evicted_entries = previous.evicted_entries.saturating_add(evicted_entries);
    previous.evicted_unsupported_candidates =
        previous.evicted_unsupported_candidates.saturating_add(evicted_unsupported_candidates);
    previous.rejected_oversize_entries =
        previous.rejected_oversize_entries.saturating_add(rejected_entries);
    previous.rejected_oversize_unsupported_candidates = previous
        .rejected_oversize_unsupported_candidates
        .saturating_add(rejected_unsupported_candidates);
    previous.entries = entries;
    previous.unsupported_candidates = unsupported_candidates;
    previous
}

async fn publish_leaderboard(
    state: &AppState,
    data_dir: &Path,
    registry: &Registry,
    total: usize,
    entries: Vec<LeaderboardEntry>,
    updated_at: &str,
    persist: bool,
) {
    let (previous_entries, impostors) = {
        let board = state.leaderboard.read().await;
        (board.entries.clone(), board.impostors.clone())
    };
    let previous_count = previous_entries.len();
    if entries.is_empty() {
        if previous_count > 0 {
            defer_leaderboard_refresh(
                state,
                data_dir,
                "discovery returned an empty board",
                DISCOVERY_REFRESH_SECS,
            )
            .await;
            return;
        }
        let mut board = state.leaderboard.write().await;
        board.updated_at = updated_at.to_owned();
        board.next_refresh_at = timestamp_after(DISCOVERY_REFRESH_SECS);
        board.total = 0;
        board.entries.clear();
        board.restored = false;
        board.refreshing = false;
        board.empty_successful = true;
        return;
    }
    if !safe_leaderboard_replacement(&previous_entries, &entries) {
        defer_leaderboard_refresh(
            state,
            data_dir,
            "discovery returned an incomplete pool set",
            DISCOVERY_REFRESH_SECS,
        )
        .await;
        return;
    }
    let mut registry_snapshot = state.registry_status.read().await.clone();
    registry_snapshot.entries = registry::active_count(registry);
    registry_snapshot.issuers = registry::active_issuers(registry);
    let leaderboard = Leaderboard {
        updated_at: updated_at.to_owned(),
        next_refresh_at: timestamp_after(DISCOVERY_REFRESH_SECS),
        source: LEADERBOARD_SOURCE.to_owned(),
        registry: registry_snapshot,
        total,
        entries,
        restored: false,
        refreshing: false,
        empty_successful: false,
        impostors,
    };
    if persist {
        save_leaderboard(data_dir, &leaderboard);
        state.board_store.persist_leaderboard(&leaderboard).await;
    }
    *state.leaderboard.write().await = leaderboard;
}

async fn defer_leaderboard_refresh(
    state: &AppState,
    data_dir: &Path,
    reason: &str,
    delay_secs: u64,
) {
    let board = {
        let mut board = state.leaderboard.write().await;
        board.next_refresh_at = timestamp_after(delay_secs);
        board.refreshing = false;
        board.clone()
    };
    save_leaderboard(data_dir, &board);
    warn!(
        reason,
        entries = board.entries.len(),
        "keeping previous leaderboard after an unsuccessful refresh"
    );
}

async fn refresh_leaderboard(
    state: &AppState,
    data_dir: &Path,
    registry: &Registry,
    candidates: Vec<LeaderboardCandidate>,
) -> HashMap<(Chain, String), CheckResult> {
    let candidates = rank_leaderboard_candidates(candidates);
    let candidate_count = candidates.len();
    let updated_at = now_rfc3339();
    let mut entries = candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| provisional_leaderboard_entry(index + 1, candidate))
        .collect::<Vec<_>>();
    let mut shared_checks = HashMap::with_capacity(candidate_count);
    let transient_retry_keys = state
        .leaderboard
        .read()
        .await
        .entries
        .iter()
        .filter(|entry| {
            entry.read_status == "not_read_yet" && entry.read_reason.as_deref() == Some("transient")
        })
        .filter_map(|entry| {
            chain_from_dex_id(&entry.chain).map(|chain| (chain, entry.pool.to_ascii_lowercase()))
        })
        .collect::<HashSet<_>>();
    for (index, candidate) in candidates.into_iter().enumerate() {
        if index > 0 {
            sleep(REQUEST_INTERVAL).await;
        }
        let force_refresh =
            transient_retry_keys.contains(&(candidate.chain, candidate.pool.to_ascii_lowercase()));
        let result = cached_leaderboard_check(state, &candidate, force_refresh).await;
        shared_checks
            .insert((candidate.chain, candidate.pool.to_ascii_lowercase()), result.clone());
        entries[index] = leaderboard_entry(index + 1, candidate, result);
    }
    let entry_count = entries.len();
    publish_leaderboard(state, data_dir, registry, candidate_count, entries, &updated_at, true)
        .await;
    info!(entries = entry_count, candidates = candidate_count, "leaderboard refresh complete");
    shared_checks
}

fn startup_discovery_waits_for_registry(first_refresh: bool, registry: &Registry) -> bool {
    first_refresh && registry::active_count(registry) == 0
}

pub async fn refresh_discovery(
    state: &AppState,
    data_dir: &Path,
    first_refresh: bool,
    warm_notify: &tokio::sync::Notify,
) -> bool {
    {
        let mut board = state.leaderboard.write().await;
        board.next_refresh_at = timestamp_after(DISCOVERY_REFRESH_SECS);
        board.refreshing = true;
    }
    {
        let mut status = state.featured_status.write().await;
        status.next_refresh_at = timestamp_after(DISCOVERY_REFRESH_SECS);
        status.refreshing = true;
    }
    let retry_delay = if first_refresh { 30 } else { DISCOVERY_REFRESH_SECS };
    let registry = state.registry.read().await.clone();
    let publisher_catalog_snapshot_hash = state
        .app
        .registry_hash
        .read()
        .ok()
        .map(|hash| hash.clone())
        .filter(|hash| hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()));
    if startup_discovery_waits_for_registry(first_refresh, &registry) {
        defer_leaderboard_refresh(
            state,
            data_dir,
            "waiting for a refreshed registry before first discovery",
            retry_delay,
        )
        .await;
        defer_featured_refresh(
            state,
            "waiting for a refreshed registry before first discovery",
            retry_delay,
        )
        .await;
        return true;
    }
    let batch = match discover_registry(state, &registry).await {
        Ok(batch) => batch,
        Err(error) if is_dex_blocked(&error) => {
            defer_leaderboard_refresh(
                state,
                data_dir,
                "DexScreener request budget unavailable",
                retry_delay,
            )
            .await;
            defer_featured_refresh(state, "DexScreener request budget unavailable", retry_delay)
                .await;
            return first_refresh;
        }
        Err(error) => {
            warn!(error = %error, "shared discovery failed; keeping previous boards");
            defer_leaderboard_refresh(
                state,
                data_dir,
                "DexScreener discovery failure",
                retry_delay,
            )
            .await;
            defer_featured_refresh(state, "DexScreener discovery failure", retry_delay).await;
            return first_refresh;
        }
    };
    let shared_checks = refresh_leaderboard(state, data_dir, &registry, batch.leaderboard).await;
    if let Err(error) = crate::adapters::web::refresh_stats_snapshot(state).await {
        warn!(%error, "could not refresh prepared statistics after leaderboard refresh");
    }
    refresh_featured(state, data_dir, batch.featured, &shared_checks).await;
    let previous_impostors = state.leaderboard.read().await.impostors.clone();
    let impostors = refresh_impostor_watch(
        state,
        &MarketDataClient::new(state.http.clone()),
        &registry,
        publisher_catalog_snapshot_hash,
        previous_impostors,
    )
    .await;
    let persisted_board = {
        let mut board = state.leaderboard.write().await;
        board.impostors = impostors;
        board.clone()
    };
    save_leaderboard(data_dir, &persisted_board);
    state.board_store.persist_leaderboard(&persisted_board).await;
    crate::app::warm::notify_powers_warm_if_targets(&state.app, warm_notify).await;
    if let Err(error) = crate::adapters::web::refresh_stats_snapshot(state).await {
        warn!(%error, "could not refresh prepared statistics snapshot");
    }
    false
}

fn price_point_from_pair(pair: DexPair) -> Option<PricePoint> {
    let chain = chain_from_dex_id(&pair.chain_id)?;
    Some(PricePoint {
        chain: chain_slug(chain).to_owned(),
        pool: pair.pair_address,
        price_usd: pair.price_usd,
        change_24h_pct: pair.price_change.and_then(|change| change.h24),
        volume_24h_usd: pair.volume.and_then(|volume| volume.h24),
        liquidity_usd: pair.liquidity.and_then(|liquidity| liquidity.usd),
        source: pair.source,
    })
}

/// Merge market-data pairs into the last ticker snapshot. Keeping points not
/// present in a response makes a partial upstream failure non-destructive.
pub fn merge_price_points(
    previous: &[PricePoint],
    pairs: impl IntoIterator<Item = DexPair>,
) -> Vec<PricePoint> {
    let mut merged = previous.to_vec();
    let mut indexes = merged
        .iter()
        .enumerate()
        .map(|(index, point)| {
            ((point.chain.to_ascii_lowercase(), point.pool.to_ascii_lowercase()), index)
        })
        .collect::<HashMap<_, _>>();
    for pair in pairs {
        let Some(point) = price_point_from_pair(pair) else { continue };
        let key = (point.chain.to_ascii_lowercase(), point.pool.to_ascii_lowercase());
        if let Some(index) = indexes.get(&key).copied() {
            merged[index] = point;
        } else {
            indexes.insert(key, merged.len());
            merged.push(point);
        }
    }
    merged
}

pub async fn refresh_prices(state: &AppState) -> bool {
    let entries = state.leaderboard.read().await.entries.clone();
    let mut pools_by_chain = HashMap::<Chain, Vec<String>>::new();
    for entry in entries {
        let Some(chain) = chain_from_dex_id(&entry.chain) else { continue };
        let pools = pools_by_chain.entry(chain).or_default();
        if !pools.iter().any(|pool| pool.eq_ignore_ascii_case(&entry.pool)) {
            pools.push(entry.pool);
        }
    }
    if pools_by_chain.is_empty() {
        return true;
    }

    let client = MarketDataClient::new(state.http.clone());
    let mut pairs = Vec::new();
    for (chain, pools) in pools_by_chain {
        match client.pairs(chain, &pools).await {
            Ok(mut chain_pairs) => pairs.append(&mut chain_pairs),
            Err(error) if is_dex_blocked(&error) => return false,
            Err(error) => warn!(%chain, error = %error, "market-data price ticker failed"),
        }
    }

    let updated_at = now_rfc3339();
    let prices = {
        let mut snapshot = state.prices.write().await;
        snapshot.prices = merge_price_points(&snapshot.prices, pairs);
        snapshot.updated_at = updated_at;
        snapshot.prices.clone()
    };
    let by_pool = prices
        .iter()
        .map(|point| ((point.chain.to_ascii_lowercase(), point.pool.to_ascii_lowercase()), point))
        .collect::<HashMap<_, _>>();
    let mut leaderboard = state.leaderboard.write().await;
    for entry in &mut leaderboard.entries {
        let key = (entry.chain.to_ascii_lowercase(), entry.pool.to_ascii_lowercase());
        let Some(point) = by_pool.get(&key) else { continue };
        entry.price_usd = point.price_usd;
        entry.change_24h_pct = point.change_24h_pct;
        entry.volume_24h_usd = point.volume_24h_usd;
        entry.liquidity_usd = point.liquidity_usd;
        entry.source = point.source;
    }
    true
}

/// Format raw token units without a floating point conversion. This keeps
/// large EVM and Solana balances exact while adding grouping separators.
pub fn format_balance(raw: &str, decimals: Option<u8>, symbol: Option<&str>) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let negative = raw.starts_with('-');
    let digits = raw.strip_prefix('-').unwrap_or(raw);
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let decimals = usize::from(decimals.unwrap_or(0));
    let (integer, fraction_owned) = if decimals == 0 {
        (digits, String::new())
    } else if digits.len() <= decimals {
        ("0", format!("{}{}", "0".repeat(decimals - digits.len()), digits))
    } else {
        let (integer, fraction) = digits.split_at(digits.len() - decimals);
        (integer, fraction.to_owned())
    };
    let fraction = fraction_owned.as_str();
    let integer = integer.trim_start_matches('0');
    let integer = if integer.is_empty() { "0" } else { integer };
    let mut grouped = String::with_capacity(integer.len() + integer.len() / 3);
    for (index, byte) in integer.bytes().enumerate() {
        if index > 0 && (integer.len() - index) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(char::from(byte));
    }
    let fraction = fraction.trim_end_matches('0').to_owned();
    let mut output = String::new();
    if negative && grouped != "0" {
        output.push('-');
    }
    output.push_str(&grouped);
    if !fraction.is_empty() {
        output.push('.');
        output.push_str(&fraction);
    }
    if let Some(symbol) = symbol.filter(|symbol| !symbol.is_empty()) {
        output.push(' ');
        output.push_str(symbol);
    }
    Some(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::solana::METAPLEX_METADATA_PROGRAM;
    use crate::domain::{chain::Chain, registry::Entry};

    #[test]
    fn robinhood_explorer_url_uses_blockscout() {
        assert_eq!(
            explorer_url(Chain::RobinhoodChain, "0xpool"),
            "https://robinhoodchain.blockscout.com/address/0xpool"
        );
    }

    #[test]
    fn public_chain_slug_matches_json_chain_encoding() {
        for chain in
            [Chain::Solana, Chain::RobinhoodChain, Chain::Base, Chain::Ethereum, Chain::Bnb]
        {
            assert_eq!(
                serde_json::to_value(chain).expect("chain JSON"),
                serde_json::json!(chain_slug(chain))
            );
        }
    }

    #[test]
    fn normalizes_cached_legacy_chain_names_and_urls() {
        let mut board = sample_leaderboard("2026-10-06T00:00:00Z", 1);
        let pool = "0x0000000000000000000000000000000000000001";
        board.entries[0].chain = "RobinhoodChain".to_owned();
        board.entries[0].pool = pool.to_owned();
        board.entries[0].detail_url = format!("/validated/robinhoodchain/{pool}");
        board.entries[0].trade_url = format!("https://dexscreener.com/robinhoodchain/{pool}");
        board.impostors.entries.push(ImpostorEntry {
            chain: "robinhoodchain".to_owned(),
            chain_label: "RobinhoodChain".to_owned(),
            ticker: "NVDA".to_owned(),
            publisher: "Backed xStocks".to_owned(),
            symbol: "NVDAx".to_owned(),
            name: "NVIDIA xStock".to_owned(),
            address: pool.to_owned(),
            first_seen_at: "2026-10-06T00:00:00Z".to_owned(),
            last_seen_at: "2026-10-06T00:00:00Z".to_owned(),
            volume_24h_usd: None,
            source: MarketSource::Dexscreener,
            guard_url: format!("/guard/robinhoodchain/{pool}"),
            reason: "catalog observation".to_owned(),
            reads: Vec::new(),
            on_chain_symbol: None,
            on_chain_name: None,
            publisher_catalog_snapshot_hash: None,
            evidence_truncated: false,
            guard_document: None,
        });

        let normalized = normalize_leaderboard(board);
        let entry = &normalized.entries[0];
        assert_eq!(entry.chain, "robinhood");
        assert_eq!(entry.chain_label, "Robinhood Chain");
        assert_eq!(entry.detail_url, format!("/validated/robinhood/{pool}"));
        assert_eq!(entry.trade_url, format!("https://dexscreener.com/robinhood/{pool}"));
        assert_eq!(
            entry.explorer_url,
            format!("https://robinhoodchain.blockscout.com/address/{pool}")
        );
        let impostor = &normalized.impostors.entries[0];
        assert_eq!(impostor.chain, "robinhood");
        assert_eq!(impostor.guard_url, format!("/guard/robinhood/{pool}"));
    }

    #[test]
    fn legacy_market_data_rows_default_to_dexscreener() {
        let mut board = sample_leaderboard("2026-10-07T00:00:00Z", 1);
        board.impostors.entries.push(ImpostorEntry {
            chain: "base".to_owned(),
            chain_label: "Base".to_owned(),
            ticker: "NVDA".to_owned(),
            publisher: "Backed xStocks".to_owned(),
            symbol: "NVDAx".to_owned(),
            name: "NVIDIA xStock".to_owned(),
            address: "0x0000000000000000000000000000000000000001".to_owned(),
            first_seen_at: "2026-10-07T00:00:00Z".to_owned(),
            last_seen_at: "2026-10-07T00:00:00Z".to_owned(),
            volume_24h_usd: None,
            source: MarketSource::Geckoterminal,
            guard_url: "/guard/base/0x0000000000000000000000000000000000000001".to_owned(),
            reason: "catalog observation".to_owned(),
            reads: Vec::new(),
            on_chain_symbol: None,
            on_chain_name: None,
            publisher_catalog_snapshot_hash: None,
            evidence_truncated: false,
            guard_document: None,
        });
        let mut legacy = serde_json::to_value(board).expect("legacy board value");
        legacy["entries"][0].as_object_mut().expect("legacy leaderboard row").remove("source");
        legacy["impostors"]["entries"][0]
            .as_object_mut()
            .expect("legacy watch row")
            .remove("source");

        let restored: Leaderboard =
            serde_json::from_value(legacy).expect("legacy source-less leaderboard");
        assert_eq!(restored.entries[0].source, MarketSource::Dexscreener);
        assert_eq!(restored.impostors.entries[0].source, MarketSource::Dexscreener);
    }

    #[derive(Debug, Deserialize)]
    struct DexSearchResponse {
        #[serde(default)]
        pairs: Vec<DexPair>,
    }

    fn entry(chain: Chain, contract: &str, ticker: &str) -> Entry {
        Entry {
            issuer: "Backed xStocks".to_owned(),
            ticker: ticker.to_owned(),
            name: ticker.to_owned(),
            chain,
            contract: contract.to_owned(),
            decimals: Some(18),
            source: "fixture".to_owned(),
            source_url: "https://example.invalid".to_owned(),
            last_checked: "2026-09-22T00:00:00Z".to_owned(),
            removed_at: None,
            stale_since: None,
            official_deployments: Vec::new(),
        }
    }

    async fn start_local_discovery_fixture(
        app: axum::Router,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let listener =
            tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("fixture listener");
        let base_url = format!("http://{}", listener.local_addr().expect("fixture address"));
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("fixture server");
        });
        (base_url, server)
    }

    #[tokio::test]
    async fn gecko_search_and_discovery_map_recorded_search_token_and_pool_fixtures() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let dex_canary_requests = Arc::new(AtomicUsize::new(0));
        let dex_token_requests = Arc::new(AtomicUsize::new(0));
        let gecko_search_requests = Arc::new(AtomicUsize::new(0));
        let gecko_token_requests = Arc::new(AtomicUsize::new(0));
        let gecko_pool_requests = Arc::new(AtomicUsize::new(0));
        let bad_accept_headers = Arc::new(AtomicUsize::new(0));
        let app = axum::Router::new().fallback({
            let dex_canary_requests = Arc::clone(&dex_canary_requests);
            let dex_token_requests = Arc::clone(&dex_token_requests);
            let gecko_search_requests = Arc::clone(&gecko_search_requests);
            let gecko_token_requests = Arc::clone(&gecko_token_requests);
            let gecko_pool_requests = Arc::clone(&gecko_pool_requests);
            let bad_accept_headers = Arc::clone(&bad_accept_headers);
            move |uri: axum::http::Uri, headers: axum::http::HeaderMap| {
                let path = uri.path().to_owned();
                if path.starts_with("/api/v2/")
                    && headers.get(reqwest::header::ACCEPT).and_then(|value| value.to_str().ok())
                        != Some("application/json")
                {
                    bad_accept_headers.fetch_add(1, Ordering::Relaxed);
                }
                let dex_canary_requests = Arc::clone(&dex_canary_requests);
                let dex_token_requests = Arc::clone(&dex_token_requests);
                let gecko_search_requests = Arc::clone(&gecko_search_requests);
                let gecko_token_requests = Arc::clone(&gecko_token_requests);
                let gecko_pool_requests = Arc::clone(&gecko_pool_requests);
                async move {
                    let body = match path.as_str() {
                        "/latest/dex/search" => {
                            dex_canary_requests.fetch_add(1, Ordering::Relaxed);
                            r#"{"schemaVersion":"1.0.0","pairs":[]}"#.to_owned()
                        }
                        "/api/v2/search/pools" => {
                            gecko_search_requests.fetch_add(1, Ordering::Relaxed);
                            include_str!("../../tests/fixtures/discovery/gecko-search-pools.json")
                                .to_owned()
                        }
                        path if path.starts_with("/api/v2/networks/base/tokens/multi/") => {
                            gecko_token_requests.fetch_add(1, Ordering::Relaxed);
                            include_str!("../../tests/fixtures/discovery/gecko-token-multi.json")
                                .to_owned()
                        }
                        path if path.starts_with("/api/v2/networks/base/pools/multi/") => {
                            gecko_pool_requests.fetch_add(1, Ordering::Relaxed);
                            include_str!("../../tests/fixtures/discovery/gecko-pool-multi.json")
                                .to_owned()
                        }
                        path if path.starts_with("/tokens/v1/") => {
                            dex_token_requests.fetch_add(1, Ordering::Relaxed);
                            "[]".to_owned()
                        }
                        _ => return (axum::http::StatusCode::NOT_FOUND, String::new()),
                    };
                    (axum::http::StatusCode::OK, body)
                }
            }
        });
        let (base_url, server) = start_local_discovery_fixture(app).await;
        let http = reqwest::Client::new();
        let gecko = GeckoTerminalClient::with_base_url(http.clone(), format!("{base_url}/api/v2"));
        let search_pairs = gecko
            .search("NVIDIA xStock", "base", None)
            .await
            .expect("recorded GeckoTerminal search response");
        assert_eq!(search_pairs.len(), 1);
        let search_pair = &search_pairs[0];
        assert_eq!(search_pair.source, MarketSource::Geckoterminal);
        assert_eq!(search_pair.base_token.symbol.as_deref(), Some("NVDAx"));
        assert_eq!(search_pair.quote_token.symbol.as_deref(), Some("USDC"));
        assert_eq!(search_pair.price_usd, Some(132.5));
        assert_eq!(search_pair.volume.as_ref().and_then(|volume| volume.h24), Some(12_345.67));
        assert_eq!(
            search_pair.liquidity.as_ref().and_then(|liquidity| liquidity.usd),
            Some(456_789.01)
        );
        assert_eq!(
            search_pair.url.as_deref(),
            Some(
                "https://www.geckoterminal.com/base/pools/0x0000000000000000000000000000000000000001"
            )
        );

        let registry =
            vec![entry(Chain::Base, "0x0000000000000000000000000000000000000011", "NVDA")];
        let client = MarketDataClient {
            dex: DexScreenerClient::with_base_url(http.clone(), base_url.clone()),
            gecko,
            availability: Arc::new(Mutex::new(DexAvailability::default())),
        };
        let mut gecko_budget = GeckoRegistryBudget::default();
        let discovered = client
            .registry_pools(
                Chain::Base,
                &["0x0000000000000000000000000000000000000011".to_owned()],
                &registry,
                &mut gecko_budget,
            )
            .await
            .expect("empty canary should route discovery through GeckoTerminal");
        assert_eq!(dex_canary_requests.load(Ordering::Relaxed), 1);
        assert_eq!(dex_token_requests.load(Ordering::Relaxed), 0);
        assert_eq!(gecko_search_requests.load(Ordering::Relaxed), 1);
        assert_eq!(gecko_token_requests.load(Ordering::Relaxed), 1);
        assert_eq!(gecko_pool_requests.load(Ordering::Relaxed), 1);
        assert_eq!(bad_accept_headers.load(Ordering::Relaxed), 0);
        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0].base_token.symbol.as_deref(), Some("NVDAx"));
        assert_eq!(discovered[0].source, MarketSource::Geckoterminal);
        server.abort();
    }

    #[test]
    fn gecko_uniswap_v4_pool_maps_to_robinhood_leaderboard_candidate() {
        let response: GeckoApiResponse = serde_json::from_str(include_str!(
            "../../tests/fixtures/discovery/gecko-pool-robinhood-v4.json"
        ))
        .expect("recorded Robinhood V4 pool fixture");
        let pairs = gecko_pools_to_pairs(response, "robinhood");
        assert_eq!(pairs.len(), 1);
        let pair = &pairs[0];
        let pool = "0x6444a8e0b267406a15db74ca00c4a24bdfa81ed3180f5b6d0851f8ed6f4f29c5";
        assert_eq!(pair.pair_address, pool);
        assert_eq!(pair.dex_id, "uniswap");
        assert_eq!(pair.labels, vec!["v4".to_owned()]);
        assert!(valid_pair_address(Chain::RobinhoodChain, pair));

        let registry = vec![entry(
            Chain::RobinhoodChain,
            "0x0000000000000000000000000000000000000011",
            "NVDA",
        )];
        let candidate = leaderboard_candidate_from_pair(pair, &registry)
            .expect("Robinhood V4 pool should survive leaderboard candidate validation");
        assert_eq!(candidate.chain, Chain::RobinhoodChain);
        assert_eq!(candidate.pool, pool);
        assert_eq!(candidate.dex, "uniswap");
        assert_eq!(candidate.source, MarketSource::Geckoterminal);
        assert_eq!(
            candidate.trade_url,
            format!("https://www.geckoterminal.com/robinhood/pools/{pool}")
        );

        let mut name_only_response: GeckoApiResponse = serde_json::from_str(include_str!(
            "../../tests/fixtures/discovery/gecko-pool-robinhood-v4.json"
        ))
        .expect("recorded Robinhood V4 pool fixture");
        let dex = name_only_response
            .included
            .iter_mut()
            .find(|entity| entity.id == "robinhood_uniswap-v4")
            .expect("included V4 DEX");
        dex.attributes["identifier"] = serde_json::json!("uniswap-v2");
        let name_mapped = gecko_pools_to_pairs(name_only_response, "robinhood");
        assert_eq!(name_mapped[0].dex_id, "uniswap");
        assert_eq!(name_mapped[0].labels, vec!["v4".to_owned()]);
    }

    #[tokio::test]
    async fn gecko_fallback_runs_after_a_dexscreener_http_error() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let dex_errors = Arc::new(AtomicUsize::new(0));
        let gecko_requests = Arc::new(AtomicUsize::new(0));
        let bad_accept_headers = Arc::new(AtomicUsize::new(0));
        let app = axum::Router::new().fallback({
            let dex_errors = Arc::clone(&dex_errors);
            let gecko_requests = Arc::clone(&gecko_requests);
            let bad_accept_headers = Arc::clone(&bad_accept_headers);
            move |uri: axum::http::Uri, headers: axum::http::HeaderMap| {
                let path = uri.path().to_owned();
                if path.starts_with("/api/v2/")
                    && headers.get(reqwest::header::ACCEPT).and_then(|value| value.to_str().ok())
                        != Some("application/json")
                {
                    bad_accept_headers.fetch_add(1, Ordering::Relaxed);
                }
                let dex_errors = Arc::clone(&dex_errors);
                let gecko_requests = Arc::clone(&gecko_requests);
                async move {
                    if path.starts_with("/latest/dex/pairs/") {
                        dex_errors.fetch_add(1, Ordering::Relaxed);
                        return (
                            axum::http::StatusCode::SERVICE_UNAVAILABLE,
                            "fixture outage".to_owned(),
                        );
                    }
                    if path.starts_with("/api/v2/networks/base/pools/multi/") {
                        gecko_requests.fetch_add(1, Ordering::Relaxed);
                        return (
                            axum::http::StatusCode::OK,
                            include_str!("../../tests/fixtures/discovery/gecko-pool-multi.json")
                                .to_owned(),
                        );
                    }
                    (axum::http::StatusCode::NOT_FOUND, String::new())
                }
            }
        });
        let (base_url, server) = start_local_discovery_fixture(app).await;
        let http = reqwest::Client::new();
        let client = MarketDataClient {
            dex: DexScreenerClient::with_base_url(http.clone(), base_url.clone()),
            gecko: GeckoTerminalClient::with_base_url(http, format!("{base_url}/api/v2")),
            availability: Arc::new(Mutex::new(DexAvailability {
                checked_at: Some(Instant::now()),
                available: true,
            })),
        };
        let pairs = client
            .pairs(Chain::Base, &["0x0000000000000000000000000000000000000001".to_owned()])
            .await
            .expect("DexScreener HTTP errors should use GeckoTerminal");
        assert_eq!(dex_errors.load(Ordering::Relaxed), 1);
        assert_eq!(gecko_requests.load(Ordering::Relaxed), 1);
        assert_eq!(bad_accept_headers.load(Ordering::Relaxed), 0);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].source, MarketSource::Geckoterminal);
        assert_eq!(
            canonical_market_url_for_source(
                Chain::Base,
                &pairs[0].pair_address,
                pairs[0].url.as_deref(),
                pairs[0].source,
            ),
            "https://www.geckoterminal.com/base/pools/0x0000000000000000000000000000000000000001"
        );
        server.abort();
    }

    #[test]
    fn gecko_budget_waits_for_capacity_and_applies_429_backoff() {
        let start = Instant::now();
        let mut budget = GeckoBudget::default();
        for _ in 0..GECKOTERMINAL_CALLS_PER_MINUTE {
            assert_eq!(budget.delay_or_acquire(start), None);
        }
        assert_eq!(budget.delay_or_acquire(start), Some(GECKOTERMINAL_WINDOW));
        budget.backoff(start);
        assert_eq!(budget.delay_or_acquire(start), Some(GECKOTERMINAL_BACKOFF));
        assert_eq!(budget.delay_or_acquire(start + GECKOTERMINAL_BACKOFF), None);
    }

    #[tokio::test]
    async fn gecko_client_retries_a_429_after_backoff() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let requests = Arc::new(AtomicUsize::new(0));
        let app = axum::Router::new().fallback({
            let requests = Arc::clone(&requests);
            move |uri: axum::http::Uri| {
                let path = uri.path().to_owned();
                let requests = Arc::clone(&requests);
                async move {
                    if path == "/api/v2/search/pools" {
                        if requests.fetch_add(1, Ordering::Relaxed) == 0 {
                            return (axum::http::StatusCode::TOO_MANY_REQUESTS, String::new());
                        }
                        return (axum::http::StatusCode::OK, r#"{"data":[]}"#.to_owned());
                    }
                    (axum::http::StatusCode::NOT_FOUND, String::new())
                }
            }
        });
        let (base_url, server) = start_local_discovery_fixture(app).await;
        let client = GeckoTerminalClient::with_base_url(
            reqwest::Client::new(),
            format!("{base_url}/api/v2"),
        );
        {
            let mut budget = client.budget.lock().await;
            budget.request_limit = usize::MAX;
            budget.backoff_duration = Duration::ZERO;
        }
        let pairs = client
            .search("NVIDIA xStock", "robinhood", None)
            .await
            .expect("GeckoTerminal should retry one rate-limited request");
        assert!(pairs.is_empty());
        assert_eq!(requests.load(Ordering::Relaxed), 2);
        server.abort();
    }

    #[tokio::test]
    async fn gecko_registry_caps_requests_and_validated_pool_ids() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let token_requests = Arc::new(AtomicUsize::new(0));
        let next_pool_id = Arc::new(AtomicUsize::new(0));
        let pool_paths = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let app = axum::Router::new().fallback({
            let token_requests = Arc::clone(&token_requests);
            let next_pool_id = Arc::clone(&next_pool_id);
            let pool_paths = Arc::clone(&pool_paths);
            move |uri: axum::http::Uri| {
                let path = uri.path().to_owned();
                let token_requests = Arc::clone(&token_requests);
                let next_pool_id = Arc::clone(&next_pool_id);
                let pool_paths = Arc::clone(&pool_paths);
                async move {
                    if let Some(addresses) = path
                        .split("/tokens/multi/")
                        .nth(1)
                        .filter(|_| path.contains("/tokens/multi/"))
                    {
                        token_requests.fetch_add(1, Ordering::Relaxed);
                        let addresses = addresses.replace("%2C", ",").replace("%2c", ",");
                        let data = addresses
                            .split(',')
                            .map(|address| {
                                let sequence = next_pool_id.fetch_add(1, Ordering::Relaxed);
                                let mut top_pools = vec![serde_json::json!({
                                    "id": format!("base_0x{:040x}", sequence + 1)
                                })];
                                if sequence == 0 {
                                    top_pools.push(serde_json::json!({"id": "base_not-a-pool"}));
                                }
                                serde_json::json!({
                                    "id": format!("base_{address}"),
                                    "attributes": {"address": address},
                                    "relationships": {
                                        "top_pools": {"data": top_pools}
                                    }
                                })
                            })
                            .collect::<Vec<_>>();
                        return (
                            axum::http::StatusCode::OK,
                            serde_json::json!({"data": data}).to_string(),
                        );
                    }
                    if path.starts_with("/api/v2/networks/base/pools/multi/") {
                        pool_paths.lock().expect("pool request log").push(path);
                        return (axum::http::StatusCode::OK, r#"{"data":[]}"#.to_owned());
                    }
                    (axum::http::StatusCode::NOT_FOUND, String::new())
                }
            }
        });
        let (base_url, server) = start_local_discovery_fixture(app).await;
        let client = GeckoTerminalClient::with_base_url(
            reqwest::Client::new(),
            format!("{base_url}/api/v2"),
        );
        client.budget.lock().await.request_limit = usize::MAX;
        let addresses = (0..630).map(|index| format!("0x{:040x}", index + 1)).collect::<Vec<_>>();
        let registry = Registry::new();
        let mut refresh_budget = GeckoRegistryBudget::default();
        let pairs = client
            .registry_pools(Chain::Base, &addresses, &registry, &mut refresh_budget)
            .await
            .expect("bounded GeckoTerminal registry discovery");
        assert!(pairs.is_empty());
        let pool_paths = pool_paths.lock().expect("pool request log").clone();
        assert_eq!(token_requests.load(Ordering::Relaxed), 4);
        assert_eq!(pool_paths.len(), 2);
        assert_eq!(refresh_budget.requests, 6);
        assert_eq!(refresh_budget.pool_ids.len(), 60);
        let mut requested_pool_ids = Vec::new();
        for path in pool_paths {
            let addresses = path.rsplit('/').next().expect("pool multi path");
            let addresses = addresses.replace("%2C", ",").replace("%2c", ",");
            requested_pool_ids.extend(addresses.split(',').map(str::to_owned));
        }
        assert_eq!(requested_pool_ids.len(), 60);
        assert_eq!(requested_pool_ids.iter().collect::<HashSet<_>>().len(), 60);
        assert!(
            requested_pool_ids.iter().all(|address| valid_gecko_pool_address(Chain::Base, address))
        );
        server.abort();
    }

    #[test]
    fn gecko_registry_budget_covers_each_network_within_the_global_caps() {
        let chains =
            [Chain::Solana, Chain::RobinhoodChain, Chain::Base, Chain::Ethereum, Chain::Bnb];
        let mut budget = GeckoRegistryBudget::default();
        for chain in chains {
            for _ in 0..GECKOTERMINAL_REGISTRY_TOKEN_REQUESTS_PER_CHAIN {
                assert!(budget.try_acquire(chain, GeckoRegistryRequest::Tokens));
            }
            assert!(!budget.try_acquire(chain, GeckoRegistryRequest::Tokens));
            for _ in 0..GECKOTERMINAL_REGISTRY_POOL_REQUESTS_PER_CHAIN {
                assert!(budget.try_acquire(chain, GeckoRegistryRequest::Pools));
            }
            assert!(!budget.try_acquire(chain, GeckoRegistryRequest::Pools));

            let network = gecko_network_id(chain).expect("supported GeckoTerminal network");
            for index in 0..GECKOTERMINAL_REGISTRY_POOL_IDS_PER_CHAIN {
                let address = format!("0x{index:040x}");
                assert!(budget.try_add_pool_id(chain, network, &address));
            }
            let excess = format!("0x{:040x}", GECKOTERMINAL_REGISTRY_POOL_IDS_PER_CHAIN);
            assert!(!budget.try_add_pool_id(chain, network, &excess));
        }
        assert_eq!(budget.requests, GECKOTERMINAL_REGISTRY_REQUESTS_PER_REFRESH);
        assert_eq!(budget.pool_ids.len(), GECKOTERMINAL_REGISTRY_POOL_IDS_PER_REFRESH);
    }

    #[tokio::test]
    async fn watch_fallback_searches_each_product_name_across_networks_without_judging_unsupported()
    {
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let dex_requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let app = axum::Router::new().fallback({
            let requests = Arc::clone(&requests);
            let dex_requests = Arc::clone(&dex_requests);
            move |uri: axum::http::Uri| {
                let path = uri.path().to_owned();
                let url =
                    reqwest::Url::parse(&format!("http://fixture{uri}")).expect("fixture URL");
                let mut query = String::new();
                let mut network = String::new();
                for (key, value) in url.query_pairs() {
                    match key.as_ref() {
                        "query" => query = value.into_owned(),
                        "network" => network = value.into_owned(),
                        _ => {}
                    }
                }
                if path.starts_with("/latest/dex/") {
                    dex_requests.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                if path == "/api/v2/search/pools" {
                    requests.lock().expect("fixture request log").push((query, network.clone()));
                }
                async move {
                    if path == "/api/v2/search/pools" {
                        let body = if network == "arc" {
                            include_str!("../../tests/fixtures/discovery/gecko-search-pools.json")
                        } else {
                            r#"{"data":[]}"#
                        };
                        return (axum::http::StatusCode::OK, body.to_owned());
                    }
                    (axum::http::StatusCode::NOT_FOUND, String::new())
                }
            }
        });
        let (base_url, server) = start_local_discovery_fixture(app).await;
        let mut nvda = entry(Chain::Base, "0x0000000000000000000000000000000000000011", "NVDA");
        nvda.name = "NVIDIA xStock".to_owned();
        let mut robinhood_nvda =
            entry(Chain::RobinhoodChain, "0x0000000000000000000000000000000000000012", "NVDA");
        robinhood_nvda.name = "NVIDIA Robinhood Token".to_owned();
        let state = AppState::for_tests(vec![nvda, robinhood_nvda], Vec::new(), true);
        let registry = state.registry.read().await.clone();
        let http = state.http.clone();
        let gecko = GeckoTerminalClient::with_base_url(http, format!("{base_url}/api/v2"));
        gecko.budget.lock().await.request_limit = 100;
        let client = MarketDataClient::with_clients(
            DexScreenerClient::with_base_url(state.http.clone(), base_url.clone()),
            gecko,
            false,
        );

        let snapshot = refresh_impostor_watch(
            &state,
            &client,
            &registry,
            Some("d".repeat(64)),
            ImpostorSnapshot::default(),
        )
        .await;
        let requests = requests.lock().expect("fixture request log").clone();
        assert_eq!(dex_requests.load(std::sync::atomic::Ordering::Relaxed), 0);
        assert_eq!(requests.len(), 2 * GECKOTERMINAL_WATCH_NETWORKS.len());
        let mut expected = Vec::new();
        for product_name in ["NVIDIA xStock", "NVIDIA Robinhood Token"] {
            for network in GECKOTERMINAL_WATCH_NETWORKS {
                expected.push((product_name.to_owned(), network.to_owned()));
            }
        }
        assert_eq!(
            requests.into_iter().collect::<HashSet<_>>(),
            expected.into_iter().collect::<HashSet<_>>()
        );
        assert!(snapshot.entries.is_empty());
        assert_eq!(snapshot.unsupported_seen, 1);
        assert_eq!(snapshot.unsupported_candidates.len(), 1);
        assert_eq!(snapshot.unsupported_candidates[0].dex_chain_id, "arc");
        assert_eq!(snapshot.unsupported_candidates[0].source, MarketSource::Geckoterminal);
        server.abort();
    }

    #[tokio::test]
    async fn watch_counts_and_excludes_official_ton_spyx_deployment() {
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let app = axum::Router::new().fallback({
            let requests = Arc::clone(&requests);
            move |uri: axum::http::Uri| {
                let path = uri.path().to_owned();
                let url =
                    reqwest::Url::parse(&format!("http://fixture{uri}")).expect("fixture URL");
                let mut query = String::new();
                let mut network = String::new();
                for (key, value) in url.query_pairs() {
                    match key.as_ref() {
                        "query" => query = value.into_owned(),
                        "network" => network = value.into_owned(),
                        _ => {}
                    }
                }
                if path == "/api/v2/search/pools" {
                    requests.lock().expect("fixture request log").push((query, network.clone()));
                }
                async move {
                    if path == "/api/v2/search/pools" {
                        let body = if network == "ton" {
                            include_str!(
                                "../../tests/fixtures/discovery/gecko-search-pools-ton-spyx.json"
                            )
                        } else {
                            r#"{"data":[]}"#
                        };
                        return (axum::http::StatusCode::OK, body.to_owned());
                    }
                    (axum::http::StatusCode::NOT_FOUND, String::new())
                }
            }
        });
        let (base_url, server) = start_local_discovery_fixture(app).await;
        let mut spy = entry(Chain::Base, "0x0000000000000000000000000000000000000012", "SPY");
        spy.name = "SPY xStock".to_owned();
        spy.official_deployments = vec![crate::domain::registry::OfficialDeployment {
            network: "The Open Network".to_owned(),
            address: "EQB1fyBAA9qQDP6LEGaF3cbU-Xbr-p6ESBZGnqlHkHIHAJZv".to_owned(),
            wrapper_address: Some("EQB1fyBAA9qQDP6LEGaF3cbU-Xbr-p6ESBZGnqlHkHIHAJZ1".to_owned()),
            wrapper_address_v2: Some("EQB1fyBAA9qQDP6LEGaF3cbU-Xbr-p6ESBZGnqlHkHIHAJZ2".to_owned()),
        }];
        let state = AppState::for_tests(vec![spy], Vec::new(), true);
        let registry = state.registry.read().await.clone();
        let http = state.http.clone();
        let client = MarketDataClient::with_clients(
            DexScreenerClient::with_base_url(http.clone(), base_url.clone()),
            GeckoTerminalClient::with_base_url(http, format!("{base_url}/api/v2")),
            false,
        );

        let snapshot = refresh_impostor_watch(
            &state,
            &client,
            &registry,
            Some("d".repeat(64)),
            ImpostorSnapshot::default(),
        )
        .await;
        let requests = requests.lock().expect("fixture request log").clone();
        assert_eq!(requests.len(), GECKOTERMINAL_WATCH_NETWORKS.len());
        assert_eq!(
            requests,
            GECKOTERMINAL_WATCH_NETWORKS
                .iter()
                .map(|network| ("SPY xStock".to_owned(), (*network).to_owned()))
                .collect::<Vec<_>>()
        );
        assert!(snapshot.entries.is_empty());
        assert_eq!(snapshot.official_on_unsupported_chain, 1);
        assert_eq!(snapshot.unsupported_seen, 0);
        assert!(snapshot.unsupported_candidates.is_empty());
        server.abort();
    }

    #[test]
    fn official_unsupported_deployment_match_checks_address_and_wrappers() {
        let address = "EQB1fyBAA9qQDP6LEGaF3cbU-Xbr-p6ESBZGnqlHkHIHAJZv";
        let wrapper = "EQB1fyBAA9qQDP6LEGaF3cbU-Xbr-p6ESBZGnqlHkHIHAJZ1";
        let wrapper_v2 = "EQB1fyBAA9qQDP6LEGaF3cbU-Xbr-p6ESBZGnqlHkHIHAJZ2";
        let mut spy = entry(Chain::Base, "0x0000000000000000000000000000000000000012", "SPY");
        spy.official_deployments = vec![crate::domain::registry::OfficialDeployment {
            network: "TON".to_owned(),
            address: address.to_owned(),
            wrapper_address: Some(wrapper.to_owned()),
            wrapper_address_v2: Some(wrapper_v2.to_owned()),
        }];
        let registry = vec![spy];

        assert!(official_unsupported_deployment_matches(&registry, "ton", address));
        assert!(official_unsupported_deployment_matches(&registry, "The Open Network", wrapper));
        assert!(official_unsupported_deployment_matches(&registry, "ton", wrapper_v2));
    }

    #[tokio::test]
    async fn warm_tickers_preserve_featured_then_rank_order_and_deduplicate() {
        let featured = std::sync::Arc::new(tokio::sync::RwLock::new(vec![
            FeaturedPool {
                chain: Chain::Base,
                dex: "test".to_owned(),
                pool: "featured-1".to_owned(),
                base_symbol: String::new(),
                base_address: String::new(),
                quote_symbol: String::new(),
                quote_address: String::new(),
                issuer: None,
                ticker: Some("first".to_owned()),
                verdict: "unknown".to_owned(),
                quote_balance: None,
                quote_share_of_supply: None,
                volume_24h_usd: None,
                liquidity_usd: None,
                curated: false,
                note: None,
                updated_at: String::new(),
            },
            FeaturedPool {
                chain: Chain::Base,
                dex: "test".to_owned(),
                pool: "featured-2".to_owned(),
                base_symbol: String::new(),
                base_address: String::new(),
                quote_symbol: String::new(),
                quote_address: String::new(),
                issuer: None,
                ticker: Some("dup".to_owned()),
                verdict: "unknown".to_owned(),
                quote_balance: None,
                quote_share_of_supply: None,
                volume_24h_usd: None,
                liquidity_usd: None,
                curated: false,
                note: None,
                updated_at: String::new(),
            },
        ]));
        let leaderboard_entry = |rank, ticker: &str| LeaderboardEntry {
            rank,
            chain: "base".to_owned(),
            chain_label: "Base".to_owned(),
            dex: "test".to_owned(),
            pool: format!("rank-{rank}"),
            base_symbol: String::new(),
            quote_symbol: String::new(),
            source: MarketSource::Dexscreener,
            issuer: None,
            ticker: Some(ticker.to_owned()),
            issuer_on_base: None,
            verdict: "unknown".to_owned(),
            read_status: "checked".to_owned(),
            read_reason: None,
            price_usd: None,
            change_24h_pct: None,
            volume_24h_usd: None,
            liquidity_usd: None,
            txns_24h: None,
            detail_url: String::new(),
            trade_url: String::new(),
            explorer_url: String::new(),
            attestation_id: None,
            checked_at: None,
        };
        let leaderboard = std::sync::Arc::new(tokio::sync::RwLock::new(Leaderboard {
            entries: vec![
                leaderboard_entry(2, "second"),
                leaderboard_entry(1, "first"),
                leaderboard_entry(3, "third"),
            ],
            ..Leaderboard::default()
        }));
        let index = DiscoveryPoolIndex::new(featured, leaderboard);
        assert_eq!(index.warm_tickers().await, ["FIRST", "DUP", "SECOND", "THIRD"]);
    }
    #[tokio::test]
    async fn discovery_pool_index_reads_featured_and_attested_leaderboard_rows() {
        let base = "0x0000000000000000000000000000000000000011";
        let quote = "0x0000000000000000000000000000000000000022";
        let featured = std::sync::Arc::new(tokio::sync::RwLock::new(vec![FeaturedPool {
            chain: Chain::Base,
            dex: "test".to_owned(),
            pool: "FeaturedPool".to_owned(),
            base_symbol: "NVDA".to_owned(),
            base_address: base.to_owned(),
            quote_symbol: "USDC".to_owned(),
            quote_address: quote.to_owned(),
            issuer: Some("Backed xStocks".to_owned()),
            ticker: Some("NVDA".to_owned()),
            verdict: "verified".to_owned(),
            quote_balance: None,
            quote_share_of_supply: None,
            volume_24h_usd: None,
            liquidity_usd: None,
            curated: true,
            note: None,
            updated_at: "2026-09-22T00:00:00Z".to_owned(),
        }]));
        let mut attestation = crate::app::attestation::signed_test_attestation([7; 32], false);
        attestation.chain = Chain::Base;
        attestation.pool.chain = Chain::Base;
        attestation.pool.pool = "FeaturedPool".to_owned();
        attestation.pool.base.address = base.to_owned();
        attestation.pool.quote.address = quote.to_owned();
        let attestation_id = attestation.id.clone();
        let attestations = std::collections::HashMap::from([(attestation_id.clone(), attestation)]);
        let leaderboard_entry = |rank, pool: &str, attestation_id: Option<&str>| LeaderboardEntry {
            rank,
            chain: "base".to_owned(),
            chain_label: "Base".to_owned(),
            dex: "test".to_owned(),
            pool: pool.to_owned(),
            base_symbol: "NVDA".to_owned(),
            quote_symbol: "USDC".to_owned(),
            source: MarketSource::Dexscreener,
            issuer: Some("Backed xStocks".to_owned()),
            ticker: Some("NVDA".to_owned()),
            issuer_on_base: None,
            verdict: "verified".to_owned(),
            read_status: "checked".to_owned(),
            read_reason: None,
            price_usd: None,
            change_24h_pct: None,
            volume_24h_usd: None,
            liquidity_usd: None,
            txns_24h: None,
            detail_url: format!("https://qed.example/validated/base/{pool}"),
            trade_url: "https://dexscreener.com/base/featured-pool".to_owned(),
            explorer_url: String::new(),
            attestation_id: attestation_id.map(str::to_owned),
            checked_at: None,
        };
        let leaderboard = std::sync::Arc::new(tokio::sync::RwLock::new(Leaderboard {
            entries: vec![
                leaderboard_entry(1, "FEATUREDPOOL", Some(&attestation_id)),
                leaderboard_entry(2, "ranked-pool", Some(&attestation_id)),
                leaderboard_entry(3, "unlinked-pool", Some("missing")),
            ],
            ..Leaderboard::default()
        }));
        let index = DiscoveryPoolIndex::new(featured, leaderboard);

        assert_eq!(
            index.candidate_pools(Chain::Base, &base.to_ascii_uppercase(), &attestations).await,
            ["FeaturedPool", "ranked-pool"]
        );
        let known = index.known_pools(&attestations).await;
        let mut known_pairs = known
            .iter()
            .map(|pool| (pool.pool.clone(), pool.token_address.clone()))
            .collect::<Vec<_>>();
        known_pairs.sort();
        assert_eq!(
            known_pairs,
            [
                ("FEATUREDPOOL".to_owned(), base.to_owned()),
                ("FEATUREDPOOL".to_owned(), quote.to_owned()),
            ]
        );
    }
    #[tokio::test]
    async fn known_pools_bind_solana_attestations_to_exact_leaderboard_chain_and_pool() {
        let pool = bs58::encode([31u8; 32]).into_string();
        let variant = pool
            .char_indices()
            .find_map(|(index, character)| {
                let replacement = if character.is_ascii_lowercase() {
                    character.to_ascii_uppercase()
                } else if character.is_ascii_uppercase() {
                    character.to_ascii_lowercase()
                } else {
                    return None;
                };
                let mut candidate = pool.clone();
                candidate.replace_range(index..index + 1, &replacement.to_string());
                Chain::decode_solana_address(&candidate).is_some().then_some(candidate)
            })
            .expect("valid Solana case variant");
        let leaderboard_entry =
            |rank, row_pool: &str, attestation_id: Option<&str>| LeaderboardEntry {
                rank,
                chain: "solana".to_owned(),
                chain_label: "Solana".to_owned(),
                dex: "test".to_owned(),
                pool: row_pool.to_owned(),
                base_symbol: "NVDA".to_owned(),
                quote_symbol: "USDC".to_owned(),
                issuer: Some("Backed xStocks".to_owned()),
                source: MarketSource::Dexscreener,
                ticker: Some("NVDA".to_owned()),
                issuer_on_base: None,
                verdict: "verified".to_owned(),
                read_status: "checked".to_owned(),
                read_reason: None,
                price_usd: None,
                change_24h_pct: None,
                volume_24h_usd: None,
                liquidity_usd: None,
                txns_24h: None,
                detail_url: String::new(),
                trade_url: String::new(),
                explorer_url: String::new(),
                attestation_id: attestation_id.map(str::to_owned),
                checked_at: None,
            };
        let empty_featured = || std::sync::Arc::new(tokio::sync::RwLock::new(Vec::new()));

        let mut exact_solana = crate::app::attestation::signed_test_attestation([7; 32], false);
        exact_solana.chain = Chain::Solana;
        exact_solana.pool.chain = Chain::Solana;
        exact_solana.pool.pool = pool.clone();
        let exact_id = exact_solana.id.clone();
        let exact_attestations =
            std::collections::HashMap::from([(exact_id.clone(), exact_solana)]);
        let case_variant_index = DiscoveryPoolIndex::new(
            empty_featured(),
            std::sync::Arc::new(tokio::sync::RwLock::new(Leaderboard {
                entries: vec![
                    leaderboard_entry(1, &variant, Some(&exact_id)),
                    leaderboard_entry(2, &variant, None),
                ],
                ..Leaderboard::default()
            })),
        );
        assert!(case_variant_index.known_pools(&exact_attestations).await.is_empty());

        let mut wrong_pool_chain = crate::app::attestation::signed_test_attestation([8; 32], false);
        wrong_pool_chain.chain = Chain::Solana;
        wrong_pool_chain.pool.chain = Chain::Base;
        wrong_pool_chain.pool.pool = pool.clone();
        let wrong_pool_id = wrong_pool_chain.id.clone();
        let wrong_pool_attestations =
            std::collections::HashMap::from([(wrong_pool_id.clone(), wrong_pool_chain)]);
        let wrong_pool_index = DiscoveryPoolIndex::new(
            empty_featured(),
            std::sync::Arc::new(tokio::sync::RwLock::new(Leaderboard {
                entries: vec![
                    leaderboard_entry(1, &pool, Some(&wrong_pool_id)),
                    leaderboard_entry(2, &pool, None),
                ],
                ..Leaderboard::default()
            })),
        );
        assert!(wrong_pool_index.known_pools(&wrong_pool_attestations).await.is_empty());

        let mut wrong_attestation_chain =
            crate::app::attestation::signed_test_attestation([9; 32], false);
        wrong_attestation_chain.chain = Chain::Base;
        wrong_attestation_chain.pool.chain = Chain::Solana;
        wrong_attestation_chain.pool.pool = pool.clone();
        let wrong_chain_id = wrong_attestation_chain.id.clone();
        let wrong_chain_attestations =
            std::collections::HashMap::from([(wrong_chain_id.clone(), wrong_attestation_chain)]);
        let wrong_chain_index = DiscoveryPoolIndex::new(
            empty_featured(),
            std::sync::Arc::new(tokio::sync::RwLock::new(Leaderboard {
                entries: vec![leaderboard_entry(1, &pool, Some(&wrong_chain_id))],
                ..Leaderboard::default()
            })),
        );
        assert!(wrong_chain_index.known_pools(&wrong_chain_attestations).await.is_empty());
    }

    #[test]
    fn first_discovery_retries_until_registry_has_active_entries() {
        let mut registry = vec![entry(Chain::Base, "0xabc", "STOCK")];
        registry[0].stale_since = Some("2026-09-27T00:00:00Z".to_owned());
        assert!(startup_discovery_waits_for_registry(true, &registry));
        assert!(!startup_discovery_waits_for_registry(false, &registry));

        registry[0].stale_since = None;
        assert!(!startup_discovery_waits_for_registry(true, &registry));
    }

    #[test]
    fn restored_discovery_delay_accepts_only_fresh_non_empty_boards() {
        let now = DateTime::parse_from_rfc3339("2026-09-30T12:00:00Z")
            .expect("fixed timestamp parses")
            .with_timezone(&Utc);
        assert_eq!(
            restored_discovery_delay("2026-09-30T09:00:00Z", true, now),
            Some(Duration::from_secs(3 * 60 * 60))
        );
        assert_eq!(
            restored_discovery_delay("2026-09-30T06:00:01Z", true, now),
            Some(Duration::from_secs(1))
        );
        assert_eq!(restored_discovery_delay("2026-09-30T05:59:59Z", true, now), None);
        assert_eq!(restored_discovery_delay("2026-09-30T12:00:01Z", true, now), None);
        assert_eq!(restored_discovery_delay("not-a-timestamp", true, now), None);
        assert_eq!(restored_discovery_delay("2026-09-30T06:00:01Z", false, now), None);
    }

    #[test]
    fn first_discovery_runs_immediately_for_restored_boards() {
        let now = DateTime::parse_from_rfc3339("2026-09-30T12:00:00Z")
            .expect("fixed timestamp parses")
            .with_timezone(&Utc);
        let older = restored_discovery_delay("2026-09-30T07:00:00Z", true, now);
        let newer = restored_discovery_delay("2026-09-30T11:00:00Z", true, now);

        assert_eq!(first_discovery_delay(older, newer), Some(Duration::ZERO));
        assert_eq!(first_discovery_delay(older, None), None);
        assert_eq!(first_discovery_delay(None, newer), None);
    }

    #[test]
    fn failed_empty_discovery_retries_but_successful_empty_does_not() {
        assert!(should_retry_empty_discovery(5, 5, 0));
        assert!(should_retry_empty_discovery(5, 1, 0));
        assert!(!should_retry_empty_discovery(5, 0, 0));
        assert!(!should_retry_empty_discovery(0, 0, 0));
    }

    #[cfg(feature = "s3")]
    #[tokio::test]
    async fn durable_board_body_rejects_oversized_stream_even_with_small_length() {
        let bytes = vec![0_u8; MAX_DURABLE_BOARD_BYTES + 1];
        let error = read_bounded_body(std::io::Cursor::new(bytes.clone()), None)
            .await
            .expect_err("body must be capped");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        let error = read_bounded_body(std::io::Cursor::new(bytes), Some(1))
            .await
            .expect_err("body must be capped");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn dex_budget_shares_global_ceiling_and_endpoint_backoff() {
        let mut budget = DexBudget {
            all_requests: VecDeque::new(),
            endpoint_requests: HashMap::new(),
            backoff_until: HashMap::new(),
        };
        for _ in 0..DEX_GLOBAL_REQUESTS_PER_MINUTE {
            assert!(budget.try_acquire(DexEndpoint::TokenPairs).is_ok());
        }
        assert!(matches!(
            budget.try_acquire(DexEndpoint::Pairs),
            Err(DexBudgetError::Exhausted(DexEndpoint::Pairs))
        ));
        budget.backoff(DexEndpoint::TokenPairs);
        assert!(matches!(
            budget.try_acquire(DexEndpoint::TokenPairs),
            Err(DexBudgetError::Backoff(DexEndpoint::TokenPairs))
        ));
    }

    #[test]
    fn maps_custom_quote_fixture_when_quote_is_registry_token() {
        let response: DexSearchResponse = serde_json::from_str(include_str!(
            "../../tests/fixtures/discovery/search-nvdax-pump.json"
        ))
        .expect("fixture parses");
        let registry =
            vec![entry(Chain::Solana, "Xsc9qvGR1efVDFGLrVsmkzv3qi45LTBjeUKSPmx9qEh", "NVDA")];
        let candidates = candidates_from_pairs(&registry, response.pairs);
        assert!(candidates.iter().any(|candidate| candidate.quote_symbol == "NVDAx"));
    }

    #[test]
    fn deduplicates_and_orders_by_liquidity() {
        let candidate = |pool: &str, liquidity: f64| DiscoveryCandidate {
            chain: Chain::Solana,
            dex: "pumpfun".to_owned(),
            pool: pool.to_owned(),
            base_symbol: "BASE".to_owned(),
            base_address: "base".to_owned(),
            quote_symbol: "NVDAx".to_owned(),
            quote_address: "quote".to_owned(),
            issuer: Some("Issuer".to_owned()),
            ticker: Some("NVDA".to_owned()),
            volume_24h_usd: None,
            liquidity_usd: Some(liquidity),
            curated: false,
            note: None,
        };
        let ordered = dedup_and_order([
            candidate("0xA", 100.0),
            candidate("0xa", 200.0),
            candidate("0xB", 500.0),
        ]);
        assert_eq!(ordered.len(), 2);
        assert_eq!(ordered[0].pool, "0xB");
        assert_eq!(ordered[1].liquidity_usd, Some(200.0));
    }

    #[test]
    fn curated_candidates_stay_first_and_discovered_fill_the_cap() {
        let candidate = |pool: &str, liquidity: f64| DiscoveryCandidate {
            chain: Chain::Solana,
            dex: "pumpfun".to_owned(),
            pool: pool.to_owned(),
            base_symbol: "BASE".to_owned(),
            base_address: "base".to_owned(),
            quote_symbol: "NVDAx".to_owned(),
            quote_address: "quote".to_owned(),
            issuer: Some("Issuer".to_owned()),
            ticker: Some("NVDA".to_owned()),
            volume_24h_usd: None,
            liquidity_usd: Some(liquidity),
            curated: false,
            note: None,
        };
        let curated = |pool: &str, note: &str| {
            let mut candidate = candidate(pool, 0.0);
            candidate.curated = true;
            candidate.liquidity_usd = None;
            candidate.note = Some(note.to_owned());
            candidate
        };
        let mut candidates = vec![curated("curated-a", "first"), curated("curated-b", "second")];
        candidates.extend([
            candidate("0xD1", 100.0),
            candidate("0xd1", 1_200.0),
            candidate("0xD2", 1_100.0),
            candidate("0xD3", 1_000.0),
            candidate("0xD4", 900.0),
            candidate("0xD5", 800.0),
            candidate("0xD6", 700.0),
            candidate("0xD7", 600.0),
            candidate("0xD8", 500.0),
            candidate("0xD9", 400.0),
            candidate("0xD10", 300.0),
            candidate("0xD11", 200.0),
        ]);

        let ordered = dedup_and_order(candidates);

        assert_eq!(ordered.len(), MAX_FEATURED_POOLS);
        assert_eq!(ordered[0].pool, "curated-a");
        assert_eq!(ordered[1].pool, "curated-b");
        assert!(ordered[..2].iter().all(|candidate| candidate.curated));
        assert_eq!(ordered[2].pool, "0xd1");
        assert_eq!(ordered[2].liquidity_usd, Some(1_200.0));
        assert_eq!(ordered[ordered.len() - 1].pool, "0xD10");
        assert!(ordered[2..].iter().all(|candidate| !candidate.curated));
    }

    #[test]
    fn featured_registry_order_and_notes_are_preserved() {
        let curated: Vec<CuratedPool> =
            serde_json::from_str(include_str!("../../registry/featured.json"))
                .expect("featured registry parses");
        let ordered = dedup_and_order(curated.iter().cloned().map(candidate_from_curated));

        assert_eq!(ordered.len(), curated.len());
        for (candidate, source) in ordered.iter().zip(&curated) {
            assert!(candidate.curated);
            assert_eq!(candidate.pool, source.pool);
            assert_eq!(candidate.note.as_deref(), Some(source.note.as_str()));
        }
    }

    #[test]
    fn unknown_checks_remain_cards_with_reason_or_curated_note() {
        let candidate = DiscoveryCandidate {
            chain: Chain::Solana,
            dex: "pumpfun".to_owned(),
            pool: "pool".to_owned(),
            base_symbol: String::new(),
            base_address: String::new(),
            quote_symbol: String::new(),
            quote_address: String::new(),
            issuer: None,
            ticker: None,
            volume_24h_usd: None,
            liquidity_usd: None,
            curated: false,
            note: None,
        };
        let result = CheckResult {
            input: candidate.pool.clone(),
            chain: Chain::Solana,
            pool: None,
            verdict: Verdict::Unknown { reason: "RPC unavailable".to_owned() },
            quote_share_of_supply: None,
            evidence: Vec::new(),
            checked_at: "2026-09-22T00:00:00Z".to_owned(),
            attestation_id: None,
            powers: None,
            read_issue: Some(crate::domain::check::CheckReadIssue::Transient),
        };

        let card = featured_from_check(candidate.clone(), result.clone());
        assert_eq!(card.verdict, "unknown");
        assert_eq!(card.note.as_deref(), Some("RPC unavailable"));
        assert!(!card.curated);

        let curated_candidate = DiscoveryCandidate {
            curated: true,
            note: Some("curated context".to_owned()),
            ..candidate
        };
        let curated_card = featured_from_check(curated_candidate, result);
        assert_eq!(curated_card.verdict, "unknown");
        assert_eq!(curated_card.note.as_deref(), Some("curated context"));
        assert!(curated_card.curated);
    }

    #[test]
    fn formats_balances_with_decimals_and_grouping() {
        assert_eq!(
            format_balance("123456789", Some(6), Some("USDG")),
            Some("123.456789 USDG".to_owned())
        );
        assert_eq!(
            format_balance("123456789000", Some(6), Some("USDG")),
            Some("123,456.789 USDG".to_owned())
        );
        assert_eq!(format_balance("42", Some(2), None), Some("0.42".to_owned()));
        assert_eq!(format_balance("nope", Some(2), None), None);
    }

    fn fixture_address(name: &str) -> String {
        let seed = name.bytes().fold(0u8, |sum, byte| sum.wrapping_add(byte.to_ascii_lowercase()));
        bs58::encode([seed; 32]).into_string()
    }

    fn leaderboard_pair(pool: &str, chain: Chain, volume: Option<f64>) -> DexPair {
        let pool = fixture_address(pool);
        let base = fixture_address("base-token");
        let stock = fixture_address("stock-token");
        DexPair {
            chain_id: chain_slug(chain).to_owned(),
            dex_id: "fixture-dex".to_owned(),
            url: None,
            pair_address: pool,
            labels: Vec::new(),
            base_token: DexToken {
                address: Some(base),
                name: Some("Base".to_owned()),
                symbol: Some("BASE".to_owned()),
            },
            quote_token: DexToken {
                address: Some(stock),
                name: Some("Stock".to_owned()),
                symbol: Some("STOCKx".to_owned()),
            },
            price_usd: Some(12.5),
            volume: volume.map(|h24| DexVolume { h24: Some(h24), h6: None, h1: None, m5: None }),
            price_change: Some(DexPriceChange { h24: Some(-2.5) }),
            txns: Some(DexTransactions {
                h24: Some(DexTxnWindow { buys: Some(4), sells: Some(6) }),
            }),
            liquidity: Some(DexLiquidity { usd: Some(500.0), base: None, quote: None }),
            source: MarketSource::Dexscreener,
        }
    }

    #[test]
    fn leaderboard_fixture_uses_exact_source_market_url() {
        let pairs: Vec<DexPair> = serde_json::from_str(include_str!(
            "../../tests/fixtures/discovery/token-pairs-robinhood-nvda.json"
        ))
        .expect("fixture parses");
        let registry = vec![entry(
            Chain::RobinhoodChain,
            "0xd0601CE157Db5bdC3162BbaC2a2C8aF5320D9EEC",
            "NVDA",
        )];
        let candidate = leaderboard_candidates_from_pairs(&registry, pairs)
            .into_iter()
            .next()
            .expect("fixture has a stock pair");
        assert_eq!(
            candidate.trade_url,
            "https://dexscreener.com/robinhood/0xd4eb21209c4d6093f80b5b84f5c45cc093ea14a3"
        );
        assert!(!candidate.trade_url.contains("robinhoodchain"));
    }

    #[test]
    fn market_url_fallback_maps_supported_dex_chain_ids() {
        let pool = "0xd4EB21209C4D6093f80B5b84f5C45cc093EA14a3";
        assert_eq!(
            dex_pair_url(Chain::RobinhoodChain, pool),
            format!("https://dexscreener.com/robinhood/{pool}")
        );
        assert_eq!(dex_pair_url(Chain::Bnb, pool), format!("https://dexscreener.com/bsc/{pool}"));
        assert_eq!(
            canonical_market_url(
                Chain::RobinhoodChain,
                pool,
                Some("https://dexscreener.com/robinhoodchain/0xdead")
            ),
            format!("https://dexscreener.com/robinhood/{pool}")
        );
        assert_eq!(
            canonical_market_url(
                Chain::RobinhoodChain,
                pool,
                Some("https://evil.example/robinhood/0xdead")
            ),
            format!("https://dexscreener.com/robinhood/{pool}")
        );
        assert_eq!(
            canonical_market_url(
                Chain::RobinhoodChain,
                pool,
                Some(
                    "https://dexscreener.com/robinhood/0xd4eb21209c4d6093f80b5b84f5c45cc093ea14a3"
                )
            ),
            "https://dexscreener.com/robinhood/0xd4eb21209c4d6093f80b5b84f5c45cc093ea14a3"
        );
        let fallback = format!("https://dexscreener.com/robinhood/{pool}");
        for source_url in [
            format!("https://dexscreener.com:443/robinhood/{pool}"),
            format!("https://user@dexscreener.com/robinhood/{pool}"),
            format!("https://dexscreener.com.evil.test/robinhood/{pool}"),
            format!("https://dexscreener.com/%72obinhood/{pool}"),
        ] {
            assert_eq!(
                canonical_market_url(Chain::RobinhoodChain, pool, Some(&source_url)),
                fallback
            );
        }
        let solana_pool = "AbcDEf123";
        assert_eq!(
            canonical_market_url(
                Chain::Solana,
                solana_pool,
                Some("https://dexscreener.com/solana/abcdef123")
            ),
            format!("https://dexscreener.com/solana/{solana_pool}")
        );
    }

    #[test]
    fn ethereum_v4_pool_id_is_kept_when_registry_token_matches() {
        let mut pair = leaderboard_pair("v4-pool", Chain::Ethereum, Some(2_000.0));
        pair.chain_id = "ethereum".to_owned();
        pair.dex_id = "uniswap".to_owned();
        pair.pair_address = format!("0x{}", "ab".repeat(32));
        pair.labels = vec!["v4".to_owned()];
        pair.base_token.address = Some(format!("0x{}", "01".repeat(20)));
        pair.quote_token.address = Some("0xc845b2894dBddd03858fd2D643B4eF725fE0849d".to_owned());
        let registry =
            vec![entry(Chain::Ethereum, "0xc845b2894dBddd03858fd2D643B4eF725fE0849d", "NVDA")];
        let mut no_label = pair.clone();
        no_label.labels.clear();
        assert!(leaderboard_candidates_from_pairs(&registry, [no_label]).is_empty());
        let mut wrong_dex = pair.clone();
        wrong_dex.dex_id = "other-dex".to_owned();
        assert!(leaderboard_candidates_from_pairs(&registry, [wrong_dex]).is_empty());
        let candidate = leaderboard_candidates_from_pairs(&registry, [pair])
            .pop()
            .expect("official Ethereum NVDA v4 pair is retained");
        assert_eq!(candidate.chain, Chain::Ethereum);
        assert_eq!(candidate.ticker.as_deref(), Some("NVDA"));
        assert!(!candidate.issuer_on_base);
        assert_eq!(
            candidate.trade_url,
            format!("https://dexscreener.com/ethereum/0x{}", "ab".repeat(32))
        );
    }
    #[test]
    fn dex_screener_labels_accept_64_bytes_and_reject_65() {
        let stock_address = fixture_address("stock-token");
        let registry = vec![entry(Chain::Solana, &stock_address, "STOCK")];
        let label = "x".repeat(64);
        let mut pair = leaderboard_pair("bounded-labels", Chain::Solana, Some(2.0));
        pair.dex_id = label.clone();
        pair.base_token.symbol = Some(label.clone());
        pair.base_token.name = Some(label.clone());
        pair.quote_token.symbol = Some(label.clone());
        pair.quote_token.name = Some(label);

        assert_eq!(leaderboard_candidates_from_pairs(&registry, [pair.clone()]).len(), 1);
        for field in 0..5 {
            let mut oversized = pair.clone();
            let label = "x".repeat(65);
            match field {
                0 => oversized.dex_id = label,
                1 => oversized.base_token.symbol = Some(label),
                2 => oversized.base_token.name = Some(label),
                3 => oversized.quote_token.symbol = Some(label),
                4 => oversized.quote_token.name = Some(label),
                _ => unreachable!(),
            }
            assert!(
                leaderboard_candidates_from_pairs(&registry, [oversized]).is_empty(),
                "overlong DexScreener field {field} must be rejected"
            );
        }
    }

    fn leaderboard_candidate(
        chain: Chain,
        pool: &str,
        volume: Option<f64>,
    ) -> LeaderboardCandidate {
        LeaderboardCandidate {
            chain,
            dex: "fixture-dex".to_owned(),
            pool: pool.to_owned(),
            base_symbol: "BASE".to_owned(),
            quote_symbol: "STOCKx".to_owned(),
            issuer: Some("Issuer".to_owned()),
            ticker: Some("STOCK".to_owned()),
            issuer_on_base: false,
            price_usd: Some(1.0),
            change_24h_pct: None,
            volume_24h_usd: volume,
            liquidity_usd: None,
            txns_24h: None,
            trade_url: "https://example.invalid".to_owned(),
            source: MarketSource::Dexscreener,
        }
    }
    struct TransientPositionReader(std::sync::Arc<std::sync::atomic::AtomicUsize>);

    #[async_trait]
    impl crate::ports::ChainReader for TransientPositionReader {
        fn chain(&self) -> Chain {
            Chain::Base
        }

        async fn read_pool(
            &self,
            _address: &str,
        ) -> Result<PoolInfo, crate::domain::pool::PoolError> {
            Err(crate::domain::pool::PoolError::Unknown("not a pool".to_owned()))
        }

        async fn code_at(&self, _address: &str) -> Result<Vec<u8>, crate::domain::pool::PoolError> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(vec![1])
        }

        async fn record_position(&self) -> Result<(), crate::domain::pool::PoolError> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Err(crate::domain::pool::PoolError::Reader("temporary timeout".to_owned()))
        }
    }

    #[tokio::test]
    async fn transient_leaderboard_read_is_retried_on_the_next_refresh() {
        let pool = "0x0000000000000000000000000000000000000011";
        let registry =
            vec![entry(Chain::Base, "0x0000000000000000000000000000000000000022", "NVDA")];
        let reads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let state = crate::adapters::state::AppState::for_tests(
            registry.clone(),
            vec![Box::new(TransientPositionReader(std::sync::Arc::clone(&reads)))],
            false,
        );
        let initial = check::check(&state.app, pool).await;
        let candidate = leaderboard_candidate(Chain::Base, pool, None);
        let initial_entry = leaderboard_entry(1, candidate.clone(), initial.clone());
        assert_eq!(initial_entry.read_status, "not_read_yet");
        assert_eq!(initial_entry.read_reason.as_deref(), Some("transient"));
        state
            .leaderboard_check_cache
            .insert((Chain::Base, pool.to_ascii_lowercase()), initial)
            .await;
        *state.leaderboard.write().await =
            Leaderboard { total: 1, entries: vec![initial_entry], ..Leaderboard::default() };

        let directory = tempfile::tempdir().expect("temporary leaderboard directory");
        refresh_leaderboard(&state, directory.path(), &registry, vec![candidate]).await;

        assert_eq!(reads.load(std::sync::atomic::Ordering::Relaxed), 4);
        let refreshed = state.leaderboard.read().await;
        assert_eq!(refreshed.entries[0].read_status, "not_read_yet");
        assert_eq!(refreshed.entries[0].read_reason.as_deref(), Some("transient"));
    }

    #[test]
    fn leaderboard_fixture_maps_metrics_and_urls() {
        let pairs: Vec<DexPair> = serde_json::from_str(include_str!(
            "../../tests/fixtures/discovery/token-pairs-robinhood-nvda.json"
        ))
        .expect("fixture parses");
        let registry = vec![entry(
            Chain::RobinhoodChain,
            "0xd0601CE157Db5bdC3162BbaC2a2C8aF5320D9EEC",
            "NVDA",
        )];
        let candidate = leaderboard_candidates_from_pairs(&registry, pairs)
            .into_iter()
            .next()
            .expect("fixture has a stock pair");
        assert_eq!(candidate.ticker.as_deref(), Some("NVDA"));
        assert!(candidate.price_usd.is_some());
        assert!(candidate.change_24h_pct.is_some());
        assert!(candidate.volume_24h_usd.is_some());
        assert!(candidate.liquidity_usd.is_some());
        assert!(candidate.txns_24h.is_some());
        assert!(candidate.trade_url.starts_with("https://dexscreener.com/"));
    }

    #[test]
    fn leaderboard_null_metrics_keep_candidate_and_serialize_as_none() {
        let pair = DexPair {
            price_usd: None,
            volume: None,
            price_change: None,
            txns: None,
            liquidity: None,
            ..leaderboard_pair("null-pool", Chain::Solana, None)
        };
        let registry = vec![entry(Chain::Solana, &fixture_address("stock-token"), "STOCK")];
        let candidate = leaderboard_candidates_from_pairs(&registry, [pair])
            .pop()
            .expect("stock pairing is retained");
        assert_eq!(candidate.price_usd, None);
        assert_eq!(candidate.volume_24h_usd, None);
        assert_eq!(candidate.txns_24h, None);
        assert_eq!(candidate.liquidity_usd, None);
    }

    #[test]
    fn leaderboard_deduplicates_by_chain_and_pool_using_best_metrics() {
        let registry = vec![entry(Chain::Solana, &fixture_address("stock-token"), "STOCK")];
        let pairs = vec![
            leaderboard_pair("same-pool", Chain::Solana, Some(10.0)),
            leaderboard_pair("SAME-POOL", Chain::Solana, Some(100.0)),
            leaderboard_pair("other-pool", Chain::Solana, Some(50.0)),
        ];
        let candidates =
            dedup_leaderboard_candidates(leaderboard_candidates_from_pairs(&registry, pairs));
        assert_eq!(candidates.len(), 2);
        assert_eq!(
            candidates
                .iter()
                .find(|candidate| candidate.pool == fixture_address("same-pool"))
                .and_then(|candidate| candidate.volume_24h_usd),
            Some(100.0)
        );
    }

    #[test]
    fn leaderboard_cap_keeps_the_highest_volume_candidates() {
        let mut candidates = Vec::new();
        for index in 0..150 {
            candidates.push(leaderboard_candidate(
                Chain::Solana,
                &format!("solana-{index}"),
                Some(1.0),
            ));
            candidates.push(leaderboard_candidate(
                Chain::RobinhoodChain,
                &format!("robinhood-{index}"),
                Some(1.0),
            ));
        }
        candidates.extend((0..100).map(|index| {
            leaderboard_candidate(Chain::Base, &format!("base-{index}"), Some(1_000.0))
        }));
        let capped = truncate_leaderboard_candidates(candidates);
        assert_eq!(capped.len(), MAX_LEADERBOARD_CANDIDATES);
        assert!(capped.iter().take(100).all(|candidate| candidate.chain == Chain::Base));
    }

    #[test]
    fn leaderboard_ranking_sorts_volume_and_places_null_last() {
        let ranked = rank_leaderboard_candidates(vec![
            leaderboard_candidate(Chain::Solana, "low", Some(1.0)),
            leaderboard_candidate(Chain::Solana, "missing", None),
            leaderboard_candidate(Chain::Solana, "high", Some(3.0)),
        ]);
        assert_eq!(
            ranked.iter().map(|candidate| candidate.pool.as_str()).collect::<Vec<_>>(),
            vec!["high", "low", "missing"]
        );
    }

    #[test]
    fn publisher_catalog_search_matches_only_ticker_or_ticker_x() {
        for symbol in ["NVDA", "nvda", "NVDAx", "nvdax", " NVDAx "] {
            assert!(symbol_matches_search_ticker(symbol, "NVDA"), "{symbol}");
        }
        for symbol in ["XNVD", "NVDAxx", "NVDA X", "NVDA+"] {
            assert!(!symbol_matches_search_ticker(symbol, "NVDA"), "{symbol}");
        }
    }

    #[test]
    fn publisher_catalog_search_uses_unique_active_tickers() {
        let mut removed = entry(Chain::Base, "removed", "OLD");
        removed.removed_at = Some("2026-09-22T00:00:00Z".to_owned());
        let tickers = active_search_tickers(&vec![
            entry(Chain::Base, "tsla-base", "TSLA"),
            entry(Chain::Ethereum, "nvda-eth", "NVDA"),
            entry(Chain::Base, "nvda-base", "nvda"),
            removed,
        ]);
        assert_eq!(tickers, vec!["NVDA", "TSLA"]);
    }

    #[test]
    fn publisher_catalog_search_prioritizes_leaderboard_volume_and_queries_ticker_variants() {
        let registry = vec![entry(Chain::Base, "tsla", "TSLA"), entry(Chain::Base, "nvda", "NVDA")];
        let mut board = sample_leaderboard("2026-10-06T00:00:00Z", 2);
        board.entries[0].base_symbol = "TSLAx".to_owned();
        board.entries[0].volume_24h_usd = Some(10.0);
        board.entries[1].base_symbol = "NVDAx".to_owned();
        board.entries[1].volume_24h_usd = Some(100.0);
        let tickers = prioritize_search_tickers(&registry, &board.entries);
        assert_eq!(tickers, vec!["NVDA", "TSLA"]);
        let (searches, next_offset) = rotating_impostor_search_terms(&tickers, 0);
        assert_eq!(next_offset, 0);
        assert_eq!(
            searches,
            vec![
                ("NVDA".to_owned(), "NVDA".to_owned()),
                ("NVDA".to_owned(), "NVDAx".to_owned()),
                ("TSLA".to_owned(), "TSLA".to_owned()),
                ("TSLA".to_owned(), "TSLAx".to_owned()),
            ]
        );
    }
    #[test]
    fn hot_watch_tickers_are_reserved_while_tail_searches_rotate() {
        let tickers = (0..50).map(|index| format!("T{index:02}")).collect::<Vec<_>>();
        let (first, first_offset) = rotating_impostor_search_terms(&tickers, 0);
        let (second, second_offset) = rotating_impostor_search_terms(&tickers, first_offset);

        assert_eq!(first.len(), MAX_IMPOSTOR_SEARCHES_PER_REFRESH);
        assert_eq!(second.len(), MAX_IMPOSTOR_SEARCHES_PER_REFRESH);
        assert_eq!(first_offset, 30);
        assert_eq!(second_offset, 60);
        for ticker in &tickers[..IMPOSTOR_HOT_TICKERS_PER_REFRESH] {
            assert!(first[..20].contains(&(ticker.clone(), ticker.clone())));
            assert!(first[..20].contains(&(ticker.clone(), format!("{ticker}x"))));
            assert!(second[..20].contains(&(ticker.clone(), ticker.clone())));
            assert!(second[..20].contains(&(ticker.clone(), format!("{ticker}x"))));
        }
        let first_tail = first[20..].iter().map(|(_, term)| term.as_str()).collect::<HashSet<_>>();
        let second_tail =
            second[20..].iter().map(|(_, term)| term.as_str()).collect::<HashSet<_>>();
        assert!(first_tail.contains("T10"));
        assert!(!first_tail.contains("T25"));
        assert!(second_tail.contains("T25"));
        assert_ne!(&first[20..], &second[20..]);
    }

    #[test]
    fn one_character_tickers_are_searched_only_in_product_form() {
        let mut tickers = vec!["F".to_owned()];
        tickers.extend((0..9).map(|index| format!("T{index:02}")));
        tickers.push("1".to_owned());
        let (searches, next_offset) = rotating_impostor_search_terms(&tickers, 0);

        assert!(searches.iter().all(|(_, search)| search.len() >= MIN_DEXSCREENER_SEARCH_BYTES));
        for ticker in ["F", "1"] {
            assert!(searches.contains(&(ticker.to_owned(), format!("{ticker}x"))));
            assert!(!searches.contains(&(ticker.to_owned(), ticker.to_owned())));
        }
        assert!(searches.contains(&("T00".to_owned(), "T00".to_owned())));
        assert_eq!(searches.len(), 20);
        assert_eq!(next_offset, 0);
    }

    fn ranked_candidate_pair(index: usize) -> DexPair {
        DexPair {
            chain_id: "base".to_owned(),
            dex_id: "test".to_owned(),
            url: None,
            pair_address: format!("pair-{index:03}"),
            labels: Vec::new(),
            base_token: DexToken {
                address: Some(format!("0x{index:040x}")),
                name: Some("NVIDIA xStock".to_owned()),
                symbol: Some("NVDAx".to_owned()),
            },
            quote_token: DexToken { address: None, name: None, symbol: None },
            price_usd: None,
            volume: Some(DexVolume { h24: Some(index as f64), h6: None, h1: None, m5: None }),
            price_change: None,
            txns: None,
            liquidity: None,
            source: MarketSource::Dexscreener,
        }
    }

    #[test]
    fn publisher_catalog_watch_prioritizes_ticker_then_caps_by_volume() {
        let candidates = (0..10)
            .map(|index| (Chain::Base, 0, ranked_candidate_pair(index)))
            .chain((10..70).map(|index| (Chain::Base, 1, ranked_candidate_pair(index))))
            .collect();
        let ranked = rank_impostor_candidates(candidates);

        assert_eq!(ranked.len(), MAX_IMPOSTOR_CANDIDATES_PER_REFRESH);
        assert_eq!(ranked.iter().filter(|(_, rank, _)| *rank == 0).count(), 10);
        assert!(ranked[..10].iter().all(|(_, rank, _)| *rank == 0));
        assert_eq!(ranked.first().unwrap().2.volume.as_ref().unwrap().h24, Some(9.0));
        assert_eq!(ranked[9].2.volume.as_ref().unwrap().h24, Some(0.0));
        assert_eq!(ranked[10].2.volume.as_ref().unwrap().h24, Some(69.0));
        assert_eq!(ranked.last().unwrap().2.volume.as_ref().unwrap().h24, Some(30.0));
    }

    #[test]
    fn publisher_catalog_watch_preserves_first_seen_when_rechecked() {
        let make_entry =
            |address: &str, first_seen_at: &str, last_seen_at: &str, volume: f64| ImpostorEntry {
                chain: "base".to_owned(),
                chain_label: "Base".to_owned(),
                ticker: "NVDA".to_owned(),
                publisher: "Robinhood".to_owned(),
                symbol: "NVDAx".to_owned(),
                name: "NVIDIA xStock".to_owned(),
                address: address.to_owned(),
                first_seen_at: first_seen_at.to_owned(),
                last_seen_at: last_seen_at.to_owned(),
                volume_24h_usd: Some(volume),
                source: MarketSource::Dexscreener,
                guard_url: format!("/guard/base/{address}"),
                reason: format!("catalog observation at {last_seen_at}"),
                reads: Vec::new(),
                on_chain_symbol: None,
                on_chain_name: None,
                publisher_catalog_snapshot_hash: None,
                evidence_truncated: false,
                guard_document: None,
            };
        let previous = make_entry("0xAbCd", "first", "old", 1.0);
        let current = make_entry("0xabcd", "later", "latest", 2.0);
        let (merged, evicted) = merge_impostor_entries(vec![previous], vec![current]);
        assert_eq!(evicted, 0);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].first_seen_at, "first");
        assert_eq!(merged[0].last_seen_at, "latest");
        assert_eq!(merged[0].volume_24h_usd, Some(2.0));
    }

    fn hours_ago(hours: i64) -> String {
        (Utc::now() - chrono::Duration::hours(hours)).to_rfc3339_opts(SecondsFormat::Secs, true)
    }

    fn sample_leaderboard(updated_at: &str, entries: usize) -> Leaderboard {
        Leaderboard {
            updated_at: updated_at.to_owned(),
            next_refresh_at: timestamp_after(DISCOVERY_REFRESH_SECS),
            restored: false,
            refreshing: false,
            empty_successful: false,
            source: LEADERBOARD_SOURCE.to_owned(),
            registry: RegistrySnapshot {
                entries: 42,
                issuers: 2,
                updated_at: updated_at.to_owned(),
                next_refresh_at: updated_at.to_owned(),
                restored: false,
                refreshing: false,
            },
            total: entries,
            entries: (0..entries)
                .map(|index| LeaderboardEntry {
                    rank: index + 1,
                    chain: "solana".to_owned(),
                    chain_label: "Solana".to_owned(),
                    dex: "raydium".to_owned(),
                    pool: format!("pool-{index}"),
                    base_symbol: "NVDAx".to_owned(),
                    quote_symbol: "USDC".to_owned(),
                    source: MarketSource::Dexscreener,
                    issuer: Some("Backed xStocks".to_owned()),
                    ticker: Some("NVDA".to_owned()),
                    issuer_on_base: Some(true),
                    verdict: "backed".to_owned(),
                    read_status: "checked".to_owned(),
                    read_reason: None,
                    price_usd: Some(1.5),
                    change_24h_pct: Some(-0.25),
                    volume_24h_usd: Some(1_000.0),
                    liquidity_usd: Some(2_000.0),
                    txns_24h: Some(17),
                    detail_url: format!("/validated/solana/pool-{index}"),
                    trade_url: format!("https://dexscreener.com/solana/pool-{index}"),
                    explorer_url: format!("https://solscan.io/account/pool-{index}"),
                    attestation_id: Some("att-1".to_owned()),
                    checked_at: Some(updated_at.to_owned()),
                })
                .collect(),
            impostors: ImpostorSnapshot::default(),
        }
    }

    fn sample_featured(updated_at: &str) -> FeaturedSnapshot {
        FeaturedSnapshot {
            updated_at: updated_at.to_owned(),
            pools: vec![FeaturedPool {
                chain: Chain::Solana,
                dex: "raydium".to_owned(),
                pool: "pool-1".to_owned(),
                base_symbol: "NVDAx".to_owned(),
                base_address: "base".to_owned(),
                quote_symbol: "USDC".to_owned(),
                quote_address: "quote".to_owned(),
                issuer: Some("Backed xStocks".to_owned()),
                ticker: Some("NVDA".to_owned()),
                verdict: "backed".to_owned(),
                quote_balance: Some("1,000".to_owned()),
                quote_share_of_supply: Some(0.5),
                volume_24h_usd: Some(1_000.0),
                liquidity_usd: Some(2_000.0),
                curated: true,
                note: None,
                updated_at: updated_at.to_owned(),
            }],
            next_refresh_at: timestamp_after(DISCOVERY_REFRESH_SECS),
            restored: false,
            refreshing: false,
            empty_successful: false,
        }
    }
    #[test]
    fn board_replacement_rejects_empty_or_less_than_half() {
        assert!(!safe_board_replacement(0, 0));
        assert!(!safe_board_replacement(10, 0));
        assert!(!safe_board_replacement(10, 4));
        assert!(safe_board_replacement(10, 5));
        assert!(safe_board_replacement(10, 11));
        let previous = sample_leaderboard(&hours_ago(1), 4);
        let mut incomplete = sample_leaderboard(&hours_ago(0), 2);
        for entry in &mut incomplete.entries {
            entry.source = MarketSource::Geckoterminal;
        }
        incomplete.entries[1].pool = "new-pool".to_owned();
        assert!(safe_board_replacement(previous.entries.len(), incomplete.entries.len()));
        assert!(!safe_leaderboard_replacement(&previous.entries, &incomplete.entries));

        let mut complete = sample_leaderboard(&hours_ago(0), 4);
        for entry in &mut complete.entries {
            entry.source = MarketSource::Geckoterminal;
        }
        assert!(safe_leaderboard_replacement(&previous.entries, &complete.entries));

        let dir = tempfile::tempdir().expect("temp dir");
        let previous = sample_leaderboard(&hours_ago(1), 10);
        save_leaderboard(dir.path(), &previous);
        save_leaderboard(dir.path(), &sample_leaderboard(&hours_ago(0), 0));
        assert_eq!(load_leaderboard(dir.path()).map(|board| board.entries.len()), Some(10));
        save_leaderboard(dir.path(), &sample_leaderboard(&hours_ago(0), 4));
        assert_eq!(load_leaderboard(dir.path()).map(|board| board.entries.len()), Some(10));
        save_leaderboard(dir.path(), &sample_leaderboard(&hours_ago(0), 5));
        assert_eq!(load_leaderboard(dir.path()).map(|board| board.entries.len()), Some(5));
    }

    #[test]
    fn empty_discovery_never_creates_a_persisted_board() {
        let dir = tempfile::tempdir().expect("temp dir");
        let empty_leaderboard = sample_leaderboard(&hours_ago(0), 0);
        save_leaderboard(dir.path(), &empty_leaderboard);
        assert!(!dir.path().join("leaderboard.json").exists());
        store_board(&dir.path().join("leaderboard.json"), &empty_leaderboard);
        assert!(load_leaderboard(dir.path()).is_none());

        let empty_featured = FeaturedSnapshot {
            updated_at: hours_ago(0),
            pools: Vec::new(),
            next_refresh_at: timestamp_after(DISCOVERY_REFRESH_SECS),
            restored: false,
            refreshing: false,
            empty_successful: false,
        };
        save_featured(dir.path(), &empty_featured);
        assert!(!dir.path().join("featured.json").exists());
        store_board(&dir.path().join("featured.json"), &empty_featured);
        assert!(load_featured_snapshot(dir.path()).is_none());
    }

    #[test]
    fn persisted_featured_board_rejects_empty_replacement() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut previous = sample_featured(&hours_ago(1));
        previous.pools = (0..4).map(|_| previous.pools[0].clone()).collect();
        save_featured(dir.path(), &previous);
        let empty = FeaturedSnapshot {
            updated_at: hours_ago(0),
            pools: Vec::new(),
            next_refresh_at: timestamp_after(DISCOVERY_REFRESH_SECS),
            restored: false,
            refreshing: false,
            empty_successful: false,
        };
        save_featured(dir.path(), &empty);
        assert_eq!(load_featured(dir.path()).map(|pools| pools.len()), Some(4));
    }

    #[test]
    fn persisted_leaderboard_round_trips_with_its_own_timestamp() {
        let dir = tempfile::tempdir().expect("temp dir");
        assert!(load_leaderboard(dir.path()).is_none(), "no file yet");
        let first = sample_leaderboard(&hours_ago(1), 2);
        save_leaderboard(dir.path(), &first);
        assert_eq!(load_leaderboard(dir.path()).as_ref(), Some(&first));
        let second = sample_leaderboard(&hours_ago(0), 50);
        save_leaderboard(dir.path(), &second);
        assert_eq!(load_leaderboard(dir.path()).as_ref(), Some(&second));
        assert!(!dir.path().join("leaderboard.json.tmp").exists(), "temp file renamed away");
    }

    fn watch_deny_guard(address: &str) -> crate::domain::guard::GuardDocument {
        use crate::domain::guard::{GuardReason, GuardVerdict, IdentityStatus};

        let mut guard = crate::domain::guard::signed_test_guard([19; 32]);
        guard.address = address.to_owned();
        guard.subject_address = Some(address.to_owned());
        guard.identity.publisher = Some("Robinhood".to_owned());
        guard.identity.ticker = Some("NVDA".to_owned());
        guard.identity.status = IdentityStatus::Mismatch;
        guard.identity.observed_symbol = Some("NVDAx".to_owned());
        guard.identity.observed_name = Some("NVIDIA xStock".to_owned());
        guard.verdict = GuardVerdict::Deny;
        guard.reasons = vec![GuardReason {
            code: "claims_unpublished_publisher_product".to_owned(),
            detail: "The matching product is absent from the publisher deployment catalog."
                .to_owned(),
        }];
        let mut reads = Vec::with_capacity(150);
        for (selector, result) in [("symbol()", "NVDAx"), ("name()", "NVIDIA xStock")] {
            reads.push(crate::domain::attestation::Read {
                method: "eth_call".to_owned(),
                params: serde_json::json!([address, selector]),
                result_hash: "a".repeat(64),
                raw_result: Some(serde_json::json!(result)),
                block: Some(42),
                slot: None,
            });
        }
        for index in 0..148 {
            reads.push(crate::domain::attestation::Read {
                method: "eth_call".to_owned(),
                params: serde_json::json!([address, format!("powerRead{index}")]),
                result_hash: "b".repeat(64),
                raw_result: Some(serde_json::json!({"value": "x".repeat(2048)})),
                block: Some(42),
                slot: None,
            });
        }
        guard.reads = reads;
        guard
    }

    #[test]
    fn large_guard_deny_keeps_compact_identity_evidence_and_is_counted() {
        let address = "0x0000000000000000000000000000000000000001";
        let observation = catalog_absent_entry(
            Chain::Base,
            "2026-10-07T12:00:00Z",
            "NVDAx".to_owned(),
            "NVIDIA xStock".to_owned(),
            Some(12.5),
            MarketSource::Dexscreener,
            Some("c".repeat(64)),
            watch_deny_guard(address),
        )
        .expect("well-formed Guard observation")
        .expect("catalog-deny observation");
        assert_eq!(observation.reads.len(), 2);
        assert!(observation.evidence_truncated);
        assert_eq!(
            observation.publisher_catalog_snapshot_hash.as_deref(),
            Some("c".repeat(64).as_str())
        );
        assert!(observation.reads.iter().all(|read| {
            read.params[0] == address
                && matches!(read.params[1].as_str(), Some("symbol()" | "name()"))
                && read
                    .raw_result
                    .as_ref()
                    .is_some_and(|result| json_value_len(result) <= MAX_IMPOSTOR_READ_RESULT_BYTES)
        }));

        let directory = tempfile::tempdir().expect("temporary watch cache");
        let mut board = sample_leaderboard("2026-10-07T12:00:00Z", 1);
        board.impostors.entries.push(observation);
        save_leaderboard(directory.path(), &board);
        let restored = load_leaderboard(directory.path()).expect("persisted watch row");
        assert_eq!(restored.impostors.entries.len(), 1);
        assert_eq!(restored.impostors.rejected_oversize_entries, 0);
        assert_eq!(restored.impostors.entries[0].reads.len(), 2);
        assert!(restored.impostors.entries[0].evidence_truncated);
    }

    /// Guard document whose deny reason comes from the real identity and verdict
    /// producers: a Robinhood Chain clone of a product published on 12 networks.
    fn real_catalog_absence_guard(address: &str) -> crate::domain::guard::GuardDocument {
        use crate::domain::{
            guard::{SourceStatus, evaluate, identify_with_contract_metadata, issuer_metadata_key},
            pool::TokenMeta,
            registry::OfficialDeployment,
        };

        let official = "0x0000000000000000000000000000000000000101";
        let product = |address: &str| TokenMeta {
            address: address.to_owned(),
            symbol: Some("NVDAx".to_owned()),
            name: Some("NVIDIA xStock".to_owned()),
            decimals: Some(18),
            total_supply: None,
        };
        let publisher_entry = Entry {
            issuer: "Backed xStocks".to_owned(),
            ticker: "NVDA".to_owned(),
            name: "NVIDIA xStock".to_owned(),
            chain: Chain::Ethereum,
            contract: official.to_owned(),
            decimals: Some(18),
            source: "xstocks-api".to_owned(),
            source_url: "https://example.invalid/xstocks".to_owned(),
            last_checked: now_rfc3339(),
            removed_at: None,
            stale_since: None,
            official_deployments: [
                "Arbitrum", "BSC", "Ethereum", "HyperEVM", "Ink", "Mantle", "Monad", "Optimism",
                "Solana", "Ton", "Tron", "XLayer",
            ]
            .into_iter()
            .enumerate()
            .map(|(index, network)| OfficialDeployment {
                network: network.to_owned(),
                address: format!("0x{:040x}", 0x200 + index),
                wrapper_address: None,
                wrapper_address_v2: None,
            })
            .collect(),
        };
        let publisher_metadata =
            HashMap::from([(issuer_metadata_key(Chain::Ethereum, official), product(official))]);
        let identity = identify_with_contract_metadata(
            Chain::RobinhoodChain,
            address,
            Some(&product(address)),
            std::slice::from_ref(&publisher_entry),
            &publisher_metadata,
        );
        let (verdict, reasons) = evaluate(&identity, None, None, SourceStatus::Verified);
        let mut guard = watch_deny_guard(address);
        guard.chain = Chain::RobinhoodChain;
        guard.identity = identity;
        guard.verdict = verdict;
        guard.reasons = reasons;
        guard
    }

    #[test]
    fn long_real_guard_reason_and_text_fields_are_truncated_without_rejection() {
        let address = "0x0000000000000000000000000000000000000001";
        let guard = real_catalog_absence_guard(address);
        assert_eq!(guard.verdict, crate::domain::guard::GuardVerdict::Deny);
        assert_eq!(guard.reasons[0].code, "claims_unpublished_publisher_product");
        assert!(guard.reasons[0].detail.contains("XLayer"));
        assert!(guard.reasons[0].detail.len() > MAX_IMPOSTOR_REASON_BYTES);
        let reason_observation = catalog_absent_entry(
            Chain::RobinhoodChain,
            "2026-10-07T12:00:00Z",
            "NVDAx".to_owned(),
            "NVIDIA xStock".to_owned(),
            Some(12.5),
            MarketSource::Dexscreener,
            Some("c".repeat(64)),
            guard,
        )
        .expect("valid token address and registry hash")
        .expect("catalog-deny observation");

        assert_eq!(reason_observation.reason.len(), MAX_IMPOSTOR_REASON_BYTES);
        assert!(reason_observation.reason.starts_with(
            "QED Guard found an exact on-chain symbol/name match for NVIDIA xStock (NVDA)"
        ));
        assert!(reason_observation.evidence_truncated);

        let long_symbol = "NVDAx".repeat(20);
        let long_name = "NVIDIA • xStock ".repeat(8);
        let mut label_guard = watch_deny_guard(address);
        label_guard.reads.truncate(2);
        label_guard.identity.observed_symbol = Some(long_symbol.clone());
        label_guard.identity.observed_name = Some(long_name.clone());
        label_guard.reads[0].raw_result = Some(serde_json::json!(long_symbol));
        label_guard.reads[1].raw_result = Some(serde_json::json!(long_name));
        let label_observation = catalog_absent_entry(
            Chain::Base,
            "2026-10-07T12:00:00Z",
            "NVDAx".repeat(20),
            "NVIDIA • xStock ".repeat(8),
            Some(12.5),
            MarketSource::Dexscreener,
            Some("c".repeat(64)),
            label_guard,
        )
        .expect("valid token address and registry hash")
        .expect("catalog-deny observation");
        assert!(label_observation.symbol.len() <= MAX_IMPOSTOR_LABEL_BYTES);
        assert!(label_observation.name.len() <= MAX_IMPOSTOR_LABEL_BYTES);
        assert!(
            label_observation.on_chain_symbol.as_ref().unwrap().len() <= MAX_IMPOSTOR_LABEL_BYTES
        );
        assert!(
            label_observation.on_chain_name.as_ref().unwrap().len() <= MAX_IMPOSTOR_LABEL_BYTES
        );
        assert!(label_observation.evidence_truncated);

        let mut observation = reason_observation;
        observation.ticker = "N".repeat(80);
        observation.publisher = "P".repeat(80);
        observation.symbol = "S".repeat(80);
        observation.name = "N".repeat(80);
        observation.on_chain_symbol = Some("O".repeat(80));
        observation.on_chain_name = Some("C".repeat(80));
        observation.first_seen_at.push_str(&"x".repeat(80));
        observation.last_seen_at.push_str(&"x".repeat(80));
        let mut invalid_chain = observation.clone();
        invalid_chain.chain = "not-supported".to_owned();
        let mut invalid_address = observation.clone();
        invalid_address.address = "not-an-address".to_owned();
        let mut invalid_hash = observation.clone();
        invalid_hash.publisher_catalog_snapshot_hash = Some("not-a-hash".to_owned());
        let unsupported = UnsupportedImpostorCandidate {
            dex_chain_id: "arc".to_owned(),
            ticker: "T".repeat(80),
            publisher: "P".repeat(80),
            symbol: "S".repeat(80),
            name: "NVIDIA • xStock ".repeat(8),
            address: "0x0000000000000000000000000000000000000002".to_owned(),
            first_seen_at: format!("2026-10-07T12:00:00Z{}", "x".repeat(80)),
            last_seen_at: "2026-10-07T12:00:00Z".to_owned(),
            volume_24h_usd: Some(8.0),
            source: MarketSource::Dexscreener,
            evidence_truncated: false,
        };
        let mut invalid_unsupported_chain = unsupported.clone();
        invalid_unsupported_chain.dex_chain_id = "arc chain".to_owned();
        let mut invalid_unsupported_address = unsupported.clone();
        invalid_unsupported_address.address = "0x 0002".to_owned();
        let mut board = sample_leaderboard("2026-10-07T12:00:00Z", 1);
        board.impostors.entries.extend([observation, invalid_chain, invalid_address, invalid_hash]);
        board.impostors.unsupported_candidates.extend([
            unsupported,
            invalid_unsupported_chain,
            invalid_unsupported_address,
        ]);

        let directory = tempfile::tempdir().expect("temporary bounded watch cache");
        save_leaderboard(directory.path(), &board);
        let restored = load_leaderboard(directory.path()).expect("restore compact watch rows");
        assert_eq!(restored.impostors.entries.len(), 1);
        assert_eq!(restored.impostors.rejected_oversize_entries, 3);
        let stored = &restored.impostors.entries[0];
        assert!(stored.evidence_truncated);
        assert_eq!(stored.reason.len(), MAX_IMPOSTOR_REASON_BYTES);
        assert!(stored.ticker.len() <= MAX_IMPOSTOR_LABEL_BYTES);
        assert!(stored.publisher.len() <= MAX_IMPOSTOR_LABEL_BYTES);
        assert!(stored.symbol.len() <= MAX_IMPOSTOR_LABEL_BYTES);
        assert!(stored.name.len() <= MAX_IMPOSTOR_LABEL_BYTES);
        assert!(stored.on_chain_symbol.as_ref().unwrap().len() <= MAX_IMPOSTOR_LABEL_BYTES);
        assert!(stored.on_chain_name.as_ref().unwrap().len() <= MAX_IMPOSTOR_LABEL_BYTES);
        assert!(stored.first_seen_at.len() <= MAX_IMPOSTOR_TIMESTAMP_BYTES);
        assert!(stored.last_seen_at.len() <= MAX_IMPOSTOR_TIMESTAMP_BYTES);
        assert_eq!(
            stored.publisher_catalog_snapshot_hash.as_deref(),
            Some("c".repeat(64).as_str())
        );

        assert_eq!(restored.impostors.unsupported_candidates.len(), 1);
        assert_eq!(restored.impostors.unsupported_seen, 1);
        assert_eq!(restored.impostors.rejected_oversize_unsupported_candidates, 2);
        let candidate = &restored.impostors.unsupported_candidates[0];
        assert!(candidate.evidence_truncated);
        assert_eq!(candidate.address, "0x0000000000000000000000000000000000000002");
        for text in [&candidate.ticker, &candidate.publisher, &candidate.symbol, &candidate.name] {
            assert!(text.len() <= MAX_IMPOSTOR_LABEL_BYTES);
        }
        assert!(candidate.first_seen_at.len() <= MAX_IMPOSTOR_TIMESTAMP_BYTES);
    }

    #[tokio::test]
    async fn real_catalog_absence_observation_is_stored_and_counted_in_signed_stats() {
        let address = format!("0x9d70{:036x}", 1);
        let scanned_at = now_rfc3339();
        let observation = catalog_absent_entry(
            Chain::RobinhoodChain,
            &scanned_at,
            "NVDAx".to_owned(),
            "NVIDIA xStock".to_owned(),
            Some(12.5),
            MarketSource::Geckoterminal,
            Some("c".repeat(64)),
            real_catalog_absence_guard(&address),
        )
        .expect("valid token address and registry hash")
        .expect("catalog-deny observation");
        let directory = tempfile::tempdir().expect("temporary watch cache");
        let mut board = sample_leaderboard(&scanned_at, 1);
        board.entries[0].source = MarketSource::Geckoterminal;
        board.entries[0].trade_url = "https://www.geckoterminal.com/solana/pools/pool-0".to_owned();
        board.impostors.scanned_at.clone_from(&scanned_at);
        board.impostors.entries.push(observation);
        save_leaderboard(directory.path(), &board);
        let restored = load_leaderboard(directory.path()).expect("restore watch row");

        let state = AppState::for_tests(Vec::new(), Vec::new(), true);
        *state.leaderboard.write().await = restored;
        crate::adapters::web::refresh_stats_snapshot(&state).await.expect("signed stats snapshot");
        let snapshot = state.stats_snapshot.read().await.clone().expect("prepared stats snapshot");
        let document: serde_json::Value =
            serde_json::from_slice(&snapshot.json).expect("signed stats JSON");
        let watch = &document["stats"]["publisher_catalog_watch"];
        assert_eq!(watch["currently_flagged_last_7_days"], 1);
        assert_eq!(watch["first_flagged_this_week"], 1);
        assert_eq!(watch["rejected_oversize_entries"], 0);
        let stored = &document["reducer_inputs"]["impostor_watch"]["entries"][0];
        assert_eq!(stored["chain"], "robinhood");
        assert_eq!(stored["source"], "geckoterminal");
        assert_eq!(document["reducer_inputs"]["leaderboard"][0]["source"], "geckoterminal");
        assert_eq!(stored["address"], address.as_str());
        assert_eq!(stored["reason"].as_str().map(str::len), Some(MAX_IMPOSTOR_REASON_BYTES));
        assert_eq!(stored["evidence_truncated"], true);
        assert!(String::from_utf8_lossy(&snapshot.html).contains(address.as_str()));
    }

    #[tokio::test]
    async fn impostor_search_outage_keeps_last_results_and_one_failed_query_does_not_abort() {
        let no_pairs =
            (axum::http::StatusCode::OK, r#"{"schemaVersion":"1.0.0","pairs":[]}"#.to_owned());
        let failure = (axum::http::StatusCode::SERVICE_UNAVAILABLE, "unavailable".to_owned());
        let responses = Arc::new(std::sync::Mutex::new(HashMap::from([
            ("NVDA".to_owned(), no_pairs.clone()),
            ("NVDAx".to_owned(), no_pairs.clone()),
        ])));
        let set_response = |query: &str, response: (axum::http::StatusCode, String)| {
            responses.lock().expect("fixture responses").insert(query.to_owned(), response);
        };
        let gecko_requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let app = axum::Router::new()
            .route(
                "/latest/dex/search",
                axum::routing::get({
                    let responses = Arc::clone(&responses);
                    move |axum::extract::Query(query): axum::extract::Query<
                        HashMap<String, String>,
                    >| {
                        let response = responses
                            .lock()
                            .expect("fixture responses")
                            .get(query.get("q").map(String::as_str).unwrap_or_default())
                            .cloned()
                            .unwrap_or((axum::http::StatusCode::NOT_FOUND, String::new()));
                        async move { response }
                    }
                }),
            )
            .route(
                "/api/v2/search/pools",
                axum::routing::get({
                    let gecko_requests = Arc::clone(&gecko_requests);
                    move || {
                        gecko_requests.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        async { (axum::http::StatusCode::OK, r#"{"data":[]}"#.to_owned()) }
                    }
                }),
            );
        let listener =
            tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("fixture listener");
        let base_url = format!("http://{}", listener.local_addr().expect("fixture address"));
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("fixture search server");
        });
        let state = AppState::for_tests(
            vec![entry(Chain::Base, "0x0000000000000000000000000000000000000101", "NVDA")],
            Vec::new(),
            true,
        );
        let client = MarketDataClient::with_clients(
            DexScreenerClient::with_base_url(state.http.clone(), base_url.clone()),
            GeckoTerminalClient::with_base_url(state.http.clone(), format!("{base_url}/api/v2")),
            true,
        );
        let registry = state.registry.read().await.clone();
        let hash = Some("c".repeat(64));
        let stats_headline = |impostors: ImpostorSnapshot| {
            let state = state.clone();
            async move {
                state.leaderboard.write().await.impostors = impostors;
                crate::adapters::web::refresh_stats_snapshot(&state).await.expect("stats");
                let snapshot = state.stats_snapshot.read().await.clone().expect("stats snapshot");
                serde_json::from_slice::<serde_json::Value>(&snapshot.json).expect("stats JSON")
                    ["stats"]
                    .clone()
            }
        };

        let never_scanned =
            refresh_impostor_watch(&state, &client, &registry, hash.clone(), Default::default())
                .await;
        let first_outage = never_scanned.source_unavailable_since.clone().expect("outage start");
        assert!(never_scanned.scanned_at.is_empty());
        let stats = stats_headline(never_scanned).await;
        let headline = stats["headline"].as_str().expect("headline");
        assert!(headline.contains(&format!(
            "Impostor search source unavailable since {first_outage}; no successful search yet."
        )));
        assert!(!headline.contains("catalog observations are currently flagged"));

        let last_success = (chrono::Utc::now() - chrono::Duration::hours(1))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let address = format!("0x9d70{:036x}", 2);
        let observation = catalog_absent_entry(
            Chain::RobinhoodChain,
            &last_success,
            "NVDAx".to_owned(),
            "NVIDIA xStock".to_owned(),
            Some(12.5),
            MarketSource::Dexscreener,
            hash.clone(),
            real_catalog_absence_guard(&address),
        )
        .expect("valid observation")
        .expect("catalog-deny observation");
        let last_good = ImpostorSnapshot {
            scanned_at: last_success.clone(),
            next_ticker_offset: 0,
            entries: vec![observation],
            ..ImpostorSnapshot::default()
        };

        let empty =
            refresh_impostor_watch(&state, &client, &registry, hash.clone(), last_good.clone())
                .await;
        let since = empty.source_unavailable_since.clone().expect("empty search is an outage");
        assert_eq!(ImpostorSnapshot { source_unavailable_since: None, ..empty.clone() }, last_good);
        assert_eq!(gecko_requests.load(std::sync::atomic::Ordering::Relaxed), 0);

        set_response("NVDA", failure.clone());
        set_response("NVDAx", no_pairs.clone());
        let failed =
            refresh_impostor_watch(&state, &client, &registry, hash.clone(), empty.clone()).await;
        assert_eq!(failed, empty, "an all-failing source keeps results and the first outage time");
        assert_eq!(
            gecko_requests.load(std::sync::atomic::Ordering::Relaxed),
            GECKOTERMINAL_WATCH_NETWORKS.len()
        );

        let stats = stats_headline(failed.clone()).await;
        let watch = &stats["publisher_catalog_watch"];
        assert_eq!(watch["source_unavailable_since"], since.as_str());
        assert_eq!(watch["last_scanned_at"], last_success.as_str());
        assert_eq!(watch["currently_flagged_last_7_days"], 1);
        assert!(stats["headline"].as_str().expect("headline").contains(&format!(
            "Impostor search source unavailable since {since}; from the last successful search at {last_success}, 1 catalog observations are currently flagged"
        )));

        set_response("NVDA", no_pairs);
        set_response(
            "NVDAx",
            (
                axum::http::StatusCode::OK,
                serde_json::json!({"schemaVersion": "1.0.0", "pairs": [{
                    "chainId": "arc",
                    "dexId": "uniswap",
                    "pairAddress": "0x00000000000000000000000000000000000000a0",
                    "baseToken": {
                        "address": "0x00000000000000000000000000000000000000a1",
                        "name": "NVIDIA xStock",
                        "symbol": "NVDAx"
                    },
                    "quoteToken": {"address": "0x00000000000000000000000000000000000000a2", "symbol": "USDC"}
                }]})
                .to_string(),
            ),
        );
        let recovered = refresh_impostor_watch(&state, &client, &registry, hash, failed).await;
        assert_eq!(recovered.source_unavailable_since, None);
        assert_ne!(recovered.scanned_at, last_success);
        assert_eq!(recovered.entries.len(), 1);
        assert_eq!(recovered.unsupported_candidates.len(), 1);
        assert_eq!(recovered.unsupported_candidates[0].dex_chain_id, "arc");
        assert_eq!(recovered.unsupported_candidates[0].source, MarketSource::Dexscreener);
        assert_eq!(gecko_requests.load(std::sync::atomic::Ordering::Relaxed), 7);
        server.abort();
    }

    #[test]
    fn solana_watch_compaction_keeps_mint_metadata_reads_only() {
        let address = bs58::encode([23u8; 32]).into_string();
        let reads = vec![
            crate::domain::attestation::Read {
                method: "getAccountInfo".to_owned(),
                params: serde_json::json!([address, { "encoding": "base64", "commitment": "confirmed" }]),
                result_hash: "d".repeat(64),
                raw_result: Some(serde_json::json!({
                    "context": {"slot": 44},
                    "value": {"data": ["x".repeat(2048), "base64"]}
                })),
                block: None,
                slot: Some(44),
            },
            crate::domain::attestation::Read {
                method: "getProgramAccounts".to_owned(),
                params: serde_json::json!([
                    METAPLEX_METADATA_PROGRAM,
                    {"filters": [{"memcmp": {"offset": 33, "bytes": address}}]}
                ]),
                result_hash: "e".repeat(64),
                raw_result: None,
                block: None,
                slot: Some(45),
            },
            crate::domain::attestation::Read {
                method: "getAccountInfo".to_owned(),
                params: serde_json::json!(["another-mint", {}]),
                result_hash: "f".repeat(64),
                raw_result: None,
                block: None,
                slot: Some(46),
            },
            crate::domain::attestation::Read {
                method: "getProgramAccounts".to_owned(),
                params: serde_json::json!([
                    "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA",
                    {"filters": [{"memcmp": {"offset": 33, "bytes": address}}]}
                ]),
                result_hash: "1".repeat(64),
                raw_result: None,
                block: None,
                slot: Some(47),
            },
        ];

        let (compact, truncated) = compact_watch_reads(&reads, Chain::Solana, &address);
        assert_eq!(compact.len(), 2);
        assert!(truncated);
        assert_eq!(compact[0].slot, Some(44));
        assert_eq!(compact[1].slot, Some(45));
        assert!(
            compact[0]
                .raw_result
                .as_ref()
                .is_some_and(|result| { json_value_len(result) <= MAX_IMPOSTOR_READ_RESULT_BYTES })
        );
        assert_eq!(compact[1].params[0], METAPLEX_METADATA_PROGRAM);
    }

    #[test]
    fn legacy_full_guard_observation_restores_as_compact_evidence() {
        let address = "0x0000000000000000000000000000000000000001";
        let mut legacy_entry = serde_json::json!({
            "chain": "base",
            "chain_label": "Base",
            "ticker": "NVDA",
            "publisher": "Robinhood",
            "symbol": "NVDAx",
            "name": "NVIDIA xStock",
            "address": address,
            "first_seen_at": "2026-10-07T12:00:00Z",
            "last_seen_at": "2026-10-07T12:00:00Z",
            "volume_24h_usd": 12.5,
            "guard_url": format!("/guard/base/{address}"),
            "reason": "Publisher deployment not listed.",
            "reads": [],
            "on_chain_symbol": null,
            "on_chain_name": null
        });
        legacy_entry["guard_document"] =
            serde_json::to_value(watch_deny_guard(address)).expect("legacy Guard JSON");
        let mut board = serde_json::to_value(sample_leaderboard("2026-10-07T12:00:00Z", 1))
            .expect("legacy board JSON");
        board["impostors"]["entries"] = serde_json::json!([legacy_entry]);
        let directory = tempfile::tempdir().expect("temporary legacy board");
        std::fs::write(
            directory.path().join(LEADERBOARD_CACHE_FILE),
            serde_json::to_vec(&board).expect("legacy board bytes"),
        )
        .expect("write legacy board");

        let restored = load_leaderboard(directory.path()).expect("restore legacy board");
        assert_eq!(restored.impostors.entries.len(), 1);
        let observation = &restored.impostors.entries[0];
        assert_eq!(observation.reads.len(), 2);
        assert_eq!(observation.on_chain_symbol.as_deref(), Some("NVDAx"));
        assert_eq!(observation.on_chain_name.as_deref(), Some("NVIDIA xStock"));
        assert!(observation.publisher_catalog_snapshot_hash.is_none());
        assert!(observation.evidence_truncated);
        assert!(observation.guard_document.is_none());
        assert_eq!(restored.impostors.rejected_oversize_entries, 0);
    }

    #[test]
    fn persisted_leaderboard_retains_catalog_watch_history() {
        let dir = tempfile::tempdir().expect("temp dir");
        let scanned_at = "2026-10-07T12:00:00Z".to_owned();
        let mut board = sample_leaderboard(&hours_ago(0), 2);
        board.impostors.scanned_at = scanned_at.clone();
        board.impostors.unsupported_seen = 1;
        board.impostors.entries.push(ImpostorEntry {
            chain: "robinhood".to_owned(),
            chain_label: "Robinhood Chain".to_owned(),
            ticker: "NVDA".to_owned(),
            publisher: "Robinhood".to_owned(),
            symbol: "NVDAx".to_owned(),
            name: "NVIDIA xStock".to_owned(),
            address: "0x0000000000000000000000000000000000000001".to_owned(),
            first_seen_at: scanned_at.clone(),
            last_seen_at: scanned_at.clone(),
            volume_24h_usd: Some(12.5),
            source: MarketSource::Dexscreener,
            guard_url: "/guard/robinhood/0x0000000000000000000000000000000000000001".to_owned(),
            reason: "Publisher deployment not listed.".to_owned(),
            reads: Vec::new(),
            on_chain_symbol: Some("NVDAx".to_owned()),
            on_chain_name: Some("NVIDIA xStock".to_owned()),
            publisher_catalog_snapshot_hash: Some("a".repeat(64)),
            evidence_truncated: false,
            guard_document: None,
        });
        board.impostors.unsupported_candidates.push(UnsupportedImpostorCandidate {
            dex_chain_id: "arc".to_owned(),
            ticker: "NVDA".to_owned(),
            publisher: "Robinhood".to_owned(),
            symbol: "NVDAx".to_owned(),
            name: "NVIDIA xStock".to_owned(),
            address: "0x0000000000000000000000000000000000000002".to_owned(),
            first_seen_at: scanned_at.clone(),
            last_seen_at: scanned_at,
            volume_24h_usd: Some(8.0),
            source: MarketSource::Dexscreener,
            evidence_truncated: false,
        });

        save_leaderboard(dir.path(), &board);
        assert_eq!(load_leaderboard(dir.path()), Some(board));
    }

    #[test]
    fn legacy_market_rows_default_to_dexscreener_source() {
        let scanned_at = now_rfc3339();
        let address = "0x9d70000000000000000000000000000000000001";
        let observation = catalog_absent_entry(
            Chain::Base,
            &scanned_at,
            "NVDAx".to_owned(),
            "NVIDIA xStock".to_owned(),
            Some(12.5),
            MarketSource::Geckoterminal,
            Some("e".repeat(64)),
            real_catalog_absence_guard(address),
        )
        .expect("valid observation")
        .expect("catalog-deny observation");
        let mut board = sample_leaderboard(&scanned_at, 1);
        board.impostors.entries.push(observation);
        board.impostors.unsupported_candidates.push(UnsupportedImpostorCandidate {
            dex_chain_id: "arc".to_owned(),
            ticker: "NVDA".to_owned(),
            publisher: "Robinhood".to_owned(),
            symbol: "NVDAx".to_owned(),
            name: "NVIDIA xStock".to_owned(),
            address: "0x0000000000000000000000000000000000000002".to_owned(),
            first_seen_at: scanned_at.clone(),
            last_seen_at: scanned_at,
            volume_24h_usd: Some(8.0),
            source: MarketSource::Geckoterminal,
            evidence_truncated: false,
        });
        let mut stored = serde_json::to_value(board).expect("stored board JSON");
        stored["entries"][0].as_object_mut().unwrap().remove("source");
        stored["impostors"]["entries"][0].as_object_mut().unwrap().remove("source");
        stored["impostors"]["unsupported_candidates"][0].as_object_mut().unwrap().remove("source");
        stored["impostors"].as_object_mut().unwrap().remove("official_on_unsupported_chain");

        let restored: Leaderboard =
            serde_json::from_value(stored).expect("legacy source fields default");
        assert_eq!(restored.entries[0].source, MarketSource::Dexscreener);
        assert_eq!(restored.impostors.entries[0].source, MarketSource::Dexscreener);
        assert_eq!(restored.impostors.unsupported_candidates[0].source, MarketSource::Dexscreener);
        assert_eq!(restored.impostors.official_on_unsupported_chain, 0);
    }

    #[test]
    fn persisted_leaderboard_stale_file_is_restored() {
        let dir = tempfile::tempdir().expect("temp dir");
        let stale = sample_leaderboard(&hours_ago(25), 50);
        save_leaderboard(dir.path(), &stale);
        assert_eq!(load_leaderboard(dir.path()).as_ref(), Some(&stale));
    }

    #[test]
    fn persisted_featured_round_trips() {
        let dir = tempfile::tempdir().expect("temp dir");
        assert!(load_featured(dir.path()).is_none(), "no file yet");
        let snapshot = sample_featured(&hours_ago(2));
        save_featured(dir.path(), &snapshot);
        assert_eq!(load_featured(dir.path()), Some(snapshot.pools));
    }

    #[test]
    fn persisted_featured_stale_file_is_restored() {
        let dir = tempfile::tempdir().expect("temp dir");
        let stale = sample_featured(&hours_ago(30));
        save_featured(dir.path(), &stale);
        assert_eq!(load_featured(dir.path()), Some(stale.pools));
    }
    #[test]
    fn price_merge_replaces_matching_pool_and_keeps_unseen_points() {
        let previous = vec![
            PricePoint {
                chain: "solana".to_owned(),
                pool: "old".to_owned(),
                price_usd: Some(1.0),
                change_24h_pct: Some(1.0),
                volume_24h_usd: Some(10.0),
                liquidity_usd: Some(20.0),
                source: MarketSource::Dexscreener,
            },
            PricePoint {
                chain: "base".to_owned(),
                pool: "untouched".to_owned(),
                price_usd: Some(2.0),
                change_24h_pct: None,
                volume_24h_usd: None,
                liquidity_usd: None,
                source: MarketSource::Dexscreener,
            },
        ];
        let mut updated = leaderboard_pair("old", Chain::Solana, Some(42.0));
        updated.pair_address = "old".to_owned();
        let merged = merge_price_points(&previous, [updated]);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].price_usd, Some(12.5));
        assert_eq!(merged[0].volume_24h_usd, Some(42.0));
        assert_eq!(merged[1].pool, "untouched");
    }

    #[test]
    fn dex_requests_are_batched_in_groups_of_thirty() {
        assert_eq!(0usize.div_ceil(DEXSCREENER_BATCH_SIZE), 0);
        assert_eq!(1usize.div_ceil(DEXSCREENER_BATCH_SIZE), 1);
        assert_eq!(30usize.div_ceil(DEXSCREENER_BATCH_SIZE), 1);
        assert_eq!(31usize.div_ceil(DEXSCREENER_BATCH_SIZE), 2);
        assert_eq!(300usize.div_ceil(DEXSCREENER_BATCH_SIZE), 10);
    }
}
