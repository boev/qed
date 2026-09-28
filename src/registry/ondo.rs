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
    #[error("Ondo response was invalid JSON")]
    Json(#[from] serde_json::Error),
    #[error("Ondo response body failed limits: {0}")]
    Body(String),
}

/// The Ondo endpoint is access-controlled in some environments. A 403 is
/// returned to the caller so startup can retain the committed registry and
/// report that Ondo remains manual until public metadata is available.
pub async fn fetch(client: &Client, endpoint_url: &str) -> Result<Vec<Entry>, Error> {
    let response = client.get(endpoint_url).send().await?.error_for_status()?;
    let body = crate::net::body(response).await.map_err(Error::Body)?;
    map_payload_with_source(
        std::str::from_utf8(&body).map_err(|_| Error::Body("response was not UTF-8".to_owned()))?,
        &super::now_rfc3339(),
        endpoint_url,
    )
}

/// Map the public Ondo metadata shape. The endpoint has used both a top-level
/// array and `{data: [...]}` over time, so aliases are accepted deliberately.
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
        let inherited_name = string(&record, &["name", "tokenName", "description"]);
        let inherited_chain = chain(
            record
                .get("chain")
                .or_else(|| record.get("network"))
                .or_else(|| record.get("blockchain"))
                .or_else(|| record.get("chainId")),
        );
        let inherited_decimals = number(&record, &["decimals", "tokenDecimals"])
            .and_then(|value| u8::try_from(value).ok());
        let deployments = record
            .get("deployments")
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
                    .or_else(|| deployment.get("blockchain"))
                    .or_else(|| deployment.get("chainId")),
            )
            .or(inherited_chain);
            let Some(chain) = chain else {
                continue;
            };
            let name = string(deployment, &["name", "tokenName", "description"])
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
                "ethereum" | "mainnet" | "eth" => Some(Chain::Ethereum),
                "bnb" | "bnbchain" | "binancesmartchain" | "bsc" => Some(Chain::Bnb),
                "base" => Some(Chain::Base),
                "robinhoodchain" => Some(Chain::RobinhoodChain),
                "solana" => Some(Chain::Solana),
                _ => string.parse::<u64>().ok().and_then(|id| chain(Some(&Value::from(id)))),
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::map_payload;
    use crate::chain::Chain;

    #[test]
    fn maps_data_array_and_deployments() {
        let payload = include_str!("../../tests/fixtures/ondo.json");
        let entries = map_payload(payload, "2026-09-21T00:00:00Z").unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].chain, Chain::Ethereum);
        assert_eq!(entries[0].ticker, "OUSG");
        assert_eq!(entries[1].chain, Chain::Bnb);
    }
}
