use crate::{
    app::context::{CachedNoContract, Context},
    domain::{
        chain::Chain,
        pool::PoolError,
        powers::{PowersRecord, SourceVerified, SourceVerifiedSubject},
        registry,
    },
    ports::{
        capture_reads, current_rpc_request_context, rpc_budget_rejected, rpc_crawler_blocked,
        with_rpc_request_context,
    },
};
use chrono::SecondsFormat;

const MAX_QUEUED_PREFETCHES: usize = 64;
const POWERS_DEADLINE: std::time::Duration = std::time::Duration::from_secs(15);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LookupError {
    InvalidAddress,
    NotFound,
    ReadFailed,
    DeadlineExceeded,
    DailyBudgetExceeded,
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
            chains.push(
                crate::app::check::detect_evm_chain(
                    state,
                    address,
                    crate::domain::asset_history::SubjectKind::Token,
                    crate::domain::asset_history::Facet::Powers,
                )
                .await
                .map_err(|error| {
                    if matches!(&error, PoolError::InvalidAddress) {
                        LookupError::InvalidAddress
                    } else if error.is_daily_budget_exceeded() {
                        LookupError::DailyBudgetExceeded
                    } else {
                        LookupError::NotFound
                    }
                })?,
            );
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
        records.push(inspect(state, chain, address).await.map_err(|error| {
            if error.is_daily_budget_exceeded() || rpc_budget_rejected() {
                LookupError::DailyBudgetExceeded
            } else {
                LookupError::ReadFailed
            }
        })?);
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
    if let Some(asset) = crate::domain::asset_history::AssetKey::new(
        chain,
        crate::domain::asset_history::SubjectKind::Token,
        &contract,
    ) && crate::app::asset_history::already_missed_slot(
        state,
        &asset,
        crate::domain::asset_history::Facet::Powers,
    )
    .is_some()
    {
        return Err(PoolError::Reader("scheduled retry already attempted this hour".into()));
    }
    let cache_key = (chain, contract.clone());
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
    let guard = lock.lock_owned().await;
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
    let state = state.clone();
    crate::app::asset_history::finish_execution(async move {
        let _guard = guard;
        let state = &state;
        let reader = state
            .readers
            .iter()
            .find(|reader| reader.chain() == chain)
            .ok_or_else(|| PoolError::Reader(format!("no reader configured for {chain}")))?;
        let (result, read_log) = tokio::time::timeout(
            std::time::Duration::from_secs(14),
            capture_reads(async {
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
            }),
        )
        .await
        .unwrap_or_else(|_| {
            (
                Err(PoolError::Reader("token controls execution timed out".into())),
                crate::ports::ReadLog::default(),
            )
        });
        let (facts, source_verified, source_verified_proxy) = match result {
            Ok(value) => value,
            Err(error) => {
                if !rpc_crawler_blocked()
                    && let Some(asset) = crate::domain::asset_history::AssetKey::new(
                        chain,
                        crate::domain::asset_history::SubjectKind::Token,
                        &contract,
                    )
                {
                    crate::app::asset_history::miss(
                        state,
                        asset,
                        crate::domain::asset_history::Facet::Powers,
                        if error.is_daily_budget_exceeded() {
                            "budget_rejection"
                        } else {
                            "read_failure"
                        },
                    )
                    .await;
                }
                if !error.is_daily_budget_exceeded() && !rpc_crawler_blocked() {
                    state.powers_failure_cache.insert(cache_key, ()).await;
                }
                return Err(error);
            }
        };
        if rpc_budget_rejected() {
            if let Some(asset) = crate::domain::asset_history::AssetKey::new(
                chain,
                crate::domain::asset_history::SubjectKind::Token,
                &contract,
            ) {
                crate::app::asset_history::miss(
                    state,
                    asset,
                    crate::domain::asset_history::Facet::Powers,
                    "budget_rejection",
                )
                .await;
            }
            return Err(PoolError::DailyBudgetExceeded);
        }
        if rpc_crawler_blocked() {
            return Err(PoolError::Reader("crawler cache miss".to_owned()));
        }
        let cache_complete = facts.unavailable.is_empty() && !facts.transient_failure;
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

        crate::app::asset_history::powers(state, &record, cache_complete).await;
        state.powers_failure_cache.invalidate(&cache_key).await;
        if !cache_complete {
            state.powers_retry_cache.insert(cache_key, record.clone()).await;
        } else {
            state.powers_retry_cache.invalidate(&cache_key).await;
            state.powers_cache.insert(cache_key, record.clone()).await;
        }
        Ok(record)
    })
    .await
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
    let cache_key = (chain, contract);
    if let Some(record) = state.powers_cache.get(&cache_key).await {
        return Some(record);
    }
    state.powers_retry_cache.get(&cache_key).await
}

pub(crate) async fn confirmed_no_contract(
    state: &Context,
    chain: Chain,
    address: &str,
) -> Result<Option<CachedNoContract>, PoolError> {
    if chain == Chain::Solana {
        return Ok(None);
    }
    let contract = canonical_contract(chain, address)?;
    let key = (chain, contract.clone());
    if let Some(cached) = state.no_code_cache.get(&key).await {
        return Ok(Some(cached));
    }
    let reader = state
        .readers
        .iter()
        .find(|reader| reader.chain() == chain)
        .ok_or_else(|| PoolError::Reader(format!("no reader configured for {chain}")))?;
    let (result, read_log) = capture_reads(reader.code_at(&contract)).await;
    let code = result?;
    if code.is_empty() {
        let evidence = CachedNoContract { read_log, observed_at: state.clock.now() };
        state.no_code_cache.insert(key, evidence.clone()).await;
        Ok(Some(evidence))
    } else {
        Ok(None)
    }
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
    let key = (chain, contract.clone());
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
    let request_context = current_rpc_request_context();
    let state = state.clone();
    let future = async move {
        let _ = inspect_prefetched(&state, chain, &contract).await;
        state.powers_prefetching.lock().await.remove(&key);
    };
    if let Some(context) = request_context {
        tokio::spawn(async move { with_rpc_request_context(context, future).await });
    } else {
        tokio::spawn(future);
    }
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

    #[tokio::test]
    async fn cached_powers_survive_a_registry_change() {
        let state = crate::app::context::test_support::build(Vec::new(), Vec::new(), false);
        let contract = "0x00000000000000000000000000000000000000aa";
        let record = PowersRecord {
            chain: Chain::Base,
            contract: contract.to_owned(),
            can_seize: Vec::new(),
            can_block: Vec::new(),
            can_change_rules: Vec::new(),
            token_paused: Some(false),
            sanctions_list: None,
            unavailable: Vec::new(),
            source_verified_subject: SourceVerifiedSubject::Contract,
            source_verified: SourceVerified::ExactMatch,
            source_verified_proxy: None,
            observed_at: "2026-10-15T00:00:00Z".to_owned(),
            block: Some(1),
            slot: None,
            reads: Vec::new(),
        };
        state.powers_cache.insert((Chain::Base, contract.to_owned()), record.clone()).await;
        state.registry_version.fetch_add(1, std::sync::atomic::Ordering::AcqRel);

        // No chain reader is configured: a cache miss would fail, so success proves reuse.
        let reused = inspect(&state, Chain::Base, contract).await.expect("cached powers reused");
        assert_eq!(reused, record);
    }
}
