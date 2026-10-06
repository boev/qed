use crate::{
    adapters::net,
    domain::{chain::Chain, powers::SourceVerified},
    ports::{SourceVerifier, record_read},
};
use async_trait::async_trait;
use axum::http::StatusCode;
use serde_json::{Value, json};
use std::time::Duration;

pub(crate) struct SourcifyVerifier {
    source_http: reqwest::Client,
}

impl SourcifyVerifier {
    pub(crate) fn new(source_http: reqwest::Client) -> Self {
        Self { source_http }
    }
}

#[async_trait]
impl SourceVerifier for SourcifyVerifier {
    async fn verify_evm(&self, chain: Chain, contract: &str) -> SourceVerified {
        verify_evm(&self.source_http, chain, contract).await
    }

    async fn verify_solana(&self, program_id: &str) -> SourceVerified {
        verify_solana(&self.source_http, program_id).await
    }
}

async fn verify_evm(source_http: &reqwest::Client, chain: Chain, contract: &str) -> SourceVerified {
    let Some(url) = sourcify_url(chain, contract) else {
        return SourceVerified::Unavailable;
    };
    source_status(source_json(source_http, &url, "sourcify.dev").await)
}

async fn verify_solana(source_http: &reqwest::Client, program_id: &str) -> SourceVerified {
    let url = format!("https://verify.osec.io/status/{program_id}");
    let Some((status, body)) = source_json(source_http, &url, "verify.osec.io").await else {
        return SourceVerified::Unavailable;
    };
    solana_source_status(status, &body)
}
fn sourcify_url(chain: Chain, contract: &str) -> Option<String> {
    Some(format!(
        "https://sourcify.dev/server/v2/contract/{}/{contract}",
        sourcify_chain_id(chain)?
    ))
}

fn sourcify_chain_id(chain: Chain) -> Option<u64> {
    match chain {
        Chain::Ethereum => Some(1),
        Chain::Bnb => Some(56),
        Chain::Base => Some(8453),
        Chain::RobinhoodChain => Some(4663),
        Chain::Solana => None,
    }
}

fn source_status(response: Option<(StatusCode, Value)>) -> SourceVerified {
    let Some((status, body)) = response else {
        return SourceVerified::Unavailable;
    };
    if status == StatusCode::NOT_FOUND {
        return SourceVerified::None;
    }
    if !status.is_success() {
        return SourceVerified::Unavailable;
    }
    match body.get("match").and_then(Value::as_str) {
        Some("exact_match") => SourceVerified::ExactMatch,
        Some("match") => SourceVerified::Match,
        Some("none") => SourceVerified::None,
        _ => SourceVerified::Unavailable,
    }
}

fn solana_source_status(status: StatusCode, body: &Value) -> SourceVerified {
    if status == StatusCode::NOT_FOUND {
        return SourceVerified::None;
    }
    if !status.is_success() {
        return SourceVerified::Unavailable;
    }
    match body.get("is_verified").and_then(Value::as_bool) {
        Some(false) => SourceVerified::None,
        Some(true) => {
            let on_chain = body.get("on_chain_hash").and_then(Value::as_str);
            let executable = body.get("executable_hash").and_then(Value::as_str);
            if on_chain.is_some_and(|hash| !hash.is_empty()) && on_chain == executable {
                SourceVerified::ExactMatch
            } else {
                SourceVerified::Match
            }
        }
        None => SourceVerified::Unavailable,
    }
}

async fn source_json(
    source_http: &reqwest::Client,
    url: &str,
    allowed_host: &str,
) -> Option<(StatusCode, Value)> {
    let allowed_url = net::allowlisted_https_url(url, allowed_host)?;
    let request_url = allowed_url.as_str().to_owned();
    let response = source_http.get(allowed_url).timeout(Duration::from_secs(5)).send().await;
    let Ok(response) = response else {
        let unavailable = json!({"available": false});
        record_read("GET", json!([request_url]), &unavailable, false, None, None);
        return None;
    };
    let status = response.status();
    let value = match net::body(response).await {
        Ok(bytes) => serde_json::from_slice::<Value>(&bytes)
            .unwrap_or_else(|_| json!({"http_status": status.as_u16()})),
        Err(_) => json!({"http_status": status.as_u16(), "available": false}),
    };
    record_read("GET", json!([request_url]), &value, false, None, None);
    Some((status, value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_each_sourcify_result_status() {
        let fixture: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/powers/evm-powers.json"))
                .expect("EVM fixture");
        let sourcify = &fixture["sourcify"];
        let cases = [
            ("exact_match", SourceVerified::ExactMatch),
            ("match", SourceVerified::Match),
            ("not_found", SourceVerified::None),
            ("none", SourceVerified::None),
            ("bad_request", SourceVerified::Unavailable),
            ("server_error", SourceVerified::Unavailable),
        ];
        for (name, expected) in cases {
            let response = &sourcify[name];
            let status =
                StatusCode::from_u16(response["status"].as_u64().expect("fixture status") as u16)
                    .expect("fixture status code");
            assert_eq!(source_status(Some((status, response["body"].clone()))), expected, "{name}");
        }
        assert_eq!(source_status(None), SourceVerified::Unavailable);
    }

    #[test]
    fn builds_sourcify_urls_for_supported_evm_chains() {
        assert_eq!(sourcify_chain_id(Chain::Ethereum), Some(1));
        assert_eq!(sourcify_chain_id(Chain::Bnb), Some(56));
        assert_eq!(sourcify_chain_id(Chain::Base), Some(8453));
        assert_eq!(sourcify_chain_id(Chain::RobinhoodChain), Some(4663));
        assert_eq!(sourcify_chain_id(Chain::Solana), None);
        let url = sourcify_url(Chain::RobinhoodChain, "0xd0601ce157db5bdc3162bbac2a2c8af5320d9eec")
            .expect("supported chain");
        assert_eq!(
            url,
            "https://sourcify.dev/server/v2/contract/4663/0xd0601ce157db5bdc3162bbac2a2c8af5320d9eec"
        );
        assert!(!url.contains("fields="));
    }

    #[test]
    fn maps_verified_build_statuses_without_treating_errors_as_unverified() {
        let matching = json!({"is_verified":true,"on_chain_hash":"abc","executable_hash":"abc"});
        assert_eq!(solana_source_status(StatusCode::OK, &matching), SourceVerified::ExactMatch);
        let different = json!({"is_verified":true,"on_chain_hash":"abc","executable_hash":"def"});
        assert_eq!(solana_source_status(StatusCode::OK, &different), SourceVerified::Match);
        assert_eq!(
            solana_source_status(StatusCode::OK, &json!({"is_verified":false})),
            SourceVerified::None
        );
        assert_eq!(solana_source_status(StatusCode::NOT_FOUND, &json!({})), SourceVerified::None);
        assert_eq!(solana_source_status(StatusCode::OK, &json!({})), SourceVerified::Unavailable);
        assert_eq!(
            solana_source_status(StatusCode::BAD_GATEWAY, &json!({})),
            SourceVerified::Unavailable
        );
    }
}
