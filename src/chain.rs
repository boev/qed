use serde::{Deserialize, Serialize};
use std::fmt;

/// Supported networks. Robinhood Chain mainnet is chain ID 4663.
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Chain {
    Solana,
    RobinhoodChain,
    Base,
    Ethereum,
    Bnb,
}

impl Chain {
    pub const ROBINHOOD_CHAIN_ID: u64 = 4663;

    /// Classify an address by its wire format.
    ///
    /// A valid Solana public key identifies Solana. EVM addresses are
    /// intentionally returned as `None`: the same 20-byte address can exist
    /// on every EVM network, so [`crate::pool::detect_evm_chain`] must ask
    /// configured RPC providers in priority order before choosing a chain.
    pub fn detect(address: &str) -> Option<Self> {
        if bs58::decode(address).into_vec().is_ok_and(|bytes| bytes.len() == 32) {
            return Some(Self::Solana);
        }
        if Self::is_evm_address(address) {
            return None;
        }
        None
    }

    pub fn is_evm_address(address: &str) -> bool {
        let Some(hex) = address.strip_prefix("0x") else {
            return false;
        };
        hex.len() == 40 && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
    }
    pub fn is_v4_pool_id(value: &str) -> bool {
        let Some(hex) = value.strip_prefix("0x") else {
            return false;
        };
        hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
    }
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().replace(['-', '_', ' '], "").as_str() {
            "solana" => Some(Self::Solana),
            "robinhood" | "robinhoodchain" | "rh" => Some(Self::RobinhoodChain),
            "base" => Some(Self::Base),
            "ethereum" | "eth" => Some(Self::Ethereum),
            "bnb" | "bnbchain" | "binance" => Some(Self::Bnb),
            _ => None,
        }
    }

}

impl fmt::Display for Chain {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Solana => "Solana",
            Self::RobinhoodChain => "Robinhood Chain",
            Self::Base => "Base",
            Self::Ethereum => "Ethereum",
            Self::Bnb => "BNB Chain",
        };
        formatter.write_str(name)
    }
}

#[cfg(test)]
mod tests {
    use super::Chain;

    #[test]
    fn detects_solana_public_key() {
        assert_eq!(Chain::detect("11111111111111111111111111111111"), Some(Chain::Solana));
    }

    #[test]
    fn recognises_v4_pool_id_without_selecting_a_chain() {
        let pool_id = "0x48cff3c087c11b88bab76488f0d17b3e0c3dab113334e8b7a3d1205f1b31d923";
        assert!(Chain::is_v4_pool_id(pool_id));
        assert_eq!(Chain::detect(pool_id), None);
    }

    #[test]
    fn rejects_malformed_v4_pool_ids() {
        assert!(!Chain::is_v4_pool_id(
            "0X48cff3c087c11b88bab76488f0d17b3e0c3dab113334e8b7a3d1205f1b31d923"
        ));
        assert!(!Chain::is_v4_pool_id(
            "0x48cff3c087c11b88bab76488f0d17b3e0c3dab113334e8b7a3d1205f1b31d92"
        ));
        assert!(!Chain::is_v4_pool_id(
            "0x48cff3c087c11b88bab76488f0d17b3e0c3dab113334e8b7a3d1205f1b31d92z"
        ));
    }
    #[test]
    fn leaves_evm_chain_ambiguous() {
        assert_eq!(Chain::detect("0x0000000000000000000000000000000000000001"), None);
    }


    #[test]
    fn parses_power_chain_filters_and_aliases() {
        for (input, expected) in [
            ("solana", Chain::Solana),
            ("Robinhood Chain", Chain::RobinhoodChain),
            ("robinhood-chain", Chain::RobinhoodChain),
            ("rh", Chain::RobinhoodChain),
            ("base", Chain::Base),
            ("eth", Chain::Ethereum),
            ("BNB Chain", Chain::Bnb),
        ] {
            assert_eq!(Chain::parse(input), Some(expected), "{input}");
        }
        assert_eq!(Chain::parse("avalanche"), None);
    }
    #[test]
    fn rejects_malformed_addresses() {
        assert_eq!(Chain::detect("not-an-address"), None);
        assert!(!Chain::is_evm_address("0X0000000000000000000000000000000000000001"));
    }
}
