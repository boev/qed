use crate::{
    app::context::Context,
    domain::{
        chain::Chain,
        powers::Reason,
        registry::{self, Entry},
        statement::{
            PowersSummary, Statement, StatementAsset, StatementHolding, StatementPosition,
            StatementWallet,
        },
    },
    ports::capture_reads,
};
use chrono::SecondsFormat;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use std::{collections::HashSet, sync::atomic::Ordering};
use thiserror::Error;

const STATEMENT_DEADLINE: Duration = Duration::from_secs(15);

const MAX_WALLETS: usize = 32;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct StatementRequest {
    pub wallets: Vec<String>,
    pub chains: Vec<String>,
    pub block: Option<u64>,
}

#[derive(Debug, Error)]
pub(crate) enum StatementError {
    #[error("statement request is invalid")]
    InvalidRequest,
    #[error("wallet address is not supported by the requested chains")]
    UnknownWallet,
    #[error("no reader is configured for {0}")]
    ReaderUnavailable(Chain),
    #[error("balance reads failed on {0}")]
    ReadFailed(Chain),
    #[error("statement balance reads exceeded the 15 second deadline")]
    DeadlineExceeded,
    #[error("statement signing failed")]
    Signing,
}
pub(crate) async fn create(
    state: &Context,
    mut request: StatementRequest,
) -> Result<Statement, StatementError> {
    let chains = parse_chains(&request.chains)?;
    request.wallets = normalize_wallets(&request.wallets)?;
    let (data, read_log) = tokio::time::timeout(
        STATEMENT_DEADLINE,
        capture_reads(async {
            let entries = state.registry.snapshot().await;
            collect_statement_data(state, &request, &chains, &entries).await
        }),
    )
    .await
    .map_err(|_| StatementError::DeadlineExceeded)?;
    let (wallets, assets, positions) = data?;
    let mut statement = Statement {
        id: String::new(),
        kind: "statement".to_owned(),
        version: 1,
        wallets,
        assets,
        positions,
        block: request.block,
        observed_at: state.clock.now().to_rfc3339_opts(SecondsFormat::Secs, true),
        reads: read_log.reads,
        reads_truncated: read_log.reads_truncated,
        signer: state.signer.public_key(),
        signature: String::new(),
        dev: state.dev_signer,
    };
    let payload = crate::domain::statement::canonical_payload_json(&statement)
        .map_err(|_| StatementError::Signing)?;
    let (id, signature) = crate::app::attestation::sign_document_payload(state, &payload)
        .map_err(|_| StatementError::Signing)?;
    statement.id = id;
    statement.signature = signature;
    state.statement_cache.insert(statement.id.clone(), statement.clone()).await;
    Ok(statement)
}

pub(crate) async fn get(state: &Context, id: &str) -> Option<Statement> {
    state.statement_cache.get(&id.to_owned()).await
}

async fn collect_statement_data(
    state: &Context,
    request: &StatementRequest,
    chains: &[Chain],
    entries: &[Entry],
) -> Result<(Vec<StatementWallet>, Vec<StatementAsset>, Vec<StatementPosition>), StatementError> {
    let mut wallets = Vec::new();
    let mut assets = Vec::new();
    let mut positions = Vec::new();
    for address in &request.wallets {
        let matching_chains = chains
            .iter()
            .copied()
            .filter(|chain| address_matches_chain(address, *chain))
            .collect::<Vec<_>>();
        if matching_chains.is_empty() {
            return Err(StatementError::UnknownWallet);
        }
        for chain in matching_chains {
            let reader = state
                .readers
                .iter()
                .find(|reader| reader.chain() == chain)
                .ok_or(StatementError::ReaderUnavailable(chain))?;
            let (holdings, position) = reader
                .statement_holdings(address, entries, request.block)
                .await
                .map_err(|_| StatementError::ReadFailed(chain))?;
            wallets.push(StatementWallet { chain, address: address.clone() });
            positions.push(position);
            for statement_holding in holdings {
                let holding = &statement_holding.holding;
                let Some(entry) = registry::lookup(entries, chain, &holding.token_address) else {
                    continue;
                };
                let Some(decimals) = holding.decimals else {
                    return Err(StatementError::ReadFailed(chain));
                };
                if holding.amount.is_empty()
                    || !holding.amount.bytes().all(|byte| byte.is_ascii_digit())
                {
                    return Err(StatementError::ReadFailed(chain));
                }
                if holding.amount.bytes().all(|byte| byte == b'0') {
                    continue;
                }
                assets.push(asset_row(state, address, entry, statement_holding, decimals).await);
            }
        }
    }
    wallets.sort_by(|left, right| {
        left.chain
            .to_string()
            .cmp(&right.chain.to_string())
            .then_with(|| left.address.cmp(&right.address))
    });
    assets.sort_by(|left, right| {
        left.chain
            .to_string()
            .cmp(&right.chain.to_string())
            .then_with(|| left.wallet.cmp(&right.wallet))
            .then_with(|| {
                left.contract.to_ascii_lowercase().cmp(&right.contract.to_ascii_lowercase())
            })
            .then_with(|| left.slot.cmp(&right.slot))
    });
    positions.sort_by(|left, right| {
        left.chain
            .to_string()
            .cmp(&right.chain.to_string())
            .then_with(|| left.wallet.cmp(&right.wallet))
    });
    Ok((wallets, assets, positions))
}

async fn asset_row(
    state: &Context,
    wallet: &str,
    entry: &Entry,
    statement_holding: StatementHolding,
    decimals: u8,
) -> StatementAsset {
    let holding = statement_holding.holding;
    let version = state.registry_version.load(Ordering::Acquire);
    let cached = crate::app::powers::canonical_contract(entry.chain, &entry.contract)
        .ok()
        .and_then(|contract| Some((entry.chain, contract, version)));
    let facts = match cached {
        Some(key) => state.powers_cache.get(&key).await,
        None => None,
    };
    let powers_observed_at = facts.as_ref().map(|facts| facts.observed_at.clone());
    let powers_block = facts.as_ref().and_then(|facts| facts.block);
    let powers_slot = facts.as_ref().and_then(|facts| facts.slot);
    let powers_summary = facts.map_or_else(
        || PowersSummary {
            can_seize: Vec::new(),
            can_block: Vec::new(),
            can_change_rules: Vec::new(),
            unavailable: vec![Reason::new(
                "not_observed",
                format!(
                    "Power observations were not observed for this asset; use /api/powers/{} for a live check.",
                    entry.contract
                ),
            )],
        },
        |facts| PowersSummary {
            can_seize: facts.can_seize,
            can_block: facts.can_block,
            can_change_rules: facts.can_change_rules,
            unavailable: facts.unavailable,
        },
    );
    StatementAsset {
        wallet: wallet.to_owned(),
        chain: entry.chain,
        contract: entry.contract.clone(),
        ticker: entry.ticker.clone(),
        issuer: entry.issuer.clone(),
        issuer_match: true,
        balance: holding.amount,
        decimals,
        slot: statement_holding.slot,
        powers_observed_at,
        powers_block,
        powers_slot,
        powers_summary,
    }
}

fn parse_chains(values: &[String]) -> Result<Vec<Chain>, StatementError> {
    if values.is_empty() || values.len() > 5 {
        return Err(StatementError::InvalidRequest);
    }
    let mut seen = HashSet::new();
    values
        .iter()
        .map(|value| {
            let chain = Chain::parse(value).ok_or(StatementError::InvalidRequest)?;
            if !seen.insert(chain) {
                return Err(StatementError::InvalidRequest);
            }
            Ok(chain)
        })
        .collect()
}

fn normalize_wallets(wallets: &[String]) -> Result<Vec<String>, StatementError> {
    if wallets.is_empty() || wallets.len() > MAX_WALLETS {
        return Err(StatementError::InvalidRequest);
    }
    let mut seen = HashSet::new();
    wallets
        .iter()
        .map(|wallet| {
            if wallet.trim() != wallet || wallet.is_empty() || wallet.len() > 128 {
                return Err(StatementError::InvalidRequest);
            }
            let normalized = if Chain::is_evm_address(wallet) {
                wallet.to_ascii_lowercase()
            } else {
                wallet.clone()
            };
            if !seen.insert(normalized.clone()) {
                return Err(StatementError::InvalidRequest);
            }
            Ok(normalized)
        })
        .collect()
}

fn address_matches_chain(address: &str, chain: Chain) -> bool {
    match chain {
        Chain::Solana => Chain::detect(address) == Some(Chain::Solana),
        Chain::RobinhoodChain | Chain::Base | Chain::Ethereum | Chain::Bnb => {
            Chain::is_evm_address(address)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::context::test_support::{TestContext, build},
        domain::{
            chain::Chain,
            pool::{PoolError, PoolInfo, WalletHolding},
            powers::{PowerFacts, PowersRecord, SourceVerified, SourceVerifiedSubject},
            registry::{Entry, Registry},
            statement::{StatementHolding, StatementPosition},
        },
        ports::ChainReader,
    };
    use async_trait::async_trait;
    use serde_json::json;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering as AtomicOrdering},
    };

    struct TestReader {
        fail: bool,
        statement_reads: Arc<AtomicUsize>,
        power_calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ChainReader for TestReader {
        fn chain(&self) -> Chain {
            Chain::Base
        }

        async fn read_pool(&self, _address: &str) -> Result<PoolInfo, PoolError> {
            Err(PoolError::Reader("unused test method".to_owned()))
        }

        async fn statement_holdings(
            &self,
            owner: &str,
            entries: &[Entry],
            block: Option<u64>,
        ) -> Result<(Vec<StatementHolding>, StatementPosition), PoolError> {
            self.statement_reads.fetch_add(1, AtomicOrdering::SeqCst);
            if self.fail {
                return Err(PoolError::Reader("transient test read failure".to_owned()));
            }
            crate::ports::record_read(
                "test_balanceOf",
                json!({
                    "owner": owner,
                    "tokens": entries.iter().map(|entry| &entry.contract).collect::<Vec<_>>()
                }),
                &json!({ "amount": "12345" }),
                true,
                block,
                None,
            );
            let holdings = entries
                .first()
                .map(|entry| {
                    vec![StatementHolding {
                        holding: WalletHolding {
                            chain: Chain::Base,
                            token_address: entry.contract.clone(),
                            symbol: Some(entry.ticker.clone()),
                            amount: "12345".to_owned(),
                            decimals: Some(2),
                        },
                        slot: None,
                    }]
                })
                .unwrap_or_default();
            Ok((
                holdings,
                StatementPosition {
                    chain: Chain::Base,
                    wallet: owner.to_owned(),
                    block: Some(block.unwrap_or(100)),
                    min_slot: None,
                    max_slot: None,
                },
            ))
        }

        async fn power_facts(&self, _address: &str) -> Result<PowerFacts, PoolError> {
            self.power_calls.fetch_add(1, AtomicOrdering::SeqCst);
            Ok(PowerFacts {
                can_seize: vec![Reason::new("admin", "issuer admin authority")],
                ..PowerFacts::default()
            })
        }
    }

    fn entry(token: &str, ticker: &str) -> Entry {
        Entry {
            issuer: "Robinhood".to_owned(),
            ticker: ticker.to_owned(),
            name: format!("Robinhood {ticker}"),
            chain: Chain::Base,
            contract: token.to_owned(),
            decimals: Some(2),
            source: "test".to_owned(),
            source_url: "https://issuer.example".to_owned(),
            last_checked: "2026-10-04T00:00:00Z".to_owned(),
            removed_at: None,
            stale_since: None,
        }
    }

    fn statement_context(
        entries: Registry,
        fail: bool,
    ) -> (TestContext, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let statement_reads = Arc::new(AtomicUsize::new(0));
        let power_calls = Arc::new(AtomicUsize::new(0));
        let state = build(
            vec![Box::new(TestReader {
                fail,
                statement_reads: Arc::clone(&statement_reads),
                power_calls: Arc::clone(&power_calls),
            })],
            entries,
            false,
        );
        (state, statement_reads, power_calls)
    }

    fn request(wallets: Vec<String>) -> StatementRequest {
        StatementRequest { wallets, chains: vec!["base".to_owned()], block: Some(100) }
    }

    #[tokio::test]
    async fn statement_signs_registry_holdings_and_marks_uncached_powers_unobserved() {
        let token = "0x0000000000000000000000000000000000000011";
        let (state, _, power_calls) = statement_context(vec![entry(token, "NVDA")], false);
        let wallets = [
            "0x0000000000000000000000000000000000000001",
            "0x0000000000000000000000000000000000000002",
        ];
        let statement =
            create(&state, request(wallets.iter().map(|wallet| (*wallet).to_owned()).collect()))
                .await
                .unwrap();

        assert_eq!(statement.wallets.len(), 2);
        assert_eq!(statement.assets.len(), 2);
        assert!(statement.assets.iter().all(|asset| {
            asset.ticker == "NVDA"
                && asset.issuer_match
                && asset.balance == "12345"
                && asset.powers_summary.unavailable[0].code == "not_observed"
                && asset.powers_summary.unavailable[0].detail.contains("/api/powers/")
        }));
        assert!(statement.assets.iter().all(|asset| asset.slot.is_none()));
        assert!(statement.positions.iter().all(|position| position.block == Some(100)));
        assert_eq!(statement.reads.len(), 2);
        assert!(statement.reads.iter().all(|read| read.method == "test_balanceOf"));
        assert_eq!(power_calls.load(AtomicOrdering::SeqCst), 0);
        crate::domain::statement::verify(&statement).unwrap();
        assert_eq!(get(&state, &statement.id).await.unwrap(), statement);
    }
    #[tokio::test]
    async fn statement_normalizes_evm_wallet_identity_but_preserves_case_sensitive_addresses() {
        let token = "0x0000000000000000000000000000000000000011";
        let (state, statement_reads, _) = statement_context(vec![entry(token, "NVDA")], false);
        let mixed_case = "0xAa00000000000000000000000000000000000001";
        let normalized = mixed_case.to_ascii_lowercase();
        let statement = create(&state, request(vec![mixed_case.to_owned()])).await.unwrap();

        assert_eq!(statement.wallets[0].address, normalized);
        assert_eq!(statement.assets[0].wallet, normalized);
        assert_eq!(statement.positions[0].wallet, normalized);
        assert_eq!(statement.reads[0].params["owner"], normalized);
        assert!(matches!(
            create(&state, request(vec![mixed_case.to_owned(), normalized.clone()])).await,
            Err(StatementError::InvalidRequest)
        ));
        assert_eq!(statement_reads.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(
            normalize_wallets(&["SoLaNaCase".to_owned(), "solanacase".to_owned()]).unwrap(),
            ["SoLaNaCase", "solanacase"]
        );
    }

    #[tokio::test]
    async fn statement_uses_cached_powers_without_reading_the_chain() {
        let token = "0x0000000000000000000000000000000000000011";
        let (state, _, power_calls) = statement_context(vec![entry(token, "NVDA")], false);
        state
            .powers_cache
            .insert(
                (Chain::Base, token.to_owned(), 0),
                PowersRecord {
                    chain: Chain::Base,
                    contract: token.to_owned(),
                    can_seize: vec![Reason::new("admin", "cached issuer authority")],
                    can_block: Vec::new(),
                    can_change_rules: Vec::new(),
                    token_paused: None,
                    sanctions_list: None,
                    unavailable: Vec::new(),
                    source_verified_subject: SourceVerifiedSubject::Contract,
                    source_verified: SourceVerified::None,
                    source_verified_proxy: None,
                    observed_at: "2026-10-04T00:00:00Z".to_owned(),
                    block: Some(100),
                    slot: None,
                    reads: Vec::new(),
                },
            )
            .await;
        let statement =
            create(&state, request(vec!["0x0000000000000000000000000000000000000001".to_owned()]))
                .await
                .unwrap();
        assert_eq!(
            statement.assets[0].powers_summary.can_seize[0].detail,
            "cached issuer authority"
        );
        assert_eq!(statement.assets[0].powers_observed_at.as_deref(), Some("2026-10-04T00:00:00Z"));
        assert_eq!(statement.assets[0].powers_block, Some(100));
        assert_eq!(statement.assets[0].powers_slot, None);
        assert_eq!(power_calls.load(AtomicOrdering::SeqCst), 0);
    }

    #[tokio::test]
    async fn statement_returns_read_failures_without_signing_partial_data() {
        let token = "0x0000000000000000000000000000000000000011";
        let (state, _, _) = statement_context(vec![entry(token, "NVDA")], true);
        let error =
            create(&state, request(vec!["0x0000000000000000000000000000000000000001".to_owned()]))
                .await
                .unwrap_err();
        assert!(matches!(error, StatementError::ReadFailed(Chain::Base)));
    }

    #[tokio::test]
    async fn statement_rejects_wallets_that_do_not_match_any_selected_chain() {
        let token = "0x0000000000000000000000000000000000000011";
        let (state, statement_reads, _) = statement_context(vec![entry(token, "NVDA")], false);
        let error = create(&state, request(vec!["11111111111111111111111111111111".to_owned()]))
            .await
            .unwrap_err();
        assert!(matches!(error, StatementError::UnknownWallet));
        assert_eq!(statement_reads.load(AtomicOrdering::SeqCst), 0);
    }

    #[tokio::test]
    async fn statement_mock_reader_receives_large_registry_in_one_chain_read() {
        let entries = (0..3_278)
            .map(|index| entry(&format!("0x{index:040x}"), &format!("T{index}")))
            .collect();
        let (state, statement_reads, _) = statement_context(entries, false);
        create(&state, request(vec!["0x0000000000000000000000000000000000000001".to_owned()]))
            .await
            .unwrap();
        assert_eq!(statement_reads.load(AtomicOrdering::SeqCst), 1);
    }

    #[test]
    fn rejects_empty_duplicate_or_unsupported_selections() {
        assert!(parse_chains(&[]).is_err());
        assert!(parse_chains(&["unknown".to_owned()]).is_err());
        assert!(parse_chains(&["base".to_owned(), "BASE".to_owned()]).is_err());
        assert!(normalize_wallets(&[]).is_err());
        let wallet = "0x0000000000000000000000000000000000000001".to_owned();
        assert!(normalize_wallets(&[wallet.clone(), wallet]).is_err());
    }

    #[test]
    fn accepts_wallets_only_on_address_compatible_selected_chains() {
        let solana = "11111111111111111111111111111111";
        let evm = "0x0000000000000000000000000000000000000001";
        assert!(address_matches_chain(solana, Chain::Solana));
        assert!(!address_matches_chain(solana, Chain::Base));
        assert!(address_matches_chain(evm, Chain::Base));
        assert!(!address_matches_chain(evm, Chain::Solana));
    }

    #[test]
    fn accepts_supported_chains_and_wallet_count_boundary() {
        assert_eq!(
            parse_chains(&["base".to_owned(), "solana".to_owned()]).unwrap(),
            vec![Chain::Base, Chain::Solana]
        );
        let at_limit = (1..=32).map(|address| format!("0x{address:040x}")).collect::<Vec<_>>();
        let over_limit = (1..=33).map(|address| format!("0x{address:040x}")).collect::<Vec<_>>();
        assert!(normalize_wallets(&at_limit).is_ok());
        assert!(normalize_wallets(&over_limit).is_err());
    }
}
