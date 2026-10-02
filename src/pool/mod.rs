use crate::chain::Chain;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub mod evm;
pub mod solana;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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
    #[error("RPC provider does not support bytecode detection")]
    CodeLookupUnsupported,
    #[error("pool discovery budget exhausted: {0}")]
    BudgetExceeded(&'static str),
    #[error("pool reader failed: {0}")]
    Reader(String),
}

/// A reader owns one chain's RPC provider and understands that chain's pool
/// account layouts. EVM readers also implement `code_at`, allowing dispatch
/// to distinguish an address that exists on several EVM networks.
#[async_trait]
pub trait PoolReader: Send + Sync {
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
    async fn power_facts(
        &self,
        _address: &str,
    ) -> Result<crate::powers::PowerFacts, PoolError> {
        Err(PoolError::Reader("token powers are unsupported on this chain".to_owned()))
    }


    async fn code_at(&self, _address: &str) -> Result<Vec<u8>, PoolError> {
        Err(PoolError::CodeLookupUnsupported)
    }

    /// Record the chain position used by a check without changing reader APIs.
    async fn record_position(&self) -> Result<(), PoolError> {
        Ok(())
    }
}

/// Ask providers in the required order and return the first chain where the
/// address has non-empty contract bytecode. An RPC error on one provider does
/// not hide a later provider, because the same EVM address can be absent from
/// one network and present on another.
pub async fn detect_evm_chain(
    address: &str,
    readers: &[&dyn PoolReader],
) -> Result<Chain, PoolError> {
    if !Chain::is_evm_address(address) {
        return Err(PoolError::InvalidAddress);
    }
    for chain in [Chain::RobinhoodChain, Chain::Base, Chain::Ethereum, Chain::Bnb] {
        for reader in readers.iter().filter(|reader| reader.chain() == chain) {
            if let Ok(code) = reader.code_at(address).await
                && !code.is_empty()
            {
                return Ok(chain);
            }
        }
    }
    Err(PoolError::Unknown(
        "address has no contract bytecode on configured EVM providers".to_owned(),
    ))
}
