pub(crate) use crate::domain::registry::{Entry, Registry};
use crate::{
    adapters::{discovery, state::AppState},
    app::context::Context,
    domain::registry,
    ports::IssuerRegistry,
};
use alloy::primitives::Address;
use async_trait::async_trait;
use chrono::{SecondsFormat, Utc};

use sha2::Digest;
use std::{
    path::Path,
    sync::{Arc, atomic::Ordering},
};
use thiserror::Error;
use tokio::sync::RwLock;
use tracing::{info, warn};

pub const XSTOCKS_URL: &str = "https://api.xstocks.fi/api/v2/public/assets";
pub const ONDO_URL: &str = "https://api.gm.ondo.finance/v1/assets/all/metadata";
pub const ONDO_DOCS_URL: &str = "https://docs.ondo.finance/addresses.md";
pub const ROBINHOOD_URL: &str = "https://api.robinhood.com/rhj/assets";

#[derive(Clone)]
pub(crate) struct RegistryEndpoints {
    pub(crate) xstocks: String,
    pub(crate) ondo: String,
    pub(crate) ondo_api_key: Option<String>,
    pub(crate) robinhood: String,
}

pub(crate) fn apply_registry_source(
    registry: &mut Registry,
    issuer: &str,
    result: Result<Vec<Entry>, String>,
    checked_at: &str,
    tombstone_missing: bool,
) -> (bool, bool) {
    match result {
        Ok(entries) => match registry::reconcile_snapshot(
            registry,
            issuer,
            entries,
            checked_at,
            tombstone_missing,
        ) {
            Ok(previous) => {
                if tombstone_missing {
                    info!(issuer, previous, "issuer registry full snapshot accepted");
                } else {
                    info!(
                        issuer,
                        previous,
                        "issuer registry partial snapshot accepted; existing deployments retained"
                    );
                }
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

pub(crate) use crate::domain::registry::{active_count, active_issuers, matchable};
#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("could not read registry file: {0}")]
    Io(#[from] std::io::Error),
    #[error("could not parse registry JSON: {0}")]
    Json(#[from] serde_json::Error),
}

pub fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

pub fn load_from_file(path: impl AsRef<Path>) -> Result<Registry, RegistryError> {
    let bytes = std::fs::read(path)?;
    let mut registry: Registry = serde_json::from_slice(&bytes)?;
    for entry in &mut registry {
        entry.contract = canonical_contract(entry.chain, &entry.contract);
    }
    let checked_at = now_rfc3339();
    let issuers =
        registry.iter().map(|entry| entry.issuer.clone()).collect::<std::collections::HashSet<_>>();
    for issuer in issuers {
        registry::mark_source_failure(&mut registry, &issuer, &checked_at);
    }
    Ok(registry)
}

pub fn save_to_file(path: impl AsRef<Path>, registry: &Registry) -> Result<(), RegistryError> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut sorted = registry.clone();
    sorted.sort_by(|left, right| {
        left.issuer
            .cmp(&right.issuer)
            .then_with(|| left.chain.to_string().cmp(&right.chain.to_string()))
            .then_with(|| left.ticker.cmp(&right.ticker))
            .then_with(|| {
                left.contract.to_ascii_lowercase().cmp(&right.contract.to_ascii_lowercase())
            })
    });
    std::fs::write(path, serde_json::to_vec_pretty(&sorted)?)?;
    Ok(())
}

pub fn canonical_contract(chain: crate::domain::chain::Chain, contract: impl AsRef<str>) -> String {
    let contract = contract.as_ref();
    if chain == crate::domain::chain::Chain::Solana {
        return contract.to_owned();
    }
    contract
        .parse::<Address>()
        .map(|address| address.to_checksum(None))
        .unwrap_or_else(|_| contract.to_owned())
}

#[async_trait]
impl IssuerRegistry for Arc<RwLock<Arc<Registry>>> {
    async fn snapshot(&self) -> Arc<Registry> {
        Arc::clone(&*self.read().await)
    }
}
pub(crate) fn file_hash(path: impl AsRef<Path>) -> String {
    let bytes = std::fs::read(path).unwrap_or_default();
    let mut hasher = sha2::Sha256::new();
    sha2::Digest::update(&mut hasher, bytes);
    sha2::Digest::finalize(hasher).iter().map(|byte| format!("{byte:02x}")).collect()
}
pub(crate) async fn refresh_registry(state: &AppState) {
    let (changed, updated_hash) = refresh_registry_entries(
        &state.registry,
        &state.registry_status,
        &state.registry_endpoints,
        &state.http,
        &state.registry_path,
        Some(&state.app),
    )
    .await;
    if let Some(hash) = updated_hash
        && let Ok(mut current_hash) = state.app.registry_hash.write()
    {
        *current_hash = hash;
    }
    if changed {
        state.powers_warm_notify.notify_one();
    }
    if let Err(error) = crate::adapters::web::refresh_stats_snapshot(state).await {
        tracing::warn!(%error, "could not refresh prepared statistics snapshot after registry update");
    }
}
fn record_catalog_completeness(
    context: Option<&Context>,
    issuer: &str,
    accepted: bool,
    complete: bool,
) {
    if !accepted {
        return;
    }
    let Some(context) = context else { return };
    let Ok(mut complete_issuers) = context.publisher_catalog_complete.write() else { return };
    if complete {
        complete_issuers.insert(issuer.to_owned());
    } else {
        complete_issuers.remove(issuer);
    }
}

struct OndoSnapshotPlan {
    tombstone_missing: bool,
    catalog_complete: bool,
    pending_removals: Vec<registry::DeploymentKey>,
}

fn plan_ondo_snapshot(
    current: &Registry,
    outcome: &registries::ondo::FetchOutcome,
) -> OndoSnapshotPlan {
    let previous_pending = &outcome.pending_removals;
    if !outcome.public_catalog {
        let complete = outcome.invalid_rows == 0;
        return OndoSnapshotPlan {
            tombstone_missing: complete,
            catalog_complete: complete,
            pending_removals: Vec::new(),
        };
    }

    let omitted = omitted_ondo_deployments(current, &outcome.entries);
    if !outcome.persist_snapshot {
        let pending_removals = if !previous_pending.is_empty() {
            previous_pending.clone()
        } else if outcome.invalid_rows == 0 {
            omitted.clone()
        } else {
            Vec::new()
        };
        let complete =
            pending_removals.is_empty() && omitted.is_empty() && outcome.invalid_rows == 0;
        return OndoSnapshotPlan {
            tombstone_missing: complete,
            catalog_complete: complete,
            pending_removals,
        };
    }
    if outcome.invalid_rows != 0 {
        return OndoSnapshotPlan {
            tombstone_missing: false,
            catalog_complete: false,
            pending_removals: Vec::new(),
        };
    }

    let omitted_set = omitted.iter().cloned().collect::<std::collections::HashSet<_>>();
    let previous_set = previous_pending.iter().cloned().collect::<std::collections::HashSet<_>>();
    let confirmed = omitted.is_empty() || omitted_set == previous_set;
    OndoSnapshotPlan {
        tombstone_missing: confirmed,
        catalog_complete: confirmed,
        pending_removals: if confirmed { Vec::new() } else { omitted },
    }
}

fn omitted_ondo_deployments(
    current: &Registry,
    incoming_entries: &[Entry],
) -> Vec<registry::DeploymentKey> {
    let incoming = incoming_entries
        .iter()
        .map(|entry| registry::DeploymentKey::new(entry.chain, &entry.contract))
        .collect::<std::collections::HashSet<_>>();
    let mut omitted = current
        .iter()
        .filter(|entry| entry.issuer == "Ondo" && registry::matchable(entry))
        .map(|entry| registry::DeploymentKey::new(entry.chain, &entry.contract))
        .filter(|key| !incoming.contains(key))
        .collect::<Vec<_>>();
    omitted.sort_by(|left, right| {
        chain_order(left.chain)
            .cmp(&chain_order(right.chain))
            .then_with(|| left.contract.cmp(&right.contract))
    });
    omitted
}

fn chain_order(chain: crate::domain::chain::Chain) -> u8 {
    use crate::domain::chain::Chain;
    match chain {
        Chain::Ethereum => 0,
        Chain::Bnb => 1,
        Chain::Solana => 2,
        Chain::Base => 3,
        Chain::RobinhoodChain => 4,
    }
}

pub(crate) async fn refresh_registry_entries(
    registry: &RwLock<Arc<Registry>>,
    registry_status: &RwLock<discovery::RegistrySnapshot>,
    endpoints: &RegistryEndpoints,
    http: &reqwest::Client,
    registry_path: &Path,
    invalidate: Option<&Context>,
) -> (bool, Option<String>) {
    {
        let mut status = registry_status.write().await;
        status.next_refresh_at = discovery::timestamp_after(discovery::REGISTRY_REFRESH_SECS);
        status.refreshing = true;
    }
    let (xstocks_result, ondo_result, robinhood_result) = tokio::join!(
        async {
            let result = registries::xstocks::fetch(http, &endpoints.xstocks)
                .await
                .map_err(|error| error.to_string());
            (result, now_rfc3339())
        },
        async {
            let result = registries::ondo::fetch_daily(
                http,
                &endpoints.ondo,
                endpoints.ondo_api_key.as_deref(),
                registry_path,
            )
            .await
            .map_err(|error| error.to_string());
            (result, now_rfc3339())
        },
        async {
            let result = registries::robinhood::fetch(http, &endpoints.robinhood)
                .await
                .map_err(|error| error.to_string());
            (result, now_rfc3339())
        },
    );

    let mut current = registry.write().await;
    let mut next = current.as_ref().clone();
    let mut accepted = false;
    let mut changed = false;
    for (issuer, (result, checked_at)) in
        [("Backed xStocks", xstocks_result), ("Robinhood", robinhood_result)]
    {
        let (source_accepted, source_changed) =
            apply_registry_source(&mut next, issuer, result, &checked_at, true);
        record_catalog_completeness(invalidate, issuer, source_accepted, true);
        accepted |= source_accepted;
        changed |= source_changed;
    }
    let (ondo_result, checked_at) = ondo_result;
    let mut ondo_catalog_complete = false;
    let (source_accepted, source_changed) = match ondo_result {
        Ok(outcome) => {
            let plan = plan_ondo_snapshot(&next, &outcome);
            let mut candidate = next.clone();
            let candidate_result = crate::domain::registry::reconcile_snapshot(
                &mut candidate,
                "Ondo",
                outcome.entries.clone(),
                &checked_at,
                plan.tombstone_missing,
            );
            if candidate_result.is_err() {
                apply_registry_source(
                    &mut next,
                    "Ondo",
                    Ok(outcome.entries),
                    &checked_at,
                    plan.tombstone_missing,
                )
            } else if outcome.persist_snapshot
                && let Err(error) = registries::ondo::persist_accepted_snapshot(
                    registry_path,
                    &outcome,
                    &plan.pending_removals,
                )
            {
                apply_registry_source(&mut next, "Ondo", Err(error.to_string()), &checked_at, false)
            } else {
                let result = apply_registry_source(
                    &mut next,
                    "Ondo",
                    Ok(outcome.entries),
                    &checked_at,
                    plan.tombstone_missing,
                );
                ondo_catalog_complete = result.0 && plan.catalog_complete;
                result
            }
        }
        Err(error) => apply_registry_source(&mut next, "Ondo", Err(error), &checked_at, false),
    };
    record_catalog_completeness(invalidate, "Ondo", source_accepted, ondo_catalog_complete);
    accepted |= source_accepted;
    changed |= source_changed;
    let snapshot = Arc::new(next);
    if changed && let Some(context) = invalidate {
        // Keep registry readers from observing new entries with cached old verdicts.
        context.registry_version.fetch_add(1, Ordering::AcqRel);
        context.check_cache.invalidate_all();
        context.powers_cache.invalidate_all();
    }
    *current = Arc::clone(&snapshot);
    drop(current);

    let updated_hash = if changed {
        match save_to_file(registry_path, &snapshot) {
            Ok(()) => Some(file_hash(registry_path)),
            Err(error) => {
                warn!(%error, "could not persist reconciled registry");
                None
            }
        }
    } else {
        None
    };
    let mut status = registry_status.write().await;
    status.entries = registry::active_count(&snapshot);
    status.issuers = registry::active_issuers(&snapshot);
    if accepted {
        status.updated_at = now_rfc3339();
        status.restored = false;
    }
    status.refreshing = false;
    (changed, updated_hash)
}
pub(crate) mod registries {
    use crate::adapters::registry::{Entry, canonical_contract, now_rfc3339};
    #[cfg(test)]
    use crate::adapters::registry::{ONDO_URL, ROBINHOOD_URL, XSTOCKS_URL};

    pub(crate) mod ondo {
        use super::Entry;
        #[cfg(test)]
        use super::ONDO_URL;
        use crate::domain::{chain::Chain, registry::OfficialDeployment};
        use chrono::{DateTime, Duration, SecondsFormat, Utc};
        use reqwest::Client;
        use serde::{Deserialize, Serialize};
        use serde_json::{Map, Value};
        use std::{
            future::Future,
            path::{Path, PathBuf},
            sync::Mutex,
        };
        use thiserror::Error;
        use tracing::warn;

        #[derive(Debug, Error)]
        pub enum Error {
            #[error("Ondo request failed")]
            Request(#[from] reqwest::Error),
            #[error("Ondo API returned HTTP {0}")]
            HttpStatus(u16),
            #[error("Ondo response was invalid JSON")]
            Json(#[from] serde_json::Error),
            #[error("Ondo response body failed limits: {0}")]
            Body(String),
            #[error("Ondo documentation returned HTTP {0}")]
            DocsHttpStatus(u16),
            #[error("Ondo Stocks CSV returned HTTP {0}")]
            CsvHttpStatus(u16),
            #[error("Ondo documentation did not contain an allowed Stocks CSV link")]
            MissingCsvLink,
            #[error("Ondo public source resolved outside its HTTPS host allowlist")]
            DisallowedPublicUrl,
            #[error("Ondo Stocks CSV header did not match the expected columns")]
            InvalidCsvHeader,
            #[error("Ondo Stocks CSV body is not UTF-8")]
            CsvUtf8,
            #[error("Ondo Stocks snapshot contained {0} valid rows; at least 100 are required")]
            IncompleteCsvSnapshot(usize),
            #[error("Ondo Stocks snapshot contained {invalid} damaged rows out of {total}")]
            DamagedCsvSnapshot { invalid: usize, total: usize },
            #[error("Ondo Stocks CSV had invalid quoting")]
            InvalidCsvFormat,
            #[error("Ondo Stocks CSV did not contain supported-chain token deployments")]
            NoSupportedDeployments,
            #[error("Ondo public catalog cache failed: {0}")]
            Cache(String),
            #[error("no accepted Ondo public catalog snapshot is cached yet")]
            NoCachedSnapshot,
        }

        #[derive(Debug, Deserialize, Serialize)]
        #[serde(deny_unknown_fields)]
        struct PublicCatalogCache {
            last_success_at: String,
            entries: Vec<Entry>,
            schema: Vec<String>,
            valid_rows: usize,
            invalid_rows: usize,
            total_rows: usize,
            source_csv: Option<String>,
            csv_url: Option<String>,
            #[serde(default, skip_serializing_if = "Vec::is_empty")]
            pending_removals: Vec<crate::domain::registry::DeploymentKey>,
        }

        #[derive(Debug, Deserialize, Serialize)]
        #[serde(deny_unknown_fields)]
        struct PublicCatalogAttempt {
            version: String,
            attempted_at: String,
        }

        #[derive(Debug, Clone)]
        struct ParsedCatalog {
            entries: Vec<Entry>,
            valid_rows: usize,
            invalid_rows: usize,
            total_rows: usize,
            source_csv: String,
            csv_url: String,
        }

        const EXPECTED_HEADER: [&str; 8] = [
            "Name",
            "Symbol",
            "CoinGecko API ID",
            "CMC UCID",
            "Ethereum Deployed Address",
            "BSC Deployed Address",
            "Solana Deployed Address",
            "HyperEVM Deployed Address",
        ];

        const CSV_ALLOWED_HOSTS: &[&str] = &["www.dropbox.com", "dl.dropboxusercontent.com"];
        const DOCS_ALLOWED_HOSTS: &[&str] = &["docs.ondo.finance"];
        const MAX_CSV_INVALID_ROWS: usize = 5;
        const MAX_REDIRECTS: usize = 3;
        const MIN_VALID_CSV_ROWS: usize = 100;
        const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

        /// Use the authenticated API when configured; otherwise use Ondo's published catalog.
        pub async fn fetch(
            client: &Client,
            endpoint_url: &str,
            api_key: Option<&str>,
        ) -> Result<Vec<Entry>, Error> {
            if let Some(api_key) = api_key.filter(|key| !key.is_empty()) {
                let response = client
                    .get(endpoint_url)
                    .header("x-api-key", api_key)
                    .timeout(REQUEST_TIMEOUT)
                    .send()
                    .await?;
                if !response.status().is_success() {
                    return Err(Error::HttpStatus(response.status().as_u16()));
                }
                let body = crate::adapters::net::body(response).await.map_err(Error::Body)?;
                return map_payload_with_source(
                    std::str::from_utf8(&body)
                        .map_err(|_| Error::Body("response was not UTF-8".to_owned()))?,
                    &super::now_rfc3339(),
                    endpoint_url,
                );
            }

            let catalog = fetch_public_catalog().await?;
            Ok(catalog.entries)
        }

        async fn fetch_public_catalog() -> Result<ParsedCatalog, Error> {
            let docs_client = public_client(DOCS_ALLOWED_HOSTS)?;
            let docs_response =
                docs_client.get(crate::adapters::registry::ONDO_DOCS_URL).send().await?;
            validate_response_url(&docs_response, DOCS_ALLOWED_HOSTS)?;
            if docs_response.status().is_redirection() {
                return Err(Error::DisallowedPublicUrl);
            }
            if !docs_response.status().is_success() {
                return Err(Error::DocsHttpStatus(docs_response.status().as_u16()));
            }
            let docs = crate::adapters::net::body(docs_response).await.map_err(Error::Body)?;
            let docs = std::str::from_utf8(&docs).map_err(|_| Error::CsvUtf8)?;
            let csv_url = extract_stocks_csv_url(docs).ok_or(Error::MissingCsvLink)?;
            let csv_client = public_client(CSV_ALLOWED_HOSTS)?;
            let csv_response = csv_client.get(&csv_url).send().await?;
            validate_response_url(&csv_response, CSV_ALLOWED_HOSTS)?;
            if csv_response.status().is_redirection() {
                return Err(Error::DisallowedPublicUrl);
            }
            if !csv_response.status().is_success() {
                return Err(Error::CsvHttpStatus(csv_response.status().as_u16()));
            }
            let csv = crate::adapters::net::body(csv_response).await.map_err(Error::Body)?;
            let csv = std::str::from_utf8(&csv).map_err(|_| Error::CsvUtf8)?;
            parse_public_catalog(csv, &csv_url, &super::now_rfc3339())
        }

        fn public_client(allowed_hosts: &'static [&'static str]) -> Result<Client, Error> {
            let redirect = reqwest::redirect::Policy::custom(move |attempt| {
                if attempt.previous().len() < MAX_REDIRECTS
                    && is_allowed_public_url(attempt.url(), allowed_hosts)
                {
                    attempt.follow()
                } else {
                    attempt.stop()
                }
            });
            Client::builder()
                .user_agent("qed/0.1")
                .timeout(REQUEST_TIMEOUT)
                .redirect(redirect)
                .build()
                .map_err(Error::Request)
        }

        fn is_allowed_public_url(url: &reqwest::Url, allowed_hosts: &[&str]) -> bool {
            url.scheme() == "https"
                && url.username().is_empty()
                && url.password().is_none()
                && url.host_str().is_some_and(|host| {
                    allowed_hosts.iter().any(|allowed| {
                        host.eq_ignore_ascii_case(allowed)
                            || (allowed.eq_ignore_ascii_case("dl.dropboxusercontent.com")
                                && host.len() > allowed.len()
                                && host.as_bytes()[host.len() - allowed.len() - 1] == b'.'
                                && host
                                    .get(host.len() - allowed.len()..)
                                    .is_some_and(|suffix| suffix.eq_ignore_ascii_case(allowed)))
                    })
                })
        }

        fn validate_response_url(
            response: &reqwest::Response,
            allowed_hosts: &[&str],
        ) -> Result<(), Error> {
            if is_allowed_public_url(response.url(), allowed_hosts) {
                Ok(())
            } else {
                Err(Error::DisallowedPublicUrl)
            }
        }

        const PUBLIC_CACHE_FILE: &str = "ondo-public-catalog.json";
        const PUBLIC_REFRESH_INTERVAL: Duration = Duration::hours(24);
        const PUBLIC_ATTEMPT_FILE: &str = "ondo-public-attempt.json";
        static LAST_PUBLIC_ATTEMPT_AT: Mutex<Option<DateTime<Utc>>> = Mutex::new(None);

        #[derive(Debug)]
        pub struct FetchOutcome {
            pub entries: Vec<Entry>,
            pub persist_snapshot: bool,
            pub public_catalog: bool,
            pub valid_rows: usize,
            pub invalid_rows: usize,
            pub total_rows: usize,
            pub pending_removals: Vec<crate::domain::registry::DeploymentKey>,
            last_attempt_at: Option<String>,
            source_csv: Option<String>,
            csv_url: Option<String>,
        }

        /// Reuse accepted snapshots and persist same-version attempt throttling across restarts.
        pub async fn fetch_daily(
            client: &Client,
            endpoint_url: &str,
            api_key: Option<&str>,
            registry_path: &Path,
        ) -> Result<FetchOutcome, Error> {
            if api_key.is_some_and(|key| !key.is_empty()) {
                return fetch(client, endpoint_url, api_key).await.map(|entries| FetchOutcome {
                    entries,
                    persist_snapshot: false,
                    public_catalog: false,
                    valid_rows: 0,
                    invalid_rows: 0,
                    total_rows: 0,
                    pending_removals: Vec::new(),
                    last_attempt_at: None,
                    source_csv: None,
                    csv_url: None,
                });
            }
            let cache_path = public_cache_path(registry_path);
            fetch_cached_at(&cache_path, Utc::now(), &LAST_PUBLIC_ATTEMPT_AT, fetch_public_catalog)
                .await
        }

        async fn fetch_cached_at<F, Fut>(
            cache_path: &Path,
            now: DateTime<Utc>,
            last_attempt_at: &Mutex<Option<DateTime<Utc>>>,
            fetch: F,
        ) -> Result<FetchOutcome, Error>
        where
            F: FnOnce() -> Fut,
            Fut: Future<Output = Result<ParsedCatalog, Error>>,
        {
            fetch_cached_at_for_version(
                cache_path,
                now,
                last_attempt_at,
                env!("CARGO_PKG_VERSION"),
                fetch,
            )
            .await
        }

        async fn fetch_cached_at_for_version<F, Fut>(
            cache_path: &Path,
            now: DateTime<Utc>,
            last_attempt_at: &Mutex<Option<DateTime<Utc>>>,
            version: &str,
            fetch: F,
        ) -> Result<FetchOutcome, Error>
        where
            F: FnOnce() -> Fut,
            Fut: Future<Output = Result<ParsedCatalog, Error>>,
        {
            let mut cache = match load_cache(cache_path) {
                Ok(cache) => cache,
                Err(error) => {
                    warn!(path = %cache_path.display(), %error, "ignoring invalid Ondo public cache");
                    None
                }
            };
            if cache.as_ref().is_some_and(|cached| cached.entries.is_empty()) {
                cache = None;
            }
            if cache
                .as_ref()
                .and_then(|cached| DateTime::parse_from_rfc3339(&cached.last_success_at).ok())
                .is_some_and(|last_success| last_success.with_timezone(&Utc) > now)
            {
                warn!(path = %cache_path.display(), "ignoring future-dated Ondo public cache");
                cache = None;
            }

            let marker_path = public_attempt_path(cache_path);
            let attempt = match load_attempt_marker(&marker_path) {
                Ok(attempt) => attempt,
                Err(error) => {
                    warn!(path = %marker_path.display(), %error, "ignoring invalid Ondo attempt marker");
                    None
                }
            };
            let recent_same_version_attempt = attempt.as_ref().is_some_and(|attempt| {
                DateTime::parse_from_rfc3339(&attempt.attempted_at)
                    .ok()
                    .map(|attempted_at| attempted_at.with_timezone(&Utc))
                    .is_some_and(|attempted_at| {
                        attempt.version == version
                            && attempted_at <= now
                            && now.signed_duration_since(attempted_at) < PUBLIC_REFRESH_INTERVAL
                    })
            });
            if recent_same_version_attempt {
                return cache.as_ref().map_or(Err(Error::NoCachedSnapshot), cached_snapshot);
            }

            let throttled = {
                let mut last_attempt = last_attempt_at.lock().expect("Ondo attempt mutex poisoned");
                if last_attempt
                    .as_ref()
                    .is_some_and(|last| now.signed_duration_since(*last) < PUBLIC_REFRESH_INTERVAL)
                {
                    true
                } else {
                    *last_attempt = Some(now);
                    false
                }
            };
            if throttled {
                return cache.as_ref().map_or(Err(Error::NoCachedSnapshot), cached_snapshot);
            }

            save_attempt_marker(&marker_path, version, now)?;
            let pending_removals =
                cache.as_ref().map_or_else(Vec::new, |cached| cached.pending_removals.clone());
            let catalog = fetch().await?;
            Ok(FetchOutcome {
                entries: catalog.entries,
                persist_snapshot: true,
                public_catalog: true,
                valid_rows: catalog.valid_rows,
                invalid_rows: catalog.invalid_rows,
                total_rows: catalog.total_rows,
                pending_removals,
                last_attempt_at: Some(now.to_rfc3339_opts(SecondsFormat::Secs, true)),
                source_csv: Some(catalog.source_csv),
                csv_url: Some(catalog.csv_url),
            })
        }

        fn cached_snapshot(cached: &PublicCatalogCache) -> Result<FetchOutcome, Error> {
            if cached.entries.is_empty() {
                return Err(Error::NoCachedSnapshot);
            }
            Ok(FetchOutcome {
                entries: cached.entries.clone(),
                persist_snapshot: false,
                public_catalog: true,
                valid_rows: cached.valid_rows,
                invalid_rows: cached.invalid_rows,
                total_rows: cached.total_rows,
                pending_removals: cached.pending_removals.clone(),
                last_attempt_at: None,
                source_csv: None,
                csv_url: None,
            })
        }

        pub fn persist_accepted_snapshot(
            registry_path: &Path,
            outcome: &FetchOutcome,
            pending_removals: &[crate::domain::registry::DeploymentKey],
        ) -> Result<(), Error> {
            let catalog = ParsedCatalog {
                entries: outcome.entries.clone(),
                valid_rows: outcome.valid_rows,
                invalid_rows: outcome.invalid_rows,
                total_rows: outcome.total_rows,
                source_csv: outcome
                    .source_csv
                    .clone()
                    .ok_or_else(|| Error::Cache("accepted CSV is missing".to_owned()))?,
                csv_url: outcome
                    .csv_url
                    .clone()
                    .ok_or_else(|| Error::Cache("accepted CSV URL is missing".to_owned()))?,
            };
            let last_attempt_at = outcome
                .last_attempt_at
                .as_deref()
                .ok_or_else(|| Error::Cache("accepted refresh timestamp is missing".to_owned()))?;
            save_accepted_cache_with_pending(
                &public_cache_path(registry_path),
                &catalog,
                last_attempt_at,
                pending_removals,
            )
        }

        fn public_cache_path(registry_path: &Path) -> PathBuf {
            registry_path.parent().unwrap_or_else(|| Path::new(".")).join(PUBLIC_CACHE_FILE)
        }

        fn public_attempt_path(cache_path: &Path) -> PathBuf {
            cache_path.parent().unwrap_or_else(|| Path::new(".")).join(PUBLIC_ATTEMPT_FILE)
        }

        fn save_attempt_marker(
            path: &Path,
            version: &str,
            attempted_at: DateTime<Utc>,
        ) -> Result<(), Error> {
            let marker = PublicCatalogAttempt {
                version: version.to_owned(),
                attempted_at: attempted_at.to_rfc3339_opts(SecondsFormat::Secs, true),
            };
            save_cache(path, &marker)
        }

        fn load_attempt_marker(path: &Path) -> Result<Option<PublicCatalogAttempt>, Error> {
            match std::fs::read(path) {
                Ok(bytes) => {
                    let marker: PublicCatalogAttempt = serde_json::from_slice(&bytes)
                        .map_err(|error| Error::Cache(error.to_string()))?;
                    DateTime::parse_from_rfc3339(&marker.attempted_at)
                        .map_err(|error| Error::Cache(error.to_string()))?;
                    Ok(Some(marker))
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(Error::Cache(error.to_string())),
            }
        }

        #[cfg(test)]
        fn save_accepted_cache(
            cache_path: &Path,
            catalog: &ParsedCatalog,
            last_success_at: &str,
        ) -> Result<(), Error> {
            save_accepted_cache_with_pending(cache_path, catalog, last_success_at, &[])
        }

        fn save_accepted_cache_with_pending(
            cache_path: &Path,
            catalog: &ParsedCatalog,
            last_success_at: &str,
            pending_removals: &[crate::domain::registry::DeploymentKey],
        ) -> Result<(), Error> {
            let cache = PublicCatalogCache {
                last_success_at: last_success_at.to_owned(),
                entries: catalog.entries.clone(),
                schema: expected_header_strings(),
                valid_rows: catalog.valid_rows,
                invalid_rows: catalog.invalid_rows,
                total_rows: catalog.total_rows,
                source_csv: Some(catalog.source_csv.clone()),
                csv_url: Some(catalog.csv_url.clone()),
                pending_removals: pending_removals.to_vec(),
            };
            validate_cached_snapshot(&cache)?;
            save_cache(cache_path, &cache)
        }

        fn load_cache(path: &Path) -> Result<Option<PublicCatalogCache>, Error> {
            match std::fs::read(path) {
                Ok(bytes) => {
                    let cache: PublicCatalogCache = serde_json::from_slice(&bytes)
                        .map_err(|error| Error::Cache(error.to_string()))?;
                    DateTime::parse_from_rfc3339(&cache.last_success_at)
                        .map_err(|error| Error::Cache(error.to_string()))?;
                    validate_cached_snapshot(&cache)?;
                    if cache.entries.is_empty() { Ok(None) } else { Ok(Some(cache)) }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(Error::Cache(error.to_string())),
            }
        }

        fn save_cache<T: Serialize>(path: &Path, cache: &T) -> Result<(), Error> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|error| Error::Cache(error.to_string()))?;
            }
            let temporary = path.with_extension("tmp");
            let bytes =
                serde_json::to_vec(cache).map_err(|error| Error::Cache(error.to_string()))?;
            std::fs::write(&temporary, bytes).map_err(|error| Error::Cache(error.to_string()))?;
            if let Err(error) = std::fs::rename(&temporary, path) {
                let _ = std::fs::remove_file(&temporary);
                return Err(Error::Cache(error.to_string()));
            }
            Ok(())
        }
        fn extract_stocks_csv_url(markdown: &str) -> Option<String> {
            let (_, stocks) = markdown.split_once("## Ondo Stocks")?;
            let section = stocks
                .lines()
                .take_while(|line| !line.starts_with("## "))
                .collect::<Vec<_>>()
                .join("\n");
            for link_start in
                section.match_indices("https://www.dropbox.com/").map(|(index, _)| index)
            {
                let Some(link_end) = section[link_start..].find(')') else {
                    continue;
                };
                let candidate = section[link_start..link_start + link_end].replace("\\&", "&");
                let Some(mut url) =
                    crate::adapters::net::allowlisted_https_url(&candidate, "www.dropbox.com")
                else {
                    continue;
                };
                if !url.path().to_ascii_lowercase().ends_with(".csv") {
                    continue;
                }
                let retained_query = url
                    .query()
                    .unwrap_or_default()
                    .split('&')
                    .filter(|pair| pair.split('=').next() != Some("dl"))
                    .collect::<Vec<_>>()
                    .join("&");
                let query = if retained_query.is_empty() {
                    "dl=1".to_owned()
                } else {
                    format!("{retained_query}&dl=1")
                };
                url.set_query(Some(&query));
                return Some(url.to_string());
            }
            None
        }

        /// Map the current Ondo asset metadata shape while retaining older deployment aliases.
        #[cfg(test)]
        pub fn map_payload(payload: &str, checked_at: &str) -> Result<Vec<Entry>, Error> {
            map_payload_with_source(payload, checked_at, ONDO_URL)
        }

        fn map_payload_with_source(
            payload: &str,
            checked_at: &str,
            source_url: &str,
        ) -> Result<Vec<Entry>, Error> {
            let value: Value = serde_json::from_str(payload)?;
            let records = records(&value);
            let mut entries = Vec::new();
            for record in records {
                let inherited_ticker = string(&record, &["ticker", "symbol", "tokenSymbol"]);
                let inherited_name = string(
                    &record,
                    &["underlyingName", "displayName", "name", "tokenName", "description"],
                );
                let inherited_network = network_value(
                    &record,
                    &["chain", "network", "networkChainId", "blockchain", "chainId"],
                );
                let inherited_chain =
                    inherited_network.as_deref().and_then(Chain::from_network_name);
                let inherited_decimals = number(&record, &["decimals", "tokenDecimals"])
                    .and_then(|value| u8::try_from(value).ok());
                let deployments = record
                    .get("deployments")
                    .or_else(|| record.get("addresses"))
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_else(|| vec![Value::Object(record.clone())]);
                let official_deployments = deployments
                    .iter()
                    .filter_map(|value| {
                        let deployment = value.as_object()?;
                        let address = string(
                            deployment,
                            &["address", "contract", "contractAddress", "tokenAddress"],
                        )?;
                        let network = network_value(
                            deployment,
                            &["chain", "network", "networkChainId", "blockchain", "chainId"],
                        )
                        .or_else(|| inherited_network.clone())?;
                        Some(OfficialDeployment {
                            network,
                            address,
                            wrapper_address: string(
                                deployment,
                                &["wrapperAddress", "wrapper_address", "wrapperTokenAddress"],
                            ),
                            wrapper_address_v2: string(
                                deployment,
                                &["wrapperAddressV2", "wrapper_address_v2"],
                            ),
                        })
                    })
                    .collect::<Vec<_>>();
                for deployment in deployments {
                    let Some(deployment) = deployment.as_object() else {
                        continue;
                    };
                    let Some(contract) = string(
                        deployment,
                        &["address", "contract", "contractAddress", "tokenAddress"],
                    ) else {
                        continue;
                    };
                    let ticker = string(deployment, &["ticker", "symbol", "tokenSymbol"])
                        .or_else(|| inherited_ticker.clone());
                    let Some(ticker) = ticker else {
                        continue;
                    };
                    let network = network_value(
                        deployment,
                        &["chain", "network", "networkChainId", "blockchain", "chainId"],
                    )
                    .or_else(|| inherited_network.clone());
                    let chain =
                        network.as_deref().and_then(Chain::from_network_name).or(inherited_chain);
                    let Some(chain) = chain else {
                        continue;
                    };
                    let name = string(
                        deployment,
                        &["underlyingName", "displayName", "name", "tokenName", "description"],
                    )
                    .or_else(|| inherited_name.clone())
                    .unwrap_or_else(|| ticker.clone());
                    let decimals = number(deployment, &["decimals", "tokenDecimals"])
                        .and_then(|value| u8::try_from(value).ok())
                        .or(inherited_decimals);
                    entries.push(Entry {
                        issuer: "Ondo".to_owned(),
                        ticker,
                        name,
                        chain,
                        contract,
                        decimals,
                        source: "ondo-api".to_owned(),
                        source_url: source_url.to_owned(),
                        last_checked: checked_at.to_owned(),
                        removed_at: None,
                        stale_since: None,
                        official_deployments: official_deployments.clone(),
                    });
                }
            }
            Ok(entries)
        }
        fn parse_stocks_csv(
            payload: &str,
            checked_at: &str,
            csv_url: &str,
        ) -> Result<ParsedCatalog, Error> {
            let payload = payload.strip_prefix('\u{feff}').unwrap_or(payload);
            let rows = parse_csv_rows(payload)?;
            let Some(header) = rows.first() else {
                return Err(Error::InvalidCsvHeader);
            };
            if header.len() < EXPECTED_HEADER.len()
                || header.iter().take(EXPECTED_HEADER.len()).map(String::as_str).ne(EXPECTED_HEADER)
            {
                return Err(Error::InvalidCsvHeader);
            }

            let mut entries = Vec::new();
            let mut valid_rows = 0;
            let mut invalid_rows = 0;
            let mut total_rows = 0;
            for row in rows.iter().skip(1) {
                let address_columns = [
                    (4, "Ethereum", Some(Chain::Ethereum)),
                    (5, "BNB Chain", Some(Chain::Bnb)),
                    (6, "Solana", Some(Chain::Solana)),
                    (7, "HyperEVM", None),
                ];
                if !address_columns.iter().any(|(index, _, _)| {
                    row.get(*index).is_some_and(|address| !address.trim().is_empty())
                }) {
                    continue;
                }
                total_rows += 1;
                if row.len() < EXPECTED_HEADER.len() {
                    invalid_rows += 1;
                    continue;
                }
                let Some(symbol) =
                    row.get(1).map(|value| value.trim()).filter(|value| !value.is_empty())
                else {
                    invalid_rows += 1;
                    continue;
                };
                let name = row
                    .first()
                    .map(|value| value.trim())
                    .filter(|value| !value.is_empty())
                    .unwrap_or(symbol);
                let mut deployments = Vec::new();
                let mut invalid_address = false;
                for (index, network, chain) in address_columns {
                    let address = row[index].trim();
                    if address.is_empty() {
                        continue;
                    }
                    let Some(address) = valid_catalog_address(network, address) else {
                        invalid_address = true;
                        break;
                    };
                    deployments.push((network, chain, address));
                }
                if invalid_address {
                    invalid_rows += 1;
                    continue;
                }
                if deployments.is_empty() {
                    invalid_rows += 1;
                    continue;
                }
                valid_rows += 1;
                let ticker = symbol.strip_suffix("on").unwrap_or(symbol);
                let official_deployments = deployments
                    .iter()
                    .map(|(network, _, address)| OfficialDeployment {
                        network: (*network).to_owned(),
                        address: address.clone(),
                        wrapper_address: None,
                        wrapper_address_v2: None,
                    })
                    .collect::<Vec<_>>();
                for (_, chain, address) in deployments {
                    let Some(chain) = chain else {
                        continue;
                    };
                    entries.push(Entry {
                        issuer: "Ondo".to_owned(),
                        ticker: ticker.to_owned(),
                        name: name.to_owned(),
                        chain,
                        contract: address,
                        decimals: None,
                        source: crate::adapters::registry::ONDO_DOCS_URL.to_owned(),
                        source_url: csv_url.to_owned(),
                        last_checked: checked_at.to_owned(),
                        removed_at: None,
                        stale_since: None,
                        official_deployments: official_deployments.clone(),
                    });
                }
            }
            Ok(ParsedCatalog {
                entries,
                valid_rows,
                invalid_rows,
                total_rows,
                source_csv: payload.to_owned(),
                csv_url: csv_url.to_owned(),
            })
        }
        fn parse_public_catalog(
            csv: &str,
            csv_url: &str,
            checked_at: &str,
        ) -> Result<ParsedCatalog, Error> {
            let catalog = parse_stocks_csv(csv, checked_at, csv_url)?;
            if catalog.valid_rows < MIN_VALID_CSV_ROWS {
                return Err(Error::IncompleteCsvSnapshot(catalog.valid_rows));
            }
            if !row_quality_is_valid(catalog.valid_rows, catalog.invalid_rows, catalog.total_rows) {
                return Err(Error::DamagedCsvSnapshot {
                    invalid: catalog.invalid_rows,
                    total: catalog.total_rows,
                });
            }
            if catalog.entries.is_empty() {
                return Err(Error::NoSupportedDeployments);
            }
            Ok(catalog)
        }

        fn valid_catalog_address(network: &str, address: &str) -> Option<String> {
            if network == "Solana" {
                let decoded = bs58::decode(address).into_vec().ok()?;
                return (decoded.len() == 32).then(|| address.to_owned());
            }
            let address = address.parse::<alloy::primitives::Address>().ok()?;
            Some(address.to_checksum(None))
        }
        fn expected_header_strings() -> Vec<String> {
            EXPECTED_HEADER.iter().map(|value| (*value).to_owned()).collect()
        }

        fn row_quality_is_valid(valid_rows: usize, invalid_rows: usize, total_rows: usize) -> bool {
            valid_rows.checked_add(invalid_rows) == Some(total_rows)
                && invalid_rows <= MAX_CSV_INVALID_ROWS
                && invalid_rows.saturating_mul(100) <= total_rows.saturating_mul(2)
        }

        fn validate_cached_snapshot(cache: &PublicCatalogCache) -> Result<(), Error> {
            if cache.schema != expected_header_strings() {
                return Err(Error::Cache("cached CSV schema is invalid".to_owned()));
            }
            if cache.entries.is_empty() {
                return if cache.valid_rows == 0
                    && cache.invalid_rows == 0
                    && cache.total_rows == 0
                    && cache.source_csv.is_none()
                    && cache.csv_url.is_none()
                    && cache.pending_removals.is_empty()
                {
                    Ok(())
                } else {
                    Err(Error::Cache("cached catalog quality metadata is inconsistent".to_owned()))
                };
            }
            if cache.valid_rows < MIN_VALID_CSV_ROWS
                || !row_quality_is_valid(cache.valid_rows, cache.invalid_rows, cache.total_rows)
            {
                return Err(Error::Cache("cached catalog is incomplete".to_owned()));
            }
            let source_csv = cache
                .source_csv
                .as_deref()
                .ok_or_else(|| Error::Cache("cached source CSV is missing".to_owned()))?;
            let csv_url = cache
                .csv_url
                .as_deref()
                .ok_or_else(|| Error::Cache("cached source URL is missing".to_owned()))?;
            let checked_at = &cache.entries[0].last_checked;
            DateTime::parse_from_rfc3339(checked_at)
                .map_err(|error| Error::Cache(error.to_string()))?;
            if cache.entries.iter().any(|entry| entry.last_checked != *checked_at) {
                return Err(Error::Cache("cached deployment timestamps differ".to_owned()));
            }
            let parsed = parse_public_catalog(source_csv, csv_url, checked_at)
                .map_err(|error| Error::Cache(error.to_string()))?;
            if parsed.entries != cache.entries
                || parsed.valid_rows != cache.valid_rows
                || parsed.invalid_rows != cache.invalid_rows
                || parsed.total_rows != cache.total_rows
            {
                return Err(Error::Cache("cached catalog differs from its source CSV".to_owned()));
            }
            let mut keys = std::collections::HashSet::new();
            for entry in &cache.entries {
                validate_cached_entry(entry)?;
                if !keys.insert(crate::domain::registry::DeploymentKey::new(
                    entry.chain,
                    &entry.contract,
                )) {
                    return Err(Error::Cache(
                        "cached catalog contains duplicate deployments".to_owned(),
                    ));
                }
            }
            if cache.pending_removals.len() > cache.valid_rows.saturating_mul(4) {
                return Err(Error::Cache("cached pending-removal set is too large".to_owned()));
            }
            let mut pending_keys = std::collections::HashSet::new();
            for key in &cache.pending_removals {
                let network = match key.chain {
                    Chain::Ethereum => "Ethereum",
                    Chain::Bnb => "BNB Chain",
                    Chain::Solana => "Solana",
                    _ => {
                        return Err(Error::Cache(
                            "cached pending removal uses an unsupported chain".to_owned(),
                        ));
                    }
                };
                if crate::domain::registry::DeploymentKey::new(key.chain, &key.contract) != *key
                    || valid_catalog_address(network, &key.contract).is_none()
                    || keys.contains(key)
                    || !pending_keys.insert(key)
                {
                    return Err(Error::Cache("cached pending removal is invalid".to_owned()));
                }
            }
            Ok(())
        }

        fn validate_cached_entry(entry: &Entry) -> Result<(), Error> {
            let network = match entry.chain {
                Chain::Ethereum => "Ethereum",
                Chain::Bnb => "BNB Chain",
                Chain::Solana => "Solana",
                _ => {
                    return Err(Error::Cache(
                        "cached deployment uses an unsupported chain".to_owned(),
                    ));
                }
            };
            if entry.issuer != "Ondo"
                || entry.ticker.trim().is_empty()
                || entry.name.trim().is_empty()
                || entry.source != crate::adapters::registry::ONDO_DOCS_URL
                || DateTime::parse_from_rfc3339(&entry.last_checked).is_err()
                || valid_catalog_address(network, &entry.contract).is_none()
                || entry.official_deployments.is_empty()
            {
                return Err(Error::Cache("cached deployment failed catalog validation".to_owned()));
            }
            let source_url = reqwest::Url::parse(&entry.source_url)
                .map_err(|error| Error::Cache(error.to_string()))?;
            if !is_allowed_public_url(&source_url, CSV_ALLOWED_HOSTS)
                || !source_url.path().to_ascii_lowercase().ends_with(".csv")
            {
                return Err(Error::Cache("cached CSV source URL is not allowlisted".to_owned()));
            }
            for deployment in &entry.official_deployments {
                if !matches!(
                    deployment.network.as_str(),
                    "Ethereum" | "BNB Chain" | "Solana" | "HyperEVM"
                ) || valid_catalog_address(&deployment.network, &deployment.address).is_none()
                    || deployment.wrapper_address.is_some()
                    || deployment.wrapper_address_v2.is_some()
                {
                    return Err(Error::Cache("cached official deployment is invalid".to_owned()));
                }
            }
            if !crate::domain::registry::official_deployment_matches(
                entry,
                entry.chain,
                &entry.contract,
            ) {
                return Err(Error::Cache(
                    "cached contract is absent from its published deployments".to_owned(),
                ));
            }
            Ok(())
        }

        fn parse_csv_rows(payload: &str) -> Result<Vec<Vec<String>>, Error> {
            let mut rows = Vec::new();
            let mut row = Vec::new();
            let mut field = String::new();
            let mut chars = payload.chars().peekable();
            let mut quoted = false;
            let mut after_quote = false;
            while let Some(character) = chars.next() {
                if quoted {
                    match character {
                        '"' if chars.peek() == Some(&'"') => {
                            chars.next();
                            field.push('"');
                        }
                        '"' => {
                            quoted = false;
                            after_quote = true;
                        }
                        _ => field.push(character),
                    }
                    continue;
                }
                if after_quote {
                    match character {
                        ',' => {
                            row.push(std::mem::take(&mut field).trim().to_owned());
                            after_quote = false;
                        }
                        '\n' => {
                            row.push(std::mem::take(&mut field).trim().to_owned());
                            rows.push(std::mem::take(&mut row));
                            after_quote = false;
                        }
                        '\r' => {
                            if chars.peek() == Some(&'\n') {
                                chars.next();
                            }
                            row.push(std::mem::take(&mut field).trim().to_owned());
                            rows.push(std::mem::take(&mut row));
                            after_quote = false;
                        }
                        ' ' | '\t' => {}
                        _ => return Err(Error::InvalidCsvFormat),
                    }
                    continue;
                }
                match character {
                    '"' if field.is_empty() => quoted = true,
                    ',' => row.push(std::mem::take(&mut field).trim().to_owned()),
                    '\n' => {
                        row.push(std::mem::take(&mut field).trim().to_owned());
                        rows.push(std::mem::take(&mut row));
                    }
                    '\r' => {
                        if chars.peek() == Some(&'\n') {
                            chars.next();
                        }
                        row.push(std::mem::take(&mut field).trim().to_owned());
                        rows.push(std::mem::take(&mut row));
                    }
                    '"' => return Err(Error::InvalidCsvFormat),
                    _ => field.push(character),
                }
            }
            if quoted {
                return Err(Error::InvalidCsvFormat);
            }
            if !field.is_empty() || !row.is_empty() {
                row.push(field.trim().to_owned());
                rows.push(row);
            }
            Ok(rows)
        }

        fn records(value: &Value) -> Vec<Map<String, Value>> {
            if let Some(array) = value.as_array() {
                return array.iter().filter_map(Value::as_object).cloned().collect();
            }
            let Some(object) = value.as_object() else {
                return Vec::new();
            };
            for key in ["assets", "data", "items", "results"] {
                if let Some(array) = object.get(key).and_then(Value::as_array) {
                    return array.iter().filter_map(Value::as_object).cloned().collect();
                }
            }
            vec![object.clone()]
        }

        fn string(object: &Map<String, Value>, keys: &[&str]) -> Option<String> {
            keys.iter().find_map(|key| object.get(*key).and_then(Value::as_str).map(str::to_owned))
        }

        fn number(object: &Map<String, Value>, keys: &[&str]) -> Option<u64> {
            keys.iter().find_map(|key| {
                object.get(*key).and_then(|value| {
                    value.as_u64().or_else(|| value.as_str().and_then(|string| string.parse().ok()))
                })
            })
        }

        fn network_value(object: &Map<String, Value>, keys: &[&str]) -> Option<String> {
            keys.iter().find_map(|key| {
                object.get(*key).and_then(|value| match value {
                    Value::String(network) => Some(network.clone()),
                    Value::Number(network) => Some(network.to_string()),
                    _ => None,
                })
            })
        }

        #[cfg(test)]
        mod tests {
            use super::{
                CSV_ALLOWED_HOSTS, EXPECTED_HEADER, Error, FetchOutcome, ParsedCatalog,
                cached_snapshot, extract_stocks_csv_url, fetch, fetch_cached_at,
                fetch_cached_at_for_version, load_cache, map_payload, parse_public_catalog,
                parse_stocks_csv, public_attempt_path, public_client, save_accepted_cache,
                save_accepted_cache_with_pending,
            };
            use crate::{
                adapters::registry::{apply_registry_source, plan_ondo_snapshot},
                domain::{
                    chain::Chain,
                    guard::{
                        GuardVerdict, IdentityStatus, SourceStatus, evaluate,
                        identify_with_catalog_status, issuer_metadata_key,
                    },
                    pool::TokenMeta,
                    registry::{DeploymentKey, Registry, match_status},
                },
            };
            use chrono::{Duration, Utc};
            use std::sync::{
                Mutex,
                atomic::{AtomicUsize, Ordering},
            };
            use tokio::{
                io::{AsyncReadExt, AsyncWriteExt},
                net::TcpListener,
            };
            fn csv_with_rows(rows: usize, damaged_rows: usize) -> String {
                let mut csv = format!("{}\n", EXPECTED_HEADER.join(","));
                for index in 0..rows {
                    let address = if index < damaged_rows {
                        "malformed".to_owned()
                    } else {
                        format!("0x{:040x}", index + 1)
                    };
                    csv.push_str(&format!("Token {index},T{index},,,{address},,,\n"));
                }
                csv
            }

            fn valid_catalog(rows: usize) -> ParsedCatalog {
                let csv = csv_with_rows(rows, 0);
                parse_public_catalog(
                    &csv,
                    "https://www.dropbox.com/scl/fi/fixture/ondo-stocks.csv?dl=1",
                    "2026-10-08T00:00:00Z",
                )
                .unwrap()
            }

            fn fresh_outcome(
                entries: Vec<crate::domain::registry::Entry>,
                pending_removals: Vec<DeploymentKey>,
            ) -> FetchOutcome {
                FetchOutcome {
                    valid_rows: entries.len(),
                    total_rows: entries.len(),
                    entries,
                    persist_snapshot: true,
                    public_catalog: true,
                    invalid_rows: 0,
                    pending_removals,
                    last_attempt_at: Some("2026-10-08T00:00:00Z".to_owned()),
                    source_csv: None,
                    csv_url: None,
                }
            }

            fn apply_public_snapshot(
                registry: &mut Registry,
                outcome: FetchOutcome,
            ) -> super::super::super::OndoSnapshotPlan {
                let plan = plan_ondo_snapshot(registry, &outcome);
                let (accepted, _) = apply_registry_source(
                    registry,
                    "Ondo",
                    Ok(outcome.entries),
                    "2026-10-08T00:00:00Z",
                    plan.tombstone_missing,
                );
                assert!(accepted);
                plan
            }

            #[test]
            fn first_clean_ondo_omission_stays_active_and_guard_is_unknown() {
                let full = valid_catalog(120).entries;
                let omitted = full[100].clone();
                let mut registry = full.clone();
                let plan = apply_public_snapshot(
                    &mut registry,
                    fresh_outcome(full[..100].to_vec(), vec![]),
                );

                assert!(!plan.tombstone_missing);
                assert!(!plan.catalog_complete);
                assert_eq!(
                    plan.pending_removals,
                    full[100..]
                        .iter()
                        .map(|entry| DeploymentKey::new(Chain::Ethereum, &entry.contract))
                        .collect::<Vec<_>>()
                );
                assert_eq!(
                    match_status(&registry, Chain::Ethereum, &omitted.contract),
                    crate::domain::registry::MatchStatus::Active
                );

                let observed = TokenMeta {
                    address: "0x000000000000000000000000000000000000ffff".to_owned(),
                    symbol: Some(omitted.ticker.clone()),
                    name: Some(omitted.name.clone()),
                    decimals: Some(18),
                    total_supply: None,
                };
                let published = TokenMeta {
                    address: omitted.contract.clone(),
                    symbol: observed.symbol.clone(),
                    name: observed.name.clone(),
                    decimals: Some(18),
                    total_supply: None,
                };
                let publisher_metadata = std::collections::HashMap::from([(
                    issuer_metadata_key(Chain::Ethereum, &omitted.contract),
                    published,
                )]);
                let identity = identify_with_catalog_status(
                    Chain::Ethereum,
                    &observed.address,
                    Some(&observed),
                    &registry,
                    &publisher_metadata,
                    &std::collections::HashSet::new(),
                );
                let verdict = evaluate(&identity, None, None, SourceStatus::Verified).0;
                assert_eq!(identity.status, IdentityStatus::NoPublisher);
                assert_eq!(verdict, GuardVerdict::Unknown);
            }

            #[test]
            fn second_identical_clean_ondo_omission_confirms_removal() {
                let full = valid_catalog(120).entries;
                let omitted = full[100].clone();
                let mut registry = full.clone();
                let first = apply_public_snapshot(
                    &mut registry,
                    fresh_outcome(full[..100].to_vec(), vec![]),
                );
                let second = apply_public_snapshot(
                    &mut registry,
                    fresh_outcome(full[..100].to_vec(), first.pending_removals.clone()),
                );

                assert!(second.tombstone_missing);
                assert!(second.catalog_complete);
                assert!(second.pending_removals.is_empty());
                assert!(matches!(
                    match_status(&registry, Chain::Ethereum, &omitted.contract),
                    crate::domain::registry::MatchStatus::Removed { .. }
                ));
            }

            #[test]
            fn reverted_ondo_omission_clears_pending_removal() {
                let full = valid_catalog(120).entries;
                let restored = full[100].clone();
                let mut registry = full.clone();
                let first = apply_public_snapshot(
                    &mut registry,
                    fresh_outcome(full[..100].to_vec(), vec![]),
                );
                let second = apply_public_snapshot(
                    &mut registry,
                    fresh_outcome(full.clone(), first.pending_removals),
                );

                assert!(second.tombstone_missing);
                assert!(second.catalog_complete);
                assert!(second.pending_removals.is_empty());
                assert_eq!(
                    match_status(&registry, Chain::Ethereum, &restored.contract),
                    crate::domain::registry::MatchStatus::Active
                );
            }

            #[test]
            fn pending_ondo_removals_round_trip_and_validate_against_cache() {
                let directory = tempfile::tempdir().expect("temporary catalog cache directory");
                let path = directory.path().join("ondo-public-catalog.json");
                let catalog = valid_catalog(100);
                let prior_catalog = valid_catalog(120);
                let pending = prior_catalog.entries[100..]
                    .iter()
                    .map(|entry| DeploymentKey::new(Chain::Ethereum, &entry.contract))
                    .collect::<Vec<_>>();
                save_accepted_cache_with_pending(&path, &catalog, "2026-10-08T00:00:00Z", &pending)
                    .unwrap();
                let loaded = load_cache(&path).unwrap().unwrap();
                assert_eq!(loaded.pending_removals, pending);

                let cached = cached_snapshot(&loaded).unwrap();
                let plan = plan_ondo_snapshot(&prior_catalog.entries, &cached);
                assert!(!plan.tombstone_missing);
                assert!(!plan.catalog_complete);

                let present = DeploymentKey::new(Chain::Ethereum, &catalog.entries[0].contract);
                assert!(
                    save_accepted_cache_with_pending(
                        &path,
                        &catalog,
                        "2026-10-08T00:00:00Z",
                        &[present],
                    )
                    .is_err()
                );
                save_accepted_cache(&path, &catalog, "2026-10-08T00:00:00Z").unwrap();
                let legacy_cache = load_cache(&path).unwrap().unwrap();
                let legacy_snapshot = cached_snapshot(&legacy_cache).unwrap();
                let legacy_plan = plan_ondo_snapshot(&prior_catalog.entries, &legacy_snapshot);
                assert!(!legacy_plan.tombstone_missing);
                assert!(!legacy_plan.catalog_complete);
                assert_eq!(legacy_plan.pending_removals.len(), 20);
            }

            #[test]
            fn maps_current_addresses_and_legacy_deployments() {
                let payload = include_str!("../../tests/fixtures/ondo.json");
                let entries = map_payload(payload, "2026-09-21T00:00:00Z").unwrap();
                assert_eq!(entries.len(), 5);
                assert_eq!(entries[0].ticker, "AAPL");
                assert_eq!(entries[0].name, "Apple");
                assert_eq!(entries[0].chain, Chain::Ethereum);
                assert_eq!(entries[0].decimals, Some(18));
                assert_eq!(entries[0].official_deployments.len(), 3);
                assert!(entries[0].official_deployments.iter().any(|deployment| {
                    deployment.network == "solana-900"
                        && deployment.address == "So11111111111111111111111111111111111111112"
                }));
                assert_eq!(entries[1].chain, Chain::Bnb);
                assert_eq!(entries[2].chain, Chain::Solana);
                assert_eq!(entries[3].ticker, "OUSG");
                assert_eq!(entries[3].chain, Chain::Ethereum);
                assert_eq!(entries[4].chain, Chain::Bnb);
            }

            #[test]
            fn retains_official_deployments_across_chains() {
                let payload = r#"{"assets":[{"tokenSymbol":"NVDA","tokenName":"NVIDIA • Robinhood Token","tokenDecimals":18,"deployments":[{"chainId":4663,"contractAddress":"0xAbCd000000000000000000000000000000000001"},{"chainId":8453,"contractAddress":"0xAbCd000000000000000000000000000000000002"}]}]}"#;
                let entries = map_payload(payload, "2026-09-21T00:00:00Z").unwrap();
                assert_eq!(entries.len(), 2);
                assert_eq!(entries[0].official_deployments.len(), 2);
                assert!(entries[0].official_deployments.iter().any(|deployment| {
                    deployment.network == "8453"
                        && deployment.address == "0xAbCd000000000000000000000000000000000002"
                }));
            }
            #[test]
            fn parses_recorded_ondo_stocks_catalog_and_retains_all_official_networks() {
                let csv = include_str!("../../tests/fixtures/ondo-stocks.csv");
                let csv_url = "https://www.dropbox.com/scl/fi/fixture/ondo-stocks.csv?dl=1";
                let catalog = parse_stocks_csv(csv, "2026-10-08T00:00:00Z", csv_url).unwrap();
                assert_eq!(catalog.valid_rows, 3);
                assert_eq!(catalog.entries.len(), 6);
                let apple = catalog
                    .entries
                    .iter()
                    .find(|entry| entry.ticker == "AAPL" && entry.chain == Chain::Ethereum)
                    .unwrap();
                assert_eq!(apple.name, "Apple Inc.");
                assert_eq!(apple.source, crate::adapters::registry::ONDO_DOCS_URL);
                assert_eq!(apple.source_url, csv_url);
                assert!(apple.official_deployments.iter().any(|deployment| {
                    deployment.network == "HyperEVM"
                        && deployment.address == "0x0000000000000000000000000000000000000003"
                }));
                assert!(!catalog.entries.iter().any(|entry| entry.chain.to_string() == "HyperEVM"));
                assert!(
                    catalog
                        .entries
                        .iter()
                        .any(|entry| entry.ticker == "USD" && entry.name == "USDon")
                );
                assert!(matches!(
                    parse_public_catalog(csv, csv_url, "2026-10-08T00:00:00Z"),
                    Err(Error::IncompleteCsvSnapshot(3))
                ));
            }

            #[test]
            fn rejects_unexpected_ondo_stocks_csv_headers() {
                let csv = include_str!("../../tests/fixtures/ondo-stocks.csv").replacen(
                    "Name,Symbol",
                    "Label,Symbol",
                    1,
                );
                assert!(matches!(
                    parse_stocks_csv(
                        &csv,
                        "2026-10-08T00:00:00Z",
                        "https://example.invalid/data.csv"
                    ),
                    Err(Error::InvalidCsvHeader)
                ));
            }

            #[test]
            fn extracts_only_the_ondo_stocks_dropbox_csv_and_forces_download() {
                let docs = r#"## Other
[Data](https://example.com/other.csv)
## Ondo Stocks
[Stocks](https://www.dropbox.com/scl/fi/key/tokens.csv?rlkey=abc\&dl=0)
## Next"#;
                let url = extract_stocks_csv_url(docs).unwrap();
                let parsed = reqwest::Url::parse(&url).unwrap();
                assert_eq!(parsed.host_str(), Some("www.dropbox.com"));
                assert_eq!(parsed.query_pairs().find(|(key, _)| key == "dl").unwrap().1, "1");
            }

            #[test]
            fn allows_only_https_dropbox_csv_hosts_and_label_bounded_subdomains() {
                for (url, expected) in [
                    ("https://www.dropbox.com/catalog.csv", true),
                    ("https://dl.dropboxusercontent.com/catalog.csv", true),
                    (
                        "https://uc93ef1c46600228e160888b77ac.dl.dropboxusercontent.com/catalog.csv",
                        true,
                    ),
                    ("https://evil-dl.dropboxusercontent.com.attacker.com/catalog.csv", false),
                    ("https://dl.dropboxusercontent.com.evil.com/catalog.csv", false),
                    (
                        "http://uc93ef1c46600228e160888b77ac.dl.dropboxusercontent.com/catalog.csv",
                        false,
                    ),
                ] {
                    let url = reqwest::Url::parse(url).unwrap();
                    assert_eq!(super::is_allowed_public_url(&url, CSV_ALLOWED_HOSTS), expected);
                }
            }

            #[tokio::test]
            async fn public_catalog_cache_reuses_snapshot_until_the_next_daily_fetch() {
                let directory = tempfile::tempdir().expect("temporary catalog cache directory");
                let cache_path = directory.path().join("ondo-public-catalog.json");
                let now = Utc::now();
                let catalog = valid_catalog(100);
                let fetches = AtomicUsize::new(0);
                let attempts = Mutex::new(None);

                let first = fetch_cached_at(&cache_path, now, &attempts, || async {
                    fetches.fetch_add(1, Ordering::Relaxed);
                    Ok(catalog.clone())
                })
                .await
                .unwrap();
                assert!(first.persist_snapshot);
                assert_eq!(first.entries, catalog.entries);
                save_accepted_cache(
                    &cache_path,
                    &catalog,
                    first.last_attempt_at.as_deref().unwrap(),
                )
                .unwrap();

                let after_restart = fetch_cached_at(
                    &cache_path,
                    now + Duration::hours(23),
                    &Mutex::new(None),
                    || async {
                        fetches.fetch_add(1, Ordering::Relaxed);
                        Ok(catalog.clone())
                    },
                )
                .await
                .unwrap();
                assert!(!after_restart.persist_snapshot);
                assert_eq!(after_restart.entries, catalog.entries);
                assert_eq!(fetches.load(Ordering::Relaxed), 1);

                let next_daily_fetch =
                    fetch_cached_at(&cache_path, now + Duration::hours(24), &attempts, || async {
                        fetches.fetch_add(1, Ordering::Relaxed);
                        Ok(catalog.clone())
                    })
                    .await
                    .unwrap();
                assert!(next_daily_fetch.persist_snapshot);
                assert_eq!(next_daily_fetch.entries, catalog.entries);
                assert_eq!(fetches.load(Ordering::Relaxed), 2);
            }

            #[tokio::test]
            async fn failed_attempt_marker_throttles_same_version_restart_but_new_version_retries()
            {
                let directory = tempfile::tempdir().expect("temporary catalog cache directory");
                let cache_path = directory.path().join("ondo-public-catalog.json");
                let now = Utc::now();
                let catalog = valid_catalog(100);
                let fetches = AtomicUsize::new(0);
                let old_success_at = (now - Duration::hours(25)).to_rfc3339();
                save_accepted_cache(&cache_path, &catalog, &old_success_at).unwrap();

                assert!(matches!(
                    fetch_cached_at_for_version(
                        &cache_path,
                        now,
                        &Mutex::new(None),
                        env!("CARGO_PKG_VERSION"),
                        || async {
                            fetches.fetch_add(1, Ordering::Relaxed);
                            Err(Error::DisallowedPublicUrl)
                        },
                    )
                    .await,
                    Err(Error::DisallowedPublicUrl)
                ));
                let marker_path = public_attempt_path(&cache_path);
                assert!(marker_path.exists());

                let same_version_restart = fetch_cached_at_for_version(
                    &cache_path,
                    now + Duration::hours(1),
                    &Mutex::new(None),
                    env!("CARGO_PKG_VERSION"),
                    || async {
                        fetches.fetch_add(1, Ordering::Relaxed);
                        Ok(catalog.clone())
                    },
                )
                .await
                .unwrap();
                assert!(!same_version_restart.persist_snapshot);
                assert_eq!(same_version_restart.entries, catalog.entries);
                assert_eq!(fetches.load(Ordering::Relaxed), 1);

                let new_version_restart = fetch_cached_at_for_version(
                    &cache_path,
                    now + Duration::hours(2),
                    &Mutex::new(None),
                    "next-release",
                    || async {
                        fetches.fetch_add(1, Ordering::Relaxed);
                        Ok(catalog.clone())
                    },
                )
                .await
                .unwrap();
                assert!(new_version_restart.persist_snapshot);
                assert_eq!(fetches.load(Ordering::Relaxed), 2);
            }
            #[tokio::test]
            async fn corrupt_attempt_marker_allows_fetch_with_a_fresh_snapshot() {
                let directory = tempfile::tempdir().expect("temporary catalog cache directory");
                let cache_path = directory.path().join("ondo-public-catalog.json");
                let now = Utc::now();
                let catalog = valid_catalog(100);
                let fetches = AtomicUsize::new(0);
                save_accepted_cache(&cache_path, &catalog, &now.to_rfc3339()).unwrap();
                std::fs::write(public_attempt_path(&cache_path), b"{broken").unwrap();

                let refreshed = fetch_cached_at_for_version(
                    &cache_path,
                    now + Duration::hours(1),
                    &Mutex::new(None),
                    env!("CARGO_PKG_VERSION"),
                    || async {
                        fetches.fetch_add(1, Ordering::Relaxed);
                        Ok(catalog.clone())
                    },
                )
                .await
                .unwrap();
                assert!(refreshed.persist_snapshot);
                assert_eq!(fetches.load(Ordering::Relaxed), 1);
            }
            #[test]
            fn strips_bom_and_preserves_quoted_commas_in_csv_fields() {
                let csv = format!(
                    "\u{feff}{}\n\"Apple, Inc.\",AAPL,,,0x0000000000000000000000000000000000000001,,,\n",
                    EXPECTED_HEADER.join(",")
                );
                let catalog = parse_stocks_csv(
                    &csv,
                    "2026-10-08T00:00:00Z",
                    "https://www.dropbox.com/scl/fi/fixture/ondo-stocks.csv?dl=1",
                )
                .unwrap();
                assert_eq!(catalog.valid_rows, 1);
                assert_eq!(catalog.entries[0].name, "Apple, Inc.");
            }

            #[test]
            fn damaged_large_catalog_is_rejected_without_tombstoning_existing_entries() {
                let checked_at = "2026-10-08T00:00:00Z";
                let csv = csv_with_rows(120, 6);
                let parsed = parse_public_catalog(
                    &csv,
                    "https://www.dropbox.com/scl/fi/fixture/ondo-stocks.csv?dl=1",
                    checked_at,
                );
                assert!(matches!(
                    &parsed,
                    Err(Error::DamagedCsvSnapshot { invalid: 6, total: 120 })
                ));

                let old = crate::domain::registry::Entry {
                    issuer: "Ondo".to_owned(),
                    ticker: "OLD".to_owned(),
                    name: "Old token".to_owned(),
                    chain: Chain::Ethereum,
                    contract: "0x0000000000000000000000000000000000000001".to_owned(),
                    decimals: Some(18),
                    source: crate::adapters::registry::ONDO_DOCS_URL.to_owned(),
                    source_url: "https://www.dropbox.com/scl/fi/fixture/ondo-stocks.csv?dl=1"
                        .to_owned(),
                    last_checked: checked_at.to_owned(),
                    removed_at: None,
                    stale_since: None,
                    official_deployments: Vec::new(),
                };
                let mut registry = vec![old];
                let (accepted, _) = crate::adapters::registry::apply_registry_source(
                    &mut registry,
                    "Ondo",
                    parsed.map(|catalog| catalog.entries).map_err(|error| error.to_string()),
                    checked_at,
                    true,
                );
                assert!(!accepted);
                assert_eq!(registry.len(), 1);
                assert!(crate::domain::registry::matchable(&registry[0]));
                assert_eq!(registry[0].contract, "0x0000000000000000000000000000000000000001");
            }

            #[test]
            fn damaged_ondo_row_keeps_previously_matchable_deployment_active() {
                let checked_at = "2026-10-08T00:00:00Z";
                let catalog = parse_public_catalog(
                    &csv_with_rows(120, 1),
                    "https://www.dropbox.com/scl/fi/fixture/ondo-stocks.csv?dl=1",
                    checked_at,
                )
                .unwrap();
                assert_eq!(catalog.valid_rows, 119);
                assert_eq!(catalog.invalid_rows, 1);
                assert_eq!(catalog.total_rows, 120);

                let old_address = "0x0000000000000000000000000000000000000001";
                let old = crate::domain::registry::Entry {
                    issuer: "Ondo".to_owned(),
                    ticker: "T0".to_owned(),
                    name: "Token 0".to_owned(),
                    chain: Chain::Ethereum,
                    contract: old_address.to_owned(),
                    decimals: Some(18),
                    source: crate::adapters::registry::ONDO_DOCS_URL.to_owned(),
                    source_url: "https://www.dropbox.com/scl/fi/fixture/ondo-stocks.csv?dl=1"
                        .to_owned(),
                    last_checked: checked_at.to_owned(),
                    removed_at: None,
                    stale_since: None,
                    official_deployments: Vec::new(),
                };
                let mut registry = vec![old];
                let before =
                    crate::domain::registry::match_status(&registry, Chain::Ethereum, old_address);
                assert_eq!(before, crate::domain::registry::MatchStatus::Active);

                let (accepted, _) = crate::adapters::registry::apply_registry_source(
                    &mut registry,
                    "Ondo",
                    Ok(catalog.entries),
                    checked_at,
                    catalog.invalid_rows == 0,
                );

                assert!(accepted);
                assert_eq!(
                    crate::domain::registry::match_status(&registry, Chain::Ethereum, old_address),
                    before
                );
                assert_eq!(registry.len(), 120);
            }

            #[tokio::test]
            async fn invalid_or_tampered_cache_is_refetched_after_restart() {
                let directory = tempfile::tempdir().expect("temporary catalog cache directory");
                let cache_path = directory.path().join("ondo-public-catalog.json");
                let now = Utc::now();
                let catalog = valid_catalog(100);
                let fetches = AtomicUsize::new(0);

                std::fs::write(&cache_path, b"{broken").unwrap();
                let first = fetch_cached_at(&cache_path, now, &Mutex::new(None), || async {
                    fetches.fetch_add(1, Ordering::Relaxed);
                    Ok(catalog.clone())
                })
                .await
                .unwrap();
                assert!(first.persist_snapshot);
                save_accepted_cache(
                    &cache_path,
                    &catalog,
                    first.last_attempt_at.as_deref().unwrap(),
                )
                .unwrap();

                let mut cached: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(&cache_path).unwrap()).unwrap();
                cached["last_success_at"] = serde_json::json!("not-a-timestamp");
                std::fs::write(&cache_path, serde_json::to_vec(&cached).unwrap()).unwrap();
                std::fs::write(public_attempt_path(&cache_path), b"{broken").unwrap();
                let second = fetch_cached_at(
                    &cache_path,
                    now + Duration::hours(1),
                    &Mutex::new(None),
                    || async {
                        fetches.fetch_add(1, Ordering::Relaxed);
                        Ok(catalog.clone())
                    },
                )
                .await
                .unwrap();
                assert!(second.persist_snapshot);
                save_accepted_cache(
                    &cache_path,
                    &catalog,
                    second.last_attempt_at.as_deref().unwrap(),
                )
                .unwrap();

                let mut cached: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(&cache_path).unwrap()).unwrap();
                cached["entries"][0]["contract"] = serde_json::json!("not-an-address");
                std::fs::write(&cache_path, serde_json::to_vec(&cached).unwrap()).unwrap();
                std::fs::write(public_attempt_path(&cache_path), b"{broken").unwrap();
                let third = fetch_cached_at(
                    &cache_path,
                    now + Duration::hours(2),
                    &Mutex::new(None),
                    || async {
                        fetches.fetch_add(1, Ordering::Relaxed);
                        Ok(catalog.clone())
                    },
                )
                .await
                .unwrap();
                assert!(third.persist_snapshot);
                assert_eq!(third.entries, catalog.entries);
                assert_eq!(fetches.load(Ordering::Relaxed), 3);
            }

            #[tokio::test]
            async fn public_redirect_to_unallowlisted_host_is_not_followed() {
                let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("listener");
                let address = listener.local_addr().expect("listener address");
                let server = tokio::spawn(async move {
                    let (mut stream, _) = listener.accept().await.expect("request");
                    let mut request = Vec::new();
                    let mut chunk = [0_u8; 1024];
                    loop {
                        let count = stream.read(&mut chunk).await.expect("read request");
                        if count == 0 {
                            break;
                        }
                        request.extend_from_slice(&chunk[..count]);
                        if request.windows(4).any(|window| window == b"\r\n\r\n") {
                            break;
                        }
                    }
                    stream
                        .write_all(
                            b"HTTP/1.1 302 Found\r\nLocation: https://evil.example/catalog.csv\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        )
                        .await
                        .expect("write redirect");
                });
                let url = format!("http://{address}/catalog.csv");
                let response =
                    public_client(CSV_ALLOWED_HOSTS).unwrap().get(&url).send().await.unwrap();
                server.await.unwrap();
                assert_eq!(response.status().as_u16(), 302);
                assert_eq!(response.url().as_str(), url);
                assert!(matches!(
                    super::validate_response_url(&response, CSV_ALLOWED_HOSTS),
                    Err(Error::DisallowedPublicUrl)
                ));
                assert!(!super::is_allowed_public_url(
                    &reqwest::Url::parse("https://evil.example/catalog.csv").unwrap(),
                    CSV_ALLOWED_HOSTS
                ));
            }

            #[tokio::test]
            async fn fetch_sends_api_key_and_reports_http_status_without_response_body() {
                let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("listener");
                let address = listener.local_addr().expect("listener address");
                let server = tokio::spawn(async move {
                    let (mut stream, _) = listener.accept().await.expect("request");
                    let mut request = Vec::new();
                    let mut chunk = [0_u8; 1024];
                    loop {
                        let count = stream.read(&mut chunk).await.expect("read request");
                        if count == 0 {
                            break;
                        }
                        request.extend_from_slice(&chunk[..count]);
                        if request.windows(4).any(|window| window == b"\r\n\r\n") {
                            break;
                        }
                    }
                    stream
                .write_all(
                    b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await
                .expect("write response");
                    String::from_utf8_lossy(&request).to_ascii_lowercase()
                });

                let url = format!("http://{address}/metadata");
                let error =
                    fetch(&reqwest::Client::new(), &url, Some("test-api-key")).await.unwrap_err();
                let request = server.await.expect("mock server");
                assert!(request.contains("x-api-key: test-api-key"));
                assert_eq!(error.to_string(), "Ondo API returned HTTP 403");
            }
        }
    }

    pub(crate) mod robinhood {
        use super::Entry;
        #[cfg(test)]
        use super::ROBINHOOD_URL;
        use crate::domain::chain::Chain;
        use reqwest::Client;
        use serde::Deserialize;
        use thiserror::Error;

        #[derive(Debug, Error)]
        pub enum Error {
            #[error("Robinhood request failed")]
            Request(#[from] reqwest::Error),
            #[error("Robinhood response was invalid JSON")]
            Json(#[from] serde_json::Error),
            #[error("Robinhood response body failed limits: {0}")]
            Body(String),
        }

        #[derive(Debug, Deserialize)]
        struct Response {
            assets: Vec<Asset>,
        }

        #[derive(Debug, Deserialize)]
        struct Asset {
            #[serde(rename = "tokenSymbol")]
            token_symbol: String,
            #[serde(rename = "tokenName")]
            token_name: String,
            deployments: Vec<Deployment>,
            #[serde(rename = "tokenDecimals")]
            token_decimals: Option<u8>,
            status: Option<String>,
        }

        #[derive(Debug, Deserialize)]
        struct Deployment {
            #[serde(rename = "contractAddress")]
            contract_address: String,
            #[serde(rename = "chainId")]
            chain_id: u64,
            #[serde(default, rename = "wrapperAddress")]
            wrapper_address: Option<String>,
            #[serde(default, rename = "wrapperAddressV2")]
            wrapper_address_v2: Option<String>,
        }

        pub async fn fetch(client: &Client, endpoint_url: &str) -> Result<Vec<Entry>, Error> {
            let response = client.get(endpoint_url).send().await?.error_for_status()?;
            let body = crate::adapters::net::body(response).await.map_err(Error::Body)?;
            map_payload_with_source(
                std::str::from_utf8(&body)
                    .map_err(|_| Error::Body("response was not UTF-8".to_owned()))?,
                &super::now_rfc3339(),
                endpoint_url,
            )
        }

        #[cfg(test)]
        pub fn map_payload(payload: &str, checked_at: &str) -> Result<Vec<Entry>, Error> {
            map_payload_with_source(payload, checked_at, ROBINHOOD_URL)
        }

        fn map_payload_with_source(
            payload: &str,
            checked_at: &str,
            source_url: &str,
        ) -> Result<Vec<Entry>, Error> {
            let response: Response = serde_json::from_str(payload)?;
            let mut entries = Vec::new();
            for asset in response.assets {
                if asset.status.as_deref().is_some_and(|status| status != "ASSET_STATUS_ACTIVE") {
                    continue;
                }
                let name = asset
                    .token_name
                    .strip_suffix(" • Robinhood Token")
                    .unwrap_or(&asset.token_name)
                    .to_owned();
                let official_deployments = asset
                    .deployments
                    .iter()
                    .map(|deployment| {
                        let network = Chain::from_network_name(&deployment.chain_id.to_string())
                            .map(|chain| chain.to_string())
                            .unwrap_or_else(|| format!("Chain ID {}", deployment.chain_id));
                        crate::domain::registry::OfficialDeployment {
                            network,
                            address: deployment.contract_address.clone(),
                            wrapper_address: deployment.wrapper_address.clone(),
                            wrapper_address_v2: deployment.wrapper_address_v2.clone(),
                        }
                    })
                    .collect::<Vec<_>>();
                for deployment in asset.deployments {
                    let Some(chain) = Chain::from_network_name(&deployment.chain_id.to_string())
                    else {
                        continue;
                    };
                    entries.push(Entry {
                        issuer: "Robinhood".to_owned(),
                        ticker: asset.token_symbol.clone(),
                        name: name.clone(),
                        chain,
                        contract: super::canonical_contract(chain, deployment.contract_address),
                        decimals: asset.token_decimals,
                        source: "robinhood-registry".to_owned(),
                        source_url: source_url.to_owned(),
                        last_checked: checked_at.to_owned(),
                        removed_at: None,
                        stale_since: None,
                        official_deployments: official_deployments.clone(),
                    });
                }
            }
            Ok(entries)
        }

        #[cfg(test)]
        mod tests {
            use super::map_payload;
            use crate::domain::chain::Chain;

            #[test]
            fn maps_live_asset_deployment() {
                let payload = include_str!("../../tests/fixtures/robinhood.json");
                let entries = map_payload(payload, "2026-09-21T00:00:00Z").unwrap();
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0].chain, Chain::RobinhoodChain);
                assert_eq!(entries[0].name, "NVIDIA");
                assert_eq!(entries[0].decimals, Some(18));
            }
            #[test]
            fn retains_every_robinhood_deployment_and_wrapper_on_each_entry() {
                let payload = r#"{"assets":[{"tokenSymbol":"NVDA","tokenName":"NVIDIA • Robinhood Token","tokenDecimals":18,"deployments":[{"chainId":4663,"contractAddress":"0x0000000000000000000000000000000000000001","wrapperAddress":"0x0000000000000000000000000000000000000002","wrapperAddressV2":"0x0000000000000000000000000000000000000003"},{"chainId":8453,"contractAddress":"0x0000000000000000000000000000000000000004","wrapperAddress":"0x0000000000000000000000000000000000000005"},{"chainId":99999,"contractAddress":"0x0000000000000000000000000000000000000006"}]}]}"#;
                let entries = map_payload(payload, "2026-09-21T00:00:00Z").unwrap();

                assert_eq!(entries.len(), 2);
                for entry in &entries {
                    assert_eq!(entry.official_deployments.len(), 3);
                    assert!(entry.official_deployments.iter().any(|deployment| {
                        deployment.network == "Robinhood Chain"
                            && deployment.address == "0x0000000000000000000000000000000000000001"
                            && deployment.wrapper_address.as_deref()
                                == Some("0x0000000000000000000000000000000000000002")
                            && deployment.wrapper_address_v2.as_deref()
                                == Some("0x0000000000000000000000000000000000000003")
                    }));
                    assert!(entry.official_deployments.iter().any(|deployment| {
                        deployment.network == "Base"
                            && deployment.address == "0x0000000000000000000000000000000000000004"
                            && deployment.wrapper_address.as_deref()
                                == Some("0x0000000000000000000000000000000000000005")
                    }));
                    assert!(
                        entry
                            .official_deployments
                            .iter()
                            .any(|deployment| deployment.network == "Chain ID 99999")
                    );
                }
            }
        }
    }

    pub(crate) mod xstocks {
        use super::Entry;
        #[cfg(test)]
        use super::XSTOCKS_URL;
        use crate::domain::chain::Chain;
        use crate::domain::registry::OfficialDeployment;
        use reqwest::Client;
        use serde::Deserialize;
        use thiserror::Error;
        use tokio::time::{Duration, sleep};

        #[derive(Debug, Error)]
        pub enum Error {
            #[error("xStocks request failed")]
            Request(#[from] reqwest::Error),
            #[error("xStocks response was invalid JSON")]
            Json(#[from] serde_json::Error),
            #[error("xStocks response body failed limits: {0}")]
            Body(String),
        }

        #[derive(Debug, Deserialize)]
        struct Response {
            nodes: Vec<Node>,
            #[serde(default)]
            page: Option<PageInfo>,
        }

        #[derive(Debug, Deserialize)]
        struct PageInfo {
            #[serde(rename = "hasNextPage")]
            has_next_page: bool,
        }

        #[derive(Debug, Deserialize)]
        struct Node {
            name: String,
            symbol: String,
            #[serde(rename = "underlyingSymbol")]
            underlying_symbol: Option<String>,
            deployments: Vec<Deployment>,
        }

        #[derive(Debug, Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Deployment {
            address: String,
            network: String,
            decimals: Option<u8>,
            #[serde(default)]
            wrapper_address: Option<String>,
            #[serde(default)]
            wrapper_address_v2: Option<String>,
        }

        const MAX_PAGES: usize = 20;

        fn page_has_more(response: &Response, page: usize) -> Result<bool, Error> {
            let Some(info) = response.page.as_ref() else {
                return Err(Error::Body("pagination metadata was missing".to_owned()));
            };
            if page + 1 == MAX_PAGES && info.has_next_page {
                return Err(Error::Body(
                    "source still has pages beyond the configured limit".to_owned(),
                ));
            }
            Ok(info.has_next_page)
        }

        pub async fn fetch(client: &Client, endpoint_url: &str) -> Result<Vec<Entry>, Error> {
            let checked_at = super::now_rfc3339();
            let mut nodes = Vec::new();
            for page in 0..MAX_PAGES {
                let response = client
                    .get(endpoint_url)
                    .header(reqwest::header::USER_AGENT, "qed/0.1")
                    .query(&[("page", page)])
                    .send()
                    .await?
                    .error_for_status()?;
                let body = crate::adapters::net::body(response).await.map_err(Error::Body)?;
                let response: Response = serde_json::from_slice(&body)?;
                let has_next_page = page_has_more(&response, page)?;
                nodes.extend(response.nodes);
                if !has_next_page {
                    return map_nodes_with_source(nodes, &checked_at, endpoint_url);
                }
                sleep(Duration::from_millis(50)).await;
            }
            unreachable!("the last page fails if the source is incomplete")
        }

        #[cfg(test)]
        fn map_payload(payload: &str, checked_at: &str) -> Result<Vec<Entry>, Error> {
            let response: Response = serde_json::from_str(payload)?;
            map_nodes(response.nodes, checked_at)
        }

        #[cfg(test)]
        fn map_nodes(nodes: Vec<Node>, checked_at: &str) -> Result<Vec<Entry>, Error> {
            map_nodes_with_source(nodes, checked_at, XSTOCKS_URL)
        }

        fn map_nodes_with_source(
            nodes: Vec<Node>,
            checked_at: &str,
            source_url: &str,
        ) -> Result<Vec<Entry>, Error> {
            let mut entries = Vec::new();
            for node in nodes {
                let ticker =
                    node.underlying_symbol.filter(|value| !value.is_empty()).unwrap_or(node.symbol);
                let name = node.name.strip_suffix(" xStock").unwrap_or(&node.name).to_owned();
                let official_deployments = node
                    .deployments
                    .iter()
                    .map(|deployment| OfficialDeployment {
                        network: deployment.network.clone(),
                        address: deployment.address.clone(),
                        wrapper_address: deployment.wrapper_address.clone(),
                        wrapper_address_v2: deployment.wrapper_address_v2.clone(),
                    })
                    .collect::<Vec<_>>();
                let mut official_list_attached = false;
                for deployment in node.deployments {
                    let Some(chain) = chain_for_network(&deployment.network) else {
                        continue;
                    };
                    let attached = if official_list_attached {
                        Vec::new()
                    } else {
                        official_list_attached = true;
                        official_deployments.clone()
                    };
                    entries.push(Entry {
                        issuer: "Backed xStocks".to_owned(),
                        ticker: ticker.clone(),
                        name: name.clone(),
                        chain,
                        contract: super::canonical_contract(chain, deployment.address),
                        decimals: deployment.decimals,
                        source: "xstocks-api".to_owned(),
                        source_url: source_url.to_owned(),
                        last_checked: checked_at.to_owned(),
                        removed_at: None,
                        stale_since: None,
                        official_deployments: attached,
                    });
                }
            }
            Ok(entries)
        }

        fn chain_for_network(network: &str) -> Option<Chain> {
            Chain::from_network_name(network)
        }

        #[cfg(test)]
        mod tests {
            use super::map_payload;
            use crate::domain::chain::Chain;

            #[test]
            fn maps_supported_deployments() {
                let payload = include_str!("../../tests/fixtures/xstocks.json");
                let entries = map_payload(payload, "2026-09-21T00:00:00Z").unwrap();
                assert_eq!(entries.len(), 3);
                assert_eq!(entries[0].issuer, "Backed xStocks");
                assert_eq!(entries[0].ticker, "NVDA");
                assert_eq!(entries[0].chain, Chain::Solana);
                assert_eq!(entries[1].chain, Chain::Ethereum);
                assert_eq!(entries[2].chain, Chain::Bnb);
                assert_eq!(entries[2].contract, "0x0000000000000000000000000000000000000102");
                assert_eq!(entries[0].official_deployments.len(), 12);
                let ethereum = entries[0]
                    .official_deployments
                    .iter()
                    .find(|deployment| deployment.network == "Ethereum")
                    .expect("official Ethereum deployment retained");
                assert_eq!(
                    ethereum.wrapper_address.as_deref(),
                    Some("0xAbCd000000000000000000000000000000000002")
                );
                assert_eq!(
                    ethereum.wrapper_address_v2.as_deref(),
                    Some("0xAbCd000000000000000000000000000000000003")
                );
            }

            #[test]
            fn xstocks_requires_explicit_complete_pagination() {
                let missing: super::Response = serde_json::from_str(r#"{"nodes":[]}"#).unwrap();
                assert!(super::page_has_more(&missing, 0).is_err());

                let capped: super::Response =
                    serde_json::from_str(r#"{"nodes":[],"page":{"hasNextPage":true}}"#).unwrap();
                assert!(super::page_has_more(&capped, super::MAX_PAGES - 1).is_err());

                let complete: super::Response =
                    serde_json::from_str(r#"{"nodes":[],"page":{"hasNextPage":false}}"#).unwrap();
                assert!(!super::page_has_more(&complete, 0).unwrap());
            }

            #[test]
            fn merges_paginated_fixture_pages() {
                let first: super::Response =
                    serde_json::from_str(include_str!("../../tests/fixtures/xstocks-page-0.json"))
                        .expect("first page");
                let second: super::Response =
                    serde_json::from_str(include_str!("../../tests/fixtures/xstocks-page-1.json"))
                        .expect("second page");
                let mut nodes = first.nodes;
                nodes.extend(second.nodes);
                let entries = super::map_nodes(nodes, "2026-09-22T00:00:00Z").expect("entries");
                assert!(entries.len() > 100);
                assert!(entries.iter().any(|entry| entry.ticker == "NVDAx"));
                assert!(
                    entries.iter().any(
                        |entry| entry.contract == "Xsc9qvGR1efVDFGLrVsmkzv3qi45LTBjeUKSPmx9qEh"
                    )
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Entry, load_from_file, save_to_file};
    use crate::domain::{
        chain::Chain,
        registry::{MatchStatus, lookup, match_status, matchable},
    };
    use chrono::{Duration, SecondsFormat, Utc};

    #[test]
    fn persisted_old_entries_are_not_matchable_at_startup() {
        let directory = tempfile::tempdir().expect("temporary registry directory");
        let path = directory.path().join("registry.json");
        let old = Entry {
            issuer: "Test".to_owned(),
            ticker: "OLD".to_owned(),
            name: "OLD".to_owned(),
            chain: Chain::Ethereum,
            contract: "0xabc".to_owned(),
            decimals: Some(18),
            source: "manual".to_owned(),
            source_url: "https://example.invalid".to_owned(),
            last_checked: (Utc::now() - Duration::hours(49))
                .to_rfc3339_opts(SecondsFormat::Secs, true),
            removed_at: None,
            stale_since: None,
            official_deployments: Vec::new(),
        };
        save_to_file(&path, &vec![old]).expect("write registry");

        let loaded = load_from_file(&path).expect("load registry");

        assert!(matches!(
            match_status(&loaded, Chain::Ethereum, "0xABC"),
            MatchStatus::Stale { .. }
        ));
        assert!(lookup(&loaded, Chain::Ethereum, "0xabc").is_none());
        assert!(!matchable(&loaded[0]));
    }
}
