use crate::{
    app::{context::Context, powers},
    domain::{
        attestation::MAX_ATTESTATION_BYTES,
        chain::Chain,
        check::{self, Verdict},
        guard::{
            GuardDocument, GuardPool, GuardQuote, GuardReason, GuardSource, GuardVerdict,
            GuardWalletCheck, IdentityStatus, SourceStatus, WalletCheckStatus,
        },
        pool::{IndexedPool, PoolInfo, TokenMeta, TokenSide},
        powers::{PowersRecord, SourceVerified},
        registry::{self, Entry},
    },
    ports::{ChainReader, ReadLog, capture_reads},
};
use chrono::SecondsFormat;
use futures_util::StreamExt;
use std::time::Duration;
use thiserror::Error;

const GUARD_DEADLINE: Duration = Duration::from_secs(15);
const MAX_GUARD_ADDRESS_CHARS: usize = 66;
const MAX_GUARD_POOLS: usize = 16;
const MAX_GUARD_READS: usize = 256;
const MAX_PUBLISHER_METADATA_CANDIDATES: usize = 8;
#[derive(Debug, Error)]
pub(crate) enum GuardError {
    #[error("guard address is invalid for the selected chain")]
    InvalidAddress,
    #[error("wallet address is invalid for the selected chain")]
    InvalidWallet,
    #[error("no reader is configured for the selected chain")]
    ReaderUnavailable,
    #[error("guard reads exceeded the 15 second deadline")]
    DeadlineExceeded,
    #[error("guard signing failed")]
    Signing,
}

pub(crate) async fn create(
    state: &Context,
    address: &str,
    chain: Chain,
    wallet: Option<&str>,
) -> Result<GuardDocument, GuardError> {
    let address = canonical_input(address, chain)?;
    if wallet.is_some_and(|wallet| wallet.len() > Chain::MAX_SOLANA_ADDRESS_CHARS) {
        return Err(GuardError::InvalidWallet);
    }
    let wallet = wallet
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|wallet| canonical_wallet(wallet, chain))
        .transpose()?;
    tokio::time::timeout(GUARD_DEADLINE, create_inner(state, &address, chain, wallet.as_deref()))
        .await
        .map_err(|_| GuardError::DeadlineExceeded)?
}

async fn create_inner(
    state: &Context,
    address: &str,
    chain: Chain,
    wallet: Option<&str>,
) -> Result<GuardDocument, GuardError> {
    let reader = state
        .readers
        .iter()
        .find(|reader| reader.chain() == chain)
        .ok_or(GuardError::ReaderUnavailable)?;
    let registry = state.registry.snapshot().await;
    let attestations = state
        .attestations
        .read()
        .map(|value| value.clone())
        .unwrap_or_else(|poisoned| poisoned.into_inner().clone());
    let known_pools = state.pool_index.known_pools(&attestations).await;

    let (subject, metadata, pools, subject_type, pool_unavailable, mut read_log) = resolve_subject(
        reader.as_ref(),
        state.clock.as_ref(),
        chain,
        address,
        &registry,
        &known_pools,
    )
    .await;
    let ((publisher_metadata, publisher_metadata_unavailable), publisher_metadata_log) =
        capture_reads(load_publisher_metadata(
            state.readers.as_slice(),
            chain,
            &subject,
            metadata.as_ref(),
            std::sync::Arc::clone(&registry),
        ))
        .await;
    append_read_log(&mut read_log, publisher_metadata_log);
    let identity = crate::domain::guard::identify_with_contract_metadata(
        chain,
        &subject,
        metadata.as_ref(),
        &registry,
        &publisher_metadata,
    );
    let power_record =
        if subject.is_empty() { None } else { powers::inspect(state, chain, &subject).await.ok() };
    let (source, provider) = source_status(chain, power_record.as_ref());

    let (wallet_check, wallet_log) = if let Some(wallet_address) = wallet.as_deref() {
        if subject.is_empty() {
            (
                Some(GuardWalletCheck {
                    status: WalletCheckStatus::Unavailable,
                    restrictions: Vec::new(),
                }),
                ReadLog::default(),
            )
        } else {
            let sanctions_list =
                power_record.as_ref().and_then(|record| record.sanctions_list.as_deref());
            let (result, wallet_log) =
                capture_reads(reader.wallet_restrictions(&subject, wallet_address, sanctions_list))
                    .await;
            let check = match result {
                Ok(report) => GuardWalletCheck {
                    status: if !report.complete {
                        WalletCheckStatus::Unavailable
                    } else if report.applicable {
                        WalletCheckStatus::Checked
                    } else {
                        WalletCheckStatus::NotApplicable
                    },
                    restrictions: report
                        .restrictions
                        .into_iter()
                        .map(|reason| GuardReason { code: reason.code, detail: reason.detail })
                        .collect(),
                },
                Err(_) => GuardWalletCheck {
                    status: WalletCheckStatus::Unavailable,
                    restrictions: Vec::new(),
                },
            };
            (Some(check), wallet_log)
        }
    } else {
        (None, ReadLog::default())
    };
    append_read_log(&mut read_log, wallet_log);

    let (mut verdict, mut reasons) = if pool_unavailable {
        (
            crate::domain::guard::GuardVerdict::Unknown,
            vec![GuardReason {
                code: "pool_unavailable".to_owned(),
                detail: "QED could not read this pool — retry".to_owned(),
            }],
        )
    } else {
        crate::domain::guard::evaluate(
            &identity,
            power_record.as_ref(),
            wallet_check.as_ref(),
            source,
        )
    };
    if publisher_metadata_unavailable && identity.status != IdentityStatus::Mismatch {
        if verdict == GuardVerdict::Allow {
            verdict = GuardVerdict::Unknown;
        }
        reasons.push(GuardReason {
            code: "publisher_metadata_unavailable".to_owned(),
            detail: "QED could not read complete publisher token metadata for at least one matching registry candidate.".to_owned(),
        });
    }

    let mut document = GuardDocument {
        id: String::new(),
        kind: "guard".to_owned(),
        chain,
        address: address.to_owned(),
        subject_type,
        subject_address: (!subject.is_empty()).then_some(subject),
        wallet: wallet.map(str::to_owned),
        wallet_check,
        identity,
        powers: power_record,
        source: GuardSource { status: source, provider: provider.to_owned() },
        pools,
        verdict,
        reasons,
        observed_at: state.clock.now().to_rfc3339_opts(SecondsFormat::Secs, true),
        reads: read_log.reads,
        reads_truncated: read_log.reads_truncated,
        public_key: state.signer.public_key(),
        signature: String::new(),
        dev: state.dev_signer,
    };
    let payload =
        crate::domain::guard::canonical_payload_json(&document).map_err(|_| GuardError::Signing)?;
    if payload.len() > MAX_ATTESTATION_BYTES.saturating_sub(1024) {
        return Err(GuardError::Signing);
    }
    (document.id, document.signature) =
        crate::app::attestation::sign_document_payload(state, &payload)
            .map_err(|_| GuardError::Signing)?;
    Ok(document)
}
async fn load_publisher_metadata(
    readers: &[Box<dyn ChainReader>],
    chain: Chain,
    target: &str,
    metadata: Option<&TokenMeta>,
    entries: std::sync::Arc<Vec<Entry>>,
) -> (std::collections::HashMap<String, TokenMeta>, bool) {
    let empty = || (std::collections::HashMap::new(), false);
    let Some(metadata) = metadata else {
        return empty();
    };
    if registry::lookup(entries.as_ref(), chain, target).is_some()
        || !matches!(
            registry::match_status(entries.as_ref(), chain, target),
            registry::MatchStatus::NotFound
        )
    {
        return empty();
    }

    let mut candidates = Vec::<(String, usize)>::with_capacity(MAX_PUBLISHER_METADATA_CANDIDATES);
    let mut truncated = false;
    'priority: for exact_ticker in [true, false] {
        for (index, entry) in entries.iter().enumerate() {
            if !registry::matchable(entry)
                || (entry.chain == chain && same_address(chain, target, &entry.contract))
                || (!crate::domain::guard::metadata_resembles_entry(metadata, entry)
                    && !crate::domain::guard::metadata_claims_product_symbol(metadata, entry))
                || crate::domain::guard::exact_ticker_candidate(metadata, entry) != exact_ticker
            {
                continue;
            }
            let key = crate::domain::guard::issuer_metadata_key(entry.chain, &entry.contract);
            if candidates.iter().any(|(candidate_key, _)| candidate_key == &key) {
                continue;
            }
            if candidates.len() == MAX_PUBLISHER_METADATA_CANDIDATES {
                truncated = true;
                break 'priority;
            }
            candidates.push((key, index));
        }
    }
    if candidates.is_empty() {
        return empty();
    }
    if !crate::domain::guard::metadata_complete(metadata) {
        return (std::collections::HashMap::new(), true);
    }

    let results = futures_util::stream::iter(candidates.into_iter().map(|(key, index)| {
        let entries = std::sync::Arc::clone(&entries);
        async move {
            let entry = &entries[index];
            let result = match readers.iter().find(|reader| reader.chain() == entry.chain) {
                Some(reader) => reader.token_meta(&entry.contract).await.ok(),
                None => None,
            };
            (key, index, result)
        }
    }))
    .buffer_unordered(MAX_PUBLISHER_METADATA_CANDIDATES)
    .collect::<Vec<_>>()
    .await;
    let mut publisher_metadata = std::collections::HashMap::new();
    let mut unavailable = truncated;
    for (key, index, result) in results {
        match result {
            Some(metadata)
                if crate::domain::guard::metadata_complete(&metadata)
                    && same_address(
                        entries[index].chain,
                        &metadata.address,
                        &entries[index].contract,
                    ) =>
            {
                publisher_metadata.insert(key, metadata);
            }
            _ => unavailable = true,
        }
    }
    (publisher_metadata, unavailable)
}

fn append_read_log(target: &mut ReadLog, additional: ReadLog) {
    let remaining = MAX_GUARD_READS.saturating_sub(target.reads.len());
    if additional.reads.len() > remaining {
        target.reads_truncated = true;
    }
    target.reads.extend(additional.reads.into_iter().take(remaining));
    target.reads_truncated |= additional.reads_truncated;
}

async fn resolve_subject(
    reader: &dyn ChainReader,
    clock: &dyn crate::ports::Clock,
    chain: Chain,
    address: &str,
    entries: &[Entry],
    known_pools: &[IndexedPool],
) -> (
    String,
    Option<TokenMeta>,
    Vec<GuardPool>,
    crate::domain::guard::GuardSubjectType,
    bool,
    ReadLog,
) {
    let (resolved, read_log) = capture_reads(async {
        if Chain::is_v4_pool_id(address) {
            let pool = reader.read_v4_pool(address).await.ok().map(|(pool, _)| pool);
            let Some(pool) = pool else {
                return (
                    String::new(),
                    None,
                    Vec::new(),
                    crate::domain::guard::GuardSubjectType::Pool,
                    true,
                );
            };
            let (subject, metadata, pools) =
                resolve_pool_subject(reader, clock, chain, Some(pool), entries).await;
            let unavailable = subject.is_empty();
            return (
                subject,
                metadata,
                pools,
                crate::domain::guard::GuardSubjectType::Pool,
                unavailable,
            );
        }
        let indexed = known_pools
            .iter()
            .filter(|pool| pool.chain == chain && same_address(chain, &pool.pool, address))
            .collect::<Vec<_>>();
        if !indexed.is_empty() {
            let (subject, metadata) = indexed_subject(reader, chain, &indexed, entries).await;
            let pools = indexed_guard_pools(&indexed, MAX_GUARD_POOLS);
            return (subject, metadata, pools, crate::domain::guard::GuardSubjectType::Pool, false);
        }
        // Solana account ownership and a plausible DEX layout do not prove
        // pool provenance. Treat an unindexed address as a token unless the
        // reader can establish derivation; indexed pools were handled above.
        if chain != Chain::Solana {
            if let Ok(pool) = reader.read_pool(address).await {
                let (subject, metadata, pools) =
                    resolve_pool_subject(reader, clock, chain, Some(pool), entries).await;
                let unavailable = subject.is_empty();
                return (
                    subject,
                    metadata,
                    pools,
                    crate::domain::guard::GuardSubjectType::Pool,
                    unavailable,
                );
            }
        }
        let metadata = reader.token_meta(address).await.ok();
        let mut pools = known_pools
            .iter()
            .filter(|pool| pool.chain == chain && same_address(chain, &pool.token_address, address))
            .collect::<Vec<_>>();
        pools.sort_by(|left, right| left.observed_at.cmp(&right.observed_at));
        let guard_pools = indexed_guard_pools(&pools, MAX_GUARD_POOLS);
        (
            address.to_owned(),
            metadata,
            guard_pools,
            crate::domain::guard::GuardSubjectType::Token,
            false,
        )
    })
    .await;
    let (subject, metadata, pools, subject_type, pool_unavailable) = resolved;
    (subject, metadata, pools, subject_type, pool_unavailable, read_log)
}

async fn resolve_pool_subject(
    reader: &dyn ChainReader,
    clock: &dyn crate::ports::Clock,
    chain: Chain,
    pool: Option<PoolInfo>,
    entries: &[Entry],
) -> (String, Option<TokenMeta>, Vec<GuardPool>) {
    let Some(pool) = pool.filter(|pool| pool.chain == chain) else {
        return (String::new(), None, Vec::new());
    };
    let base_meta = reader.token_meta(&pool.base.address).await.ok();
    let quote_meta = reader.token_meta(&pool.quote.address).await.ok();
    let (subject, metadata) =
        choose_pool_subject(&pool, base_meta.as_ref(), quote_meta.as_ref(), entries);
    let sides = check::selected_sides(&pool, entries);
    let evaluation = check::evaluate_pool(
        &pool,
        sides.0,
        sides.1,
        base_meta.as_ref(),
        quote_meta.as_ref(),
        entries,
    );
    let verdict = check_verdict(&evaluation.verdict).to_owned();
    let observed_at = clock.now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let guard_pool = GuardPool {
        address: pool.pool.clone(),
        venue: pool.dex.clone(),
        quote: GuardQuote {
            address: pool.quote.address.clone(),
            symbol: pool.quote.symbol.clone(),
        },
        verdict,
        observed_at: Some(observed_at),
    };
    (subject.address.clone(), metadata.cloned(), vec![guard_pool])
}

fn choose_pool_subject<'a>(
    pool: &'a PoolInfo,
    base_meta: Option<&'a TokenMeta>,
    quote_meta: Option<&'a TokenMeta>,
    entries: &[Entry],
) -> (&'a TokenSide, Option<&'a TokenMeta>) {
    for (side, metadata) in [(&pool.quote, quote_meta), (&pool.base, base_meta)] {
        if registry::lookup(entries, pool.chain, &side.address).is_some() {
            return (side, metadata);
        }
    }
    for (side, metadata) in [(&pool.base, base_meta), (&pool.quote, quote_meta)] {
        let identity = crate::domain::guard::identify(pool.chain, &side.address, metadata, entries);
        if identity.status != IdentityStatus::NoPublisher || identity.candidate.is_some() {
            return (side, metadata);
        }
    }
    (&pool.quote, quote_meta)
}

async fn indexed_subject(
    reader: &dyn ChainReader,
    chain: Chain,
    pools: &[&IndexedPool],
    entries: &[Entry],
) -> (String, Option<TokenMeta>) {
    let mut chosen = None;
    let quote_address = pools.first().map(|pool| pool.quote_address.as_str());
    for pool in pools {
        let fallback = TokenMeta {
            address: pool.token_address.clone(),
            symbol: pool.symbol.clone(),
            name: None,
            decimals: None,
            total_supply: None,
        };
        let identity =
            crate::domain::guard::identify(chain, &pool.token_address, Some(&fallback), entries);
        if identity.status == IdentityStatus::Match {
            chosen = Some((pool.token_address.as_str(), Some(fallback)));
            break;
        }
        if (identity.status == IdentityStatus::Mismatch || identity.candidate.is_some())
            && chosen.is_none()
        {
            chosen = Some((pool.token_address.as_str(), Some(fallback)));
        }
    }
    let (address, fallback) = chosen.unwrap_or_else(|| {
        let pool = pools
            .iter()
            .find(|pool| Some(pool.token_address.as_str()) == quote_address)
            .copied()
            .unwrap_or(pools[0]);
        (
            pool.token_address.as_str(),
            Some(TokenMeta {
                address: pool.token_address.clone(),
                symbol: pool.symbol.clone(),
                name: None,
                decimals: None,
                total_supply: None,
            }),
        )
    });
    let metadata = reader.token_meta(address).await.ok().or(fallback);
    (address.to_owned(), metadata)
}

fn indexed_guard_pools(pools: &[&IndexedPool], limit: usize) -> Vec<GuardPool> {
    let mut seen = std::collections::HashSet::new();
    pools
        .iter()
        .filter(|pool| {
            let key = if pool.chain == Chain::Solana {
                pool.pool.clone()
            } else {
                pool.pool.to_ascii_lowercase()
            };
            seen.insert((pool.chain, key))
        })
        .take(limit)
        .map(|pool| GuardPool {
            address: pool.pool.clone(),
            venue: pool.venue.clone(),
            quote: GuardQuote {
                address: pool.quote_address.clone(),
                symbol: pool.quote_symbol.clone(),
            },
            verdict: pool.verdict.clone(),
            observed_at: Some(pool.observed_at.clone()),
        })
        .collect()
}

fn check_verdict(verdict: &Verdict) -> &'static str {
    match verdict {
        Verdict::Verified { .. } => "verified",
        Verdict::Mismatch { .. } => "mismatch",
        Verdict::NoMatch => "no_match",
        Verdict::Unknown { .. } => "unknown",
    }
}

fn source_status(chain: Chain, powers: Option<&PowersRecord>) -> (SourceStatus, &'static str) {
    let provider = match chain {
        Chain::Solana => "verify.osec.io",
        Chain::RobinhoodChain | Chain::Base | Chain::Ethereum | Chain::Bnb => "Sourcify",
    };
    let status = match powers.map(|record| record.source_verified) {
        Some(SourceVerified::ExactMatch | SourceVerified::Match) => SourceStatus::Verified,
        Some(SourceVerified::None) => SourceStatus::Unverified,
        Some(SourceVerified::Unavailable) | None => SourceStatus::Unavailable,
    };
    (status, provider)
}

fn canonical_input(address: &str, chain: Chain) -> Result<String, GuardError> {
    if address.len() > MAX_GUARD_ADDRESS_CHARS {
        return Err(GuardError::InvalidAddress);
    }
    let address = address.trim();
    if Chain::is_v4_pool_id(address) {
        return (chain != Chain::Solana)
            .then(|| address.to_ascii_lowercase())
            .ok_or(GuardError::InvalidAddress);
    }
    powers::canonical_contract(chain, address).map_err(|_| GuardError::InvalidAddress)
}

fn canonical_wallet(wallet: &str, chain: Chain) -> Result<String, GuardError> {
    if wallet.len() > Chain::MAX_SOLANA_ADDRESS_CHARS {
        return Err(GuardError::InvalidWallet);
    }
    let wallet = wallet.trim();
    match chain {
        Chain::Solana => Chain::decode_solana_address(wallet)
            .map(|bytes| bs58::encode(bytes).into_string())
            .ok_or(GuardError::InvalidWallet),
        Chain::RobinhoodChain | Chain::Base | Chain::Ethereum | Chain::Bnb
            if wallet.len() == 42 && Chain::is_evm_address(wallet) =>
        {
            Ok(wallet.to_ascii_lowercase())
        }
        Chain::RobinhoodChain | Chain::Base | Chain::Ethereum | Chain::Bnb => {
            Err(GuardError::InvalidWallet)
        }
    }
}

fn same_address(chain: Chain, left: &str, right: &str) -> bool {
    if chain == Chain::Solana { left == right } else { left.eq_ignore_ascii_case(right) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::context::test_support,
        domain::{
            guard::{GuardSubjectType, GuardVerdict},
            pool::{IndexedPool, PoolError, PoolInfo, TokenMeta, TokenSide},
            powers::{PowerFacts, Reason},
            registry::Entry,
        },
        ports::ChainReader,
    };
    use async_trait::async_trait;
    use std::collections::HashMap;

    const TOKEN: &str = "0x0000000000000000000000000000000000000001";
    const USD: &str = "0x0000000000000000000000000000000000000002";
    const POOL: &str = "0x0000000000000000000000000000000000000003";
    const WALLET: &str = "0x0000000000000000000000000000000000000004";

    struct GuardReader {
        chain: Chain,
        pool: Option<PoolInfo>,
        metadata: HashMap<String, TokenMeta>,
        restrictions: Vec<crate::domain::powers::Reason>,
        wallet_applicable: bool,
        token_meta_reads: Option<std::sync::Arc<std::sync::atomic::AtomicUsize>>,
    }

    #[async_trait]
    impl ChainReader for GuardReader {
        fn chain(&self) -> Chain {
            self.chain
        }

        async fn read_pool(&self, address: &str) -> Result<PoolInfo, PoolError> {
            self.pool
                .as_ref()
                .filter(|pool| same_address(pool.chain, &pool.pool, address))
                .cloned()
                .ok_or_else(|| PoolError::Unknown("not a pool".to_owned()))
        }

        async fn token_meta(&self, address: &str) -> Result<TokenMeta, PoolError> {
            if let Some(reads) = &self.token_meta_reads {
                reads.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            let key = if self.chain == Chain::Solana {
                address.to_owned()
            } else {
                address.to_ascii_lowercase()
            };
            let result = self
                .metadata
                .get(&key)
                .cloned()
                .ok_or_else(|| PoolError::Unknown("metadata unavailable".to_owned()));
            let logged_result = match &result {
                Ok(metadata) => serde_json::json!({
                    "address": metadata.address,
                    "symbol": metadata.symbol,
                    "name": metadata.name,
                }),
                Err(_) => serde_json::json!({ "error": "metadata unavailable" }),
            };
            crate::ports::record_read(
                "token_meta",
                serde_json::json!({ "address": address }),
                &logged_result,
                false,
                None,
                None,
            );
            result
        }
        async fn power_facts(
            &self,
            _address: &str,
        ) -> Result<crate::domain::powers::PowerFacts, PoolError> {
            Ok(PowerFacts::default())
        }

        async fn wallet_restrictions(
            &self,
            _contract: &str,
            _wallet: &str,
            _sanctions_list: Option<&str>,
        ) -> Result<crate::ports::WalletRestrictionReport, PoolError> {
            Ok(crate::ports::WalletRestrictionReport {
                restrictions: self.restrictions.clone(),
                complete: true,
                applicable: self.wallet_applicable,
            })
        }
    }

    fn entry() -> Entry {
        Entry {
            issuer: "Example Publisher".to_owned(),
            ticker: "NVDA".to_owned(),
            name: "NVIDIA".to_owned(),
            chain: Chain::Base,
            contract: TOKEN.to_owned(),
            decimals: Some(18),
            source: "test".to_owned(),
            source_url: "https://issuer.example".to_owned(),
            last_checked: "2026-10-01T00:00:00Z".to_owned(),
            removed_at: None,
            stale_since: None,
            official_deployments: Vec::new(),
        }
    }

    fn meta(address: &str, symbol: &str, name: &str) -> TokenMeta {
        TokenMeta {
            address: address.to_owned(),
            symbol: Some(symbol.to_owned()),
            name: Some(name.to_owned()),
            decimals: Some(18),
            total_supply: Some("1000000".to_owned()),
        }
    }

    fn fixture(
        pool: Option<PoolInfo>,
        restrictions: Vec<crate::domain::powers::Reason>,
    ) -> test_support::TestContext {
        let metadata = [
            (TOKEN.to_owned(), meta(TOKEN, "NVDA", "NVIDIA")),
            (USD.to_owned(), meta(USD, "USDC", "USD Coin")),
        ]
        .into_iter()
        .map(|(address, meta)| (address.to_ascii_lowercase(), meta))
        .collect();
        test_support::build(
            vec![Box::new(GuardReader {
                chain: Chain::Base,
                pool,
                metadata,
                restrictions,
                wallet_applicable: true,
                token_meta_reads: None,
            })],
            vec![entry()],
            false,
        )
    }

    fn token_side(address: &str, symbol: &str) -> TokenSide {
        TokenSide {
            address: address.to_owned(),
            symbol: Some(symbol.to_owned()),
            decimals: Some(18),
            balance: Some("100".to_owned()),
        }
    }

    #[tokio::test]
    async fn guard_resolves_quote_side_signs_facts_and_checks_wallet_restrictions() {
        let pool = PoolInfo {
            chain: Chain::Base,
            pool: POOL.to_owned(),
            dex: "uniswap-v2".to_owned(),
            base: token_side(USD, "USDC"),
            quote: token_side(TOKEN, "NVDA"),
        };
        let state = fixture(
            Some(pool),
            vec![crate::domain::powers::Reason::new(
                "wallet_frozen",
                "a matching token account is frozen",
            )],
        );
        let document = create(&state, POOL, Chain::Base, Some(WALLET)).await.expect("signed Guard");
        assert_eq!(document.identity.status, IdentityStatus::Match);
        assert_eq!(document.subject_type, GuardSubjectType::Pool);
        assert_eq!(document.subject_address.as_deref(), Some(TOKEN));
        assert_eq!(document.pools[0].quote.address, TOKEN);
        assert_eq!(
            document.wallet_check.as_ref().map(|check| check.status),
            Some(WalletCheckStatus::Checked)
        );
        assert_eq!(document.verdict, GuardVerdict::Deny);
        crate::domain::guard::verify(&document).expect("Guard signature verifies");
    }
    #[tokio::test]
    async fn missing_wallet_probe_is_reported_as_not_applicable() {
        let metadata = HashMap::from([(TOKEN.to_owned(), meta(TOKEN, "NVDA", "NVIDIA"))]);
        let state = test_support::build(
            vec![Box::new(GuardReader {
                chain: Chain::Base,
                pool: None,
                metadata,
                restrictions: Vec::new(),
                wallet_applicable: false,
                token_meta_reads: None,
            })],
            vec![entry()],
            false,
        );

        let document =
            create(&state, TOKEN, Chain::Base, Some(WALLET)).await.expect("signed Guard");
        assert_eq!(
            document.wallet_check.as_ref().map(|check| check.status),
            Some(WalletCheckStatus::NotApplicable)
        );
        assert_eq!(document.verdict, GuardVerdict::Unknown);
        assert!(document.reasons.iter().any(|reason| reason.code == "wallet_check_not_applicable"));
    }
    #[tokio::test]
    async fn solana_frozen_wallet_is_denied_without_power_capability_signals() {
        let token = bs58::encode([21u8; 32]).into_string();
        let wallet = bs58::encode([22u8; 32]).into_string();
        let mut registered = entry();
        registered.chain = Chain::Solana;
        registered.contract = token.clone();
        let metadata = HashMap::from([(token.clone(), meta(&token, "NVDA", "NVIDIA"))]);
        let state = test_support::build(
            vec![Box::new(GuardReader {
                chain: Chain::Solana,
                pool: None,
                metadata,
                restrictions: vec![Reason::new(
                    "wallet_frozen",
                    "The matching token account is frozen.",
                )],
                token_meta_reads: None,
                wallet_applicable: true,
            })],
            vec![registered],
            false,
        );

        let document =
            create(&state, &token, Chain::Solana, Some(&wallet)).await.expect("signed Guard");

        let powers = document.powers.as_ref().expect("power observation");
        assert!(powers.can_block.is_empty());
        assert!(powers.can_seize.is_empty());
        assert_eq!(
            document.wallet_check.as_ref().map(|check| check.status),
            Some(WalletCheckStatus::Checked)
        );
        assert!(document.wallet_check.as_ref().is_some_and(|check| {
            check.restrictions.iter().any(|reason| reason.code == "wallet_frozen")
        }));
        assert_eq!(document.verdict, GuardVerdict::Deny);
    }

    #[tokio::test]
    async fn failed_publisher_metadata_read_is_unknown_and_signed() {
        const CLONE: &str = "0x0000000000000000000000000000000000000005";
        let reads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let state = test_support::build(
            vec![Box::new(GuardReader {
                chain: Chain::Base,
                pool: None,
                metadata: HashMap::from([(
                    CLONE.to_ascii_lowercase(),
                    meta(CLONE, "NVDAx", "NVIDIA xStock"),
                )]),
                restrictions: Vec::new(),
                wallet_applicable: true,
                token_meta_reads: Some(std::sync::Arc::clone(&reads)),
            })],
            vec![entry()],
            false,
        );

        let document = create(&state, CLONE, Chain::Base, None).await.expect("Guard");
        assert_eq!(document.identity.status, IdentityStatus::NoPublisher);
        assert_eq!(document.verdict, GuardVerdict::Unknown);
        assert!(
            document.reasons.iter().any(|reason| reason.code == "publisher_metadata_unavailable")
        );
        assert_eq!(reads.load(std::sync::atomic::Ordering::Relaxed), 2);
        assert_eq!(document.reads.iter().filter(|read| read.method == "token_meta").count(), 2);
        crate::domain::guard::verify(&document).expect("failed read is recorded in signed Guard");
    }

    #[tokio::test]
    async fn incomplete_publisher_metadata_cannot_contradict() {
        const CLONE: &str = "0x0000000000000000000000000000000000000005";
        let mut incomplete = meta(TOKEN, "NVDAx", "NVIDIA xStock");
        incomplete.name = None;
        let state = test_support::build(
            vec![Box::new(GuardReader {
                chain: Chain::Base,
                pool: None,
                metadata: HashMap::from([
                    (TOKEN.to_owned(), incomplete),
                    (CLONE.to_ascii_lowercase(), meta(CLONE, "NVDAx", "NVIDIA xStock")),
                ]),
                restrictions: Vec::new(),
                wallet_applicable: true,
                token_meta_reads: None,
            })],
            vec![entry()],
            false,
        );

        let document = create(&state, CLONE, Chain::Base, None).await.expect("Guard");
        assert_eq!(document.identity.status, IdentityStatus::NoPublisher);
        assert_eq!(document.verdict, GuardVerdict::Unknown);
        assert!(
            document.reasons.iter().any(|reason| reason.code == "publisher_metadata_unavailable")
        );
    }

    #[tokio::test]
    async fn fresh_publisher_metadata_detects_clone_and_is_signed() {
        const CLONE: &str = "0x0000000000000000000000000000000000000005";
        let reads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let state = test_support::build(
            vec![Box::new(GuardReader {
                chain: Chain::Base,
                pool: None,
                metadata: HashMap::from([
                    (TOKEN.to_owned(), meta(TOKEN, "NVDAx", "NVIDIA xStock")),
                    (CLONE.to_ascii_lowercase(), meta(CLONE, "NVDAx", "NVIDIA xStock")),
                ]),
                restrictions: Vec::new(),
                wallet_applicable: true,
                token_meta_reads: Some(std::sync::Arc::clone(&reads)),
            })],
            vec![entry()],
            false,
        );

        let first = create(&state, CLONE, Chain::Base, None).await.expect("Guard");
        assert_eq!(first.identity.status, IdentityStatus::Mismatch);
        assert_eq!(first.verdict, GuardVerdict::Deny);
        assert_eq!(reads.load(std::sync::atomic::Ordering::Relaxed), 2);
        assert_eq!(first.reads.iter().filter(|read| read.method == "token_meta").count(), 2);
        assert!(
            !first.reasons.iter().any(|reason| reason.code == "publisher_metadata_unavailable")
        );
        crate::domain::guard::verify(&first).expect("fresh publisher read is signed");

        let second = create(&state, CLONE, Chain::Base, None).await.expect("second Guard");
        assert_eq!(second.identity.status, IdentityStatus::Mismatch);
        assert_eq!(reads.load(std::sync::atomic::Ordering::Relaxed), 4);
        assert_eq!(second.reads.iter().filter(|read| read.method == "token_meta").count(), 2);
    }
    #[tokio::test]
    async fn cross_chain_xstock_clone_is_denied_but_official_wrapper_matches() {
        const CLONE: &str = "0x0000000000000000000000000000000000000005";
        const BASE_WRAPPER: &str = "0x0000000000000000000000000000000000000011";
        let mut backed = entry();
        backed.official_deployments = vec![
            crate::domain::registry::OfficialDeployment {
                network: "Arbitrum".to_owned(),
                address: "0x0000000000000000000000000000000000000101".to_owned(),
                wrapper_address: None,
                wrapper_address_v2: None,
            },
            crate::domain::registry::OfficialDeployment {
                network: "Base".to_owned(),
                address: TOKEN.to_owned(),
                wrapper_address: Some(BASE_WRAPPER.to_owned()),
                wrapper_address_v2: None,
            },
            crate::domain::registry::OfficialDeployment {
                network: "Ethereum".to_owned(),
                address: "0x0000000000000000000000000000000000000102".to_owned(),
                wrapper_address: None,
                wrapper_address_v2: None,
            },
        ];
        let state = test_support::build(
            vec![
                Box::new(GuardReader {
                    chain: Chain::Base,
                    pool: None,
                    metadata: HashMap::from([(
                        TOKEN.to_ascii_lowercase(),
                        meta(TOKEN, "NVDAx", "NVIDIA xStock"),
                    )]),
                    restrictions: Vec::new(),
                    wallet_applicable: true,
                    token_meta_reads: None,
                }),
                Box::new(GuardReader {
                    chain: Chain::RobinhoodChain,
                    pool: None,
                    metadata: HashMap::from([(
                        CLONE.to_ascii_lowercase(),
                        meta(CLONE, "NVDAx", "NVIDIA xStock"),
                    )]),
                    restrictions: Vec::new(),
                    wallet_applicable: true,
                    token_meta_reads: None,
                }),
            ],
            vec![backed],
            false,
        );

        let clone = create(&state, CLONE, Chain::RobinhoodChain, None).await.expect("clone Guard");
        assert_eq!(clone.identity.status, IdentityStatus::Mismatch);
        assert_eq!(clone.verdict, GuardVerdict::Deny);
        let reason = clone
            .reasons
            .iter()
            .find(|reason| reason.code == "claims_unpublished_publisher_product")
            .expect("specific cross-chain denial reason");
        assert_eq!(
            reason.detail,
            "QED Guard found an exact on-chain symbol/name match for NVIDIA xStock (NVDA), but this address is absent from Example Publisher's published deployment catalog. The catalog lists deployments on Arbitrum, Base, Ethereum."
        );
        assert!(clone.reads.iter().any(|read| read.method == "token_meta"));
        crate::domain::guard::verify(&clone).expect("cross-chain denial is signed");

        let wrapper =
            create(&state, BASE_WRAPPER, Chain::Base, None).await.expect("official wrapper Guard");
        assert_eq!(wrapper.identity.status, IdentityStatus::Match);
        assert_eq!(wrapper.identity.matched_contract.as_deref(), Some(BASE_WRAPPER));
        assert_ne!(wrapper.verdict, GuardVerdict::Deny);
    }

    #[tokio::test]
    async fn publisher_candidates_prioritize_exact_ticker_and_cap_reads_at_eight() {
        const CLONE: &str = "0x00000000000000000000000000000000000000ff";
        const EXACT_PUBLISHER: &str = "0x0000000000000000000000000000000000000011";
        let clone_metadata = meta(CLONE, "NVDAx", "NVIDIA xStock");
        let exact_metadata = meta(EXACT_PUBLISHER, "NVDAx", "NVIDIA xStock");
        let metadata = HashMap::from([
            (CLONE.to_ascii_lowercase(), clone_metadata),
            (EXACT_PUBLISHER.to_owned(), exact_metadata),
        ]);
        let mut entries = Vec::new();
        for index in 1..=10 {
            let mut candidate = entry();
            candidate.contract = format!("0x{index:040x}");
            candidate.ticker = format!("FUND{index}");
            candidate.name = "NVIDIA Holdings".to_owned();
            entries.push(candidate);
        }
        let mut exact_candidate = entry();
        exact_candidate.contract = EXACT_PUBLISHER.to_owned();
        exact_candidate.ticker = "NVDAx".to_owned();
        exact_candidate.name = "NVIDIA xStock".to_owned();
        entries.push(exact_candidate);
        let reads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let state = test_support::build(
            vec![Box::new(GuardReader {
                chain: Chain::Base,
                pool: None,
                metadata,
                restrictions: Vec::new(),
                wallet_applicable: true,
                token_meta_reads: Some(std::sync::Arc::clone(&reads)),
            })],
            entries,
            false,
        );

        let document = create(&state, CLONE, Chain::Base, None).await.expect("Guard");
        assert_eq!(document.identity.status, IdentityStatus::Mismatch);
        assert_eq!(document.identity.matched_contract.as_deref(), Some(EXACT_PUBLISHER));
        assert_eq!(reads.load(std::sync::atomic::Ordering::Relaxed), 9);
        assert_eq!(document.reads.iter().filter(|read| read.method == "token_meta").count(), 9);
    }
    #[tokio::test]
    async fn guard_warm_power_cache_does_not_acquire_prefetch_permit() {
        let state = fixture(None, Vec::new());
        let first = create(&state, TOKEN, Chain::Base, None).await.expect("initial Guard");
        assert_eq!(first.identity.status, IdentityStatus::Match);

        state.powers_prefetch_concurrency.close();
        let cached = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            create(&state, TOKEN, Chain::Base, None),
        )
        .await
        .expect("warm-cache Guard does not wait for the prefetch permit")
        .expect("cached Guard");
        assert_eq!(cached.identity.status, IdentityStatus::Match);
    }

    #[tokio::test]
    async fn token_guard_distinguishes_publisher_mismatch_from_no_publisher() {
        let state = fixture(None, Vec::new());
        let unknown = create(&state, USD, Chain::Base, None).await.expect("Guard");
        assert_eq!(unknown.identity.status, IdentityStatus::NoPublisher);
        assert_eq!(unknown.verdict, GuardVerdict::Unknown);

        let metadata = HashMap::from([
            (TOKEN.to_owned(), meta(TOKEN, "NVDAx", "NVIDIA xStock")),
            (USD.to_owned(), meta(USD, "NVDAx", "NVIDIA xStock")),
        ]);
        let mut registered = entry();
        registered.contract = USD.to_owned();
        let mismatched = test_support::build(
            vec![Box::new(GuardReader {
                chain: Chain::Base,
                pool: None,
                metadata,
                restrictions: Vec::new(),
                wallet_applicable: true,
                token_meta_reads: None,
            })],
            vec![registered],
            false,
        );
        let result = create(&mismatched, TOKEN, Chain::Base, None).await.expect("Guard");
        assert_eq!(result.identity.status, IdentityStatus::Mismatch);
        assert_eq!(result.verdict, GuardVerdict::Deny);
    }
    fn solana_case_variant(address: &str) -> String {
        for (index, byte) in address.bytes().enumerate() {
            let replacement = if byte.is_ascii_lowercase() {
                byte.to_ascii_uppercase()
            } else if byte.is_ascii_uppercase() {
                byte.to_ascii_lowercase()
            } else {
                continue;
            };
            let mut candidate = address.to_owned();
            candidate.replace_range(index..index + 1, &(replacement as char).to_string());
            if Chain::decode_solana_address(&candidate).is_some() {
                return candidate;
            }
        }
        panic!("fixture address has no valid case variant");
    }

    #[tokio::test]
    async fn solana_case_variant_does_not_reuse_indexed_pool_facts() {
        let token = bs58::encode([9u8; 32]).into_string();
        let variant = solana_case_variant(&token);
        assert_ne!(token, variant);
        assert!(Chain::decode_solana_address(&variant).is_some());

        let mut registered = entry();
        registered.chain = Chain::Solana;
        registered.contract = token.clone();
        let metadata = HashMap::from([(token.clone(), meta(&token, "NVDA", "NVIDIA"))]);
        let state = test_support::build(
            vec![Box::new(GuardReader {
                chain: Chain::Solana,
                pool: None,
                metadata,
                restrictions: Vec::new(),
                wallet_applicable: true,
                token_meta_reads: None,
            })],
            vec![registered.clone()],
            false,
        );
        let indexed = IndexedPool {
            chain: Chain::Solana,
            token_address: token.clone(),
            symbol: Some("NVDA".to_owned()),
            pool: bs58::encode([10u8; 32]).into_string(),
            pool_url: String::new(),
            trade_url: String::new(),
            venue: "Raydium".to_owned(),
            quote_address: bs58::encode([11u8; 32]).into_string(),
            quote_symbol: Some("USDC".to_owned()),
            verdict: "verified".to_owned(),
            observed_at: "2026-10-01T00:00:00Z".to_owned(),
        };
        let (subject, metadata, pools, subject_type, pool_unavailable, _) = resolve_subject(
            state.readers[0].as_ref(),
            state.clock.as_ref(),
            Chain::Solana,
            &variant,
            &[registered],
            &[indexed],
        )
        .await;

        assert_eq!(subject, variant);
        assert!(metadata.is_none());
        assert!(pools.is_empty());
        assert_eq!(subject_type, GuardSubjectType::Token);
        assert!(!pool_unavailable);
    }
    #[tokio::test]
    async fn unindexed_solana_account_is_not_treated_as_a_pool() {
        let address = bs58::encode([12u8; 32]).into_string();
        let pool = PoolInfo {
            chain: Chain::Solana,
            pool: address.clone(),
            dex: "raydium-cpmm".to_owned(),
            base: token_side(&bs58::encode([13u8; 32]).into_string(), "BASE"),
            quote: token_side(&bs58::encode([14u8; 32]).into_string(), "QUOTE"),
        };
        let state = test_support::build(
            vec![Box::new(GuardReader {
                chain: Chain::Solana,
                pool: Some(pool),
                metadata: HashMap::new(),
                token_meta_reads: None,
                restrictions: Vec::new(),
                wallet_applicable: true,
            })],
            Vec::new(),
            false,
        );

        let (subject, _, pools, subject_type, pool_unavailable, _) = resolve_subject(
            state.readers[0].as_ref(),
            state.clock.as_ref(),
            Chain::Solana,
            &address,
            &[],
            &[],
        )
        .await;

        assert_eq!(subject, address);
        assert!(pools.is_empty());
        assert_eq!(subject_type, GuardSubjectType::Token);
        assert!(!pool_unavailable);
    }
}
