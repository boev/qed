use crate::chain::Chain;
use crate::check::{self, CheckResult, Verdict};
use crate::pool::PoolInfo;
use crate::registry::{self, Registry};
use crate::state::AppState;
#[cfg(feature = "s3")]
use aws_sdk_s3::primitives::ByteStream;
#[cfg(feature = "s3")]
use aws_sdk_s3::Client as S3Client;
#[cfg(feature = "s3")]
use tokio::io::{AsyncRead, AsyncReadExt};

const DURABLE_BOARD_PREFIX: &str = "discovery/";
#[cfg(feature = "s3")]
const MAX_DURABLE_BOARD_BYTES: usize = 2 * 1024 * 1024;

#[cfg(feature = "s3")]
async fn read_bounded_body<R>(
    reader: R,
    content_length: Option<i64>,
) -> std::io::Result<Vec<u8>>
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
            return Self {
                client: Some(S3Client::new(&config)),
                bucket: Some(bucket),
            };
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
            .filter(|board: &Leaderboard| {
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
            && !safe_board_replacement(previous.entries.len(), board.entries.len())
        {
            warn!(
                previous = previous.entries.len(),
                next = board.entries.len(),
                "refusing to replace durable leaderboard with a much smaller board"
            );
            return;
        }
        self.write(
            &format!("{DURABLE_BOARD_PREFIX}{LEADERBOARD_CACHE_FILE}"),
            board,
        )
        .await;
    }

    pub async fn load_featured(&self) -> Option<FeaturedSnapshot> {
        self.read(&format!("{DURABLE_BOARD_PREFIX}{FEATURED_CACHE_FILE}"))
            .await
            .filter(|snapshot: &FeaturedSnapshot| {
                !snapshot.pools.is_empty() && snapshot.pools.len() <= MAX_FEATURED_POOLS
            })
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
        self.write(&format!("{DURABLE_BOARD_PREFIX}{FEATURED_CACHE_FILE}"), snapshot)
            .await;
    }
}

use chrono::{DateTime, SecondsFormat, Utc};
use reqwest::StatusCode;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
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
pub const MIN_LIQUIDITY_USD: f64 = 1_000.0;
pub const MAX_FEATURED_POOLS: usize = 12;
pub const MAX_LEADERBOARD_CANDIDATES: usize = 300;
pub const LEADERBOARD_PAGE_SIZE: usize = 50;
pub const DISCOVERY_REFRESH_SECS: u64 = 6 * 60 * 60;
pub const PRICE_REFRESH_SECS: u64 = 5 * 60;
pub const PRICE_RETRY_SECS: u64 = 60;
pub const REGISTRY_REFRESH_SECS: u64 = 60 * 60;
const LEADERBOARD_SOURCE: &str = "DexScreener + on-chain reads";
const DEX_BUDGET_WINDOW: Duration = Duration::from_secs(60);
const DEX_GLOBAL_REQUESTS_PER_MINUTE: usize = 240;
const DEX_ENDPOINT_BACKOFF: Duration = Duration::from_secs(120);

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
pub struct LeaderboardEntry {
    pub rank: usize,
    pub chain: String,
    pub chain_label: String,
    pub dex: String,
    pub pool: String,
    pub base_symbol: String,
    pub quote_symbol: String,
    pub issuer: Option<String>,
    pub ticker: Option<String>,
    pub verdict: String,
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
    (window - age)
        .to_std()
        .ok()
        .map(|delay| delay.min(Duration::from_secs(DISCOVERY_REFRESH_SECS)))
}

/// A complete restored board needs both persisted views. Run when the first
/// one reaches its six-hour age so neither view exceeds the verification
/// interval.
pub fn first_discovery_delay(
    leaderboard: Option<Duration>,
    featured: Option<Duration>,
) -> Option<Duration> {
    leaderboard.zip(featured).map(|(leaderboard, featured)| leaderboard.min(featured))
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

pub fn save_leaderboard(data_dir: &Path, leaderboard: &Leaderboard) {
    let path = data_dir.join(LEADERBOARD_CACHE_FILE);
    if !safe_board_replacement(0, leaderboard.entries.len()) {
        warn!("refusing to persist an empty leaderboard");
        return;
    }
    if let Some(previous) = read_board::<Leaderboard>(&path)
        && !safe_board_replacement(previous.entries.len(), leaderboard.entries.len())
    {
        warn!(
            previous = previous.entries.len(),
            next = leaderboard.entries.len(),
            "refusing to replace persisted leaderboard with a much smaller board"
        );
        return;
    }
    store_board(&path, leaderboard);
}

pub fn load_leaderboard(data_dir: &Path) -> Option<Leaderboard> {
    read_board::<Leaderboard>(&data_dir.join(LEADERBOARD_CACHE_FILE))
        .filter(|leaderboard| !leaderboard.entries.is_empty())
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
}

impl DexScreenerClient {
    pub fn new(http: reqwest::Client) -> Self {
        Self { http }
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
            let url = format!("{DEXSCREENER_API}/tokens/v1/{chain_id}/{addresses}");
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
            let url = format!("{DEXSCREENER_API}/latest/dex/pairs/{chain_id}/{addresses}");
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
        let url = format!("{DEXSCREENER_API}/latest/dex/pairs/{chain_id}/{pool_address}");
        let response: DexPairResponse = self.get_json(&url, DexEndpoint::Pairs).await?;
        Ok(response.pair.or_else(|| response.pairs.and_then(|mut pairs| pairs.pop())))
    }

    #[allow(dead_code)]
    pub async fn search(&self, query: &str) -> Result<serde_json::Value, DiscoveryError> {
        let url = format!("{DEXSCREENER_API}/latest/dex/search?q={query}");
        self.get_json(&url, DexEndpoint::Search).await
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
        let body = crate::net::body(response).await.map_err(DiscoveryError::Body)?;
        Ok(serde_json::from_slice(&body)?)
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
    price_usd: Option<f64>,
    change_24h_pct: Option<f64>,
    volume_24h_usd: Option<f64>,
    liquidity_usd: Option<f64>,
    txns_24h: Option<u64>,
    trade_url: String,
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
        Chain::RobinhoodChain => "robinhoodchain",
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

fn valid_token_address(chain: Chain, address: &str) -> bool {
    match chain {
        Chain::Solana => bs58::decode(address).into_vec().is_ok_and(|bytes| bytes.len() == 32),
        Chain::RobinhoodChain | Chain::Base | Chain::Ethereum | Chain::Bnb => {
            Chain::is_evm_address(address)
        }
    }
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
    let authority = url
        .split_once("://")
        .and_then(|(_, rest)| rest.split('/').next())
        .unwrap_or_default();
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
pub fn canonical_market_url(
    chain: Chain,
    pool: &str,
    source_url: Option<&str>,
) -> String {
    source_url
        .filter(|url| trusted_market_url(url, chain, pool))
        .map(str::to_owned)
        .unwrap_or_else(|| dex_pair_url(chain, pool))
}

pub fn load_curated(path: impl AsRef<Path>) -> Result<Vec<CuratedPool>, DiscoveryError> {
    let bytes = std::fs::read(path)?;
    Ok(serde_json::from_slice(&bytes)?)
}
fn candidate_from_pair(pair: &DexPair, registry: &Registry) -> Option<DiscoveryCandidate> {
    let chain = chain_from_dex_id(&pair.chain_id)?;
    let base_address = pair.base_token.address.clone()?;
    let quote_address = pair.quote_token.address.clone()?;
    if !valid_pair_address(chain, pair)
        || !valid_token_address(chain, &base_address)
        || !valid_token_address(chain, &quote_address)
    {
        return None;
    }
    let quote_entry = registry::lookup(registry, chain, &quote_address);
    let base_entry = registry::lookup(registry, chain, &base_address);
    let matched = quote_entry.or(base_entry)?;
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
    if !valid_pair_address(chain, pair)
        || !valid_token_address(chain, base_address)
        || !valid_token_address(chain, quote_address)
    {
        return None;
    }
    let matched = registry::lookup(registry, chain, quote_address)
        .or_else(|| registry::lookup(registry, chain, base_address))?;
    let trade_url =
        canonical_market_url(chain, &pair.pair_address, pair.url.as_deref());
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
        price_usd: pair.price_usd,
        change_24h_pct: pair.price_change.as_ref().and_then(|change| change.h24),
        volume_24h_usd: pair.volume.as_ref().and_then(|volume| volume.h24),
        liquidity_usd: pair.liquidity.as_ref().and_then(|liquidity| liquidity.usd),
        txns_24h,
        trade_url,
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
    let client = DexScreenerClient::new(state.http.clone());
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
        match client.tokens(chain, &addresses).await {
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
                warn!(%chain, error = %error, "DexScreener discovery failed");
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
    let client = DexScreenerClient::new(state.http.clone());
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
            None => check::check(state, &candidate.pool).await,
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
) -> CheckResult {
    let key = (candidate.chain, candidate.pool.to_ascii_lowercase());
    if let Some(result) = state.leaderboard_check_cache.get(&key).await {
        return result;
    }
    let result = check::check(state, &candidate.pool).await;
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
        verdict: "unknown".to_owned(),
        price_usd: candidate.price_usd,
        change_24h_pct: candidate.change_24h_pct,
        volume_24h_usd: candidate.volume_24h_usd,
        liquidity_usd: candidate.liquidity_usd,
        txns_24h: candidate.txns_24h,
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
        verdict: verdict_label(&result.verdict).to_owned(),
        price_usd: candidate.price_usd,
        change_24h_pct: candidate.change_24h_pct,
        volume_24h_usd: candidate.volume_24h_usd,
        liquidity_usd: candidate.liquidity_usd,
        txns_24h: candidate.txns_24h,
        detail_url: format!("/validated/{}/{}", chain_slug(candidate.chain), candidate.pool),
        trade_url: candidate.trade_url.clone(),
        explorer_url: explorer_url(candidate.chain, &candidate.pool),
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

async fn publish_leaderboard(
    state: &AppState,
    data_dir: &Path,
    registry: &Registry,
    total: usize,
    entries: Vec<LeaderboardEntry>,
    updated_at: &str,
    persist: bool,
) {
    let previous_count = state.leaderboard.read().await.entries.len();
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
    if !safe_board_replacement(previous_count, entries.len()) {
        defer_leaderboard_refresh(
            state,
            data_dir,
            "discovery returned too few pools",
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
    for (index, candidate) in candidates.into_iter().enumerate() {
        if index > 0 {
            sleep(REQUEST_INTERVAL).await;
        }
        let result = cached_leaderboard_check(state, &candidate).await;
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
    refresh_featured(state, data_dir, batch.featured, &shared_checks).await;
    crate::powers::notify_powers_warm_if_targets(state, warm_notify).await;
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
    })
}

/// Merge a DexScreener response into the last ticker snapshot. Keeping points
/// not present in a response makes a partial upstream failure non-destructive.
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

    let client = DexScreenerClient::new(state.http.clone());
    let mut pairs = Vec::new();
    for (chain, pools) in pools_by_chain {
        match client.pairs(chain, &pools).await {
            Ok(mut chain_pairs) => pairs.append(&mut chain_pairs),
            Err(error) if is_dex_blocked(&error) => return false,
            Err(error) => warn!(%chain, error = %error, "DexScreener price ticker failed"),
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
    use crate::chain::Chain;
    use crate::registry::Entry;

    #[test]
    fn robinhood_explorer_url_uses_blockscout() {
        assert_eq!(
            explorer_url(Chain::RobinhoodChain, "0xpool"),
            "https://robinhoodchain.blockscout.com/address/0xpool"
        );
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
        }
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
    fn first_discovery_delay_uses_earliest_due_board() {
        let now = DateTime::parse_from_rfc3339("2026-09-30T12:00:00Z")
            .expect("fixed timestamp parses")
            .with_timezone(&Utc);
        let older = restored_discovery_delay("2026-09-30T07:00:00Z", true, now);
        let newer = restored_discovery_delay("2026-09-30T11:00:00Z", true, now);

        assert_eq!(
            first_discovery_delay(older, newer),
            Some(Duration::from_secs(60 * 60))
        );
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
            "../tests/fixtures/discovery/search-nvdax-pump.json"
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
            serde_json::from_str(include_str!("../registry/featured.json"))
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
        let seed =
            name.bytes().fold(0u8, |sum, byte| sum.wrapping_add(byte.to_ascii_lowercase()));
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
        }
    }

    #[test]
    fn leaderboard_fixture_uses_exact_source_market_url() {
        let pairs: Vec<DexPair> = serde_json::from_str(include_str!(
            "../tests/fixtures/discovery/token-pairs-robinhood-nvda.json"
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
        assert_eq!(
            dex_pair_url(Chain::Bnb, pool),
            format!("https://dexscreener.com/bsc/{pool}")
        );
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
                Some("https://dexscreener.com/robinhood/0xd4eb21209c4d6093f80b5b84f5c45cc093ea14a3")
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
        pair.quote_token.address =
            Some("0xc845b2894dBddd03858fd2D643B4eF725fE0849d".to_owned());
        let registry = vec![entry(
            Chain::Ethereum,
            "0xc845b2894dBddd03858fd2D643B4eF725fE0849d",
            "NVDA",
        )];
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
        assert_eq!(
            candidate.trade_url,
            format!("https://dexscreener.com/ethereum/0x{}", "ab".repeat(32))
        );
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
            price_usd: Some(1.0),
            change_24h_pct: None,
            volume_24h_usd: volume,
            liquidity_usd: None,
            txns_24h: None,
            trade_url: "https://example.invalid".to_owned(),
        }
    }

    #[test]
    fn leaderboard_fixture_maps_metrics_and_urls() {
        let pairs: Vec<DexPair> = serde_json::from_str(include_str!(
            "../tests/fixtures/discovery/token-pairs-robinhood-nvda.json"
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
                    issuer: Some("Backed xStocks".to_owned()),
                    ticker: Some("NVDA".to_owned()),
                    verdict: "backed".to_owned(),
                    price_usd: Some(1.5),
                    change_24h_pct: Some(-0.25),
                    volume_24h_usd: Some(1_000.0),
                    liquidity_usd: Some(2_000.0),
                    txns_24h: Some(17),
                    detail_url: format!("/validated/solana/pool-{index}"),
                    trade_url: "https://example.invalid".to_owned(),
                    explorer_url: "https://example.invalid/explorer".to_owned(),
                    attestation_id: Some("att-1".to_owned()),
                    checked_at: Some(updated_at.to_owned()),
                })
                .collect(),
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
            },
            PricePoint {
                chain: "base".to_owned(),
                pool: "untouched".to_owned(),
                price_usd: Some(2.0),
                change_24h_pct: None,
                volume_24h_usd: None,
                liquidity_usd: None,
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
