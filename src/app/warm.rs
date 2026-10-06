use crate::{
    app::{
        context::Context,
        powers::{canonical_contract, inspect_for_warm_refresh},
    },
    domain::{
        chain::Chain,
        pool::PoolError,
        powers::{PowersRecord, SourceVerified},
        registry,
    },
};
use chrono::Utc;
use std::{
    collections::{BTreeMap, HashSet},
    sync::atomic::Ordering,
    time::{Duration, Instant},
};
use tokio::sync::Notify;
use tracing::info;

pub(crate) const POWERS_CACHE_TTL: Duration = Duration::from_secs(30 * 60);
pub(crate) const POWERS_WARM_INTERVAL: Duration = Duration::from_secs(25 * 60);
const POWERS_CACHE_REFRESH_AGE: Duration =
    Duration::from_secs(POWERS_CACHE_TTL.as_secs() - POWERS_WARM_INTERVAL.as_secs());
pub(crate) const POWERS_WARM_MAX_CONTRACTS: usize = 160;
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

pub(crate) fn powers_record_needs_warm_refresh(
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
        for entry in registry
            .iter()
            .filter(|entry| registry::matchable(entry) && entry.ticker.eq_ignore_ascii_case(ticker))
        {
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

async fn current_warm_targets(state: &Context) -> WarmTargetSelection {
    let tickers = state.pool_index.warm_tickers().await;
    let registry = state.registry.snapshot().await;
    select_warm_targets(&registry, &tickers, POWERS_WARM_MAX_CONTRACTS)
}

pub(crate) async fn notify_powers_warm_if_targets(state: &Context, notify: &Notify) -> bool {
    if current_warm_targets(state).await.targets.is_empty() {
        return false;
    }
    notify.notify_one();
    true
}

fn warm_chain_counts_mut(summary: &mut WarmPassSummary, chain: Chain) -> &mut WarmChainSummary {
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

pub(crate) async fn warm_current_pool_powers(state: &Context) -> WarmPassSummary {
    let started = Instant::now();
    let selection = current_warm_targets(state).await;
    let mut summary = WarmPassSummary {
        warmed: !selection.targets.is_empty(),
        target_count: selection.targets.len(),
        tickers_covered: selection.tickers_covered,
        cap_hit: selection.cap_hit,
        ..WarmPassSummary::default()
    };
    let version = state.registry_version.load(Ordering::Acquire);
    for (chain, contract) in selection.targets {
        warm_target(state, chain, contract, version, &mut summary).await;
    }

    log_summary(&summary, started.elapsed());
    summary
}

async fn warm_target(
    state: &Context,
    chain: Chain,
    contract: String,
    version: u64,
    summary: &mut WarmPassSummary,
) {
    let key = (chain, contract.clone(), version);
    let cached_record = state.powers_cache.get(&key).await;
    let refresh_cached = cached_record
        .as_ref()
        .is_some_and(|record| powers_record_needs_warm_refresh(record, &state.clock.now()));
    if cached_record.is_some() && !refresh_cached {
        summary.skipped += 1;
        return;
    }
    if state.powers_retry_cache.get(&key).await.is_some()
        || state.powers_failure_cache.get(&key).await.is_some()
    {
        summary.skipped += 1;
        return;
    }

    let Some(_permit) = warm_permit(state).await else {
        record_warm_transient(summary, chain, "prefetch_semaphore_closed");
        return;
    };
    if refresh_cached
        && state
            .powers_cache
            .get(&key)
            .await
            .is_some_and(|record| !powers_record_needs_warm_refresh(&record, &state.clock.now()))
    {
        summary.skipped += 1;
        return;
    }
    match inspect_for_warm_refresh(state, chain, &contract).await {
        Ok(record) => {
            let current_key = (chain, contract, state.registry_version.load(Ordering::Acquire));
            if let Some(record) = state.powers_retry_cache.get(&current_key).await {
                record_transient_record(summary, chain, &record);
            } else if let Some(record) = state.powers_cache.get(&current_key).await {
                record_warm_success(summary, chain, &record);
            } else {
                let reason = if state.registry_version.load(Ordering::Acquire) != version {
                    "registry_changed"
                } else if record.source_verified == SourceVerified::Unavailable
                    || record.source_verified_proxy == Some(SourceVerified::Unavailable)
                {
                    "source_unavailable"
                } else {
                    "rpc_unavailable"
                };
                record_warm_transient(summary, chain, reason);
            }
        }
        Err(error) => record_warm_transient(summary, chain, transient_pool_error_code(&error)),
    }
}

fn log_summary(summary: &WarmPassSummary, elapsed: Duration) {
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
        elapsed_ms = elapsed.as_millis(),
        "powers warm pass completed"
    );
}
async fn warm_permit(state: &Context) -> Option<tokio::sync::OwnedSemaphorePermit> {
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
#[cfg(test)]
mod tests {
    use super::*;
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
            last_checked: "2026-10-01T00:00:00Z".to_owned(),
            removed_at: None,
            stale_since: None,
        }
    }

    #[test]
    fn warm_targets_prioritize_ticker_order_and_keep_ticker_groups() {
        let tickers = ["FEAT", "FIRST", "LATER"].into_iter().map(str::to_owned).collect::<Vec<_>>();

        let registry = vec![
            warm_registry_entry(Chain::Base, "LATER", "0x0000000000000000000000000000000000000003"),
            warm_registry_entry(Chain::Base, "FIRST", "0x0000000000000000000000000000000000000001"),
            warm_registry_entry(
                Chain::Ethereum,
                "FIRST",
                "0x0000000000000000000000000000000000000002",
            ),
            warm_registry_entry(Chain::Solana, "FEAT", "11111111111111111111111111111111"),
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
