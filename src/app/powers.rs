use crate::{
    app::context::Context,
    domain::{
        chain::Chain,
        pool::PoolError,
        powers::{PowersRecord, SourceVerified, SourceVerifiedSubject},
        registry,
    },
    ports::capture_reads,
};
use chrono::SecondsFormat;
use std::sync::atomic::Ordering;

const MAX_QUEUED_PREFETCHES: usize = 64;
const POWERS_DEADLINE: std::time::Duration = std::time::Duration::from_secs(15);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LookupError {
    InvalidAddress,
    NotFound,
    ReadFailed,
    DeadlineExceeded,
}

pub(crate) async fn for_any(
    state: &Context,
    address: &str,
    requested_chain: Option<Chain>,
) -> Result<Vec<PowersRecord>, LookupError> {
    if address.len() > Chain::MAX_SOLANA_ADDRESS_CHARS {
        return Err(LookupError::InvalidAddress);
    }
    tokio::time::timeout(POWERS_DEADLINE, for_any_inner(state, address, requested_chain))
        .await
        .map_err(|_| LookupError::DeadlineExceeded)?
}

async fn for_any_inner(
    state: &Context,
    address: &str,
    requested_chain: Option<Chain>,
) -> Result<Vec<PowersRecord>, LookupError> {
    let address = address.trim();
    if !(Chain::is_evm_address(address) || Chain::detect(address) == Some(Chain::Solana)) {
        return Err(LookupError::InvalidAddress);
    }
    if requested_chain.is_some_and(|chain| canonical_contract(chain, address).is_err()) {
        return Err(LookupError::InvalidAddress);
    }
    let mut chains = if let Some(chain) = requested_chain {
        vec![chain]
    } else {
        let registry = state.registry.snapshot().await;
        [Chain::Solana, Chain::RobinhoodChain, Chain::Ethereum, Chain::Bnb, Chain::Base]
            .into_iter()
            .filter(|chain| registry::lookup(&registry, *chain, address).is_some())
            .collect::<Vec<_>>()
    };
    if chains.is_empty() {
        if Chain::detect(address) == Some(Chain::Solana) {
            chains.push(Chain::Solana);
        } else {
            let readers = state
                .readers
                .iter()
                .map(|reader| reader.as_ref() as &dyn crate::ports::ChainReader)
                .collect::<Vec<_>>();
            chains.push(crate::app::check::detect_evm_chain(address, &readers).await.map_err(
                |error| match error {
                    PoolError::InvalidAddress => LookupError::InvalidAddress,
                    _ => LookupError::NotFound,
                },
            )?);
        }
    }
    let _permit = state
        .powers_prefetch_concurrency
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| LookupError::ReadFailed)?;
    let mut records = Vec::with_capacity(chains.len());
    for chain in chains {
        records.push(inspect(state, chain, address).await.map_err(|_| LookupError::ReadFailed)?);
    }
    Ok(records)
}

pub(crate) async fn inspect(
    state: &Context,
    chain: Chain,
    contract: &str,
) -> Result<PowersRecord, PoolError> {
    inspect_with_refresh(state, chain, contract, false).await
}

pub(crate) async fn inspect_for_warm_refresh(
    state: &Context,
    chain: Chain,
    contract: &str,
) -> Result<PowersRecord, PoolError> {
    inspect_with_refresh(state, chain, contract, true).await
}

async fn inspect_with_refresh(
    state: &Context,
    chain: Chain,
    contract: &str,
    refresh_stale: bool,
) -> Result<PowersRecord, PoolError> {
    let contract = canonical_contract(chain, contract)?;
    let version = state.registry_version.load(Ordering::Acquire);
    let cache_key = (chain, contract.clone(), version);
    if !refresh_stale {
        if let Some(record) = state.powers_cache.get(&cache_key).await {
            return Ok(record);
        }
        if let Some(record) = state.powers_retry_cache.get(&cache_key).await {
            return Ok(record);
        }
    } else if let Some(record) = state.powers_cache.get(&cache_key).await
        && !crate::app::warm::powers_record_needs_warm_refresh(&record, &state.clock.now())
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
        .get_or_insert(cache_key.clone(), std::sync::Arc::new(tokio::sync::Mutex::new(())))
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
        && !crate::app::warm::powers_record_needs_warm_refresh(&record, &state.clock.now())
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
    let (result, read_log) = capture_reads(async {
        let facts = reader.power_facts(&contract).await?;
        let (source_verified, source_verified_proxy) = match chain {
            Chain::Solana => {
                let Some(program_id) = facts.source_target.as_deref() else {
                    return Ok((facts, SourceVerified::Unavailable, None));
                };
                (state.source_verifier.verify_solana(program_id).await, None)
            }
            _ if facts.source_is_proxy => {
                let implementation = match facts.source_target.as_deref() {
                    Some(implementation) => {
                        state.source_verifier.verify_evm(chain, implementation).await
                    }
                    None => SourceVerified::Unavailable,
                };
                let proxy = state.source_verifier.verify_evm(chain, &contract).await;
                (implementation, Some(proxy))
            }
            _ => (state.source_verifier.verify_evm(chain, &contract).await, None),
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
        token_paused: facts.token_paused,
        sanctions_list: facts.sanctions_list,
        unavailable: facts.unavailable,
        source_verified_subject: source_verified_subject(chain, facts.source_is_proxy),
        source_verified,
        source_verified_proxy,
        observed_at: state.clock.now().to_rfc3339_opts(SecondsFormat::Secs, true),
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
    state: &Context,
    chain: Chain,
    contract: &str,
) -> Option<PowersRecord> {
    let contract = canonical_contract(chain, contract).ok()?;
    let cache_key = (chain, contract, state.registry_version.load(Ordering::Acquire));
    if let Some(record) = state.powers_cache.get(&cache_key).await {
        return Some(record);
    }
    state.powers_retry_cache.get(&cache_key).await
}

pub(crate) async fn inspect_prefetched(
    state: &Context,
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

pub(crate) async fn schedule_prefetch(state: &Context, chain: Chain, address: &str) {
    let Ok(contract) = canonical_contract(chain, address) else {
        return;
    };
    let version = state.registry_version.load(Ordering::Acquire);
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

pub(crate) fn canonical_contract(chain: Chain, address: &str) -> Result<String, PoolError> {
    if chain == Chain::Solana {
        if address.len() > Chain::MAX_SOLANA_ADDRESS_CHARS {
            return Err(PoolError::InvalidAddress);
        }
        let bytes = Chain::decode_solana_address(address).ok_or(PoolError::InvalidAddress)?;
        return Ok(bs58::encode(bytes).into_string());
    }
    if address.len() != 42 || !Chain::is_evm_address(address) {
        return Err(PoolError::InvalidAddress);
    }
    Ok(address.to_ascii_lowercase())
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
        assert_eq!(serde_json::to_value(SourceVerifiedSubject::Contract).unwrap(), "contract");
        assert_eq!(
            serde_json::to_value(SourceVerifiedSubject::Implementation).unwrap(),
            "implementation"
        );
    }
}
