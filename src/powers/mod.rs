use crate::{
    attest::{self, Read},
    chain::Chain,
    pool::PoolError,
    registry,
    state::AppState,
};
use axum::http::StatusCode;
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    time::{Duration, Instant},
};
use tracing::info;
use tokio::sync::Notify;

pub(crate) mod evm;
pub(crate) mod solana;

const MAX_QUEUED_PREFETCHES: usize = 64;
pub(crate) const POWERS_CACHE_TTL: Duration = Duration::from_secs(30 * 60);
pub(crate) const POWERS_WARM_INTERVAL: Duration = Duration::from_secs(25 * 60);
const POWERS_CACHE_REFRESH_AGE: Duration = Duration::from_secs(
    POWERS_CACHE_TTL.as_secs() - POWERS_WARM_INTERVAL.as_secs(),
);
pub(crate) const POWERS_WARM_MAX_CONTRACTS: usize = 160;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Reason {
    pub code: String,
    pub detail: String,
}

impl Reason {
    pub(crate) fn new(code: &str, detail: impl Into<String>) -> Self {
        Self { code: code.to_owned(), detail: detail.into() }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceVerified {
    ExactMatch,
    Match,
    None,
    Unavailable,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceVerifiedSubject {
    TokenProgram,
    Contract,
    Implementation,
}

impl SourceVerifiedSubject {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::TokenProgram => "Token program build",
            Self::Contract => "Contract source",
            Self::Implementation => "Implementation source",
        }
    }
}

impl SourceVerified {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ExactMatch => "exact_match",
            Self::Match => "match",
            Self::None => "none",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct PowerFacts {
    pub can_seize: Vec<Reason>,
    pub can_block: Vec<Reason>,
    pub can_change_rules: Vec<Reason>,
    pub source_target: Option<String>,
    pub source_is_proxy: bool,
    pub unavailable: Vec<Reason>,
    pub transient_failure: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PowersRecord {
    pub chain: Chain,
    pub contract: String,
    pub can_seize: Vec<Reason>,
    pub can_block: Vec<Reason>,
    pub can_change_rules: Vec<Reason>,
    pub unavailable: Vec<Reason>,
    pub source_verified_subject: SourceVerifiedSubject,
    pub source_verified: SourceVerified,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_verified_proxy: Option<SourceVerified>,
    pub observed_at: String,
    pub block: Option<u64>,
    pub slot: Option<u64>,
    pub reads: Vec<Read>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LookupError {
    InvalidAddress,
    NotFound,
    ReadFailed,
}

pub(crate) async fn for_registered(
    state: &AppState,
    address: &str,
    requested_chain: Option<Chain>,
) -> Result<Vec<PowersRecord>, LookupError> {
    let address = address.trim();
    if !(Chain::is_evm_address(address) || Chain::detect(address) == Some(Chain::Solana)) {
        return Err(LookupError::InvalidAddress);
    }
    let chains = {
        let registry = state.registry.read().await;
        [
            Chain::Solana,
            Chain::RobinhoodChain,
            Chain::Ethereum,
            Chain::Bnb,
            Chain::Base,
        ]
        .into_iter()
        .filter(|chain| requested_chain.is_none_or(|requested| requested == *chain))
        .filter(|chain| registry::lookup(&registry, *chain, address).is_some())
        .collect::<Vec<_>>()
    };
    if chains.is_empty() {
        return Err(LookupError::NotFound);
    }
    let mut records = Vec::with_capacity(chains.len());
    for chain in chains {
        records.push(inspect(state, chain, address).await.map_err(|_| LookupError::ReadFailed)?);
    }
    Ok(records)
}

pub(crate) async fn inspect(
    state: &AppState,
    chain: Chain,
    contract: &str,
) -> Result<PowersRecord, PoolError> {
    inspect_with_refresh(state, chain, contract, false).await
}

async fn inspect_for_warm_refresh(
    state: &AppState,
    chain: Chain,
    contract: &str,
) -> Result<PowersRecord, PoolError> {
    inspect_with_refresh(state, chain, contract, true).await
}

async fn inspect_with_refresh(
    state: &AppState,
    chain: Chain,
    contract: &str,
    refresh_stale: bool,
) -> Result<PowersRecord, PoolError> {
    let contract = canonical_contract(chain, contract)?;
    let version = crate::state::current_registry_version();
    let cache_key = (chain, contract.clone(), version);
    if !refresh_stale {
        if let Some(record) = state.powers_cache.get(&cache_key).await {
            return Ok(record);
        }
        if let Some(record) = state.powers_retry_cache.get(&cache_key).await {
            return Ok(record);
        }
    } else if let Some(record) = state.powers_cache.get(&cache_key).await
        && !powers_record_needs_warm_refresh(&record, &Utc::now())
    {
        return Ok(record);
    } else if let Some(record) = state.powers_retry_cache.get(&cache_key).await {
        return Ok(record);
    }
    if state.powers_failure_cache.get(&cache_key).await.is_some() {
        return Err(PoolError::Reader("recent token-power reads are unavailable".to_owned()));
    }
    let lock = state
        .powers_locks
        .get_with(cache_key.clone(), async {
            std::sync::Arc::new(tokio::sync::Mutex::new(()))
        })
        .await;
    let _guard = lock.lock().await;
    if !refresh_stale {
        if let Some(record) = state.powers_cache.get(&cache_key).await {
            return Ok(record);
        }
        if let Some(record) = state.powers_retry_cache.get(&cache_key).await {
            return Ok(record);
        }
    } else if let Some(record) = state.powers_cache.get(&cache_key).await
        && !powers_record_needs_warm_refresh(&record, &Utc::now())
    {
        return Ok(record);
    } else if let Some(record) = state.powers_retry_cache.get(&cache_key).await {
        return Ok(record);
    }
    if state.powers_failure_cache.get(&cache_key).await.is_some() {
        return Err(PoolError::Reader("recent token-power reads are unavailable".to_owned()));
    }
    let reader = state
        .readers
        .iter()
        .find(|reader| reader.chain() == chain)
        .ok_or_else(|| PoolError::Reader(format!("no reader configured for {chain}")))?;
    let (result, read_log) = attest::capture_reads(async {
        let facts = reader.power_facts(&contract).await?;
        let (source_verified, source_verified_proxy) = match chain {
            Chain::Solana => {
                let Some(program_id) = facts.source_target.as_deref() else {
                    return Ok((facts, SourceVerified::Unavailable, None));
                };
                (solana::verify_source(state, program_id).await, None)
            }
            _ if facts.source_is_proxy => {
                let implementation = match facts.source_target.as_deref() {
                    Some(implementation) => {
                        evm::verify_source(state, chain, implementation).await
                    }
                    None => SourceVerified::Unavailable,
                };
                let proxy = evm::verify_source(state, chain, &contract).await;
                (implementation, Some(proxy))
            }
            _ => (evm::verify_source(state, chain, &contract).await, None),
        };
        Ok::<_, PoolError>((facts, source_verified, source_verified_proxy))
    })
    .await;
    let (facts, source_verified, source_verified_proxy) = match result {
        Ok(value) => value,
        Err(error) => {
            state.powers_failure_cache.insert(cache_key, ()).await;
            return Err(error);
        }
    };
    let is_transient = facts.transient_failure;
    let record = PowersRecord {
        chain,
        contract,
        can_seize: facts.can_seize,
        can_block: facts.can_block,
        can_change_rules: facts.can_change_rules,
        unavailable: facts.unavailable,
        source_verified_subject: source_verified_subject(chain, facts.source_is_proxy),
        source_verified,
        source_verified_proxy,
        observed_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        block: read_log.block,
        slot: read_log.slot,
        reads: read_log.reads,
    };

    state.powers_failure_cache.invalidate(&cache_key).await;
    if is_transient {
        state.powers_retry_cache.insert(cache_key, record.clone()).await;
    } else {
        state.powers_retry_cache.invalidate(&cache_key).await;
        state.powers_cache.insert(cache_key, record.clone()).await;
    }
    Ok(record)
}

fn source_verified_subject(chain: Chain, is_proxy: bool) -> SourceVerifiedSubject {
    match chain {
        Chain::Solana => SourceVerifiedSubject::TokenProgram,
        Chain::RobinhoodChain | Chain::Base | Chain::Ethereum | Chain::Bnb if is_proxy => {
            SourceVerifiedSubject::Implementation
        }
        Chain::RobinhoodChain | Chain::Base | Chain::Ethereum | Chain::Bnb => {
            SourceVerifiedSubject::Contract
        }
    }
}

pub(crate) async fn cached_record(
    state: &AppState,
    chain: Chain,
    contract: &str,
) -> Option<PowersRecord> {
    let contract = canonical_contract(chain, contract).ok()?;
    let cache_key = (chain, contract, crate::state::current_registry_version());
    if let Some(record) = state.powers_cache.get(&cache_key).await {
        return Some(record);
    }
    state.powers_retry_cache.get(&cache_key).await
}

pub(crate) async fn inspect_prefetched(
    state: &AppState,
    chain: Chain,
    contract: &str,
) -> Result<PowersRecord, PoolError> {
    let _permit = state
        .powers_prefetch_concurrency
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| PoolError::Reader("powers prefetch semaphore closed".to_owned()))?;
    inspect(state, chain, contract).await
}

pub(crate) async fn schedule_prefetch(state: &AppState, chain: Chain, address: &str) {
    let Ok(contract) = canonical_contract(chain, address) else {
        return;
    };
    let version = crate::state::current_registry_version();
    let key = (chain, contract.clone(), version);
    if state.powers_cache.get(&key).await.is_some()
        || state.powers_retry_cache.get(&key).await.is_some()
        || state.powers_failure_cache.get(&key).await.is_some()
    {
        return;
    }
    {
        let mut in_progress = state.powers_prefetching.lock().await;
        if in_progress.contains(&key) || in_progress.len() >= MAX_QUEUED_PREFETCHES {
            return;
        }
        in_progress.insert(key.clone());
    }
    let state = state.clone();
    tokio::spawn(async move {
        let _ = inspect_prefetched(&state, chain, &contract).await;
        state.powers_prefetching.lock().await.remove(&key);
    });
}
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct WarmChainSummary {
    pub(crate) ok: usize,
    pub(crate) transient: usize,
    pub(crate) source_unavailable: usize,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct WarmPassSummary {
    pub(crate) warmed: bool,
    pub(crate) target_count: usize,
    pub(crate) tickers_covered: usize,
    pub(crate) cap_hit: bool,
    pub(crate) ok: usize,
    pub(crate) transient: usize,
    pub(crate) skipped: usize,
    pub(crate) by_chain: [WarmChainSummary; 5],
    pub(crate) transient_reasons: BTreeMap<String, usize>,
}

#[derive(Debug, Default)]
struct WarmTargetSelection {
    targets: Vec<(Chain, String)>,
    tickers_covered: usize,
    cap_hit: bool,
}

fn powers_record_needs_warm_refresh(
    record: &PowersRecord,
    now: &chrono::DateTime<Utc>,
) -> bool {
    chrono::DateTime::parse_from_rfc3339(&record.observed_at)
        .map(|observed_at| {
            now.signed_duration_since(observed_at.with_timezone(&Utc)).num_seconds()
                >= POWERS_CACHE_REFRESH_AGE.as_secs() as i64
        })
        .unwrap_or(true)
}

fn append_warm_ticker(ticker: &str, seen: &mut HashSet<String>, ordered: &mut Vec<String>) {
    let ticker = ticker.trim().to_ascii_uppercase();
    if !ticker.is_empty() && seen.insert(ticker.clone()) {
        ordered.push(ticker);
    }
}

fn ordered_warm_tickers(
    featured: &[crate::discovery::FeaturedPool],
    leaderboard: &crate::discovery::Leaderboard,
) -> Vec<String> {
    let mut ordered = Vec::new();
    let mut seen = HashSet::new();
    for ticker in featured.iter().filter_map(|pool| pool.ticker.as_deref()) {
        append_warm_ticker(ticker, &mut seen, &mut ordered);
    }
    let mut entries = leaderboard.entries.iter().collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.rank);
    for ticker in entries.iter().filter_map(|entry| entry.ticker.as_deref()) {
        append_warm_ticker(ticker, &mut seen, &mut ordered);
    }
    ordered
}

fn select_warm_targets(
    registry: &registry::Registry,
    tickers: &[String],
    max_contracts: usize,
) -> WarmTargetSelection {
    let mut selection = WarmTargetSelection::default();
    let mut seen_contracts = HashSet::new();
    for ticker in tickers {
        let mut ticker_targets = Vec::new();
        let mut ticker_contracts = HashSet::new();
        for entry in registry.iter().filter(|entry| {
            registry::matchable(entry) && entry.ticker.eq_ignore_ascii_case(ticker)
        }) {
            let Ok(contract) = canonical_contract(entry.chain, &entry.contract) else {
                continue;
            };
            let key = (entry.chain, contract.clone());
            if !seen_contracts.contains(&key) && ticker_contracts.insert(key) {
                ticker_targets.push((entry.chain, contract));
            }
        }
        if ticker_targets.is_empty() {
            continue;
        }
        if selection.targets.len().saturating_add(ticker_targets.len()) > max_contracts {
            selection.cap_hit = true;
            break;
        }
        for (chain, contract) in ticker_targets {
            seen_contracts.insert((chain, contract.clone()));
            selection.targets.push((chain, contract));
        }
        selection.tickers_covered += 1;
    }
    selection
}

async fn current_warm_targets(state: &AppState) -> WarmTargetSelection {
    let featured = state.featured.read().await;
    let leaderboard = state.leaderboard.read().await;
    let tickers = ordered_warm_tickers(&featured, &leaderboard);
    drop(leaderboard);
    drop(featured);
    let registry = state.registry.read().await;
    select_warm_targets(&registry, &tickers, POWERS_WARM_MAX_CONTRACTS)
}

pub(crate) async fn notify_powers_warm_if_targets(state: &AppState, notify: &Notify) -> bool {
    if current_warm_targets(state).await.targets.is_empty() {
        return false;
    }
    notify.notify_one();
    true
}

fn warm_chain_counts_mut(
    summary: &mut WarmPassSummary,
    chain: Chain,
) -> &mut WarmChainSummary {
    &mut summary.by_chain[match chain {
        Chain::Solana => 0,
        Chain::RobinhoodChain => 1,
        Chain::Base => 2,
        Chain::Ethereum => 3,
        Chain::Bnb => 4,
    }]
}

fn record_warm_success(summary: &mut WarmPassSummary, chain: Chain, record: &PowersRecord) {
    summary.ok += 1;
    let counts = warm_chain_counts_mut(summary, chain);
    counts.ok += 1;
    if record.source_verified == SourceVerified::Unavailable
        || record.source_verified_proxy == Some(SourceVerified::Unavailable)
    {
        counts.source_unavailable += 1;
    }
}

fn record_transient_reason(summary: &mut WarmPassSummary, reason: &str) {
    *summary.transient_reasons.entry(reason.to_owned()).or_default() += 1;
}

fn record_warm_transient(summary: &mut WarmPassSummary, chain: Chain, reason: &str) {
    summary.transient += 1;
    warm_chain_counts_mut(summary, chain).transient += 1;
    record_transient_reason(summary, reason);
}

fn record_transient_record(summary: &mut WarmPassSummary, chain: Chain, record: &PowersRecord) {
    let mut found_reason = false;
    let mut seen = HashSet::new();
    for reason in &record.unavailable {
        if !reason.code.is_empty() && seen.insert(reason.code.as_str()) {
            if found_reason {
                record_transient_reason(summary, &reason.code);
            } else {
                record_warm_transient(summary, chain, &reason.code);
                found_reason = true;
            }
        }
    }
    if !found_reason {
        record_warm_transient(summary, chain, "rpc_unavailable");
    }
}

fn transient_pool_error_code(error: &PoolError) -> &'static str {
    match error {
        PoolError::InvalidAddress => "invalid_address",
        PoolError::Unknown(_) => "not_found",
        PoolError::CodeLookupUnsupported => "unsupported",
        PoolError::BudgetExceeded("deadline") => "rpc_deadline",
        PoolError::BudgetExceeded(_) => "budget_exceeded",
        PoolError::Reader(_) => "rpc_unavailable",
    }
}

pub(crate) async fn warm_current_pool_powers(state: &AppState) -> WarmPassSummary {
    let started = Instant::now();
    let selection = current_warm_targets(state).await;
    let mut summary = WarmPassSummary {
        warmed: !selection.targets.is_empty(),
        target_count: selection.targets.len(),
        tickers_covered: selection.tickers_covered,
        cap_hit: selection.cap_hit,
        ..WarmPassSummary::default()
    };
    let version = crate::state::current_registry_version();
    for (chain, contract) in selection.targets {
        let key = (chain, contract.clone(), version);
        let cached_record = state.powers_cache.get(&key).await;
        let refresh_cached = cached_record
            .as_ref()
            .is_some_and(|record| powers_record_needs_warm_refresh(record, &Utc::now()));
        if cached_record.is_some() && !refresh_cached {
            summary.skipped += 1;
            continue;
        }
        if state.powers_retry_cache.get(&key).await.is_some()
            || state.powers_failure_cache.get(&key).await.is_some()
        {
            summary.skipped += 1;
            continue;
        }

        let Some(_permit) = warm_permit(state).await else {
            record_warm_transient(&mut summary, chain, "prefetch_semaphore_closed");
            continue;
        };
        if refresh_cached
            && state
                .powers_cache
                .get(&key)
                .await
                .is_some_and(|record| !powers_record_needs_warm_refresh(&record, &Utc::now()))
        {
            summary.skipped += 1;
            continue;
        }
        match inspect_for_warm_refresh(state, chain, &contract).await {
            Ok(record) => {
                let current_key =
                    (chain, contract.clone(), crate::state::current_registry_version());
                if let Some(record) = state.powers_retry_cache.get(&current_key).await {
                    record_transient_record(&mut summary, chain, &record);
                } else if let Some(record) = state.powers_cache.get(&current_key).await {
                    record_warm_success(&mut summary, chain, &record);
                } else {
                    let reason = if crate::state::current_registry_version() != version {
                        "registry_changed"
                    } else if record.source_verified == SourceVerified::Unavailable
                        || record.source_verified_proxy == Some(SourceVerified::Unavailable)
                    {
                        "source_unavailable"
                    } else {
                        "rpc_unavailable"
                    };
                    record_warm_transient(&mut summary, chain, reason);
                }
            }
            Err(error) => record_warm_transient(
                &mut summary,
                chain,
                transient_pool_error_code(&error),
            ),
        }
    }

    let mut top_reasons = summary
        .transient_reasons
        .iter()
        .map(|(reason, count)| (reason.as_str(), *count))
        .collect::<Vec<_>>();
    top_reasons.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(right.0)));
    top_reasons.truncate(5);
    info!(
        warmed = summary.warmed,
        targets = summary.target_count,
        tickers_covered = summary.tickers_covered,
        cap_hit = summary.cap_hit,
        ok = summary.ok,
        transient = summary.transient,
        skipped = summary.skipped,
        solana_ok = summary.by_chain[0].ok,
        solana_transient = summary.by_chain[0].transient,
        solana_source_unavailable = summary.by_chain[0].source_unavailable,
        robinhood_ok = summary.by_chain[1].ok,
        robinhood_transient = summary.by_chain[1].transient,
        robinhood_source_unavailable = summary.by_chain[1].source_unavailable,
        base_ok = summary.by_chain[2].ok,
        base_transient = summary.by_chain[2].transient,
        base_source_unavailable = summary.by_chain[2].source_unavailable,
        ethereum_ok = summary.by_chain[3].ok,
        ethereum_transient = summary.by_chain[3].transient,
        ethereum_source_unavailable = summary.by_chain[3].source_unavailable,
        bnb_ok = summary.by_chain[4].ok,
        bnb_transient = summary.by_chain[4].transient,
        bnb_source_unavailable = summary.by_chain[4].source_unavailable,
        top_transient_reasons = ?top_reasons,
        elapsed_ms = started.elapsed().as_millis(),
        "powers warm pass completed"
    );
    summary
}

async fn warm_permit(state: &AppState) -> Option<tokio::sync::OwnedSemaphorePermit> {
    loop {
        match state.powers_prefetch_concurrency.clone().try_acquire_owned() {
            Ok(permit) => return Some(permit),
            Err(tokio::sync::TryAcquireError::NoPermits) => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(tokio::sync::TryAcquireError::Closed) => return None,
        }
    }
}



fn canonical_contract(chain: Chain, address: &str) -> Result<String, PoolError> {
    if chain == Chain::Solana {
        let bytes = bs58::decode(address).into_vec().map_err(|_| PoolError::InvalidAddress)?;
        if bytes.len() != 32 {
            return Err(PoolError::InvalidAddress);
        }
        return Ok(bs58::encode(bytes).into_string());
    }
    if !Chain::is_evm_address(address) {
        return Err(PoolError::InvalidAddress);
    }
    Ok(address.to_ascii_lowercase())
}

pub(crate) async fn source_json(
    state: &AppState,
    url: &str,
    allowed_host: &str,
) -> Option<(StatusCode, Value)> {
    let allowed_url = crate::net::allowlisted_https_url(url, allowed_host)?;
    let request_url = allowed_url.as_str().to_owned();
    let response = state.source_http.get(allowed_url).timeout(Duration::from_secs(5)).send().await;
    let Ok(response) = response else {
        let unavailable = json!({"available": false});
        attest::record_read("GET", json!([request_url]), &unavailable, false, None, None);
        return None;
    };
    let status = response.status();
    let value = match crate::net::body(response).await {
        Ok(bytes) => serde_json::from_slice::<Value>(&bytes)
            .unwrap_or_else(|_| json!({"http_status": status.as_u16()})),
        Err(_) => json!({"http_status": status.as_u16(), "available": false}),
    };
    attest::record_read("GET", json!([request_url]), &value, false, None, None);
    Some((status, value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalizes_contract_addresses_by_chain() {
        assert_eq!(
            canonical_contract(Chain::Ethereum, "0xAa00000000000000000000000000000000000001")
                .unwrap(),
            "0xaa00000000000000000000000000000000000001"
        );
        assert_eq!(
            canonical_contract(Chain::Solana, "11111111111111111111111111111111").unwrap(),
            "11111111111111111111111111111111"
        );
        assert!(canonical_contract(Chain::Solana, "not-a-public-key").is_err());
    }
    #[test]
    fn source_verification_subject_names_the_evidence_target() {
        assert_eq!(
            source_verified_subject(Chain::Solana, false),
            SourceVerifiedSubject::TokenProgram
        );
        assert_eq!(
            source_verified_subject(Chain::RobinhoodChain, false),
            SourceVerifiedSubject::Contract
        );
        assert_eq!(
            source_verified_subject(Chain::Base, true),
            SourceVerifiedSubject::Implementation
        );
        assert_eq!(
            serde_json::to_value(SourceVerifiedSubject::TokenProgram).unwrap(),
            "token_program"
        );
        assert_eq!(
            serde_json::to_value(SourceVerifiedSubject::Contract).unwrap(),
            "contract"
        );
        assert_eq!(
            serde_json::to_value(SourceVerifiedSubject::Implementation).unwrap(),
            "implementation"
        );
    }

    fn warm_registry_entry(chain: Chain, ticker: &str, contract: &str) -> registry::Entry {
        registry::Entry {
            issuer: "Issuer".to_owned(),
            ticker: ticker.to_owned(),
            name: ticker.to_owned(),
            chain,
            contract: contract.to_owned(),
            decimals: None,
            source: "test".to_owned(),
            source_url: "https://issuer.example/token".to_owned(),
            last_checked: registry::now_rfc3339(),
            removed_at: None,
            stale_since: None,
        }
    }

    fn warm_featured_pool(ticker: &str) -> crate::discovery::FeaturedPool {
        crate::discovery::FeaturedPool {
            chain: Chain::Solana,
            dex: "raydium".to_owned(),
            pool: format!("pool-{ticker}"),
            base_symbol: ticker.to_owned(),
            base_address: "11111111111111111111111111111111".to_owned(),
            quote_symbol: "USDC".to_owned(),
            quote_address: "So11111111111111111111111111111111111111112".to_owned(),
            issuer: Some("Issuer".to_owned()),
            ticker: Some(ticker.to_owned()),
            verdict: "verified".to_owned(),
            quote_balance: None,
            quote_share_of_supply: None,
            volume_24h_usd: None,
            liquidity_usd: None,
            curated: false,
            note: None,
            updated_at: registry::now_rfc3339(),
        }
    }

    fn warm_leaderboard_entry(rank: usize, ticker: &str) -> crate::discovery::LeaderboardEntry {
        crate::discovery::LeaderboardEntry {
            rank,
            chain: "base".to_owned(),
            chain_label: "Base".to_owned(),
            dex: "uniswap-v3".to_owned(),
            pool: format!("pool-{ticker}"),
            base_symbol: ticker.to_owned(),
            quote_symbol: "USDC".to_owned(),
            issuer: Some("Issuer".to_owned()),
            ticker: Some(ticker.to_owned()),
            verdict: "verified".to_owned(),
            price_usd: None,
            change_24h_pct: None,
            volume_24h_usd: None,
            liquidity_usd: None,
            txns_24h: None,
            detail_url: "/validated/base/pool".to_owned(),
            trade_url: "https://dexscreener.com/base/pool".to_owned(),
            explorer_url: "https://basescan.org/address/pool".to_owned(),
            attestation_id: None,
            checked_at: None,
        }
    }

    #[test]
    fn warm_targets_prioritize_featured_then_rank_and_keep_ticker_groups() {
        let featured = vec![warm_featured_pool("FEAT")];
        let mut leaderboard = crate::discovery::Leaderboard::default();
        leaderboard.entries = vec![
            warm_leaderboard_entry(2, "LATER"),
            warm_leaderboard_entry(1, "FIRST"),
            warm_leaderboard_entry(3, "FEAT"),
        ];
        let tickers = ordered_warm_tickers(&featured, &leaderboard);
        assert_eq!(tickers, ["FEAT", "FIRST", "LATER"]);

        let registry = vec![
            warm_registry_entry(
                Chain::Base,
                "LATER",
                "0x0000000000000000000000000000000000000003",
            ),
            warm_registry_entry(
                Chain::Base,
                "FIRST",
                "0x0000000000000000000000000000000000000001",
            ),
            warm_registry_entry(
                Chain::Ethereum,
                "FIRST",
                "0x0000000000000000000000000000000000000002",
            ),
            warm_registry_entry(
                Chain::Solana,
                "FEAT",
                "11111111111111111111111111111111",
            ),
        ];
        let selection = select_warm_targets(&registry, &tickers, 3);
        assert_eq!(
            selection.targets,
            [
                (Chain::Solana, "11111111111111111111111111111111".to_owned()),
                (Chain::Base, "0x0000000000000000000000000000000000000001".to_owned()),
                (Chain::Ethereum, "0x0000000000000000000000000000000000000002".to_owned()),
            ]
        );
        assert_eq!(selection.tickers_covered, 2);
        let exact_fill = select_warm_targets(&registry, &tickers[..2], 3);
        assert_eq!(exact_fill.targets.len(), 3);
        assert!(!exact_fill.cap_hit);
        assert!(selection.cap_hit);
    }

    #[test]
    fn transient_reason_codes_use_typed_pool_errors() {
        assert_eq!(
            transient_pool_error_code(&PoolError::BudgetExceeded("deadline")),
            "rpc_deadline"
        );
        assert_eq!(
            transient_pool_error_code(&PoolError::Reader(
                "request https://rpc.example/?timeout=5000 returned error".to_owned()
            )),
            "rpc_unavailable"
        );
    }
}
