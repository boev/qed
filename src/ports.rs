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
use std::{cell::RefCell, collections::HashMap, future::Future, hash::Hash, io};

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
    async fn get(&self, id: &str) -> io::Result<Option<Attestation>>;
    async fn prune_expired(&self, older_than: chrono::DateTime<chrono::Utc>) -> io::Result<()>;
    async fn quarantine(&self, id: &str) -> io::Result<()>;
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
