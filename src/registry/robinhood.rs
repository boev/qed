use super::Entry;
#[cfg(test)]
use super::ROBINHOOD_URL;
use crate::chain::Chain;
use reqwest::Client;
use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Robinhood request failed")]
    Request(#[from] reqwest::Error),
    #[error("Robinhood response was invalid JSON")]
    Json(#[from] serde_json::Error),
    #[error("Robinhood response body failed limits: {0}")]
    Body(String),
}

#[derive(Debug, Deserialize)]
struct Response {
    assets: Vec<Asset>,
}

#[derive(Debug, Deserialize)]
struct Asset {
    #[serde(rename = "tokenSymbol")]
    token_symbol: String,
    #[serde(rename = "tokenName")]
    token_name: String,
    deployments: Vec<Deployment>,
    #[serde(rename = "tokenDecimals")]
    token_decimals: Option<u8>,
    status: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Deployment {
    #[serde(rename = "contractAddress")]
    contract_address: String,
    #[serde(rename = "chainId")]
    chain_id: u64,
}

pub async fn fetch(client: &Client, endpoint_url: &str) -> Result<Vec<Entry>, Error> {
    let response = client.get(endpoint_url).send().await?.error_for_status()?;
    let body = crate::net::body(response).await.map_err(Error::Body)?;
    map_payload_with_source(
        std::str::from_utf8(&body).map_err(|_| Error::Body("response was not UTF-8".to_owned()))?,
        &super::now_rfc3339(),
        endpoint_url,
    )
}

#[cfg(test)]
pub fn map_payload(payload: &str, checked_at: &str) -> Result<Vec<Entry>, Error> {
    map_payload_with_source(payload, checked_at, ROBINHOOD_URL)
}

fn map_payload_with_source(
    payload: &str,
    checked_at: &str,
    source_url: &str,
) -> Result<Vec<Entry>, Error> {
    let response: Response = serde_json::from_str(payload)?;
    let mut entries = Vec::new();
    for asset in response.assets {
        if asset.status.as_deref().is_some_and(|status| status != "ASSET_STATUS_ACTIVE") {
            continue;
        }
        let name = asset
            .token_name
            .strip_suffix(" • Robinhood Token")
            .unwrap_or(&asset.token_name)
            .to_owned();
        for deployment in asset.deployments {
            if deployment.chain_id != Chain::ROBINHOOD_CHAIN_ID {
                continue;
            }
            entries.push(Entry {
                issuer: "Robinhood".to_owned(),
                ticker: asset.token_symbol.clone(),
                name: name.clone(),
                chain: Chain::RobinhoodChain,
                contract: super::canonical_contract(
                    Chain::RobinhoodChain,
                    deployment.contract_address,
                ),
                decimals: asset.token_decimals,
                source: "robinhood-registry".to_owned(),
                source_url: source_url.to_owned(),
                last_checked: checked_at.to_owned(),
                removed_at: None,
                stale_since: None,
            });
        }
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::map_payload;
    use crate::chain::Chain;

    #[test]
    fn maps_live_asset_deployment() {
        let payload = include_str!("../../tests/fixtures/robinhood.json");
        let entries = map_payload(payload, "2026-09-21T00:00:00Z").unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].chain, Chain::RobinhoodChain);
        assert_eq!(entries[0].name, "NVIDIA");
        assert_eq!(entries[0].decimals, Some(18));
    }
}
