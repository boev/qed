pub(crate) use crate::domain::registry::{Entry, Registry};
use crate::{
    adapters::{discovery, state::AppState},
    app::context::Context,
    domain::registry,
    ports::IssuerRegistry,
};
use alloy::primitives::Address;
use async_trait::async_trait;
use chrono::{SecondsFormat, Utc};
use sha2::Digest;
use std::{
    path::Path,
    sync::{Arc, atomic::Ordering},
};
use thiserror::Error;
use tokio::sync::RwLock;
use tracing::{info, warn};

pub const XSTOCKS_URL: &str = "https://api.xstocks.fi/api/v2/public/assets";
pub const ONDO_URL: &str = "https://api.gm.ondo.finance/v1/assets/all/metadata";
pub const ROBINHOOD_URL: &str = "https://api.robinhood.com/rhj/assets";

#[derive(Clone)]
pub(crate) struct RegistryEndpoints {
    pub(crate) xstocks: String,
    pub(crate) ondo: String,
    pub(crate) ondo_api_key: Option<String>,
    pub(crate) robinhood: String,
}

pub(crate) fn apply_registry_source(
    registry: &mut Registry,
    issuer: &str,
    result: Result<Vec<Entry>, String>,
    checked_at: &str,
) -> (bool, bool) {
    match result {
        Ok(entries) => match registry::reconcile_snapshot(registry, issuer, entries, checked_at) {
            Ok(previous) => {
                info!(issuer, previous, "issuer registry full snapshot accepted");
                (true, true)
            }
            Err(rejected) => {
                let changed = registry::mark_source_failure(registry, issuer, checked_at);
                warn!(
                    issuer,
                    previous = rejected.previous,
                    incoming = rejected.incoming,
                    "issuer registry snapshot rejected as partial"
                );
                (false, changed)
            }
        },
        Err(error) => {
            let changed = registry::mark_source_failure(registry, issuer, checked_at);
            warn!(issuer, %error, "issuer registry refresh failed; previous snapshot retained");
            (false, changed)
        }
    }
}

pub(crate) use crate::domain::registry::{active_count, active_issuers, matchable};
#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("could not read registry file: {0}")]
    Io(#[from] std::io::Error),
    #[error("could not parse registry JSON: {0}")]
    Json(#[from] serde_json::Error),
}

pub fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

pub fn load_from_file(path: impl AsRef<Path>) -> Result<Registry, RegistryError> {
    let bytes = std::fs::read(path)?;
    let mut registry: Registry = serde_json::from_slice(&bytes)?;
    for entry in &mut registry {
        entry.contract = canonical_contract(entry.chain, &entry.contract);
    }
    let checked_at = now_rfc3339();
    let issuers =
        registry.iter().map(|entry| entry.issuer.clone()).collect::<std::collections::HashSet<_>>();
    for issuer in issuers {
        registry::mark_source_failure(&mut registry, &issuer, &checked_at);
    }
    Ok(registry)
}

pub fn save_to_file(path: impl AsRef<Path>, registry: &Registry) -> Result<(), RegistryError> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut sorted = registry.clone();
    sorted.sort_by(|left, right| {
        left.issuer
            .cmp(&right.issuer)
            .then_with(|| left.chain.to_string().cmp(&right.chain.to_string()))
            .then_with(|| left.ticker.cmp(&right.ticker))
            .then_with(|| {
                left.contract.to_ascii_lowercase().cmp(&right.contract.to_ascii_lowercase())
            })
    });
    std::fs::write(path, serde_json::to_vec_pretty(&sorted)?)?;
    Ok(())
}

pub fn canonical_contract(chain: crate::domain::chain::Chain, contract: impl AsRef<str>) -> String {
    let contract = contract.as_ref();
    if chain == crate::domain::chain::Chain::Solana {
        return contract.to_owned();
    }
    contract
        .parse::<Address>()
        .map(|address| address.to_checksum(None))
        .unwrap_or_else(|_| contract.to_owned())
}

#[async_trait]
impl IssuerRegistry for Arc<RwLock<Arc<Registry>>> {
    async fn snapshot(&self) -> Arc<Registry> {
        Arc::clone(&*self.read().await)
    }
}
pub(crate) fn file_hash(path: impl AsRef<Path>) -> String {
    let bytes = std::fs::read(path).unwrap_or_default();
    let mut hasher = sha2::Sha256::new();
    sha2::Digest::update(&mut hasher, bytes);
    sha2::Digest::finalize(hasher).iter().map(|byte| format!("{byte:02x}")).collect()
}
pub(crate) async fn refresh_registry(state: &AppState) {
    let (changed, updated_hash) = refresh_registry_entries(
        &state.registry,
        &state.registry_status,
        &state.registry_endpoints,
        &state.http,
        &state.registry_path,
        Some(&state.app),
    )
    .await;
    if let Some(hash) = updated_hash
        && let Ok(mut current_hash) = state.app.registry_hash.write()
    {
        *current_hash = hash;
    }
    if changed {
        state.powers_warm_notify.notify_one();
    }
}

pub(crate) async fn refresh_registry_entries(
    registry: &RwLock<Arc<Registry>>,
    registry_status: &RwLock<discovery::RegistrySnapshot>,
    endpoints: &RegistryEndpoints,
    http: &reqwest::Client,
    registry_path: &Path,
    invalidate: Option<&Context>,
) -> (bool, Option<String>) {
    {
        let mut status = registry_status.write().await;
        status.next_refresh_at = discovery::timestamp_after(discovery::REGISTRY_REFRESH_SECS);
        status.refreshing = true;
    }
    let (xstocks_result, ondo_result, robinhood_result) = tokio::join!(
        async {
            let result = registries::xstocks::fetch(http, &endpoints.xstocks)
                .await
                .map_err(|error| error.to_string());
            (result, now_rfc3339())
        },
        async {
            let result =
                registries::ondo::fetch(http, &endpoints.ondo, endpoints.ondo_api_key.as_deref())
                    .await
                    .map_err(|error| error.to_string());
            (result, now_rfc3339())
        },
        async {
            let result = registries::robinhood::fetch(http, &endpoints.robinhood)
                .await
                .map_err(|error| error.to_string());
            (result, now_rfc3339())
        },
    );

    let mut current = registry.write().await;
    let mut next = current.as_ref().clone();
    let mut accepted = false;
    let mut changed = false;
    for (issuer, (result, checked_at)) in
        [("Backed xStocks", xstocks_result), ("Ondo", ondo_result), ("Robinhood", robinhood_result)]
    {
        let (source_accepted, source_changed) =
            apply_registry_source(&mut next, issuer, result, &checked_at);
        accepted |= source_accepted;
        changed |= source_changed;
    }
    let snapshot = Arc::new(next);
    if changed && let Some(context) = invalidate {
        // Keep registry readers from observing new entries with cached old verdicts.
        context.registry_version.fetch_add(1, Ordering::AcqRel);
        context.check_cache.invalidate_all();
        context.powers_cache.invalidate_all();
    }
    *current = Arc::clone(&snapshot);
    drop(current);

    let updated_hash = if changed {
        match save_to_file(registry_path, &snapshot) {
            Ok(()) => Some(file_hash(registry_path)),
            Err(error) => {
                warn!(%error, "could not persist reconciled registry");
                None
            }
        }
    } else {
        None
    };
    let mut status = registry_status.write().await;
    status.entries = registry::active_count(&snapshot);
    status.issuers = registry::active_issuers(&snapshot);
    if accepted {
        status.updated_at = now_rfc3339();
        status.restored = false;
    }
    status.refreshing = false;
    (changed, updated_hash)
}
pub(crate) mod registries {
    use crate::adapters::registry::{Entry, canonical_contract, now_rfc3339};
    #[cfg(test)]
    use crate::adapters::registry::{ONDO_URL, ROBINHOOD_URL, XSTOCKS_URL};

    pub(crate) mod ondo {
        use super::Entry;
        #[cfg(test)]
        use super::ONDO_URL;
        use crate::domain::chain::Chain;
        use reqwest::Client;
        use serde_json::{Map, Value};
        use thiserror::Error;

        #[derive(Debug, Error)]
        pub enum Error {
            #[error("Ondo request failed")]
            Request(#[from] reqwest::Error),
            #[error("QED_REGISTRY_ONDO_API_KEY is unset; Ondo refresh skipped")]
            MissingApiKey,
            #[error("Ondo API returned HTTP {0}")]
            HttpStatus(u16),
            #[error("Ondo response was invalid JSON")]
            Json(#[from] serde_json::Error),
            #[error("Ondo response body failed limits: {0}")]
            Body(String),
        }

        /// Ondo requires an API key from its onboarding process; missing credentials
        /// skip this source, and HTTP status errors are reported without response bodies.
        pub async fn fetch(
            client: &Client,
            endpoint_url: &str,
            api_key: Option<&str>,
        ) -> Result<Vec<Entry>, Error> {
            let Some(api_key) = api_key.filter(|key| !key.is_empty()) else {
                return Err(Error::MissingApiKey);
            };
            let response = client.get(endpoint_url).header("x-api-key", api_key).send().await?;
            if !response.status().is_success() {
                return Err(Error::HttpStatus(response.status().as_u16()));
            }
            let body = crate::adapters::net::body(response).await.map_err(Error::Body)?;
            map_payload_with_source(
                std::str::from_utf8(&body)
                    .map_err(|_| Error::Body("response was not UTF-8".to_owned()))?,
                &super::now_rfc3339(),
                endpoint_url,
            )
        }

        /// Map the current Ondo asset metadata shape while retaining older deployment aliases.
        #[cfg(test)]
        pub fn map_payload(payload: &str, checked_at: &str) -> Result<Vec<Entry>, Error> {
            map_payload_with_source(payload, checked_at, ONDO_URL)
        }

        fn map_payload_with_source(
            payload: &str,
            checked_at: &str,
            source_url: &str,
        ) -> Result<Vec<Entry>, Error> {
            let value: Value = serde_json::from_str(payload)?;
            let records = records(&value);
            let mut entries = Vec::new();
            for record in records {
                let inherited_ticker = string(&record, &["ticker", "symbol", "tokenSymbol"]);
                let inherited_name = string(
                    &record,
                    &["underlyingName", "displayName", "name", "tokenName", "description"],
                );
                let inherited_chain = chain(
                    record
                        .get("chain")
                        .or_else(|| record.get("network"))
                        .or_else(|| record.get("networkChainId"))
                        .or_else(|| record.get("blockchain"))
                        .or_else(|| record.get("chainId")),
                );
                let inherited_decimals = number(&record, &["decimals", "tokenDecimals"])
                    .and_then(|value| u8::try_from(value).ok());
                let deployments = record
                    .get("deployments")
                    .or_else(|| record.get("addresses"))
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_else(|| vec![Value::Object(record.clone())]);
                for deployment in deployments {
                    let Some(deployment) = deployment.as_object() else {
                        continue;
                    };
                    let Some(contract) = string(
                        deployment,
                        &["address", "contract", "contractAddress", "tokenAddress"],
                    ) else {
                        continue;
                    };
                    let ticker = string(deployment, &["ticker", "symbol", "tokenSymbol"])
                        .or_else(|| inherited_ticker.clone());
                    let Some(ticker) = ticker else {
                        continue;
                    };
                    let chain = chain(
                        deployment
                            .get("chain")
                            .or_else(|| deployment.get("network"))
                            .or_else(|| deployment.get("networkChainId"))
                            .or_else(|| deployment.get("blockchain"))
                            .or_else(|| deployment.get("chainId")),
                    )
                    .or(inherited_chain);
                    let Some(chain) = chain else {
                        continue;
                    };
                    let name = string(
                        deployment,
                        &["underlyingName", "displayName", "name", "tokenName", "description"],
                    )
                    .or_else(|| inherited_name.clone())
                    .unwrap_or_else(|| ticker.clone());
                    let decimals = number(deployment, &["decimals", "tokenDecimals"])
                        .and_then(|value| u8::try_from(value).ok())
                        .or(inherited_decimals);
                    entries.push(Entry {
                        issuer: "Ondo".to_owned(),
                        ticker,
                        name,
                        chain,
                        contract,
                        decimals,
                        source: "ondo-api".to_owned(),
                        source_url: source_url.to_owned(),
                        last_checked: checked_at.to_owned(),
                        removed_at: None,
                        stale_since: None,
                    });
                }
            }
            Ok(entries)
        }

        fn records(value: &Value) -> Vec<Map<String, Value>> {
            if let Some(array) = value.as_array() {
                return array.iter().filter_map(Value::as_object).cloned().collect();
            }
            let Some(object) = value.as_object() else {
                return Vec::new();
            };
            for key in ["assets", "data", "items", "results"] {
                if let Some(array) = object.get(key).and_then(Value::as_array) {
                    return array.iter().filter_map(Value::as_object).cloned().collect();
                }
            }
            vec![object.clone()]
        }

        fn string(object: &Map<String, Value>, keys: &[&str]) -> Option<String> {
            keys.iter().find_map(|key| object.get(*key).and_then(Value::as_str).map(str::to_owned))
        }

        fn number(object: &Map<String, Value>, keys: &[&str]) -> Option<u64> {
            keys.iter().find_map(|key| {
                object.get(*key).and_then(|value| {
                    value.as_u64().or_else(|| value.as_str().and_then(|string| string.parse().ok()))
                })
            })
        }

        fn chain(value: Option<&Value>) -> Option<Chain> {
            match value {
                Some(Value::Number(number)) => match number.as_u64()? {
                    1 => Some(Chain::Ethereum),
                    56 => Some(Chain::Bnb),
                    8453 => Some(Chain::Base),
                    4663 => Some(Chain::RobinhoodChain),
                    _ => None,
                },
                Some(Value::String(string)) => {
                    match string.to_ascii_lowercase().replace([' ', '_', '-'], "").as_str() {
                        "ethereum" | "ethereum1" | "mainnet" | "eth" => Some(Chain::Ethereum),
                        "bnb" | "bnbchain" | "binancesmartchain" | "bsc" | "bsc56" => {
                            Some(Chain::Bnb)
                        }
                        "base" => Some(Chain::Base),
                        "robinhoodchain" => Some(Chain::RobinhoodChain),
                        "solana" | "solana900" => Some(Chain::Solana),
                        _ => {
                            string.parse::<u64>().ok().and_then(|id| chain(Some(&Value::from(id))))
                        }
                    }
                }
                _ => None,
            }
        }

        #[cfg(test)]
        mod tests {
            use super::{Error, fetch, map_payload};
            use crate::domain::chain::Chain;
            use tokio::{
                io::{AsyncReadExt, AsyncWriteExt},
                net::TcpListener,
            };

            #[test]
            fn maps_current_addresses_and_legacy_deployments() {
                let payload = include_str!("../../tests/fixtures/ondo.json");
                let entries = map_payload(payload, "2026-09-21T00:00:00Z").unwrap();
                assert_eq!(entries.len(), 5);
                assert_eq!(entries[0].ticker, "AAPL");
                assert_eq!(entries[0].name, "Apple");
                assert_eq!(entries[0].chain, Chain::Ethereum);
                assert_eq!(entries[0].decimals, Some(18));
                assert_eq!(entries[1].chain, Chain::Bnb);
                assert_eq!(entries[2].chain, Chain::Solana);
                assert_eq!(entries[3].ticker, "OUSG");
                assert_eq!(entries[3].chain, Chain::Ethereum);
                assert_eq!(entries[4].chain, Chain::Bnb);
            }

            #[tokio::test]
            async fn missing_api_key_skips_ondo_refresh_before_network_access() {
                let error =
                    fetch(&reqwest::Client::new(), "http://127.0.0.1:1/not-requested", None)
                        .await
                        .unwrap_err();
                assert!(matches!(&error, Error::MissingApiKey));
                assert_eq!(
                    error.to_string(),
                    "QED_REGISTRY_ONDO_API_KEY is unset; Ondo refresh skipped"
                );
            }

            #[tokio::test]
            async fn fetch_sends_api_key_and_reports_http_status_without_response_body() {
                let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("listener");
                let address = listener.local_addr().expect("listener address");
                let server = tokio::spawn(async move {
                    let (mut stream, _) = listener.accept().await.expect("request");
                    let mut request = Vec::new();
                    let mut chunk = [0_u8; 1024];
                    loop {
                        let count = stream.read(&mut chunk).await.expect("read request");
                        if count == 0 {
                            break;
                        }
                        request.extend_from_slice(&chunk[..count]);
                        if request.windows(4).any(|window| window == b"\r\n\r\n") {
                            break;
                        }
                    }
                    stream
                .write_all(
                    b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await
                .expect("write response");
                    String::from_utf8_lossy(&request).to_ascii_lowercase()
                });

                let url = format!("http://{address}/metadata");
                let error =
                    fetch(&reqwest::Client::new(), &url, Some("test-api-key")).await.unwrap_err();
                let request = server.await.expect("mock server");
                assert!(request.contains("x-api-key: test-api-key"));
                assert_eq!(error.to_string(), "Ondo API returned HTTP 403");
            }
        }
    }

    pub(crate) mod robinhood {
        use super::Entry;
        #[cfg(test)]
        use super::ROBINHOOD_URL;
        use crate::domain::chain::Chain;
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
            let body = crate::adapters::net::body(response).await.map_err(Error::Body)?;
            map_payload_with_source(
                std::str::from_utf8(&body)
                    .map_err(|_| Error::Body("response was not UTF-8".to_owned()))?,
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
            use crate::domain::chain::Chain;

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
    }

    pub(crate) mod xstocks {
        use super::Entry;
        #[cfg(test)]
        use super::XSTOCKS_URL;
        use crate::domain::chain::Chain;
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
                let body = crate::adapters::net::body(response).await.map_err(Error::Body)?;
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
            use crate::domain::chain::Chain;

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
                    entries.iter().any(
                        |entry| entry.contract == "Xsc9qvGR1efVDFGLrVsmkzv3qi45LTBjeUKSPmx9qEh"
                    )
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Entry, load_from_file, save_to_file};
    use crate::domain::{
        chain::Chain,
        registry::{MatchStatus, lookup, match_status, matchable},
    };
    use chrono::{Duration, SecondsFormat, Utc};

    #[test]
    fn persisted_old_entries_are_not_matchable_at_startup() {
        let directory = tempfile::tempdir().expect("temporary registry directory");
        let path = directory.path().join("registry.json");
        let old = Entry {
            issuer: "Test".to_owned(),
            ticker: "OLD".to_owned(),
            name: "OLD".to_owned(),
            chain: Chain::Ethereum,
            contract: "0xabc".to_owned(),
            decimals: Some(18),
            source: "manual".to_owned(),
            source_url: "https://example.invalid".to_owned(),
            last_checked: (Utc::now() - Duration::hours(49))
                .to_rfc3339_opts(SecondsFormat::Secs, true),
            removed_at: None,
            stale_since: None,
        };
        save_to_file(&path, &vec![old]).expect("write registry");

        let loaded = load_from_file(&path).expect("load registry");

        assert!(matches!(
            match_status(&loaded, Chain::Ethereum, "0xABC"),
            MatchStatus::Stale { .. }
        ));
        assert!(lookup(&loaded, Chain::Ethereum, "0xabc").is_none());
        assert!(!matchable(&loaded[0]));
    }
}
