use crate::{
    app::{check, context::Context},
    domain::{
        chain::Chain,
        check::CheckResult,
        pool::{IndexedPool, PoolError, WalletHolding},
        registry::{self, Entry},
    },
};
use std::time::Duration;
use thiserror::Error;

pub(crate) struct WalletHoldingRow {
    pub(crate) holding: WalletHolding,
    pub(crate) entry: Option<Entry>,
    pub(crate) pools: Vec<IndexedPool>,
    pub(crate) verdict: Option<CheckResult>,
}

const WALLET_DEADLINE: Duration = Duration::from_secs(20);

#[derive(Debug, Error)]
pub(crate) enum WalletError {
    #[error("wallet address is invalid or unsupported")]
    InvalidAddress,
    #[error("wallet scan deadline exceeded")]
    DeadlineExceeded,
    #[error("wallet RPC budget exceeded")]
    BudgetExceeded,
    #[error("wallet balance read failed")]
    ReaderUnavailable,
}

pub(crate) async fn wallet_holdings(
    state: &Context,
    address: &str,
) -> Result<Vec<WalletHoldingRow>, WalletError> {
    let deadline = tokio::time::Instant::now() + WALLET_DEADLINE;
    tokio::time::timeout_at(deadline, wallet_holdings_inner(state, address))
        .await
        .map_err(|_| WalletError::DeadlineExceeded)?
}

async fn wallet_holdings_inner(
    state: &Context,
    address: &str,
) -> Result<Vec<WalletHoldingRow>, WalletError> {
    if address.is_empty() || address.len() > 128 {
        return Err(WalletError::InvalidAddress);
    }
    let Some(chains) = wallet_chains(address) else {
        return Err(WalletError::InvalidAddress);
    };
    let registry = state.registry.snapshot().await;
    let attestations =
        state.attestations.read().ok().map(|value| value.clone()).unwrap_or_default();
    let known_pools = state.pool_index.known_pools(&attestations).await;
    let (solana, robinhood, base, ethereum, bnb) = tokio::join!(
        wallet_chain_holdings(
            state,
            address,
            Chain::Solana,
            chains.contains(&Chain::Solana),
            &registry,
            &known_pools,
        ),
        wallet_chain_holdings(
            state,
            address,
            Chain::RobinhoodChain,
            chains.contains(&Chain::RobinhoodChain),
            &registry,
            &known_pools,
        ),
        wallet_chain_holdings(
            state,
            address,
            Chain::Base,
            chains.contains(&Chain::Base),
            &registry,
            &known_pools,
        ),
        wallet_chain_holdings(
            state,
            address,
            Chain::Ethereum,
            chains.contains(&Chain::Ethereum),
            &registry,
            &known_pools,
        ),
        wallet_chain_holdings(
            state,
            address,
            Chain::Bnb,
            chains.contains(&Chain::Bnb),
            &registry,
            &known_pools,
        ),
    );
    let mut rows = Vec::new();
    for chain_rows in [solana, robinhood, base, ethereum, bnb] {
        rows.extend(chain_rows?);
    }
    Ok(rows)
}

async fn wallet_chain_holdings(
    state: &Context,
    address: &str,
    chain: Chain,
    enabled: bool,
    registry: &registry::Registry,
    known_pools: &[IndexedPool],
) -> Result<Vec<WalletHoldingRow>, WalletError> {
    if !enabled {
        return Ok(Vec::new());
    }
    let Some(reader) = state.readers.iter().find(|reader| reader.chain() == chain) else {
        return Ok(Vec::new());
    };
    let mut known_tokens: Vec<String> = registry
        .iter()
        .filter(|entry| entry.chain == chain && registry::matchable(entry))
        .map(|entry| entry.contract.clone())
        .collect();
    known_tokens.extend(
        known_pools
            .iter()
            .filter(|pool| pool.chain == chain)
            .map(|pool| pool.token_address.clone()),
    );
    known_tokens.sort();
    known_tokens.dedup();
    let holdings = reader.wallet_holdings(address, &known_tokens).await.map_err(|error| {
        if matches!(error, PoolError::BudgetExceeded(_)) {
            WalletError::BudgetExceeded
        } else {
            WalletError::ReaderUnavailable
        }
    })?;
    let mut rows = Vec::with_capacity(holdings.len());
    for holding in holdings {
        let entry = registry::lookup(registry, holding.chain, &holding.token_address).cloned();
        let pools = known_pools
            .iter()
            .filter(|pool| {
                pool.chain == holding.chain
                    && same_wallet_token(&pool.token_address, &holding.token_address, holding.chain)
            })
            .cloned()
            .collect::<Vec<_>>();
        let verdict = if entry.is_none() {
            if let Some(pool) = pools.first() {
                Some(check::check(state, &pool.pool).await)
            } else {
                None
            }
        } else {
            None
        };
        rows.push(WalletHoldingRow { holding, entry, pools, verdict });
    }
    Ok(rows)
}
fn same_wallet_token(left: &str, right: &str, chain: Chain) -> bool {
    if chain == Chain::Solana { left == right } else { left.eq_ignore_ascii_case(right) }
}

pub(crate) fn wallet_chains(address: &str) -> Option<Vec<Chain>> {
    if address.is_empty() || address.len() > 128 {
        return None;
    }
    if Chain::detect(address) == Some(Chain::Solana) {
        Some(vec![Chain::Solana])
    } else if Chain::is_evm_address(address) {
        Some(vec![Chain::RobinhoodChain, Chain::Base, Chain::Ethereum, Chain::Bnb])
    } else {
        None
    }
}
