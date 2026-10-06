use crate::domain::{attestation::Read, chain::Chain, powers::Reason};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StatementWallet {
    pub chain: Chain,
    pub address: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StatementPosition {
    pub chain: Chain,
    pub wallet: String,
    pub block: Option<u64>,
    pub min_slot: Option<u64>,
    pub max_slot: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatementHolding {
    pub holding: crate::domain::pool::WalletHolding,
    pub slot: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PowersSummary {
    pub can_seize: Vec<Reason>,
    pub can_block: Vec<Reason>,
    pub can_change_rules: Vec<Reason>,
    pub unavailable: Vec<Reason>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StatementAsset {
    pub wallet: String,
    pub chain: Chain,
    pub contract: String,
    pub ticker: String,
    pub issuer: String,
    pub issuer_match: bool,
    pub balance: String,
    pub decimals: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub powers_observed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub powers_block: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub powers_slot: Option<u64>,
    pub slot: Option<u64>,
    pub powers_summary: PowersSummary,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Statement {
    pub id: String,
    pub kind: String,
    pub version: u16,
    pub wallets: Vec<StatementWallet>,
    pub assets: Vec<StatementAsset>,
    pub positions: Vec<StatementPosition>,
    pub block: Option<u64>,
    pub observed_at: String,
    pub reads: Vec<Read>,
    #[serde(default)]
    pub reads_truncated: bool,
    pub signer: String,
    pub signature: String,
    pub dev: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct StatementPayload {
    kind: String,
    version: u16,
    wallets: Vec<StatementWallet>,
    assets: Vec<StatementAsset>,
    positions: Vec<StatementPosition>,
    block: Option<u64>,
    observed_at: String,
    reads: Vec<Read>,
    #[serde(default, skip_serializing_if = "is_false")]
    reads_truncated: bool,
    signer: String,
    dev: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}
impl Statement {
    fn payload(&self) -> StatementPayload {
        StatementPayload {
            kind: self.kind.clone(),
            version: self.version,
            wallets: self.wallets.clone(),
            assets: self.assets.clone(),
            positions: self.positions.clone(),
            block: self.block,
            observed_at: self.observed_at.clone(),
            reads: self.reads.clone(),
            reads_truncated: self.reads_truncated,
            signer: self.signer.clone(),
            dev: self.dev,
        }
    }
}

#[derive(Debug, Error)]
pub enum StatementVerifyError {
    #[error("statement payload is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("statement kind is not `statement`")]
    Kind,
    #[error("statement id is not a 32-byte hexadecimal digest")]
    Id,
    #[error("statement id does not match the payload")]
    IdMismatch,
    #[error("statement signer is not valid base58")]
    SignerEncoding,
    #[error("statement signer is not a 32-byte Ed25519 public key")]
    SignerKey,
    #[error("statement signature is not valid base64")]
    SignatureEncoding,
    #[error("statement signature is not 64 bytes")]
    SignatureLength,
    #[error("statement signature does not verify")]
    SignatureInvalid,
}

pub fn canonical_payload_json(statement: &Statement) -> Result<Vec<u8>, serde_json::Error> {
    crate::domain::attestation::canonical_json(&statement.payload())
}

pub fn verify(statement: &Statement) -> Result<(), StatementVerifyError> {
    if statement.kind != "statement" {
        return Err(StatementVerifyError::Kind);
    }
    let payload = canonical_payload_json(statement)?;
    let digest = Sha256::digest(&payload);
    let expected = crate::domain::attestation::hex_lower(&digest);
    if statement.id.len() != 64 || !statement.id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(StatementVerifyError::Id);
    }
    if !statement.id.eq_ignore_ascii_case(&expected) {
        return Err(StatementVerifyError::IdMismatch);
    }
    if statement.signer.len() > Chain::MAX_SOLANA_ADDRESS_CHARS {
        return Err(StatementVerifyError::SignerKey);
    }
    let mut signer_bytes = [0; 32];
    let signer_len = bs58::decode(&statement.signer)
        .onto(&mut signer_bytes)
        .map_err(|_| StatementVerifyError::SignerEncoding)?;
    if signer_len != signer_bytes.len() {
        return Err(StatementVerifyError::SignerKey);
    }
    let signer =
        VerifyingKey::from_bytes(&signer_bytes).map_err(|_| StatementVerifyError::SignerKey)?;
    if statement.signature.len() != 88 {
        return Err(StatementVerifyError::SignatureLength);
    }
    let mut signature_bytes = [0; 64];
    let signature_len = BASE64
        .decode_slice(&statement.signature, &mut signature_bytes)
        .map_err(|_| StatementVerifyError::SignatureEncoding)?;
    if signature_len != signature_bytes.len() {
        return Err(StatementVerifyError::SignatureLength);
    }
    let signature = Signature::from_slice(&signature_bytes)
        .map_err(|_| StatementVerifyError::SignatureLength)?;
    signer.verify(&payload, &signature).map_err(|_| StatementVerifyError::SignatureInvalid)
}

#[cfg(test)]
pub(crate) fn signed_test_statement(key: [u8; 32], dev: bool) -> Statement {
    let signing_key = ed25519_dalek::SigningKey::from_bytes(&key);
    let mut statement = Statement {
        id: String::new(),
        kind: "statement".to_owned(),
        version: 1,
        wallets: Vec::new(),
        assets: Vec::new(),
        positions: Vec::new(),
        block: None,
        observed_at: "2026-01-01T00:00:00Z".to_owned(),
        reads: Vec::new(),
        reads_truncated: false,
        signer: bs58::encode(signing_key.verifying_key().as_bytes()).into_string(),
        signature: String::new(),
        dev,
    };
    let payload = canonical_payload_json(&statement).expect("test statement serializes");
    statement.id = crate::domain::attestation::hex_lower(&Sha256::digest(&payload));
    let signature = ed25519_dalek::Signer::sign(&signing_key, &payload);
    statement.signature = BASE64.encode(signature.to_bytes());
    statement
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statement_kind_is_part_of_the_signed_contract() {
        let mut statement = signed_test_statement([7; 32], false);
        verify(&statement).expect("signed statement");
        statement.kind = "attestation".to_owned();
        assert!(matches!(verify(&statement), Err(StatementVerifyError::Kind)));
    }
    #[test]
    fn read_log_truncation_is_signed_and_legacy_payloads_default_to_false() {
        let statement = signed_test_statement([7; 32], false);
        let mut legacy = serde_json::to_value(&statement).unwrap();
        legacy.as_object_mut().unwrap().remove("reads_truncated");
        let legacy: Statement = serde_json::from_value(legacy).unwrap();
        verify(&legacy).expect("old statement without reads_truncated still verifies");

        let mut changed = statement;
        changed.reads_truncated = true;
        assert!(matches!(verify(&changed), Err(StatementVerifyError::IdMismatch)));
    }
}
