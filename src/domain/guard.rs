use crate::domain::{
    attestation::{Read, canonical_json},
    chain::Chain,
    check::{claims_name, claims_symbol},
    pool::TokenMeta,
    powers::PowersRecord,
    registry::{self, Entry},
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IdentityStatus {
    Match,
    Mismatch,
    NoPublisher,
    RegistryStale,
    RegistryRemoved,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GuardIdentityCandidate {
    pub publisher: String,
    pub ticker: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GuardIdentity {
    pub publisher: Option<String>,
    pub matched_contract: Option<String>,
    pub ticker: Option<String>,
    pub status: IdentityStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate: Option<GuardIdentityCandidate>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GuardSubjectType {
    Token,
    Pool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GuardVerdict {
    Allow,
    Deny,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GuardReason {
    pub code: String,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceStatus {
    Verified,
    Unverified,
    Unavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GuardSource {
    pub status: SourceStatus,
    pub provider: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GuardQuote {
    pub address: String,
    pub symbol: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GuardPool {
    pub address: String,
    pub venue: String,
    pub quote: GuardQuote,
    pub verdict: String,
    pub observed_at: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WalletCheckStatus {
    Checked,
    NotApplicable,
    Unavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GuardWalletCheck {
    pub status: WalletCheckStatus,
    pub restrictions: Vec<GuardReason>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GuardDocument {
    pub id: String,
    pub kind: String,
    pub chain: Chain,
    pub address: String,
    pub subject_type: GuardSubjectType,
    pub subject_address: Option<String>,
    pub wallet: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wallet_check: Option<GuardWalletCheck>,
    pub identity: GuardIdentity,
    pub powers: Option<PowersRecord>,
    pub source: GuardSource,
    pub pools: Vec<GuardPool>,
    pub verdict: GuardVerdict,
    pub reasons: Vec<GuardReason>,
    pub observed_at: String,
    pub reads: Vec<Read>,
    pub reads_truncated: bool,
    pub public_key: String,
    pub signature: String,
    pub dev: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct GuardPayload {
    kind: String,
    chain: Chain,
    address: String,
    subject_type: GuardSubjectType,
    subject_address: Option<String>,
    wallet: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    wallet_check: Option<GuardWalletCheck>,
    identity: GuardIdentity,
    powers: Option<PowersRecord>,
    source: GuardSource,
    pools: Vec<GuardPool>,
    verdict: GuardVerdict,
    reasons: Vec<GuardReason>,
    observed_at: String,
    reads: Vec<Read>,
    #[serde(default, skip_serializing_if = "is_false")]
    reads_truncated: bool,
    public_key: String,
    dev: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl GuardDocument {
    fn payload(&self) -> GuardPayload {
        GuardPayload {
            kind: self.kind.clone(),
            chain: self.chain,
            address: self.address.clone(),
            subject_type: self.subject_type,
            subject_address: self.subject_address.clone(),
            wallet: self.wallet.clone(),
            wallet_check: self.wallet_check.clone(),
            identity: self.identity.clone(),
            powers: self.powers.clone(),
            source: self.source.clone(),
            pools: self.pools.clone(),
            verdict: self.verdict,
            reasons: self.reasons.clone(),
            observed_at: self.observed_at.clone(),
            reads: self.reads.clone(),
            reads_truncated: self.reads_truncated,
            public_key: self.public_key.clone(),
            dev: self.dev,
        }
    }
}

#[derive(Debug, Error)]
pub enum GuardVerifyError {
    #[error("guard payload is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("document kind is not `guard`")]
    Kind,
    #[error("guard id is not a 32-byte hexadecimal digest")]
    Id,
    #[error("guard id does not match the payload")]
    IdMismatch,
    #[error("guard public key is not valid base58")]
    PublicKeyEncoding,
    #[error("guard public key is not a 32-byte Ed25519 key")]
    PublicKeyLength,
    #[error("guard public key is invalid")]
    PublicKeyInvalid,
    #[error("guard signature is not valid base64")]
    SignatureEncoding,
    #[error("guard signature has an invalid length")]
    SignatureLength,
    #[error("guard signature does not verify")]
    SignatureInvalid,
}

pub fn canonical_payload_json(document: &GuardDocument) -> Result<Vec<u8>, serde_json::Error> {
    canonical_json(&document.payload())
}

pub fn verify(document: &GuardDocument) -> Result<(), GuardVerifyError> {
    if document.kind != "guard" {
        return Err(GuardVerifyError::Kind);
    }
    if document.public_key.len() > Chain::MAX_SOLANA_ADDRESS_CHARS {
        return Err(GuardVerifyError::PublicKeyLength);
    }
    if document.signature.len() != 88 {
        return Err(GuardVerifyError::SignatureLength);
    }
    let payload = canonical_payload_json(document)?;
    let expected = crate::domain::attestation::hex_lower(&Sha256::digest(&payload));
    if document.id.len() != 64 || !document.id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(GuardVerifyError::Id);
    }
    if !document.id.eq_ignore_ascii_case(&expected) {
        return Err(GuardVerifyError::IdMismatch);
    }
    let mut public_key_bytes = [0; 32];
    let public_key_len = bs58::decode(&document.public_key)
        .onto(&mut public_key_bytes)
        .map_err(|_| GuardVerifyError::PublicKeyEncoding)?;
    if public_key_len != public_key_bytes.len() {
        return Err(GuardVerifyError::PublicKeyLength);
    }
    let public_key = VerifyingKey::from_bytes(&public_key_bytes)
        .map_err(|_| GuardVerifyError::PublicKeyInvalid)?;
    let mut signature_bytes = [0; 64];
    let signature_len = BASE64
        .decode_slice(&document.signature, &mut signature_bytes)
        .map_err(|_| GuardVerifyError::SignatureEncoding)?;
    if signature_len != signature_bytes.len() {
        return Err(GuardVerifyError::SignatureLength);
    }
    let signature = Signature::from_bytes(&signature_bytes);
    public_key.verify(&payload, &signature).map_err(|_| GuardVerifyError::SignatureInvalid)
}

pub(crate) fn identify(
    chain: Chain,
    address: &str,
    metadata: Option<&TokenMeta>,
    entries: &[Entry],
) -> GuardIdentity {
    identify_with_contract_metadata(
        chain,
        address,
        metadata,
        entries,
        &std::collections::HashMap::new(),
    )
}

pub(crate) fn identify_with_contract_metadata(
    chain: Chain,
    address: &str,
    metadata: Option<&TokenMeta>,
    entries: &[Entry],
    publisher_metadata: &std::collections::HashMap<String, TokenMeta>,
) -> GuardIdentity {
    if let Some(entry) = registry::lookup(entries, chain, address) {
        return identity_for(entry, IdentityStatus::Match);
    }
    match registry::match_status(entries, chain, address) {
        registry::MatchStatus::Stale { .. } => {
            if let Some(entry) = registry_entry_for(entries, chain, address, |entry| {
                entry.stale_since.is_some() && entry.removed_at.is_none()
            }) {
                return identity_for(entry, IdentityStatus::RegistryStale);
            }
        }
        registry::MatchStatus::Removed { .. } => {
            if let Some(entry) =
                registry_entry_for(entries, chain, address, |entry| entry.removed_at.is_some())
            {
                return identity_for(entry, IdentityStatus::RegistryRemoved);
            }
        }
        registry::MatchStatus::Active | registry::MatchStatus::NotFound => {}
    }

    let mut candidate = None;
    if let Some(metadata) = metadata {
        for exact_ticker in [true, false] {
            for entry in entries {
                if !registry::matchable(entry)
                    || entry.chain != chain
                    || !metadata_resembles_entry(metadata, entry)
                    || exact_ticker_candidate(metadata, entry) != exact_ticker
                {
                    continue;
                }
                let published =
                    publisher_metadata.get(&issuer_metadata_key(chain, &entry.contract));
                if is_contradicted_claim(chain, metadata, entry, published) {
                    return identity_for(entry, IdentityStatus::Mismatch);
                }
                candidate.get_or_insert(entry);
            }
        }
    }
    if let Some(entry) = candidate {
        return GuardIdentity {
            publisher: None,
            matched_contract: None,
            ticker: None,
            status: IdentityStatus::NoPublisher,
            candidate: Some(GuardIdentityCandidate {
                publisher: entry.issuer.clone(),
                ticker: entry.ticker.clone(),
                name: entry.name.clone(),
            }),
        };
    }
    no_publisher()
}

pub(crate) fn issuer_metadata_key(chain: Chain, address: &str) -> String {
    if chain == Chain::Solana { address.to_owned() } else { address.to_ascii_lowercase() }
}

pub(crate) fn metadata_resembles_entry(metadata: &TokenMeta, entry: &Entry) -> bool {
    metadata
        .symbol
        .as_deref()
        .is_some_and(|symbol| claims_symbol(symbol, &entry.ticker, &entry.name))
        || metadata
            .name
            .as_deref()
            .is_some_and(|name| claims_name(name, &entry.ticker, &entry.name))
}

pub(crate) fn metadata_complete(metadata: &TokenMeta) -> bool {
    metadata.symbol.as_deref().is_some_and(|value| !value.trim().is_empty())
        && metadata.name.as_deref().is_some_and(|value| !value.trim().is_empty())
}

pub(crate) fn exact_ticker_candidate(metadata: &TokenMeta, entry: &Entry) -> bool {
    metadata
        .symbol
        .as_deref()
        .is_some_and(|symbol| normalized_identity(symbol) == normalized_identity(&entry.ticker))
}

fn registry_entry_for<'a>(
    entries: &'a [Entry],
    chain: Chain,
    address: &str,
    predicate: impl Fn(&Entry) -> bool,
) -> Option<&'a Entry> {
    entries.iter().find(|entry| {
        entry.chain == chain && same_contract(chain, &entry.contract, address) && predicate(entry)
    })
}

fn same_contract(chain: Chain, left: &str, right: &str) -> bool {
    if chain == Chain::Solana { left == right } else { left.eq_ignore_ascii_case(right) }
}

fn normalized_identity(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .map(|character| character.to_ascii_lowercase())
        .collect()
}

fn is_contradicted_claim(
    chain: Chain,
    metadata: &TokenMeta,
    entry: &Entry,
    published_metadata: Option<&TokenMeta>,
) -> bool {
    if same_contract(chain, &metadata.address, &entry.contract) || !metadata_complete(metadata) {
        return false;
    }
    let Some(published) = published_metadata.filter(|value| metadata_complete(value)) else {
        return false;
    };
    normalized_identity(metadata.symbol.as_deref().unwrap())
        == normalized_identity(published.symbol.as_deref().unwrap())
        && normalized_identity(metadata.name.as_deref().unwrap())
            == normalized_identity(published.name.as_deref().unwrap())
}

fn no_publisher() -> GuardIdentity {
    GuardIdentity {
        publisher: None,
        matched_contract: None,
        ticker: None,
        status: IdentityStatus::NoPublisher,
        candidate: None,
    }
}

fn identity_for(entry: &Entry, status: IdentityStatus) -> GuardIdentity {
    GuardIdentity {
        publisher: Some(entry.issuer.clone()),
        matched_contract: Some(entry.contract.clone()),
        ticker: Some(entry.ticker.clone()),
        status,
        candidate: None,
    }
}

pub(crate) fn evaluate(
    identity: &GuardIdentity,
    powers: Option<&PowersRecord>,
    wallet_check: Option<&GuardWalletCheck>,
    source: SourceStatus,
) -> (GuardVerdict, Vec<GuardReason>) {
    let mut reasons = Vec::new();
    let mut deny = false;
    let mut unknown = false;
    match identity.status {
        IdentityStatus::Match => reasons.push(GuardReason {
            code: "publisher_contract_match".to_owned(),
            detail: "The token contract matches the active publisher registry entry on this chain."
                .to_owned(),
        }),
        IdentityStatus::Mismatch => {
            deny = true;
            reasons.push(GuardReason {
                code: "publisher_contract_mismatch".to_owned(),
                detail: format!(
                    "The token symbol and name match {}, but this contract is not the publisher's active contract on this chain.",
                    identity.ticker.as_deref().unwrap_or("a registry ticker")
                ),
            });
        }
        IdentityStatus::NoPublisher if identity.candidate.is_some() => {
            unknown = true;
            let candidate = identity.candidate.as_ref().expect("candidate status checked");
            reasons.push(GuardReason {
                code: "name_resembles_registry_entry".to_owned(),
                detail: format!(
                    "Token metadata resembles {} ({}), but does not establish a publisher-contract contradiction.",
                    candidate.name, candidate.ticker
                ),
            });
        }
        IdentityStatus::NoPublisher => {
            unknown = true;
            reasons.push(GuardReason {
                code: "no_publisher".to_owned(),
                detail:
                    "No active issuer publishing this token contract is known in QED's registry."
                        .to_owned(),
            });
        }
        IdentityStatus::RegistryStale => {
            unknown = true;
            reasons.push(GuardReason {
                code: "registry_stale".to_owned(),
                detail: "The registry entry for this contract is stale; QED cannot confirm its current publisher status."
                    .to_owned(),
            });
        }
        IdentityStatus::RegistryRemoved => {
            unknown = true;
            reasons.push(GuardReason {
                code: "registry_removed".to_owned(),
                detail: "The publisher removed this contract from its registry.".to_owned(),
            });
        }
    }

    if let Some(powers) = powers {
        if powers.token_paused == Some(true) {
            unknown |= wallet_check.is_none();
            deny |= wallet_check.is_some();
            reasons.push(GuardReason {
                code: "token_paused".to_owned(),
                detail: "The token-wide pause is active; transfers are blocked until it is lifted."
                    .to_owned(),
            });
        }
        if !powers.unavailable.is_empty() {
            unknown = true;
            reasons.push(GuardReason {
                code: "powers_incomplete".to_owned(),
                detail: "One or more token-power reads were unavailable.".to_owned(),
            });
        }
    } else {
        unknown = true;
        reasons.push(GuardReason {
            code: "powers_unavailable".to_owned(),
            detail: "Token powers could not be read for this contract.".to_owned(),
        });
    }

    if let Some(wallet_check) = wallet_check {
        match wallet_check.status {
            WalletCheckStatus::Checked => {}
            WalletCheckStatus::NotApplicable => {
                unknown = true;
                reasons.push(GuardReason {
                    code: "wallet_check_not_applicable".to_owned(),
                    detail: "No wallet-specific restriction getter applies to this token."
                        .to_owned(),
                });
            }
            WalletCheckStatus::Unavailable => {
                unknown = true;
                reasons.push(GuardReason {
                    code: "wallet_check_unavailable".to_owned(),
                    detail: "QED could not determine whether the supplied wallet has an active transfer restriction."
                        .to_owned(),
                });
            }
        }
        for restriction in &wallet_check.restrictions {
            deny = true;
            reasons.push(restriction.clone());
        }
    }

    match source {
        SourceStatus::Verified => {}
        SourceStatus::Unverified => {
            unknown = true;
            reasons.push(GuardReason {
                code: "source_unverified".to_owned(),
                detail: "Published source did not match the observed contract or program."
                    .to_owned(),
            });
        }
        SourceStatus::Unavailable => {
            unknown = true;
            reasons.push(GuardReason {
                code: "source_unavailable".to_owned(),
                detail: "QED could not verify the published source for this contract or program."
                    .to_owned(),
            });
        }
    }

    let verdict = if deny {
        GuardVerdict::Deny
    } else if unknown {
        GuardVerdict::Unknown
    } else {
        GuardVerdict::Allow
    };
    (verdict, reasons)
}
#[cfg(test)]
pub(crate) fn signed_test_guard(seed: [u8; 32]) -> GuardDocument {
    signed_test_guard_with_dev(seed, false)
}

#[cfg(test)]
pub(crate) fn signed_test_guard_with_dev(seed: [u8; 32], dev: bool) -> GuardDocument {
    use ed25519_dalek::Signer as _;
    let address = "0x0000000000000000000000000000000000000001".to_owned();
    let signing_key = ed25519_dalek::SigningKey::from_bytes(&seed);
    let mut document = GuardDocument {
        id: String::new(),
        kind: "guard".to_owned(),
        chain: Chain::Base,
        address: address.clone(),
        subject_type: GuardSubjectType::Token,
        subject_address: Some(address.clone()),
        wallet: None,
        wallet_check: None,
        identity: GuardIdentity {
            publisher: Some("Example Publisher".to_owned()),
            matched_contract: Some(address),
            ticker: Some("NVDA".to_owned()),
            status: IdentityStatus::Match,
            candidate: None,
        },
        powers: None,
        source: GuardSource { status: SourceStatus::Unavailable, provider: "Sourcify".to_owned() },
        pools: Vec::new(),
        verdict: GuardVerdict::Unknown,
        reasons: Vec::new(),
        observed_at: "2026-10-05T12:00:00Z".to_owned(),
        reads: Vec::new(),
        reads_truncated: false,
        public_key: bs58::encode(signing_key.verifying_key().as_bytes()).into_string(),
        signature: String::new(),
        dev,
    };
    let payload = canonical_payload_json(&document).expect("guard payload");
    document.id = crate::domain::attestation::hex_lower(&Sha256::digest(&payload));
    document.signature = BASE64.encode(signing_key.sign(&payload).to_bytes());
    document
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        chain::Chain,
        powers::{Reason, SourceVerified, SourceVerifiedSubject},
    };
    use serde_json::json;

    fn entry(contract: &str, ticker: &str, name: &str) -> Entry {
        Entry {
            issuer: "Example Publisher".to_owned(),
            ticker: ticker.to_owned(),
            name: name.to_owned(),
            chain: Chain::Base,
            contract: contract.to_owned(),
            decimals: Some(18),
            source: "test".to_owned(),
            source_url: "https://issuer.example".to_owned(),
            last_checked: "2026-10-01T00:00:00Z".to_owned(),
            removed_at: None,
            stale_since: None,
        }
    }

    fn metadata(address: &str, symbol: &str, name: &str) -> TokenMeta {
        TokenMeta {
            address: address.to_owned(),
            symbol: Some(symbol.to_owned()),
            name: Some(name.to_owned()),
            decimals: Some(18),
            total_supply: None,
        }
    }

    fn powers() -> PowersRecord {
        PowersRecord {
            chain: Chain::Base,
            contract: "0x0000000000000000000000000000000000000001".to_owned(),
            can_seize: Vec::new(),
            can_block: Vec::new(),
            can_change_rules: Vec::new(),
            token_paused: None,
            sanctions_list: None,
            unavailable: Vec::new(),
            source_verified_subject: SourceVerifiedSubject::Contract,
            source_verified: SourceVerified::ExactMatch,
            source_verified_proxy: None,
            observed_at: "2026-10-05T12:00:00Z".to_owned(),
            block: Some(1),
            slot: None,
            reads: Vec::new(),
        }
    }

    #[test]
    fn metadata_alone_cannot_deny_without_fresh_publisher_metadata() {
        let contract = "0x0000000000000000000000000000000000000001";
        let entries = vec![entry(contract, "NVDA", "NVIDIA")];
        let exact = metadata(contract, "NVDA", "NVIDIA xStock");
        let matched = identify(Chain::Base, contract, Some(&exact), &entries);
        assert_eq!(matched.status, IdentityStatus::Match);
        assert_eq!(
            evaluate(&matched, Some(&powers()), None, SourceStatus::Verified).0,
            GuardVerdict::Allow
        );

        let contradicted =
            metadata("0x0000000000000000000000000000000000000002", "n-v-d-a", "NVIDIA token");
        let unknown = identify(Chain::Base, &contradicted.address, Some(&contradicted), &entries);
        assert_eq!(unknown.status, IdentityStatus::NoPublisher);
        assert_eq!(
            evaluate(&unknown, Some(&powers()), None, SourceStatus::Verified).0,
            GuardVerdict::Unknown
        );

        let loose_symbol =
            metadata("0x0000000000000000000000000000000000000002", "NVIDIA", "NVIDIA");
        let unknown = identify(Chain::Base, &loose_symbol.address, Some(&loose_symbol), &entries);
        assert_eq!(unknown.status, IdentityStatus::NoPublisher);
        assert_eq!(
            evaluate(&unknown, Some(&powers()), None, SourceStatus::Verified).0,
            GuardVerdict::Unknown
        );
    }

    #[test]
    fn clone_detection_checks_all_resembling_entries_against_published_metadata() {
        let first = entry("0x0000000000000000000000000000000000000001", "NV", "NVIDIA Group");
        let published = entry("0x0000000000000000000000000000000000000002", "NVDA", "NVIDIA");
        let clone =
            metadata("0x0000000000000000000000000000000000000003", "NVDAx", "NVIDIA xStock");
        let published_metadata = metadata(&published.contract, "NVDAx", "NVIDIA xStock");
        let publisher_metadata = std::collections::HashMap::from([(
            issuer_metadata_key(Chain::Base, &published.contract),
            published_metadata,
        )]);
        let entries = vec![first, published.clone()];

        let identity = identify_with_contract_metadata(
            Chain::Base,
            &clone.address,
            Some(&clone),
            &entries,
            &publisher_metadata,
        );

        assert_eq!(identity.status, IdentityStatus::Mismatch);
        assert_eq!(identity.matched_contract.as_deref(), Some(published.contract.as_str()));
    }

    #[test]
    fn familiar_names_and_short_tickers_never_create_denials() {
        let cases = [
            ("ZG", "Zijin Gold", "PAXG", "Paxos Gold"),
            ("ZG", "Zijin Gold", "XAUT", "Tether Gold"),
            ("PYPL", "PayPal Inc.", "PYUSD", "PayPal USD"),
            ("APLD", "Applied Digital", "FDUSD", "First Digital USD"),
            ("GUSD", "Global Dollar", "USDG", "Global Dollar"),
            ("AI", "C3.ai", "SOMETHING", "Something AI token"),
        ];
        for (ticker, registry_name, symbol, token_name) in cases {
            let entries =
                vec![entry("0x0000000000000000000000000000000000000001", ticker, registry_name)];
            let token = metadata("0x0000000000000000000000000000000000000002", symbol, token_name);
            let identity = identify(Chain::Base, &token.address, Some(&token), &entries);
            let (verdict, reasons) =
                evaluate(&identity, Some(&powers()), None, SourceStatus::Verified);
            assert_ne!(verdict, GuardVerdict::Deny, "{token_name}");
            assert_eq!(verdict, GuardVerdict::Unknown, "{token_name}");
            if identity.candidate.is_some() {
                assert!(
                    reasons.iter().any(|reason| reason.code == "name_resembles_registry_entry"),
                    "{token_name}: {reasons:?}"
                );
            }
        }
    }

    #[test]
    fn verdict_collects_reasons_and_only_denies_observed_wallet_restrictions() {
        let identity = GuardIdentity {
            publisher: Some("Example Publisher".to_owned()),
            matched_contract: Some("0x0000000000000000000000000000000000000001".to_owned()),
            ticker: Some("NVDA".to_owned()),
            status: IdentityStatus::Match,
            candidate: None,
        };
        let mut incomplete = powers();
        incomplete
            .unavailable
            .push(Reason { code: "rpc_error".to_owned(), detail: "read failed".to_owned() });
        let blocked = GuardWalletCheck {
            status: WalletCheckStatus::Checked,
            restrictions: vec![GuardReason {
                code: "wallet_frozen".to_owned(),
                detail: "A matching token account is frozen.".to_owned(),
            }],
        };
        let (verdict, reasons) =
            evaluate(&identity, Some(&incomplete), Some(&blocked), SourceStatus::Unavailable);
        assert_eq!(verdict, GuardVerdict::Deny);
        for code in
            ["publisher_contract_match", "powers_incomplete", "wallet_frozen", "source_unavailable"]
        {
            assert!(reasons.iter().any(|reason| reason.code == code), "{reasons:?}");
        }

        let mut paused = powers();
        paused.token_paused = Some(true);
        let (without_wallet, reasons) =
            evaluate(&identity, Some(&paused), None, SourceStatus::Verified);
        assert_eq!(without_wallet, GuardVerdict::Unknown);
        assert!(reasons.iter().any(|reason| reason.code == "token_paused"));
        assert_eq!(
            reasons.iter().find(|reason| reason.code == "token_paused").unwrap().detail,
            "The token-wide pause is active; transfers are blocked until it is lifted."
        );

        let checked =
            GuardWalletCheck { status: WalletCheckStatus::Checked, restrictions: Vec::new() };
        let (with_wallet, reasons) =
            evaluate(&identity, Some(&paused), Some(&checked), SourceStatus::Verified);
        assert_eq!(with_wallet, GuardVerdict::Deny);
        assert!(reasons.iter().any(|reason| reason.code == "token_paused"));

        let mut capability = powers();
        capability.can_block.push(Reason::new("freeze_authority", "authority exists"));
        assert_eq!(
            evaluate(&identity, Some(&capability), None, SourceStatus::Verified).0,
            GuardVerdict::Allow
        );
        let unavailable =
            GuardWalletCheck { status: WalletCheckStatus::Unavailable, restrictions: Vec::new() };
        assert_eq!(
            evaluate(&identity, Some(&powers()), Some(&unavailable), SourceStatus::Verified).0,
            GuardVerdict::Unknown
        );
        let not_applicable =
            GuardWalletCheck { status: WalletCheckStatus::NotApplicable, restrictions: Vec::new() };
        let (verdict, reasons) =
            evaluate(&identity, Some(&powers()), Some(&not_applicable), SourceStatus::Verified);
        assert_eq!(verdict, GuardVerdict::Unknown);
        assert!(reasons.iter().any(|reason| reason.code == "wallet_check_not_applicable"));
    }

    #[test]
    fn stale_and_removed_contracts_remain_unknown_with_specific_reasons() {
        for (status, code) in [
            (IdentityStatus::RegistryStale, "registry_stale"),
            (IdentityStatus::RegistryRemoved, "registry_removed"),
        ] {
            let mut old = entry("0x0000000000000000000000000000000000000001", "NVDA", "NVIDIA");
            if status == IdentityStatus::RegistryStale {
                old.stale_since = Some("2026-10-01T00:00:00Z".to_owned());
            } else {
                old.removed_at = Some("2026-10-01T00:00:00Z".to_owned());
            }
            let address = old.contract.clone();
            let identity = identify(Chain::Base, &address, None, &[old]);
            assert_eq!(identity.status, status);
            let (verdict, reasons) =
                evaluate(&identity, Some(&powers()), None, SourceStatus::Verified);
            assert_eq!(verdict, GuardVerdict::Unknown);
            assert!(reasons.iter().any(|reason| reason.code == code));
        }
    }

    #[test]
    fn verification_rejects_unknown_guard_fields_recursively() {
        let mut document = signed_test_guard([7; 32]);
        document.wallet = Some("0x0000000000000000000000000000000000000004".to_owned());
        document.wallet_check = Some(GuardWalletCheck {
            status: WalletCheckStatus::Checked,
            restrictions: vec![GuardReason {
                code: "wallet_frozen".to_owned(),
                detail: "frozen".to_owned(),
            }],
        });
        document.powers = Some(powers());
        document.pools = vec![GuardPool {
            address: "0x0000000000000000000000000000000000000003".to_owned(),
            venue: "uniswap-v2".to_owned(),
            quote: GuardQuote {
                address: "0x0000000000000000000000000000000000000004".to_owned(),
                symbol: Some("USDC".to_owned()),
            },
            verdict: "unknown".to_owned(),
            observed_at: None,
        }];
        document.reads = vec![Read {
            method: "eth_call".to_owned(),
            params: json!([]),
            result_hash: "00".to_owned(),
            raw_result: Some(json!("0x")),
            block: Some(1),
            slot: None,
        }];
        document.reasons = vec![GuardReason {
            code: "publisher_contract_match".to_owned(),
            detail: "matched".to_owned(),
        }];
        let valid = serde_json::to_value(document).expect("Guard JSON");
        for path in [
            "",
            "/identity",
            "/wallet_check",
            "/wallet_check/restrictions/0",
            "/powers",
            "/pools/0",
            "/pools/0/quote",
            "/reasons/0",
            "/reads/0",
        ] {
            let mut value = valid.clone();
            let target =
                if path.is_empty() { &mut value } else { value.pointer_mut(path).unwrap() };
            target.as_object_mut().unwrap().insert("note".to_owned(), json!("unsigned"));
            assert!(
                serde_json::from_value::<GuardDocument>(value).is_err(),
                "unexpected field accepted at {path}"
            );
        }
    }

    #[test]
    fn guard_verification_bounds_base58_and_signature_inputs_before_decode() {
        let mut document = signed_test_guard([7; 32]);
        document.public_key = "1".repeat(1_000_000);
        assert!(matches!(verify(&document), Err(GuardVerifyError::PublicKeyLength)));
        document.public_key = "1".repeat(44);
        document.signature = "A".repeat(1_000_000);
        assert!(matches!(verify(&document), Err(GuardVerifyError::SignatureLength)));
    }

    #[test]
    fn guard_signature_covers_verdict_and_reasons() {
        let mut document = signed_test_guard([7; 32]);
        verify(&document).expect("signed guard verifies");
        document.verdict = GuardVerdict::Deny;
        assert!(verify(&document).is_err());
    }
}
