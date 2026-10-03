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
    collections::HashSet,
    time::{Duration, Instant},
};
use tracing::info;

pub(crate) mod evm;
pub(crate) mod solana;

const MAX_QUEUED_PREFETCHES: usize = 64;
pub(crate) const POWERS_CACHE_TTL: Duration = Duration::from_secs(30 * 60);
pub(crate) const POWERS_WARM_INTERVAL: Duration = Duration::from_secs(25 * 60);
const POWERS_CACHE_REFRESH_AGE: Duration = Duration::from_secs(
    POWERS_CACHE_TTL.as_secs() - POWERS_WARM_INTERVAL.as_secs(),
);

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
    let contract = canonical_contract(chain, contract)?;
    let version = crate::state::current_registry_version();
    let cache_key = (chain, contract.clone(), version);
    if let Some(record) = state.powers_cache.get(&cache_key).await {
        return Ok(record);
    }
    if let Some(record) = state.powers_retry_cache.get(&cache_key).await {
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
    if let Some(record) = state.powers_cache.get(&cache_key).await {
        return Ok(record);
    }
    if let Some(record) = state.powers_retry_cache.get(&cache_key).await {
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
    let is_transient = facts.transient_failure
        || source_verified == SourceVerified::Unavailable
        || source_verified_proxy == Some(SourceVerified::Unavailable);
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

    if version == crate::state::current_registry_version() {
        if is_transient {
            state.powers_retry_cache.insert(cache_key, record.clone()).await;
        } else {
            state.powers_cache.insert(cache_key, record.clone()).await;
        }
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
pub(crate) struct WarmPassSummary {
    pub(crate) ok: usize,
    pub(crate) transient: usize,
    pub(crate) skipped: usize,
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

pub(crate) async fn warm_current_pool_powers(state: &AppState) -> WarmPassSummary {
    let started = Instant::now();
    let mut tickers = HashSet::new();
    {
        let leaderboard = state.leaderboard.read().await;
        tickers.extend(
            leaderboard
                .entries
                .iter()
                .filter_map(|entry| entry.ticker.as_deref())
                .map(|ticker| ticker.to_ascii_uppercase()),
        );
    }
    {
        let featured = state.featured.read().await;
        tickers.extend(
            featured
                .iter()
                .filter_map(|pool| pool.ticker.as_deref())
                .map(|ticker| ticker.to_ascii_uppercase()),
        );
    }

    let targets = {
        let registry = state.registry.read().await;
        let mut seen = HashSet::new();
        let mut targets = Vec::new();
        for entry in registry.iter().filter(|entry| registry::matchable(entry)) {
            if !tickers.contains(&entry.ticker.to_ascii_uppercase()) {
                continue;
            }
            let Ok(contract) = canonical_contract(entry.chain, &entry.contract) else {
                continue;
            };
            if seen.insert((entry.chain, contract.clone())) {
                targets.push((entry.chain, contract));
            }
        }
        targets
    };

    let mut summary = WarmPassSummary::default();
    let version = crate::state::current_registry_version();
    for (chain, contract) in targets {
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
            summary.transient += 1;
            continue;
        };
        if refresh_cached {
            if state
                .powers_cache
                .get(&key)
                .await
                .is_some_and(|record| !powers_record_needs_warm_refresh(&record, &Utc::now()))
            {
                summary.skipped += 1;
                continue;
            }
            state.powers_cache.invalidate(&key).await;
        }
        match inspect(state, chain, &contract).await {
            Ok(_) => {
                let current_key = (chain, contract, crate::state::current_registry_version());
                if state.powers_cache.get(&current_key).await.is_some() {
                    summary.ok += 1;
                } else {
                    summary.transient += 1;
                }
            }
            Err(_) => summary.transient += 1,
        }

    }

    info!(
        ok = summary.ok,
        transient = summary.transient,
        skipped = summary.skipped,
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
}
