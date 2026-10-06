#[cfg(test)]
use crate::domain::{attestation::canonical_payload_json, pool::TokenSide};
#[cfg(test)]
use crate::ports::{MAX_RAW_RESULT_BYTES, MAX_READ_LOG_ENTRIES, capture_reads, record_read};
use crate::{
    app::context::Context,
    domain::{
        attestation::{
            Attestation, AttestationPayload, MAX_ATTESTATION_BYTES, canonical_json, hex_lower,
            verify,
        },
        chain::Chain,
        check::{CheckResult, Verdict},
        pool::PoolInfo,
        registry::{self, Entry, Registry},
    },
    ports::ReadLog,
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use chrono::{Duration, SecondsFormat, Utc};
#[cfg(test)]
use ed25519_dalek::{Signer as Ed25519Signer, SigningKey};
use serde::{Deserialize, Serialize};
#[cfg(test)]
use serde_json::Value;
use sha2::{Digest, Sha256};

pub async fn get_async(state: &Context, id: &str) -> Option<Attestation> {
    if let Some(attestation) = get(state, id) {
        if valid_for_state(state, id, &attestation).await {
            return Some(attestation);
        }
        quarantine_if_invalid(state, id, &attestation).await;
        return None;
    }
    let attestation = state.attest_store.get(id).await.ok().flatten()?;
    if !valid_for_state(state, id, &attestation).await {
        quarantine_if_invalid(state, id, &attestation).await;
        return None;
    }
    cache_index(state, &attestation);
    Some(attestation)
}

/// Load a certificate for its historical proof page without requiring it to
/// remain within the current freshness window. Expiry is still shown by the
/// certificate, while signature, signer, environment, and ID checks remain
/// mandatory.
pub async fn get_certificate_async(state: &Context, id: &str) -> Option<Attestation> {
    if let Some(attestation) = get(state, id) {
        if certificate_valid_for_state(state, id, &attestation) {
            return Some(attestation);
        }
        quarantine_if_invalid(state, id, &attestation).await;
        return None;
    }
    let attestation = state.attest_store.get(id).await.ok().flatten()?;
    if !certificate_valid_for_state(state, id, &attestation) {
        quarantine_if_invalid(state, id, &attestation).await;
        return None;
    }
    cache_index(state, &attestation);
    Some(attestation)
}

pub(crate) fn certificate_valid_for_state(
    state: &Context,
    id: &str,
    attestation: &Attestation,
) -> bool {
    id.eq_ignore_ascii_case(&attestation.id)
        && verify(attestation).is_ok()
        && signer_trusted(state, attestation)
        && attestation.dev == state.dev_signer
}
async fn quarantine_if_invalid(state: &Context, id: &str, attestation: &Attestation) {
    if certificate_valid_for_state(state, id, attestation) {
        return;
    }
    tracing::warn!(%id, "quarantining cryptographically invalid attestation");
    if let Err(error) = state.attest_store.quarantine(id).await {
        tracing::warn!(%error, %id, "could not quarantine invalid attestation");
    }
}

fn cache_index(state: &Context, attestation: &Attestation) {
    if let Ok(mut index) = state.attestations.write() {
        if index.len() >= 10_000
            && let Some(oldest) = index
                .iter()
                .min_by(|left, right| left.1.checked_at.cmp(&right.1.checked_at))
                .map(|(id, _)| id.clone())
        {
            index.remove(&oldest);
        }
        index.insert(attestation.id.clone(), attestation.clone());
    }
}

pub fn get(state: &Context, id: &str) -> Option<Attestation> {
    state.attestations.read().ok()?.get(id).cloned()
}

pub async fn latest_for_pool_async(
    state: &Context,
    chain: Chain,
    pool: &str,
) -> Option<Attestation> {
    let registry = state.registry.snapshot().await;
    latest_for_pool_with_registry(state, &registry, chain, pool).await
}

async fn latest_for_pool_with_registry(
    state: &Context,
    registry: &Registry,
    chain: Chain,
    pool: &str,
) -> Option<Attestation> {
    let attestation =
        state.attest_store.latest_for_pool(&chain.to_string(), pool).await.ok().flatten()?;
    if !valid_for_state_with_registry(state, registry, &attestation.id, &attestation) {
        quarantine_if_invalid(state, &attestation.id, &attestation).await;
        return None;
    }
    cache_index(state, &attestation);
    Some(attestation)
}

pub fn public_key_b58(state: &Context) -> String {
    state.signer.public_key()
}

#[cfg(test)]
pub fn trusted_signer(attestation: &Attestation, expected: &str) -> bool {
    attestation.signer == expected
}

pub fn signer_trusted(state: &Context, attestation: &Attestation) -> bool {
    state.signer.public_key() == attestation.signer
        || state.trusted_signers.contains(&attestation.signer)
}

fn within_last_minute(timestamp: &str) -> bool {
    chrono::DateTime::parse_from_rfc3339(timestamp)
        .ok()
        .is_some_and(|value| (Utc::now() - value.with_timezone(&Utc)).num_seconds().abs() < 60)
}

pub fn fresh(attestation: &Attestation) -> bool {
    let now = Utc::now();
    let checked = chrono::DateTime::parse_from_rfc3339(&attestation.checked_at).ok();
    let expires = chrono::DateTime::parse_from_rfc3339(&attestation.expires_at).ok();
    checked.is_some_and(|value| value <= now.fixed_offset())
        && expires.is_some_and(|value| value > now.fixed_offset())
}

fn current_registry_entry(registry: &Registry, attestation: &Attestation) -> bool {
    let Some(recorded) = attestation.registry_entry.as_ref() else {
        return !matches!(attestation.verdict, Verdict::Verified { .. });
    };
    registry.iter().any(|current| {
        current.issuer == recorded.issuer
            && current.ticker == recorded.ticker
            && current.chain == recorded.chain
            && registry::matchable(current)
            && if current.chain == Chain::Solana {
                current.contract == recorded.contract
            } else {
                current.contract.eq_ignore_ascii_case(&recorded.contract)
            }
    })
}

pub(crate) fn valid_for_state_with_registry(
    state: &Context,
    registry: &Registry,
    id: &str,
    attestation: &Attestation,
) -> bool {
    id.eq_ignore_ascii_case(&attestation.id)
        && verify(attestation).is_ok()
        && signer_trusted(state, attestation)
        && attestation.dev == state.dev_signer
        && current_registry_entry(registry, attestation)
        && fresh(attestation)
}

pub(crate) async fn valid_for_state(state: &Context, id: &str, attestation: &Attestation) -> bool {
    let registry = state.registry.snapshot().await;
    valid_for_state_with_registry(state, &registry, id, attestation)
}

pub async fn recheck(state: &Context, id: &str) -> RecheckResult {
    let Some(previous) = get(state, id) else {
        return RecheckResult {
            equal: false,
            changed: vec!["attestation not found".to_owned()],
            fresh_id: String::new(),
        };
    };
    if within_last_minute(&previous.checked_at) {
        return RecheckResult { equal: true, changed: Vec::new(), fresh_id: previous.id };
    }
    state.check_cache.invalidate(&crate::domain::check::cache_key(&previous.subject)).await;
    let fresh = crate::app::check::check(state, &previous.subject).await;
    let Some(fresh_id) = fresh.attestation_id.clone() else {
        return RecheckResult {
            equal: false,
            changed: vec!["fresh check did not produce an attestation".to_owned()],
            fresh_id: String::new(),
        };
    };
    let Some(current) = get(state, &fresh_id) else {
        return RecheckResult {
            equal: false,
            changed: vec!["fresh attestation was not stored".to_owned()],
            fresh_id,
        };
    };
    let mut changed = Vec::new();
    if previous.verdict != current.verdict {
        changed.push("verdict".to_owned());
    }
    if previous.pool.quote.address != current.pool.quote.address {
        changed.push("quote contract".to_owned());
    }
    let before = previous.reads.iter().map(|read| read.result_hash.as_str()).collect::<Vec<_>>();
    let after = current.reads.iter().map(|read| read.result_hash.as_str()).collect::<Vec<_>>();
    if before != after {
        changed.push("read result hashes".to_owned());
    }
    RecheckResult { equal: changed.is_empty(), changed, fresh_id }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecheckResult {
    pub equal: bool,
    pub changed: Vec<String>,
    pub fresh_id: String,
}

pub(crate) async fn create_for_check(
    state: &Context,
    registry: &Registry,
    result: &CheckResult,
    read_log: ReadLog,
) -> Option<Attestation> {
    let pool = result.pool.clone()?;
    if matches!(result.verdict, Verdict::Unknown { .. }) {
        return None;
    }
    let registry_entry = match &result.verdict {
        Verdict::Verified { issuer, ticker } => registry
            .iter()
            .find(|entry| {
                registry::matchable(entry)
                    && entry.chain == result.chain
                    && entry.issuer == *issuer
                    && entry.ticker == *ticker
            })
            .cloned(),
        Verdict::Mismatch { claimed, .. } => registry
            .iter()
            .find(|entry| {
                registry::matchable(entry)
                    && entry.chain == result.chain
                    && entry.ticker == *claimed
            })
            .cloned(),
        Verdict::NoMatch | Verdict::Unknown { .. } => None,
    };
    if matches!(result.verdict, Verdict::Verified { .. }) && registry_entry.is_none() {
        return None;
    }
    let (issuer, ticker) = registry_entry
        .as_ref()
        .map(|entry| (Some(entry.issuer.clone()), Some(entry.ticker.clone())))
        .unwrap_or_else(|| match &result.verdict {
            Verdict::Verified { issuer, ticker } => (Some(issuer.clone()), Some(ticker.clone())),
            Verdict::Mismatch { claimed, .. } => (None, Some(claimed.clone())),
            Verdict::NoMatch | Verdict::Unknown { .. } => (None, None),
        });
    if let Some(previous) =
        latest_for_pool_with_registry(state, registry, result.chain, &pool.pool).await
    {
        let registry_hash =
            state.registry_hash.read().map(|value| value.clone()).unwrap_or_default();
        if fresh(&previous)
            && previous.verdict == result.verdict
            && previous.pool == pool
            && previous.reads == read_log.reads
            && previous.registry_hash == registry_hash
        {
            return Some(previous);
        }
    }
    let expires_at =
        (state.clock.now() + Duration::hours(24)).to_rfc3339_opts(SecondsFormat::Secs, true);
    let Some(attestation) = sign_payload(
        state,
        result.chain,
        pool.pool.clone(),
        result.verdict.clone(),
        issuer,
        ticker,
        pool,
        result.quote_share_of_supply,
        registry_entry,
        result.checked_at.clone(),
        expires_at,
        read_log,
    ) else {
        tracing::warn!("refusing to sign an oversized attestation");
        return None;
    };
    if !persist(state, &attestation).await {
        return None;
    }
    Some(attestation)
}
pub(crate) fn sign_document_payload(
    state: &Context,
    payload: &[u8],
) -> Result<(String, String), String> {
    let id = hex_lower(&Sha256::digest(payload));
    let signature = BASE64.encode(state.signer.sign(payload)?);
    Ok((id, signature))
}

#[allow(clippy::too_many_arguments)]
fn sign_payload(
    state: &Context,
    chain: Chain,
    subject: String,
    verdict: Verdict,
    issuer: Option<String>,
    ticker: Option<String>,
    pool: PoolInfo,
    quote_share_of_supply: Option<f64>,
    registry_entry: Option<Entry>,
    checked_at: String,
    expires_at: String,
    read_log: ReadLog,
) -> Option<Attestation> {
    let registry_hash = state.registry_hash.read().map(|value| value.clone()).unwrap_or_default();
    let signer = public_key_b58(state);
    let payload = AttestationPayload {
        version: 1,
        chain,
        subject,
        verdict,
        issuer,
        ticker,
        pool,
        quote_share_of_supply,
        registry_entry,
        registry_hash,
        reads: read_log.reads,
        block: read_log.block,
        slot: read_log.slot,
        checked_at,
        expires_at,
        signer: signer.clone(),
        dev: state.dev_signer,
    };
    let bytes = match canonical_json(&payload) {
        Ok(bytes) => bytes,
        Err(error) => unreachable!("attestation payload is serializable: {error}"),
    };
    if bytes.len() > MAX_ATTESTATION_BYTES.saturating_sub(1024) {
        return None;
    }
    let (id, signature) = sign_document_payload(state, &bytes).ok()?;
    Some(Attestation {
        id,
        version: payload.version,
        chain: payload.chain,
        subject: payload.subject,
        verdict: payload.verdict,
        issuer: payload.issuer,
        ticker: payload.ticker,
        pool: payload.pool,
        quote_share_of_supply: payload.quote_share_of_supply,
        registry_entry: payload.registry_entry,
        registry_hash: payload.registry_hash,
        reads: payload.reads,
        block: payload.block,
        slot: payload.slot,
        checked_at: payload.checked_at,
        expires_at: payload.expires_at,
        signer: payload.signer,
        signature,
        dev: payload.dev,
    })
}

async fn persist(state: &Context, attestation: &Attestation) -> bool {
    let bytes = match serde_json::to_vec(attestation) {
        Ok(bytes) if bytes.len() <= MAX_ATTESTATION_BYTES => bytes,
        Ok(_) => {
            tracing::warn!(id = %attestation.id, "refusing to persist an oversized attestation");
            return false;
        }
        Err(error) => {
            tracing::warn!(%error, "could not serialize attestation");
            return false;
        }
    };
    let _ = bytes;
    if let Err(error) = state.attest_store.persist(attestation).await {
        tracing::warn!(%error, "could not persist attestation");
        return false;
    }
    cache_index(state, attestation);
    true
}

pub(crate) async fn hydrate_attestations(state: &Context) -> std::io::Result<()> {
    let cutoff = state.clock.now() - Duration::days(30);
    if let Err(error) = state.attest_store.prune_expired(cutoff).await {
        tracing::warn!(%error, "could not prune expired attestations");
    }
    let loaded = state.attest_store.load_recent().await?;
    let mut records = loaded.into_iter().collect::<Vec<_>>();
    records.sort_by(|left, right| {
        right.1.checked_at.cmp(&left.1.checked_at).then_with(|| right.0.cmp(&left.0))
    });
    records.truncate(10_000);
    let mut valid = std::collections::HashMap::with_capacity(records.len());
    let registry = state.registry.snapshot().await;
    for (id, attestation) in records {
        if valid_for_state_with_registry(state, &registry, &id, &attestation)
            || certificate_valid_for_state(state, &id, &attestation)
        {
            valid.insert(id, attestation);
        } else {
            tracing::warn!(%id, "quarantining invalid persisted attestation");
            let _ = state.attest_store.quarantine(&id).await;
        }
    }
    let count = valid.len();
    if let Ok(mut index) = state.attestations.write() {
        *index = valid;
    }
    tracing::info!(count, "indexed persisted attestations");
    Ok(())
}
#[cfg(test)]
pub(crate) fn signed_test_attestation(key: [u8; 32], dev: bool) -> Attestation {
    signed_test_attestation_with_quote_share(key, dev, 0.2)
}

#[cfg(test)]
pub(crate) fn signed_test_attestation_with_quote_share(
    key: [u8; 32],
    dev: bool,
    quote_share: f64,
) -> Attestation {
    let signing_key = SigningKey::from_bytes(&key);
    let pool = PoolInfo {
        chain: Chain::Base,
        pool: "0x0000000000000000000000000000000000000001".to_owned(),
        dex: "uniswap-v2".to_owned(),
        base: TokenSide {
            address: "0x0000000000000000000000000000000000000002".to_owned(),
            symbol: Some("xNVDA".to_owned()),
            decimals: Some(18),
            balance: Some("1".to_owned()),
        },
        quote: TokenSide {
            address: "0x0000000000000000000000000000000000000003".to_owned(),
            symbol: Some("USDG".to_owned()),
            decimals: Some(6),
            balance: Some("2".to_owned()),
        },
    };
    let payload = AttestationPayload {
        version: 1,
        chain: Chain::Base,
        subject: pool.pool.clone(),
        verdict: Verdict::NoMatch,
        issuer: None,
        ticker: None,
        pool,
        quote_share_of_supply: Some(quote_share),
        registry_entry: None,
        registry_hash: String::new(),
        reads: Vec::new(),
        block: None,
        slot: None,
        checked_at: (Utc::now() - Duration::minutes(1)).to_rfc3339_opts(SecondsFormat::Secs, true),
        expires_at: (Utc::now() + Duration::hours(1)).to_rfc3339_opts(SecondsFormat::Secs, true),
        signer: bs58::encode(signing_key.verifying_key().as_bytes()).into_string(),
        dev,
    };
    let bytes = canonical_json(&payload).expect("test attestation payload serializes");
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Attestation {
        id: hex_lower(&hasher.finalize()),
        version: payload.version,
        chain: payload.chain,
        subject: payload.subject,
        verdict: payload.verdict,
        issuer: payload.issuer,
        ticker: payload.ticker,
        pool: payload.pool,
        quote_share_of_supply: payload.quote_share_of_supply,
        registry_entry: payload.registry_entry,
        registry_hash: payload.registry_hash,
        reads: payload.reads,
        block: payload.block,
        slot: payload.slot,
        checked_at: payload.checked_at,
        expires_at: payload.expires_at,
        signer: payload.signer,
        signature: BASE64.encode(Ed25519Signer::sign(&signing_key, &bytes).to_bytes()),
        dev: payload.dev,
    }
}
#[cfg(test)]
pub(crate) fn expired_signed_test_attestation(key: [u8; 32], dev: bool) -> Attestation {
    let signing_key = SigningKey::from_bytes(&key);
    let mut attestation = signed_test_attestation(key, dev);
    attestation.checked_at =
        (Utc::now() - Duration::hours(2)).to_rfc3339_opts(SecondsFormat::Secs, true);
    attestation.expires_at =
        (Utc::now() - Duration::minutes(1)).to_rfc3339_opts(SecondsFormat::Secs, true);
    let payload = attestation.payload();
    let bytes = canonical_json(&payload).expect("expired test payload serializes");
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    attestation.id = hex_lower(&hasher.finalize());
    attestation.signature = BASE64.encode(Ed25519Signer::sign(&signing_key, &bytes).to_bytes());
    attestation
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::context::test_support::{TestContext, build, build_with_signers};
    use crate::domain::attestation::Read;
    use crate::ports::IssuerRegistry;
    use serde_json::json;
    use std::collections::HashSet;

    fn test_state(dev_signer: bool) -> TestContext {
        build(Vec::new(), Vec::new(), dev_signer)
    }
    fn update_registry(state: &TestContext, mutate: impl FnOnce(&mut Registry)) {
        let mut current = state.registry.try_write().unwrap();
        let mut entries = current.as_ref().clone();
        mutate(&mut entries);
        *current = std::sync::Arc::new(entries);
    }

    fn sample() -> Attestation {
        let signing_key = SigningKey::from_bytes(&[7; 32]);
        let signer = bs58::encode(signing_key.verifying_key().as_bytes()).into_string();
        let pool = PoolInfo {
            chain: Chain::Base,
            pool: "0x0000000000000000000000000000000000000001".to_owned(),
            dex: "uniswap-v2".to_owned(),
            base: TokenSide {
                address: "0x0000000000000000000000000000000000000002".to_owned(),
                symbol: Some("xNVDA".to_owned()),
                decimals: Some(18),
                balance: Some("1".to_owned()),
            },
            quote: TokenSide {
                address: "0x0000000000000000000000000000000000000003".to_owned(),
                symbol: Some("USDG".to_owned()),
                decimals: Some(6),
                balance: Some("2".to_owned()),
            },
        };
        let payload = AttestationPayload {
            version: 1,
            chain: Chain::Base,
            subject: pool.pool.clone(),
            verdict: Verdict::NoMatch,
            issuer: None,
            ticker: None,
            pool,
            quote_share_of_supply: Some(0.2),
            registry_entry: None,
            registry_hash: "a".repeat(64),
            reads: vec![Read {
                method: "eth_call".to_owned(),
                params: json!(["0x00"]),
                result_hash: "b".repeat(64),
                raw_result: Some(json!("0x01")),
                block: Some(1),
                slot: None,
            }],
            block: Some(1),
            slot: None,
            checked_at: "2026-09-22T00:00:00Z".to_owned(),
            expires_at: "2026-09-23T00:00:00Z".to_owned(),
            signer,
            dev: false,
        };
        let bytes = canonical_json(&payload).unwrap();
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        let id = hex_lower(&hasher.finalize());
        let signature = BASE64.encode(Ed25519Signer::sign(&signing_key, &bytes).to_bytes());
        Attestation {
            id,
            version: payload.version,
            chain: payload.chain,
            subject: payload.subject,
            verdict: payload.verdict,
            issuer: payload.issuer,
            ticker: payload.ticker,
            pool: payload.pool,
            quote_share_of_supply: payload.quote_share_of_supply,
            registry_entry: payload.registry_entry,
            registry_hash: payload.registry_hash,
            reads: payload.reads,
            block: payload.block,
            slot: payload.slot,
            checked_at: payload.checked_at,
            expires_at: payload.expires_at,
            signer: payload.signer,
            signature,
            dev: payload.dev,
        }
    }
    fn signed_sample(key: [u8; 32], dev: bool) -> Attestation {
        let signing_key = SigningKey::from_bytes(&key);
        let mut attestation = sample();
        attestation.checked_at =
            (Utc::now() - Duration::minutes(1)).to_rfc3339_opts(SecondsFormat::Secs, true);
        attestation.expires_at =
            (Utc::now() + Duration::hours(1)).to_rfc3339_opts(SecondsFormat::Secs, true);
        attestation.signer = bs58::encode(signing_key.verifying_key().as_bytes()).into_string();
        attestation.dev = dev;
        let payload = attestation.payload();
        let bytes = canonical_json(&payload).unwrap();
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        attestation.id = hex_lower(&hasher.finalize());
        attestation.signature = BASE64.encode(Ed25519Signer::sign(&signing_key, &bytes).to_bytes());
        attestation
    }

    #[tokio::test]
    async fn fresh_attestation_matches_the_position_read_method_fixture() {
        const PRE_CHANGE_METHODS: [&str; 2] = ["eth_blockNumber", "eth_call"];

        let ((), read_log) = capture_reads(async {
            let block_result = json!("0x7b");
            record_read("eth_blockNumber", json!([]), &block_result, false, Some(123), None);
            let call_result = json!("0x01");
            record_read(
                "eth_call",
                json!([{"to": "0x0000000000000000000000000000000000000001"}]),
                &call_result,
                true,
                Some(123),
                None,
            );
        })
        .await;
        let state = test_state(false);
        let pool = sample().pool;
        let attestation = sign_payload(
            &state,
            Chain::Base,
            pool.pool.clone(),
            Verdict::NoMatch,
            None,
            None,
            pool,
            None,
            None,
            "2026-10-02T00:00:00Z".to_owned(),
            "2026-10-03T00:00:00Z".to_owned(),
            read_log,
        )
        .expect("fresh attestation");
        let methods = attestation.reads.iter().map(|read| read.method.as_str()).collect::<Vec<_>>();

        assert_eq!(methods, PRE_CHANGE_METHODS);
        assert_eq!(attestation.block, Some(123));
    }

    #[tokio::test]
    async fn solana_get_slot_read_is_kept_after_account_context_slot() {
        let ((), read_log) = capture_reads(async {
            let account_result = json!({"context": {"slot": 456}});
            record_read("getAccountInfo", json!(["mint"]), &account_result, false, None, Some(456));
            let slot_result = json!(455);
            record_read("getSlot", json!([]), &slot_result, false, None, Some(455));
        })
        .await;
        let methods = read_log.reads.iter().map(|read| read.method.as_str()).collect::<Vec<_>>();

        assert_eq!(methods, ["getAccountInfo", "getSlot"]);
        assert_eq!(read_log.slot, Some(456));
    }
    fn verified_registry_entry() -> Entry {
        Entry {
            issuer: "Issuer".to_owned(),
            ticker: "xNVDA".to_owned(),
            name: "xNVDA".to_owned(),
            chain: Chain::Base,
            contract: "0x0000000000000000000000000000000000000002".to_owned(),
            decimals: Some(18),
            source: "manual".to_owned(),
            source_url: "https://example.invalid".to_owned(),
            last_checked: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
            removed_at: None,
            stale_since: None,
        }
    }

    fn signed_verified(state: &Context, entry: Entry) -> Attestation {
        let pool = sample().pool;
        sign_payload(
            state,
            Chain::Base,
            pool.pool.clone(),
            Verdict::Verified { issuer: entry.issuer.clone(), ticker: entry.ticker.clone() },
            Some(entry.issuer.clone()),
            Some(entry.ticker.clone()),
            pool,
            Some(0.2),
            Some(entry),
            (Utc::now() - Duration::minutes(1)).to_rfc3339_opts(SecondsFormat::Secs, true),
            (Utc::now() + Duration::hours(1)).to_rfc3339_opts(SecondsFormat::Secs, true),
            ReadLog::default(),
        )
        .expect("fixture attestation fits size limit")
    }

    #[tokio::test]
    async fn live_attestation_rejects_revoked_or_stale_entry_but_certificate_keeps_history() {
        let state = test_state(false);
        let entry = verified_registry_entry();
        update_registry(&state, |registry| registry.push(entry.clone()));
        let attestation = signed_verified(&state, entry);

        assert!(valid_for_state(&state, &attestation.id, &attestation).await);

        update_registry(&state, |registry| {
            registry[0].removed_at = Some("2026-09-23T00:00:00Z".to_owned());
        });
        assert!(!valid_for_state(&state, &attestation.id, &attestation).await);
        assert!(certificate_valid_for_state(&state, &attestation.id, &attestation));

        update_registry(&state, |registry| {
            registry[0].removed_at = None;
            registry[0].stale_since = Some("2026-09-23T00:00:00Z".to_owned());
        });
        assert!(!valid_for_state(&state, &attestation.id, &attestation).await);
        assert!(certificate_valid_for_state(&state, &attestation.id, &attestation));
    }

    #[tokio::test]
    async fn verified_attestation_requires_current_registry_entry() {
        let state = test_state(false);
        let pool = sample().pool;
        let result = CheckResult {
            input: pool.pool.clone(),
            chain: Chain::Base,
            pool: Some(pool),
            verdict: Verdict::Verified { issuer: "Issuer".to_owned(), ticker: "xNVDA".to_owned() },
            quote_share_of_supply: Some(0.2),
            evidence: Vec::new(),
            checked_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
            attestation_id: None,
            powers: None,
        };

        let registry = state.registry.snapshot().await;
        assert!(create_for_check(&state, &registry, &result, ReadLog::default()).await.is_none());
    }

    #[tokio::test]
    async fn check_powers_remain_outside_the_signed_attestation_payload() {
        let state = test_state(false);
        let pool = sample().pool;
        let result = CheckResult {
            input: pool.pool.clone(),
            chain: Chain::Base,
            pool: Some(pool),
            verdict: Verdict::NoMatch,
            quote_share_of_supply: None,
            evidence: Vec::new(),
            checked_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
            attestation_id: None,
            powers: Some(crate::domain::powers::PowersRecord {
                chain: Chain::Base,
                contract: "0x0000000000000000000000000000000000000002".to_owned(),
                can_seize: Vec::new(),
                can_block: Vec::new(),
                can_change_rules: Vec::new(),
                token_paused: None,
                sanctions_list: None,
                unavailable: Vec::new(),
                source_verified_subject: crate::domain::powers::SourceVerifiedSubject::Contract,
                source_verified: crate::domain::powers::SourceVerified::None,
                source_verified_proxy: None,
                observed_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
                block: None,
                slot: None,
                reads: Vec::new(),
            }),
        };

        let registry = state.registry.snapshot().await;
        let attestation = create_for_check(&state, &registry, &result, ReadLog::default())
            .await
            .expect("NoMatch attestation");
        let payload = canonical_payload_json(&attestation).expect("canonical payload");
        assert!(!payload.contains("\"powers\""));
        verify(&attestation).expect("signed payload remains valid");
    }

    #[test]
    fn sign_and_verify_round_trip() {
        verify(&sample()).unwrap();
    }

    #[test]
    fn awkward_float_attestations_verify_after_json_round_trip() {
        for quote_share in [1.1129609814871755e-8, 0.1 + 0.2] {
            let original = signed_test_attestation_with_quote_share([7; 32], false, quote_share);
            let serialized = serde_json::to_vec(&original).expect("serialize test attestation");
            let parsed: Attestation =
                serde_json::from_slice(&serialized).expect("parse test attestation");

            assert_eq!(parsed.id, original.id, "quote share: {quote_share:?}");
            assert_eq!(parsed.quote_share_of_supply, Some(quote_share));
            verify(&parsed).expect("serialized attestation still verifies");
        }
    }

    #[test]
    fn tampered_payload_fails_verification() {
        let mut attestation = sample();
        attestation.subject.push('x');
        assert!(verify(&attestation).is_err());
    }

    #[test]
    fn expired_attestation_is_not_fresh() {
        assert!(!fresh(&sample()));
    }
    #[tokio::test]
    async fn expired_attestation_remains_valid_as_historical_certificate() {
        let state = test_state(false);
        let attestation = expired_signed_test_attestation([7; 32], false);

        assert!(!valid_for_state(&state, &attestation.id, &attestation).await);
        assert!(certificate_valid_for_state(&state, &attestation.id, &attestation));
    }

    #[test]
    fn foreign_signer_is_not_trusted() {
        let attestation = sample();
        let foreign = bs58::encode([8u8; 32]).into_string();
        assert!(!trusted_signer(&attestation, &foreign));
    }

    #[tokio::test]
    async fn state_trust_policy_accepts_current_and_previous_keys() {
        let previous_key =
            bs58::encode(SigningKey::from_bytes(&[8; 32]).verifying_key().as_bytes()).into_string();
        let state = build_with_signers(
            Vec::new(),
            Vec::new(),
            false,
            [7; 32],
            HashSet::from([previous_key]),
        );
        let current = signed_sample([7; 32], false);
        assert!(signer_trusted(&state, &current));
        assert!(valid_for_state(&state, &current.id, &current).await);
        let rotated = signed_sample([8; 32], false);
        assert!(signer_trusted(&state, &rotated));
        assert!(valid_for_state(&state, &rotated.id, &rotated).await);
    }

    #[tokio::test]
    async fn state_trust_policy_rejects_foreign_and_environment_mismatch() {
        let production = test_state(false);
        let foreign = signed_sample([9; 32], false);
        assert!(!valid_for_state(&production, &foreign.id, &foreign).await);
        let development_attestation = signed_sample([7; 32], true);
        assert!(
            !valid_for_state(&production, &development_attestation.id, &development_attestation)
                .await
        );

        let development = test_state(true);
        let production_attestation = signed_sample([7; 32], false);
        assert!(
            !valid_for_state(&development, &production_attestation.id, &production_attestation)
                .await
        );
    }
    #[test]
    fn canonical_json_sorts_nested_keys_without_whitespace() {
        let left = canonical_json(&json!({"z": {"b": 2, "a": 1}, "a": [3, 2, 1]})).unwrap();
        let right = canonical_json(&json!({"a": [3, 2, 1], "z": {"a": 1, "b": 2}})).unwrap();
        assert_eq!(left, right);
        assert_eq!(left, br#"{"a":[3,2,1],"z":{"a":1,"b":2}}"#);
    }

    #[test]
    fn changed_read_hash_is_detected() {
        let mut before = sample();
        let mut after = sample();
        after.reads[0].result_hash = "c".repeat(64);
        let old = before.reads.iter().map(|read| read.result_hash.as_str()).collect::<Vec<_>>();
        let new = after.reads.iter().map(|read| read.result_hash.as_str()).collect::<Vec<_>>();
        assert_ne!(old, new);

        before.reads[0].result_hash = after.reads[0].result_hash.clone();
        assert_eq!(before.reads, after.reads);
    }

    #[tokio::test]
    async fn read_log_caps_entries_and_raw_result_bytes() {
        let oversized = Value::String("x".repeat(MAX_RAW_RESULT_BYTES));
        let (_, log) = capture_reads(async {
            record_read("oversized", json!({}), &oversized, true, None, None);
            for index in 0..(MAX_READ_LOG_ENTRIES + 16) {
                record_read("small", json!({ "index": index }), &json!(index), true, None, None);
            }
        })
        .await;

        assert_eq!(log.reads.len(), MAX_READ_LOG_ENTRIES);
        assert!(log.reads[0].raw_result.is_none());
        assert!(log.reads[1].raw_result.is_some());
        assert!(log.raw_result_bytes <= MAX_RAW_RESULT_BYTES);
    }

    #[tokio::test]
    async fn context_slot_is_retained_as_attestation_position() {
        let (_, log) = capture_reads(async {
            let response = json!({"context": {"slot": 9876}, "value": null});
            record_read("getAccountInfo", json!([]), &response, true, None, Some(9876));
        })
        .await;

        assert_eq!(log.slot, Some(9876));
        assert_eq!(log.reads[0].slot, Some(9876));
    }
}
