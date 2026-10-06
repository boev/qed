#[cfg(feature = "s3")]
use crate::domain::chain::Chain;
use crate::{
    domain::attestation::{Attestation, MAX_ATTESTATION_BYTES},
    ports::{AttestationStore, Signer},
};
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use chrono::Utc;
use ed25519_dalek::SigningKey;
#[cfg(feature = "s3")]
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
#[cfg(feature = "s3")]
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

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

pub(crate) fn development_signing_key(data_dir: &Path) -> Result<SigningKey, String> {
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

#[derive(Clone)]
pub(crate) struct Ed25519Signer(Arc<SigningKey>);

impl Ed25519Signer {
    pub(crate) fn new(key: Arc<SigningKey>) -> Self {
        Self(key)
    }
}

impl Signer for Ed25519Signer {
    fn public_key(&self) -> String {
        bs58::encode(self.0.verifying_key().as_bytes()).into_string()
    }

    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, String> {
        Ok(ed25519_dalek::Signer::sign(self.0.as_ref(), message).to_bytes().to_vec())
    }
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
    async fn load_recent(&self) -> io::Result<HashMap<String, Attestation>> {
        load_attestations(&self.dir)
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
#[cfg(test)]
mod tests {
    use super::*;

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

    #[tokio::test]
    async fn file_store_round_trip_is_offline() {
        let directory = tempfile::tempdir().expect("temporary attestation directory");
        let store = FileAttestationStore::new(directory.path().join("attestations"));
        let expected: Attestation = serde_json::from_str(include_str!(
            "../../tests/fixtures/attest/fc6147d4cd42374b72246cc6340d23e26f5c411e63f613322a18413c3da243bb.json"
        ))
        .expect("golden attestation fixture");

        store.persist(&expected).await.expect("persist attestation");
        let loaded = store.load_recent().await.expect("load persisted attestation");

        assert_eq!(loaded.get(&expected.id), Some(&expected));
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
}
