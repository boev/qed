use crate::domain::chain::Chain;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TokenSide {
    pub address: String,
    pub symbol: Option<String>,
    pub decimals: Option<u8>,
    /// Raw token units represented as a decimal string.
    pub balance: Option<String>,
}
/// ERC-20 metadata used when evaluating a stock-linked token.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenMeta {
    pub address: String,
    pub symbol: Option<String>,
    pub name: Option<String>,
    pub decimals: Option<u8>,
    /// Raw total supply represented as a decimal string.
    pub total_supply: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WalletHolding {
    pub chain: Chain,
    pub token_address: String,
    pub symbol: Option<String>,
    pub amount: String,
    pub decimals: Option<u8>,
}

#[derive(Debug, Clone)]
pub(crate) struct IndexedPool {
    pub(crate) chain: Chain,
    pub(crate) token_address: String,
    pub(crate) symbol: Option<String>,
    pub(crate) pool: String,
    pub(crate) pool_url: String,
    pub(crate) trade_url: String,
    pub(crate) venue: String,
    pub(crate) quote_address: String,
    pub(crate) quote_symbol: Option<String>,
    pub(crate) verdict: String,
    pub(crate) observed_at: String,
}
const MAX_TOKEN_TEXT_BYTES: usize = 256;

pub(crate) fn cap_token_text(value: Option<String>) -> Option<String> {
    value.map(|mut value| {
        if value.len() > MAX_TOKEN_TEXT_BYTES {
            let mut end = MAX_TOKEN_TEXT_BYTES;
            while !value.is_char_boundary(end) {
                end -= 1;
            }
            value.truncate(end);
        }
        value
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PoolInfo {
    pub chain: Chain,
    pub pool: String,
    /// One of pumpswap, raydium-amm, raydium-cpmm, raydium-clmm,
    /// orca-whirlpool, meteora-dlmm, uniswap-v2, uniswap-v3, uniswap-v4,
    /// or pancake-v3.
    pub dex: String,
    pub base: TokenSide,
    pub quote: TokenSide,
}

#[derive(Debug, Error)]
pub enum PoolError {
    #[error("pool address is not a supported address format")]
    InvalidAddress,
    #[error("pool is unknown: {0}")]
    Unknown(String),
    #[error("pool venue is not supported: {0}")]
    UnsupportedVenue(String),
    #[error("RPC log-query budget exhausted: {0}")]
    RpcLimit(&'static str),
    #[error("RPC provider does not support bytecode detection")]
    CodeLookupUnsupported,
    #[error("pool discovery budget exhausted: {0}")]
    BudgetExceeded(&'static str),
    #[error("pool reader failed: {0}")]
    Reader(String),
}
