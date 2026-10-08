use serde::{Deserialize, Serialize};
use std::fmt;

/// Supported networks. Robinhood Chain mainnet is chain ID 4663.
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Chain {
    #[serde(rename = "solana", alias = "Solana")]
    Solana,
    #[serde(rename = "robinhood", alias = "RobinhoodChain")]
    RobinhoodChain,
    #[serde(rename = "base", alias = "Base")]
    Base,
    #[serde(rename = "ethereum", alias = "Ethereum")]
    Ethereum,
    #[serde(rename = "bnb", alias = "Bnb")]
    Bnb,
}

impl Chain {
    pub const MAX_SOLANA_ADDRESS_CHARS: usize = 44;

    /// Decode a Solana public key without allocating in proportion to untrusted input.
    pub(crate) fn decode_solana_address(address: &str) -> Option<[u8; 32]> {
        if address.len() > Self::MAX_SOLANA_ADDRESS_CHARS {
            return None;
        }
        let mut bytes = [0; 32];
        let decoded = bs58::decode(address).onto(&mut bytes).ok()?;
        (decoded == bytes.len()).then_some(bytes)
    }

    /// Classify an address by its wire format.
    ///
    /// A valid Solana public key identifies Solana. EVM addresses are
    /// intentionally returned as `None`: the same 20-byte address can exist
    /// on every EVM network, so the application must ask
    /// configured RPC providers in priority order before choosing a chain.
    pub fn detect(address: &str) -> Option<Self> {
        if Self::decode_solana_address(address).is_some() {
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
        Self::from_network_name(value)
    }

    pub fn from_network_name(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().replace(['-', '_', ' '], "").as_str() {
            "solana" | "solana900" | "900" => Some(Self::Solana),
            "robinhood" | "robinhoodchain" | "rh" | "robinhoodchain4663" | "4663" => {
                Some(Self::RobinhoodChain)
            }
            "base" | "base8453" | "8453" => Some(Self::Base),
            "ethereum" | "ethereum1" | "mainnet" | "eth" | "eth1" | "1" => Some(Self::Ethereum),
            "bnb"
            | "bnbchain"
            | "binance"
            | "binancesmartchain"
            | "binancesmartchain56"
            | "bsc"
            | "bsc56"
            | "bnb56"
            | "56" => Some(Self::Bnb),
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
    fn serializes_canonical_chain_slugs_and_accepts_legacy_signed_names() {
        for (chain, slug, legacy_name) in [
            (Chain::Solana, "solana", "Solana"),
            (Chain::RobinhoodChain, "robinhood", "RobinhoodChain"),
            (Chain::Base, "base", "Base"),
            (Chain::Ethereum, "ethereum", "Ethereum"),
            (Chain::Bnb, "bnb", "Bnb"),
        ] {
            assert_eq!(serde_json::to_value(chain).unwrap(), serde_json::json!(slug));
            assert_eq!(
                serde_json::from_str::<Chain>(&format!("\"{legacy_name}\"")).unwrap(),
                chain
            );
        }
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
