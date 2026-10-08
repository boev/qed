use crate::app::context::{CachedCheckResult, Context};
use crate::domain::chain::Chain;
use crate::domain::check::{
    CheckReadIssue, CheckResult, Verdict, bytecode_similarity, cache_key, decimal_cmp,
    evaluate_pool, registered_token_address, same_contract, selected_sides,
};

#[cfg(test)]
use crate::domain::check::{
    claims_name, claims_symbol, claims_ticker, quote_share_of_supply,
    quote_share_unavailable_reason,
};
use crate::domain::pool::{PoolError, PoolInfo, TokenMeta, TokenSide};
use crate::domain::registry::{Entry, Registry};
use crate::ports::ChainReader;
use chrono::{SecondsFormat, Utc};
use std::sync::atomic::Ordering;
pub(crate) async fn detect_evm_chain(
    address: &str,
    readers: &[&dyn ChainReader],
) -> Result<Chain, PoolError> {
    if !Chain::is_evm_address(address) {
        return Err(PoolError::InvalidAddress);
    }
    for chain in [Chain::RobinhoodChain, Chain::Base, Chain::Ethereum, Chain::Bnb] {
        for reader in readers.iter().filter(|reader| reader.chain() == chain) {
            if let Ok(code) = reader.code_at(address).await
                && !code.is_empty()
            {
                return Ok(chain);
            }
        }
    }
    Err(PoolError::Unknown(
        "address has no contract bytecode on configured EVM providers".to_owned(),
    ))
}

fn public_pool_error(error: &PoolError) -> &'static str {
    match error {
        PoolError::InvalidAddress => "unsupported pool address",
        PoolError::Unknown(_) => "pool was not found",
        PoolError::UnsupportedVenue(_) => "unsupported venue",
        PoolError::RpcLimit(_) => "RPC query limit reached",
        PoolError::CodeLookupUnsupported => "contract lookup is unavailable",
        PoolError::BudgetExceeded(_) => "bounded pool discovery was exhausted",
        PoolError::Reader(_) => "RPC provider request failed",
    }
}

struct CheckFailure {
    reason: String,
    issue: CheckReadIssue,
}

impl CheckFailure {
    fn unsupported(reason: impl Into<String>) -> Self {
        Self { reason: reason.into(), issue: CheckReadIssue::Unsupported }
    }

    fn from_pool_error(error: PoolError) -> Self {
        let issue = match &error {
            PoolError::UnsupportedVenue(_) => CheckReadIssue::UnsupportedVenue,
            PoolError::RpcLimit(_) | PoolError::BudgetExceeded(_) => CheckReadIssue::RpcLimit,
            PoolError::Reader(_) | PoolError::CodeLookupUnsupported => CheckReadIssue::Transient,
            PoolError::InvalidAddress | PoolError::Unknown(_) => CheckReadIssue::Unsupported,
        };
        let reason = match error {
            PoolError::Unknown(reason) | PoolError::UnsupportedVenue(reason) => reason,
            error => public_pool_error(&error).to_owned(),
        };
        Self { reason, issue }
    }

    fn from_pool_error_ref(error: &PoolError) -> Self {
        let issue = match error {
            PoolError::UnsupportedVenue(_) => CheckReadIssue::UnsupportedVenue,
            PoolError::RpcLimit(_) | PoolError::BudgetExceeded(_) => CheckReadIssue::RpcLimit,
            PoolError::Reader(_) | PoolError::CodeLookupUnsupported => CheckReadIssue::Transient,
            PoolError::InvalidAddress | PoolError::Unknown(_) => CheckReadIssue::Unsupported,
        };
        let reason = match error {
            PoolError::Unknown(reason) | PoolError::UnsupportedVenue(reason) => reason.clone(),
            other => public_pool_error(other).to_owned(),
        };
        Self { reason, issue }
    }
}

/// Check an address against the configured readers and issuer registry.
/// Results are kept in the application cache for thirty seconds. EVM
/// addresses use a canonical lower-case key; Solana addresses remain
/// case-sensitive.
pub async fn check(state: &Context, address: &str) -> CheckResult {
    let cache_key = cache_key(address);
    let leader_sender = loop {
        let registry_version = state.registry_version.load(Ordering::Acquire);
        if let Some(cached) = state.check_cache.get(&cache_key).await
            && cached.registry_version == registry_version
        {
            return cached.result;
        }
        let mut in_flight = state.check_inflight.lock().await;
        if let Some(sender) = in_flight.get(&cache_key).and_then(std::sync::Weak::upgrade) {
            let mut receiver = sender.subscribe();
            drop(sender);
            drop(in_flight);
            let _ = receiver.changed().await;
            continue;
        }
        in_flight.remove(&cache_key);
        let (sender, _) = tokio::sync::watch::channel(None);
        let sender = std::sync::Arc::new(sender);
        in_flight.insert(cache_key.clone(), std::sync::Arc::downgrade(&sender));
        break sender;
    };
    let registry_version = state.registry_version.load(Ordering::Acquire);

    let registry = state.registry.snapshot().await;
    let (mut result, read_log) =
        crate::ports::capture_reads(check_uncached(state, address, &registry)).await;
    if let Some(attestation) =
        crate::app::attestation::create_for_check(state, &registry, &result, read_log).await
    {
        result.attestation_id = Some(attestation.id);
    }
    if let Some(pool) = result.pool.as_ref() {
        let token_address = registered_token_address(pool, &registry).map(str::to_owned);
        if let Some(token_address) = token_address {
            result.powers =
                crate::app::powers::inspect(state, result.chain, &token_address).await.ok();
        }
    }

    // A refresh advances the version while holding the registry write lock.
    // Do not repopulate the cache with a check that crossed that replacement.
    if registry_version == state.registry_version.load(Ordering::Acquire) {
        state
            .check_cache
            .insert(
                cache_key.clone(),
                CachedCheckResult { registry_version, result: result.clone() },
            )
            .await;
    }
    let _ = leader_sender.send(Some(result.clone()));
    let mut in_flight = state.check_inflight.lock().await;
    in_flight.remove(&cache_key);
    result
}

const MAX_INDEXED_POOL_CANDIDATES: usize = 16;
const INDEXED_POOL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

async fn indexed_pool_candidates(state: &Context, chain: Chain, token: &str) -> Vec<String> {
    let attestations = match state.attestations.read() {
        Ok(attestations) => attestations.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    };
    state
        .pool_index
        .candidate_pools(chain, token, &attestations)
        .await
        .into_iter()
        .take(MAX_INDEXED_POOL_CANDIDATES)
        .collect()
}

async fn check_uncached(state: &Context, address: &str, registry: &Registry) -> CheckResult {
    let mut evidence = Vec::new();
    let (chain, v4_pool) = match resolve_chain(state, address, &mut evidence).await {
        Ok(resolved) => resolved,
        Err(failure) => {
            return unknown_with_issue(address, Chain::RobinhoodChain, failure, evidence);
        }
    };
    let chain_entries =
        registry.iter().filter(|entry| entry.chain == chain).cloned().collect::<Vec<_>>();
    evidence.push(format!("Registry has {} entries for {chain}.", chain_entries.len()));
    if chain_entries.is_empty() {
        return unknown(address, chain, format!("no registry entries for {chain}"), evidence);
    }
    let Some(reader) = state.readers.iter().find(|reader| reader.chain() == chain) else {
        return unknown(address, chain, format!("no pool reader configured for {chain}"), evidence);
    };
    if let Err(error) = reader.record_position().await {
        let failure = CheckFailure::from_pool_error(error);
        evidence.push(format!("chain position read failed: {}.", failure.reason));
        return unknown_with_issue(address, chain, failure, evidence);
    }
    let (pool, input_meta) = match resolve_pool(
        state,
        reader.as_ref(),
        chain,
        &chain_entries,
        address,
        v4_pool,
        &mut evidence,
    )
    .await
    {
        Ok(resolved) => resolved,
        Err(failure) => return unknown_with_issue(address, chain, failure, evidence),
    };

    evidence.push(format!("Detected supported {} pool at {}.", pool.dex, pool.pool));
    let (base_side, quote_side) = selected_sides(&pool, &chain_entries);
    evidence.push(format!(
        "Sides: base {}{}; quote {}{}.",
        base_side.address,
        format_symbol(&base_side.symbol),
        quote_side.address,
        format_symbol(&quote_side.symbol)
    ));
    let base_meta = match metadata_for_side(reader.as_ref(), base_side, input_meta.as_ref()).await {
        Ok(meta) => meta,
        Err(error) => {
            let mut failure = CheckFailure::from_pool_error(error);
            failure.reason = format!("base token metadata read failed: {}", failure.reason);
            evidence.push(format!("{}.", failure.reason));
            return unknown_with_issue(address, chain, failure, evidence);
        }
    };
    let quote_meta = match metadata_for_side(reader.as_ref(), quote_side, input_meta.as_ref()).await
    {
        Ok(meta) => meta,
        Err(error) => {
            let failure = CheckFailure::from_pool_error(error);
            evidence.push(format!("quote token metadata read failed: {}.", failure.reason));
            return unknown_with_issue(address, chain, failure, evidence);
        }
    };
    evidence.push(format!(
        "Token metadata: base symbol {} name {}; quote symbol {} name {}.",
        metadata_symbol(base_meta.as_ref(), base_side),
        metadata_name(base_meta.as_ref()),
        metadata_symbol(quote_meta.as_ref(), quote_side),
        metadata_name(quote_meta.as_ref())
    ));
    let evaluation = evaluate_pool(
        &pool,
        base_side,
        quote_side,
        base_meta.as_ref(),
        quote_meta.as_ref(),
        &chain_entries,
    );
    evidence.extend(evaluation.evidence);

    if let Some(entry) = evaluation.claimed_entry.as_ref()
        && chain != Chain::Solana
    {
        let quote_code = reader.code_at(&evaluation.quote_address).await;
        let registry_code = reader.code_at(&entry.contract).await;
        match (quote_code, registry_code) {
            (Ok(quote_code), Ok(registry_code)) => {
                let similarity = bytecode_similarity(&quote_code, &registry_code) * 100.0;
                evidence.push(format!(
                    "Bytecode similarity between quote {} and {} ({}) is {:.1}%.",
                    evaluation.quote_address, entry.contract, entry.ticker, similarity
                ));
            }
            (Err(error), _) | (_, Err(error)) => {
                let failure = CheckFailure::from_pool_error(error);
                evidence.push(format!("bytecode read failed: {}.", failure.reason));
                return unknown_with_issue(address, chain, failure, evidence);
            }
        }
    }

    CheckResult {
        input: address.to_owned(),
        chain,
        pool: Some(pool),
        verdict: evaluation.verdict,
        quote_share_of_supply: evaluation.quote_share_of_supply,
        evidence,
        checked_at: now(),
        attestation_id: None,
        powers: None,
        read_issue: None,
    }
}

async fn resolve_chain(
    state: &Context,
    address: &str,
    evidence: &mut Vec<String>,
) -> Result<(Chain, Option<PoolInfo>), CheckFailure> {
    if Chain::is_v4_pool_id(address) {
        evidence.push(format!("Detected Uniswap v4 pool id format for {address}."));
        let mut found = None;
        let mut resolution_issue: Option<(u8, CheckFailure)> = None;
        for reader in state.readers.iter().filter(|reader| reader.chain() != Chain::Solana) {
            match reader.read_v4_pool(address).await {
                Ok((pool, pool_evidence)) => {
                    found = Some((reader.chain(), pool, pool_evidence));
                    break;
                }
                Err(error) => {
                    let important_issue = match &error {
                        PoolError::RpcLimit(_) => {
                            Some((3, CheckFailure::from_pool_error_ref(&error)))
                        }
                        PoolError::Unknown(reason) if reason.contains("pool key hash mismatch") => {
                            Some((2, CheckFailure::unsupported("v4 pool key hash mismatch")))
                        }
                        PoolError::Reader(_) => {
                            Some((1, CheckFailure::from_pool_error_ref(&error)))
                        }
                        _ => None,
                    };
                    if let Some(issue) = important_issue
                        && resolution_issue.as_ref().is_none_or(|current| issue.0 > current.0)
                    {
                        resolution_issue = Some(issue);
                    }
                    evidence.push(format!(
                        "{} v4 StateView probe did not identify the pool: {}.",
                        reader.chain(),
                        public_pool_error(&error)
                    ));
                }
            }
        }
        let Some((chain, pool, pool_evidence)) = found else {
            return Err(resolution_issue.map(|(_, failure)| failure).unwrap_or_else(|| {
                CheckFailure::unsupported("v4 pool id was not found on any configured EVM chain")
            }));
        };
        evidence.extend(pool_evidence);
        evidence.push(format!("Resolved Uniswap v4 pool id on {chain}."));
        return Ok((chain, Some(pool)));
    }

    match Chain::detect(address) {
        Some(Chain::Solana) => {
            evidence.push(format!("Detected Solana address format for {address}."));
            Ok((Chain::Solana, None))
        }
        Some(chain) => {
            evidence.push(format!("Detected {chain} address format."));
            Ok((chain, None))
        }
        None if Chain::is_evm_address(address) => {
            let readers = state
                .readers
                .iter()
                .map(|reader| reader.as_ref() as &dyn ChainReader)
                .collect::<Vec<_>>();
            match detect_evm_chain(address, &readers).await {
                Ok(chain) => {
                    evidence.push(format!("Detected {chain} from contract bytecode."));
                    Ok((chain, None))
                }
                Err(error) => {
                    let failure = CheckFailure::from_pool_error(error);
                    evidence.push(format!("EVM chain detection failed: {}.", failure.reason));
                    Err(failure)
                }
            }
        }
        None => {
            evidence.push(format!(
                "Address {address} is neither a Solana public key nor an EVM address."
            ));
            Err(CheckFailure::unsupported("unsupported address format"))
        }
    }
}

async fn resolve_pool(
    state: &Context,
    reader: &dyn ChainReader,
    chain: Chain,
    entries: &[Entry],
    address: &str,
    v4_pool: Option<PoolInfo>,
    evidence: &mut Vec<String>,
) -> Result<(PoolInfo, Option<TokenMeta>), CheckFailure> {
    if let Some(pool) = v4_pool {
        return Ok((pool, None));
    }
    match reader.read_pool(address).await {
        Ok(pool) => Ok((pool, None)),
        Err(pool_error @ (PoolError::UnsupportedVenue(_) | PoolError::RpcLimit(_))) => {
            Err(CheckFailure::from_pool_error(pool_error))
        }
        Err(pool_error) => match reader.token_meta(address).await {
            Ok(meta) if chain == Chain::Solana => {
                let candidate_programs =
                    entries.iter().map(|entry| entry.contract.clone()).collect::<Vec<_>>();
                match reader.pools_for_token(address, &candidate_programs).await {
                    Ok(mut pools) => {
                        pools.sort_by(|left, right| {
                            decimal_cmp(
                                right.quote.balance.as_deref(),
                                left.quote.balance.as_deref(),
                            )
                        });
                        pools.into_iter().next().map(|pool| (pool, Some(meta))).ok_or_else(|| {
                            CheckFailure::unsupported(format!(
                                "no supported pool found for token: {address}"
                            ))
                        })
                    }
                    Err(error) => Err(CheckFailure::from_pool_error(error)),
                }
            }
            Ok(meta) => {
                let candidates = indexed_pool_candidates(state, chain, address).await;
                evidence.push(format!(
                    "Indexed pool candidate count: {} (maximum {}).",
                    candidates.len(),
                    MAX_INDEXED_POOL_CANDIDATES
                ));
                if candidates.is_empty() {
                    return Err(CheckFailure::unsupported("no indexed pool candidate for token"));
                }
                let token = address.to_ascii_lowercase();
                let discovered = tokio::time::timeout(INDEXED_POOL_DEADLINE, async {
                    for candidate in candidates {
                        let Ok(pool) = reader.read_pool(&candidate).await else { continue };
                        if pool.base.address.eq_ignore_ascii_case(&token)
                            || pool.quote.address.eq_ignore_ascii_case(&token)
                        {
                            return Some(pool);
                        }
                    }
                    None
                })
                .await;
                match discovered {
                    Ok(Some(pool)) => Ok((pool, Some(meta))),
                    Ok(None) => Err(CheckFailure::unsupported(
                        "indexed pool candidates did not contain the token",
                    )),
                    Err(_) => Err(CheckFailure::from_pool_error(PoolError::BudgetExceeded(
                        "indexed pool candidate scan",
                    ))),
                }
            }
            Err(meta_error) => Err(CheckFailure::from_pool_error(PoolError::Reader(format!(
                "pool read failed: {}; token metadata failed: {}",
                public_pool_error(&pool_error),
                public_pool_error(&meta_error)
            )))),
        },
    }
}

async fn metadata_for_side(
    reader: &dyn ChainReader,
    side: &TokenSide,
    input_meta: Option<&TokenMeta>,
) -> Result<Option<TokenMeta>, PoolError> {
    if let Some(meta) = input_meta
        && same_contract(side.address.as_str(), meta.address.as_str(), Chain::Solana)
    {
        return Ok(Some(meta.clone()));
    }
    reader.token_meta(&side.address).await.map(Some)
}

fn format_symbol(symbol: &Option<String>) -> String {
    symbol.as_deref().map_or_else(String::new, |value| format!(" ({value})"))
}

fn unknown(input: &str, chain: Chain, reason: String, evidence: Vec<String>) -> CheckResult {
    unknown_with_issue(input, chain, CheckFailure::unsupported(reason), evidence)
}

fn unknown_with_issue(
    input: &str,
    chain: Chain,
    failure: CheckFailure,
    mut evidence: Vec<String>,
) -> CheckResult {
    evidence.push(format!("Check is unknown: {}.", failure.reason));
    CheckResult {
        input: input.to_owned(),
        chain,
        pool: None,
        verdict: Verdict::Unknown { reason: failure.reason },
        quote_share_of_supply: None,
        evidence,
        checked_at: now(),
        attestation_id: None,
        powers: None,
        read_issue: Some(failure.issue),
    }
}

fn metadata_symbol(meta: Option<&TokenMeta>, side: &TokenSide) -> String {
    meta.and_then(|value| value.symbol.as_deref())
        .or(side.symbol.as_deref())
        .unwrap_or("unknown")
        .to_owned()
}

fn metadata_name(meta: Option<&TokenMeta>) -> String {
    meta.and_then(|value| value.name.as_deref()).unwrap_or("unknown").to_owned()
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::context::test_support::{TestContext, build_with_signers};
    use crate::ports::IssuerRegistry;

    fn entry(ticker: &str, contract: &str) -> Entry {
        Entry {
            issuer: "Robinhood".to_owned(),
            ticker: ticker.to_owned(),
            name: ticker.to_owned(),
            chain: Chain::RobinhoodChain,
            contract: contract.to_owned(),
            decimals: Some(6),
            source: "test".to_owned(),
            source_url: "test".to_owned(),
            last_checked: "2026-09-22T00:00:00Z".to_owned(),
            removed_at: None,
            stale_since: None,
            official_deployments: Vec::new(),
        }
    }

    fn pool(base: &str, base_symbol: &str, quote: &str) -> PoolInfo {
        PoolInfo {
            chain: Chain::RobinhoodChain,
            pool: "0x00000000000000000000000000000000000000ff".to_owned(),
            dex: "uniswap-v2".to_owned(),
            base: TokenSide {
                address: base.to_owned(),
                symbol: Some(base_symbol.to_owned()),
                decimals: Some(6),
                balance: Some("25".to_owned()),
            },
            quote: TokenSide {
                address: quote.to_owned(),
                symbol: Some("USDG".to_owned()),
                decimals: Some(6),
                balance: Some("100".to_owned()),
            },
        }
    }
    fn test_state_with(readers: Vec<Box<dyn ChainReader>>, registry: Vec<Entry>) -> TestContext {
        build_with_signers(readers, registry, true, [9; 32], Default::default())
    }

    #[tokio::test]
    async fn mixed_case_burst_coalesces_chain_detection() {
        struct SlowReader(std::sync::Arc<std::sync::atomic::AtomicUsize>);
        #[async_trait::async_trait]
        impl ChainReader for SlowReader {
            fn chain(&self) -> Chain {
                Chain::Base
            }

            async fn code_at(&self, _address: &str) -> Result<Vec<u8>, PoolError> {
                self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                Ok(vec![1])
            }

            async fn read_pool(&self, _address: &str) -> Result<PoolInfo, PoolError> {
                Err(PoolError::Unknown("not a pool".to_owned()))
            }
        }

        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut base_entry = entry("USDG", "0x0000000000000000000000000000000000000012");
        base_entry.chain = Chain::Base;
        let state = test_state_with(
            vec![Box::new(SlowReader(std::sync::Arc::clone(&calls)))],
            vec![base_entry],
        );
        let mixed = "0xAbCd000000000000000000000000000000000012";
        let lower = mixed.to_ascii_lowercase();
        let (left, right) = tokio::join!(check(&state, mixed), check(&state, &lower));

        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(left.verdict, right.verdict);
        assert!(state.check_inflight.lock().await.is_empty());
    }
    #[tokio::test]
    async fn canceled_leader_releases_followers_and_flight_slot() {
        struct CancellableReader {
            started: std::sync::Arc<tokio::sync::Notify>,
            second_started: std::sync::Arc<tokio::sync::Notify>,
            calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        }
        #[async_trait::async_trait]
        impl ChainReader for CancellableReader {
            fn chain(&self) -> Chain {
                Chain::Base
            }

            async fn code_at(&self, _address: &str) -> Result<Vec<u8>, PoolError> {
                let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if call > 0 {
                    self.second_started.notify_one();
                } else {
                    self.started.notify_one();
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                Ok(vec![1])
            }

            async fn read_pool(&self, _address: &str) -> Result<PoolInfo, PoolError> {
                Err(PoolError::Unknown("not a pool".to_owned()))
            }
        }

        let started = std::sync::Arc::new(tokio::sync::Notify::new());
        let second_started = std::sync::Arc::new(tokio::sync::Notify::new());
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut base_entry = entry("USDG", "0x0000000000000000000000000000000000000012");
        base_entry.chain = Chain::Base;
        let state = test_state_with(
            vec![Box::new(CancellableReader {
                started: std::sync::Arc::clone(&started),
                second_started: std::sync::Arc::clone(&second_started),
                calls: std::sync::Arc::clone(&calls),
            })],
            vec![base_entry],
        );
        let address = "0xAbCd000000000000000000000000000000000012";
        let leader_state = state.clone();
        let leader = tokio::spawn(async move { check(&leader_state, address).await });
        tokio::time::timeout(std::time::Duration::from_secs(1), started.notified()).await.unwrap();
        let follower_state = state.clone();
        let follower = tokio::spawn(async move { check(&follower_state, address).await });
        tokio::task::yield_now().await;
        leader.abort();
        let _ = leader.await;
        tokio::time::timeout(std::time::Duration::from_secs(1), second_started.notified())
            .await
            .unwrap();

        let follower_result =
            tokio::time::timeout(std::time::Duration::from_secs(1), follower).await.unwrap();
        assert!(follower_result.is_ok());
        assert!(state.check_inflight.lock().await.is_empty());

        let later =
            tokio::time::timeout(std::time::Duration::from_secs(1), check(&state, address)).await;
        assert!(later.is_ok());
        assert!(state.check_inflight.lock().await.is_empty());
    }
    #[tokio::test]
    async fn distinct_check_keys_do_not_share_flight_slot() {
        struct ConcurrentReader {
            active: std::sync::Arc<std::sync::atomic::AtomicUsize>,
            maximum: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        }
        #[async_trait::async_trait]
        impl ChainReader for ConcurrentReader {
            fn chain(&self) -> Chain {
                Chain::Base
            }

            async fn code_at(&self, _address: &str) -> Result<Vec<u8>, PoolError> {
                let active = self.active.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                self.maximum.fetch_max(active, std::sync::atomic::Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                self.active.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                Ok(vec![1])
            }

            async fn read_pool(&self, _address: &str) -> Result<PoolInfo, PoolError> {
                Err(PoolError::Unknown("not a pool".to_owned()))
            }
        }

        let active = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let maximum = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut base_entry = entry("USDG", "0x0000000000000000000000000000000000000012");
        base_entry.chain = Chain::Base;
        let state = test_state_with(
            vec![Box::new(ConcurrentReader {
                active: std::sync::Arc::clone(&active),
                maximum: std::sync::Arc::clone(&maximum),
            })],
            vec![base_entry],
        );
        let left = check(&state, "0x00000000000000000000000000000000000000aa");
        let right = check(&state, "0x00000000000000000000000000000000000000bb");
        let _ = tokio::join!(left, right);

        assert!(maximum.load(std::sync::atomic::Ordering::SeqCst) >= 2);
    }

    #[tokio::test]
    async fn indexed_pool_candidates_are_bounded_and_absent_tokens_are_empty() {
        let state = test_state_with(Vec::new(), Vec::new());
        let token = "0x00000000000000000000000000000000000000aa";
        for number in 0..32 {
            state.pool_index.add_candidate(Chain::Base, token, &format!("0x{number:040x}"));
        }

        let candidates = indexed_pool_candidates(&state, Chain::Base, token).await;
        assert_eq!(candidates.len(), MAX_INDEXED_POOL_CANDIDATES);
        assert_eq!(
            candidates
                .iter()
                .map(|pool| pool.to_ascii_lowercase())
                .collect::<std::collections::HashSet<_>>()
                .len(),
            MAX_INDEXED_POOL_CANDIDATES
        );
        assert!(
            indexed_pool_candidates(
                &state,
                Chain::Base,
                "0x00000000000000000000000000000000000000cc"
            )
            .await
            .is_empty()
        );
    }
    #[tokio::test]
    async fn indexed_evm_token_candidate_is_read_directly() {
        struct IndexedReader {
            token: String,
            candidate: String,
            candidate_reads: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        }
        #[async_trait::async_trait]
        impl ChainReader for IndexedReader {
            fn chain(&self) -> Chain {
                Chain::Base
            }

            async fn code_at(&self, _address: &str) -> Result<Vec<u8>, PoolError> {
                Ok(vec![1])
            }

            async fn read_pool(&self, address: &str) -> Result<PoolInfo, PoolError> {
                if !address.eq_ignore_ascii_case(&self.candidate) {
                    return Err(PoolError::Unknown("not a pool".to_owned()));
                }
                self.candidate_reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(PoolInfo {
                    chain: Chain::Base,
                    pool: self.candidate.clone(),
                    dex: "uniswap-v2".to_owned(),
                    base: TokenSide {
                        address: self.token.clone(),
                        symbol: Some("TOKEN".to_owned()),
                        decimals: Some(18),
                        balance: Some("100".to_owned()),
                    },
                    quote: TokenSide {
                        address: "0x0000000000000000000000000000000000000012".to_owned(),
                        symbol: Some("USDG".to_owned()),
                        decimals: Some(6),
                        balance: Some("100".to_owned()),
                    },
                })
            }

            async fn token_meta(&self, address: &str) -> Result<TokenMeta, PoolError> {
                Ok(TokenMeta {
                    address: address.to_owned(),
                    symbol: Some("TOKEN".to_owned()),
                    name: Some("Token".to_owned()),
                    decimals: Some(18),
                    total_supply: Some("1000".to_owned()),
                })
            }
        }

        let token = "0x00000000000000000000000000000000000000aa".to_owned();
        let candidate = "0x00000000000000000000000000000000000000cc".to_owned();
        let reads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut base_entry = entry("USDG", "0x0000000000000000000000000000000000000012");
        base_entry.chain = Chain::Base;
        let state = test_state_with(
            vec![Box::new(IndexedReader {
                token: token.clone(),
                candidate: candidate.clone(),
                candidate_reads: std::sync::Arc::clone(&reads),
            })],
            vec![base_entry],
        );
        state.pool_index.add_candidate(Chain::Base, &token, &candidate);

        let registry = state.registry.snapshot().await;
        let result = check_uncached(&state, &token, &registry).await;
        assert_eq!(result.pool.as_ref().map(|pool| pool.pool.as_str()), Some(candidate.as_str()));
        assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn verified_when_quote_contract_is_registry_contract() {
        let quote = "0x0000000000000000000000000000000000000010";
        let input = pool("0x0000000000000000000000000000000000000011", "xNVDA", quote);
        let quote_meta = TokenMeta {
            address: quote.to_owned(),
            symbol: Some("USDG".to_owned()),
            name: Some("Dollar".to_owned()),
            decimals: Some(6),
            total_supply: Some("1000".to_owned()),
        };
        let result = evaluate_pool(
            &input,
            &input.base,
            &input.quote,
            None,
            Some(&quote_meta),
            &[entry("USDG", quote)],
        );
        assert_eq!(
            result.verdict,
            Verdict::Verified { issuer: "Robinhood".to_owned(), ticker: "USDG".to_owned() }
        );
        assert_eq!(result.quote_share_of_supply, Some(0.1));
    }
    #[test]
    fn powers_follow_registry_matched_base_side() {
        let stock = "0x0000000000000000000000000000000000000010";
        let pool = pool(stock, "NVDAx", "0x0000000000000000000000000000000000000012");
        let address = registered_token_address(&pool, &[entry("NVDA", stock)]);
        assert_eq!(address, Some(stock));
    }

    #[test]
    fn mismatch_when_base_claims_registry_ticker() {
        let input = pool(
            "0x0000000000000000000000000000000000000011",
            "xNVDA",
            "0x0000000000000000000000000000000000000012",
        );
        let result = evaluate_pool(
            &input,
            &input.base,
            &input.quote,
            None,
            None,
            &[entry("NVDA", "0x0000000000000000000000000000000000000010")],
        );
        assert_eq!(
            result.verdict,
            Verdict::Mismatch {
                claimed: "NVDA".to_owned(),
                actual: "0x0000000000000000000000000000000000000012".to_owned(),
            }
        );
    }
    #[test]
    fn mismatch_when_quote_metadata_claims_registry_ticker() {
        let input = pool(
            "0x0000000000000000000000000000000000000011",
            "ABC",
            "0x0000000000000000000000000000000000000012",
        );
        let quote_meta = TokenMeta {
            address: input.quote.address.clone(),
            symbol: Some("NVDA.d".to_owned()),
            name: Some("NVIDIA".to_owned()),
            decimals: Some(6),
            total_supply: Some("1000".to_owned()),
        };
        let result = evaluate_pool(
            &input,
            &input.base,
            &input.quote,
            None,
            Some(&quote_meta),
            &[entry("NVDA", "0x0000000000000000000000000000000000000010")],
        );
        assert_eq!(
            result.verdict,
            Verdict::Mismatch { claimed: "NVDA".to_owned(), actual: input.quote.address }
        );
    }

    #[test]
    fn no_match_when_no_side_claims_registry_ticker() {
        let input = pool(
            "0x0000000000000000000000000000000000000011",
            "ABC",
            "0x0000000000000000000000000000000000000012",
        );
        let result = evaluate_pool(
            &input,
            &input.base,
            &input.quote,
            None,
            None,
            &[entry("NVDA", "0x0000000000000000000000000000000000000010")],
        );
        assert_eq!(result.verdict, Verdict::NoMatch);
    }

    #[tokio::test]
    async fn reader_failure_is_unknown_not_no_match() {
        struct FailingReader;
        #[async_trait::async_trait]
        impl ChainReader for FailingReader {
            async fn code_at(&self, _address: &str) -> Result<Vec<u8>, PoolError> {
                Ok(vec![1])
            }
            fn chain(&self) -> Chain {
                Chain::RobinhoodChain
            }

            async fn read_pool(&self, _address: &str) -> Result<PoolInfo, PoolError> {
                Ok(pool(
                    "0x0000000000000000000000000000000000000011",
                    "ABC",
                    "0x0000000000000000000000000000000000000012",
                ))
            }

            async fn token_meta(&self, _address: &str) -> Result<TokenMeta, PoolError> {
                Err(PoolError::Reader("fixture timeout".to_owned()))
            }
        }

        let state = test_state_with(
            vec![Box::new(FailingReader)],
            vec![entry("USDG", "0x0000000000000000000000000000000000000012")],
        );
        let registry = state.registry.snapshot().await;
        let result =
            check_uncached(&state, "0x00000000000000000000000000000000000000ff", &registry).await;
        assert!(
            matches!(result.verdict, Verdict::Unknown { reason } if reason.contains("metadata read failed"))
        );
    }

    #[test]
    fn normalises_x_on_and_dot_d_claims() {
        assert!(claims_ticker("x NVDA", "NVDA"));
        assert!(claims_ticker("ONNVDA", "NVDA"));
        assert!(claims_ticker("NVDA.d", "NVDA"));
        assert!(!claims_ticker("FAMI", "F"));
        assert!(!claims_symbol("Farmmi", "F", "Ford"));
        assert!(!claims_name("Farmmi, Inc.", "ALAB", "Astera Labs, Inc."));
        assert!(claims_name("NVIDIA token", "NVDA", "NVIDIA"));
    }

    #[test]
    fn share_requires_valid_nonzero_supply() {
        assert_eq!(quote_share_of_supply(Some("25"), Some("100")), Some(0.25));
        assert_eq!(quote_share_of_supply(Some("25"), Some("0")), None);
        assert_eq!(quote_share_of_supply(Some("not-a-number"), Some("100")), None);
    }

    #[test]
    fn share_rejects_inconsistent_token_scaling() {
        assert_eq!(quote_share_of_supply(Some("1214"), Some("100")), None);
        assert_eq!(
            quote_share_unavailable_reason(Some("1214"), Some("100")),
            Some("quote balance exceeds total supply, so token reads have inconsistent scaling")
        );
    }

    #[test]
    fn inconsistent_share_adds_scaling_evidence() {
        let quote = "0x0000000000000000000000000000000000000010";
        let mut input = pool("0x0000000000000000000000000000000000000011", "NVDA", quote);
        input.quote.balance = Some("1214".to_owned());
        let quote_meta = TokenMeta {
            address: quote.to_owned(),
            symbol: Some("USDG".to_owned()),
            name: Some("Dollar".to_owned()),
            decimals: Some(6),
            total_supply: Some("1000".to_owned()),
        };
        let result = evaluate_pool(
            &input,
            &input.base,
            &input.quote,
            None,
            Some(&quote_meta),
            &[entry("USDG", quote)],
        );
        assert_eq!(result.quote_share_of_supply, None);
        assert!(result.evidence.iter().any(|line| line.contains("inconsistent scaling")));
    }

    #[tokio::test]
    async fn unknown_when_chain_has_no_registry_entries() {
        let state = test_state_with(Vec::new(), Vec::new());
        let result = check(&state, "11111111111111111111111111111111").await;
        assert_eq!(
            result.verdict,
            Verdict::Unknown { reason: "no registry entries for Solana".to_owned() }
        );
    }

    #[test]
    fn provider_error_redaction_never_exposes_keyed_url() {
        let error = PoolError::Reader(
            "request https://rpc.example.invalid/?api-key=secret-key failed".to_owned(),
        );
        let public = public_pool_error(&error);
        assert_eq!(public, "RPC provider request failed");
        assert!(!public.contains("secret-key"));
    }

    #[test]
    fn canonical_cache_key_is_case_insensitive_only_for_evm() {
        let mixed_case = format!("0xAbCd{}", "0".repeat(36));
        assert_eq!(cache_key(&format!("  {mixed_case}  ")), mixed_case.to_ascii_lowercase());
        let solana = "So11111111111111111111111111111111111111112";
        assert_eq!(cache_key(solana), solana);
    }
}
