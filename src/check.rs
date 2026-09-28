use crate::chain::Chain;
use crate::pool::{PoolError, PoolInfo, PoolReader, TokenMeta, TokenSide, detect_evm_chain};
use crate::registry::Entry;
use crate::state::{AppState, CachedCheckResult};
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Verdict {
    Verified { issuer: String, ticker: String },
    NoMatch,
    Mismatch { claimed: String, actual: String },
    Unknown { reason: String },
}

fn public_pool_error(error: &PoolError) -> &'static str {
    match error {
        PoolError::InvalidAddress => "unsupported pool address",
        PoolError::Unknown(_) => "pool was not found",
        PoolError::CodeLookupUnsupported => "contract lookup is unavailable",
        PoolError::BudgetExceeded(_) => "bounded pool discovery was exhausted",
        PoolError::Reader(_) => "RPC provider request failed",
    }
}

pub(crate) fn cache_key(address: &str) -> String {
    let address = address.trim();
    if Chain::is_evm_address(address) { address.to_ascii_lowercase() } else { address.to_owned() }
}

pub(crate) fn valid_public_input(address: &str) -> bool {
    let address = address.trim();
    !address.is_empty()
        && address.len() <= 128
        && (Chain::is_evm_address(address)
            || Chain::detect(address).is_some()
            || Chain::is_v4_pool_id(address))
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CheckResult {
    pub input: String,
    pub chain: Chain,
    pub pool: Option<PoolInfo>,
    pub verdict: Verdict,
    pub quote_share_of_supply: Option<f64>,
    pub evidence: Vec<String>,
    pub checked_at: String,
    pub attestation_id: Option<String>,
}

/// Check an address against the configured readers and issuer registry.
/// Results are kept in the application cache for thirty seconds. EVM
/// addresses use a canonical lower-case key; Solana addresses remain
/// case-sensitive.
pub async fn check(state: &AppState, address: &str) -> CheckResult {
    let cache_key = cache_key(address);
    let leader_sender = loop {
        let registry_version = crate::state::current_registry_version();
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
    let registry_version = crate::state::current_registry_version();

    let (mut result, read_log) = crate::attest::capture_reads(check_uncached(state, address)).await;
    if let Some(attestation) = crate::attest::create_for_check(state, &result, read_log).await {
        result.attestation_id = Some(attestation.id);
    }

    // A refresh advances the version while holding the registry write lock.
    // Do not repopulate the cache with a check that crossed that replacement.
    if registry_version == crate::state::current_registry_version() {
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

async fn indexed_pool_candidates(state: &AppState, chain: Chain, token: &str) -> Vec<String> {
    let token = token.to_ascii_lowercase();
    let featured = state.featured.read().await.clone();
    let leaderboard = state.leaderboard.read().await.clone();
    let attestations = match state.attestations.read() {
        Ok(attestations) => attestations.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    };
    let mut candidates = Vec::new();
    let mut seen = HashSet::new();
    let mut add = |pool: &str| {
        let key = pool.to_ascii_lowercase();
        if !pool.is_empty() && seen.insert(key) {
            candidates.push(pool.to_owned());
        }
    };
    for pool in featured.iter().filter(|pool| pool.chain == chain) {
        if pool.base_address.eq_ignore_ascii_case(&token)
            || pool.quote_address.eq_ignore_ascii_case(&token)
        {
            add(&pool.pool);
        }
    }
    let chain_slug = crate::discovery::chain_slug(chain);
    for entry in leaderboard.entries.iter().filter(|entry| entry.chain == chain_slug) {
        let Some(attestation_id) = entry.attestation_id.as_deref() else { continue };
        let Some(attestation) = attestations.get(attestation_id) else { continue };
        if attestation.pool.base.address.eq_ignore_ascii_case(&token)
            || attestation.pool.quote.address.eq_ignore_ascii_case(&token)
        {
            add(&entry.pool);
        }
    }
    candidates.truncate(MAX_INDEXED_POOL_CANDIDATES);
    candidates
}

async fn check_uncached(state: &AppState, address: &str) -> CheckResult {
    let mut evidence = Vec::new();
    let mut v4_pool = None;
    let chain = if Chain::is_v4_pool_id(address) {
        evidence.push(format!("Detected Uniswap v4 pool id format for {address}."));
        let mut found = None;
        for reader in state.readers.iter().filter(|reader| reader.chain() != Chain::Solana) {
            match reader.read_v4_pool(address).await {
                Ok((pool, pool_evidence)) => {
                    found = Some((reader.chain(), pool, pool_evidence));
                    break;
                }
                Err(error) => {
                    evidence.push(format!(
                        "{} v4 StateView probe did not identify the pool: {}.",
                        reader.chain(),
                        public_pool_error(&error)
                    ));
                }
            }
        }
        let Some((chain, pool, pool_evidence)) = found else {
            return unknown(
                address,
                Chain::RobinhoodChain,
                "v4 pool id was not found on any configured EVM chain".to_owned(),
                evidence,
            );
        };
        evidence.extend(pool_evidence);
        v4_pool = Some(pool);
        evidence.push(format!("Resolved Uniswap v4 pool id on {chain}."));
        chain
    } else {
        match Chain::detect(address) {
            Some(Chain::Solana) => {
                evidence.push(format!("Detected Solana address format for {address}."));
                Chain::Solana
            }
            Some(chain) => {
                evidence.push(format!("Detected {chain} address format."));
                chain
            }
            None if Chain::is_evm_address(address) => {
                let readers = state
                    .readers
                    .iter()
                    .map(|reader| reader.as_ref() as &dyn PoolReader)
                    .collect::<Vec<_>>();
                match detect_evm_chain(address, &readers).await {
                    Ok(chain) => {
                        evidence.push(format!("Detected {chain} from contract bytecode."));
                        chain
                    }
                    Err(error) => {
                        let reason = public_pool_error(&error).to_owned();
                        evidence.push(format!("EVM chain detection failed: {reason}."));
                        return unknown(address, Chain::RobinhoodChain, reason, evidence);
                    }
                }
            }
            None => {
                evidence.push(format!(
                    "Address {address} is neither a Solana public key nor an EVM address."
                ));
                return unknown(
                    address,
                    Chain::RobinhoodChain,
                    "unsupported address format".to_owned(),
                    evidence,
                );
            }
        }
    };
    let chain_entries = {
        let registry = state.registry.read().await;
        registry.iter().filter(|entry| entry.chain == chain).cloned().collect::<Vec<_>>()
    };
    evidence.push(format!("Registry has {} entries for {chain}.", chain_entries.len()));
    if chain_entries.is_empty() {
        return unknown(address, chain, format!("no registry entries for {chain}"), evidence);
    }
    let Some(reader) = state.readers.iter().find(|reader| reader.chain() == chain) else {
        return unknown(address, chain, format!("no pool reader configured for {chain}"), evidence);
    };
    if let Err(error) = reader.record_position().await {
        let reason = format!("chain position read failed: {}", public_pool_error(&error));
        evidence.push(reason.clone());
        return unknown(address, chain, reason, evidence);
    }

    let (pool, input_meta) = if let Some(pool) = v4_pool.take() {
        (pool, None)
    } else {
        match reader.read_pool(address).await {
            Ok(pool) => (pool, None),
            Err(pool_error) => match reader.token_meta(address).await {
                Ok(meta) if chain == Chain::Solana => {
                    let candidate_programs = chain_entries
                        .iter()
                        .map(|entry| entry.contract.clone())
                        .collect::<Vec<_>>();
                    match reader.pools_for_token(address, &candidate_programs).await {
                        Ok(mut pools) => {
                            pools.sort_by(|left, right| {
                                decimal_cmp(
                                    right.quote.balance.as_deref(),
                                    left.quote.balance.as_deref(),
                                )
                            });
                            match pools.into_iter().next() {
                                Some(pool) => (pool, Some(meta)),
                                None => {
                                    return unknown(
                                        address,
                                        chain,
                                        format!("no supported pool found for token: {address}"),
                                        evidence,
                                    );
                                }
                            }
                        }
                        Err(error) => {
                            return unknown(
                                address,
                                chain,
                                format!(
                                    "token pool discovery failed: {}",
                                    public_pool_error(&error)
                                ),
                                evidence,
                            );
                        }
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
                        return unknown(
                            address,
                            chain,
                            "no indexed pool candidate for token".to_owned(),
                            evidence,
                        );
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
                        Ok(Some(pool)) => (pool, Some(meta)),
                        Ok(None) => {
                            return unknown(
                                address,
                                chain,
                                "indexed pool candidates did not contain the token".to_owned(),
                                evidence,
                            );
                        }
                        Err(_) => {
                            return unknown(
                                address,
                                chain,
                                "indexed pool candidate budget was exhausted".to_owned(),
                                evidence,
                            );
                        }
                    }
                }
                Err(meta_error) => {
                    return unknown(
                        address,
                        chain,
                        format!(
                            "pool read failed: {}; token metadata failed: {}",
                            public_pool_error(&pool_error),
                            public_pool_error(&meta_error)
                        ),
                        evidence,
                    );
                }
            },
        }
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
            let reason = format!("base token metadata read failed: {}", public_pool_error(&error));
            evidence.push(reason.clone());
            return unknown(address, chain, reason, evidence);
        }
    };
    let quote_meta = match metadata_for_side(reader.as_ref(), quote_side, input_meta.as_ref()).await
    {
        Ok(meta) => meta,
        Err(error) => {
            let reason = format!("quote token metadata read failed: {}", public_pool_error(&error));
            evidence.push(reason.clone());
            return unknown(address, chain, reason, evidence);
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
        let quote_code: Result<Vec<u8>, crate::pool::PoolError> =
            reader.code_at(&evaluation.quote_address).await;
        let registry_code: Result<Vec<u8>, crate::pool::PoolError> =
            reader.code_at(&entry.contract).await;
        match (quote_code, registry_code) {
            (Ok(quote_code), Ok(registry_code)) => {
                let similarity = bytecode_similarity(&quote_code, &registry_code) * 100.0;
                evidence.push(format!(
                    "Bytecode similarity between quote {} and {} ({}) is {:.1}%.",
                    evaluation.quote_address, entry.contract, entry.ticker, similarity
                ));
            }
            (Err(error), _) | (_, Err(error)) => {
                let reason = format!("bytecode read failed: {}", public_pool_error(&error));
                evidence.push(reason.clone());
                return unknown(address, chain, reason, evidence);
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
    }
}

async fn metadata_for_side(
    reader: &dyn PoolReader,
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

fn selected_sides<'a>(pool: &'a PoolInfo, entries: &[Entry]) -> (&'a TokenSide, &'a TokenSide) {
    let quote_is_registry = !matches!(
        crate::registry::match_status(entries, pool.chain, &pool.quote.address),
        crate::registry::MatchStatus::NotFound
    );
    if quote_is_registry {
        (&pool.base, &pool.quote)
    } else if !matches!(
        crate::registry::match_status(entries, pool.chain, &pool.base.address),
        crate::registry::MatchStatus::NotFound
    ) {
        (&pool.quote, &pool.base)
    } else {
        (&pool.base, &pool.quote)
    }
}

struct PoolEvaluation {
    verdict: Verdict,
    quote_share_of_supply: Option<f64>,
    evidence: Vec<String>,
    claimed_entry: Option<Entry>,
    quote_address: String,
}

fn evaluate_pool(
    pool: &PoolInfo,
    base_side: &TokenSide,
    quote_side: &TokenSide,
    base_meta: Option<&TokenMeta>,
    quote_meta: Option<&TokenMeta>,
    entries: &[Entry],
) -> PoolEvaluation {
    let quote_entry = crate::registry::lookup(entries, pool.chain, &quote_side.address);
    let base_entry = crate::registry::lookup(entries, pool.chain, &base_side.address);
    let mut evidence = Vec::new();
    let quote_share_of_supply = quote_share_of_supply(
        quote_side.balance.as_deref(),
        quote_meta.and_then(|meta| meta.total_supply.as_deref()),
    );
    if let Some(share) = quote_share_of_supply {
        evidence.push(format!(
            "Quote balance / total supply is {:.4}% ({:.8} as a fraction).",
            share * 100.0,
            share
        ));
    } else if let Some(reason) = quote_share_unavailable_reason(
        quote_side.balance.as_deref(),
        quote_meta.and_then(|meta| meta.total_supply.as_deref()),
    ) {
        evidence.push(format!("Quote share unavailable: {reason}."));
    } else {
        evidence.push(
            "Quote share of supply is unavailable because balance or total supply is missing."
                .to_owned(),
        );
    }

    let (verdict, claimed_entry) = if let Some(entry) = quote_entry {
        evidence.push(format!(
            "Quote contract {} matches registry ticker {} from {}.",
            quote_side.address, entry.ticker, entry.issuer
        ));
        (
            Verdict::Verified { issuer: entry.issuer.clone(), ticker: entry.ticker.clone() },
            Some(entry.clone()),
        )
    } else {
        let status = crate::registry::match_status(entries, pool.chain, &quote_side.address);
        if let crate::registry::MatchStatus::Removed { issuer, removed_at } = status {
            let reason = format!("removed from {issuer} registry on {removed_at}");
            evidence.push(reason.clone());
            return PoolEvaluation {
                verdict: Verdict::Unknown { reason },
                quote_share_of_supply,
                evidence,
                claimed_entry: None,
                quote_address: quote_side.address.clone(),
            };
        }
        if let crate::registry::MatchStatus::Stale { since } = status {
            let reason = format!("registry source stale since {since}");
            evidence.push(reason.clone());
            return PoolEvaluation {
                verdict: Verdict::Unknown { reason },
                quote_share_of_supply,
                evidence,
                claimed_entry: None,
                quote_address: quote_side.address.clone(),
            };
        }
        evidence.push(format!(
            "Quote contract {} does not match a registry entry on {}.",
            quote_side.address, pool.chain
        ));
        let base_claim = claimed_entry(base_side, base_meta, entries, pool.chain);
        let quote_claim = claimed_entry(quote_side, quote_meta, entries, pool.chain);
        if let Some(entry) = base_claim.or(quote_claim) {
            evidence.push(format!(
                "Token metadata claims registry ticker {} while quote contract is {}.",
                entry.ticker, quote_side.address
            ));
            (
                Verdict::Mismatch {
                    claimed: entry.ticker.clone(),
                    actual: quote_side.address.clone(),
                },
                Some(entry.clone()),
            )
        } else {
            evidence.push("Neither token side claims a registry ticker.".to_owned());
            (Verdict::NoMatch, base_entry.cloned())
        }
    };

    PoolEvaluation {
        verdict,
        quote_share_of_supply,
        evidence,
        claimed_entry,
        quote_address: quote_side.address.clone(),
    }
}

fn claimed_entry<'a>(
    side: &TokenSide,
    meta: Option<&TokenMeta>,
    entries: &'a [Entry],
    chain: Chain,
) -> Option<&'a Entry> {
    let symbols = [side.symbol.as_deref(), meta.and_then(|value| value.symbol.as_deref())];
    let names = [meta.and_then(|value| value.name.as_deref())];
    entries.iter().find(|entry| {
        crate::registry::matchable(entry)
            && entry.chain == chain
            && (symbols
                .into_iter()
                .flatten()
                .any(|value| claims_symbol(value, &entry.ticker, &entry.name))
                || names
                    .into_iter()
                    .flatten()
                    .any(|value| claims_name(value, &entry.ticker, &entry.name)))
    })
}

#[cfg(test)]
fn claims_ticker(value: &str, ticker: &str) -> bool {
    claims_symbol(value, ticker, "")
}
fn claims_symbol(value: &str, ticker: &str, name: &str) -> bool {
    let value = normalise_claim(value);
    let ticker = normalise_registry(ticker);
    let name = normalise_registry(name);
    !value.is_empty() && (value == ticker || (!name.is_empty() && value == name))
}

fn claims_name(value: &str, ticker: &str, name: &str) -> bool {
    let ticker = normalise_registry(ticker);
    if ticker.chars().count() <= 1 {
        return false;
    }
    let registry_name_words = name
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty() && !is_name_stop_word(word))
        .map(normalise_registry)
        .collect::<Vec<_>>();
    claim_words(value)
        .into_iter()
        .any(|word| word == ticker || registry_name_words.iter().any(|name| name == &word))
}

fn claim_words(value: &str) -> Vec<String> {
    value
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty() && !is_name_stop_word(word))
        .map(normalise_claim)
        .filter(|word| !word.is_empty())
        .collect()
}

fn is_name_stop_word(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "inc"
            | "incorporated"
            | "corp"
            | "corporation"
            | "co"
            | "company"
            | "ltd"
            | "limited"
            | "llc"
            | "plc"
            | "token"
            | "coin"
            | "stock"
            | "shares"
            | "class"
    )
}

fn normalise_registry(value: &str) -> String {
    value
        .to_ascii_lowercase()
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .collect()
}

fn normalise_claim(value: &str) -> String {
    let mut value = value
        .to_ascii_lowercase()
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    loop {
        let mut changed = false;
        if let Some(stripped) = value.strip_prefix("on") {
            value = stripped.to_owned();
            changed = true;
        }
        if let Some(stripped) = value.strip_prefix('x') {
            value = stripped.to_owned();
            changed = true;
        }
        if let Some(stripped) = value.strip_suffix(".d") {
            value = stripped.to_owned();
            changed = true;
        }
        if !changed {
            break;
        }
    }
    value.chars().filter(|character| character.is_ascii_alphanumeric()).collect()
}

pub fn quote_share_of_supply(balance: Option<&str>, total_supply: Option<&str>) -> Option<f64> {
    let balance = balance?.parse::<f64>().ok()?;
    let total_supply = total_supply?.parse::<f64>().ok()?;
    if !balance.is_finite()
        || !total_supply.is_finite()
        || balance < 0.0
        || total_supply <= 0.0
        || balance > total_supply
    {
        return None;
    }
    Some(balance / total_supply)
}

fn quote_share_unavailable_reason(
    balance: Option<&str>,
    total_supply: Option<&str>,
) -> Option<&'static str> {
    let balance = balance?.parse::<f64>().ok()?;
    let total_supply = total_supply?.parse::<f64>().ok()?;
    (balance.is_finite() && total_supply.is_finite() && balance > total_supply)
        .then_some("quote balance exceeds total supply, so token reads have inconsistent scaling")
}

fn bytecode_similarity(left: &[u8], right: &[u8]) -> f64 {
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    if left == right {
        return 1.0;
    }
    const SHINGLE: usize = 4;
    let left_shingles = left.windows(SHINGLE).collect::<HashSet<_>>();
    let right_shingles = right.windows(SHINGLE).collect::<HashSet<_>>();
    if left_shingles.is_empty() || right_shingles.is_empty() {
        return 0.0;
    }
    let intersection = left_shingles.intersection(&right_shingles).count() as f64;
    let union = left_shingles.union(&right_shingles).count() as f64;
    if union == 0.0 { 0.0 } else { intersection / union }
}

fn same_contract(left: &str, right: &str, chain: Chain) -> bool {
    if chain == Chain::Solana { left == right } else { left.eq_ignore_ascii_case(right) }
}

fn format_symbol(symbol: &Option<String>) -> String {
    symbol.as_deref().map_or_else(String::new, |value| format!(" ({value})"))
}

fn unknown(input: &str, chain: Chain, reason: String, mut evidence: Vec<String>) -> CheckResult {
    evidence.push(format!("Check is unknown: {reason}."));
    CheckResult {
        input: input.to_owned(),
        chain,
        pool: None,
        verdict: Verdict::Unknown { reason },
        quote_share_of_supply: None,
        evidence,
        checked_at: now(),
        attestation_id: None,
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

fn decimal_cmp(left: Option<&str>, right: Option<&str>) -> std::cmp::Ordering {
    match (
        left.and_then(|value| value.parse::<u128>().ok()),
        right.and_then(|value| value.parse::<u128>().ok()),
    ) {
        (Some(left), Some(right)) => left.cmp(&right),
        _ => match (
            left.and_then(|value| value.parse::<f64>().ok()),
            right.and_then(|value| value.parse::<f64>().ok()),
        ) {
            (Some(left), Some(right)) => {
                left.partial_cmp(&right).unwrap_or(std::cmp::Ordering::Equal)
            }
            _ => std::cmp::Ordering::Equal,
        },
    }
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn test_state_with(readers: Vec<Box<dyn PoolReader>>, registry: Vec<Entry>) -> AppState {
        AppState {
            registry: std::sync::Arc::new(tokio::sync::RwLock::new(registry)),
            registry_status: std::sync::Arc::new(tokio::sync::RwLock::new(
                crate::discovery::RegistrySnapshot::default(),
            )),
            readers: std::sync::Arc::new(readers),
            http: reqwest::Client::new(),
            check_cache: moka::future::Cache::builder().build(),
            leaderboard_check_cache: moka::future::Cache::builder().build(),
            check_inflight: std::sync::Arc::new(tokio::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            featured: std::sync::Arc::new(tokio::sync::RwLock::new(Vec::new())),
            featured_status: std::sync::Arc::new(tokio::sync::RwLock::new(
                crate::discovery::FeaturedStatus::default(),
            )),
            leaderboard: std::sync::Arc::new(tokio::sync::RwLock::new(
                crate::discovery::Leaderboard::default(),
            )),
            prices: std::sync::Arc::new(tokio::sync::RwLock::new(
                crate::discovery::PriceSnapshot::default(),
            )),
            attestations: std::sync::Arc::new(std::sync::RwLock::new(
                std::collections::HashMap::new(),
            )),
            signing_key: std::sync::Arc::new(ed25519_dalek::SigningKey::from_bytes(&[9; 32])),
            dev_signer: true,
            attest_store: std::sync::Arc::new(crate::attest::FileAttestationStore::new(
                std::path::PathBuf::from("target/test-check-attestations"),
            )),
            registry_hash: std::sync::Arc::new(std::sync::RwLock::new(String::new())),
            registry_api_cache: std::sync::Arc::new(tokio::sync::RwLock::new(None)),
            public_url: std::sync::Arc::new("http://localhost:3000".to_owned()),
            rate_limiter: std::sync::Arc::new(crate::state::RateLimiter::default()),
            expensive_concurrency: std::sync::Arc::new(tokio::sync::Semaphore::new(32)),
            wallet_concurrency: std::sync::Arc::new(tokio::sync::Semaphore::new(1)),
            registry_api_concurrency: std::sync::Arc::new(tokio::sync::Semaphore::new(4)),
        }
    }

    #[tokio::test]
    async fn mixed_case_burst_coalesces_chain_detection() {
        struct SlowReader(std::sync::Arc<std::sync::atomic::AtomicUsize>);
        #[async_trait::async_trait]
        impl PoolReader for SlowReader {
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
        impl PoolReader for CancellableReader {
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
        impl PoolReader for ConcurrentReader {
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
        let mut featured = Vec::new();
        for number in 0..32 {
            featured.push(crate::discovery::FeaturedPool {
                chain: Chain::Base,
                dex: "uniswap-v2".to_owned(),
                pool: format!("0x{number:040x}"),
                base_symbol: "TOKEN".to_owned(),
                base_address: token.to_owned(),
                quote_symbol: "USDG".to_owned(),
                quote_address: "0x00000000000000000000000000000000000000bb".to_owned(),
                issuer: Some("Issuer".to_owned()),
                ticker: Some("TOKEN".to_owned()),
                verdict: "verified".to_owned(),
                quote_balance: Some("100".to_owned()),
                quote_share_of_supply: Some(0.1),
                volume_24h_usd: None,
                liquidity_usd: None,
                curated: false,
                note: None,
                updated_at: "2026-09-27T00:00:00Z".to_owned(),
            });
        }
        *state.featured.write().await = featured;

        let candidates = indexed_pool_candidates(&state, Chain::Base, token).await;
        assert_eq!(candidates.len(), MAX_INDEXED_POOL_CANDIDATES);
        assert_eq!(
            candidates.iter().map(|pool| pool.to_ascii_lowercase()).collect::<HashSet<_>>().len(),
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
        impl PoolReader for IndexedReader {
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
        *state.featured.write().await = vec![crate::discovery::FeaturedPool {
            chain: Chain::Base,
            dex: "uniswap-v2".to_owned(),
            pool: candidate.clone(),
            base_symbol: "TOKEN".to_owned(),
            base_address: token.clone(),
            quote_symbol: "USDG".to_owned(),
            quote_address: "0x0000000000000000000000000000000000000012".to_owned(),
            issuer: Some("Issuer".to_owned()),
            ticker: Some("TOKEN".to_owned()),
            verdict: "verified".to_owned(),
            quote_balance: Some("100".to_owned()),
            quote_share_of_supply: Some(0.1),
            volume_24h_usd: None,
            liquidity_usd: None,
            curated: false,
            note: None,
            updated_at: "2026-09-27T00:00:00Z".to_owned(),
        }];

        let result = check_uncached(&state, &token).await;
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
        impl PoolReader for FailingReader {
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

        let signing_key = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        let state = AppState {
            registry: std::sync::Arc::new(tokio::sync::RwLock::new(vec![entry(
                "USDG",
                "0x0000000000000000000000000000000000000012",
            )])),
            registry_status: std::sync::Arc::new(tokio::sync::RwLock::new(
                crate::discovery::RegistrySnapshot::default(),
            )),
            readers: std::sync::Arc::new(vec![Box::new(FailingReader) as Box<dyn PoolReader>]),
            http: reqwest::Client::new(),
            check_cache: moka::future::Cache::builder().build(),
            leaderboard_check_cache: moka::future::Cache::builder().build(),
            check_inflight: std::sync::Arc::new(tokio::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            featured: std::sync::Arc::new(tokio::sync::RwLock::new(Vec::new())),
            featured_status: std::sync::Arc::new(tokio::sync::RwLock::new(
                crate::discovery::FeaturedStatus::default(),
            )),
            leaderboard: std::sync::Arc::new(tokio::sync::RwLock::new(
                crate::discovery::Leaderboard::default(),
            )),
            prices: std::sync::Arc::new(tokio::sync::RwLock::new(
                crate::discovery::PriceSnapshot::default(),
            )),
            attestations: std::sync::Arc::new(std::sync::RwLock::new(
                std::collections::HashMap::new(),
            )),
            signing_key: std::sync::Arc::new(signing_key),
            dev_signer: true,
            attest_store: std::sync::Arc::new(crate::attest::FileAttestationStore::new(
                std::path::PathBuf::from("data/attestations"),
            )),
            registry_hash: std::sync::Arc::new(std::sync::RwLock::new(String::new())),
            registry_api_cache: std::sync::Arc::new(tokio::sync::RwLock::new(None)),
            public_url: std::sync::Arc::new("http://localhost:3000".to_owned()),
            rate_limiter: std::sync::Arc::new(crate::state::RateLimiter::default()),
            expensive_concurrency: std::sync::Arc::new(tokio::sync::Semaphore::new(32)),
            wallet_concurrency: std::sync::Arc::new(tokio::sync::Semaphore::new(1)),
            registry_api_concurrency: std::sync::Arc::new(tokio::sync::Semaphore::new(4)),
        };
        let result = check_uncached(&state, "0x00000000000000000000000000000000000000ff").await;
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
        let signing_key = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let state = AppState {
            registry: std::sync::Arc::new(tokio::sync::RwLock::new(Vec::new())),
            registry_status: std::sync::Arc::new(tokio::sync::RwLock::new(
                crate::discovery::RegistrySnapshot::default(),
            )),
            readers: std::sync::Arc::new(Vec::new()),
            http: reqwest::Client::new(),
            check_cache: moka::future::Cache::builder().build(),
            leaderboard_check_cache: moka::future::Cache::builder().build(),
            check_inflight: std::sync::Arc::new(tokio::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            featured: std::sync::Arc::new(tokio::sync::RwLock::new(Vec::new())),
            featured_status: std::sync::Arc::new(tokio::sync::RwLock::new(
                crate::discovery::FeaturedStatus::default(),
            )),
            leaderboard: std::sync::Arc::new(tokio::sync::RwLock::new(
                crate::discovery::Leaderboard::default(),
            )),
            prices: std::sync::Arc::new(tokio::sync::RwLock::new(
                crate::discovery::PriceSnapshot::default(),
            )),
            attestations: std::sync::Arc::new(std::sync::RwLock::new(
                std::collections::HashMap::new(),
            )),
            signing_key: std::sync::Arc::new(signing_key),
            dev_signer: true,
            attest_store: std::sync::Arc::new(crate::attest::FileAttestationStore::new(
                std::path::PathBuf::from("data/attestations"),
            )),
            registry_hash: std::sync::Arc::new(std::sync::RwLock::new(String::new())),
            registry_api_cache: std::sync::Arc::new(tokio::sync::RwLock::new(None)),
            public_url: std::sync::Arc::new("http://localhost:3000".to_owned()),
            rate_limiter: std::sync::Arc::new(crate::state::RateLimiter::default()),
            expensive_concurrency: std::sync::Arc::new(tokio::sync::Semaphore::new(32)),
            wallet_concurrency: std::sync::Arc::new(tokio::sync::Semaphore::new(1)),
            registry_api_concurrency: std::sync::Arc::new(tokio::sync::Semaphore::new(4)),
        };
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
