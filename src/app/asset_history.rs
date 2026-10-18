use crate::{
    app::context::Context,
    domain::{
        asset_history::{AssetKey, AssetState, Facet, Observation, Outcome, SubjectKind},
        chain::Chain,
        check::{CheckReadIssue, CheckResult, Verdict},
        powers::PowersRecord,
    },
    ports::{current_rpc_request_context, rpc_budget_rejected, rpc_crawler_blocked},
};
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};

/// A request/render deadline must not cancel an already-owned read or durable capture.
/// Keep its original budget flags and admission class when the caller disconnects.
pub(crate) async fn finish_execution<F, T>(future: F) -> T
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let context = current_rpc_request_context();
    tokio::spawn(async move {
        match context {
            Some(context) => crate::ports::with_rpc_request_context(context, future).await.0,
            None => future.await,
        }
    })
    .await
    .expect("observation execution task panicked")
}
pub(crate) fn event(
    state: &Context,
    asset: AssetKey,
    facet: Facet,
    outcome: Outcome,
    observed_at: DateTime<Utc>,
    evidence: serde_json::Value,
) -> Observation {
    let scheduled_slot = current_rpc_request_context()
        .filter(|context| !context.on_demand)
        .map(|_| observed_at.timestamp() / 3600);
    let nonce: u128 = rand::random();
    let identity = if scheduled_slot.is_some()
        && matches!(outcome, Outcome::Miss | Outcome::ControlsPartial)
    {
        format!("{asset:?}:{facet:?}:miss:{}", scheduled_slot.unwrap_or_default())
    } else {
        format!("{asset:?}:{facet:?}:{observed_at}:{nonce}")
    };
    let event_id = crate::domain::attestation::hex_lower(&Sha256::digest(identity.as_bytes()));
    Observation {
        event_id,
        asset,
        facet,
        outcome,
        observed_at,
        miss_reason: None,
        attestation_id: None,
        block: None,
        slot: None,
        source: if scheduled_slot.is_some() { "background" } else { "on_demand" }.into(),
        registry_hash: Some(
            state.registry_hash.read().unwrap_or_else(std::sync::PoisonError::into_inner).clone(),
        ),
        ticker: None,
        evidence,
        signed_document: None,
        scheduled_slot,
    }
}
pub(crate) async fn record(state: &Context, mut observation: Observation) {
    if crate::ports::rpc_reads_rejected()
        || observation.miss_reason.as_deref() == Some("budget_rejection")
    {
        if observation.scheduled_slot.is_some() {
            observation.outcome = Outcome::Miss;
            observation.miss_reason = Some("budget_rejection".into());
        } else {
            observation.outcome = Outcome::Rejected;
            observation.miss_reason = Some("read_not_executed".into());
        }
    }
    // Adapter emits only a rate-limited transition log; errors never include credentials or evidence.
    let _ = state.history.append(observation).await;
}
pub(crate) async fn check(state: &Context, result: &CheckResult, known_chain: Option<Chain>) {
    if rpc_crawler_blocked() {
        return;
    }
    let chain = known_chain
        .or_else(|| result.pool.as_ref().map(|pool| pool.chain))
        .or_else(|| Chain::detect(&result.input));
    let Some(chain) = chain else {
        return;
    };
    let address = result.pool.as_ref().map_or(result.input.as_str(), |pool| pool.pool.as_str());
    let Some(asset) = AssetKey::new(chain, SubjectKind::Pool, address) else {
        return;
    };
    let outcome = if rpc_budget_rejected() {
        Outcome::Miss
    } else {
        match result.read_issue {
            Some(CheckReadIssue::Transient | CheckReadIssue::RpcLimit) => Outcome::Miss,
            Some(CheckReadIssue::UnsupportedVenue | CheckReadIssue::Unsupported) => {
                Outcome::Unsupported
            }
            None => match result.verdict {
                Verdict::Verified { .. } => Outcome::Verified,
                Verdict::Mismatch { .. } => Outcome::Mismatch,
                Verdict::NoMatch => Outcome::NoMatch,
                Verdict::Unknown { .. } => Outcome::RegistryStale,
            },
        }
    };
    let observed_at = DateTime::parse_from_rfc3339(&result.checked_at)
        .map(|time| time.with_timezone(&Utc))
        .unwrap_or_else(|_| state.clock.now());
    let mut observation = event(
        state,
        asset,
        Facet::ContractMatch,
        outcome,
        observed_at,
        serde_json::to_value(result).unwrap_or_default(),
    );
    observation.miss_reason = (outcome == Outcome::Miss)
        .then(|| if rpc_budget_rejected() { "budget_rejection" } else { "read_failure" }.into());
    observation.attestation_id = result.attestation_id.clone();
    if let Some(id) = &result.attestation_id
        && let Some(attestation) =
            state.attestations.read().unwrap_or_else(std::sync::PoisonError::into_inner).get(id)
    {
        observation.block = attestation.block;
        observation.slot = attestation.slot;
        observation.ticker = attestation.ticker.clone();
        observation.signed_document = serde_json::to_value(attestation).ok();
    }
    record(state, observation).await;
}
pub(crate) async fn powers(state: &Context, record_value: &PowersRecord, complete: bool) {
    if rpc_crawler_blocked() {
        return;
    }
    let Some(asset) = AssetKey::new(record_value.chain, SubjectKind::Token, &record_value.contract)
    else {
        return;
    };
    let time = DateTime::parse_from_rfc3339(&record_value.observed_at)
        .map(|time| time.with_timezone(&Utc))
        .unwrap_or_else(|_| state.clock.now());
    let mut observation = event(
        state,
        asset,
        Facet::Powers,
        if complete { Outcome::ControlsComplete } else { Outcome::ControlsPartial },
        time,
        serde_json::to_value(record_value).unwrap_or_default(),
    );
    observation.block = record_value.block;
    observation.slot = record_value.slot;
    let registry = state.registry.snapshot().await;
    observation.ticker =
        crate::domain::registry::lookup(&registry, record_value.chain, &record_value.contract)
            .map(|entry| entry.ticker.clone());
    if !complete {
        observation.miss_reason = Some("controls_incomplete".into());
    }
    record(state, observation).await;
}
pub(crate) async fn miss(state: &Context, asset: AssetKey, facet: Facet, reason: &str) {
    if rpc_crawler_blocked() {
        return;
    }
    let mut observation =
        event(state, asset, facet, Outcome::Miss, state.clock.now(), serde_json::json!({}));
    observation.miss_reason = Some(reason.into());
    record(state, observation).await;
}
pub(crate) fn projection(state: &Context, asset: &AssetKey, facet: Facet) -> Option<AssetState> {
    state.history.state(asset, facet)
}
/// Re-evaluate publisher availability independently: it is not an RPC failure.
pub(crate) async fn registry_changed(state: &Context) {
    let registry = state.registry.snapshot().await;
    for (asset, facet, projection) in state.history.states() {
        if facet != Facet::ContractMatch || projection.label() != "Verified" {
            continue;
        }
        let Some(confirmed) = projection.last_confirmed else {
            continue;
        };
        let Some(id) = confirmed.attestation_id.as_ref() else {
            continue;
        };
        let Ok(Some(attestation)) = state.attest_store.get(id).await else {
            continue;
        };
        if !crate::app::attestation::current_registry_entry(&registry, &attestation) {
            let stale = attestation.registry_entry.as_ref().is_some_and(|recorded| {
                registry.iter().any(|entry| {
                    entry.chain == recorded.chain
                        && entry.issuer == recorded.issuer
                        && entry.removed_at.is_none()
                        && entry.stale_since.is_some()
                        && crate::domain::check::same_contract(
                            &entry.contract,
                            &recorded.contract,
                            entry.chain,
                        )
                })
            });
            record(
                state,
                event(
                    state,
                    asset,
                    facet,
                    if stale { Outcome::RegistryStale } else { Outcome::RegistryRemoved },
                    state.clock.now(),
                    serde_json::json!({"reason":"publisher deployment no longer current"}),
                ),
            )
            .await;
        }
    }
}

pub(crate) fn already_missed_slot(
    state: &Context,
    asset: &AssetKey,
    facet: Facet,
) -> Option<std::sync::Arc<Observation>> {
    let context = current_rpc_request_context()?;
    if context.on_demand {
        return None;
    }
    let latest = projection(state, asset, facet)?.latest?;
    (latest.scheduled_slot == Some(state.clock.now().timestamp() / 3600)
        && matches!(latest.outcome, Outcome::Miss | Outcome::ControlsPartial))
    .then_some(latest)
}

/// Retry policy lives with the projection. Adapters supply admission state and trigger work.
pub(crate) fn due_retries(state: &Context) -> Vec<(AssetKey, Facet)> {
    let now = state.clock.now();
    state
        .history
        .states()
        .into_iter()
        .filter(|(asset, facet, projection)| {
            projection.next_retry_at.is_some_and(|due| due <= now)
                && already_missed_slot(state, asset, *facet).is_none()
                && state.readers.iter().any(|reader| reader.chain() == asset.chain)
        })
        .map(|(asset, facet, _)| (asset, facet))
        .collect()
}

pub(crate) async fn retry(state: &Context, asset: AssetKey, facet: Facet, budget_exhausted: bool) {
    if budget_exhausted {
        miss(state, asset, facet, "budget_rejection").await;
        return;
    }
    match facet {
        Facet::ContractMatch if asset.kind == SubjectKind::Pool => {
            let key =
                format!("{}:{}", asset.chain, crate::domain::check::cache_key(&asset.address));
            state.check_cache.invalidate(&key).await;
            let _ = crate::app::check::check_on_chain(state, asset.chain, &asset.address).await;
        }
        Facet::Powers => {
            let key = (asset.chain, asset.address.clone());
            state.powers_cache.invalidate(&key).await;
            state.powers_retry_cache.invalidate(&key).await;
            state.powers_failure_cache.invalidate(&key).await;
            let _ =
                crate::app::powers::inspect_for_warm_refresh(state, asset.chain, &asset.address)
                    .await;
        }
        Facet::Guard => {
            let key = (
                asset.chain,
                crate::domain::check::cache_key(&asset.address),
                state.registry_version.load(std::sync::atomic::Ordering::Acquire),
            );
            state.guard_cache.invalidate(&key).await;
            let _ = crate::app::guard::create(state, &asset.address, asset.chain, None).await;
        }
        _ => {}
    }
}
