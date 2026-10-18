use crate::domain::chain::Chain;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) struct AssetKey {
    pub chain: Chain,
    pub kind: SubjectKind,
    pub address: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SubjectKind {
    Pool,
    Token,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Facet {
    ContractMatch,
    Powers,
    Guard,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Outcome {
    Verified,
    Mismatch,
    NoMatch,
    ControlsComplete,
    ControlsPartial,
    Allow,
    Deny,
    Unknown,
    RegistryStale,
    RegistryRemoved,
    Unsupported,
    Miss,
    Rejected,
}
impl Outcome {
    pub fn label(self) -> &'static str {
        match self {
            Self::Verified => "Verified",
            Self::Mismatch => "Mismatch",
            Self::NoMatch => "No match",
            Self::ControlsComplete => "Controls observed",
            Self::ControlsPartial => "Other controls incomplete",
            Self::Allow => "Allow",
            Self::Deny => "Deny",
            Self::Unknown => "Unknown",
            Self::RegistryStale => "Registry stale",
            Self::RegistryRemoved => "Registry removed",
            Self::Unsupported => "Unsupported",
            Self::Miss => "Miss",
            Self::Rejected => "Read not executed",
        }
    }
    pub fn confirms(self) -> bool {
        !matches!(
            self,
            Self::Miss
                | Self::ControlsPartial
                | Self::RegistryStale
                | Self::RegistryRemoved
                | Self::Unsupported
                | Self::Rejected
        )
    }
}
impl AssetKey {
    pub fn new(chain: Chain, kind: SubjectKind, address: &str) -> Option<Self> {
        let address = if chain == Chain::Solana {
            bs58::encode(Chain::decode_solana_address(address)?).into_string()
        } else if Chain::is_evm_address(address)
            || (kind == SubjectKind::Pool && Chain::is_v4_pool_id(address))
        {
            address.to_ascii_lowercase()
        } else {
            return None;
        };
        Some(Self { chain, kind, address })
    }
    pub fn path(&self) -> String {
        let chain = match self.chain {
            Chain::Solana => "solana",
            Chain::RobinhoodChain => "robinhood",
            Chain::Base => "base",
            Chain::Ethereum => "ethereum",
            Chain::Bnb => "bnb",
        };
        let kind = match self.kind {
            SubjectKind::Pool => "pool",
            SubjectKind::Token => "token",
        };
        format!("/assets/{chain}/{kind}/{}", self.address)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Observation {
    pub event_id: String,
    pub asset: AssetKey,
    pub facet: Facet,
    pub observed_at: DateTime<Utc>,
    pub outcome: Outcome,
    pub miss_reason: Option<String>,
    pub attestation_id: Option<String>,
    pub block: Option<u64>,
    pub slot: Option<u64>,
    pub source: String,
    pub registry_hash: Option<String>,
    pub ticker: Option<String>,
    #[serde(default)]
    pub evidence: serde_json::Value,
    pub signed_document: Option<serde_json::Value>,
    /// Only scheduled misses are collapsed within their UTC hour.
    pub scheduled_slot: Option<i64>,
}
impl Observation {
    /// Public projection/cache metadata never retains evidence or signed document bodies.
    pub fn summary(&self) -> Self {
        Self {
            event_id: self.event_id.clone(),
            asset: self.asset.clone(),
            facet: self.facet,
            observed_at: self.observed_at,
            outcome: self.outcome,
            miss_reason: self.miss_reason.clone(),
            attestation_id: self.attestation_id.clone(),
            block: self.block,
            slot: self.slot,
            source: self.source.clone(),
            registry_hash: self.registry_hash.clone(),
            ticker: self.ticker.clone(),
            evidence: serde_json::Value::Null,
            signed_document: None,
            scheduled_slot: self.scheduled_slot,
        }
    }
    /// Non-confirming anonymous attempts cannot create new public identities.
    /// A completed Unknown may update tracked state, but cannot start on-demand tracking.
    pub fn projects(&self, tracked: bool) -> bool {
        self.outcome != Outcome::Rejected
            && (!matches!(self.outcome, Outcome::Miss | Outcome::ControlsPartial)
                || self.scheduled_slot.is_some())
            && (tracked
                || (self.outcome.confirms() && self.outcome != Outcome::Unknown)
                || self.scheduled_slot.is_some())
    }
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct AssetState {
    pub last_confirmed: Option<Arc<Observation>>,
    pub latest: Option<Arc<Observation>>,
    pub consecutive_misses: u32,
    pub next_retry_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub availability: Option<Outcome>,
    #[serde(default)]
    pub last_miss_slot: Option<i64>,
}
impl AssetState {
    pub fn label(&self) -> &'static str {
        if let Some(availability) = self.availability {
            availability.label()
        } else if self.consecutive_misses >= 3 {
            "Unverified"
        } else {
            self.last_confirmed.as_ref().map_or("Not read yet", |event| event.outcome.label())
        }
    }
    pub fn summary(&self) -> String {
        let mut text = self.label().to_owned();
        if let Some(confirmed) = &self.last_confirmed {
            if self.consecutive_misses >= 3 || self.availability.is_some() {
                text.push_str(&format!(
                    " · last confirmed {} {}",
                    confirmed.outcome.label(),
                    confirmed.observed_at.format("%-d %b %Y, %H:%M UTC")
                ));
            } else {
                text.push_str(&format!(
                    " · last confirmed {}",
                    confirmed.observed_at.format("%-d %b %Y, %H:%M UTC")
                ));
            }
        }
        if self.consecutive_misses > 0 {
            if self.consecutive_misses < 3 {
                text.push_str(&format!(" · retrying ({}/3)", self.consecutive_misses));
            } else {
                text.push_str(&format!(
                    " · retrying · {} consecutive misses",
                    self.consecutive_misses
                ));
            }
        }
        text
    }
}
/// Ordered operational projection. Older imports remain history, never rewind live state.
pub(crate) fn reduce(previous: &AssetState, event: &Arc<Observation>) -> AssetState {
    if event.outcome == Outcome::Rejected
        || (matches!(event.outcome, Outcome::Miss | Outcome::ControlsPartial)
            && event.scheduled_slot.is_none())
    {
        return previous.clone();
    }
    if previous.latest.as_ref().is_some_and(|latest| {
        latest.observed_at > event.observed_at || latest.event_id == event.event_id
    }) {
        return previous.clone();
    }
    let mut next = previous.clone();
    let summary = if event.evidence.is_null() && event.signed_document.is_none() {
        Arc::clone(event)
    } else {
        Arc::new(event.summary())
    };
    if matches!(event.outcome, Outcome::Miss | Outcome::ControlsPartial) {
        let slot = event.scheduled_slot.expect("scheduled miss");
        if previous.last_miss_slot.is_some_and(|old| slot <= old) {
            return next;
        }
        next.last_miss_slot = Some(slot);
        next.consecutive_misses = next.consecutive_misses.saturating_add(1);
        next.next_retry_at = Some(event.observed_at + Duration::hours(1));
    } else {
        next.consecutive_misses = 0;
        next.next_retry_at = None;
        next.availability = (!event.outcome.confirms()).then_some(event.outcome);
        if event.outcome.confirms() {
            next.last_confirmed = Some(Arc::clone(&summary));
        }
    }
    next.latest = Some(summary);
    next
}

#[cfg(test)]
mod tests {
    use super::*;
    pub(super) fn event(outcome: Outcome, hour: i64) -> Arc<Observation> {
        Arc::new(Observation {
            event_id: format!("event-{hour}-{outcome:?}"),
            asset: AssetKey::new(
                Chain::Base,
                SubjectKind::Pool,
                "0xAa00000000000000000000000000000000000001",
            )
            .unwrap(),
            facet: Facet::ContractMatch,
            observed_at: DateTime::from_timestamp(1_790_000_000 + hour * 3600, 0).unwrap(),
            outcome,
            miss_reason: (outcome == Outcome::Miss).then(|| "timeout".into()),
            attestation_id: Some("proof".into()),
            block: None,
            slot: None,
            source: "test".into(),
            registry_hash: None,
            ticker: None,
            evidence: serde_json::json!({}),
            signed_document: None,
            scheduled_slot: Some((1_790_000_000 + hour * 3600) / 3600),
        })
    }
    #[test]
    fn keeps_confirmation_until_third_miss_and_recovers_immediately() {
        let confirmed = event(Outcome::Verified, 0);
        let mut state = reduce(&AssetState::default(), &confirmed);
        for hour in 1..=3 {
            state = reduce(&state, &event(Outcome::Miss, hour));
            assert_eq!(state.last_confirmed.as_ref().unwrap().observed_at, confirmed.observed_at);
            assert_eq!(state.label(), if hour < 3 { "Verified" } else { "Unverified" });
        }
        for (hour, outcome, label) in [
            (4, Outcome::Mismatch, "Mismatch"),
            (5, Outcome::NoMatch, "No match"),
            (6, Outcome::Verified, "Verified"),
        ] {
            state = reduce(&state, &event(outcome, hour));
            assert_eq!(state.label(), label);
            assert_eq!(state.consecutive_misses, 0);
        }
    }
    #[test]
    fn never_confirmed_slots_registry_and_old_import() {
        let mut state = AssetState::default();
        for hour in 0..3 {
            state = reduce(&state, &event(Outcome::Miss, hour));
            assert_eq!(state.label(), if hour < 2 { "Not read yet" } else { "Unverified" });
        }
        state = reduce(&state, &event(Outcome::Verified, 4));
        state = reduce(&state, &event(Outcome::RegistryRemoved, 5));
        assert_eq!(state.label(), "Registry removed");
        assert_eq!(state.last_confirmed.as_ref().unwrap().outcome, Outcome::Verified);
        assert_eq!(reduce(&state, &event(Outcome::Verified, 1)).label(), "Registry removed");
        let mut miss = event(Outcome::Miss, 6);
        state = reduce(&state, &miss);
        assert_eq!(
            state.label(),
            "Registry removed",
            "later RPC misses cannot resurrect a removed publisher claim"
        );
        Arc::make_mut(&mut miss).event_id = "another".into();
        assert_eq!(reduce(&state, &miss).consecutive_misses, 1);
    }
    #[test]
    fn on_demand_misses_and_unexecuted_rejections_never_advance_public_status() {
        let verified = reduce(&AssetState::default(), &event(Outcome::Verified, 0));
        let mut state = verified.clone();
        for hour in 1..=3 {
            let mut miss = event(Outcome::Miss, hour);
            Arc::make_mut(&mut miss).scheduled_slot = None;
            state = reduce(&state, &miss);
            state = reduce(&state, &event(Outcome::Rejected, hour));
        }
        assert_eq!(state.consecutive_misses, 0);
        assert_eq!(
            state.latest.as_ref().unwrap().event_id,
            verified.latest.as_ref().unwrap().event_id
        );
        assert_eq!(state.label(), "Verified");
        let mut mismatch = event(Outcome::Mismatch, 4);
        Arc::make_mut(&mut mismatch).scheduled_slot = None;
        assert_eq!(reduce(&state, &mismatch).label(), "Mismatch");
    }
    #[test]
    fn identity_keeps_chain_kind_and_solana_case() {
        let base = event(Outcome::Verified, 0).asset.clone();
        assert_eq!(base.address, "0xaa00000000000000000000000000000000000001");
        assert_ne!(base, AssetKey::new(Chain::Ethereum, SubjectKind::Pool, &base.address).unwrap());
        assert_ne!(base, AssetKey::new(Chain::Base, SubjectKind::Token, &base.address).unwrap());
        let sol = bs58::encode([42; 32]).into_string();
        let key = AssetKey::new(Chain::Solana, SubjectKind::Token, &sol).unwrap();
        assert_eq!(key.address, sol);
        assert_ne!(key.address, sol.to_ascii_lowercase());
    }
}
