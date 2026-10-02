mod attest;
mod chain;
mod check;
mod config;
mod discovery;
mod net;
mod pool;
mod powers;
mod registry;
mod state;
mod web;
use anyhow::{Context, Result};
use chrono::{Duration as ChronoDuration, Utc};
use clap::Parser;
use config::Config;
use moka::future::Cache;
use pool::{PoolReader, evm::EvmReader, solana::SolanaReader};
use registry::{Entry, Registry};
use sha2::Digest;
use state::{AppState, CachedCheckResult, RateLimiter, advance_registry_version};
use std::path::Path;
use std::sync::{Arc, RwLock as StdRwLock};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use tracing::{info, warn};
#[derive(Debug, Parser)]
#[command(name = "qed", about = "QED registry checker")]
struct Args {
    /// Refresh issuer registries once and exit instead of serving HTTP.
    #[arg(long)]
    refresh_once: bool,
}
#[derive(Clone)]
struct RegistryEndpoints {
    xstocks: String,
    ondo: String,
    ondo_api_key: Option<String>,
    robinhood: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    let args = Args::parse();
    let config = Config::from_env().context("reading QED configuration")?;
    let imprint_incomplete =
        include_str!("../release/legal/imprint.md").contains("[NOT PUBLISHED:");
    let imprint_override =
        std::env::var("QED_ALLOW_INCOMPLETE_IMPRINT").is_ok_and(|value| value == "1");
    if std::env::var("QED_ENV").is_ok_and(|value| value.eq_ignore_ascii_case("production"))
        && imprint_incomplete
        && !imprint_override
    {
        anyhow::bail!(
            "production imprint is incomplete; set QED_ALLOW_INCOMPLETE_IMPRINT=1 only for an explicitly approved temporary run"
        );
    }
    if std::env::var("QED_ENV").is_ok_and(|value| value.eq_ignore_ascii_case("production"))
        && imprint_incomplete
        && imprint_override
    {
        warn!("production started with incomplete imprint because QED_ALLOW_INCOMPLETE_IMPRINT=1");
    }
    std::fs::create_dir_all(&config.data_dir)
        .with_context(|| format!("creating data directory {}", config.data_dir.display()))?;
    let runtime_registry_path = config.data_dir.join("registry.json");
    let registry_source =
        if runtime_registry_path.exists() { &runtime_registry_path } else { &config.registry_path };
    let registry = load_initial_registry(registry_source)?;
    let registry_hash = registry_file_hash(registry_source);
    let registry_status = Arc::new(RwLock::new(discovery::RegistrySnapshot {
        entries: registry::active_count(&registry),
        issuers: registry::active_issuers(&registry),
        updated_at: registry::now_rfc3339(),
        next_refresh_at: discovery::timestamp_after(discovery::REGISTRY_REFRESH_SECS),
        restored: true,
        refreshing: false,
    }));
    let shared_registry = Arc::new(RwLock::new(registry));
    let (signing_key, dev_signer) =
        attest::signing_key_from_env(&config.data_dir).map_err(anyhow::Error::msg)?;
    if dev_signer {
        info!(public_key = %bs58::encode(signing_key.verifying_key().as_bytes()).into_string(), "using development QED signing key");
    }
    let client = reqwest::Client::builder()
        .user_agent("qed/0.1")
        .timeout(Duration::from_secs(30))
        .build()
        .context("building HTTP client")?;

    let source_http = reqwest::Client::builder()
        .user_agent("qed/0.1")
        .timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .context("building source verification HTTP client")?;

    let registry_hash_state = Arc::new(StdRwLock::new(registry_hash));
    let check_cache =
        Cache::builder().max_capacity(10_000).time_to_live(Duration::from_secs(30)).build();
    let powers_cache: Cache<
        (crate::chain::Chain, String, u64),
        crate::powers::PowersRecord,
    > = Cache::builder()
        .max_capacity(10_000)
        .time_to_live(Duration::from_secs(30 * 60))
        .build();
    let registry_endpoints = RegistryEndpoints {
        xstocks: config.registry_xstocks_url.clone(),
        ondo: config.registry_ondo_url.clone(),
        ondo_api_key: config.registry_ondo_api_key.clone(),
        robinhood: config.registry_robinhood_url.clone(),
    };
    if args.refresh_once {
        refresh_registry(
            &client,
            &runtime_registry_path,
            &shared_registry,
            &registry_hash_state,
            &registry_status,
            &check_cache,
            &powers_cache,
            &registry_endpoints,
        )
        .await;
        return Ok(());
    }

    let admin_auth = Arc::new(state::AdminAuth::new(
        config.admin_username.as_deref(),
        config.admin_password.as_deref(),
    ));
    let usage_stats = Arc::new(state::UsageStats::new());
    let rate_limiter =
        Arc::new(RateLimiter::with_rpc_rps(config.rpc_rps_solana, config.rpc_rps_evm));
    let rpc_buckets = rate_limiter.rpc_buckets();
    let configured_readers: Vec<Box<dyn PoolReader>> = vec![
        Box::new(
            EvmReader::new_with_limiter(
                crate::chain::Chain::RobinhoodChain,
                &config.rpc_robinhood,
                Arc::clone(&rpc_buckets.robinhood),
            )
            .context("building Robinhood Chain reader")?,
        ),
        Box::new(
            EvmReader::new_with_limiter(
                crate::chain::Chain::Base,
                &config.rpc_base,
                Arc::clone(&rpc_buckets.base),
            )
            .context("building Base reader")?,
        ),
        Box::new(
            EvmReader::new_with_limiter(
                crate::chain::Chain::Ethereum,
                &config.rpc_ethereum,
                Arc::clone(&rpc_buckets.ethereum),
            )
            .context("building Ethereum reader")?,
        ),
        Box::new(
            EvmReader::new_with_limiter(
                crate::chain::Chain::Bnb,
                &config.rpc_bnb,
                Arc::clone(&rpc_buckets.bnb),
            )
            .context("building BNB Chain reader")?,
        ),
        Box::new(SolanaReader::with_client_and_limiter(
            client.clone(),
            config.rpc_solana,
            Arc::clone(&rpc_buckets.solana),
        )),
    ];

    let attestations_dir = config.data_dir.join("attestations");
    let attest_store: Arc<dyn attest::AttestationStore> = match config.attest_bucket.clone() {
        Some(bucket) => {
            #[cfg(feature = "s3")]
            {
                Arc::new(attest::S3AttestationStore::new(bucket).await)
            }
            #[cfg(not(feature = "s3"))]
            {
                let _ = bucket;
                anyhow::bail!("QED_ATTEST_BUCKET requires the `s3` cargo feature");
            }
        }
        None => Arc::new(attest::FileAttestationStore::new(attestations_dir)),
    };
    let loaded_attestations = std::collections::HashMap::new();
    let board_store =
        Arc::new(discovery::DurableBoardStore::new(config.attest_bucket.clone()).await);
    let cached_leaderboard = match discovery::load_leaderboard(&config.data_dir) {
        Some(leaderboard) => Some(leaderboard),
        None => board_store.load_leaderboard().await,
    };
    let cached_featured = match discovery::load_featured_snapshot(&config.data_dir) {
        Some(snapshot) => Some(snapshot),
        None => board_store.load_featured().await,
    };
    let restore_now = Utc::now();
    let leaderboard_restore_delay = cached_leaderboard.as_ref().and_then(|leaderboard| {
        discovery::restored_discovery_delay(
            &leaderboard.updated_at,
            !leaderboard.entries.is_empty(),
            restore_now,
        )
    });
    let featured_restore_delay = cached_featured.as_ref().and_then(|snapshot| {
        discovery::restored_discovery_delay(
            &snapshot.updated_at,
            !snapshot.pools.is_empty(),
            restore_now,
        )
    });
    let first_discovery_delay =
        discovery::first_discovery_delay(leaderboard_restore_delay, featured_restore_delay);
    let restored_next_refresh_at = first_discovery_delay
        .map(|delay| discovery::timestamp_after(delay.as_secs()))
        .unwrap_or_else(|| discovery::timestamp_after(0));
    let cached_leaderboard = cached_leaderboard.map(|mut leaderboard| {
        leaderboard.next_refresh_at = restored_next_refresh_at.clone();
        leaderboard.restored = true;
        leaderboard.refreshing = false;
        leaderboard.empty_successful = false;
        leaderboard
    });
    let featured_status = cached_featured
        .as_ref()
        .map(|snapshot| discovery::FeaturedStatus {
            updated_at: snapshot.updated_at.clone(),
            next_refresh_at: restored_next_refresh_at,
            restored: true,
            refreshing: false,
            empty_successful: false,
        })
        .unwrap_or_default();
    if let Some(leaderboard) = &cached_leaderboard {
        info!(
            entries = leaderboard.entries.len(),
            updated_at = %leaderboard.updated_at,
            "restored leaderboard from the last run"
        );
    }
    if let Some(featured) = &cached_featured {
        info!(pools = featured.pools.len(), "restored featured pools from the last run");
    }
    let state = AppState {
        registry: Arc::clone(&shared_registry),
        registry_status: Arc::clone(&registry_status),
        readers: Arc::new(configured_readers),
        http: client.clone(),
        source_http,
        check_cache: check_cache.clone(),
        powers_cache: powers_cache.clone(),
        powers_retry_cache: Cache::builder()
            .max_capacity(10_000)
            .time_to_live(Duration::from_secs(30))
            .build(),
        powers_failure_cache: Cache::builder()
            .max_capacity(10_000)
            .time_to_live(Duration::from_secs(30))
            .build(),
        powers_locks: Cache::builder()
            .max_capacity(10_000)
            .time_to_live(Duration::from_secs(30 * 60))
            .build(),
        powers_prefetching: Arc::new(tokio::sync::Mutex::new(std::collections::HashSet::new())),
        leaderboard_check_cache: Cache::builder()
            .time_to_live(Duration::from_secs(30 * 60))
            .build(),
        check_inflight: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
        featured: Arc::new(RwLock::new(
            cached_featured.as_ref().map(|snapshot| snapshot.pools.clone()).unwrap_or_default(),
        )),
        featured_status: Arc::new(RwLock::new(featured_status)),
        leaderboard: Arc::new(RwLock::new(cached_leaderboard.unwrap_or_default())),
        prices: Arc::new(RwLock::new(discovery::PriceSnapshot::default())),
        attestations: Arc::new(StdRwLock::new(loaded_attestations)),
        signing_key: Arc::new(signing_key),
        dev_signer,
        board_store,
        attest_store,
        registry_hash: registry_hash_state.clone(),
        registry_api_cache: Arc::new(RwLock::new(None)),
        public_url: Arc::new(config.public_url.clone()),
        admin_auth,
        usage_stats,
        rate_limiter: Arc::clone(&rate_limiter),
        expensive_concurrency: Arc::new(tokio::sync::Semaphore::new(32)),
        powers_prefetch_concurrency: Arc::new(tokio::sync::Semaphore::new(2)),
        wallet_concurrency: Arc::new(tokio::sync::Semaphore::new(1)),
        registry_api_concurrency: Arc::new(tokio::sync::Semaphore::new(4)),
    };

    hydrate_attestations(&state).await.context("hydrating persisted attestations")?;
    let listener =
        TcpListener::bind(config.bind).await.with_context(|| format!("binding {}", config.bind))?;
    info!(address = %config.bind, "QED server listening");
    let prune_store = Arc::clone(&state.attest_store);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(24 * 60 * 60));
        interval.tick().await;
        loop {
            interval.tick().await;
            let cutoff = Utc::now() - ChronoDuration::days(30);
            if let Err(error) = prune_store.prune_expired(cutoff).await {
                warn!(%error, "could not prune expired attestations");
            }
        }
    });

    let refresh_client = client.clone();
    let refresh_path = runtime_registry_path.clone();
    let refresh_registry_state = Arc::clone(&shared_registry);
    let refresh_hash_state = registry_hash_state.clone();
    let refresh_status_state = Arc::clone(&registry_status);
    let refresh_check_cache = check_cache.clone();
    let refresh_powers_cache = powers_cache.clone();
    let refresh_endpoints = registry_endpoints.clone();
    tokio::spawn(async move {
        let mut interval =
            tokio::time::interval(Duration::from_secs(discovery::REGISTRY_REFRESH_SECS));
        loop {
            interval.tick().await;
            refresh_registry(
                &refresh_client,
                &refresh_path,
                &refresh_registry_state,
                &refresh_hash_state,
                &refresh_status_state,
                &refresh_check_cache,
                &refresh_powers_cache,
                &refresh_endpoints,
            )
            .await;
        }
    });

    let discovery_state = state.clone();
    let discovery_dir = config.data_dir.clone();
    tokio::spawn(async move {
        if let Some(delay) = first_discovery_delay {
            tokio::time::sleep(delay).await;
        }
        let mut first_refresh = true;
        loop {
            let retry_soon =
                discovery::refresh_discovery(&discovery_state, &discovery_dir, first_refresh).await;
            if !retry_soon {
                first_refresh = false;
            }
            let wait = if retry_soon { 30 } else { discovery::DISCOVERY_REFRESH_SECS };
            tokio::time::sleep(Duration::from_secs(wait)).await;
        }
    });
    let price_state = state.clone();
    tokio::spawn(async move {
        let mut wait = Duration::from_secs(discovery::PRICE_REFRESH_SECS);
        loop {
            tokio::time::sleep(wait).await;
            wait = if discovery::refresh_prices(&price_state).await {
                Duration::from_secs(discovery::PRICE_REFRESH_SECS)
            } else {
                Duration::from_secs(discovery::PRICE_RETRY_SECS)
            };
        }
    });

    axum::serve(
        listener,
        web::router(state).into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await
    .context("serving HTTP")?;
    Ok(())
}
async fn hydrate_attestations(state: &AppState) -> Result<()> {
    let cutoff = Utc::now() - ChronoDuration::days(30);
    if let Err(error) = state.attest_store.prune_expired(cutoff).await {
        warn!(%error, "could not prune expired attestations");
    }
    let loaded =
        state.attest_store.load_recent().await.context("loading recent persisted attestations")?;
    let mut records = loaded.into_iter().collect::<Vec<_>>();
    records.sort_by(|left, right| {
        right.1.checked_at.cmp(&left.1.checked_at).then_with(|| right.0.cmp(&left.0))
    });
    records.truncate(10_000);
    let mut valid = std::collections::HashMap::with_capacity(records.len());
    for (id, attestation) in records {
        if attest::valid_for_state(state, &id, &attestation)
            || attest::certificate_valid_for_state(state, &id, &attestation)
        {
            valid.insert(id, attestation);
        } else {
            warn!(%id, "quarantining invalid persisted attestation");
            let _ = state.attest_store.quarantine(&id).await;
        }
    }
    let count = valid.len();
    if let Ok(mut index) = state.attestations.write() {
        *index = valid;
    }
    info!(count, "indexed persisted attestations");
    Ok(())
}

fn load_initial_registry(path: &Path) -> Result<Registry> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    registry::load_from_file(path)
        .with_context(|| format!("loading registry from {}", path.display()))
}

fn apply_registry_source(
    registry: &mut Registry,
    issuer: &str,
    result: Result<Vec<Entry>, String>,
    checked_at: &str,
) -> (bool, bool) {
    match result {
        Ok(entries) => match registry::reconcile_snapshot(registry, issuer, entries, checked_at) {
            Ok(previous) => {
                info!(issuer, previous, "issuer registry full snapshot accepted");
                (true, true)
            }
            Err(rejected) => {
                let changed = registry::mark_source_failure(registry, issuer, checked_at);
                warn!(
                    issuer,
                    previous = rejected.previous,
                    incoming = rejected.incoming,
                    "issuer registry snapshot rejected as partial"
                );
                (false, changed)
            }
        },
        Err(error) => {
            let changed = registry::mark_source_failure(registry, issuer, checked_at);
            warn!(issuer, %error, "issuer registry refresh failed; previous snapshot retained");
            (false, changed)
        }
    }
}

async fn refresh_registry(
    client: &reqwest::Client,
    path: &Path,
    shared_registry: &Arc<RwLock<Registry>>,
    registry_hash: &Arc<StdRwLock<String>>,
    registry_status: &Arc<RwLock<discovery::RegistrySnapshot>>,
    check_cache: &Cache<String, CachedCheckResult>,
    powers_cache: &Cache<
        (crate::chain::Chain, String, u64),
        crate::powers::PowersRecord,
    >,
    endpoints: &RegistryEndpoints,
) {
    {
        let mut status = registry_status.write().await;
        status.next_refresh_at = discovery::timestamp_after(discovery::REGISTRY_REFRESH_SECS);
        status.refreshing = true;
    }
    let (xstocks_result, ondo_result, robinhood_result) = tokio::join!(
        async {
            let result = registry::xstocks::fetch(client, &endpoints.xstocks)
                .await
                .map_err(|error| error.to_string());
            (result, registry::now_rfc3339())
        },
        async {
            let result =
                registry::ondo::fetch(client, &endpoints.ondo, endpoints.ondo_api_key.as_deref())
                    .await
                    .map_err(|error| error.to_string());
            (result, registry::now_rfc3339())
        },
        async {
            let result = registry::robinhood::fetch(client, &endpoints.robinhood)
                .await
                .map_err(|error| error.to_string());
            (result, registry::now_rfc3339())
        },
    );
    let mut registry = shared_registry.write().await;
    let mut accepted = false;
    let mut changed = false;
    let (result, checked_at) = xstocks_result;
    let (source_accepted, source_changed) =
        apply_registry_source(&mut registry, "Backed xStocks", result, &checked_at);
    accepted |= source_accepted;
    changed |= source_changed;
    let (result, checked_at) = ondo_result;
    let (source_accepted, source_changed) =
        apply_registry_source(&mut registry, "Ondo", result, &checked_at);
    accepted |= source_accepted;
    changed |= source_changed;
    let (result, checked_at) = robinhood_result;
    let (source_accepted, source_changed) =
        apply_registry_source(&mut registry, "Robinhood", result, &checked_at);
    accepted |= source_accepted;
    changed |= source_changed;
    if changed {
        // Advance the cache version while the registry write lock is held.
        // In-flight checks then cannot repopulate a cache entry for this
        // replaced registry snapshot.
        advance_registry_version();
        check_cache.invalidate_all();
        powers_cache.invalidate_all();
    }

    let snapshot = registry.clone();
    drop(registry);
    if changed {
        if let Err(error) = registry::save_to_file(path, &snapshot) {
            warn!(%error, "could not persist reconciled registry");
        } else if let Ok(mut current_hash) = registry_hash.write() {
            *current_hash = registry_file_hash(path);
        }
    }
    let mut status = registry_status.write().await;
    status.entries = registry::active_count(&snapshot);
    status.issuers = registry::active_issuers(&snapshot);
    if accepted {
        status.updated_at = registry::now_rfc3339();
        status.restored = false;
    }
    status.refreshing = false;
}
fn registry_file_hash(path: &Path) -> String {
    let bytes = std::fs::read(path).unwrap_or_default();
    let mut hasher = sha2::Sha256::new();
    sha2::Digest::update(&mut hasher, bytes);
    let digest = sha2::Digest::finalize(hasher);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}
