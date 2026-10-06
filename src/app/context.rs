use crate::{
    domain::{
        attestation::Attestation, chain::Chain, check::CheckResult, powers::PowersRecord,
        statement::Statement,
    },
    ports::{
        AttestationStore, Cache, ChainReader, Clock, IssuerRegistry, PoolIndex, Signer,
        SourceVerifier,
    },
};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, RwLock, Weak, atomic::AtomicU64},
};
use tokio::sync::Semaphore;

pub(crate) type PowersCacheKey = (Chain, String, u64);
pub(crate) type CheckFlight = Weak<tokio::sync::watch::Sender<Option<CheckResult>>>;
pub(crate) type CheckInFlight = tokio::sync::Mutex<HashMap<String, CheckFlight>>;

#[derive(Clone)]
pub(crate) struct CachedCheckResult {
    pub(crate) registry_version: u64,
    pub(crate) result: CheckResult,
}

#[derive(Clone)]
pub(crate) struct Context {
    pub(crate) registry: Arc<dyn IssuerRegistry>,
    pub(crate) readers: Arc<Vec<Box<dyn ChainReader>>>,
    pub(crate) check_cache: Arc<dyn Cache<String, CachedCheckResult>>,
    pub(crate) statement_cache: Arc<dyn Cache<String, Statement>>,
    pub(crate) powers_cache: Arc<dyn Cache<PowersCacheKey, PowersRecord>>,
    pub(crate) powers_retry_cache: Arc<dyn Cache<PowersCacheKey, PowersRecord>>,
    pub(crate) powers_failure_cache: Arc<dyn Cache<PowersCacheKey, ()>>,
    pub(crate) powers_locks: Arc<dyn Cache<PowersCacheKey, Arc<tokio::sync::Mutex<()>>>>,
    pub(crate) powers_prefetching: Arc<tokio::sync::Mutex<HashSet<PowersCacheKey>>>,
    pub(crate) check_inflight: Arc<CheckInFlight>,
    pub(crate) pool_index: Arc<dyn PoolIndex>,
    pub(crate) attestations: Arc<RwLock<HashMap<String, Attestation>>>,
    pub(crate) signer: Arc<dyn Signer>,
    pub(crate) trusted_signers: Arc<HashSet<String>>,
    pub(crate) dev_signer: bool,
    pub(crate) attest_store: Arc<dyn AttestationStore>,
    pub(crate) registry_hash: Arc<RwLock<String>>,
    pub(crate) registry_version: Arc<AtomicU64>,
    pub(crate) source_verifier: Arc<dyn SourceVerifier>,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) powers_prefetch_concurrency: Arc<Semaphore>,
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use crate::{
        domain::{attestation::Attestation, registry::Registry},
        ports::{
            AttestationStore, Cache, ChainReader, Clock, IssuerRegistry, PoolIndex, Signer,
            SourceVerifier,
        },
    };
    use async_trait::async_trait;
    use chrono::{DateTime, Utc};
    use ed25519_dalek::{Signer as Ed25519Signer, SigningKey};
    use std::{
        collections::{HashMap, HashSet},
        hash::Hash,
        ops::Deref,
        sync::{Arc, Mutex, MutexGuard, RwLock},
    };

    fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
        value.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    pub(crate) struct TestContext {
        context: Context,
        pub(crate) registry: Arc<tokio::sync::RwLock<Arc<Registry>>>,
        pub(crate) pool_index: Arc<MemoryPoolIndex>,
    }

    impl Deref for TestContext {
        type Target = Context;

        fn deref(&self) -> &Self::Target {
            &self.context
        }
    }

    pub(crate) fn build(
        readers: Vec<Box<dyn ChainReader>>,
        entries: Registry,
        dev_signer: bool,
    ) -> TestContext {
        build_with_signers(readers, entries, dev_signer, [7; 32], HashSet::new())
    }

    pub(crate) fn build_with_signers(
        readers: Vec<Box<dyn ChainReader>>,
        entries: Registry,
        dev_signer: bool,
        seed: [u8; 32],
        trusted_signers: HashSet<String>,
    ) -> TestContext {
        let registry = Arc::new(tokio::sync::RwLock::new(Arc::new(entries)));
        let pool_index = Arc::new(MemoryPoolIndex::default());
        let store = Arc::new(MemoryAttestationStore::default());
        let signer = Arc::new(TestSigner(SigningKey::from_bytes(&seed)));
        let context = Context {
            registry: Arc::new(TestRegistry(Arc::clone(&registry))),
            readers: Arc::new(readers),
            check_cache: Arc::new(MemoryCache::<String, CachedCheckResult>::default()),
            statement_cache: Arc::new(MemoryCache::<String, Statement>::default()),
            powers_cache: Arc::new(MemoryCache::<PowersCacheKey, PowersRecord>::default()),
            powers_retry_cache: Arc::new(MemoryCache::<PowersCacheKey, PowersRecord>::default()),
            powers_failure_cache: Arc::new(MemoryCache::<PowersCacheKey, ()>::default()),
            powers_locks: Arc::new(
                MemoryCache::<PowersCacheKey, Arc<tokio::sync::Mutex<()>>>::default(),
            ),
            powers_prefetching: Arc::new(tokio::sync::Mutex::new(HashSet::new())),
            check_inflight: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            pool_index: pool_index.clone(),
            attestations: Arc::new(RwLock::new(HashMap::new())),
            signer,
            trusted_signers: Arc::new(trusted_signers),
            dev_signer,
            attest_store: store.clone(),
            registry_hash: Arc::new(RwLock::new(String::new())),
            registry_version: Arc::new(AtomicU64::new(0)),
            source_verifier: Arc::new(TestSourceVerifier),
            clock: Arc::new(TestClock),
            powers_prefetch_concurrency: Arc::new(Semaphore::new(2)),
        };
        TestContext { context, registry, pool_index }
    }

    struct TestRegistry(Arc<tokio::sync::RwLock<Arc<Registry>>>);

    #[async_trait]
    impl IssuerRegistry for TestRegistry {
        async fn snapshot(&self) -> Arc<Registry> {
            Arc::clone(&*self.0.read().await)
        }
    }

    struct TestSigner(SigningKey);

    impl Signer for TestSigner {
        fn public_key(&self) -> String {
            bs58::encode(self.0.verifying_key().as_bytes()).into_string()
        }

        fn sign(&self, message: &[u8]) -> Result<Vec<u8>, String> {
            Ok(Ed25519Signer::sign(&self.0, message).to_bytes().to_vec())
        }
    }

    #[derive(Default)]
    pub(crate) struct MemoryPoolIndex {
        candidates: Mutex<Vec<(Chain, String, String)>>,
    }

    impl MemoryPoolIndex {
        pub(crate) fn add_candidate(&self, chain: Chain, token: &str, pool: &str) {
            lock(&self.candidates).push((chain, token.to_owned(), pool.to_owned()));
        }
    }

    #[async_trait]
    impl PoolIndex for MemoryPoolIndex {
        async fn candidate_pools(
            &self,
            chain: Chain,
            token: &str,
            _attestations: &HashMap<String, Attestation>,
        ) -> Vec<String> {
            lock(&self.candidates)
                .iter()
                .filter(|(candidate_chain, candidate_token, _)| {
                    *candidate_chain == chain
                        && if chain == Chain::Solana {
                            candidate_token == token
                        } else {
                            candidate_token.eq_ignore_ascii_case(token)
                        }
                })
                .map(|(_, _, pool)| pool.clone())
                .collect()
        }

        async fn known_pools(
            &self,
            _attestations: &HashMap<String, Attestation>,
        ) -> Vec<crate::domain::pool::IndexedPool> {
            Vec::new()
        }

        async fn warm_tickers(&self) -> Vec<String> {
            Vec::new()
        }
    }

    #[derive(Default)]
    pub(crate) struct MemoryAttestationStore(Mutex<HashMap<String, Attestation>>);

    #[async_trait]
    impl AttestationStore for MemoryAttestationStore {
        async fn load_recent(&self) -> std::io::Result<HashMap<String, Attestation>> {
            Ok(lock(&self.0).clone())
        }

        async fn latest_for_pool(
            &self,
            chain: &str,
            pool: &str,
        ) -> std::io::Result<Option<Attestation>> {
            Ok(lock(&self.0)
                .values()
                .filter(|attestation| {
                    let matches_address = |value: &str| {
                        if chain.eq_ignore_ascii_case("solana") {
                            value == pool
                        } else {
                            value.eq_ignore_ascii_case(pool)
                        }
                    };
                    attestation.chain.to_string().eq_ignore_ascii_case(chain)
                        && (matches_address(&attestation.subject)
                            || matches_address(&attestation.pool.pool))
                })
                .max_by(|left, right| left.checked_at.cmp(&right.checked_at))
                .cloned())
        }

        async fn persist(&self, attestation: &Attestation) -> std::io::Result<()> {
            lock(&self.0).insert(attestation.id.clone(), attestation.clone());
            Ok(())
        }

        async fn get(&self, id: &str) -> std::io::Result<Option<Attestation>> {
            Ok(lock(&self.0).get(id).cloned())
        }

        async fn prune_expired(&self, older_than: DateTime<Utc>) -> std::io::Result<()> {
            lock(&self.0).retain(|_, attestation| {
                DateTime::parse_from_rfc3339(&attestation.expires_at)
                    .ok()
                    .is_some_and(|expires| expires >= older_than)
            });
            Ok(())
        }

        async fn quarantine(&self, id: &str) -> std::io::Result<()> {
            lock(&self.0).remove(id);
            Ok(())
        }
    }

    struct MemoryCache<K, V>(Mutex<HashMap<K, V>>);

    impl<K, V> Default for MemoryCache<K, V> {
        fn default() -> Self {
            Self(Mutex::new(HashMap::new()))
        }
    }

    #[async_trait]
    impl<K, V> Cache<K, V> for MemoryCache<K, V>
    where
        K: Clone + Eq + Hash + Send + Sync + 'static,
        V: Clone + Send + Sync + 'static,
    {
        async fn get(&self, key: &K) -> Option<V> {
            lock(&self.0).get(key).cloned()
        }

        async fn insert(&self, key: K, value: V) {
            lock(&self.0).insert(key, value);
        }

        async fn invalidate(&self, key: &K) {
            lock(&self.0).remove(key);
        }
        fn invalidate_all(&self) {
            lock(&self.0).clear();
        }

        async fn get_or_insert(&self, key: K, value: V) -> V {
            lock(&self.0).entry(key).or_insert(value).clone()
        }
    }

    struct TestClock;

    impl Clock for TestClock {
        fn now(&self) -> DateTime<Utc> {
            Utc::now()
        }
    }

    pub(crate) struct TestSourceVerifier;

    #[async_trait]
    impl SourceVerifier for TestSourceVerifier {
        async fn verify_solana(&self, _program_id: &str) -> crate::domain::powers::SourceVerified {
            crate::domain::powers::SourceVerified::Unavailable
        }

        async fn verify_evm(
            &self,
            _chain: Chain,
            _contract: &str,
        ) -> crate::domain::powers::SourceVerified {
            crate::domain::powers::SourceVerified::Unavailable
        }
    }
}
