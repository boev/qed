use crate::domain::chain::Chain;
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OfficialDeployment {
    pub network: String,
    pub address: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wrapper_address: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wrapper_address_v2: Option<String>,
}

pub fn official_deployment_matches(entry: &Entry, chain: Chain, contract: &str) -> bool {
    entry.official_deployments.iter().any(|deployment| {
        deployment_network_matches(&deployment.network, chain)
            && [
                Some(deployment.address.as_str()),
                deployment.wrapper_address.as_deref(),
                deployment.wrapper_address_v2.as_deref(),
            ]
            .into_iter()
            .flatten()
            .any(|address| {
                normalize_contract(chain, address) == normalize_contract(chain, contract)
            })
    })
}

pub fn official_networks(entry: &Entry) -> Vec<String> {
    let mut networks = entry
        .official_deployments
        .iter()
        .map(|deployment| {
            Chain::from_network_name(&deployment.network)
                .map(|chain| chain.to_string())
                .unwrap_or_else(|| deployment.network.clone())
        })
        .collect::<Vec<_>>();
    networks.sort_by_key(|network| network.to_ascii_lowercase());
    networks.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
    networks
}

fn deployment_network_matches(network: &str, chain: Chain) -> bool {
    Chain::from_network_name(network) == Some(chain)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub issuer: String,
    pub ticker: String,
    pub name: String,
    pub chain: Chain,
    pub contract: String,
    pub decimals: Option<u8>,
    pub source: String,
    pub source_url: String,
    pub last_checked: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_since: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub official_deployments: Vec<OfficialDeployment>,
}

pub type Registry = Vec<Entry>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchStatus {
    Active,
    Removed { issuer: String, removed_at: String },
    Stale { since: String },
    NotFound,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotRejected {
    pub issuer: String,
    pub previous: usize,
    pub incoming: usize,
}

/// Replace one issuer's registry entries with a complete, validated snapshot.
/// Entries omitted from a successful snapshot remain as tombstones so an old
/// contract can no longer match by accident.
pub fn reconcile_snapshot(
    registry: &mut Registry,
    issuer: &str,
    incoming: impl IntoIterator<Item = Entry>,
    checked_at: &str,
) -> Result<usize, SnapshotRejected> {
    let incoming = incoming.into_iter().collect::<Vec<_>>();
    let previous =
        registry.iter().filter(|entry| entry.issuer == issuer && matchable(entry)).count();
    let incoming_keys = incoming.iter().map(key).collect::<std::collections::HashSet<_>>();
    let snapshot_is_valid = incoming_keys.len() == incoming.len()
        && incoming.iter().all(|entry| {
            entry.issuer == issuer && source_timestamp_is_fresh(&entry.last_checked, checked_at)
        });
    if !snapshot_is_valid || incoming.len() * 2 < previous {
        return Err(SnapshotRejected {
            issuer: issuer.to_owned(),
            previous,
            incoming: incoming.len(),
        });
    }
    for entry in registry.iter_mut().filter(|entry| entry.issuer == issuer) {
        if !incoming_keys.contains(&key(entry)) {
            entry.removed_at.get_or_insert_with(|| checked_at.to_owned());
            entry.stale_since = None;
        }
    }
    for mut entry in incoming {
        entry.removed_at = None;
        entry.stale_since = None;
        if let Some(existing) = registry
            .iter_mut()
            .find(|candidate| candidate.issuer == issuer && key(candidate) == key(&entry))
        {
            *existing = entry;
        } else {
            registry.push(entry);
        }
    }
    Ok(previous)
}

/// Preserve a failed source snapshot, marking entries older than 48 hours as
/// stale without changing their source timestamp.
pub fn mark_source_failure(registry: &mut Registry, issuer: &str, failed_at: &str) -> bool {
    let failed_at = chrono::DateTime::parse_from_rfc3339(failed_at)
        .map(|value| value.with_timezone(&chrono::Utc))
        .unwrap_or_else(|_| chrono::Utc::now());
    let mut changed = false;
    for entry in registry.iter_mut().filter(|entry| entry.issuer == issuer && matchable(entry)) {
        let source_time = chrono::DateTime::parse_from_rfc3339(&entry.last_checked)
            .map(|value| value.with_timezone(&chrono::Utc));
        let too_old = match source_time {
            Ok(checked) => {
                checked > failed_at || failed_at.signed_duration_since(checked).num_hours() >= 48
            }
            Err(_) => true,
        };
        if too_old && entry.stale_since.is_none() {
            entry.stale_since = Some(entry.last_checked.clone());
            changed = true;
        }
    }
    changed
}

fn source_timestamp_is_fresh(last_checked: &str, checked_at: &str) -> bool {
    let Ok(last_checked) = chrono::DateTime::parse_from_rfc3339(last_checked) else {
        return false;
    };
    let Ok(checked_at) = chrono::DateTime::parse_from_rfc3339(checked_at) else {
        return false;
    };
    let last_checked = last_checked.with_timezone(&chrono::Utc);
    let checked_at = checked_at.with_timezone(&chrono::Utc);
    last_checked <= checked_at
        && checked_at.signed_duration_since(last_checked) < chrono::Duration::hours(48)
}

pub fn match_status(registry: &[Entry], chain: Chain, contract: &str) -> MatchStatus {
    let contract = normalize_contract(chain, contract);
    let mut removed = None;
    let mut stale = None;
    for entry in registry.iter().filter(|entry| {
        (entry.chain == chain && normalize_contract(entry.chain, &entry.contract) == contract)
            || official_deployment_matches(entry, chain, &contract)
    }) {
        if matchable(entry) {
            return MatchStatus::Active;
        }
        if let Some(removed_at) = &entry.removed_at {
            removed = Some((entry.issuer.clone(), removed_at.clone()));
        } else if let Some(since) = &entry.stale_since {
            stale = Some(since.clone());
        }
    }
    if let Some((issuer, removed_at)) = removed {
        MatchStatus::Removed { issuer, removed_at }
    } else if let Some(since) = stale {
        MatchStatus::Stale { since }
    } else {
        MatchStatus::NotFound
    }
}

pub fn matchable(entry: &Entry) -> bool {
    entry.removed_at.is_none() && entry.stale_since.is_none()
}

pub fn active_count(registry: &Registry) -> usize {
    registry.iter().filter(|entry| matchable(entry)).count()
}

pub fn active_issuers(registry: &Registry) -> usize {
    registry
        .iter()
        .filter(|entry| matchable(entry))
        .map(|entry| entry.issuer.as_str())
        .collect::<std::collections::HashSet<_>>()
        .len()
}

pub fn lookup<'a>(registry: &'a [Entry], chain: Chain, contract: &str) -> Option<&'a Entry> {
    let contract = normalize_contract(chain, contract);
    registry.iter().find(|entry| {
        ((entry.chain == chain && normalize_contract(entry.chain, &entry.contract) == contract)
            || official_deployment_matches(entry, chain, &contract))
            && matchable(entry)
    })
}

fn key(entry: &Entry) -> (Chain, String) {
    (entry.chain, normalize_contract(entry.chain, &entry.contract))
}

fn normalize_contract(chain: Chain, contract: &str) -> String {
    if chain == Chain::Solana { contract.to_owned() } else { contract.to_ascii_lowercase() }
}

#[cfg(test)]
mod tests {
    use super::{
        Chain, Entry, MatchStatus, Registry, lookup, mark_source_failure, match_status, matchable,
        reconcile_snapshot,
    };

    fn entry(contract: &str, ticker: &str) -> Entry {
        Entry {
            issuer: "Test".to_owned(),
            ticker: ticker.to_owned(),
            name: ticker.to_owned(),
            chain: Chain::Ethereum,
            contract: contract.to_owned(),
            decimals: Some(18),
            source: "manual".to_owned(),
            source_url: "https://example.invalid".to_owned(),
            last_checked: "2026-09-21T00:00:00Z".to_owned(),
            removed_at: None,
            stale_since: None,
            official_deployments: Vec::new(),
        }
    }
    #[test]
    fn official_deployment_wrappers_match_only_on_their_published_chain() {
        let mut published = entry("0x0000000000000000000000000000000000000001", "NVDA");
        published.official_deployments = vec![super::OfficialDeployment {
            network: "Ethereum".to_owned(),
            address: "0x0000000000000000000000000000000000000001".to_owned(),
            wrapper_address: Some("0x0000000000000000000000000000000000000002".to_owned()),
            wrapper_address_v2: Some("0x0000000000000000000000000000000000000003".to_owned()),
        }];
        let registry = vec![published];

        assert!(
            lookup(&registry, Chain::Ethereum, "0x0000000000000000000000000000000000000002")
                .is_some()
        );
        assert_eq!(
            match_status(&registry, Chain::Ethereum, "0x0000000000000000000000000000000000000003"),
            MatchStatus::Active
        );
        assert!(
            lookup(&registry, Chain::Base, "0x0000000000000000000000000000000000000002").is_none()
        );
    }
    fn fresh_entry(contract: &str, ticker: &str) -> Entry {
        let mut result = entry(contract, ticker);
        result.last_checked = "2026-09-22T00:00:00Z".to_owned();
        result
    }

    fn assert_rejected_without_mutation(
        original: &Registry,
        incoming: impl IntoIterator<Item = Entry>,
        checked_at: &str,
    ) {
        let mut registry = original.clone();
        assert!(reconcile_snapshot(&mut registry, "Test", incoming, checked_at).is_err());
        assert_eq!(registry, *original);
    }

    #[test]
    fn snapshot_validation_rejects_wrong_issuer_duplicates_and_bad_timestamps() {
        let original = vec![entry("0xabc", "A"), entry("0xdef", "B")];
        let checked_at = "2026-09-23T00:00:00Z";

        let mut wrong_issuer = fresh_entry("0xabc", "A");
        wrong_issuer.issuer = "Other".to_owned();
        assert_rejected_without_mutation(
            &original,
            [wrong_issuer, fresh_entry("0xdef", "B")],
            checked_at,
        );

        assert_rejected_without_mutation(
            &original,
            [fresh_entry("0xabc", "A"), fresh_entry("0xabc", "A")],
            checked_at,
        );

        let mut malformed = fresh_entry("0xabc", "A");
        malformed.last_checked = "not-a-timestamp".to_owned();
        assert_rejected_without_mutation(
            &original,
            [malformed, fresh_entry("0xdef", "B")],
            checked_at,
        );

        let mut future = fresh_entry("0xabc", "A");
        future.last_checked = "2026-09-24T00:00:00Z".to_owned();
        assert_rejected_without_mutation(
            &original,
            [future, fresh_entry("0xdef", "B")],
            checked_at,
        );

        let mut stale = fresh_entry("0xabc", "A");
        stale.last_checked = "2026-09-20T00:00:00Z".to_owned();
        assert_rejected_without_mutation(&original, [stale, fresh_entry("0xdef", "B")], checked_at);
    }

    #[test]
    fn contract_matching_is_case_sensitive_only_for_solana() {
        let evm = entry("0x0000000000000000000000000000000000000aBc", "EVM");
        let mut solana = entry("So1anaContract", "SOL");
        solana.chain = Chain::Solana;
        let registry = vec![evm, solana];

        assert_eq!(
            match_status(&registry, Chain::Ethereum, "0x0000000000000000000000000000000000000ABC"),
            MatchStatus::Active
        );
        assert_eq!(match_status(&registry, Chain::Solana, "so1anacontract"), MatchStatus::NotFound);
        assert_eq!(match_status(&registry, Chain::Solana, "So1anaContract"), MatchStatus::Active);
    }

    #[test]
    fn successful_snapshot_tombstones_removed_contract() {
        let mut registry = vec![entry("0xabc", "OLD"), entry("0xdef", "KEEP")];
        let mut replacement = entry("0xdef", "KEEP");
        replacement.last_checked = "2026-09-23T00:00:00Z".to_owned();
        reconcile_snapshot(&mut registry, "Test", [replacement], "2026-09-23T00:00:00Z")
            .expect("complete snapshot");
        assert!(matches!(
            match_status(&registry, Chain::Ethereum, "0xabc"),
            MatchStatus::Removed { .. }
        ));
        assert!(lookup(&registry, Chain::Ethereum, "0xabc").is_none());
        assert!(matchable(&registry[1]));
    }

    #[test]
    fn partial_snapshot_is_rejected_without_mutation() {
        let mut registry: Registry =
            vec![entry("0xabc", "A"), entry("0xdef", "B"), entry("0x123", "C")];
        let incoming = entry("0xabc", "A");
        assert!(
            reconcile_snapshot(&mut registry, "Test", [incoming], "2026-09-23T00:00:00Z").is_err()
        );
        assert_eq!(registry.iter().filter(|item| matchable(item)).count(), 3);
    }

    #[test]
    fn failed_old_source_is_marked_stale_and_stops_matching() {
        let mut registry = vec![entry("0xabc", "OLD")];
        mark_source_failure(&mut registry, "Test", "2026-09-23T00:00:00Z");
        assert!(matches!(
            match_status(&registry, Chain::Ethereum, "0xabc"),
            MatchStatus::Stale { .. }
        ));
        assert!(!matchable(&registry[0]));
    }
}
