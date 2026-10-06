use crate::domain::{attestation::Read, chain::Chain};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Reason {
    pub code: String,
    pub detail: String,
}

impl Reason {
    pub(crate) fn new(code: &str, detail: impl Into<String>) -> Self {
        Self { code: code.to_owned(), detail: detail.into() }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceVerified {
    ExactMatch,
    Match,
    None,
    Unavailable,
}
#[derive(Debug, Clone, Default)]
pub struct PowerFacts {
    pub can_seize: Vec<Reason>,
    pub can_block: Vec<Reason>,
    pub can_change_rules: Vec<Reason>,
    pub token_paused: Option<bool>,
    pub sanctions_list: Option<String>,
    pub source_target: Option<String>,
    pub source_is_proxy: bool,
    pub unavailable: Vec<Reason>,
    pub transient_failure: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PowersRecord {
    pub chain: Chain,
    pub contract: String,
    pub can_seize: Vec<Reason>,
    pub can_block: Vec<Reason>,
    pub can_change_rules: Vec<Reason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_paused: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sanctions_list: Option<String>,
    pub unavailable: Vec<Reason>,
    pub source_verified_subject: SourceVerifiedSubject,
    pub source_verified: SourceVerified,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_verified_proxy: Option<SourceVerified>,
    pub observed_at: String,
    pub block: Option<u64>,
    pub slot: Option<u64>,
    pub reads: Vec<Read>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceVerifiedSubject {
    TokenProgram,
    Contract,
    Implementation,
}

impl SourceVerifiedSubject {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::TokenProgram => "Token program build",
            Self::Contract => "Contract source",
            Self::Implementation => "Implementation source",
        }
    }
}

impl SourceVerified {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ExactMatch => "exact_match",
            Self::Match => "match",
            Self::None => "none",
            Self::Unavailable => "unavailable",
        }
    }
}

pub(crate) mod evm {
    use super::{PowerFacts, Reason};
    use serde::Deserialize;
    #[derive(Debug, Default, Deserialize)]
    pub(crate) struct ProbeSnapshot {
        pub implementation: Option<String>,
        pub admin: Option<String>,
        pub beacon: Option<String>,
        #[serde(default)]
        pub beacon_implementation: Option<String>,
        pub paused: Option<bool>,
        pub is_paused: Option<bool>,
        pub owner: Option<String>,
        pub pauser: Option<String>,
        #[serde(rename = "sanctionsList")]
        pub sanctions_list: Option<String>,
        #[serde(default)]
        pub unavailable: Vec<Reason>,
    }

    pub(crate) fn analyze(snapshot: ProbeSnapshot) -> PowerFacts {
        let mut facts = PowerFacts::default();
        facts.source_is_proxy = snapshot.implementation.is_some()
            || snapshot.admin.is_some()
            || snapshot.beacon.is_some();
        facts.source_target =
            snapshot.implementation.clone().or_else(|| snapshot.beacon_implementation.clone());
        let pause_state = effective_paused(&snapshot);
        facts.token_paused = pause_state.map(|(paused, _)| paused);
        facts.sanctions_list = snapshot
            .sanctions_list
            .clone()
            .filter(|address| address != "0x0000000000000000000000000000000000000000");
        let pausable =
            pause_state.map(|(paused, getter)| pausable_detail(&snapshot, paused, getter));
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
    fn effective_paused(snapshot: &ProbeSnapshot) -> Option<(bool, &'static str)> {
        if snapshot.paused == Some(true) {
            Some((true, "paused()"))
        } else if snapshot.is_paused == Some(true) {
            Some((true, "isPaused()"))
        } else if snapshot.paused.is_some() {
            Some((false, "paused()"))
        } else {
            snapshot.is_paused.map(|paused| (paused, "isPaused()"))
        }
    }

    fn pausable_detail(snapshot: &ProbeSnapshot, paused: bool, getter: &str) -> String {
        let state = if paused { "currently paused" } else { "currently not paused" };
        match (snapshot.pauser.as_deref(), snapshot.owner.as_deref()) {
            (Some(pauser), Some(owner)) => format!(
                "{getter} is implemented; {state}; pauser() returned {pauser}; owner() returned {owner}."
            ),
            (Some(pauser), None) => format!(
                "{getter} is implemented; {state}; pauser() returned {pauser}; owner not identified by this read."
            ),
            (None, Some(owner)) => format!(
                "{getter} is implemented; {state}; pauser not identified by this read; owner() returned {owner}."
            ),
            (None, None) => {
                format!("{getter} is implemented; {state} (pauser not identified by this read).")
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use serde_json::Value;
        #[test]
        fn maps_eip1967_slots_and_each_owner_probe_without_claiming_seizure() {
            let fixture: Value =
                serde_json::from_str(include_str!("../../tests/fixtures/powers/evm-powers.json"))
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
            let block_codes =
                facts.can_block.iter().map(|reason| reason.code.as_str()).collect::<Vec<_>>();
            assert_eq!(
                change_codes,
                ["eip1967_implementation", "eip1967_admin", "eip1967_beacon", "owner_getter"]
            );
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
            let fixture: Value =
                serde_json::from_str(include_str!("../../tests/fixtures/powers/evm-powers.json"))
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
            assert!(
                unpaused_facts.can_block[0].detail.contains("pauser not identified by this read")
            );

            let roles_facts = analyze(unpaused_with_roles);
            assert!(
                roles_facts.can_block[0]
                    .detail
                    .contains("pauser() returned 0x0000000000000000000000000000000000000015")
            );
            assert!(
                roles_facts.can_block[0]
                    .detail
                    .contains("owner() returned 0x0000000000000000000000000000000000000014")
            );
        }
    }
}
pub(crate) mod solana {
    use super::{PowerFacts, Reason};
    use serde_json::Value;
    const SYSTEM_PROGRAM_ID: &str = "11111111111111111111111111111111";
    pub(crate) const UPGRADEABLE_LOADER_ID: &str = "BPFLoaderUpgradeab1e11111111111111111111111";
    /// Squads v4 program ID, published by the official repository under Program Addresses:
    /// https://github.com/Squads-Protocol/v4#program-smart-contract-addresses
    const SQUADS_V4_PROGRAM_ID: &str = "SQDS4ep65T869zMMBKyuUq6aD6EgTu8psMjkvj52pCf";

    pub(crate) fn analyze_mint_info(info: &Value) -> PowerFacts {
        let mut facts = PowerFacts::default();
        if let Some(authority) = public_key(info.get("mintAuthority")) {
            facts.can_change_rules.push(Reason::new(
                "mint_authority",
                format!("Mint authority {authority} can mint additional units."),
            ));
        }
        if let Some(authority) = public_key(info.get("freezeAuthority")) {
            facts.can_block.push(Reason::new(
            "freeze_authority",
            format!("Freeze authority {authority} can freeze token accounts, blocking transfers and burns."),
        ));
        }

        for extension in info.get("extensions").and_then(Value::as_array).into_iter().flatten() {
            let Some(name) = extension.get("extension").and_then(Value::as_str) else { continue };
            let normalized = name
                .bytes()
                .filter(|byte| *byte != b'_' && *byte != b'-')
                .map(|byte| byte.to_ascii_lowercase() as char)
                .collect::<String>();
            let state = extension.get("state").unwrap_or(extension);
            match normalized.as_str() {
                "permanentdelegate" => {
                    if let Some(delegate) = public_key(state.get("delegate")) {
                        facts.can_seize.push(Reason::new(
                        "permanent_delegate",
                        format!("Permanent delegate {delegate} may transfer or burn tokens from any token account."),
                    ));
                    }
                }
                "defaultaccountstate" => {
                    if state_value_is(state.get("accountState"), "frozen") {
                        facts.can_block.push(Reason::new(
                        "default_account_state_frozen",
                        "DefaultAccountState is Frozen; newly initialized token accounts start frozen until thawed.",
                    ));
                    }
                }
                "pausable" | "pausableconfig" => {
                    if let Some(paused) = state.get("paused").and_then(Value::as_bool) {
                        facts.token_paused = Some(paused);
                    }
                    if let Some(authority) = public_key(state.get("authority")) {
                        facts.can_block.push(Reason::new(
                            "pausable_authority",
                            format!(
                                "Pausable authority {authority} can pause the mint's transfers."
                            ),
                        ));
                    }
                    if facts.token_paused == Some(true) {
                        facts.can_block.push(Reason::new(
                            "mint_paused",
                            "Pausable extension reports the mint is currently paused.",
                        ));
                    }
                }
                "transferhook" => {
                    match transfer_hook_program_id(state.get("programId")) {
                    Ok(Some(program_id)) => facts.can_block.push(Reason::new(
                        "transfer_hook",
                        format!("Transfer hook program {program_id} participates in transfers and may condition or reject them."),
                    )),
                    Ok(None) => {}
                    Err(()) => facts.unavailable.push(Reason::new(
                        "unknown_extension",
                        "Transfer-hook extension has a malformed program ID.",
                    )),
                }
                    if let Some(authority) = public_key(state.get("authority")) {
                        facts.can_change_rules.push(Reason::new(
                        "transfer_hook_authority",
                        format!("Transfer-hook authority {authority} may change the configured hook program."),
                    ));
                    }
                }
                "confidentialtransfermint" => {
                    if state.get("autoApproveNewAccounts").and_then(Value::as_bool) == Some(false)
                        && let Some(authority) = public_key(state.get("authority"))
                    {
                        facts.can_block.push(Reason::new(
                        "confidential_transfer_approval",
                        format!("Confidential-transfer authority {authority} can withhold approval for new accounts when auto-approval is disabled."),
                    ));
                    }
                }
                "unparseableextension" => facts.unavailable.push(Reason::new(
                    "unknown_extension",
                    "Solana reported an extension that its JSON-RPC parser could not parse.",
                )),
                _ => {}
            }
        }
        facts
    }

    pub(crate) fn program_data_address(program_data: &[u8]) -> Option<String> {
        let tag = u32::from_le_bytes(program_data.get(..4)?.try_into().ok()?);
        if tag != 2 {
            return None;
        }
        Some(bs58::encode(program_data.get(4..36)?).into_string())
    }

    pub(crate) fn upgrade_authority(program_data: &[u8]) -> Option<Option<String>> {
        let tag = u32::from_le_bytes(program_data.get(..4)?.try_into().ok()?);
        if tag != 3 || program_data.len() < 13 {
            return None;
        }
        match program_data[12] {
            0 => Some(None),
            1 => Some(Some(bs58::encode(program_data.get(13..45)?).into_string())),
            _ => None,
        }
    }

    pub(crate) fn classify_upgrade_authority(
        authority: Option<&str>,
        authority_account_owner: Option<&str>,
    ) -> &'static str {
        match authority {
            None => "immutable",
            Some(authority) if !authority_is_on_curve(authority) => "program_derived",
            Some(_) if authority_account_owner == Some(SQUADS_V4_PROGRAM_ID) => "squads",
            Some(_) if authority_account_owner == Some(SYSTEM_PROGRAM_ID) => "key",
            Some(_) => "unknown",
        }
    }

    fn authority_is_on_curve(authority: &str) -> bool {
        crate::domain::chain::Chain::decode_solana_address(authority)
            .is_some_and(|bytes| ed25519_dalek::VerifyingKey::from_bytes(&bytes).is_ok())
    }

    pub(crate) fn add_program_authority_fact(
        facts: &mut PowerFacts,
        authority: Option<&str>,
        account_owner: Option<&str>,
    ) -> &'static str {
        let classification = classify_upgrade_authority(authority, account_owner);
        let detail = match (classification, authority) {
            ("immutable", _) => {
                "Token program ProgramData has no upgrade authority; the program is immutable."
                    .to_owned()
            }
            ("key", Some(authority)) => {
                format!("Token program upgrade authority is a key address: {authority}.")
            }
            ("program_derived", Some(authority)) => format!(
                "Token program upgrade authority is a program-derived address {authority}; controller not established by this read."
            ),
            ("squads", Some(authority)) => format!(
                "Token program upgrade authority {authority} is owned by the Squads v4 program; multisig members and threshold were not inspected."
            ),
            (_, Some(authority)) => format!(
                "Token program upgrade authority classification is unknown for {authority}."
            ),
            (_, None) => "Token program upgrade authority classification is unknown.".to_owned(),
        };
        facts.can_change_rules.push(Reason::new("token_program_upgrade_authority", detail));
        classification
    }

    fn public_key(value: Option<&Value>) -> Option<&str> {
        value
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && *value != "11111111111111111111111111111111")
    }

    fn transfer_hook_program_id(value: Option<&Value>) -> Result<Option<&str>, ()> {
        match value {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(value)) if value == SYSTEM_PROGRAM_ID => Ok(None),
            Some(Value::String(value)) => {
                if crate::domain::chain::Chain::decode_solana_address(value).is_some() {
                    Ok(Some(value))
                } else {
                    Err(())
                }
            }
            Some(_) => Err(()),
        }
    }

    fn state_value_is(value: Option<&Value>, expected: &str) -> bool {
        match value {
            Some(Value::String(value)) => value.eq_ignore_ascii_case(expected),
            Some(Value::Object(value)) => {
                value.keys().any(|key| key.eq_ignore_ascii_case(expected))
            }
            _ => false,
        }
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn parses_every_supported_mint_authority_and_extension_fixture() {
            let fixtures: Value = serde_json::from_str(include_str!(
                "../../tests/fixtures/powers/solana-powers.json"
            ))
            .expect("Solana fixture");

            let authorities = analyze_mint_info(&fixtures["authorities"]);
            assert_eq!(authorities.can_change_rules[0].code, "mint_authority");
            assert_eq!(authorities.can_block[0].code, "freeze_authority");

            let permanent_delegate = analyze_mint_info(&fixtures["permanent_delegate"]);
            assert_eq!(permanent_delegate.can_seize[0].code, "permanent_delegate");

            let default_state = analyze_mint_info(&fixtures["default_account_state_frozen"]);
            assert_eq!(default_state.can_block[0].code, "default_account_state_frozen");
            let initialized = analyze_mint_info(&fixtures["default_account_state_initialized"]);
            assert!(initialized.can_block.is_empty());
            let pausable = analyze_mint_info(&fixtures["pausable"]);
            assert_eq!(
                pausable.can_block.iter().map(|reason| reason.code.as_str()).collect::<Vec<_>>(),
                ["pausable_authority", "mint_paused"]
            );

            let transfer_hook = analyze_mint_info(&fixtures["transfer_hook"]);
            assert_eq!(transfer_hook.can_block[0].code, "transfer_hook");
            assert_eq!(transfer_hook.can_change_rules[0].code, "transfer_hook_authority");

            let transfer_hook_unset = analyze_mint_info(&fixtures["transfer_hook_unset"]);
            assert_eq!(transfer_hook_unset.can_change_rules[0].code, "transfer_hook_authority");
            assert!(transfer_hook_unset.unavailable.is_empty());
            let transfer_hook_malformed = analyze_mint_info(&fixtures["transfer_hook_malformed"]);
            assert!(transfer_hook_malformed.can_block.is_empty());
            assert_eq!(transfer_hook_malformed.unavailable[0].code, "unknown_extension");
            let unparseable = analyze_mint_info(&fixtures["unparseable_extension"]);
            assert_eq!(unparseable.unavailable[0].code, "unknown_extension");

            let confidential =
                analyze_mint_info(&fixtures["confidential_transfer_manual_approval"]);
            assert_eq!(confidential.can_block[0].code, "confidential_transfer_approval");
            let auto_approved = analyze_mint_info(&fixtures["confidential_transfer_auto_approval"]);
            assert!(auto_approved.can_block.is_empty());

            let close_authority = analyze_mint_info(&fixtures["mint_close_authority"]);
            assert!(close_authority.can_seize.is_empty());
            assert!(close_authority.can_block.is_empty());
            assert!(close_authority.can_change_rules.is_empty());
        }

        #[test]
        fn decodes_upgradeable_loader_program_and_programdata_layouts() {
            let program_data_key = [9u8; 32];
            let mut program = 2u32.to_le_bytes().to_vec();
            program.extend_from_slice(&program_data_key);
            assert_eq!(
                program_data_address(&program),
                Some(bs58::encode(program_data_key).into_string())
            );

            let mut immutable = 3u32.to_le_bytes().to_vec();
            immutable.extend_from_slice(&5u64.to_le_bytes());
            immutable.push(0);
            assert_eq!(upgrade_authority(&immutable), Some(None));

            let authority = [7u8; 32];
            let mut upgradeable = 3u32.to_le_bytes().to_vec();
            upgradeable.extend_from_slice(&5u64.to_le_bytes());
            upgradeable.push(1);
            upgradeable.extend_from_slice(&authority);
            assert_eq!(
                upgrade_authority(&upgradeable),
                Some(Some(bs58::encode(authority).into_string()))
            );
            assert_eq!(program_data_address(&immutable), None);
        }

        #[test]
        fn distinguishes_program_derived_and_on_curve_upgrade_authorities() {
            let pda = "AeLmXCbPaQHGWRLr2saFsEVfmMNuKnxRAbWCT9P5twgz";
            let key = "7pt9tkctJPK7PPNQJ77GKg8ZffSF6QxoMiCFYHxrtaCj";
            let cases = [
                (None, None, "immutable"),
                (Some(pda), Some(SYSTEM_PROGRAM_ID), "program_derived"),
                (Some(pda), None, "program_derived"),
                (Some(key), Some(SYSTEM_PROGRAM_ID), "key"),
                (Some(key), Some(SQUADS_V4_PROGRAM_ID), "squads"),
                (Some(key), Some("OtherProgram111111111111111111111111111"), "unknown"),
                (Some(key), None, "unknown"),
            ];
            for (authority, owner, expected) in cases {
                assert_eq!(classify_upgrade_authority(authority, owner), expected);
            }

            let mut facts = PowerFacts::default();
            add_program_authority_fact(&mut facts, Some(pda), Some(SYSTEM_PROGRAM_ID));
            let detail = &facts.can_change_rules[0].detail;
            assert!(detail.contains("program-derived address"));
            assert!(detail.contains("controller not established"));
            assert!(!detail.contains("key address"));
        }
    }
}
