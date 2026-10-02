use super::Entry;
#[cfg(test)]
use super::ONDO_URL;
use crate::chain::Chain;
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
    let body = crate::net::body(response).await.map_err(Error::Body)?;
    map_payload_with_source(
        std::str::from_utf8(&body).map_err(|_| Error::Body("response was not UTF-8".to_owned()))?,
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
        let inherited_name =
            string(&record, &["underlyingName", "displayName", "name", "tokenName", "description"]);
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
            let Some(contract) =
                string(deployment, &["address", "contract", "contractAddress", "tokenAddress"])
            else {
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
                "bnb" | "bnbchain" | "binancesmartchain" | "bsc" | "bsc56" => Some(Chain::Bnb),
                "base" => Some(Chain::Base),
                "robinhoodchain" => Some(Chain::RobinhoodChain),
                "solana" | "solana900" => Some(Chain::Solana),
                _ => string.parse::<u64>().ok().and_then(|id| chain(Some(&Value::from(id)))),
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{Error, fetch, map_payload};
    use crate::chain::Chain;
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
        let error = fetch(&reqwest::Client::new(), "http://127.0.0.1:1/not-requested", None)
            .await
            .unwrap_err();
        assert!(matches!(&error, Error::MissingApiKey));
        assert_eq!(error.to_string(), "QED_REGISTRY_ONDO_API_KEY is unset; Ondo refresh skipped");
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
        let error = fetch(&reqwest::Client::new(), &url, Some("test-api-key")).await.unwrap_err();
        let request = server.await.expect("mock server");
        assert!(request.contains("x-api-key: test-api-key"));
        assert_eq!(error.to_string(), "Ondo API returned HTTP 403");
    }
}
