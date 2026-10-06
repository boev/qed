use crate::domain::{
    chain::Chain,
    pool::{PoolInfo, TokenMeta, TokenSide},
    powers::PowersRecord,
    registry::Entry,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub enum Verdict {
    Verified { issuer: String, ticker: String },
    NoMatch,
    Mismatch { claimed: String, actual: String },
    Unknown { reason: String },
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
    pub powers: Option<PowersRecord>,
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
pub(crate) fn selected_sides<'a>(
    pool: &'a PoolInfo,
    entries: &[Entry],
) -> (&'a TokenSide, &'a TokenSide) {
    let quote_is_registry = !matches!(
        crate::domain::registry::match_status(entries, pool.chain, &pool.quote.address),
        crate::domain::registry::MatchStatus::NotFound
    );
    if quote_is_registry {
        (&pool.base, &pool.quote)
    } else if !matches!(
        crate::domain::registry::match_status(entries, pool.chain, &pool.base.address),
        crate::domain::registry::MatchStatus::NotFound
    ) {
        (&pool.quote, &pool.base)
    } else {
        (&pool.base, &pool.quote)
    }
}

pub(crate) fn registered_token_address<'a>(
    pool: &'a PoolInfo,
    entries: &[Entry],
) -> Option<&'a str> {
    let (_, token_side) = selected_sides(pool, entries);
    crate::domain::registry::lookup(entries, pool.chain, &token_side.address)
        .map(|_| token_side.address.as_str())
}

pub(crate) struct PoolEvaluation {
    pub(crate) verdict: Verdict,
    pub(crate) quote_share_of_supply: Option<f64>,
    pub(crate) evidence: Vec<String>,
    pub(crate) claimed_entry: Option<Entry>,
    pub(crate) quote_address: String,
}

pub(crate) fn evaluate_pool(
    pool: &PoolInfo,
    base_side: &TokenSide,
    quote_side: &TokenSide,
    base_meta: Option<&TokenMeta>,
    quote_meta: Option<&TokenMeta>,
    entries: &[Entry],
) -> PoolEvaluation {
    let quote_entry = crate::domain::registry::lookup(entries, pool.chain, &quote_side.address);
    let base_entry = crate::domain::registry::lookup(entries, pool.chain, &base_side.address);
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
        let status =
            crate::domain::registry::match_status(entries, pool.chain, &quote_side.address);
        if let crate::domain::registry::MatchStatus::Removed { issuer, removed_at } = status {
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
        if let crate::domain::registry::MatchStatus::Stale { since } = status {
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
        crate::domain::registry::matchable(entry)
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
pub(crate) fn claims_ticker(value: &str, ticker: &str) -> bool {
    claims_symbol(value, ticker, "")
}
pub(crate) fn claims_symbol(value: &str, ticker: &str, name: &str) -> bool {
    let value = normalise_claim(value);
    let ticker = normalise_registry(ticker);
    let name = normalise_registry(name);
    !value.is_empty() && (value == ticker || (!name.is_empty() && value == name))
}

pub(crate) fn claims_name(value: &str, ticker: &str, name: &str) -> bool {
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

pub(crate) fn quote_share_of_supply(
    balance: Option<&str>,
    total_supply: Option<&str>,
) -> Option<f64> {
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

pub(crate) fn quote_share_unavailable_reason(
    balance: Option<&str>,
    total_supply: Option<&str>,
) -> Option<&'static str> {
    let balance = balance?.parse::<f64>().ok()?;
    let total_supply = total_supply?.parse::<f64>().ok()?;
    (balance.is_finite() && total_supply.is_finite() && balance > total_supply)
        .then_some("quote balance exceeds total supply, so token reads have inconsistent scaling")
}

pub(crate) fn bytecode_similarity(left: &[u8], right: &[u8]) -> f64 {
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

pub(crate) fn same_contract(left: &str, right: &str, chain: Chain) -> bool {
    if chain == Chain::Solana { left == right } else { left.eq_ignore_ascii_case(right) }
}

pub(crate) fn decimal_cmp(left: Option<&str>, right: Option<&str>) -> std::cmp::Ordering {
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
