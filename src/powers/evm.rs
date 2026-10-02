use super::{PowerFacts, Reason, SourceVerified};
use crate::{chain::Chain, state::AppState};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Default, Deserialize)]
pub(crate) struct ProbeSnapshot {
    pub implementation: Option<String>,
    pub admin: Option<String>,
    pub beacon: Option<String>,
    #[serde(default)]
    pub beacon_implementation: Option<String>,
    pub paused: Option<bool>,
    pub owner: Option<String>,
    pub pauser: Option<String>,
    #[serde(rename = "sanctionsList")]
    pub sanctions_list: Option<String>,
    #[serde(default)]
    pub unavailable: Vec<Reason>,
}

pub(crate) fn analyze(snapshot: ProbeSnapshot) -> PowerFacts {
    let mut facts = PowerFacts::default();
    facts.source_is_proxy =
        snapshot.implementation.is_some() || snapshot.admin.is_some() || snapshot.beacon.is_some();
    facts.source_target =
        snapshot.implementation.clone().or_else(|| snapshot.beacon_implementation.clone());
    let pausable = snapshot.paused.map(|paused| pausable_detail(&snapshot, paused));
    facts.unavailable = snapshot.unavailable;
    if let Some(implementation) = snapshot.implementation {
        facts.can_change_rules.push(Reason::new(
            "eip1967_implementation",
            format!(
                "EIP-1967 implementation slot points to {implementation}; upgrade permissions were not established by this read."
            ),
        ));
    }
    if let Some(admin) = snapshot.admin {
        facts.can_change_rules.push(Reason::new(
            "eip1967_admin",
            format!("EIP-1967 admin slot contains {admin}."),
        ));
    }
    if let Some(beacon) = snapshot.beacon {
        facts.can_change_rules.push(Reason::new(
            "eip1967_beacon",
            format!("EIP-1967 beacon slot points to {beacon}; beacon control was not established by this read."),
        ));
    }
    if let Some(detail) = pausable {
        facts.can_block.push(Reason::new("pausable", detail));
    }
    if let Some(pauser) = snapshot.pauser {
        facts.can_block.push(Reason::new(
            "pauser",
            format!("pauser() returned {pauser}; the getter alone does not establish its permissions."),
        ));
    }
    if let Some(sanctions_list) = snapshot.sanctions_list {
        facts.can_block.push(Reason::new(
            "sanctions_list",
            format!("sanctionsList() returned {sanctions_list}; transfer effects were not inferred from the getter alone."),
        ));
    }
    if let Some(owner) = snapshot.owner {
        facts.can_change_rules.push(Reason::new(
            "owner_getter",
            format!("owner() returned {owner}; the getter alone does not establish which actions the role can take."),
        ));
    }
    facts
}
fn pausable_detail(snapshot: &ProbeSnapshot, paused: bool) -> String {
    let state = if paused { "currently paused" } else { "currently not paused" };
    match (snapshot.pauser.as_deref(), snapshot.owner.as_deref()) {
        (Some(pauser), Some(owner)) => format!(
            "paused() is implemented; {state}; pauser() returned {pauser}; owner() returned {owner}."
        ),
        (Some(pauser), None) => format!(
            "paused() is implemented; {state}; pauser() returned {pauser}; owner not identified by this read."
        ),
        (None, Some(owner)) => format!(
            "paused() is implemented; {state}; pauser not identified by this read; owner() returned {owner}."
        ),
        (None, None) => format!(
            "paused() is implemented; {state} (pauser not identified by this read)."
        ),
    }
}


pub(crate) async fn verify_source(
    state: &AppState,
    chain: Chain,
    contract: &str,
) -> SourceVerified {
    let Some(url) = sourcify_url(chain, contract) else {
        return SourceVerified::Unavailable;
    };
    let response = super::source_json(state, &url, "sourcify.dev").await;
    source_status(response)
}

pub(crate) fn sourcify_url(chain: Chain, contract: &str) -> Option<String> {
    Some(format!(
        "https://sourcify.dev/server/v2/contract/{}/{contract}",
        sourcify_chain_id(chain)?
    ))
}


pub(crate) fn sourcify_chain_id(chain: Chain) -> Option<u64> {
    match chain {
        Chain::Ethereum => Some(1),
        Chain::Bnb => Some(56),
        Chain::Base => Some(8453),
        Chain::RobinhoodChain => Some(4663),
        Chain::Solana => None,
    }
}

fn source_status(response: Option<(StatusCode, Value)>) -> SourceVerified {
    let Some((status, body)) = response else {
        return SourceVerified::Unavailable;
    };
    if status == StatusCode::NOT_FOUND {
        return SourceVerified::None;
    }
    if !status.is_success() {
        return SourceVerified::Unavailable;
    }
    match body.get("match").and_then(Value::as_str) {
        Some("exact_match") => SourceVerified::ExactMatch,
        Some("match") => SourceVerified::Match,
        Some("none") => SourceVerified::None,
        _ => SourceVerified::Unavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_each_sourcify_result_status() {
        let fixture: Value = serde_json::from_str(include_str!("fixtures/evm-powers.json"))
            .expect("EVM fixture");
        let sourcify = &fixture["sourcify"];
        let cases = [
            ("exact_match", SourceVerified::ExactMatch),
            ("match", SourceVerified::Match),
            ("not_found", SourceVerified::None),
            ("none", SourceVerified::None),
            ("bad_request", SourceVerified::Unavailable),
            ("server_error", SourceVerified::Unavailable),
        ];
        for (name, expected) in cases {
            let response = &sourcify[name];
            let status = StatusCode::from_u16(
                response["status"].as_u64().expect("fixture status") as u16,
            )
            .expect("fixture status code");
            assert_eq!(
                source_status(Some((status, response["body"].clone()))),
                expected,
                "{name}"
            );
        }
        assert_eq!(source_status(None), SourceVerified::Unavailable);
    }


    #[test]
    fn maps_eip1967_slots_and_each_owner_probe_without_claiming_seizure() {
        let fixture: Value = serde_json::from_str(include_str!("fixtures/evm-powers.json"))
            .expect("EVM fixture");
        let snapshot: ProbeSnapshot =
            serde_json::from_value(fixture["upgradeable_and_restricted"].clone())
                .expect("probe fixture");
        let facts = analyze(snapshot);
        let change_codes = facts
            .can_change_rules
            .iter()
            .map(|reason| reason.code.as_str())
            .collect::<Vec<_>>();
        let block_codes = facts
            .can_block
            .iter()
            .map(|reason| reason.code.as_str())
            .collect::<Vec<_>>();
        assert_eq!(change_codes, ["eip1967_implementation", "eip1967_admin", "eip1967_beacon", "owner_getter"]);
        assert_eq!(block_codes, ["pausable", "pauser", "sanctions_list"]);
        assert!(facts.can_seize.is_empty());
    }

    #[test]
    fn proxy_source_target_is_resolved_from_implementation_or_beacon() {
        let implementation = "0x0000000000000000000000000000000000000011";
        let beacon_implementation = "0x0000000000000000000000000000000000000022";
        let direct_proxy = analyze(ProbeSnapshot {
            implementation: Some(implementation.to_owned()),
            ..ProbeSnapshot::default()
        });
        assert!(direct_proxy.source_is_proxy);
        assert_eq!(direct_proxy.source_target.as_deref(), Some(implementation));

        let beacon_proxy = analyze(ProbeSnapshot {
            beacon: Some("0x0000000000000000000000000000000000000033".to_owned()),
            beacon_implementation: Some(beacon_implementation.to_owned()),
            ..ProbeSnapshot::default()
        });
        assert!(beacon_proxy.source_is_proxy);
        assert_eq!(beacon_proxy.source_target.as_deref(), Some(beacon_implementation));
    }

    #[test]
    fn paused_getter_reports_control_signal_and_current_state() {
        let fixture: Value = serde_json::from_str(include_str!("fixtures/evm-powers.json"))
            .expect("EVM fixture");
        let paused: ProbeSnapshot =
            serde_json::from_value(fixture["paused"].clone()).expect("paused fixture");
        let unpaused: ProbeSnapshot =
            serde_json::from_value(fixture["unpaused"].clone()).expect("unpaused fixture");
        let unpaused_with_roles: ProbeSnapshot =
            serde_json::from_value(fixture["unpaused_with_roles"].clone())
                .expect("unpaused role fixture");

        let paused_facts = analyze(paused);
        assert_eq!(paused_facts.can_block[0].code, "pausable");
        assert!(paused_facts.can_block[0].detail.contains("currently paused"));

        let unpaused_facts = analyze(unpaused);
        assert_eq!(unpaused_facts.can_block[0].code, "pausable");
        assert!(unpaused_facts.can_block[0].detail.contains("currently not paused"));
        assert!(unpaused_facts.can_block[0].detail.contains("pauser not identified by this read"));

        let roles_facts = analyze(unpaused_with_roles);
        assert!(roles_facts.can_block[0].detail.contains("pauser() returned 0x0000000000000000000000000000000000000015"));
        assert!(roles_facts.can_block[0].detail.contains("owner() returned 0x0000000000000000000000000000000000000014"));
    }

    #[test]
    fn maps_supported_sourcify_chain_ids() {
        assert_eq!(sourcify_chain_id(Chain::Ethereum), Some(1));
        assert_eq!(sourcify_chain_id(Chain::Bnb), Some(56));
        assert_eq!(sourcify_chain_id(Chain::Base), Some(8453));
        assert_eq!(sourcify_chain_id(Chain::RobinhoodChain), Some(4663));
        assert_eq!(sourcify_chain_id(Chain::Solana), None);
    }
    #[test]
    fn sourcify_request_url_omits_invalid_field_selector() {
        let url = sourcify_url(
            Chain::RobinhoodChain,
            "0xd0601ce157db5bdc3162bbac2a2c8af5320d9eec",
        )
        .expect("supported chain");
        assert_eq!(
            url,
            "https://sourcify.dev/server/v2/contract/4663/0xd0601ce157db5bdc3162bbac2a2c8af5320d9eec"
        );
        assert!(!url.contains("fields="));
    }
}
