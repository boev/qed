use std::env;

use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::PathBuf;
use thiserror::Error;

const DEFAULT_SOLANA_RPC: &str = "https://api.mainnet-beta.solana.com";
const DEFAULT_ROBINHOOD_RPC: &str = "https://rpc.mainnet.chain.robinhood.com";
const DEFAULT_BASE_RPC: &str = "https://mainnet.base.org";
const DEFAULT_ETHEREUM_RPC: &str = "https://ethereum-rpc.publicnode.com";
const DEFAULT_BNB_RPC: &str = "https://bsc-dataseed.binance.org";
pub struct Config {
    pub bind: SocketAddr,
    pub rpc_solana: String,
    pub rpc_robinhood: String,
    pub rpc_base: String,
    pub rpc_ethereum: String,
    pub rpc_bnb: String,
    pub rpc_rps_solana: u32,
    pub rpc_rps_evm: u32,
    pub registry_xstocks_url: String,
    pub registry_ondo_url: String,
    pub registry_ondo_api_key: Option<String>,
    pub registry_robinhood_url: String,
    /// Read-only seed registry committed with the source tree.
    pub registry_path: PathBuf,
    /// Writable runtime data directory for refreshed registry and attestations.
    pub data_dir: PathBuf,
    pub public_url: String,
    pub attest_bucket: Option<String>,
    pub admin_username: Option<String>,
    pub admin_password: Option<String>,
    pub trusted_signers: HashSet<String>,
}
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("QED_BIND is not a valid socket address: {0}")]
    Bind(#[from] std::net::AddrParseError),
    #[error("{name} must be a positive integer, got {value:?}")]
    RpcRps { name: &'static str, value: String },
    #[error("{name} must be configured to a non-public provider in production")]
    PublicRpc { name: &'static str },
    #[error("{name} must be an absolute HTTPS URL with a host in production")]
    InvalidRpcUrl { name: &'static str },
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        let data_dir = PathBuf::from(value("QED_DATA_DIR", "./data"));
        let rpc_solana = value("QED_RPC_SOLANA", DEFAULT_SOLANA_RPC);
        let rpc_robinhood = value("QED_RPC_ROBINHOOD", DEFAULT_ROBINHOOD_RPC);
        let rpc_base = value("QED_RPC_BASE", DEFAULT_BASE_RPC);
        let rpc_ethereum = value("QED_RPC_ETHEREUM", DEFAULT_ETHEREUM_RPC);
        let rpc_bnb = value("QED_RPC_BNB", DEFAULT_BNB_RPC);
        let registry_xstocks_url =
            value("QED_REGISTRY_XSTOCKS_URL", crate::adapters::registry::XSTOCKS_URL);
        let registry_ondo_url = value("QED_REGISTRY_ONDO_URL", crate::adapters::registry::ONDO_URL);
        let registry_ondo_api_key = optional_secret("QED_REGISTRY_ONDO_API_KEY");
        let registry_robinhood_url =
            value("QED_REGISTRY_ROBINHOOD_URL", crate::adapters::registry::ROBINHOOD_URL);
        let trusted_signers = env::var("QED_PREVIOUS_KEYS")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .map(str::to_owned)
            .collect();
        if env::var("QED_ENV").is_ok_and(|value| value.eq_ignore_ascii_case("production")) {
            require_private_rpc("QED_RPC_SOLANA", &rpc_solana, DEFAULT_SOLANA_RPC)?;
            require_private_rpc("QED_RPC_ROBINHOOD", &rpc_robinhood, DEFAULT_ROBINHOOD_RPC)?;
            require_private_rpc("QED_RPC_BASE", &rpc_base, DEFAULT_BASE_RPC)?;
            require_private_rpc("QED_RPC_ETHEREUM", &rpc_ethereum, DEFAULT_ETHEREUM_RPC)?;
            require_private_rpc("QED_RPC_BNB", &rpc_bnb, DEFAULT_BNB_RPC)?;
        }
        Ok(Self {
            bind: value("QED_BIND", "127.0.0.1:3000").parse()?,
            rpc_solana,
            rpc_robinhood,
            rpc_base,
            rpc_ethereum,
            rpc_bnb,
            rpc_rps_solana: rps("QED_RPC_RPS_SOLANA", 4)?,
            rpc_rps_evm: rps("QED_RPC_RPS_EVM", 8)?,
            registry_xstocks_url,
            registry_ondo_url,
            registry_ondo_api_key,
            registry_robinhood_url,
            registry_path: PathBuf::from(value("QED_REGISTRY_PATH", "registry/registry.json")),
            data_dir,
            public_url: value("QED_PUBLIC_URL", "http://localhost:3000")
                .trim_end_matches('/')
                .to_owned(),
            attest_bucket: env::var("QED_ATTEST_BUCKET").ok().filter(|value| !value.is_empty()),
            admin_username: optional_secret("QED_ADMIN_USERNAME"),
            admin_password: optional_secret("QED_ADMIN_PASSWORD"),
            trusted_signers,
        })
    }
}
fn require_private_rpc(
    name: &'static str,
    value: &str,
    public_default: &str,
) -> Result<(), ConfigError> {
    if value.trim().is_empty() || value == public_default {
        return Err(ConfigError::PublicRpc { name });
    }
    let parsed = reqwest::Url::parse(value).map_err(|_| ConfigError::InvalidRpcUrl { name })?;
    if parsed.scheme() != "https" || parsed.host().is_none() {
        return Err(ConfigError::InvalidRpcUrl { name });
    }
    Ok(())
}

fn value(name: &str, default: &str) -> String {
    env::var(name).unwrap_or_else(|_| default.to_owned())
}
fn optional_secret(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.is_empty())
}

fn rps(name: &'static str, default: u32) -> Result<u32, ConfigError> {
    let value = value(name, &default.to_string());
    value.parse().ok().filter(|rps| *rps > 0).ok_or(ConfigError::RpcRps { name, value })
}
#[cfg(test)]
mod tests {
    use super::{ConfigError, require_private_rpc};

    const NAME: &str = "QED_RPC_BASE";
    const PUBLIC_DEFAULT: &str = "https://mainnet.base.org";

    #[test]
    fn production_rpc_rejects_http() {
        assert!(matches!(
            require_private_rpc(NAME, "http://rpc.example.test", PUBLIC_DEFAULT),
            Err(ConfigError::InvalidRpcUrl { name: NAME })
        ));
    }

    #[test]
    fn production_rpc_rejects_malformed_url() {
        assert!(matches!(
            require_private_rpc(NAME, "not-a-url", PUBLIC_DEFAULT),
            Err(ConfigError::InvalidRpcUrl { name: NAME })
        ));
    }

    #[test]
    fn production_rpc_rejects_exact_public_default() {
        assert!(matches!(
            require_private_rpc(NAME, PUBLIC_DEFAULT, PUBLIC_DEFAULT),
            Err(ConfigError::PublicRpc { name: NAME })
        ));
    }

    #[test]
    fn production_rpc_rejects_empty_value() {
        assert!(matches!(
            require_private_rpc(NAME, "", PUBLIC_DEFAULT),
            Err(ConfigError::PublicRpc { name: NAME })
        ));
    }

    #[test]
    fn production_rpc_accepts_https_url_with_host() {
        assert!(require_private_rpc(NAME, "https://rpc.example.test/v1", PUBLIC_DEFAULT).is_ok());
    }
}
