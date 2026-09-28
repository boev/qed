use super::Entry;
#[cfg(test)]
use super::XSTOCKS_URL;
use crate::chain::Chain;
use reqwest::Client;
use serde::Deserialize;
use thiserror::Error;
use tokio::time::{Duration, sleep};

#[derive(Debug, Error)]
pub enum Error {
    #[error("xStocks request failed")]
    Request(#[from] reqwest::Error),
    #[error("xStocks response was invalid JSON")]
    Json(#[from] serde_json::Error),
    #[error("xStocks response body failed limits: {0}")]
    Body(String),
}

#[derive(Debug, Deserialize)]
struct Response {
    nodes: Vec<Node>,
    #[serde(default)]
    page: Option<PageInfo>,
}

#[derive(Debug, Deserialize)]
struct PageInfo {
    #[serde(rename = "hasNextPage")]
    has_next_page: bool,
}

#[derive(Debug, Deserialize)]
struct Node {
    name: String,
    symbol: String,
    #[serde(rename = "underlyingSymbol")]
    underlying_symbol: Option<String>,
    deployments: Vec<Deployment>,
}

#[derive(Debug, Deserialize)]
struct Deployment {
    address: String,
    network: String,
    decimals: Option<u8>,
}

pub async fn fetch(client: &Client, endpoint_url: &str) -> Result<Vec<Entry>, Error> {
    const MAX_PAGES: usize = 20;
    let checked_at = super::now_rfc3339();
    let mut nodes = Vec::new();
    for page in 0..MAX_PAGES {
        let response = client
            .get(endpoint_url)
            .header(reqwest::header::USER_AGENT, "qed/0.1")
            .query(&[("page", page)])
            .send()
            .await?
            .error_for_status()?;
        let body = crate::net::body(response).await.map_err(Error::Body)?;
        let response: Response = serde_json::from_slice(&body)?;
        let has_next_page = response.page.as_ref().is_some_and(|page| page.has_next_page);
        nodes.extend(response.nodes);
        if !has_next_page {
            break;
        }
        sleep(Duration::from_millis(50)).await;
    }
    map_nodes_with_source(nodes, &checked_at, endpoint_url)
}

#[cfg(test)]
fn map_payload(payload: &str, checked_at: &str) -> Result<Vec<Entry>, Error> {
    let response: Response = serde_json::from_str(payload)?;
    map_nodes(response.nodes, checked_at)
}

#[cfg(test)]
fn map_nodes(nodes: Vec<Node>, checked_at: &str) -> Result<Vec<Entry>, Error> {
    map_nodes_with_source(nodes, checked_at, XSTOCKS_URL)
}

fn map_nodes_with_source(
    nodes: Vec<Node>,
    checked_at: &str,
    source_url: &str,
) -> Result<Vec<Entry>, Error> {
    let mut entries = Vec::new();
    for node in nodes {
        let ticker =
            node.underlying_symbol.filter(|value| !value.is_empty()).unwrap_or(node.symbol);
        let name = node.name.strip_suffix(" xStock").unwrap_or(&node.name).to_owned();
        for deployment in node.deployments {
            let Some(chain) = chain_for_network(&deployment.network) else {
                continue;
            };
            entries.push(Entry {
                issuer: "Backed xStocks".to_owned(),
                ticker: ticker.clone(),
                name: name.clone(),
                chain,
                contract: super::canonical_contract(chain, deployment.address),
                decimals: deployment.decimals,
                source: "xstocks-api".to_owned(),
                source_url: source_url.to_owned(),
                last_checked: checked_at.to_owned(),
                removed_at: None,
                stale_since: None,
            });
        }
    }
    Ok(entries)
}

fn chain_for_network(network: &str) -> Option<Chain> {
    match network.to_ascii_lowercase().replace([' ', '_', '-'], "").as_str() {
        "solana" => Some(Chain::Solana),
        "robinhoodchain" => Some(Chain::RobinhoodChain),
        "base" => Some(Chain::Base),
        "ethereum" | "mainnet" => Some(Chain::Ethereum),
        "binancesmartchain" | "bsc" | "bnb" | "bnbchain" => Some(Chain::Bnb),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::map_payload;
    use crate::chain::Chain;

    #[test]
    fn maps_supported_deployments() {
        let payload = include_str!("../../tests/fixtures/xstocks.json");
        let entries = map_payload(payload, "2026-09-21T00:00:00Z").unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].issuer, "Backed xStocks");
        assert_eq!(entries[0].ticker, "NVDA");
        assert_eq!(entries[0].chain, Chain::Solana);
        assert_eq!(entries[1].chain, Chain::Ethereum);
    }

    #[test]
    fn merges_paginated_fixture_pages() {
        let first: super::Response =
            serde_json::from_str(include_str!("../../tests/fixtures/xstocks-page-0.json"))
                .expect("first page");
        let second: super::Response =
            serde_json::from_str(include_str!("../../tests/fixtures/xstocks-page-1.json"))
                .expect("second page");
        let mut nodes = first.nodes;
        nodes.extend(second.nodes);
        let entries = super::map_nodes(nodes, "2026-09-22T00:00:00Z").expect("entries");
        assert!(entries.len() > 100);
        assert!(entries.iter().any(|entry| entry.ticker == "NVDAx"));
        assert!(
            entries
                .iter()
                .any(|entry| entry.contract == "Xsc9qvGR1efVDFGLrVsmkzv3qi45LTBjeUKSPmx9qEh")
        );
    }
}
