use crate::chain::Chain;
use crate::check::{CheckResult, Verdict};
use crate::pool::PoolInfo;
#[cfg(test)]
use crate::pool::TokenSide;
use crate::registry::{self, Entry};
use crate::state::AppState;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use chrono::{Duration, SecondsFormat, Utc};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::future::Future;
use std::io::{self, Write};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use thiserror::Error;

/// One observed JSON-RPC read used to produce an attestation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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
const MAX_READ_LOG_ENTRIES: usize = 256;
const MAX_RAW_RESULT_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct AttestationPayload {
    version: u16,
    chain: Chain,
    subject: String,
    verdict: Verdict,
    issuer: Option<String>,
    ticker: Option<String>,
    pool: PoolInfo,
    quote_share_of_supply: Option<f64>,
    registry_entry: Option<Entry>,
    registry_hash: String,
    reads: Vec<Read>,
    block: Option<u64>,
    slot: Option<u64>,
    checked_at: String,
    expires_at: String,
    signer: String,
    dev: bool,
}

impl Attestation {
    fn payload(&self) -> AttestationPayload {
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

#[derive(Debug, Clone, Default)]
pub struct ReadLog {
    pub reads: Vec<Read>,
    pub block: Option<u64>,
    pub slot: Option<u64>,
    raw_result_bytes: usize,
}

tokio::task_local! {
    static ACTIVE_READ_LOG: RefCell<ReadLog>;
}

/// Run a check with a task-local read log. Readers can record calls without
pub async fn capture_reads<F, T>(future: F) -> (T, ReadLog)
where
    F: Future<Output = T>,
{
    ACTIVE_READ_LOG
        .scope(RefCell::new(ReadLog::default()), async {
            let output = future.await;
            let captured =
                ACTIVE_READ_LOG.try_with(|value| value.borrow().clone()).unwrap_or_default();
            (output, captured)
        })
        .await
}
pub fn record_read(
    method: &str,
    params: Value,
    result: &Value,
    raw_result: bool,
    block: Option<u64>,
    slot: Option<u64>,
) {
    let _ = ACTIVE_READ_LOG.try_with(|cell| {
        let bytes = serde_json::to_vec(result).unwrap_or_default();
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        let result_hash = hex_lower(&hasher.finalize());
        let mut log = cell.borrow_mut();
        if method == "eth_blockNumber" {
            if log.block.is_none() {
                log.block = block;
            }
            if log.reads.len() < MAX_READ_LOG_ENTRIES {
                log.reads.push(Read {
                    method: method.to_owned(),
                    params,
                    result_hash,
                    raw_result: None,
                    block,
                    slot,
                });
            }
            return;
        }
        if method == "getSlot" {
            if log.slot.is_none() {
                log.slot = slot;
            }
            if log.reads.len() < MAX_READ_LOG_ENTRIES {
                log.reads.push(Read {
                    method: method.to_owned(),
                    params,
                    result_hash,
                    raw_result: None,
                    block,
                    slot,
                });
            }
            return;
        }
        if log.block.is_none() {
            log.block = block;
        }
        if log.slot.is_none() {
            log.slot = slot;
        }
        if log.reads.len() >= MAX_READ_LOG_ENTRIES {
            return;
        }
        let raw_result = if raw_result
            && log.raw_result_bytes.saturating_add(bytes.len()) <= MAX_RAW_RESULT_BYTES
        {
            log.raw_result_bytes = log.raw_result_bytes.saturating_add(bytes.len());
            Some(result.clone())
        } else {
            None
        };
        log.reads.push(Read {
            method: method.to_owned(),
            params,
            result_hash,
            raw_result,
            block,
            slot,
        });
    });
}

pub fn load_attestations(path: &Path) -> std::io::Result<HashMap<String, Attestation>> {
    let mut result = HashMap::new();
    if !path.exists() {
        return Ok(result);
    }
    let quarantine_dir = path.join("quarantine");
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let file_path = entry.path();
        if file_path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let bytes = match std::fs::read(&file_path) {
            Ok(bytes) => bytes,
            Err(_) => continue,
        };
        let quarantine_file = || -> io::Result<()> {
            std::fs::create_dir_all(&quarantine_dir)?;
            let file_name = file_path
                .file_name()
                .ok_or_else(|| io::Error::other("attestation filename missing"))?;
            std::fs::rename(&file_path, quarantine_dir.join(file_name))
        };
        if bytes.len() > MAX_ATTESTATION_BYTES {
            if quarantine_file().is_ok() {
                tracing::warn!(path = ?file_path, "quarantined oversized attestation");
            }
            continue;
        }
        match serde_json::from_slice::<Attestation>(&bytes) {
            Ok(attestation) => {
                let expected = file_path.file_stem().and_then(|value| value.to_str());
                if expected.is_some_and(|value| value.eq_ignore_ascii_case(&attestation.id)) {
                    result.insert(attestation.id.clone(), attestation);
                } else if quarantine_file().is_ok() {
                    tracing::warn!(path = ?file_path, "quarantined attestation with mismatched file name");
                }
            }
            Err(error) => {
                if quarantine_file().is_ok() {
                    tracing::warn!(path = ?file_path, %error, "quarantined malformed attestation");
                }
            }
        }
    }
    Ok(result)
}
fn scoped_value_matches(chain: &str, left: &str, right: &str) -> bool {
    if chain.eq_ignore_ascii_case("solana") {
        left == right
    } else {
        left.eq_ignore_ascii_case(right)
    }
}
#[cfg(feature = "s3")]
fn scoped_value_key(chain: Chain, value: &str) -> String {
    if chain == Chain::Solana { value.to_owned() } else { value.to_ascii_lowercase() }
}
#[cfg(feature = "s3")]
fn scoped_value_key_for_name(chain: &str, value: &str) -> String {
    if chain.eq_ignore_ascii_case("solana") { value.to_owned() } else { value.to_ascii_lowercase() }
}

#[async_trait]
pub trait AttestationStore: Send + Sync {
    async fn load(&self) -> io::Result<HashMap<String, Attestation>>;
    async fn load_recent(&self) -> io::Result<HashMap<String, Attestation>> {
        self.load().await
    }
    async fn latest_for_pool(&self, chain: &str, pool: &str) -> io::Result<Option<Attestation>> {
        let records = self.load_recent().await?;
        Ok(records
            .into_values()
            .filter(|attestation| {
                scoped_value_matches(chain, &attestation.chain.to_string(), chain)
                    && (scoped_value_matches(chain, &attestation.subject, pool)
                        || scoped_value_matches(chain, &attestation.pool.pool, pool))
            })
            .max_by(|left, right| left.checked_at.cmp(&right.checked_at)))
    }
    async fn persist(&self, attestation: &Attestation) -> io::Result<()>;
    async fn get(&self, _id: &str) -> io::Result<Option<Attestation>> {
        Ok(None)
    }
    async fn prune_expired(&self, _older_than: chrono::DateTime<Utc>) -> io::Result<()> {
        Ok(())
    }
    async fn quarantine(&self, _id: &str) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct FileAttestationStore {
    pub dir: PathBuf,
}

impl FileAttestationStore {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }
}

#[async_trait]
impl AttestationStore for FileAttestationStore {
    async fn load(&self) -> io::Result<HashMap<String, Attestation>> {
        load_attestations(&self.dir)
    }

    async fn persist(&self, attestation: &Attestation) -> io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let bytes = serde_json::to_vec_pretty(attestation)
            .map_err(|error| io::Error::other(error.to_string()))?;
        if bytes.len() > MAX_ATTESTATION_BYTES {
            return Err(io::Error::other("attestation exceeds the size limit"));
        }
        std::fs::write(self.dir.join(format!("{}.json", attestation.id)), bytes)
    }

    async fn get(&self, id: &str) -> io::Result<Option<Attestation>> {
        if id.len() != 64 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Ok(None);
        }
        let path = self.dir.join(format!("{id}.json"));
        match std::fs::read(path) {
            Ok(bytes) if bytes.len() <= MAX_ATTESTATION_BYTES => {
                Ok(serde_json::from_slice(&bytes).ok())
            }
            Ok(_) => Err(io::Error::other("attestation exceeds the size limit")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    async fn prune_expired(&self, older_than: chrono::DateTime<Utc>) -> io::Result<()> {
        if !self.dir.exists() {
            return Ok(());
        }
        for entry in std::fs::read_dir(&self.dir)? {
            let entry = entry?;
            if entry.path().extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let Ok(bytes) = std::fs::read(entry.path()) else { continue };
            if bytes.len() > MAX_ATTESTATION_BYTES {
                continue;
            }
            let Ok(attestation) = serde_json::from_slice::<Attestation>(&bytes) else { continue };
            let expired = chrono::DateTime::parse_from_rfc3339(&attestation.expires_at)
                .ok()
                .is_some_and(|expires| expires < older_than);
            if expired {
                let _ = std::fs::remove_file(entry.path());
            }
        }
        Ok(())
    }

    async fn quarantine(&self, id: &str) -> io::Result<()> {
        if id.len() != 64 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Ok(());
        }
        let source = self.dir.join(format!("{id}.json"));
        if !source.exists() {
            return Ok(());
        }
        let quarantine = self.dir.join("quarantine");
        std::fs::create_dir_all(&quarantine)?;
        std::fs::rename(source, quarantine.join(format!("{id}.json")))?;
        Ok(())
    }
}

#[cfg(feature = "s3")]
pub struct S3AttestationStore {
    client: aws_sdk_s3::Client,
    bucket: String,
    prefix: String,
}

#[cfg(feature = "s3")]
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct AttestationIndex {
    ids: Vec<String>,
    latest_by_pool: HashMap<String, String>,
}

#[cfg(feature = "s3")]
#[derive(Debug)]
struct IndexSnapshot {
    index: AttestationIndex,
    etag: Option<String>,
}

#[cfg(feature = "s3")]
impl S3AttestationStore {
    const MAX_INDEX_ENTRIES: usize = 10_000;

    pub async fn new(bucket: String) -> Self {
        let config = aws_config::defaults(aws_config::BehaviorVersion::latest()).load().await;
        Self {
            client: aws_sdk_s3::Client::new(&config),
            bucket,
            prefix: "attestations/".to_owned(),
        }
    }

    fn index_key(&self) -> String {
        format!("{}index/latest.json", self.prefix)
    }

    fn repair_prefix(&self) -> String {
        format!("{}repair/", self.prefix)
    }

    fn repair_key(&self, id: &str) -> String {
        format!("{}{id}.json", self.repair_prefix())
    }

    async fn read_object(&self, key: &str) -> io::Result<Option<(Vec<u8>, Option<String>)>> {
        let output = match self.client.get_object().bucket(&self.bucket).key(key).send().await {
            Ok(output) => output,
            Err(error)
                if error.as_service_error().is_some_and(|error| error.is_no_such_key())
                    || error
                        .raw_response()
                        .is_some_and(|response| response.status().as_u16() == 404) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(io::Error::other(error.to_string())),
        };
        if output.content_length().is_some_and(|length| length as usize > MAX_ATTESTATION_BYTES) {
            return Err(io::Error::other("attestation object exceeds the size limit"));
        }
        let etag = output.e_tag().map(str::to_owned);
        let bytes = output
            .body
            .collect()
            .await
            .map_err(|error| io::Error::other(error.to_string()))?
            .into_bytes()
            .to_vec();
        if bytes.len() > MAX_ATTESTATION_BYTES {
            return Err(io::Error::other("attestation object exceeds the size limit"));
        }
        Ok(Some((bytes, etag)))
    }

    fn validate_index(index: &AttestationIndex) -> io::Result<()> {
        if index.ids.len() > Self::MAX_INDEX_ENTRIES {
            return Err(io::Error::other("attestation index has too many ids"));
        }
        let ids = index.ids.iter().collect::<HashSet<_>>();
        if ids.len() != index.ids.len()
            || index
                .ids
                .iter()
                .any(|id| id.len() != 64 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()))
        {
            return Err(io::Error::other("attestation index contains malformed ids"));
        }
        if index.latest_by_pool.len() > Self::MAX_INDEX_ENTRIES {
            return Err(io::Error::other("attestation index has too many pool mappings"));
        }
        for (key, id) in &index.latest_by_pool {
            let Some((chain, subject)) = key.split_once(':') else {
                return Err(io::Error::other("attestation index contains a malformed pool key"));
            };
            let known_chain =
                matches!(chain, "solana" | "robinhood chain" | "base" | "ethereum" | "bnb chain");
            if !known_chain || subject.is_empty() {
                return Err(io::Error::other("attestation index contains an unknown chain"));
            }
            if id.len() != 64
                || !id.bytes().all(|byte| byte.is_ascii_hexdigit())
                || !ids.contains(id)
            {
                return Err(io::Error::other("attestation index contains a malformed mapping id"));
            }
        }
        Ok(())
    }
    fn fit_index_to_size(
        mut index: AttestationIndex,
        required: &HashSet<String>,
    ) -> io::Result<AttestationIndex> {
        index.ids.truncate(Self::MAX_INDEX_ENTRIES);
        if index.ids.is_empty() {
            return Err(io::Error::other("attestation index has no current id"));
        }

        let candidate = |keep: usize| -> io::Result<AttestationIndex> {
            let mut candidate = index.clone();
            candidate.ids.truncate(keep);
            let ids = candidate.ids.iter().collect::<HashSet<_>>();
            candidate.latest_by_pool.retain(|key, id| required.contains(key) || ids.contains(id));
            Ok(candidate)
        };
        let serialized_len = |candidate: &AttestationIndex| -> io::Result<usize> {
            serde_json::to_vec(candidate)
                .map(|bytes| bytes.len())
                .map_err(|error| io::Error::other(error.to_string()))
        };

        let minimum = candidate(1)?;
        if serialized_len(&minimum)? > MAX_ATTESTATION_BYTES {
            return Err(io::Error::other("attestation index exceeds the size limit"));
        }
        let mut best = minimum;
        let mut low = 2usize;
        let mut high = index.ids.len();
        while low <= high {
            let keep = low + (high - low) / 2;
            let candidate = candidate(keep)?;
            if serialized_len(&candidate)? <= MAX_ATTESTATION_BYTES {
                best = candidate;
                low = keep + 1;
            } else {
                high = keep.saturating_sub(1);
            }
        }
        Ok(best)
    }

    async fn read_index(&self) -> io::Result<IndexSnapshot> {
        let Some((bytes, etag)) = self.read_object(&self.index_key()).await? else {
            return Ok(IndexSnapshot { index: AttestationIndex::default(), etag: None });
        };
        let index: AttestationIndex = match serde_json::from_slice(&bytes) {
            Ok(index) => index,
            Err(error) => {
                tracing::error!(%error, "rejecting unreadable attestation index");
                return Err(io::Error::other(error.to_string()));
            }
        };
        if let Err(error) = Self::validate_index(&index) {
            tracing::error!(%error, "rejecting malformed attestation index");
            return Err(error);
        }
        Ok(IndexSnapshot { index, etag })
    }

    async fn read_repair_ids(&self) -> io::Result<Vec<String>> {
        let mut continuation = None;
        let mut ids = Vec::new();
        loop {
            let mut request =
                self.client.list_objects_v2().bucket(&self.bucket).prefix(self.repair_prefix());
            if let Some(token) = continuation.as_deref() {
                request = request.continuation_token(token);
            }
            let output =
                request.send().await.map_err(|error| io::Error::other(error.to_string()))?;
            for object in output.contents() {
                let Some(key) = object.key() else { continue };
                let Some(id) = key
                    .strip_prefix(&self.repair_prefix())
                    .and_then(|value| value.strip_suffix(".json"))
                else {
                    continue;
                };
                if id.len() == 64
                    && id.bytes().all(|byte| byte.is_ascii_hexdigit())
                    && !ids.iter().any(|known| known == id)
                {
                    ids.push(id.to_owned());
                    if ids.len() == Self::MAX_INDEX_ENTRIES {
                        return Ok(ids);
                    }
                }
            }
            continuation = output.next_continuation_token().map(str::to_owned);
            if continuation.is_none() {
                return Ok(ids);
            }
        }
    }

    async fn write_index(&self, index: &AttestationIndex, etag: Option<&str>) -> io::Result<()> {
        let bytes =
            serde_json::to_vec(index).map_err(|error| io::Error::other(error.to_string()))?;
        if bytes.len() > MAX_ATTESTATION_BYTES {
            return Err(io::Error::other("attestation index exceeds the size limit"));
        }
        let mut request = self
            .client
            .put_object()
            .bucket(&self.bucket)
            .key(self.index_key())
            .content_type("application/json")
            .body(aws_sdk_s3::primitives::ByteStream::from(bytes));
        request =
            if let Some(etag) = etag { request.if_match(etag) } else { request.if_none_match("*") };
        request.send().await.map_err(|error| io::Error::other(error.to_string()))?;
        Ok(())
    }

    async fn record_repair(&self, id: &str) -> io::Result<()> {
        let bytes = serde_json::to_vec(&serde_json::json!({ "id": id }))
            .map_err(|error| io::Error::other(error.to_string()))?;
        let result = self
            .client
            .put_object()
            .bucket(&self.bucket)
            .key(self.repair_key(id))
            .content_type("application/json")
            .if_none_match("*")
            .body(aws_sdk_s3::primitives::ByteStream::from(bytes))
            .send()
            .await;
        match result {
            Ok(_) => Ok(()),
            Err(error) => {
                let error = io::Error::other(error.to_string());
                if Self::is_conflict(&error) { Ok(()) } else { Err(error) }
            }
        }
    }

    async fn clear_repair(&self, id: &str) -> io::Result<()> {
        self.client
            .delete_object()
            .bucket(&self.bucket)
            .key(self.repair_key(id))
            .send()
            .await
            .map_err(|error| io::Error::other(error.to_string()))?;
        Ok(())
    }

    fn is_conflict(error: &io::Error) -> bool {
        let message = error.to_string();
        message.contains("PreconditionFailed")
            || message.contains("ConditionalRequestConflict")
            || message.contains("status code: 409")
            || message.contains("status code: 412")
            || message.contains("409 Conflict")
            || message.contains("412 Precondition")
    }
    async fn index_failure(&self, id: &str, index_error: io::Error) -> io::Error {
        match self.record_repair(id).await {
            Ok(()) => index_error,
            Err(marker_error) => {
                tracing::error!(%marker_error, %index_error, %id, "could not record attestation repair marker");
                io::Error::other(format!(
                    "attestation index update failed and repair marker failed: {index_error}; {marker_error}"
                ))
            }
        }
    }

    async fn update_index(&self, attestation: &Attestation) -> io::Result<()> {
        let subject_key = format!(
            "{}:{}",
            attestation.chain.to_string().to_ascii_lowercase(),
            scoped_value_key(attestation.chain, &attestation.subject)
        );
        let pool_key = format!(
            "{}:{}",
            attestation.chain.to_string().to_ascii_lowercase(),
            scoped_value_key(attestation.chain, &attestation.pool.pool)
        );
        let required = HashSet::from([subject_key.clone(), pool_key.clone()]);
        for _attempt in 0..5 {
            let snapshot = match self.read_index().await {
                Ok(snapshot) => snapshot,
                Err(error) => return Err(self.index_failure(&attestation.id, error).await),
            };
            let mut index = snapshot.index;
            index.ids.retain(|id| id != &attestation.id);
            index.ids.insert(0, attestation.id.clone());
            index.latest_by_pool.insert(subject_key.clone(), attestation.id.clone());
            index.latest_by_pool.insert(pool_key.clone(), attestation.id.clone());
            let index = match Self::fit_index_to_size(index, &required) {
                Ok(index) => index,
                Err(error) => return Err(self.index_failure(&attestation.id, error).await),
            };
            match self.write_index(&index, snapshot.etag.as_deref()).await {
                Ok(()) => {
                    if let Err(error) = self.clear_repair(&attestation.id).await {
                        tracing::warn!(%error, id = %attestation.id, "could not clear attestation repair marker");
                    }
                    return Ok(());
                }
                Err(error) if Self::is_conflict(&error) => continue,
                Err(error) => return Err(self.index_failure(&attestation.id, error).await),
            }
        }
        let error = io::Error::other("concurrent attestation index update conflict");
        Err(self.index_failure(&attestation.id, error).await)
    }

    async fn read_key(&self, key: &str) -> io::Result<Option<Attestation>> {
        let Some((bytes, _)) = self.read_object(key).await? else { return Ok(None) };
        Ok(serde_json::from_slice(&bytes).ok())
    }

    fn object_key(&self, attestation: &Attestation) -> String {
        format!("{}{}.json", self.prefix, attestation.id)
    }
}
#[cfg(feature = "s3")]
#[async_trait]
impl AttestationStore for S3AttestationStore {
    async fn load(&self) -> io::Result<HashMap<String, Attestation>> {
        self.load_recent().await
    }

    async fn load_recent(&self) -> io::Result<HashMap<String, Attestation>> {
        let repairs = self.read_repair_ids().await?;
        let snapshot = match self.read_index().await {
            Ok(snapshot) => snapshot,
            Err(error) if !repairs.is_empty() => {
                tracing::error!(%error, "hydrating repair markers without malformed attestation index");
                IndexSnapshot { index: AttestationIndex::default(), etag: None }
            }
            Err(error) => return Err(error),
        };
        let mut ids = repairs.clone();
        for id in snapshot.index.ids {
            if ids.len() == S3AttestationStore::MAX_INDEX_ENTRIES {
                break;
            }
            if !ids.iter().any(|known| known == &id) {
                ids.push(id);
            }
        }
        let mut result = HashMap::new();
        for id in ids {
            let Some(attestation) = self.get(&id).await? else { continue };
            result.insert(attestation.id.clone(), attestation);
        }
        for id in repairs {
            if let Some(attestation) = result.get(&id).cloned()
                && let Err(error) = self.update_index(&attestation).await
            {
                tracing::warn!(%error, %id, "could not merge attestation repair marker");
            }
        }
        Ok(result)
    }

    async fn latest_for_pool(&self, chain: &str, pool: &str) -> io::Result<Option<Attestation>> {
        let snapshot = self.read_index().await?;
        let key =
            format!("{}:{}", chain.to_ascii_lowercase(), scoped_value_key_for_name(chain, pool));
        let Some(id) = snapshot.index.latest_by_pool.get(&key) else { return Ok(None) };
        let Some(attestation) = self.get(id).await? else { return Ok(None) };
        let matches = attestation.id.eq_ignore_ascii_case(id)
            && scoped_value_matches(chain, &attestation.chain.to_string(), chain)
            && (scoped_value_matches(chain, &attestation.subject, pool)
                || scoped_value_matches(chain, &attestation.pool.pool, pool));
        if !matches {
            tracing::warn!(%id, %chain, %pool, "indexed attestation did not match requested pool");
            return Ok(None);
        }
        Ok(Some(attestation))
    }

    async fn persist(&self, attestation: &Attestation) -> io::Result<()> {
        let bytes = serde_json::to_vec_pretty(attestation)
            .map_err(|error| io::Error::other(error.to_string()))?;
        if bytes.len() > MAX_ATTESTATION_BYTES {
            return Err(io::Error::other("attestation exceeds the size limit"));
        }
        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(self.object_key(attestation))
            .content_type("application/json")
            .body(aws_sdk_s3::primitives::ByteStream::from(bytes))
            .send()
            .await
            .map_err(|error| io::Error::other(error.to_string()))?;
        self.update_index(attestation).await
    }

    async fn get(&self, id: &str) -> io::Result<Option<Attestation>> {
        if id.len() != 64 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Ok(None);
        }
        self.read_key(&format!("{}{id}.json", self.prefix)).await
    }

    async fn prune_expired(&self, older_than: chrono::DateTime<Utc>) -> io::Result<()> {
        let mut continuation = None;
        loop {
            let mut request =
                self.client.list_objects_v2().bucket(&self.bucket).prefix(&self.prefix);
            if let Some(token) = continuation.as_deref() {
                request = request.continuation_token(token);
            }
            let output =
                request.send().await.map_err(|error| io::Error::other(error.to_string()))?;
            for object in output.contents() {
                let Some(key) = object.key() else { continue };
                if key.starts_with(&format!("{}quarantine/", self.prefix))
                    || key.starts_with(&format!("{}repair/", self.prefix))
                    || key == self.index_key()
                {
                    continue;
                }
                let Some(attestation) = self.read_key(key).await? else { continue };
                let expired = chrono::DateTime::parse_from_rfc3339(&attestation.expires_at)
                    .ok()
                    .is_some_and(|expires| expires < older_than);
                if expired {
                    self.client
                        .delete_object()
                        .bucket(&self.bucket)
                        .key(key)
                        .send()
                        .await
                        .map_err(|error| io::Error::other(error.to_string()))?;
                }
            }
            continuation = output.next_continuation_token().map(str::to_owned);
            if continuation.is_none() {
                break;
            }
        }
        Ok(())
    }

    async fn quarantine(&self, id: &str) -> io::Result<()> {
        if id.len() != 64 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Ok(());
        }
        let source_key = format!("{}{id}.json", self.prefix);
        let quarantine_key = format!("{}quarantine/{id}.json", self.prefix);
        self.client
            .copy_object()
            .bucket(&self.bucket)
            .key(&quarantine_key)
            .copy_source(format!("{}/{}", self.bucket, source_key))
            .send()
            .await
            .map_err(|error| io::Error::other(error.to_string()))?;
        self.client
            .delete_object()
            .bucket(&self.bucket)
            .key(source_key)
            .send()
            .await
            .map_err(|error| io::Error::other(error.to_string()))?;
        Ok(())
    }
}

pub async fn get_async(state: &AppState, id: &str) -> Option<Attestation> {
    if let Some(attestation) = get(state, id) {
        if valid_for_state(state, id, &attestation) {
            return Some(attestation);
        }
        quarantine_if_invalid(state, id, &attestation).await;
        return None;
    }
    let attestation = state.attest_store.get(id).await.ok().flatten()?;
    if !valid_for_state(state, id, &attestation) {
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
pub async fn get_certificate_async(state: &AppState, id: &str) -> Option<Attestation> {
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
    state: &AppState,
    id: &str,
    attestation: &Attestation,
) -> bool {
    id.eq_ignore_ascii_case(&attestation.id)
        && verify(attestation).is_ok()
        && signer_trusted(state, attestation)
        && attestation.dev == state.dev_signer
}
async fn quarantine_if_invalid(state: &AppState, id: &str, attestation: &Attestation) {
    if certificate_valid_for_state(state, id, attestation) {
        return;
    }
    tracing::warn!(%id, "quarantining cryptographically invalid attestation");
    if let Err(error) = state.attest_store.quarantine(id).await {
        tracing::warn!(%error, %id, "could not quarantine invalid attestation");
    }
}

fn cache_index(state: &AppState, attestation: &Attestation) {
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
fn read_signing_key_file(path: &Path) -> Result<SigningKey, String> {
    let encoded = fs::read_to_string(path)
        .map_err(|error| format!("reading development signing key {}: {error}", path.display()))?;
    let bytes = BASE64
        .decode(encoded.trim())
        .map_err(|error| format!("development signing key is not base64: {error}"))?;
    let seed: [u8; 32] = bytes
        .try_into()
        .map_err(|_| "development signing key must decode to exactly 32 bytes".to_owned())?;
    restrict_signing_key_file(path)?;
    Ok(SigningKey::from_bytes(&seed))
}

fn restrict_signing_key_file(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        let mut permissions = fs::metadata(path)
            .map_err(|error| format!("reading development signing key permissions: {error}"))?
            .permissions();
        permissions.set_mode(0o600);
        fs::set_permissions(path, permissions)
            .map_err(|error| format!("setting development signing key permissions: {error}"))?;
    }
    Ok(())
}

fn generate_signing_key_file(path: &Path) -> Result<SigningKey, String> {
    let mut rng = rand::rngs::OsRng;
    let signing_key = SigningKey::generate(&mut rng);
    let encoded = BASE64.encode(signing_key.to_bytes());
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            return read_signing_key_file(path);
        }
        Err(error) => {
            return Err(format!("creating development signing key {}: {error}", path.display()));
        }
    };
    file.write_all(encoded.as_bytes())
        .map_err(|error| format!("writing development signing key {}: {error}", path.display()))?;
    file.sync_all()
        .map_err(|error| format!("syncing development signing key {}: {error}", path.display()))?;
    Ok(signing_key)
}

fn development_signing_key(data_dir: &Path) -> Result<SigningKey, String> {
    let path = data_dir.join("dev-signing-key");
    if path.exists() {
        return read_signing_key_file(&path);
    }
    generate_signing_key_file(&path)
}

pub fn signing_key_from_env(data_dir: &Path) -> Result<(SigningKey, bool), String> {
    if let Ok(encoded) = std::env::var("QED_SIGNING_KEY") {
        let bytes = BASE64
            .decode(encoded)
            .map_err(|error| format!("QED_SIGNING_KEY is not base64: {error}"))?;
        let seed: [u8; 32] = bytes
            .try_into()
            .map_err(|_| "QED_SIGNING_KEY must decode to exactly 32 bytes".to_owned())?;
        return Ok((SigningKey::from_bytes(&seed), false));
    }
    if std::env::var("QED_ENV").is_ok_and(|value| value.eq_ignore_ascii_case("production")) {
        return Err("QED_SIGNING_KEY is required when QED_ENV=production".to_owned());
    }
    Ok((development_signing_key(data_dir)?, true))
}

pub fn get(state: &AppState, id: &str) -> Option<Attestation> {
    state.attestations.read().ok()?.get(id).cloned()
}

pub async fn latest_for_pool_async(
    state: &AppState,
    chain: Chain,
    pool: &str,
) -> Option<Attestation> {
    let attestation =
        state.attest_store.latest_for_pool(&chain.to_string(), pool).await.ok().flatten()?;
    if !valid_for_state(state, &attestation.id, &attestation) {
        quarantine_if_invalid(state, &attestation.id, &attestation).await;
        return None;
    }
    cache_index(state, &attestation);
    Some(attestation)
}

pub fn public_key_b58(state: &AppState) -> String {
    bs58::encode(state.signing_key.verifying_key().as_bytes()).into_string()
}

pub fn verify(attestation: &Attestation) -> Result<(), VerifyError> {
    let payload = canonical_json(&attestation.payload())?;
    let mut hasher = Sha256::new();
    hasher.update(&payload);
    let digest = hasher.finalize();

    let expected = hex_lower(&digest);
    if attestation.id.len() != 64 || !attestation.id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(VerifyError::Id);
    }
    if !attestation.id.eq_ignore_ascii_case(&expected) {
        return Err(VerifyError::IdMismatch);
    }
    let signer =
        bs58::decode(&attestation.signer).into_vec().map_err(|_| VerifyError::SignerEncoding)?;
    let signer: [u8; 32] = signer.try_into().map_err(|_| VerifyError::SignerKey)?;
    let signer = VerifyingKey::from_bytes(&signer).map_err(|_| VerifyError::SignerKey)?;
    let signature =
        BASE64.decode(&attestation.signature).map_err(|_| VerifyError::SignatureEncoding)?;
    let signature = Signature::from_slice(&signature).map_err(|_| VerifyError::SignatureLength)?;
    signer.verify(&payload, &signature).map_err(|_| VerifyError::SignatureInvalid)
}

#[cfg(test)]
pub fn trusted_signer(attestation: &Attestation, expected: &str) -> bool {
    attestation.signer == expected
}

pub fn trusted_signers(state: &AppState) -> HashSet<String> {
    let mut signers = HashSet::from([public_key_b58(state)]);
    if let Ok(previous) = std::env::var("QED_PREVIOUS_KEYS") {
        signers.extend(
            previous.split(',').map(str::trim).filter(|key| !key.is_empty()).map(str::to_owned),
        );
    }
    signers
}

pub fn signer_trusted(state: &AppState, attestation: &Attestation) -> bool {
    trusted_signers(state).contains(&attestation.signer)
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

fn current_registry_entry(state: &AppState, attestation: &Attestation) -> bool {
    let Some(recorded) = attestation.registry_entry.as_ref() else {
        return !matches!(attestation.verdict, Verdict::Verified { .. });
    };
    let Ok(registry) = state.registry.try_read() else {
        return false;
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

pub(crate) fn valid_for_state(state: &AppState, id: &str, attestation: &Attestation) -> bool {
    id.eq_ignore_ascii_case(&attestation.id)
        && verify(attestation).is_ok()
        && signer_trusted(state, attestation)
        && attestation.dev == state.dev_signer
        && current_registry_entry(state, attestation)
        && fresh(attestation)
}

pub async fn recheck(state: &AppState, id: &str) -> RecheckResult {
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
    state.check_cache.invalidate(&crate::check::cache_key(&previous.subject)).await;
    let fresh = crate::check::check(state, &previous.subject).await;
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
    state: &AppState,
    result: &CheckResult,
    read_log: ReadLog,
) -> Option<Attestation> {
    let pool = result.pool.clone()?;
    if matches!(result.verdict, Verdict::Unknown { .. }) {
        return None;
    }
    let registry = state.registry.read().await.clone();
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
    if let Some(previous) = latest_for_pool_async(state, result.chain, &pool.pool).await {
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
    let expires_at = (Utc::now() + Duration::hours(24)).to_rfc3339_opts(SecondsFormat::Secs, true);
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
#[allow(clippy::too_many_arguments)]
fn sign_payload(
    state: &AppState,
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
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let id = hex_lower(&hasher.finalize());
    let signature = BASE64.encode(state.signing_key.sign(&bytes).to_bytes());
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

async fn persist(state: &AppState, attestation: &Attestation) -> bool {
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

pub fn canonical_payload_json(attestation: &Attestation) -> Result<String, serde_json::Error> {
    let bytes = canonical_json(&attestation.payload())?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn canonical_json<T: Serialize>(value: &T) -> Result<Vec<u8>, serde_json::Error> {
    let value = serde_json::to_value(value)?;
    serde_json::to_vec(&canonical_value(value))
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

fn hex_lower(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
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
        signature: BASE64.encode(signing_key.sign(&bytes).to_bytes()),
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
    attestation.signature = BASE64.encode(signing_key.sign(&bytes).to_bytes());
    attestation
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain::Chain;
    use crate::pool::TokenSide;
    use serde_json::json;
    use std::sync::Mutex;

    #[test]
    fn development_signing_key_reuses_persisted_seed() {
        let directory = tempfile::tempdir().expect("temporary key directory");
        let first = development_signing_key(directory.path()).expect("generate development key");
        let path = directory.path().join("dev-signing-key");
        let encoded = fs::read_to_string(&path).expect("read persisted development key");
        let decoded = BASE64.decode(encoded.trim()).expect("decode persisted development key");
        assert_eq!(decoded.len(), 32);
        let second = development_signing_key(directory.path()).expect("reuse development key");
        assert_eq!(first.to_bytes(), second.to_bytes());
        #[cfg(unix)]
        assert_eq!(fs::metadata(path).expect("key metadata").permissions().mode() & 0o777, 0o600);
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
        let signature = BASE64.encode(signing_key.sign(&bytes).to_bytes());
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
        attestation.signature = BASE64.encode(signing_key.sign(&bytes).to_bytes());
        attestation
    }

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn test_state(dev_signer: bool) -> AppState {
        AppState {
            registry: std::sync::Arc::new(tokio::sync::RwLock::new(Vec::new())),
            registry_status: std::sync::Arc::new(tokio::sync::RwLock::new(
                crate::discovery::RegistrySnapshot::default(),
            )),
            readers: std::sync::Arc::new(Vec::new()),
            http: reqwest::Client::new(),
            source_http: reqwest::Client::new(),
            check_cache: moka::future::Cache::builder().build(),
            powers_cache: moka::future::Cache::builder().build(),
            powers_retry_cache: moka::future::Cache::builder().build(),
            powers_failure_cache: moka::future::Cache::builder().build(),
            powers_locks: moka::future::Cache::builder().build(),
            powers_prefetching: std::sync::Arc::new(tokio::sync::Mutex::new(
                std::collections::HashSet::new(),
            )),
            leaderboard_check_cache: moka::future::Cache::builder().build(),
            check_inflight: std::sync::Arc::new(tokio::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            featured: std::sync::Arc::new(tokio::sync::RwLock::new(Vec::new())),
            featured_status: std::sync::Arc::new(tokio::sync::RwLock::new(
                crate::discovery::FeaturedStatus::default(),
            )),
            leaderboard: std::sync::Arc::new(tokio::sync::RwLock::new(
                crate::discovery::Leaderboard::default(),
            )),
            prices: std::sync::Arc::new(tokio::sync::RwLock::new(
                crate::discovery::PriceSnapshot::default(),
            )),
            attestations: std::sync::Arc::new(std::sync::RwLock::new(HashMap::new())),
            signing_key: std::sync::Arc::new(SigningKey::from_bytes(&[7; 32])),
            dev_signer,
            attest_store: std::sync::Arc::new(FileAttestationStore::new(std::path::PathBuf::from(
                "target/test-attestations",
            ))),
            board_store: std::sync::Arc::new(crate::discovery::DurableBoardStore::default()),
            registry_hash: std::sync::Arc::new(std::sync::RwLock::new(String::new())),
            registry_api_cache: std::sync::Arc::new(tokio::sync::RwLock::new(None)),
            public_url: std::sync::Arc::new("http://localhost:3000".to_owned()),
            admin_auth: std::sync::Arc::new(crate::state::AdminAuth::new(
                Some("test-admin"),
                Some("test-password"),
            )),
            usage_stats: std::sync::Arc::new(crate::state::UsageStats::new()),
            rate_limiter: std::sync::Arc::new(crate::state::RateLimiter::default()),
            expensive_concurrency: std::sync::Arc::new(tokio::sync::Semaphore::new(32)),
            powers_prefetch_concurrency: std::sync::Arc::new(tokio::sync::Semaphore::new(2)),
            wallet_concurrency: std::sync::Arc::new(tokio::sync::Semaphore::new(1)),
            registry_api_concurrency: std::sync::Arc::new(tokio::sync::Semaphore::new(4)),
        }
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
            record_read(
                "getAccountInfo",
                json!(["mint"]),
                &account_result,
                false,
                None,
                Some(456),
            );
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

    fn signed_verified(state: &AppState, entry: Entry) -> Attestation {
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

    #[test]
    fn live_attestation_rejects_revoked_or_stale_entry_but_certificate_keeps_history() {
        let state = test_state(false);
        let entry = verified_registry_entry();
        state.registry.try_write().unwrap().push(entry.clone());
        let attestation = signed_verified(&state, entry);

        assert!(valid_for_state(&state, &attestation.id, &attestation));

        state.registry.try_write().unwrap()[0].removed_at = Some("2026-09-23T00:00:00Z".to_owned());
        assert!(!valid_for_state(&state, &attestation.id, &attestation));
        assert!(certificate_valid_for_state(&state, &attestation.id, &attestation));

        let mut registry = state.registry.try_write().unwrap();
        registry[0].removed_at = None;
        registry[0].stale_since = Some("2026-09-23T00:00:00Z".to_owned());
        drop(registry);
        assert!(!valid_for_state(&state, &attestation.id, &attestation));
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

        assert!(create_for_check(&state, &result, ReadLog::default()).await.is_none());
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
            powers: Some(crate::powers::PowersRecord {
                chain: Chain::Base,
                contract: "0x0000000000000000000000000000000000000002".to_owned(),
                can_seize: Vec::new(),
                can_block: Vec::new(),
                can_change_rules: Vec::new(),
                unavailable: Vec::new(),
                source_verified_subject: crate::powers::SourceVerifiedSubject::Contract,
                source_verified: crate::powers::SourceVerified::None,
                source_verified_proxy: None,
                observed_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
                block: None,
                slot: None,
                reads: Vec::new(),
            }),
        };

        let attestation = create_for_check(&state, &result, ReadLog::default())
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
    #[test]
    fn expired_attestation_remains_valid_as_historical_certificate() {
        let state = test_state(false);
        let attestation = expired_signed_test_attestation([7; 32], false);

        assert!(!valid_for_state(&state, &attestation.id, &attestation));
        assert!(certificate_valid_for_state(&state, &attestation.id, &attestation));
    }

    #[test]
    fn foreign_signer_is_not_trusted() {
        let attestation = sample();
        let foreign = bs58::encode([8u8; 32]).into_string();
        assert!(!trusted_signer(&attestation, &foreign));
    }

    #[test]
    fn state_trust_policy_accepts_current_and_previous_keys() {
        let _guard = ENV_LOCK.lock().unwrap();
        let state = test_state(false);
        let current = signed_sample([7; 32], false);
        assert!(signer_trusted(&state, &current));
        assert!(valid_for_state(&state, &current.id, &current));
        let previous_key =
            bs58::encode(SigningKey::from_bytes(&[8; 32]).verifying_key().as_bytes()).into_string();
        let previous = std::env::var_os("QED_PREVIOUS_KEYS");
        unsafe { std::env::set_var("QED_PREVIOUS_KEYS", &previous_key) };
        let rotated = signed_sample([8; 32], false);
        assert!(signer_trusted(&state, &rotated));
        assert!(valid_for_state(&state, &rotated.id, &rotated));
        unsafe {
            if let Some(previous) = previous {
                std::env::set_var("QED_PREVIOUS_KEYS", previous);
            } else {
                std::env::remove_var("QED_PREVIOUS_KEYS");
            }
        }
    }

    #[test]
    fn state_trust_policy_rejects_foreign_and_environment_mismatch() {
        let _guard = ENV_LOCK.lock().unwrap();
        let previous = std::env::var_os("QED_PREVIOUS_KEYS");
        unsafe { std::env::remove_var("QED_PREVIOUS_KEYS") };
        let production = test_state(false);
        let foreign = signed_sample([9; 32], false);
        assert!(!valid_for_state(&production, &foreign.id, &foreign));
        let development_attestation = signed_sample([7; 32], true);
        assert!(!valid_for_state(
            &production,
            &development_attestation.id,
            &development_attestation
        ));

        let development = test_state(true);
        let production_attestation = signed_sample([7; 32], false);
        assert!(!valid_for_state(
            &development,
            &production_attestation.id,
            &production_attestation
        ));
        unsafe {
            if let Some(previous) = previous {
                std::env::set_var("QED_PREVIOUS_KEYS", previous);
            } else {
                std::env::remove_var("QED_PREVIOUS_KEYS");
            }
        }
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
    #[cfg(feature = "s3")]
    #[test]
    fn index_size_trim_keeps_newest_id_and_required_subject_mapping() {
        let mut index = AttestationIndex::default();
        for number in 0..S3AttestationStore::MAX_INDEX_ENTRIES {
            let id = format!("{number:064x}");
            index.ids.push(id.clone());
            index.latest_by_pool.insert(format!("base:subject-{number:05}"), id);
        }
        let newest = "f".repeat(64);
        let required_key = "base:current-subject".to_owned();
        index.ids.insert(0, newest.clone());
        index.latest_by_pool.insert(required_key.clone(), newest.clone());
        let required = HashSet::from([required_key.clone()]);

        let trimmed = S3AttestationStore::fit_index_to_size(index, &required).unwrap();
        let serialized = serde_json::to_vec(&trimmed).unwrap();
        assert!(serialized.len() <= MAX_ATTESTATION_BYTES);
        assert_eq!(trimmed.ids.first(), Some(&newest));
        assert_eq!(trimmed.latest_by_pool.get(&required_key), Some(&newest));
        let retained = trimmed.ids.iter().collect::<HashSet<_>>();
        assert!(trimmed.latest_by_pool.values().all(|id| retained.contains(id)));
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
    #[tokio::test]
    async fn file_store_round_trip_is_offline() {
        let directory = tempfile::tempdir().unwrap();
        let store = FileAttestationStore::new(directory.path().join("attestations"));
        let expected = sample();
        store.persist(&expected).await.unwrap();
        let loaded = store.load().await.unwrap();
        assert_eq!(loaded.get(&expected.id), Some(&expected));
    }
}
