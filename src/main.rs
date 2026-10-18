mod adapters;
mod app;
mod config;
mod domain;
mod ports;
use adapters::registry::RegistryEndpoints;
use adapters::state::{AppState, RateLimiter};
use adapters::{discovery, registry};
use adapters::{evm::EvmReader, solana::SolanaReader};
use anyhow::{Context, Result};
use app::{
    context::{CachedNoContract, Context as AppContext, GuardCacheKey, PowersCacheKey},
    warm,
};
use chrono::{Duration as ChronoDuration, Utc};
use clap::Parser;
use config::Config;
use domain::{chain::Chain, pool::TokenMeta, powers::PowersRecord};
use moka::future::Cache;
use ports::{AttestationStore, ChainReader, Clock, RpcRequestContext, with_rpc_request_context};
use std::{
    sync::{Arc, RwLock as StdRwLock, atomic::AtomicU64},
    time::Duration,
};
use tokio::net::TcpListener;
use tokio::sync::{Notify, RwLock};
use tracing::{info, warn};
pub(crate) struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> chrono::DateTime<chrono::Utc> {
        Utc::now()
    }
}

#[derive(Debug, Parser)]
#[command(name = "qed", about = "QED registry checker")]
struct Args {
    /// Refresh issuer registries once and exit instead of serving HTTP.
    #[arg(long)]
    refresh_once: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, clap::Subcommand)]
enum Command {
    /// Import valid, trusted local historical attestations, including expired records.
    HistoryBackfill {
        #[arg(long)]
        dir: std::path::PathBuf,
    },
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
    if args.command.is_some() && config.attest_bucket.is_some() {
        anyhow::bail!("history-backfill is local-only; unset QED_ATTEST_BUCKET");
    }
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
    let durable = match config.database_url.as_deref() {
        Some(url) => Some(
            adapters::storage::DurableStore::open(
                url,
                &config.data_dir,
                config.attest_bucket.clone(),
                config.db_blob_max_bytes,
            )
            .await
            .map_err(anyhow::Error::msg)?,
        ),
        None => None,
    };
    if let Some(store) = &durable
        && !matches!(
            tokio::time::timeout(
                Duration::from_secs(3),
                store.restore_runtime_files(&config.data_dir)
            )
            .await,
            Ok(Ok(()))
        )
    {
        warn!("restoring database runtime snapshots failed; using local cache");
    }
    let runtime_registry_path = config.data_dir.join("registry.json");
    let registry_source =
        if runtime_registry_path.exists() { &runtime_registry_path } else { &config.registry_path };
    let registry = if registry_source.exists() {
        registry::load_from_file(registry_source)
            .with_context(|| format!("loading registry from {}", registry_source.display()))?
    } else {
        Vec::new()
    };
    let registry_hash = registry::file_hash(registry_source);
    let registry_status = Arc::new(RwLock::new(discovery::RegistrySnapshot {
        entries: registry::active_count(&registry),
        issuers: registry::active_issuers(&registry),
        updated_at: registry::now_rfc3339(),
        next_refresh_at: discovery::timestamp_after(discovery::REGISTRY_REFRESH_SECS),
        restored: true,
        refreshing: false,
    }));
    let shared_registry = Arc::new(RwLock::new(Arc::new(registry)));
    let registry_endpoints = RegistryEndpoints {
        xstocks: config.registry_xstocks_url.clone(),
        ondo: config.registry_ondo_url.clone(),
        ondo_api_key: config.registry_ondo_api_key.clone(),
        robinhood: config.registry_robinhood_url.clone(),
    };
    let client = reqwest::Client::builder()
        .user_agent("qed/0.1")
        .timeout(Duration::from_secs(30))
        .build()
        .context("building HTTP client")?;
    if args.refresh_once {
        let _ = registry::refresh_registry_entries(
            &shared_registry,
            &registry_status,
            &registry_endpoints,
            &client,
            &runtime_registry_path,
            None,
            durable.as_deref(),
        )
        .await;
        return Ok(());
    }
    let (signing_key, dev_signer) = adapters::attestation::signing_key_from_env(&config.data_dir)
        .map_err(anyhow::Error::msg)?;
    if dev_signer {
        info!(public_key = %bs58::encode(signing_key.verifying_key().as_bytes()).into_string(), "using development QED signing key");
    }

    let source_http = reqwest::Client::builder()
        .user_agent("qed/0.1")
        .timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .context("building source verification HTTP client")?;

    let registry_hash_state = Arc::new(StdRwLock::new(registry_hash));
    let registry_version = Arc::new(AtomicU64::new(0));
    let check_cache =
        Cache::builder().max_capacity(10_000).time_to_live(Duration::from_secs(30)).build();
    let guard_cache: Cache<app::context::GuardCacheKey, app::context::CachedGuardDocument> =
        Cache::builder().max_capacity(256).time_to_live(app::guard::GUARD_CACHE_TTL).build();
    let guard_locks: Cache<GuardCacheKey, Arc<tokio::sync::Mutex<()>>> =
        Cache::builder().max_capacity(10_000).time_to_live(Duration::from_secs(30 * 60)).build();
    let no_code_cache: Cache<PowersCacheKey, CachedNoContract> =
        Cache::builder().max_capacity(4_096).time_to_live(Duration::from_secs(10 * 60)).build();
    let statement_cache = Cache::builder()
        .max_capacity(10_000)
        .time_to_live(Duration::from_secs(24 * 60 * 60))
        .weigher(|_, statement: &domain::statement::Statement| {
            u32::try_from(statement.assets.len().saturating_add(1)).unwrap_or(u32::MAX)
        })
        .build();
    let powers_cache: Cache<PowersCacheKey, PowersRecord> =
        Cache::builder().max_capacity(10_000).time_to_live(warm::POWERS_CACHE_TTL).build();
    let publisher_metadata_cache: Cache<(Chain, String), TokenMeta> = Cache::builder()
        .max_capacity(10_000)
        .time_to_live(app::guard::PUBLISHER_METADATA_CACHE_TTL)
        .build();
    let powers_warm_notify = Arc::new(Notify::new());

    let admin_auth = Arc::new(adapters::state::AdminAuth::new(
        config.admin_username.as_deref(),
        config.admin_password.as_deref(),
    ));
    let usage_stats = Arc::new(adapters::state::UsageStats::new());
    let rate_limiter = Arc::new(RateLimiter::with_rpc_limits(
        config.rpc_rps_solana,
        config.rpc_rps_evm,
        config.rpc_daily_cu_limit,
        config.rpc_daily_cu_limit_on_demand,
        config.global_rpc_requests_per_minute,
    ));
    let rpc_buckets = rate_limiter.rpc_buckets();
    let configured_readers: Vec<Box<dyn ChainReader>> = vec![
        Box::new(
            EvmReader::new_with_limiter(
                Chain::RobinhoodChain,
                &config.rpc_robinhood,
                Arc::clone(&rpc_buckets.robinhood),
            )
            .context("building Robinhood Chain reader")?,
        ),
        Box::new(
            EvmReader::new_with_limiter(
                Chain::Base,
                &config.rpc_base,
                Arc::clone(&rpc_buckets.base),
            )
            .context("building Base reader")?,
        ),
        Box::new(
            EvmReader::new_with_limiter(
                Chain::Ethereum,
                &config.rpc_ethereum,
                Arc::clone(&rpc_buckets.ethereum),
            )
            .context("building Ethereum reader")?,
        ),
        Box::new(
            EvmReader::new_with_limiter(Chain::Bnb, &config.rpc_bnb, Arc::clone(&rpc_buckets.bnb))
                .context("building BNB Chain reader")?,
        ),
        Box::new(SolanaReader::with_client_and_limiter(
            client.clone(),
            config.rpc_solana,
            Arc::clone(&rpc_buckets.solana),
        )),
    ];

    let attestations_dir = config.data_dir.join("attestations");
    let legacy_attest_store: Arc<dyn AttestationStore> = match config.attest_bucket.clone() {
        Some(bucket) => {
            #[cfg(feature = "s3")]
            {
                Arc::new(adapters::attestation::S3AttestationStore::new(bucket).await)
            }
            #[cfg(not(feature = "s3"))]
            {
                let _ = bucket;
                anyhow::bail!("QED_ATTEST_BUCKET requires the `s3` cargo feature");
            }
        }
        None => {
            Arc::new(adapters::attestation::FileAttestationStore::new(attestations_dir.clone()))
        }
    };
    let mut trusted = config.trusted_signers.clone();
    trusted.insert(bs58::encode(signing_key.verifying_key().as_bytes()).into_string());
    let attest_store: Arc<dyn AttestationStore> = match &durable {
        Some(store) => Arc::new(adapters::storage::PostgresAttestationStore {
            durable: Arc::clone(store),
            legacy: legacy_attest_store,
            disk: adapters::attestation::FileAttestationStore::new(attestations_dir),
            trusted: trusted.clone(),
        }),
        None => legacy_attest_store,
    };
    let loaded_attestations = std::collections::HashMap::new();
    let board_store = Arc::new(
        discovery::DurableBoardStore::new(config.attest_bucket.clone(), durable.clone()).await,
    );
    let cached_leaderboard = if durable.is_some() {
        tokio::time::timeout(Duration::from_secs(3), board_store.load_leaderboard())
            .await
            .ok()
            .flatten()
            .or_else(|| discovery::load_leaderboard(&config.data_dir))
    } else {
        match discovery::load_leaderboard(&config.data_dir) {
            Some(board) => Some(board),
            None => board_store.load_leaderboard().await,
        }
    };
    let cached_featured = if durable.is_some() {
        tokio::time::timeout(Duration::from_secs(3), board_store.load_featured())
            .await
            .ok()
            .flatten()
            .or_else(|| discovery::load_featured_snapshot(&config.data_dir))
    } else {
        match discovery::load_featured_snapshot(&config.data_dir) {
            Some(board) => Some(board),
            None => board_store.load_featured().await,
        }
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
    let readers = Arc::new(configured_readers);
    let powers_retry_cache =
        Cache::builder().max_capacity(10_000).time_to_live(Duration::from_secs(30)).build();
    let powers_failure_cache =
        Cache::builder().max_capacity(10_000).time_to_live(Duration::from_secs(30)).build();
    let powers_locks =
        Cache::builder().max_capacity(10_000).time_to_live(Duration::from_secs(30 * 60)).build();
    let powers_prefetching = Arc::new(tokio::sync::Mutex::new(std::collections::HashSet::new()));
    let leaderboard_check_cache =
        Cache::builder().time_to_live(Duration::from_secs(30 * 60)).build();
    let check_inflight = Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
    let featured = Arc::new(RwLock::new(
        cached_featured.as_ref().map(|snapshot| snapshot.pools.clone()).unwrap_or_default(),
    ));
    let leaderboard = Arc::new(RwLock::new(cached_leaderboard.unwrap_or_default()));
    let prices = Arc::new(RwLock::new(discovery::PriceSnapshot::default()));
    let attestations = Arc::new(StdRwLock::new(loaded_attestations));
    let signing_key = Arc::new(signing_key);
    let powers_prefetch_concurrency = Arc::new(tokio::sync::Semaphore::new(2));
    let registry_hash = Arc::clone(&registry_hash_state);
    let attestations_store = Arc::clone(&attest_store);
    let issuer_registry: Arc<dyn ports::IssuerRegistry> = Arc::new(Arc::clone(&shared_registry));
    let signer = Arc::new(adapters::attestation::Ed25519Signer::new(Arc::clone(&signing_key)));
    let history: Arc<dyn ports::AssetHistoryStore> = adapters::history::HistoryStore::open(
        config.database_url.as_deref(),
        &config.data_dir,
        durable.clone(),
    )
    .await
    .map_err(anyhow::Error::msg)?;
    let app_context = Arc::new(AppContext {
        registry: issuer_registry,
        history: Arc::clone(&history),
        readers: Arc::clone(&readers),
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
        powers_prefetching: Arc::clone(&powers_prefetching),
        check_inflight: Arc::clone(&check_inflight),
        pool_index: Arc::new(discovery::DiscoveryPoolIndex::new(
            Arc::clone(&featured),
            Arc::clone(&leaderboard),
        )),
        attestations: Arc::clone(&attestations),
        signer,
        trusted_signers: Arc::new(config.trusted_signers),
        dev_signer,
        attest_store: attestations_store,
        registry_hash: Arc::clone(&registry_hash),
        publisher_catalog_complete: Arc::new(StdRwLock::new(std::collections::HashSet::new())),
        registry_version: Arc::clone(&registry_version),
        source_verifier: Arc::new(adapters::sourcify::SourcifyVerifier::new(source_http.clone())),
        clock: Arc::new(SystemClock),
        powers_prefetch_concurrency: Arc::clone(&powers_prefetch_concurrency),
    });
    if let Some(Command::HistoryBackfill { dir }) = args.command {
        adapters::history::backfill(&app_context, &dir).await.map_err(anyhow::Error::msg)?;
        return Ok(());
    }
    let state = AppState {
        app: app_context,
        registry: Arc::clone(&shared_registry),
        registry_status: Arc::clone(&registry_status),
        registry_endpoints: Arc::new(registry_endpoints),
        registry_path: Arc::new(runtime_registry_path),
        powers_warm_notify: Arc::clone(&powers_warm_notify),
        http: client.clone(),
        featured,
        featured_status: Arc::new(RwLock::new(featured_status)),
        leaderboard,
        prices,
        leaderboard_check_cache,
        board_store,
        registry_api_cache: Arc::new(RwLock::new(None)),
        stats_snapshot: Arc::new(RwLock::new(None)),
        stats_snapshot_refresh: Arc::new(tokio::sync::Mutex::new(())),
        public_url: Arc::new(config.public_url.clone()),
        admin_auth,
        usage_stats,
        rate_limiter: Arc::clone(&rate_limiter),
        expensive_concurrency: Arc::new(tokio::sync::Semaphore::new(32)),
        wallet_concurrency: Arc::new(tokio::sync::Semaphore::new(1)),
        registry_api_concurrency: Arc::new(tokio::sync::Semaphore::new(4)),
    };
    if let Some(store) = durable {
        let import_dir = config.data_dir.clone();
        let import_store = Arc::clone(&store);
        tokio::spawn(async move {
            loop {
                if import_store.import_legacy(&import_dir, &trusted).await.is_ok() {
                    break;
                }
                warn!("legacy storage import paused; will resume in 60 seconds");
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        });
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(5));
            loop {
                interval.tick().await;
                let _ = store.flush().await;
            }
        });
    }
    let history_worker = Arc::clone(&history);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        loop {
            interval.tick().await;
            let _ = history_worker.flush().await;
        }
    });
    let retry_state = state.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        loop {
            interval.tick().await;
            let _ = with_rpc_request_context(
                RpcRequestContext::new(false, false),
                adapters::history::retry_failed(&retry_state),
            )
            .await;
        }
    });
    let rpc_budget = rate_limiter.rpc_budget();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60 * 60));
        loop {
            interval.tick().await;
            let usage = rpc_budget.snapshot();
            info!(
                "rpc usage day={} est_cu_total={} on_demand={} background={} rejected={}",
                usage.day, usage.est_cu_total, usage.on_demand, usage.background, usage.rejected
            );
        }
    });

    app::attestation::hydrate_attestations(&state.app)
        .await
        .context("loading recent persisted attestations")?;
    let initial_stats_state = state.clone();
    tokio::spawn(async move {
        if !adapters::web::restore_stats_snapshot(&initial_stats_state).await {
            let (result, _) = with_rpc_request_context(
                RpcRequestContext::new(false, false),
                adapters::web::refresh_stats_snapshot(&initial_stats_state),
            )
            .await;
            if result.is_err() {
                warn!("building initial statistics snapshot failed");
            }
        }
    });

    let warm_state = state.clone();
    let warm_notify = Arc::clone(&powers_warm_notify);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(warm::POWERS_WARM_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = interval.tick() => {}
                _ = warm_notify.notified() => {}
            }
            let _ = with_rpc_request_context(
                RpcRequestContext::new(false, false),
                warm::warm_current_pool_powers(&warm_state.app),
            )
            .await;
        }
    });
    let listener =
        TcpListener::bind(config.bind).await.with_context(|| format!("binding {}", config.bind))?;
    info!(address = %config.bind, "QED server listening");
    let prune_store = Arc::clone(&state.app.attest_store);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(24 * 60 * 60));
        loop {
            interval.tick().await;
            let cutoff = Utc::now() - ChronoDuration::days(30);
            if let Err(error) = prune_store.prune_expired(cutoff).await {
                warn!(%error, "could not prune expired attestations");
            }
        }
    });

    let refresh_state = state.clone();
    tokio::spawn(async move {
        let mut interval =
            tokio::time::interval(Duration::from_secs(discovery::REGISTRY_REFRESH_SECS));
        loop {
            interval.tick().await;
            let _ = with_rpc_request_context(
                RpcRequestContext::new(false, false),
                registry::refresh_registry(&refresh_state),
            )
            .await;
        }
    });

    let discovery_state = state.clone();
    let discovery_dir = config.data_dir.clone();
    let discovery_warm_notify = Arc::clone(&powers_warm_notify);
    tokio::spawn(async move {
        if let Some(delay) = first_discovery_delay {
            tokio::time::sleep(delay).await;
        }
        let mut first_refresh = true;
        loop {
            let (retry_soon, outcome) = with_rpc_request_context(
                RpcRequestContext::new(false, false),
                discovery::refresh_discovery(
                    &discovery_state,
                    &discovery_dir,
                    first_refresh,
                    &discovery_warm_notify,
                ),
            )
            .await;
            let budget_rejected = outcome.budget_rejected();
            if !retry_soon && !budget_rejected {
                first_refresh = false;
            }
            tokio::time::sleep(discovery_wait_duration(Utc::now(), retry_soon, budget_rejected))
                .await;
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
        adapters::web::router(state).into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await
    .context("serving HTTP")?;
    Ok(())
}

fn discovery_wait_duration(
    now: chrono::DateTime<Utc>,
    retry_soon: bool,
    budget_rejected: bool,
) -> Duration {
    if budget_rejected {
        let next_midnight = (now.date_naive() + ChronoDuration::days(1))
            .and_hms_opt(0, 0, 0)
            .expect("next UTC midnight is valid")
            .and_utc();
        return next_midnight
            .signed_duration_since(now)
            .to_std()
            .unwrap_or_default()
            .min(Duration::from_secs(discovery::DISCOVERY_REFRESH_SECS));
    }
    if retry_soon {
        Duration::from_secs(30)
    } else {
        Duration::from_secs(discovery::DISCOVERY_REFRESH_SECS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn utc(value: &str) -> chrono::DateTime<Utc> {
        chrono::DateTime::parse_from_rfc3339(value).unwrap().with_timezone(&Utc)
    }
    #[test]
    fn discovery_budget_rejection_waits_until_utc_midnight_or_refresh_cap() {
        assert_eq!(
            discovery_wait_duration(utc("2026-10-17T23:30:00Z"), true, true),
            Duration::from_secs(30 * 60)
        );
        assert_eq!(
            discovery_wait_duration(utc("2026-10-17T10:00:00Z"), false, true),
            Duration::from_secs(discovery::DISCOVERY_REFRESH_SECS)
        );
        assert_eq!(
            discovery_wait_duration(utc("2026-10-17T10:00:00Z"), true, false),
            Duration::from_secs(30)
        );
    }
}
