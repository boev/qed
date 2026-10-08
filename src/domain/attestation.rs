use crate::domain::{chain::Chain, check::Verdict, pool::PoolInfo, registry::Entry};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// One observed JSON-RPC read used to produce an attestation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Read {
    pub method: String,
    pub params: Value,
    pub result_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_result: Option<Value>,
    pub block: Option<u64>,
    pub slot: Option<u64>,
}

/// The signed, reproducible result of checking one pool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Attestation {
    pub id: String,
    pub version: u16,
    pub chain: Chain,
    pub subject: String,
    pub verdict: Verdict,
    pub issuer: Option<String>,
    pub ticker: Option<String>,
    pub pool: PoolInfo,
    pub quote_share_of_supply: Option<f64>,
    pub registry_entry: Option<Entry>,
    pub registry_hash: String,
    pub reads: Vec<Read>,
    pub block: Option<u64>,
    pub slot: Option<u64>,
    pub checked_at: String,
    pub expires_at: String,
    pub signer: String,
    pub signature: String,
    pub dev: bool,
}

pub const MAX_ATTESTATION_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct AttestationPayload {
    pub(crate) version: u16,
    pub(crate) chain: Chain,
    pub(crate) subject: String,
    pub(crate) verdict: Verdict,
    pub(crate) issuer: Option<String>,
    pub(crate) ticker: Option<String>,
    pub(crate) pool: PoolInfo,
    pub(crate) quote_share_of_supply: Option<f64>,
    pub(crate) registry_entry: Option<Entry>,
    pub(crate) registry_hash: String,
    pub(crate) reads: Vec<Read>,
    pub(crate) block: Option<u64>,
    pub(crate) slot: Option<u64>,
    pub(crate) checked_at: String,
    pub(crate) expires_at: String,
    pub(crate) signer: String,
    pub(crate) dev: bool,
}

impl Attestation {
    pub(crate) fn payload(&self) -> AttestationPayload {
        AttestationPayload {
            version: self.version,
            chain: self.chain,
            subject: self.subject.clone(),
            verdict: self.verdict.clone(),
            issuer: self.issuer.clone(),
            ticker: self.ticker.clone(),
            pool: self.pool.clone(),
            quote_share_of_supply: self.quote_share_of_supply,
            registry_entry: self.registry_entry.clone(),
            registry_hash: self.registry_hash.clone(),
            reads: self.reads.clone(),
            block: self.block,
            slot: self.slot,
            checked_at: self.checked_at.clone(),
            expires_at: self.expires_at.clone(),
            signer: self.signer.clone(),
            dev: self.dev,
        }
    }
}

#[derive(Debug, Error)]
pub enum VerifyError {
    #[error("attestation payload is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("attestation id is not a 32-byte hexadecimal digest")]
    Id,
    #[error("attestation id does not match the payload")]
    IdMismatch,
    #[error("attestation signer is not valid base58")]
    SignerEncoding,
    #[error("attestation signer is not a 32-byte Ed25519 public key")]
    SignerKey,
    #[error("attestation signature is not valid base64")]
    SignatureEncoding,
    #[error("attestation signature is not 64 bytes")]
    SignatureLength,
    #[error("attestation signature does not verify")]
    SignatureInvalid,
}

pub fn verify(attestation: &Attestation) -> Result<(), VerifyError> {
    let mut payload = canonical_json(&attestation.payload())?;
    if attestation.id.len() != 64 || !attestation.id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(VerifyError::Id);
    }
    let expected = hex_lower(&Sha256::digest(&payload));
    if !attestation.id.eq_ignore_ascii_case(&expected) {
        payload = canonical_json_legacy_chains(&attestation.payload())?;
        let legacy_expected = hex_lower(&Sha256::digest(&payload));
        if !attestation.id.eq_ignore_ascii_case(&legacy_expected) {
            return Err(VerifyError::IdMismatch);
        }
    }
    if attestation.signer.len() > Chain::MAX_SOLANA_ADDRESS_CHARS {
        return Err(VerifyError::SignerKey);
    }
    let mut signer_bytes = [0; 32];
    let signer_len = bs58::decode(&attestation.signer)
        .onto(&mut signer_bytes)
        .map_err(|_| VerifyError::SignerEncoding)?;
    if signer_len != signer_bytes.len() {
        return Err(VerifyError::SignerKey);
    }
    let signer = VerifyingKey::from_bytes(&signer_bytes).map_err(|_| VerifyError::SignerKey)?;
    if attestation.signature.len() != 88 {
        return Err(VerifyError::SignatureLength);
    }
    let mut signature_bytes = [0; 64];
    let signature_len = BASE64
        .decode_slice(&attestation.signature, &mut signature_bytes)
        .map_err(|_| VerifyError::SignatureEncoding)?;
    if signature_len != signature_bytes.len() {
        return Err(VerifyError::SignatureLength);
    }
    let signature =
        Signature::from_slice(&signature_bytes).map_err(|_| VerifyError::SignatureLength)?;
    signer.verify(&payload, &signature).map_err(|_| VerifyError::SignatureInvalid)
}

pub fn canonical_payload_json(attestation: &Attestation) -> Result<String, serde_json::Error> {
    let bytes = canonical_json(&attestation.payload())?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

pub(crate) fn canonical_json<T: Serialize>(value: &T) -> Result<Vec<u8>, serde_json::Error> {
    let value = serde_json::to_value(value)?;
    serde_json::to_vec(&canonical_value(value))
}
pub(crate) fn canonical_json_legacy_chains<T: Serialize>(
    value: &T,
) -> Result<Vec<u8>, serde_json::Error> {
    let mut value = serde_json::to_value(value)?;
    restore_legacy_chain_names(&mut value);
    serde_json::to_vec(&canonical_value(value))
}

pub(crate) fn restore_legacy_chain_names(value: &mut Value) {
    match value {
        Value::Object(object) => {
            if let Some(Value::String(chain)) = object.get_mut("chain") {
                let legacy_name = match chain.as_str() {
                    "solana" => Some("Solana"),
                    "robinhood" => Some("RobinhoodChain"),
                    "base" => Some("Base"),
                    "ethereum" => Some("Ethereum"),
                    "bnb" => Some("Bnb"),
                    _ => None,
                };
                if let Some(legacy_name) = legacy_name {
                    *chain = legacy_name.to_owned();
                }
            }
            for nested in object.values_mut() {
                restore_legacy_chain_names(nested);
            }
        }
        Value::Array(values) => {
            for nested in values {
                restore_legacy_chain_names(nested);
            }
        }
        _ => {}
    }
}

fn canonical_value(value: Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut sorted = Map::new();
            let mut entries = object.into_iter().collect::<Vec<_>>();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            for (key, value) in entries {
                sorted.insert(key, canonical_value(value));
            }
            Value::Object(sorted)
        }
        Value::Array(values) => Value::Array(values.into_iter().map(canonical_value).collect()),
        other => other,
    }
}

pub(crate) fn hex_lower(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::{Attestation, AttestationPayload, canonical_json, verify};

    #[test]
    fn release_7_fixture_verifies_and_canonical_payload_reserializes_identically() {
        let attestation: Attestation = serde_json::from_str(include_str!(
            "../../tests/fixtures/attest/fc6147d4cd42374b72246cc6340d23e26f5c411e63f613322a18413c3da243bb.json"
        ))
        .expect("release-7 attestation fixture");
        verify(&attestation).expect("release-7 signature");

        let canonical = canonical_json(&attestation.payload()).expect("canonical payload");
        let payload: AttestationPayload =
            serde_json::from_slice(&canonical).expect("canonical payload round-trip");

        assert_eq!(canonical_json(&payload).expect("canonical reserialization"), canonical);
    }
}
