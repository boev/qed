use super::database;
use crate::{
    domain::attestation::{self, Attestation},
    ports::AttestationStore,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use deadpool_postgres::Pool;
#[cfg(feature = "s3")]
use futures_util::{StreamExt, TryStreamExt};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    io,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{io::AsyncWriteExt, sync::Mutex};

const MAX_OBJECT_BYTES: usize = 32 * 1024 * 1024;
const MAX_OUTBOX_BYTES: u64 = 512 * 1024 * 1024;
const MAX_OUTBOX_ENTRIES: usize = 50_000;
pub(crate) const RUNTIME_FILES: &[&str] =
    &["registry.json", "ondo-public-catalog.json", "ondo-public-attempt.json"];
fn error<T>(_: T) -> io::Error {
    io::Error::other("durable storage unavailable or incompatible")
}
fn digest(bytes: &[u8]) -> String {
    attestation::hex_lower(&Sha256::digest(bytes))
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct Metadata {
    key: String,
    kind: String,
    id: Option<String>,
    pool: Option<String>,
    subject: Option<String>,
    chain: Option<String>,
    checked_at: Option<DateTime<Utc>>,
    expires_at: Option<DateTime<Utc>>,
    quarantined: bool,
    deleted: bool,
}
struct Record {
    meta: Metadata,
    bytes: Vec<u8>,
}
#[derive(Default)]
struct Outbox {
    pending: HashMap<String, Arc<Record>>,
    bytes: u64,
}
pub(crate) struct DurableStore {
    pool: Pool,
    outbox_dir: PathBuf,
    outbox: Mutex<Outbox>,
    cache: moka::future::Cache<String, Arc<Record>>,
    migrated: Mutex<bool>,
    degraded: AtomicBool,
    read_degraded: AtomicBool,
    capture_gap: AtomicBool,
    threshold: usize,
    #[cfg(feature = "s3")]
    s3: Option<(aws_sdk_s3::Client, String)>,
}
impl DurableStore {
    pub async fn open(
        url: &str,
        data_dir: &Path,
        bucket: Option<String>,
        threshold: usize,
    ) -> Result<Arc<Self>, String> {
        let pool = database::pool(url)?;
        #[cfg(feature = "s3")]
        let s3 = if let Some(bucket) = bucket {
            let config = aws_config::defaults(aws_config::BehaviorVersion::latest()).load().await;
            Some((aws_sdk_s3::Client::new(&config), bucket))
        } else {
            None
        };
        #[cfg(not(feature = "s3"))]
        if bucket.is_some() {
            return Err("QED_ATTEST_BUCKET requires the s3 feature".into());
        }
        let outbox_dir = data_dir.join("storage-outbox");
        tokio::fs::create_dir_all(&outbox_dir).await.map_err(database::unavailable)?;
        let mut outbox = Outbox::default();
        let mut capture_gap = data_dir.join("storage-capture-gap").exists();
        let mut entries = tokio::fs::read_dir(&outbox_dir).await.map_err(database::unavailable)?;
        while let Some(entry) = entries.next_entry().await.map_err(database::unavailable)? {
            if entry.path().extension().is_none_or(|ext| ext != "pending") {
                continue;
            }
            let parsed = async {
                let bytes = tokio::fs::read(entry.path()).await?;
                let split =
                    bytes.iter().position(|byte| *byte == b'\n').ok_or_else(|| error(()))?;
                let meta: Metadata = serde_json::from_slice(&bytes[..split]).map_err(error)?;
                if entry.file_name().to_str()
                    != Some(&format!("{}.pending", digest(meta.key.as_bytes())))
                    || bytes.len() > MAX_OBJECT_BYTES + 4096
                {
                    return Err(error(()));
                }
                Ok::<_, io::Error>(Record { meta, bytes: bytes[split + 1..].to_vec() })
            }
            .await;
            match parsed {
                Ok(record)
                    if outbox.pending.len() < MAX_OUTBOX_ENTRIES
                        && outbox.bytes + record.bytes.len() as u64 <= MAX_OUTBOX_BYTES =>
                {
                    outbox.bytes += record.bytes.len() as u64;
                    outbox.pending.insert(record.meta.key.clone(), Arc::new(record));
                }
                _ => capture_gap = true,
            }
        }
        let cache = moka::future::Cache::builder()
            .max_capacity(64 * 1024 * 1024)
            .weigher(|_: &String, record: &Arc<Record>| {
                u32::try_from(record.bytes.len() + 256).unwrap_or(u32::MAX)
            })
            .build();
        for (key, record) in &outbox.pending {
            cache.insert(key.clone(), Arc::clone(record)).await;
        }
        let store = Arc::new(Self {
            pool,
            outbox_dir,
            outbox: Mutex::new(outbox),
            cache,
            migrated: Mutex::new(false),
            degraded: AtomicBool::new(true),
            capture_gap: AtomicBool::new(capture_gap),
            threshold,
            read_degraded: AtomicBool::new(false),
            #[cfg(feature = "s3")]
            s3,
        });
        if capture_gap {
            store.gap().await;
        }
        let _ = store.flush().await;
        Ok(store)
    }
    pub fn status(&self) -> serde_json::Value {
        serde_json::json!({"primary": "postgres", "degraded": self.degraded.load(Ordering::Relaxed) || self.read_degraded.load(Ordering::Relaxed) || self.capture_gap.load(Ordering::Relaxed), "capture_gap": self.capture_gap.load(Ordering::Relaxed)})
    }
    pub fn available(&self) -> bool {
        !self.degraded.load(Ordering::Relaxed) && !self.read_degraded.load(Ordering::Relaxed)
    }
    fn working_snapshot_path(&self, key: &str) -> Option<PathBuf> {
        let name = match key {
            "discovery/leaderboard.json" => "leaderboard.json",
            "discovery/featured.json" => "featured.json",
            _ => return None,
        };
        Some(self.outbox_dir.parent()?.join(name))
    }
    pub async fn working_snapshot(&self, key: &str) -> Option<Vec<u8>> {
        let path = self.working_snapshot_path(key)?;
        if tokio::fs::metadata(&path).await.ok()?.len() > MAX_OBJECT_BYTES as u64 {
            return None;
        }
        tokio::fs::read(path).await.ok()
    }
    async fn gap(&self) {
        self.capture_gap.store(true, Ordering::Relaxed);
        self.degraded.store(true, Ordering::Relaxed);
        if let Some(parent) = self.outbox_dir.parent() {
            let _ =
                tokio::fs::write(parent.join("storage-capture-gap"), b"durable capture failed\n")
                    .await;
        }
    }
    async fn client(&self) -> io::Result<deadpool_postgres::Client> {
        let mut client = self.pool.get().await.map_err(error)?;
        let mut migrated = self.migrated.lock().await;
        if !*migrated {
            super::database::migrate(&mut client).await.map_err(error)?;
            *migrated = true;
        }
        Ok(client)
    }
    async fn write_record(&self, record: &Record, import: bool) -> io::Result<()> {
        let meta = &record.meta;
        let hash = digest(&record.bytes);
        let s3_key = if !meta.deleted && record.bytes.len() > self.threshold {
            Some(self.write_blob(record, &hash).await?)
        } else {
            None
        };
        let payload = s3_key.is_none().then_some(record.bytes.as_slice());
        let client = self.client().await?;
        let conflict = if import {
            "DO NOTHING"
        } else {
            "DO UPDATE SET id=EXCLUDED.id,kind=EXCLUDED.kind,pool=EXCLUDED.pool,subject=EXCLUDED.subject,chain=EXCLUDED.chain,checked_at=EXCLUDED.checked_at,expires_at=EXCLUDED.expires_at,payload=EXCLUDED.payload,s3_key=EXCLUDED.s3_key,sha256=EXCLUDED.sha256,quarantined=EXCLUDED.quarantined,deleted=EXCLUDED.deleted,updated_at=now()"
        };
        client.execute(&format!("INSERT INTO durable_objects(key,id,kind,pool,subject,chain,checked_at,expires_at,payload,s3_key,sha256,quarantined,deleted) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13) ON CONFLICT(key) {conflict}"),
            &[&meta.key,&meta.id,&meta.kind,&meta.pool,&meta.subject,&meta.chain,&meta.checked_at,&meta.expires_at,&payload,&s3_key,&hash,&meta.quarantined,&meta.deleted]).await.map_err(error)?;
        Ok(())
    }
    async fn write_blob(&self, record: &Record, hash: &str) -> io::Result<String> {
        #[cfg(feature = "s3")]
        if let Some((client, bucket)) = &self.s3 {
            let prefix = if record.meta.key.starts_with("attestations/") {
                "attestations"
            } else {
                "discovery"
            };
            let key = format!("{prefix}/blobs/{hash}.json");
            client
                .put_object()
                .bucket(bucket)
                .key(&key)
                .body(aws_sdk_s3::primitives::ByteStream::from(record.bytes.clone()))
                .send()
                .await
                .map_err(error)?;
            return Ok(key);
        }
        let _ = (record, hash);
        Err(io::Error::other(
            "oversized durable payload requires QED_ATTEST_BUCKET and the s3 feature",
        ))
    }
    async fn queue(&self, record: Record) -> io::Result<()> {
        if record.bytes.len() > MAX_OBJECT_BYTES {
            self.gap().await;
            return Err(error(()));
        }
        let key = record.meta.key.clone();
        let record = Arc::new(record);
        let mut outbox = self.outbox.lock().await;
        let old_size = outbox.pending.get(&key).map_or(0, |old| old.bytes.len() as u64);
        let next_bytes = outbox.bytes - old_size + record.bytes.len() as u64;
        if next_bytes > MAX_OUTBOX_BYTES
            || (!outbox.pending.contains_key(&key) && outbox.pending.len() >= MAX_OUTBOX_ENTRIES)
        {
            self.gap().await;
            return Err(error(()));
        }
        let path = self.outbox_dir.join(format!("{}.pending", digest(key.as_bytes())));
        let temp = path.with_extension("tmp");
        let captured = async {
            let mut file = tokio::fs::File::create(&temp).await?;
            let mut header = serde_json::to_vec(&record.meta).map_err(error)?;
            header.push(b'\n');
            file.write_all(&header).await?;
            file.write_all(&record.bytes).await?;
            file.sync_all().await?;
            tokio::fs::rename(&temp, &path).await?;
            tokio::fs::File::open(&self.outbox_dir).await?.sync_all().await
        }
        .await;
        if captured.is_err() {
            self.gap().await;
            return captured;
        }
        outbox.bytes = next_bytes;
        outbox.pending.insert(key.clone(), Arc::clone(&record));
        self.cache.insert(key.clone(), Arc::clone(&record)).await;
        if let Some(path) = self.working_snapshot_path(&key) {
            let temp = path.with_extension("working.tmp");
            let result = async {
                let mut file = tokio::fs::File::create(&temp).await?;
                file.write_all(&record.bytes).await?;
                file.sync_all().await?;
                tokio::fs::rename(temp, &path).await?;
                tokio::fs::File::open(path.parent().expect("working cache directory"))
                    .await?
                    .sync_all()
                    .await
            }
            .await;
            if result.is_err() {
                tracing::warn!("working board cache unavailable; outbox retained");
            }
        }
        drop(outbox);
        Ok(())
    }
    pub async fn flush(&self) -> io::Result<()> {
        let result = tokio::time::timeout(Duration::from_secs(3), async {
            // Probe/replay without holding the local-capture mutex over network I/O.
            self.client().await?.execute("SELECT 1", &[]).await.map_err(error)?;
            let records: Vec<_> = self.outbox.lock().await.pending.values().cloned().collect();
            for record in records {
                self.write_record(&record, false).await?;
                let key = &record.meta.key;
                let mut outbox = self.outbox.lock().await;
                if outbox.pending.get(key).is_some_and(|current| Arc::ptr_eq(current, &record)) {
                    tokio::fs::remove_file(
                        self.outbox_dir.join(format!("{}.pending", digest(key.as_bytes()))),
                    )
                    .await?;
                    outbox.pending.remove(key);
                    outbox.bytes -= record.bytes.len() as u64;
                }
            }
            Ok::<_, io::Error>(())
        })
        .await
        .map_err(error)
        .and_then(|result| result);
        self.degraded.store(result.is_err(), Ordering::Relaxed);
        self.read_degraded.store(result.is_err(), Ordering::Relaxed);
        result
    }
    pub async fn put(&self, key: &str, bytes: Vec<u8>) -> io::Result<()> {
        self.queue(Record {
            meta: Metadata { key: key.into(), kind: "snapshot".into(), ..Default::default() },
            bytes,
        })
        .await
    }
    async fn put_attestation(
        &self,
        bytes: Vec<u8>,
        import: bool,
        trusted: &HashSet<String>,
    ) -> io::Result<bool> {
        if bytes.len() > attestation::MAX_ATTESTATION_BYTES {
            return Err(io::Error::other("attestation exceeds the size limit"));
        }
        let att: Attestation = serde_json::from_slice(&bytes).map_err(error)?;
        let valid = trusted.contains(&att.signer) && attestation::verify(&att).is_ok();
        let chain = serde_json::to_value(att.chain)
            .map_err(error)?
            .as_str()
            .ok_or_else(|| error(()))?
            .to_owned();
        let normalize = |value: &str| {
            if chain == "solana" { value.to_owned() } else { value.to_ascii_lowercase() }
        };
        let record = Record {
            meta: Metadata {
                key: format!("attestations/{}.json", att.id),
                kind: "attestation".into(),
                id: Some(att.id),
                pool: Some(normalize(&att.pool.pool)),
                subject: Some(normalize(&att.subject)),
                chain: Some(chain.clone()),
                checked_at: Some(
                    DateTime::parse_from_rfc3339(&att.checked_at)
                        .map_err(error)?
                        .with_timezone(&Utc),
                ),
                expires_at: Some(
                    DateTime::parse_from_rfc3339(&att.expires_at)
                        .map_err(error)?
                        .with_timezone(&Utc),
                ),
                quarantined: !valid,
                deleted: false,
            },
            bytes,
        };
        if import {
            self.write_record(&record, true).await?;
        } else if valid {
            self.queue(record).await?;
        } else {
            return Err(error(()));
        }
        Ok(valid)
    }
    async fn import_attestation(
        &self,
        key: &str,
        bytes: Vec<u8>,
        trusted: &HashSet<String>,
    ) -> io::Result<bool> {
        let valid = bytes.len() <= attestation::MAX_ATTESTATION_BYTES
            && serde_json::from_slice::<Attestation>(&bytes).is_ok_and(|att| {
                key == format!("attestations/{}.json", att.id)
                    && trusted.contains(&att.signer)
                    && attestation::verify(&att).is_ok()
                    && DateTime::parse_from_rfc3339(&att.checked_at).is_ok()
                    && DateTime::parse_from_rfc3339(&att.expires_at).is_ok()
            });
        if valid {
            self.put_attestation(bytes, true, trusted).await
        } else {
            let id = key
                .strip_prefix("attestations/")
                .and_then(|key| key.strip_suffix(".json"))
                .map(str::to_owned);
            self.write_record(
                &Record {
                    meta: Metadata {
                        key: key.into(),
                        kind: "attestation".into(),
                        id,
                        quarantined: true,
                        ..Default::default()
                    },
                    bytes,
                },
                true,
            )
            .await?;
            Ok(false)
        }
    }
    async fn read_record(&self, key: &str) -> io::Result<Option<Arc<Record>>> {
        // An unflushed newer local write must not be replaced by an older database value.
        if let Some(record) = self.outbox.lock().await.pending.get(key).cloned() {
            return Ok(Some(record));
        }
        if self.degraded.load(Ordering::Relaxed) || self.read_degraded.load(Ordering::Relaxed) {
            return Ok(self.cache.get(key).await);
        }
        let result = tokio::time::timeout(Duration::from_secs(3), async {
            let client = self.client().await?;
            let Some(row) = client.query_opt("SELECT payload,s3_key,sha256,quarantined,deleted FROM durable_objects WHERE key=$1", &[&key]).await.map_err(error)? else { return Ok(None); };
            let deleted: bool = row.get(4);
            let quarantined: bool = row.get(3);
            let mut bytes: Vec<u8> = row.get::<_, Option<Vec<u8>>>(0).unwrap_or_default();
            if let Some(s3_key) = row.get::<_, Option<String>>(1) {
                bytes = self.s3_read(&s3_key).await?.ok_or_else(|| error(()))?;
            }
            if !deleted && digest(&bytes) != row.get::<_, String>(2) { return Err(error(())); }
            Ok::<_,io::Error>(Some(Arc::new(Record { meta: Metadata { key: key.into(), quarantined, deleted, ..Default::default() }, bytes })))
        }).await.map_err(error).and_then(|result| result);
        self.read_degraded.store(result.is_err(), Ordering::Relaxed);
        match result {
            Ok(Some(record)) => {
                self.cache.insert(key.to_owned(), Arc::clone(&record)).await;
                Ok(Some(record))
            }
            Ok(None) => Ok(None),
            Err(_) => {
                self.degraded.store(true, Ordering::Relaxed);
                Ok(self.cache.get(key).await)
            }
        }
    }
    pub async fn get(&self, key: &str) -> io::Result<Option<Vec<u8>>> {
        Ok(self
            .read_record(key)
            .await?
            .filter(|record| !record.meta.deleted && !record.meta.quarantined)
            .map(|record| record.bytes.clone()))
    }
    async fn s3_read(&self, key: &str) -> io::Result<Option<Vec<u8>>> {
        #[cfg(feature = "s3")]
        if let Some((client, bucket)) = &self.s3 {
            let output = match client.get_object().bucket(bucket).key(key).send().await {
                Ok(output) => output,
                Err(error)
                    if error.as_service_error().is_some_and(|error| error.is_no_such_key()) =>
                {
                    return Ok(None);
                }
                Err(e) => return Err(error(e)),
            };
            use tokio::io::AsyncReadExt;
            if output.content_length().is_some_and(|size| size > MAX_OBJECT_BYTES as i64) {
                return Err(error(()));
            }
            let mut bytes = Vec::new();
            output
                .body
                .into_async_read()
                .take((MAX_OBJECT_BYTES + 1) as u64)
                .read_to_end(&mut bytes)
                .await?;
            if bytes.len() > MAX_OBJECT_BYTES {
                return Err(error(()));
            }
            return Ok(Some(bytes));
        }
        let _ = key;
        Ok(None)
    }
    pub async fn restore_runtime_files(&self, dir: &Path) -> io::Result<()> {
        for name in RUNTIME_FILES {
            let key = format!("runtime/{name}");
            if let Some(bytes) = self.get(&key).await? {
                let path = dir.join(name);
                let temp = path.with_extension("restore.tmp");
                tokio::fs::write(&temp, bytes).await?;
                tokio::fs::rename(temp, path).await?;
            } else if let Ok(bytes) = tokio::fs::read(dir.join(name)).await {
                self.import_snapshot(&key, bytes).await?;
            }
        }
        Ok(())
    }
    pub async fn capture_runtime_file(&self, path: &Path) -> io::Result<()> {
        let name = path.file_name().and_then(|name| name.to_str()).ok_or_else(|| error(()))?;
        if !RUNTIME_FILES.contains(&name) {
            return Err(error(()));
        }
        let bytes = tokio::fs::read(path).await?;
        self.put(&format!("runtime/{name}"), bytes).await
    }
    pub async fn encode_journal_json(
        &self,
        key: &str,
        value: &(impl Serialize + Sync),
    ) -> io::Result<serde_json::Value> {
        let bytes = serde_json::to_vec(value).map_err(error)?;
        if bytes.len() <= self.threshold {
            return serde_json::from_slice(&bytes).map_err(error);
        }
        let hash = digest(&bytes);
        let key = format!("{key}/{hash}");
        self.write_record(
            &Record {
                meta: Metadata { key: key.clone(), kind: "journal".into(), ..Default::default() },
                bytes,
            },
            true,
        )
        .await?;
        Ok(serde_json::json!({"qed_storage_ref": key}))
    }
    pub async fn decode_journal_json(
        &self,
        value: serde_json::Value,
    ) -> io::Result<serde_json::Value> {
        if let Some(key) = value.get("qed_storage_ref").and_then(serde_json::Value::as_str) {
            let bytes = self.get(key).await?.ok_or_else(|| error(()))?;
            serde_json::from_slice(&bytes).map_err(error)
        } else {
            Ok(value)
        }
    }
    async fn import_snapshot(&self, key: &str, bytes: Vec<u8>) -> io::Result<()> {
        let valid = match key {
            "discovery/leaderboard.json" => {
                serde_json::from_slice::<super::discovery::Leaderboard>(&bytes)
                    .is_ok_and(|board| !board.entries.is_empty() && board.entries.len() <= 300)
            }
            "discovery/featured.json" => {
                serde_json::from_slice::<super::discovery::FeaturedSnapshot>(&bytes)
                    .is_ok_and(|board| !board.pools.is_empty() && board.pools.len() <= 12)
            }
            _ => serde_json::from_slice::<serde_json::Value>(&bytes).is_ok(),
        };
        self.write_record(
            &Record {
                meta: Metadata {
                    key: key.into(),
                    kind: "snapshot".into(),
                    quarantined: !valid,
                    ..Default::default()
                },
                bytes,
            },
            true,
        )
        .await
    }
    async fn import_quarantine(&self, key: &str, bytes: Vec<u8>) -> io::Result<bool> {
        self.write_record(
            &Record {
                meta: Metadata {
                    key: key.into(),
                    kind: "quarantine".into(),
                    quarantined: true,
                    ..Default::default()
                },
                bytes,
            },
            true,
        )
        .await?;
        Ok(false)
    }
    async fn progress(&self, source: &str) -> io::Result<Option<(Option<String>, bool)>> {
        Ok(self
            .client()
            .await?
            .query_opt(
                "SELECT cursor,completed FROM storage_import_progress WHERE source=$1",
                &[&source],
            )
            .await
            .map_err(error)?
            .map(|row| (row.get(0), row.get(1))))
    }
    async fn checkpoint(
        &self,
        source: &str,
        cursor: Option<&str>,
        completed: bool,
        imported: i64,
        rejected: i64,
    ) -> io::Result<()> {
        self.client().await?.execute("INSERT INTO storage_import_progress(source,cursor,completed,imported,rejected) VALUES($1,$2,$3,$4,$5) ON CONFLICT(source) DO UPDATE SET cursor=EXCLUDED.cursor,completed=EXCLUDED.completed,imported=storage_import_progress.imported+EXCLUDED.imported,rejected=storage_import_progress.rejected+EXCLUDED.rejected,updated_at=now()", &[&source,&cursor,&completed,&imported,&rejected]).await.map_err(error)?;
        Ok(())
    }
    pub async fn import_legacy(&self, dir: &Path, trusted: &HashSet<String>) -> io::Result<()> {
        let progress = self.progress("disk-v1").await?;
        if !progress.as_ref().is_some_and(|(_, done)| *done) {
            let cursor = progress.and_then(|(cursor, _)| cursor);
            let mut files = Vec::new();
            let mut entries = match tokio::fs::read_dir(dir.join("attestations")).await {
                Ok(entries) => Some(entries),
                Err(e) if e.kind() == io::ErrorKind::NotFound => None,
                Err(e) => return Err(e),
            };
            while let Some(entries) = entries.as_mut() {
                let Some(entry) = entries.next_entry().await? else {
                    break;
                };
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.strip_suffix(".json").is_some_and(|id| {
                    id.len() == 64 && id.bytes().all(|byte| byte.is_ascii_hexdigit())
                }) && cursor.as_ref().is_none_or(|cursor| &name > cursor)
                {
                    files.push(name);
                }
            }
            files.sort_unstable();
            for batch in files.chunks(100) {
                let mut imported = 0;
                let mut rejected = 0;
                for name in batch {
                    let bytes = tokio::fs::read(dir.join("attestations").join(name)).await?;
                    if self
                        .import_attestation(&format!("attestations/{name}"), bytes, trusted)
                        .await?
                    {
                        imported += 1;
                    } else {
                        rejected += 1;
                    }
                }
                self.checkpoint(
                    "disk-v1",
                    batch.last().map(String::as_str),
                    false,
                    imported,
                    rejected,
                )
                .await?;
            }
            match tokio::fs::read_dir(dir.join("attestations/quarantine")).await {
                Ok(mut entries) => {
                    while let Some(entry) = entries.next_entry().await? {
                        let name = entry.file_name().to_string_lossy().into_owned();
                        if name.ends_with(".json") {
                            self.import_quarantine(
                                &format!("attestations/quarantine/{name}"),
                                tokio::fs::read(entry.path()).await?,
                            )
                            .await?;
                        }
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            for name in ["leaderboard.json", "featured.json"] {
                if let Ok(bytes) = tokio::fs::read(dir.join(name)).await {
                    self.import_snapshot(&format!("discovery/{name}"), bytes).await?;
                }
            }
            self.checkpoint("disk-v1", None, true, 0, 0).await?;
            tracing::info!("legacy disk import complete");
        }
        #[cfg(feature = "s3")]
        if let Some((client, bucket)) = &self.s3 {
            let source = format!("s3-v1:{bucket}");
            let progress = self.progress(&source).await?;
            if progress.as_ref().is_some_and(|(_, done)| *done) {
                return Ok(());
            }
            let mut cursor = progress
                .and_then(|(cursor, _)| cursor)
                .filter(|key| key.starts_with("attestations/"));
            loop {
                let page = client
                    .list_objects_v2()
                    .bucket(bucket)
                    .prefix("attestations/")
                    .set_start_after(cursor.clone())
                    .max_keys(100)
                    .send()
                    .await
                    .map_err(error)?;
                let keys: Vec<_> = page
                    .contents()
                    .iter()
                    .filter_map(|object| object.key())
                    .filter(|key| {
                        key.strip_prefix("attestations/quarantine/")
                            .or_else(|| key.strip_prefix("attestations/"))
                            .and_then(|name| name.strip_suffix(".json"))
                            .is_some_and(|id| {
                                id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit())
                            })
                    })
                    .map(str::to_owned)
                    .collect();
                let counts = futures_util::stream::iter(keys.into_iter().map(|key| async move {
                    let Some(bytes) = self.s3_read(&key).await? else {
                        return Ok::<_, io::Error>(false);
                    };
                    if key.starts_with("attestations/quarantine/") {
                        self.import_quarantine(&key, bytes).await
                    } else {
                        self.import_attestation(&key, bytes, trusted).await
                    }
                }))
                .buffer_unordered(8)
                .try_collect::<Vec<_>>()
                .await?;
                let more = page.is_truncated().unwrap_or(false);
                let last_key =
                    page.contents().last().and_then(|object| object.key()).map(str::to_owned);
                if more && (last_key.is_none() || last_key == cursor) {
                    return Err(error(()));
                }
                cursor = last_key;
                // Only checkpoint after every object in the page commits. A crash replays harmlessly.
                self.checkpoint(
                    &source,
                    cursor.as_deref(),
                    false,
                    counts.iter().filter(|valid| **valid).count() as i64,
                    counts.iter().filter(|valid| !**valid).count() as i64,
                )
                .await?;
                if !more {
                    break;
                }
            }
            for name in ["leaderboard.json", "featured.json"] {
                let key = format!("discovery/{name}");
                if let Some(bytes) = self.s3_read(&key).await? {
                    self.import_snapshot(&key, bytes).await?;
                }
            }
            self.checkpoint(&source, None, true, 0, 0).await?;
            tracing::info!("legacy S3 import complete");
        }
        Ok(())
    }
}

pub(crate) struct PostgresAttestationStore {
    pub durable: Arc<DurableStore>,
    pub legacy: Arc<dyn AttestationStore>,
    pub disk: super::attestation::FileAttestationStore,
    pub trusted: HashSet<String>,
}
#[async_trait]
impl AttestationStore for PostgresAttestationStore {
    async fn load_recent(&self) -> io::Result<HashMap<String, Attestation>> {
        // Never walk S3 during startup. Background import discovers legacy objects.
        let mut records = self.disk.load_recent().await?;
        let disk_ids: Vec<_> = records.keys().cloned().collect();
        let database_rows = tokio::time::timeout(Duration::from_secs(3), async {
            if !self.durable.available() { return Err(error(())); }
            let client = self.durable.client().await?;
            let existing = client.query("SELECT id FROM durable_objects WHERE kind='attestation' AND id=ANY($1)", &[&disk_ids]).await.map_err(error)?;
            let recent = client.query("WITH candidates AS MATERIALIZED (SELECT id,payload,sha256,quarantined,deleted,checked_at,key FROM durable_objects WHERE kind='attestation' AND NOT quarantined AND NOT deleted AND payload IS NOT NULL ORDER BY checked_at DESC,key LIMIT 1000), recent AS (SELECT *,sum(octet_length(payload)) OVER (ORDER BY checked_at DESC,key) AS total_bytes FROM candidates) SELECT id,payload,sha256,quarantined,deleted FROM recent WHERE total_bytes<=16777216 ORDER BY checked_at DESC,key", &[]).await.map_err(error)?;
            Ok::<_,io::Error>((existing,recent))
        }).await;
        if let Ok(Ok((existing, rows))) = database_rows {
            for row in existing {
                records.remove(&row.get::<_, String>(0));
            }
            let mut loaded_bytes = 0;
            for row in rows {
                let id: String = row.get(0);
                records.remove(&id);
                if row.get::<_, bool>(3) || row.get::<_, bool>(4) {
                    continue;
                }
                if let Some(bytes) = row.get::<_, Option<Vec<u8>>>(1) {
                    loaded_bytes += bytes.len();
                    if loaded_bytes > 16 * 1024 * 1024 {
                        break;
                    }
                    if digest(&bytes) == row.get::<_, String>(2)
                        && let Ok(att) = serde_json::from_slice::<Attestation>(&bytes)
                    {
                        records.insert(att.id.clone(), att);
                    }
                }
            }
        } else {
            self.durable.degraded.store(true, Ordering::Relaxed);
        }
        for record in self.durable.outbox.lock().await.pending.values() {
            if record.meta.kind == "attestation"
                && !record.meta.quarantined
                && !record.meta.deleted
                && let Ok(att) = serde_json::from_slice::<Attestation>(&record.bytes)
            {
                records.insert(att.id.clone(), att);
            }
        }
        Ok(records)
    }
    async fn latest_for_pool(&self, chain: &str, pool: &str) -> io::Result<Option<Attestation>> {
        let chain = crate::domain::chain::Chain::from_network_name(chain)
            .map(|chain| {
                serde_json::to_value(chain)
                    .expect("chain serializes")
                    .as_str()
                    .expect("chain string")
                    .to_owned()
            })
            .unwrap_or_else(|| chain.to_ascii_lowercase());
        let pool = if chain == "solana" { pool.to_owned() } else { pool.to_ascii_lowercase() };
        if self.durable.available() {
            let result = tokio::time::timeout(Duration::from_secs(3), async {
                let client = self.durable.client().await?;
                client.query_opt("SELECT id FROM durable_objects WHERE kind='attestation' AND chain=$1 AND (pool=$2 OR subject=$2) AND NOT quarantined AND NOT deleted ORDER BY checked_at DESC LIMIT 1", &[&chain,&pool]).await.map_err(error)
            }).await;
            match result {
                Ok(Ok(Some(row))) => return self.get(&row.get::<_, String>(0)).await,
                Ok(Ok(None)) => {}
                _ => {
                    self.durable.degraded.store(true, Ordering::Relaxed);
                }
            }
        }
        if let Ok(Some(att)) = self.legacy.latest_for_pool(&chain, &pool).await {
            return self.get(&att.id).await;
        }
        if let Some(att) = self.disk.latest_for_pool(&chain, &pool).await? {
            return self.get(&att.id).await;
        }
        Ok(None)
    }
    async fn persist(&self, att: &Attestation) -> io::Result<()> {
        self.durable
            .put_attestation(serde_json::to_vec_pretty(att).map_err(error)?, false, &self.trusted)
            .await
            .map(|_| ())
    }
    async fn import_raw(&self, id: &str, bytes: Vec<u8>) -> io::Result<()> {
        tokio::time::timeout(
            Duration::from_secs(3),
            self.durable.import_attestation(
                &format!("attestations/{id}.json"),
                bytes,
                &self.trusted,
            ),
        )
        .await
        .map_err(error)?
        .map(|_| ())
    }
    async fn get(&self, id: &str) -> io::Result<Option<Attestation>> {
        if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Ok(None);
        }
        let key = format!("attestations/{id}.json");
        if let Some(record) = self.durable.read_record(&key).await? {
            if record.meta.deleted || record.meta.quarantined {
                return Ok(None);
            }
            return serde_json::from_slice(&record.bytes).map(Some).map_err(error);
        }
        if let Ok(Some(att)) = self.legacy.get(id).await {
            return Ok(Some(att));
        }
        self.disk.get(id).await
    }
    async fn raw_json(&self, id: &str) -> io::Result<Option<Vec<u8>>> {
        if id.len() != 64 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Ok(None);
        }
        if let Some(record) = self.durable.read_record(&format!("attestations/{id}.json")).await? {
            return Ok(
                (!record.meta.deleted && !record.meta.quarantined).then(|| record.bytes.clone())
            );
        }
        if let Ok(Some(bytes)) = self.legacy.raw_json(id).await {
            return Ok(Some(bytes));
        }
        self.disk.raw_json(id).await
    }
    async fn prune_expired(&self, cutoff: DateTime<Utc>) -> io::Result<()> {
        // Tombstones prevent legacy fallback from resurrecting expired records. Journal is untouched.
        self.durable.client().await?.execute("UPDATE durable_objects SET deleted=true,payload=NULL,s3_key=NULL,updated_at=now() WHERE kind='attestation' AND expires_at < $1 AND NOT deleted", &[&cutoff]).await.map_err(error)?;
        Ok(())
    }
    async fn quarantine(&self, id: &str) -> io::Result<()> {
        if id.len() != 64 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Ok(());
        }
        let key = format!("attestations/{id}.json");
        let bytes = self.raw_json(id).await?.unwrap_or_default();
        self.durable
            .queue(Record {
                meta: Metadata {
                    key,
                    kind: "attestation".into(),
                    id: Some(id.into()),
                    quarantined: true,
                    ..Default::default()
                },
                bytes,
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn database() -> Option<(tokio_postgres::Client, String, String)> {
        let url = std::env::var("QED_TEST_DATABASE_URL").ok()?;
        let (client, connection) = tokio_postgres::connect(&url, tokio_postgres::NoTls)
            .await
            .expect("local test database");
        tokio::spawn(async move {
            let _ = connection.await;
        });
        let schema = format!("qed_storage_test_{:x}", rand::random::<u64>());
        client.batch_execute(&format!("CREATE SCHEMA {schema}")).await.unwrap();
        let mut scoped = reqwest::Url::parse(&url).unwrap();
        scoped.query_pairs_mut().append_pair("options", &format!("-csearch_path={schema}"));
        Some((client, scoped.into(), schema))
    }
    fn adapter(
        durable: Arc<DurableStore>,
        dir: &Path,
        trusted: HashSet<String>,
    ) -> PostgresAttestationStore {
        PostgresAttestationStore {
            durable,
            legacy: Arc::new(super::super::attestation::FileAttestationStore::new(
                dir.join("attestations"),
            )),
            disk: super::super::attestation::FileAttestationStore::new(dir.join("attestations")),
            trusted,
        }
    }
    #[tokio::test]
    async fn postgres_import_is_resumable_exact_trusted_and_retention_cannot_resurrect() {
        let Some((client, url, schema)) = database().await else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::create_dir(dir.path().join("attestations")).await.unwrap();
        let valid = crate::app::attestation::signed_test_attestation([31; 32], false);
        let untrusted = crate::app::attestation::signed_test_attestation([32; 32], false);
        let trusted = HashSet::from([valid.signer.clone()]);
        let bytes = format!("  {}\n\n", serde_json::to_string_pretty(&valid).unwrap()).into_bytes();
        let key = format!("attestations/{}.json", valid.id);
        tokio::fs::write(dir.path().join(&key), &bytes).await.unwrap();
        tokio::fs::write(
            dir.path().join(format!("attestations/{}.json", untrusted.id)),
            serde_json::to_vec(&untrusted).unwrap(),
        )
        .await
        .unwrap();
        let malformed = format!("attestations/{}.json", "a".repeat(64));
        tokio::fs::write(dir.path().join(&malformed), b"{broken").await.unwrap();
        let store = DurableStore::open(&url, dir.path(), None, MAX_OBJECT_BYTES).await.unwrap();
        let first = [key.clone(), format!("attestations/{}.json", untrusted.id), malformed.clone()]
            .into_iter()
            .min()
            .unwrap();
        let accepted = store
            .import_attestation(
                &first,
                tokio::fs::read(dir.path().join(&first)).await.unwrap(),
                &trusted,
            )
            .await
            .unwrap();
        store
            .checkpoint(
                "disk-v1",
                first.strip_prefix("attestations/"),
                false,
                i64::from(accepted),
                i64::from(!accepted),
            )
            .await
            .unwrap();
        drop(store);
        let store = DurableStore::open(&url, dir.path(), None, MAX_OBJECT_BYTES).await.unwrap();
        store.import_legacy(dir.path(), &trusted).await.unwrap();
        store.import_legacy(dir.path(), &trusted).await.unwrap();
        let adapter = adapter(store.clone(), dir.path(), trusted);
        assert_eq!(adapter.raw_json(&valid.id).await.unwrap().unwrap(), bytes);
        adapter.import_raw(&valid.id, serde_json::to_vec(&valid).unwrap()).await.unwrap();
        assert_eq!(
            adapter.raw_json(&valid.id).await.unwrap().unwrap(),
            bytes,
            "import never reserializes or replaces existing bytes"
        );
        attestation::verify(&adapter.get(&valid.id).await.unwrap().unwrap()).unwrap();
        assert!(adapter.get(&untrusted.id).await.unwrap().is_none());
        assert!(store.get(&malformed).await.unwrap().is_none());
        let counts: (i64, i64) = {
            let row = client.query_one(&format!("SELECT imported,rejected FROM {schema}.storage_import_progress WHERE source='disk-v1'"), &[]).await.unwrap();
            (row.get(0), row.get(1))
        };
        assert_eq!(counts, (1, 2));
        assert_eq!(
            adapter.latest_for_pool("base", &valid.pool.pool).await.unwrap().unwrap().id,
            valid.id
        );
        adapter.prune_expired(Utc::now() + chrono::Duration::days(31)).await.unwrap();
        assert!(
            adapter.get(&valid.id).await.unwrap().is_none(),
            "legacy file must not resurrect tombstoned proof"
        );
        assert!(adapter.raw_json(&valid.id).await.unwrap().is_none());
        adapter.import_raw(&valid.id, bytes).await.unwrap();
        assert!(
            adapter.raw_json(&valid.id).await.unwrap().is_none(),
            "backfill import respects retention tombstones"
        );
        let deleted: bool = client
            .query_one(
                &format!("SELECT deleted FROM {schema}.durable_objects WHERE key=$1"),
                &[&key],
            )
            .await
            .unwrap()
            .get(0);
        assert!(deleted);
        drop(adapter);
        drop(store);
        client.batch_execute(&format!("DROP SCHEMA {schema} CASCADE")).await.unwrap();
    }
    #[tokio::test]
    async fn postgres_outage_outbox_survives_restart_and_replays_latest_bytes() {
        let Some((client, url, schema)) = database().await else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let mut down = reqwest::Url::parse(&url).unwrap();
        down.set_port(Some(1)).unwrap();
        let store =
            DurableStore::open(down.as_str(), dir.path(), None, MAX_OBJECT_BYTES).await.unwrap();
        store
            .put("runtime/ondo-public-attempt.json", br#"{"version":"old"}"#.to_vec())
            .await
            .unwrap();
        let expected = br#"{"version":"new","attempted_at":"2026-10-17T12:00:00Z"}"#.to_vec();
        store.put("runtime/ondo-public-attempt.json", expected.clone()).await.unwrap();
        assert_eq!(
            store.get("runtime/ondo-public-attempt.json").await.unwrap(),
            Some(expected.clone())
        );
        assert_eq!(store.status()["degraded"], true);
        drop(store);
        let recovered = DurableStore::open(&url, dir.path(), None, MAX_OBJECT_BYTES).await.unwrap();
        assert_eq!(
            recovered.get("runtime/ondo-public-attempt.json").await.unwrap(),
            Some(expected)
        );
        assert_eq!(recovered.status()["degraded"], false);
        assert!(recovered.outbox.lock().await.pending.is_empty());
        drop(recovered);
        client.batch_execute(&format!("DROP SCHEMA {schema} CASCADE")).await.unwrap();
    }
    #[tokio::test]
    async fn startup_query_failure_falls_back_to_disk_and_opens_read_circuit() {
        let Some((client, url, schema)) = database().await else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let valid = crate::app::attestation::signed_test_attestation([41; 32], false);
        tokio::fs::create_dir(dir.path().join("attestations")).await.unwrap();
        tokio::fs::write(
            dir.path().join(format!("attestations/{}.json", valid.id)),
            serde_json::to_vec(&valid).unwrap(),
        )
        .await
        .unwrap();
        let store = DurableStore::open(&url, dir.path(), None, MAX_OBJECT_BYTES).await.unwrap();
        let adapter = adapter(store.clone(), dir.path(), HashSet::from([valid.signer.clone()]));
        client
            .batch_execute(&format!(
                "ALTER TABLE {schema}.durable_objects RENAME TO temporarily_unavailable"
            ))
            .await
            .unwrap();
        let records = adapter.load_recent().await.expect("query errors cannot abort startup");
        assert_eq!(records[&valid.id].signature, valid.signature);
        assert!(!store.available());
        assert_eq!(
            adapter.latest_for_pool("base", &valid.pool.pool).await.unwrap().unwrap().id,
            valid.id
        );
        client
            .batch_execute(&format!(
                "ALTER TABLE {schema}.temporarily_unavailable RENAME TO durable_objects"
            ))
            .await
            .unwrap();
        store.flush().await.unwrap();
        assert!(store.available(), "background probe closes the read circuit");
        client.batch_execute(&format!("DROP SCHEMA {schema} CASCADE")).await.unwrap();
    }
}

#[cfg(all(test, feature = "s3"))]
mod blob_tests {
    use super::*;
    use axum::{
        Router,
        body::Bytes,
        extract::{Path as RoutePath, Query, State},
        response::IntoResponse,
        routing::{get, put},
    };
    type Objects = Arc<Mutex<HashMap<String, Bytes>>>;
    fn local_s3_client(address: std::net::SocketAddr) -> aws_sdk_s3::Client {
        aws_sdk_s3::Client::from_conf(
            aws_sdk_s3::config::Builder::new()
                .behavior_version_latest()
                .region(aws_sdk_s3::config::Region::new("us-east-1"))
                .credentials_provider(aws_sdk_s3::config::Credentials::new(
                    "synthetic",
                    "synthetic",
                    None,
                    None,
                    "local-test",
                ))
                .endpoint_url(format!("http://{address}"))
                .force_path_style(true)
                .build(),
        )
    }
    #[tokio::test]
    async fn s3_threshold_boundary_hash_check_and_journal_round_trip() {
        let Ok(url) = std::env::var("QED_TEST_DATABASE_URL") else {
            return;
        };
        let (client, connection) =
            tokio_postgres::connect(&url, tokio_postgres::NoTls).await.unwrap();
        tokio::spawn(async move {
            let _ = connection.await;
        });
        let schema = format!("qed_blob_test_{:x}", rand::random::<u64>());
        client.batch_execute(&format!("CREATE SCHEMA {schema}")).await.unwrap();
        let mut url = reqwest::Url::parse(&url).unwrap();
        url.query_pairs_mut().append_pair("options", &format!("-csearch_path={schema}"));
        let objects: Objects = Arc::new(Mutex::new(HashMap::new()));
        let app = Router::new()
            .route(
                "/{*key}",
                put(
                    |State(objects): State<Objects>,
                     RoutePath(key): RoutePath<String>,
                     body: Bytes| async move {
                        objects.lock().await.insert(key, body);
                        axum::http::StatusCode::OK
                    },
                )
                .get(
                    |State(objects): State<Objects>, RoutePath(key): RoutePath<String>| async move {
                        objects
                            .lock()
                            .await
                            .get(&key)
                            .cloned()
                            .ok_or(axum::http::StatusCode::NOT_FOUND)
                    },
                ),
            )
            .with_state(objects.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let s3 = local_s3_client(address);
        let dir = tempfile::tempdir().unwrap();
        let mut store = DurableStore::open(url.as_str(), dir.path(), None, 16).await.unwrap();
        Arc::get_mut(&mut store).unwrap().s3 = Some((s3.clone(), "local-test".into()));
        store.put("discovery/boundary.json", vec![b'x'; 16]).await.unwrap();
        store.put("discovery/large.json", vec![b'y'; 17]).await.unwrap();
        store.flush().await.unwrap();
        assert_eq!(store.status()["degraded"], false);
        let rows = client.query(&format!("SELECT key,payload IS NULL,s3_key,sha256 FROM {schema}.durable_objects ORDER BY key"), &[]).await.unwrap();
        assert!(!rows[0].get::<_, bool>(1));
        assert!(rows[1].get::<_, bool>(1));
        let s3_key: String = rows[1].get(2);
        assert_eq!(rows[1].get::<_, String>(3), digest(&[b'y'; 17]));
        assert_eq!(store.get("discovery/large.json").await.unwrap(), Some(vec![b'y'; 17]));
        objects.lock().await.insert(format!("local-test/{s3_key}"), Bytes::from_static(b"corrupt"));
        drop(store);
        let mut reopened = DurableStore::open(url.as_str(), dir.path(), None, 16).await.unwrap();
        Arc::get_mut(&mut reopened).unwrap().s3 = Some((s3, "local-test".into()));
        assert!(reopened.get("discovery/large.json").await.unwrap().is_none());
        assert_eq!(reopened.status()["degraded"], true);
        reopened.flush().await.unwrap();
        let document = serde_json::json!({"facts": "large journal observation"});
        let reference =
            reopened.encode_journal_json("history/test/event.json", &document).await.unwrap();
        assert!(reference.get("qed_storage_ref").is_some());
        assert_eq!(reopened.decode_journal_json(reference).await.unwrap(), document);
        drop(reopened);
        server.abort();
        client.batch_execute(&format!("DROP SCHEMA {schema} CASCADE")).await.unwrap();
    }

    #[tokio::test]
    async fn s3_import_resumes_after_page_failure_and_ignores_stale_indexes() {
        let Ok(url) = std::env::var("QED_TEST_DATABASE_URL") else {
            return;
        };
        const LEGACY: &[u8] = include_bytes!(
            "../../tests/fixtures/attest/fc6147d4cd42374b72246cc6340d23e26f5c411e63f613322a18413c3da243bb.json"
        );
        type Pages = (Arc<AtomicBool>, Arc<Mutex<Vec<Option<String>>>>);
        async fn list(
            State((paused, requests)): State<Pages>,
            Query(query): Query<HashMap<String, String>>,
        ) -> axum::response::Response {
            assert_eq!(query.get("prefix").map(String::as_str), Some("attestations/"));
            let cursor = query.get("start-after").cloned();
            requests.lock().await.push(cursor.clone());
            if cursor.is_some() && paused.load(Ordering::SeqCst) {
                return (
                    axum::http::StatusCode::FORBIDDEN,
                    "<Error><Code>AccessDenied</Code></Error>",
                )
                    .into_response();
            }
            let contents = if cursor.is_none() {
                "<IsTruncated>true</IsTruncated><NextContinuationToken>opaque-token</NextContinuationToken><Contents><Key>attestations/0000index.json</Key></Contents>".to_owned()
            } else {
                assert_eq!(cursor.as_deref(), Some("attestations/0000index.json"));
                let attestation: Attestation = serde_json::from_slice(LEGACY).unwrap();
                format!(
                    "<IsTruncated>false</IsTruncated><Contents><Key>attestations/{}.json</Key></Contents>",
                    attestation.id
                )
            };
            ([(axum::http::header::CONTENT_TYPE, "application/xml")], format!("<ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Name>local-test</Name>{contents}</ListBucketResult>")).into_response()
        }
        let (client, connection) =
            tokio_postgres::connect(&url, tokio_postgres::NoTls).await.unwrap();
        tokio::spawn(async move {
            let _ = connection.await;
        });
        let schema = format!("qed_import_test_{:x}", rand::random::<u64>());
        client.batch_execute(&format!("CREATE SCHEMA {schema}")).await.unwrap();
        let mut url = reqwest::Url::parse(&url).unwrap();
        url.query_pairs_mut().append_pair("options", &format!("-csearch_path={schema}"));
        let paused = Arc::new(AtomicBool::new(true));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new()
            .route("/local-test", get(list))
            .route("/local-test/", get(list))
            .route(
                "/local-test/{*key}",
                get(|RoutePath(key): RoutePath<String>| async move {
                    let attestation: Attestation = serde_json::from_slice(LEGACY).unwrap();
                    if key == format!("attestations/{}.json", attestation.id) {
                        Bytes::from_static(LEGACY).into_response()
                    } else {
                        (axum::http::StatusCode::NOT_FOUND, "<Error><Code>NoSuchKey</Code></Error>")
                            .into_response()
                    }
                }),
            )
            .with_state((paused.clone(), requests.clone()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let s3 = local_s3_client(listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let dir = tempfile::tempdir().unwrap();
        let attestation: Attestation = serde_json::from_slice(LEGACY).unwrap();
        let trusted = HashSet::from(["93JfrkBDb7FwAXVDj4f5TH77RXswStL16yUAhyxPCwpu".to_owned()]);
        let mut store =
            DurableStore::open(url.as_str(), dir.path(), None, 1024 * 1024).await.unwrap();
        Arc::get_mut(&mut store).unwrap().s3 = Some((s3.clone(), "local-test".into()));
        assert!(store.import_legacy(dir.path(), &trusted).await.is_err());
        assert_eq!(
            store.progress("s3-v1:local-test").await.unwrap(),
            Some((Some("attestations/0000index.json".into()), false))
        );
        drop(store);
        paused.store(false, Ordering::SeqCst);
        let mut reopened =
            DurableStore::open(url.as_str(), dir.path(), None, 1024 * 1024).await.unwrap();
        Arc::get_mut(&mut reopened).unwrap().s3 = Some((s3, "local-test".into()));
        reopened.import_legacy(dir.path(), &trusted).await.unwrap();
        reopened.import_legacy(dir.path(), &trusted).await.unwrap();
        assert_eq!(
            reopened.get(&format!("attestations/{}.json", attestation.id)).await.unwrap(),
            Some(LEGACY.to_vec())
        );
        let row = client.query_one(&format!("SELECT imported,rejected,completed FROM {schema}.storage_import_progress WHERE source='s3-v1:local-test'"), &[]).await.unwrap();
        assert_eq!(row.get::<_, i64>(0), 1);
        assert_eq!(row.get::<_, i64>(1), 0);
        assert!(row.get::<_, bool>(2));
        assert_eq!(
            *requests.lock().await,
            vec![
                None,
                Some("attestations/0000index.json".into()),
                Some("attestations/0000index.json".into())
            ]
        );
        drop(reopened);
        server.abort();
        client.batch_execute(&format!("DROP SCHEMA {schema} CASCADE")).await.unwrap();
    }
}
