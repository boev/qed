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
use std::time::Duration;

pub(crate) mod evm;
pub(crate) mod solana;

const MAX_QUEUED_PREFETCHES: usize = 64;

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
        if let Ok(_permit) = state.powers_prefetch_concurrency.clone().acquire_owned().await {
            let _ = inspect(&state, chain, &contract).await;
        }
        state.powers_prefetching.lock().await.remove(&key);
    });
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
