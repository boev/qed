use crate::domain::{
    attestation::{Attestation, Read},
    chain::Chain,
    pool::{IndexedPool, PoolError, PoolInfo, TokenMeta, WalletHolding},
    powers::SourceVerified,
    registry::{Entry, Registry},
    statement::{StatementHolding, StatementPosition},
};
use async_trait::async_trait;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    collections::HashMap,
    future::Future,
    hash::Hash,
    io,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
};

#[derive(Debug, Clone, Default)]
pub(crate) struct ReadLog {
    pub(crate) reads: Vec<Read>,
    pub(crate) reads_truncated: bool,
    pub(crate) block: Option<u64>,
    pub(crate) slot: Option<u64>,
    pub(crate) raw_result_bytes: usize,
}

pub(crate) const MAX_READ_LOG_ENTRIES: usize = 256;
pub(crate) const MAX_RAW_RESULT_BYTES: usize = 64 * 1024;

tokio::task_local! {
    static ACTIVE_READ_LOG: RefCell<ReadLog>;
}

pub(crate) async fn capture_reads<F, T>(future: F) -> (T, ReadLog)
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
pub(crate) const RPC_BUDGET_REJECTED: u8 = 1;
pub(crate) const RPC_CRAWLER_BLOCKED: u8 = 2;

tokio::task_local! {
    static RPC_REQUEST_CONTEXT: RpcRequestContext;
}

#[derive(Clone)]
pub(crate) struct RpcRequestContext {
    pub(crate) on_demand: bool,
    pub(crate) crawler: bool,
    flags: Arc<AtomicU8>,
}

impl RpcRequestContext {
    pub(crate) fn new(on_demand: bool, crawler: bool) -> Self {
        Self { on_demand, crawler, flags: Arc::new(AtomicU8::new(0)) }
    }

    pub(crate) fn mark(&self, flag: u8) {
        self.flags.fetch_or(flag, Ordering::Relaxed);
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct RpcRequestOutcome(u8);

impl RpcRequestOutcome {
    pub(crate) fn budget_rejected(self) -> bool {
        self.0 & RPC_BUDGET_REJECTED != 0
    }

    pub(crate) fn crawler_blocked(self) -> bool {
        self.0 & RPC_CRAWLER_BLOCKED != 0
    }
}

pub(crate) async fn with_rpc_request_context<F, T>(
    context: RpcRequestContext,
    future: F,
) -> (T, RpcRequestOutcome)
where
    F: Future<Output = T>,
{
    let flags = Arc::clone(&context.flags);
    let result = RPC_REQUEST_CONTEXT.scope(context, future).await;
    (result, RpcRequestOutcome(flags.load(Ordering::Relaxed)))
}

pub(crate) fn current_rpc_request_context() -> Option<RpcRequestContext> {
    RPC_REQUEST_CONTEXT.try_with(Clone::clone).ok()
}

pub(crate) fn rpc_reads_rejected() -> bool {
    RPC_REQUEST_CONTEXT
        .try_with(|context| {
            context.flags.load(Ordering::Relaxed) & (RPC_BUDGET_REJECTED | RPC_CRAWLER_BLOCKED) != 0
        })
        .unwrap_or(false)
}

pub(crate) fn rpc_budget_rejected() -> bool {
    RPC_REQUEST_CONTEXT
        .try_with(|context| context.flags.load(Ordering::Relaxed) & RPC_BUDGET_REJECTED != 0)
        .unwrap_or(false)
}

pub(crate) fn rpc_crawler_blocked() -> bool {
    RPC_REQUEST_CONTEXT
        .try_with(|context| context.flags.load(Ordering::Relaxed) & RPC_CRAWLER_BLOCKED != 0)
        .unwrap_or(false)
}

pub(crate) fn record_read(
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
        let result_hash = crate::domain::attestation::hex_lower(&hasher.finalize());
        let mut log = cell.borrow_mut();
        if method == "eth_blockNumber" {
            if log.block.is_none() {
                log.block = block;
            }
            append_read(
                &mut log,
                Read {
                    method: method.to_owned(),
                    params,
                    result_hash,
                    raw_result: None,
                    block,
                    slot,
                },
            );
            return;
        }
        if method == "getSlot" {
            if log.slot.is_none() {
                log.slot = slot;
            }
            append_read(
                &mut log,
                Read {
                    method: method.to_owned(),
                    params,
                    result_hash,
                    raw_result: None,
                    block,
                    slot,
                },
            );
            return;
        }
        if log.block.is_none() {
            log.block = block;
        }
        if log.slot.is_none() {
            log.slot = slot;
        }
        if log.reads.len() >= MAX_READ_LOG_ENTRIES {
            log.reads_truncated = true;
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
        append_read(
            &mut log,
            Read { method: method.to_owned(), params, result_hash, raw_result, block, slot },
        );
    });
}
fn append_read(log: &mut ReadLog, read: Read) {
    if log.reads.len() >= MAX_READ_LOG_ENTRIES {
        log.reads_truncated = true;
        return;
    }
    log.reads.push(read);
}

#[derive(Debug, Clone)]
pub(crate) struct WalletRestrictionReport {
    pub(crate) restrictions: Vec<crate::domain::powers::Reason>,
    pub(crate) complete: bool,
    pub(crate) applicable: bool,
}

#[async_trait]
pub(crate) trait ChainReader: Send + Sync {
    fn chain(&self) -> Chain;

    async fn read_pool(&self, address: &str) -> Result<PoolInfo, PoolError>;

    async fn read_v4_pool(&self, _pool_id: &str) -> Result<(PoolInfo, Vec<String>), PoolError> {
        Err(PoolError::Unknown("v4 pools are unsupported on this chain".to_owned()))
    }

    async fn token_meta(&self, _address: &str) -> Result<TokenMeta, PoolError> {
        Err(PoolError::Reader("token metadata is unsupported".to_owned()))
    }

    async fn pools_for_token(
        &self,
        _token: &str,
        _candidate_quotes: &[String],
    ) -> Result<Vec<PoolInfo>, PoolError> {
        Err(PoolError::Reader("token pool discovery is unsupported".to_owned()))
    }

    async fn wallet_holdings(
        &self,
        _owner: &str,
        _known_tokens: &[String],
    ) -> Result<Vec<WalletHolding>, PoolError> {
        Err(PoolError::Reader("wallet holdings are unsupported".to_owned()))
    }
    async fn statement_holdings(
        &self,
        _owner: &str,
        _entries: &[Entry],
        _block: Option<u64>,
    ) -> Result<(Vec<StatementHolding>, StatementPosition), PoolError> {
        Err(PoolError::Reader("statement balance reads are unsupported on this chain".to_owned()))
    }

    async fn power_facts(
        &self,
        _address: &str,
    ) -> Result<crate::domain::powers::PowerFacts, PoolError> {
        Err(PoolError::Reader("token powers are unsupported on this chain".to_owned()))
    }

    async fn wallet_restrictions(
        &self,
        _contract: &str,
        _wallet: &str,
        _sanctions_list: Option<&str>,
    ) -> Result<WalletRestrictionReport, PoolError> {
        Err(PoolError::Reader(
            "active wallet restrictions are unsupported on this chain".to_owned(),
        ))
    }

    async fn code_at(&self, _address: &str) -> Result<Vec<u8>, PoolError> {
        Err(PoolError::CodeLookupUnsupported)
    }

    async fn record_position(&self) -> Result<(), PoolError> {
        Ok(())
    }
}

#[async_trait]
pub(crate) trait IssuerRegistry: Send + Sync {
    async fn snapshot(&self) -> std::sync::Arc<Registry>;
}

pub(crate) trait Signer: Send + Sync {
    fn public_key(&self) -> String;

    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, String>;
}

pub(crate) trait Clock: Send + Sync {
    fn now(&self) -> chrono::DateTime<chrono::Utc>;
}

#[async_trait]
pub(crate) trait Cache<K, V>: Send + Sync
where
    K: Clone + Eq + Hash + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    async fn get(&self, key: &K) -> Option<V>;

    async fn insert(&self, key: K, value: V);

    async fn invalidate(&self, key: &K);

    fn invalidate_all(&self);

    async fn get_or_insert(&self, key: K, value: V) -> V;
}

#[async_trait]
pub(crate) trait AttestationStore: Send + Sync {
    async fn load_recent(&self) -> io::Result<HashMap<String, Attestation>>;
    async fn latest_for_pool(&self, chain: &str, pool: &str) -> io::Result<Option<Attestation>>;
    async fn persist(&self, attestation: &Attestation) -> io::Result<()>;
    /// Import original bytes without replacing an existing record or retention tombstone.
    async fn import_raw(&self, id: &str, bytes: Vec<u8>) -> io::Result<()>;
    async fn get(&self, id: &str) -> io::Result<Option<Attestation>>;
    /// Original JSON document bytes, before any parsing or chain-name normalization.
    async fn raw_json(&self, id: &str) -> io::Result<Option<Vec<u8>>>;
    async fn prune_expired(&self, older_than: chrono::DateTime<chrono::Utc>) -> io::Result<()>;
    async fn quarantine(&self, id: &str) -> io::Result<()>;
}

/// Public, wallet-free observations: metadata projection in memory, bounded database reads.
#[async_trait]
pub(crate) trait AssetHistoryStore: Send + Sync {
    async fn append(
        &self,
        event: crate::domain::asset_history::Observation,
    ) -> Result<bool, String>;
    async fn flush(&self) -> Result<(), String>;
    fn states(
        &self,
    ) -> Vec<(
        crate::domain::asset_history::AssetKey,
        crate::domain::asset_history::Facet,
        crate::domain::asset_history::AssetState,
    )>;
    fn state(
        &self,
        asset: &crate::domain::asset_history::AssetKey,
        facet: crate::domain::asset_history::Facet,
    ) -> Option<crate::domain::asset_history::AssetState>;
    async fn observation(
        &self,
        asset: &crate::domain::asset_history::AssetKey,
        id: &str,
    ) -> Result<Option<std::sync::Arc<crate::domain::asset_history::Observation>>, String>;
    async fn history(
        &self,
        assets: &[crate::domain::asset_history::AssetKey],
        since: chrono::DateTime<chrono::Utc>,
        before: Option<(chrono::DateTime<chrono::Utc>, String)>,
        limit: usize,
    ) -> Result<Vec<std::sync::Arc<crate::domain::asset_history::Observation>>, String>;
    async fn inventory(
        &self,
        asset: &crate::domain::asset_history::AssetKey,
    ) -> Result<(Option<chrono::DateTime<chrono::Utc>>, usize), String>;
    fn degraded_since(&self) -> Option<chrono::DateTime<chrono::Utc>>;
    fn durable(&self) -> bool;
}

#[async_trait]
pub(crate) trait PoolIndex: Send + Sync {
    async fn candidate_pools(
        &self,
        chain: Chain,
        token: &str,
        attestations: &HashMap<String, Attestation>,
    ) -> Vec<String>;
    async fn known_pools(&self, attestations: &HashMap<String, Attestation>) -> Vec<IndexedPool>;
    async fn warm_tickers(&self) -> Vec<String>;
}

#[async_trait]
pub(crate) trait SourceVerifier: Send + Sync {
    async fn verify_solana(&self, program_id: &str) -> SourceVerified;
    async fn verify_evm(&self, chain: Chain, contract: &str) -> SourceVerified;
}
#[cfg(test)]
mod tests {
    use super::{MAX_READ_LOG_ENTRIES, capture_reads, record_read};
    use serde_json::json;

    #[tokio::test]
    async fn read_log_marks_only_omitted_entries_as_truncated() {
        let (_, exact) = capture_reads(async {
            for index in 0..MAX_READ_LOG_ENTRIES {
                record_read(
                    "getBalance",
                    json!({ "index": index }),
                    &json!({ "balance": index }),
                    false,
                    None,
                    None,
                );
            }
        })
        .await;
        assert_eq!(exact.reads.len(), MAX_READ_LOG_ENTRIES);
        assert!(!exact.reads_truncated);

        let (_, overflow) = capture_reads(async {
            for index in 0..=MAX_READ_LOG_ENTRIES {
                record_read(
                    "getBalance",
                    json!({ "index": index }),
                    &json!({ "balance": index }),
                    false,
                    None,
                    None,
                );
            }
        })
        .await;
        assert_eq!(overflow.reads.len(), MAX_READ_LOG_ENTRIES);
        assert!(overflow.reads_truncated);
    }
}
