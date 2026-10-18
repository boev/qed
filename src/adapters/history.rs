use crate::{
    domain::asset_history::{AssetKey, AssetState, Facet, Observation, reduce},
    ports::AssetHistoryStore,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use deadpool_postgres::Pool;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncSeekExt, AsyncWriteExt, BufReader, SeekFrom},
    sync::Mutex,
};

const OUTBOX_CAP: usize = 50_000;
#[cfg(test)]
const MIGRATION: &str = include_str!("../../migrations/0001_asset_history.sql");
const RECENT_CAP: usize = 512;
const QUERY_LIMIT: usize = 201;
const OUTBOX_MAX_BYTES: u64 = 512 * 1024 * 1024;
const EVENT_MAX_BYTES: usize = 32 * 1024 * 1024;
const DATABASE_DEADLINE: Duration = Duration::from_secs(3);

enum FlushError {
    Database(String),
    Capture(String),
}
impl From<String> for FlushError {
    fn from(error: String) -> Self {
        Self::Database(error)
    }
}
#[derive(Default)]
struct Projection {
    ids: HashSet<String>,
    events: VecDeque<Arc<Observation>>,
    states: HashMap<(AssetKey, Facet), AssetState>,
    tracked: HashSet<AssetKey>,
    db_unavailable: bool,
    degraded_since: Option<DateTime<Utc>>,
    capture_gap_since: Option<DateTime<Utc>>,
    last_warning: Option<DateTime<Utc>>,
}
impl Projection {
    fn apply(&mut self, event: Arc<Observation>) -> bool {
        if self.ids.contains(&event.event_id) {
            return false;
        }
        self.ids.insert(event.event_id.clone());
        let event = Arc::new(event.summary());
        let tracked = self.tracked.contains(&event.asset);
        if event.projects(tracked) {
            if !tracked {
                self.tracked.insert(event.asset.clone());
            }
            let key = (event.asset.clone(), event.facet);
            let state = self.states.entry(key).or_default();
            *state = reduce(state, &event);
        }
        if self.events.len() == RECENT_CAP
            && let Some(old) = self.events.pop_front()
        {
            self.ids.remove(&old.event_id);
        }
        self.events.push_back(event);
        true
    }
}
#[derive(Clone)]
struct Pending {
    observation: Arc<Observation>,
    offset: u64,
    bytes: usize,
}
struct Writer {
    pending: VecDeque<Pending>,
    disk_events: usize,
    disk_bytes: u64,
    migrated: bool,
    hydrated: bool,
    outbox_unsafe: bool,
}
pub(crate) struct HistoryStore {
    projection: RwLock<Projection>,
    writer: Mutex<Writer>,
    flush_lock: Mutex<()>,
    pool: Option<Pool>,
    outbox: Option<PathBuf>,
    durable: Option<Arc<super::storage::DurableStore>>,
}
fn sanitized<T>(_: T) -> String {
    "history database unavailable or incompatible".to_owned()
}
fn json_name(value: impl serde::Serialize) -> String {
    serde_json::to_value(value)
        .expect("history enums serialize")
        .as_str()
        .expect("history enum string")
        .to_owned()
}
impl HistoryStore {
    pub fn memory() -> Arc<Self> {
        Arc::new(Self {
            projection: RwLock::new(Projection::default()),
            writer: Mutex::new(Writer {
                pending: VecDeque::new(),
                disk_events: 0,
                disk_bytes: 0,
                migrated: false,
                hydrated: true,
                outbox_unsafe: false,
            }),
            flush_lock: Mutex::new(()),
            pool: None,
            outbox: None,
            durable: None,
        })
    }
    pub async fn open(
        url: Option<&str>,
        data_dir: &Path,
        durable: Option<Arc<super::storage::DurableStore>>,
    ) -> Result<Arc<Self>, String> {
        let Some(url) = url else {
            return Ok(Self::memory());
        };
        let pool = crate::adapters::database::pool(url)?;
        let mut outbox_unsafe = tokio::fs::create_dir_all(data_dir).await.is_err();
        let outbox = data_dir.join("asset-history-outbox.jsonl");
        let mut projection = Projection::default();
        if let Ok(marker) = tokio::fs::read_to_string(outbox.with_extension("gap")).await {
            let since = DateTime::parse_from_rfc3339(marker.trim())
                .map(|time| time.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());
            projection.capture_gap_since = Some(since);
            projection.degraded_since = Some(since);
        }
        let mut pending = VecDeque::new();
        let mut disk_bytes = 0u64;
        match tokio::fs::File::open(&outbox).await {
            Ok(file) => {
                if file.metadata().await.map_err(sanitized)?.len() > OUTBOX_MAX_BYTES {
                    outbox_unsafe = true;
                } else {
                    let mut reader = BufReader::new(file);
                    let mut line = Vec::new();
                    loop {
                        line.clear();
                        let length =
                            reader.read_until(b'\n', &mut line).await.map_err(sanitized)?;
                        if length == 0 {
                            break;
                        }
                        let offset = disk_bytes;
                        disk_bytes += length as u64;
                        if length > EVENT_MAX_BYTES
                            || pending.len() >= OUTBOX_CAP
                            || !line.ends_with(b"\n")
                        {
                            outbox_unsafe = true;
                            continue;
                        }
                        let Ok(event) = serde_json::from_slice::<Observation>(&line) else {
                            outbox_unsafe = true;
                            continue;
                        };
                        let summary = Arc::new(event.summary());
                        projection.apply(summary.clone());
                        pending.push_back(Pending { observation: summary, offset, bytes: length });
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => outbox_unsafe = true,
        }
        let disk_events = pending.len();
        let store = Arc::new(Self {
            projection: RwLock::new(projection),
            writer: Mutex::new(Writer {
                pending,
                disk_events,
                disk_bytes,
                migrated: false,
                hydrated: false,
                outbox_unsafe,
            }),
            flush_lock: Mutex::new(()),
            pool: Some(pool),
            outbox: Some(outbox),
            durable,
        });
        if outbox_unsafe {
            store.mark_capture_gap().await;
        }
        if !matches!(tokio::time::timeout(Duration::from_secs(3), store.flush()).await, Ok(Ok(())))
        {
            store.mark_degraded();
        }
        Ok(store)
    }
    fn mark_degraded(&self) {
        let mut projection = self.projection.write().expect("history projection lock");
        projection.db_unavailable = true;
        Self::warn_degraded(&mut projection);
    }
    fn warn_degraded(projection: &mut Projection) {
        if projection.degraded_since.is_none() {
            let now = Utc::now();
            projection.degraded_since = Some(now);
            if projection.last_warning.is_none_or(|last| (now - last).num_seconds() >= 60) {
                projection.last_warning = Some(now);
                tracing::warn!("history recording degraded; local outbox retained");
            }
        }
    }
    async fn mark_capture_gap(&self) {
        let since = {
            let mut projection = self.projection.write().expect("history projection lock");
            Self::warn_degraded(&mut projection);
            *projection.capture_gap_since.get_or_insert_with(Utc::now)
        };
        if let Some(path) = &self.outbox {
            let marker = path.with_extension("gap");
            if let Ok(mut file) =
                tokio::fs::OpenOptions::new().create_new(true).write(true).open(marker).await
            {
                let _ = file.write_all(since.to_rfc3339().as_bytes()).await;
                let _ = file.sync_all().await;
            }
        }
    }
    async fn persist(
        &self,
        client: &mut deadpool_postgres::Client,
        event: &Arc<Observation>,
    ) -> Result<(), String> {
        let transaction = client.transaction().await.map_err(sanitized)?;
        transaction.batch_execute("SET LOCAL statement_timeout = '5s'").await.map_err(sanitized)?;
        let chain = json_name(event.asset.chain);
        let kind = json_name(event.asset.kind);
        let facet = json_name(event.facet);
        let outcome = json_name(event.outcome);
        let row = transaction.query_one("INSERT INTO assets(chain,subject_kind,address,first_seen_at,ticker) VALUES($1,$2,$3,$4,$5) ON CONFLICT(chain,subject_kind,address) DO UPDATE SET first_seen_at=LEAST(assets.first_seen_at,EXCLUDED.first_seen_at),ticker=COALESCE(EXCLUDED.ticker,assets.ticker) RETURNING id", &[&chain,&kind,&event.asset.address,&event.observed_at,&event.ticker]).await.map_err(sanitized)?;
        let asset_id: i64 = row.get(0);
        // Asset row lock above serializes all live/import writers for this identity.
        let event_json = self
            .encode_json(&format!("history/events/{}/event.json", event.event_id), event)
            .await?;
        let observation_kind = match event.facet {
            Facet::ContractMatch => "check",
            Facet::Powers => "powers",
            Facet::Guard => "guard",
        };
        let block = event.block.map(|v| v.to_string());
        let slot = event.slot.map(|v| v.to_string());
        let inserted = transaction.execute("INSERT INTO observations(event_id,asset_id,observed_at,kind,facet,outcome,miss_reason,attestation_id,block,slot,source,registry_hash,event,ticker,scheduled_slot) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9::text::numeric,$10::text::numeric,$11,$12,$13,$14,$15) ON CONFLICT(event_id) DO NOTHING", &[&event.event_id,&asset_id,&event.observed_at,&observation_kind,&facet,&outcome,&event.miss_reason,&event.attestation_id,&block,&slot,&event.source,&event.registry_hash,&event_json,&event.ticker,&event.scheduled_slot]).await.map_err(sanitized)?;
        if inserted == 1
            && (event.projects(false)
                || (event.projects(true)
                    && transaction
                        .query_one(
                            "SELECT EXISTS(SELECT 1 FROM asset_state WHERE asset_id=$1)",
                            &[&asset_id],
                        )
                        .await
                        .map_err(sanitized)?
                        .get::<_, bool>(0)))
        {
            let previous = match transaction
                .query_opt(
                    "SELECT projection FROM asset_state WHERE asset_id=$1 AND facet=$2 FOR UPDATE",
                    &[&asset_id, &facet],
                )
                .await
                .map_err(sanitized)?
            {
                Some(row) => serde_json::from_value::<AssetState>(row.get(0)).map_err(sanitized)?,
                None => AssetState::default(),
            };
            let next = reduce(&previous, event);
            let json = serde_json::to_value(&next).map_err(sanitized)?;
            transaction.execute("INSERT INTO asset_state(asset_id,facet,projection,next_retry_at) VALUES($1,$2,$3,$4) ON CONFLICT(asset_id,facet) DO UPDATE SET projection=EXCLUDED.projection,next_retry_at=EXCLUDED.next_retry_at", &[&asset_id,&facet,&json,&next.next_retry_at]).await.map_err(sanitized)?;
        }
        transaction.commit().await.map_err(sanitized)
    }
    async fn encode_json(
        &self,
        key: &str,
        value: &(impl serde::Serialize + Sync),
    ) -> Result<serde_json::Value, String> {
        match &self.durable {
            Some(store) => store.encode_journal_json(key, value).await.map_err(sanitized),
            None => serde_json::to_value(value).map_err(sanitized),
        }
    }
    async fn decode_json(&self, value: serde_json::Value) -> Result<serde_json::Value, String> {
        match &self.durable {
            Some(store) => store.decode_journal_json(value).await.map_err(sanitized),
            None => Ok(value),
        }
    }
    async fn pending_event(&self, pending: &Pending) -> Result<Arc<Observation>, String> {
        let mut file = tokio::fs::File::open(self.outbox.as_ref().expect("configured outbox"))
            .await
            .map_err(sanitized)?;
        file.seek(SeekFrom::Start(pending.offset)).await.map_err(sanitized)?;
        let mut bytes = vec![0; pending.bytes];
        file.read_exact(&mut bytes).await.map_err(sanitized)?;
        serde_json::from_slice(&bytes).map_err(sanitized)
    }
    async fn compact(&self, writer: &mut Writer) -> Result<(), String> {
        let path = self.outbox.as_ref().expect("configured outbox");
        if writer.outbox_unsafe {
            return Err("history outbox requires local repair; preserved unchanged".into());
        }
        let temporary = path.with_extension("jsonl.tmp");
        let mut file = tokio::fs::File::create(&temporary).await.map_err(sanitized)?;
        let mut offset = 0u64;
        let mut offsets = Vec::with_capacity(writer.pending.len());
        for pending in &writer.pending {
            let event = self.pending_event(pending).await?;
            let mut bytes = serde_json::to_vec(&event).map_err(sanitized)?;
            bytes.push(b'\n');
            offsets.push((offset, bytes.len()));
            offset += bytes.len() as u64;
            file.write_all(&bytes).await.map_err(sanitized)?;
        }
        file.sync_all().await.map_err(sanitized)?;
        tokio::fs::rename(temporary, path).await.map_err(sanitized)?;
        let directory = tokio::fs::File::open(path.parent().expect("outbox directory"))
            .await
            .map_err(sanitized)?;
        directory.sync_all().await.map_err(sanitized)?;
        writer.disk_events = writer.pending.len();
        writer.disk_bytes = offset;
        for (pending, (offset, bytes)) in writer.pending.iter_mut().zip(offsets) {
            pending.offset = offset;
            pending.bytes = bytes;
        }
        Ok(())
    }
    async fn flush_inner(&self) -> Result<(), FlushError> {
        let Some(pool) = &self.pool else {
            return Ok(());
        };
        let _flush = self.flush_lock.lock().await;
        let mut client = pool.get().await.map_err(sanitized)?;
        client.simple_query("SELECT 1").await.map_err(sanitized)?;
        self.projection.write().expect("history projection lock").db_unavailable = false;
        let migrated = self.writer.lock().await.migrated;
        if !migrated {
            super::database::migrate(&mut client).await?;
            self.writer.lock().await.migrated = true;
        }
        let hydrated = self.writer.lock().await.hydrated;
        if !hydrated {
            let mut prepared = Projection::default();
            let mut cursor = (0i64, String::new());
            loop {
                let rows=tokio::time::timeout(DATABASE_DEADLINE,client.query(
                    "SELECT a.chain,a.subject_kind,a.address,s.facet,s.projection,s.asset_id FROM asset_state s JOIN assets a ON a.id=s.asset_id WHERE (s.asset_id,s.facet)>($1,$2::text) ORDER BY s.asset_id,s.facet LIMIT 512",
                    &[&cursor.0,&cursor.1])).await.map_err(sanitized)?.map_err(sanitized)?;
                let count = rows.len();
                for row in rows {
                    let chain = crate::domain::chain::Chain::parse(row.get::<_, &str>(0))
                        .ok_or_else(|| sanitized(()))?;
                    let kind = serde_json::from_value(serde_json::json!(row.get::<_, &str>(1)))
                        .map_err(sanitized)?;
                    let asset =
                        AssetKey::new(chain, kind, row.get(2)).ok_or_else(|| sanitized(()))?;
                    let facet = serde_json::from_value(serde_json::json!(row.get::<_, &str>(3)))
                        .map_err(sanitized)?;
                    let state: AssetState =
                        serde_json::from_value(row.get(4)).map_err(sanitized)?;
                    prepared.tracked.insert(asset.clone());
                    prepared.states.insert((asset, facet), state);
                    cursor = (row.get(5), row.get(3));
                }
                if count < 512 {
                    break;
                }
            }
            let mut writer = self.writer.lock().await;
            for event in &writer.pending {
                prepared.apply(Arc::clone(&event.observation));
            }
            {
                let previous = self.projection.read().expect("history projection lock");
                prepared.capture_gap_since = previous.capture_gap_since;
                prepared.degraded_since = previous.degraded_since;
                prepared.db_unavailable = previous.db_unavailable;
                prepared.last_warning = previous.last_warning;
            }
            *self.projection.write().expect("history projection lock") = prepared;
            writer.hydrated = true;
        }
        loop {
            let pending = self.writer.lock().await.pending.front().cloned();
            let Some(pending) = pending else {
                break;
            };
            let event = self.pending_event(&pending).await.map_err(FlushError::Capture)?;
            if let Err(error) = self.persist(&mut client, &event).await {
                let mut writer = self.writer.lock().await;
                if !writer.outbox_unsafe && self.compact(&mut writer).await.is_err() {
                    self.mark_capture_gap().await;
                }
                return Err(FlushError::Database(error));
            }
            self.writer.lock().await.pending.pop_front();
        }
        {
            let mut writer = self.writer.lock().await;
            if !writer.outbox_unsafe {
                self.compact(&mut writer).await.map_err(FlushError::Capture)?;
            }
        }
        let mut projection = self.projection.write().expect("history projection lock");
        projection.degraded_since = projection.capture_gap_since;
        projection.db_unavailable = false;
        Ok(())
    }
}
#[async_trait]
impl AssetHistoryStore for HistoryStore {
    async fn append(&self, mut event: Observation) -> Result<bool, String> {
        if event.signed_document.as_ref() == Some(&event.evidence) {
            event.evidence = serde_json::Value::Null;
        }
        let event = Arc::new(event);
        let mut writer = self.writer.lock().await;
        if self.projection.read().expect("history projection lock").ids.contains(&event.event_id) {
            return Ok(false);
        }
        if let Some(path) = &self.outbox {
            if writer.outbox_unsafe {
                self.mark_capture_gap().await;
                return Err("history outbox requires local repair; recording gap".into());
            }
            if writer.disk_events >= OUTBOX_CAP {
                self.mark_capture_gap().await;
                return Err("history outbox full; recording gap".into());
            }
            let mut bytes = serde_json::to_vec(&event).map_err(sanitized)?;
            bytes.push(b'\n');
            if bytes.len() > EVENT_MAX_BYTES
                || writer.disk_bytes + bytes.len() as u64 > OUTBOX_MAX_BYTES
            {
                self.mark_capture_gap().await;
                return Err("history outbox full; recording gap".into());
            }
            let result = async {
                let mut file =
                    tokio::fs::OpenOptions::new().create(true).append(true).open(path).await?;
                file.write_all(&bytes).await?;
                file.sync_data().await
            }
            .await;
            if result.is_err() {
                writer.outbox_unsafe = true;
                self.mark_capture_gap().await;
                return Err("history outbox write failed; recording gap".into());
            }
            let offset = writer.disk_bytes;
            writer.disk_bytes += bytes.len() as u64;
            writer.pending.push_back(Pending {
                observation: Arc::new(event.summary()),
                offset,
                bytes: bytes.len(),
            });
            writer.disk_events += 1;
        }
        Ok(self.projection.write().expect("history projection lock").apply(event))
    }
    async fn flush(&self) -> Result<(), String> {
        match tokio::time::timeout(Duration::from_secs(15), self.flush_inner()).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(FlushError::Database(error))) => {
                self.mark_degraded();
                Err(error)
            }
            Ok(Err(FlushError::Capture(error))) => {
                self.mark_capture_gap().await;
                Err(error)
            }
            Err(_) => {
                self.mark_degraded();
                Err(sanitized(()))
            }
        }
    }
    fn states(&self) -> Vec<(AssetKey, Facet, AssetState)> {
        self.projection
            .read()
            .expect("history projection lock")
            .states
            .iter()
            .map(|((asset, facet), state)| (asset.clone(), *facet, state.clone()))
            .collect()
    }
    fn state(&self, asset: &AssetKey, facet: Facet) -> Option<AssetState> {
        self.projection
            .read()
            .expect("history projection lock")
            .states
            .get(&(asset.clone(), facet))
            .cloned()
    }
    async fn observation(
        &self,
        asset: &AssetKey,
        id: &str,
    ) -> Result<Option<Arc<Observation>>, String> {
        {
            let writer = self.writer.lock().await;
            if let Some(pending) = writer.pending.iter().find(|pending| {
                pending.observation.asset == *asset && pending.observation.event_id == id
            }) {
                // Keep offsets stable across local compaction; this lock never spans network I/O.
                return self.pending_event(pending).await.map(Some);
            }
        }
        let Some(pool) = &self.pool else {
            return Ok(self
                .projection
                .read()
                .expect("history projection lock")
                .events
                .iter()
                .find(|event| event.asset == *asset && event.event_id == id)
                .cloned());
        };
        if self.projection.read().expect("history projection lock").db_unavailable {
            return Err(sanitized(()));
        }
        let result = tokio::time::timeout(DATABASE_DEADLINE, async {
            let client = pool.get().await.map_err(sanitized)?;
            let row = client.query_opt("SELECT o.event,o.outcome,o.scheduled_slot,o.miss_reason FROM observations o JOIN assets a ON a.id=o.asset_id WHERE a.chain=$1 AND a.subject_kind=$2 AND a.address=$3 AND o.event_id=$4", &[&json_name(asset.chain),&json_name(asset.kind),&asset.address,&id]).await.map_err(sanitized)?;
            match row {
                Some(row) => {
                    let mut observation:Observation=serde_json::from_value(self.decode_json(row.get(0)).await?).map_err(sanitized)?;
                    observation.outcome=serde_json::from_value(serde_json::json!(row.get::<_,&str>(1))).map_err(sanitized)?;
                    observation.scheduled_slot=row.get(2);
                    observation.miss_reason=row.get(3);
                    Ok(Some(Arc::new(observation)))
                }
                None => Ok(None),
            }
        }).await.map_err(sanitized).and_then(|result|result);
        if result.is_err() {
            self.mark_degraded();
        }
        result
    }
    async fn history(
        &self,
        assets: &[AssetKey],
        since: DateTime<Utc>,
        before: Option<(DateTime<Utc>, String)>,
        limit: usize,
    ) -> Result<Vec<Arc<Observation>>, String> {
        let limit = limit.min(QUERY_LIMIT);
        let Some(pool) = &self.pool else {
            let projection = self.projection.read().expect("history projection lock");
            let mut events: Vec<_> = projection
                .events
                .iter()
                .filter(|event| {
                    assets.contains(&event.asset)
                        && event.observed_at >= since
                        && before.as_ref().is_none_or(|(time, id)| {
                            (event.observed_at, &event.event_id) < (*time, id)
                        })
                })
                .cloned()
                .collect();
            events.sort_by(|a, b| {
                b.observed_at.cmp(&a.observed_at).then_with(|| b.event_id.cmp(&a.event_id))
            });
            events.truncate(limit);
            return Ok(events);
        };
        if self.projection.read().expect("history projection lock").db_unavailable {
            return Err(sanitized(()));
        }
        let result = tokio::time::timeout(DATABASE_DEADLINE, async {
            let keys = serde_json::Value::Array(assets.iter().map(|asset|serde_json::json!({"chain":json_name(asset.chain),"kind":json_name(asset.kind),"address":asset.address})).collect());
            let (time,id) = before.map_or((None,None),|(time,id)|(Some(time),Some(id)));
            let since = since.max(DateTime::from_timestamp(-62_135_596_800,0).expect("year one"));
            let client = pool.get().await.map_err(sanitized)?;
            let rows = client.query(r#"SELECT jsonb_build_object(
                'event_id',o.event_id,'asset',jsonb_build_object('chain',a.chain,'kind',a.subject_kind,'address',a.address),
                'facet',o.facet,'observed_at',o.observed_at,'outcome',o.outcome,'miss_reason',o.miss_reason,
                'attestation_id',o.attestation_id,'block',o.block,'slot',o.slot,'source',o.source,
                'registry_hash',o.registry_hash,'ticker',o.ticker,'scheduled_slot',o.scheduled_slot)
                FROM jsonb_to_recordset($1::jsonb) AS k(chain text,kind text,address text)
                JOIN assets a ON a.chain=k.chain AND a.subject_kind=k.kind AND a.address=k.address
                JOIN observations o ON o.asset_id=a.id
                WHERE o.observed_at >= $2 AND ($3::timestamptz IS NULL OR (o.observed_at,o.event_id)<($3,$4::text))
                ORDER BY o.observed_at DESC,o.event_id DESC LIMIT $5"#, &[&keys,&since,&time,&id,&(limit as i64)]).await.map_err(sanitized)?;
            let mut events = Vec::with_capacity(rows.len());
            for row in rows { events.push(serde_json::from_value(row.get(0)).map_err(sanitized)?); }
            Ok(events)
        }).await.map_err(sanitized).and_then(|result|result);
        if result.is_err() {
            self.mark_degraded();
        }
        result
    }
    async fn inventory(&self, asset: &AssetKey) -> Result<(Option<DateTime<Utc>>, usize), String> {
        let Some(pool) = &self.pool else {
            return Ok(self
                .projection
                .read()
                .expect("history projection lock")
                .events
                .iter()
                .filter(|event| event.asset == *asset)
                .fold((None, 0), |(first, count), event| {
                    (
                        Some(first.map_or(event.observed_at, |first: DateTime<Utc>| {
                            first.min(event.observed_at)
                        })),
                        count + 1,
                    )
                }));
        };
        if self.projection.read().expect("history projection lock").db_unavailable {
            return Err(sanitized(()));
        }
        let result = tokio::time::timeout(DATABASE_DEADLINE, async {
            let client = pool.get().await.map_err(sanitized)?;
            let row = client.query_one("SELECT min(o.observed_at),count(*) FROM assets a JOIN observations o ON o.asset_id=a.id WHERE a.chain=$1 AND a.subject_kind=$2 AND a.address=$3", &[&json_name(asset.chain),&json_name(asset.kind),&asset.address]).await.map_err(sanitized)?;
            Ok((row.get(0),row.get::<_,i64>(1) as usize))
        }).await.map_err(sanitized).and_then(|result|result);
        if result.is_err() {
            self.mark_degraded();
        }
        result
    }
    fn degraded_since(&self) -> Option<DateTime<Utc>> {
        self.projection.read().expect("history projection lock").degraded_since
    }
    fn durable(&self) -> bool {
        self.pool.is_some()
    }
}

pub(crate) async fn backfill(
    state: &crate::app::context::Context,
    directory: &Path,
) -> Result<(), String> {
    use crate::domain::{
        asset_history::{Outcome, SubjectKind},
        attestation::{Attestation, MAX_ATTESTATION_BYTES},
        check::Verdict,
    };
    let mut entries = tokio::fs::read_dir(directory)
        .await
        .map_err(|_| "backfill directory unavailable".to_owned())?;
    let mut imported = 0usize;
    let mut duplicate = 0usize;
    let mut rejected = std::collections::BTreeMap::<&str, usize>::new();
    while let Some(entry) =
        entries.next_entry().await.map_err(|_| "backfill directory read failed".to_owned())?
    {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json")
            || !entry.file_type().await.is_ok_and(|kind| kind.is_file())
        {
            continue;
        }
        let Some(id) = path.file_stem().and_then(|value| value.to_str()) else {
            continue;
        };
        if id.len() != 64 || !id.bytes().all(|value| value.is_ascii_hexdigit()) {
            *rejected.entry("not an attestation filename").or_default() += 1;
            continue;
        }
        if !entry
            .metadata()
            .await
            .is_ok_and(|metadata| metadata.len() <= MAX_ATTESTATION_BYTES as u64)
        {
            *rejected.entry("oversized or unreadable").or_default() += 1;
            continue;
        }
        let bytes =
            tokio::fs::read(&path).await.map_err(|_| "backfill file read failed".to_owned())?;
        let Ok(attestation) = serde_json::from_slice::<Attestation>(&bytes) else {
            *rejected.entry("invalid document").or_default() += 1;
            continue;
        };
        let reason = if !id.eq_ignore_ascii_case(&attestation.id)
            || crate::domain::attestation::verify(&attestation).is_err()
        {
            Some("invalid signature or id")
        } else if !crate::app::attestation::signer_trusted(state, &attestation) {
            Some("untrusted signer")
        } else if attestation.dev != state.dev_signer {
            Some("environment mismatch")
        } else {
            None
        };
        if let Some(reason) = reason {
            *rejected.entry(reason).or_default() += 1;
            continue;
        }
        let Some(asset) =
            AssetKey::new(attestation.chain, SubjectKind::Pool, &attestation.pool.pool)
        else {
            *rejected.entry("invalid asset identity").or_default() += 1;
            continue;
        };
        let Ok(time) = DateTime::parse_from_rfc3339(&attestation.checked_at) else {
            *rejected.entry("invalid observation time").or_default() += 1;
            continue;
        };
        let outcome = match attestation.verdict {
            Verdict::Verified { .. } => Outcome::Verified,
            Verdict::Mismatch { .. } => Outcome::Mismatch,
            Verdict::NoMatch => Outcome::NoMatch,
            Verdict::Unknown { .. } => Outcome::Unknown,
        };
        state
            .attest_store
            .import_raw(id, bytes)
            .await
            .map_err(|_| "historical proof storage unavailable".to_owned())?;
        let json = serde_json::to_value(&attestation).map_err(sanitized)?;
        let mut event = crate::app::asset_history::event(
            state,
            asset,
            Facet::ContractMatch,
            outcome,
            time.with_timezone(&Utc),
            json.clone(),
        );
        event.event_id = format!("attestation:{}", attestation.id);
        event.attestation_id = Some(attestation.id.clone());
        event.signed_document = Some(json);
        event.source = "local_backfill".into();
        event.ticker = attestation.ticker.clone();
        event.registry_hash = Some(attestation.registry_hash.clone());
        event.block = attestation.block;
        event.slot = attestation.slot;
        if state.history.observation(&event.asset, &event.event_id).await?.is_some() {
            duplicate += 1;
            continue;
        }
        if state.history.append(event).await? {
            imported += 1;
        } else {
            duplicate += 1;
        }
    }
    state.history.flush().await?;
    tracing::info!(imported, duplicate, rejected = ?rejected, "history backfill complete");
    Ok(())
}

/// Failed supported identities only; no DEX discovery or publisher search.
pub(crate) async fn retry_failed(state: &crate::adapters::state::AppState) {
    for (asset, facet) in crate::app::asset_history::due_retries(&state.app) {
        let usage = state.rate_limiter.rpc_budget().snapshot();
        crate::app::asset_history::retry(
            &state.app,
            asset,
            facet,
            usage.est_cu_total >= usage.total_limit_cu,
        )
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        asset_history::{Outcome, SubjectKind},
        chain::Chain,
    };
    use sha2::{Digest, Sha256};
    fn observation(id: &str, outcome: Outcome, hour: i64) -> Observation {
        Observation {
            event_id: id.into(),
            asset: AssetKey::new(
                Chain::Base,
                SubjectKind::Pool,
                "0x0000000000000000000000000000000000000011",
            )
            .unwrap(),
            facet: Facet::ContractMatch,
            observed_at: DateTime::from_timestamp(1_790_000_000 + hour * 3600, 0).unwrap(),
            outcome,
            miss_reason: (outcome == Outcome::Miss).then(|| "timeout".into()),
            attestation_id: None,
            block: None,
            slot: None,
            source: "test".into(),
            registry_hash: None,
            ticker: None,
            evidence: serde_json::json!({}),
            signed_document: None,
            scheduled_slot: Some((1_790_000_000 + hour * 3600) / 3600),
        }
    }
    #[tokio::test]
    async fn migration_repairs_old_budget_counts_without_losing_executed_misses_or_history() {
        let Ok(url) = std::env::var("QED_TEST_DATABASE_URL") else {
            return;
        };
        let (client, connection) =
            tokio_postgres::connect(&url, tokio_postgres::NoTls).await.unwrap();
        tokio::spawn(async move {
            let _ = connection.await;
        });
        let schema = format!("qed_history_upgrade_{:x}", rand::random::<u64>());
        client
            .batch_execute(&format!("CREATE SCHEMA {schema}; SET search_path TO {schema};"))
            .await
            .unwrap();
        client.batch_execute(MIGRATION).await.unwrap();
        client.batch_execute("CREATE TABLE schema_migrations(version integer PRIMARY KEY,checksum text NOT NULL,applied_at timestamptz NOT NULL DEFAULT now())").await.unwrap();
        let checksum = crate::domain::attestation::hex_lower(&Sha256::digest(MIGRATION.as_bytes()));
        client
            .execute("INSERT INTO schema_migrations(version,checksum) VALUES(1,$1)", &[&checksum])
            .await
            .unwrap();
        let confirmed = observation("legacy-confirmed", Outcome::Verified, 0);
        let asset_id:i64=client.query_one("INSERT INTO assets(chain,subject_kind,address,first_seen_at) VALUES('base','pool',$1,$2) RETURNING id",&[&confirmed.asset.address,&confirmed.observed_at]).await.unwrap().get(0);
        let phantom_id:i64=client.query_one("INSERT INTO assets(chain,subject_kind,address,first_seen_at) VALUES('ethereum','pool',$1,$2) RETURNING id",&[&confirmed.asset.address,&confirmed.observed_at]).await.unwrap().get(0);
        let mut events = vec![(asset_id, confirmed.clone())];
        for hour in 1..=2 {
            let mut miss = observation(&format!("executed-{hour}"), Outcome::Miss, hour);
            miss.source = "background".into();
            events.push((asset_id, miss));
        }
        for index in 0..3 {
            let mut rejection = observation(&format!("budget-{index}"), Outcome::Miss, 3);
            rejection.source = "on_demand".into();
            rejection.scheduled_slot = None;
            rejection.miss_reason = Some("budget_rejection".into());
            events.push((asset_id, rejection));
        }
        for id in ["scheduled-budget", "scheduled-budget-duplicate"] {
            let mut rejection = observation(id, Outcome::Miss, 3);
            rejection.source = "background".into();
            rejection.miss_reason = Some("budget_rejection".into());
            events.push((asset_id, rejection));
        }
        let mut phantom = observation("phantom-probe", Outcome::Miss, 3);
        phantom.asset.chain = Chain::Ethereum;
        phantom.source = "background".into();
        phantom.miss_reason = Some("chain_probe_failed".into());
        events.push((phantom_id, phantom.clone()));
        let mut unknown = observation("legacy-eoa-unknown", Outcome::Unknown, 3);
        unknown.asset = AssetKey::new(
            Chain::Base,
            SubjectKind::Token,
            "0x0000000000000000000000000000000000000099",
        )
        .unwrap();
        unknown.facet = Facet::Guard;
        unknown.source = "on_demand".into();
        unknown.scheduled_slot = None;
        let unknown_id:i64=client.query_one("INSERT INTO assets(chain,subject_kind,address,first_seen_at) VALUES('base','token',$1,$2) RETURNING id",&[&unknown.asset.address,&unknown.observed_at]).await.unwrap().get(0);
        events.push((unknown_id, unknown.clone()));
        for (id, event) in &events {
            let kind = if event.facet == Facet::Guard { "guard" } else { "check" };
            client.execute("INSERT INTO observations(event_id,asset_id,observed_at,kind,facet,outcome,miss_reason,source,evidence,event) VALUES($1,$2,$3,$9,$10,$4,$5,$6,$7,$8)",
                &[&event.event_id,id,&event.observed_at,&json_name(event.outcome),&event.miss_reason,&event.source,&event.evidence,&serde_json::to_value(event).unwrap(),&kind,&json_name(event.facet)]).await.unwrap();
        }
        // Old large projections may be blob references; migration needs metadata, not S3.
        let old = serde_json::json!({"qed_storage_ref":"synthetic-unavailable-legacy-projection"});
        let empty = serde_json::json!({"last_confirmed":null,"latest":phantom,"consecutive_misses":1,"availability":null,"next_retry_at":null});
        for (id, projection) in [(asset_id, old), (phantom_id, empty)] {
            client.execute("INSERT INTO asset_state(asset_id,facet,projection) VALUES($1,'contract_match',$2)",&[&id,&projection]).await.unwrap();
        }
        client
            .execute(
                "INSERT INTO asset_state(asset_id,facet,projection) VALUES($1,'guard',$2)",
                &[
                    &unknown_id,
                    &serde_json::json!({"qed_storage_ref":"synthetic-unavailable-eoa-projection"}),
                ],
            )
            .await
            .unwrap();
        // Reproduce an already-applied history-first v3: storage/index did not exist.
        let v3 = include_str!("../../migrations/0003_bounded_history.sql");
        client.batch_execute(v3).await.unwrap();
        client
            .execute(
                "INSERT INTO schema_migrations(version,checksum) VALUES(3,$1)",
                &[&crate::domain::attestation::hex_lower(&Sha256::digest(v3.as_bytes()))],
            )
            .await
            .unwrap();
        let mut scoped = reqwest::Url::parse(&url).unwrap();
        scoped.query_pairs_mut().append_pair("options", &format!("-csearch_path={schema}"));
        let dir = tempfile::tempdir().unwrap();
        let store = HistoryStore::open(Some(scoped.as_str()), dir.path(), None).await.unwrap();
        assert!(store.degraded_since().is_none());
        let repaired = store.state(&confirmed.asset, Facet::ContractMatch).unwrap();
        assert_eq!(repaired.label(), "Unverified");
        assert_eq!(repaired.consecutive_misses, 3);
        assert_eq!(
            repaired.latest.as_ref().unwrap().scheduled_slot,
            Some(observation("", Outcome::Miss, 3).observed_at.timestamp() / 3600)
        );
        assert_eq!(repaired.last_confirmed.as_ref().unwrap().event_id, "legacy-confirmed");
        assert!(store.state(&phantom.asset, Facet::ContractMatch).is_none());
        assert!(store.state(&unknown.asset, Facet::Guard).is_none());
        assert_eq!(store.inventory(&unknown.asset).await.unwrap().1, 1);
        assert_eq!(
            store.observation(&unknown.asset, "legacy-eoa-unknown").await.unwrap().unwrap().outcome,
            Outcome::Unknown
        );
        assert_eq!(store.inventory(&confirmed.asset).await.unwrap().1, 8);
        let rejection = store.observation(&confirmed.asset, "budget-2").await.unwrap().unwrap();
        assert_eq!(rejection.outcome, Outcome::Rejected);
        assert!(rejection.scheduled_slot.is_none());
        let scheduled =
            store.observation(&confirmed.asset, "scheduled-budget").await.unwrap().unwrap();
        assert_eq!(scheduled.outcome, Outcome::Miss);
        assert!(scheduled.scheduled_slot.is_some());
        let storage = super::super::storage::DurableStore::open(
            scoped.as_str(),
            dir.path(),
            None,
            32 * 1024 * 1024,
        )
        .await
        .unwrap();
        storage
            .put("runtime/migration-order.json", br#"{"migration_order":"history_first"}"#.to_vec())
            .await
            .unwrap();
        storage.flush().await.unwrap();
        assert_eq!(
            storage.get("runtime/migration-order.json").await.unwrap().unwrap(),
            br#"{"migration_order":"history_first"}"#
        );
        assert!(
            client
                .query_one("SELECT to_regclass('durable_objects_recent_idx') IS NOT NULL", &[])
                .await
                .unwrap()
                .get::<_, bool>(0)
        );
        assert_eq!(
            store.inventory(&phantom.asset).await.unwrap().1,
            1,
            "attempt remains auditable without a phantom public state"
        );
        drop(store);
        client.batch_execute(&format!("DROP SCHEMA {schema} CASCADE")).await.unwrap();
    }
    #[tokio::test]
    async fn untracked_nonconfirming_on_demand_results_are_journal_only_until_tracked() {
        let store = HistoryStore::memory();
        let mut unsupported = observation("unsupported", Outcome::Unsupported, 0);
        unsupported.source = "on_demand".into();
        unsupported.scheduled_slot = None;
        store.append(unsupported.clone()).await.unwrap();
        assert!(store.states().is_empty());
        assert_eq!(store.inventory(&unsupported.asset).await.unwrap().1, 1);
        let mut confirmed = observation("guard-confirmed", Outcome::Allow, 1);
        confirmed.facet = Facet::Guard;
        store.append(confirmed).await.unwrap();
        unsupported.event_id = "tracked-unsupported".into();
        unsupported.observed_at += chrono::Duration::hours(2);
        store.append(unsupported.clone()).await.unwrap();
        assert_eq!(
            store.state(&unsupported.asset, unsupported.facet).unwrap().availability,
            Some(Outcome::Unsupported)
        );
    }
    #[tokio::test]
    async fn memory_isolates_facets_and_replay_cannot_rewind_or_double_count() {
        let store = HistoryStore::memory();
        let confirmed = observation("confirmed", Outcome::Verified, 1);
        store.append(confirmed.clone()).await.unwrap();
        let miss = observation("miss", Outcome::Miss, 2);
        assert!(store.append(miss.clone()).await.unwrap());
        assert!(!store.append(miss).await.unwrap());
        let mut powers = observation("powers", Outcome::ControlsComplete, 3);
        powers.facet = Facet::Powers;
        powers.asset.kind = SubjectKind::Token;
        store.append(powers).await.unwrap();
        store.append(observation("old", Outcome::Mismatch, 0)).await.unwrap();
        let states = store.states();
        let pool = states.iter().find(|(_, facet, _)| *facet == Facet::ContractMatch).unwrap();
        assert_eq!(pool.2.label(), "Verified");
        assert_eq!(pool.2.consecutive_misses, 1);
        assert_eq!(pool.2.last_confirmed.as_ref().unwrap().event_id, "confirmed");
        let history =
            store.history(&[confirmed.asset], DateTime::<Utc>::MIN_UTC, None, 10).await.unwrap();
        assert_eq!(
            history.iter().map(|event| event.event_id.as_str()).collect::<Vec<_>>(),
            ["miss", "confirmed", "old"]
        );
    }
    #[tokio::test]
    async fn postgres_atomic_idempotent_outbox_recovery_and_historical_order() {
        let Ok(url) = std::env::var("QED_TEST_DATABASE_URL") else {
            eprintln!("SKIP Postgres integration: QED_TEST_DATABASE_URL is unset");
            return;
        };
        let (client, connection) = tokio_postgres::connect(&url, tokio_postgres::NoTls)
            .await
            .expect("local test database connects");
        tokio::spawn(async move {
            let _ = connection.await;
        });
        let schema = format!("qed_history_test_{:x}", rand::random::<u64>());
        client.batch_execute(&format!("CREATE SCHEMA {schema}")).await.unwrap();
        let mut scoped = reqwest::Url::parse(&url).unwrap();
        scoped.query_pairs_mut().append_pair("options", &format!("-csearch_path={schema}"));
        let dir = tempfile::tempdir().unwrap();
        let mut down = scoped.clone();
        down.set_port(Some(1)).unwrap();
        let unavailable = HistoryStore::open(Some(down.as_str()), dir.path(), None).await.unwrap();
        assert!(unavailable.degraded_since().is_some());
        let confirmed = observation("pg-confirmed", Outcome::Verified, 2);
        unavailable.append(confirmed.clone()).await.unwrap();
        assert!(!unavailable.append(confirmed.clone()).await.unwrap());
        assert_eq!(
            unavailable.states()[0].2.label(),
            "Verified",
            "DB outage still serves local projection"
        );
        drop(unavailable);
        let recovered = HistoryStore::open(Some(scoped.as_str()), dir.path(), None).await.unwrap();
        recovered.flush().await.expect("outbox replay succeeds against recovered database");
        assert!(recovered.degraded_since().is_none());
        assert_eq!(
            recovered.states()[0].2.last_confirmed.as_ref().unwrap().event_id,
            "pg-confirmed"
        );
        recovered.append(observation("pg-old", Outcome::Mismatch, 1)).await.unwrap();
        recovered.flush().await.unwrap();
        assert_eq!(recovered.states()[0].2.label(), "Verified");
        let count: i64 = client
            .query_one(
                &format!(
                    "SELECT count(*) FROM {schema}.observations WHERE event_id='pg-confirmed'"
                ),
                &[],
            )
            .await
            .unwrap()
            .get(0);
        assert_eq!(count, 1);
        client.batch_execute(&format!("CREATE FUNCTION {schema}.reject_projection() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'synthetic atomicity fault'; END $$; CREATE TRIGGER history_test_fault BEFORE INSERT OR UPDATE ON {schema}.asset_state FOR EACH ROW EXECUTE FUNCTION {schema}.reject_projection()")).await.unwrap();
        recovered.append(observation("pg-atomic", Outcome::Miss, 3)).await.unwrap();
        assert!(recovered.flush().await.is_err());
        let count: i64 = client
            .query_one(
                &format!("SELECT count(*) FROM {schema}.observations WHERE event_id='pg-atomic'"),
                &[],
            )
            .await
            .unwrap()
            .get(0);
        assert_eq!(count, 0, "projection failure rolls observation back");
        client
            .batch_execute(&format!("DROP TRIGGER history_test_fault ON {schema}.asset_state"))
            .await
            .unwrap();
        recovered.flush().await.unwrap();
        let row = client
            .query_one(
                &format!(
                    "SELECT (projection->>'consecutive_misses')::int FROM {schema}.asset_state"
                ),
                &[],
            )
            .await
            .unwrap();
        assert_eq!(row.get::<_, i32>(0), 1);
        assert!(
            tokio::fs::read(dir.path().join("asset-history-outbox.jsonl"))
                .await
                .unwrap()
                .is_empty()
        );
        let mut untracked = observation("untracked-unsupported", Outcome::Unsupported, 4);
        untracked.asset.chain = Chain::Ethereum;
        untracked.source = "on_demand".into();
        untracked.scheduled_slot = None;
        recovered.append(untracked.clone()).await.unwrap();
        recovered.flush().await.unwrap();
        assert!(recovered.state(&untracked.asset, untracked.facet).is_none());
        assert_eq!(recovered.inventory(&untracked.asset).await.unwrap().1, 1);
        drop(recovered);
        tokio::fs::write(dir.path().join("asset-history-outbox.gap"), Utc::now().to_rfc3339())
            .await
            .unwrap();
        tokio::fs::write(dir.path().join("asset-history-outbox.jsonl"), b"truncated-json\n")
            .await
            .unwrap();
        let recovered = HistoryStore::open(Some(scoped.as_str()), dir.path(), None).await.unwrap();
        recovered.flush().await.unwrap();
        assert!(recovered.degraded_since().is_some(), "capture gap stays visible");
        assert!(
            recovered
                .append(observation("blocked-by-corrupt-spool", Outcome::Miss, 6))
                .await
                .is_err()
        );
        assert_eq!(recovered.inventory(&confirmed.asset).await.unwrap().1, 3);
        assert_eq!(
            recovered
                .history(std::slice::from_ref(&confirmed.asset), DateTime::<Utc>::MIN_UTC, None, 10)
                .await
                .unwrap()
                .len(),
            3
        );
        assert_eq!(
            recovered
                .observation(&confirmed.asset, &confirmed.event_id)
                .await
                .unwrap()
                .unwrap()
                .outcome,
            Outcome::Verified
        );
        let mut state = crate::adapters::state::AppState::for_tests(Vec::new(), Vec::new(), false);
        Arc::make_mut(&mut state.app).history = recovered;
        let response = tower::ServiceExt::oneshot(
            crate::adapters::web::router(state),
            axum::http::Request::get(format!("{}/history.json", confirmed.asset.path()))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(
            response.status(),
            axum::http::StatusCode::OK,
            "a capture gap alone cannot block exports"
        );
        let json: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap(),
        )
        .unwrap();
        assert!(json["recording_degraded_since"].is_string());
        assert_eq!(json["observations"].as_array().unwrap().len(), 3);
        client.batch_execute(&format!("DROP SCHEMA {schema} CASCADE")).await.unwrap();
    }
    #[tokio::test]
    async fn postgres_scale_20000_keeps_startup_bounded_and_pages_old_documents() {
        let Ok(url) = std::env::var("QED_TEST_DATABASE_URL") else {
            return;
        };
        // Isolate RSS from unrelated parallel tests. The child runs this test only.
        if std::env::var_os("QED_HISTORY_SCALE_CHILD").is_none() {
            let output=std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact","adapters::history::tests::postgres_scale_20000_keeps_startup_bounded_and_pages_old_documents","--nocapture"])
                .env("QED_HISTORY_SCALE_CHILD","1").output().unwrap();
            print!("{}", String::from_utf8_lossy(&output.stdout));
            assert!(
                output.status.success(),
                "isolated scale probe failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let (client, connection) =
            tokio_postgres::connect(&url, tokio_postgres::NoTls).await.unwrap();
        tokio::spawn(async move {
            let _ = connection.await;
        });
        let schema = format!("qed_history_scale_{:x}", rand::random::<u64>());
        client.batch_execute(&format!("CREATE SCHEMA {schema}")).await.unwrap();
        let mut scoped = reqwest::Url::parse(&url).unwrap();
        scoped.query_pairs_mut().append_pair("options", &format!("-csearch_path={schema}"));
        let dir = tempfile::tempdir().unwrap();
        let store = HistoryStore::open(Some(scoped.as_str()), dir.path(), None).await.unwrap();
        let mut first = observation("scale-first", Outcome::NoMatch, 0);
        first.source = "synthetic_scale".into();
        first.scheduled_slot = None;
        first.evidence = serde_json::json!({"synthetic_payload":"x".repeat(14*1024)});
        store.append(first.clone()).await.unwrap();
        store.flush().await.unwrap();
        let template = serde_json::to_value(&first).unwrap();
        client.execute(&format!(r#"INSERT INTO {schema}.observations
            (event_id,asset_id,observed_at,kind,facet,outcome,source,event)
            SELECT 'scale-'||lpad(n::text,5,'0'),a.id,$2::timestamptz+(n/10)*interval '1 second',
              'check','contract_match','no_match','synthetic_scale',
              $1::jsonb||jsonb_build_object('event_id','scale-'||lpad(n::text,5,'0'),'observed_at',$2::timestamptz+(n/10)*interval '1 second')
            FROM generate_series(1,20000) n CROSS JOIN {schema}.assets a"#),&[&template,&first.observed_at]).await.unwrap();
        store.append(observation("scale-last", Outcome::Mismatch, 7)).await.unwrap();
        let mut last_asset = first.asset.clone();
        for index in 0..600 {
            let mut event = observation(&format!("hydrate-{index}"), Outcome::NoMatch, 7);
            event.asset.address = format!("0x{:040x}", index + 1000);
            last_asset = event.asset.clone();
            store.append(event).await.unwrap();
        }
        store.flush().await.unwrap();
        drop(store);
        let rss = || -> u64 {
            std::fs::read_to_string("/proc/self/status")
                .ok()
                .and_then(|status| {
                    status.lines().find_map(|line| {
                        line.strip_prefix("VmRSS:")
                            .and_then(|value| value.split_whitespace().next()?.parse().ok())
                    })
                })
                .unwrap_or(0)
        };
        let before = rss();
        let started = std::time::Instant::now();
        let store = HistoryStore::open(Some(scoped.as_str()), dir.path(), None).await.unwrap();
        let elapsed = started.elapsed();
        let after = rss();
        assert!(store.degraded_since().is_none());
        assert_eq!(store.state(&first.asset, Facet::ContractMatch).unwrap().label(), "Mismatch");
        assert_eq!(store.states().len(), 601);
        assert_eq!(
            store
                .state(&last_asset, Facet::ContractMatch)
                .unwrap()
                .last_confirmed
                .as_ref()
                .unwrap()
                .event_id,
            "hydrate-599"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(3),
            "projection hydration exceeded startup deadline"
        );
        if cfg!(target_os = "linux") {
            assert!(
                after.saturating_sub(before) < 64 * 1024,
                "20k rows added unbounded resident memory"
            );
        }
        println!(
            "SCALE observations=20002 hydrate_ms={} rss_before_kib={before} rss_after_kib={after}",
            elapsed.as_millis()
        );
        assert_eq!(store.inventory(&first.asset).await.unwrap().1, 20002);
        let page = store
            .history(std::slice::from_ref(&first.asset), DateTime::<Utc>::MIN_UTC, None, usize::MAX)
            .await
            .unwrap();
        assert_eq!(page.len(), QUERY_LIMIT);
        assert_eq!(page[0].event_id, "scale-last");
        assert!(
            page.iter().all(|event| event.evidence.is_null() && event.signed_document.is_none())
        );
        let cursor = page.last().unwrap();
        let next = store
            .history(
                std::slice::from_ref(&first.asset),
                DateTime::<Utc>::MIN_UTC,
                Some((cursor.observed_at, cursor.event_id.clone())),
                QUERY_LIMIT,
            )
            .await
            .unwrap();
        assert!(!next.iter().any(|event| page.iter().any(|old| old.event_id == event.event_id)));
        assert!(
            next.iter().all(|event| (event.observed_at, &event.event_id)
                < (cursor.observed_at, &cursor.event_id))
        );
        let oldest = store.observation(&first.asset, "scale-00001").await.unwrap().unwrap();
        assert_eq!(
            oldest.evidence, first.evidence,
            "detail retrieves original document beyond the recent window"
        );
        drop(store);
        client.batch_execute(&format!("DROP SCHEMA {schema} CASCADE")).await.unwrap();
    }

    #[tokio::test]
    async fn backfill_accepts_only_explicitly_trusted_valid_history_and_is_idempotent() {
        use crate::app::{
            attestation::{expired_signed_test_attestation, signed_test_attestation},
            context::test_support::build_with_signers,
        };
        let trusted = expired_signed_test_attestation([9; 32], false);
        let state = build_with_signers(
            Vec::new(),
            Vec::new(),
            false,
            [7; 32],
            std::collections::HashSet::from([trusted.signer.clone()]),
        );
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::write(
            dir.path().join(format!("{}.json", trusted.id)),
            serde_json::to_vec(&trusted).unwrap(),
        )
        .await
        .unwrap();
        let untrusted = signed_test_attestation([8; 32], false);
        tokio::fs::write(
            dir.path().join(format!("{}.json", untrusted.id)),
            serde_json::to_vec(&untrusted).unwrap(),
        )
        .await
        .unwrap();
        backfill(&state, dir.path()).await.unwrap();
        backfill(&state, dir.path()).await.unwrap();
        let asset = AssetKey::new(trusted.chain, SubjectKind::Pool, &trusted.pool.pool).unwrap();
        let events =
            state.history.history(&[asset], DateTime::<Utc>::MIN_UTC, None, 10).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].observed_at,
            DateTime::parse_from_rfc3339(&trusted.checked_at).unwrap()
        );
        assert_eq!(events[0].outcome, Outcome::NoMatch);
        assert!(
            crate::app::attestation::get_certificate_async(&state, &trusted.id).await.is_some(),
            "expired imported proof remains historically verifiable"
        );
    }

    #[test]
    fn database_tls_rejects_invalid_certificate() {
        use rustls::{
            client::danger::ServerCertVerifier,
            pki_types::{ServerName, UnixTime},
        };
        let verifier = crate::adapters::database::tls_verifier().unwrap();
        let invalid = rustls::pki_types::CertificateDer::from(vec![0x30, 0x03, 0x00]);
        let result = verifier.verify_server_cert(
            &invalid,
            &[],
            &ServerName::try_from("database.example").unwrap(),
            &[],
            UnixTime::now(),
        );
        assert!(matches!(result, Err(rustls::Error::InvalidCertificate(_))));
    }

    #[tokio::test]
    async fn full_outbox_rejects_new_capture_and_preserves_existing_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("asset-history-outbox.jsonl");
        let mut store = HistoryStore::memory();
        Arc::get_mut(&mut store).unwrap().outbox = Some(path.clone());
        store.writer.lock().await.disk_events = OUTBOX_CAP - 1;
        store.append(observation("last-room", Outcome::Miss, 1)).await.unwrap();
        let before = tokio::fs::read(&path).await.unwrap();
        assert!(store.append(observation("overflow", Outcome::Miss, 2)).await.is_err());
        assert_eq!(tokio::fs::read(&path).await.unwrap(), before);
        assert!(store.degraded_since().is_some());
        assert!(
            store
                .observation(&observation("overflow", Outcome::Miss, 2).asset, "overflow")
                .await
                .unwrap()
                .is_none()
        );
        assert!(path.with_extension("gap").exists());
    }

    #[tokio::test]
    async fn corrupt_outbox_line_is_preserved_and_capture_gap_survives_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("asset-history-outbox.jsonl");
        let bytes = format!(
            "{}\nnot-json\n",
            serde_json::to_string(&observation("retained", Outcome::Verified, 1)).unwrap()
        );
        tokio::fs::write(&path, &bytes).await.unwrap();
        for _ in 0..2 {
            let store = HistoryStore::open(
                Some("postgres://qed:synthetic@127.0.0.1:1/qed"),
                dir.path(),
                None,
            )
            .await
            .unwrap();
            assert!(store.degraded_since().is_some());
            assert_eq!(store.states()[0].2.label(), "Verified");
            assert!(store.append(observation("new", Outcome::Miss, 2)).await.is_err());
            assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), bytes);
        }
    }
}
