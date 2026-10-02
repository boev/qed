use super::pages;
use crate::{attest, check, registry, state::AppState};
use axum::{
    Json,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};

const MODERN_PROTOCOL_VERSION: &str = "2026-07-28";
const SUPPORTED_PROTOCOL_VERSIONS: [&str; 4] =
    [MODERN_PROTOCOL_VERSION, "2025-11-25", "2025-06-18", "2025-03-26"];
const INSTRUCTIONS: &str = "QED checks whether a pool uses the stock-token contract published by its issuer. Re-check the certificate after expiry or when the issuer registry changes.";
#[cfg(test)]
const NON_CLAIMS: &str = "It does not prove backing, custody, reserves, solvency, safety, price, liquidity, or endorsement.";
const LEGACY_PROTOCOL_VERSION: &str = "2025-11-25";

pub(crate) async fn handle(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !valid_origin(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }

    let request: Value = match serde_json::from_slice(&body) {
        Ok(request) => request,
        Err(_) => return rpc_error(Value::Null, -32700, "Parse error", StatusCode::BAD_REQUEST),
    };
    let Some(object) = request.as_object() else {
        return rpc_error(Value::Null, -32600, "Invalid Request", StatusCode::BAD_REQUEST);
    };
    let id = object.get("id").cloned().unwrap_or(Value::Null);
    let Some(method) = object.get("method").and_then(Value::as_str) else {
        return rpc_error(id, -32600, "Invalid Request", StatusCode::BAD_REQUEST);
    };
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
        || object.get("id").is_some_and(|id| !(id.is_string() || id.is_number()))
    {
        return rpc_error(id, -32600, "Invalid Request", StatusCode::BAD_REQUEST);
    }
    let params = object.get("params").unwrap_or(&Value::Null);
    if !params.is_null() && !params.is_object() {
        return rpc_error(id, -32602, "Invalid params", StatusCode::OK);
    }

    let has_protocol_metadata = params
        .get("_meta")
        .and_then(Value::as_object)
        .is_some_and(|meta| meta.contains_key("io.modelcontextprotocol/protocolVersion"));
    let modern_request = has_protocol_metadata
        || header_text(&headers, "MCP-Protocol-Version") == Some(MODERN_PROTOCOL_VERSION);
    if modern_request {
        if let Err(response) = validate_modern_request(&headers, method, params, &id) {
            return response;
        }
    } else if let Some(version) = header_text(&headers, "MCP-Protocol-Version")
        && !SUPPORTED_PROTOCOL_VERSIONS.contains(&version)
        && method != "initialize"
    {
        return unsupported_version_error(id, version, StatusCode::BAD_REQUEST);
    } else if method == "server/discover" {
        return rpc_error(id, -32602, "Missing required request metadata", StatusCode::BAD_REQUEST);
    }
    if !object.contains_key("id") {
        return StatusCode::ACCEPTED.into_response();
    }

    if modern_request && method == "initialize" {
        return rpc_error(id, -32601, "Method not found", StatusCode::NOT_FOUND);
    }

    match method {
        "initialize" => initialize(id, params),
        "server/discover" => rpc_result(id, discover_result(), modern_request),
        "notifications/initialized" => rpc_result(id, json!({}), modern_request),
        "ping" => rpc_result(id, json!({ "resultType": "complete" }), modern_request),
        "tools/list" => rpc_result(
            id,
            json!({
                "resultType": "complete",
                "tools": tool_table(),
            }),
            modern_request,
        ),
        "tools/call" => {
            let Some(params) = params.as_object() else {
                return rpc_error(id, -32602, "Invalid params", StatusCode::OK);
            };
            let Some(name) = params.get("name").and_then(Value::as_str) else {
                return rpc_error(
                    id,
                    -32602,
                    "Invalid params: name must be a string",
                    StatusCode::OK,
                );
            };
            state.usage_stats.record_mcp_tool(name);
            let Some(arguments) = params.get("arguments").filter(|value| value.is_object()) else {
                return rpc_error(
                    id,
                    -32602,
                    "Invalid params: arguments must be an object",
                    StatusCode::OK,
                );
            };
            if let Err(message) = validate_tool_arguments(name, arguments) {
                return rpc_error(id, -32602, message, StatusCode::OK);
            }
            let result = run_tool(&state, name, arguments).await;
            rpc_result(id, result, modern_request)
        }
        _ => rpc_error(id, -32601, "Method not found", StatusCode::NOT_FOUND),
    }
}

fn valid_origin(headers: &HeaderMap) -> bool {
    let Some(origin) = header_text(headers, "Origin") else {
        return true;
    };
    let Ok(origin_uri) = origin.parse::<axum::http::Uri>() else {
        return false;
    };
    let Some(origin_authority) = origin_uri.authority() else {
        return false;
    };
    let Some(host) = header_text(headers, "Host") else {
        return false;
    };
    (origin_uri.path().is_empty() || origin_uri.path() == "/")
        && origin_authority.as_str().eq_ignore_ascii_case(host)
}

fn header_text<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name)?.to_str().ok()
}

fn validate_modern_request(
    headers: &HeaderMap,
    method: &str,
    params: &Value,
    id: &Value,
) -> Result<(), Response> {
    let meta = params.get("_meta").and_then(Value::as_object).ok_or_else(|| {
        rpc_error(
            id.clone(),
            -32602,
            "Invalid params: request metadata must be an object",
            StatusCode::BAD_REQUEST,
        )
    })?;
    let version = meta
        .get("io.modelcontextprotocol/protocolVersion")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            rpc_error(
                id.clone(),
                -32602,
                "Invalid params: protocol version metadata must be a string",
                StatusCode::BAD_REQUEST,
            )
        })?;
    let Some(header_version) = header_text(headers, "MCP-Protocol-Version") else {
        return Err(header_mismatch(id, "Missing MCP-Protocol-Version header"));
    };
    if version != header_version {
        return Err(header_mismatch(id, "Protocol version header does not match request metadata"));
    }
    if !SUPPORTED_PROTOCOL_VERSIONS.contains(&version) {
        return Err(unsupported_version_error(id.clone(), version, StatusCode::BAD_REQUEST));
    }
    if !meta.get("io.modelcontextprotocol/clientCapabilities").is_some_and(Value::is_object) {
        return Err(rpc_error(
            id.clone(),
            -32602,
            "Missing client capabilities metadata",
            StatusCode::BAD_REQUEST,
        ));
    }
    if header_text(headers, "Mcp-Method") != Some(method) {
        return Err(header_mismatch(id, "Mcp-Method header does not match request method"));
    }
    if method == "tools/call" {
        let Some(name) = params.get("name").and_then(Value::as_str) else {
            return Err(rpc_error(
                id.clone(),
                -32602,
                "Invalid params: name must be a string",
                StatusCode::BAD_REQUEST,
            ));
        };
        if header_text(headers, "Mcp-Name") != Some(name) {
            return Err(header_mismatch(id, "Mcp-Name header does not match request name"));
        }
    }
    Ok(())
}

fn initialize(id: Value, params: &Value) -> Response {
    let Some(requested) = params.get("protocolVersion").and_then(Value::as_str) else {
        return rpc_error(
            id,
            -32602,
            "Invalid params: protocolVersion is required",
            StatusCode::OK,
        );
    };
    let version = if SUPPORTED_PROTOCOL_VERSIONS.contains(&requested) {
        requested
    } else {
        LEGACY_PROTOCOL_VERSION
    };
    rpc_result(
        id,
        json!({
            "protocolVersion": version,
            "capabilities": { "tools": {} },
            "serverInfo": {
                "name": "QED",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "instructions": INSTRUCTIONS,
        }),
        false,
    )
}

fn discover_result() -> Value {
    json!({
        "resultType": "complete",
        "supportedVersions": SUPPORTED_PROTOCOL_VERSIONS,
        "capabilities": { "tools": {} },
        "_meta": {
            "io.modelcontextprotocol/serverInfo": {
                "name": "QED",
                "version": env!("CARGO_PKG_VERSION"),
            },
        },
        "instructions": INSTRUCTIONS,
        "ttlMs": 3_600_000,
        "cacheScope": "public",
    })
}

fn tool_table() -> Value {
    json!([
        {
            "name": "qed_check",
            "title": "Check issuer contract match",
            "description": "Check whether a pool or token address matches an issuer's published stock-token contract. It does not prove backing, custody, reserves, solvency, safety, price, liquidity, or endorsement.",
            "annotations": { "readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": true },
            "inputSchema": {
                "type": "object",
                "properties": {
                    "address": { "type": "string", "description": "Pool or token address to check." }
                },
                "required": ["address"],
            },
        },
        {
            "name": "qed_powers",
            "title": "Read token powers",
            "description": "Read supported token authority settings and source-verification status for an active issuer registry contract. It does not prove backing, custody, reserves, solvency, safety, price, liquidity, or endorsement.",
            "annotations": { "readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": true },
            "inputSchema": {
                "type": "object",
                "properties": {
                    "address": { "type": "string", "description": "Active issuer registry token contract address." },
                    "chain": { "type": "string", "enum": ["solana", "robinhood", "base", "ethereum", "bnb"], "description": "Optional chain filter when the address is registered on more than one chain." }
                },
                "required": ["address"],
            },
        },
        {
            "name": "qed_wallet",
            "title": "Read wallet holdings",
            "description": "Read stock-token holdings for a wallet address. It does not prove backing, custody, reserves, solvency, safety, price, liquidity, or endorsement.",
            "annotations": { "readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": true },
            "inputSchema": {
                "type": "object",
                "properties": {
                    "address": { "type": "string", "description": "Wallet address to scan." }
                },
                "required": ["address"],
            },
        },
        {
            "name": "qed_registry_lookup",
            "title": "Look up issuer contracts",
            "description": "Look up active issuer registry contracts for a ticker. It does not prove backing, custody, reserves, solvency, safety, price, liquidity, or endorsement.",
            "annotations": { "readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": true },
            "inputSchema": {
                "type": "object",
                "properties": {
                    "ticker": { "type": "string", "description": "Ticker to look up." }
                },
                "required": ["ticker"],
            },
        },
        {
            "name": "qed_verify",
            "title": "Verify QED certificate",
            "description": "Verify an attestation's signature, trusted signer, environment and freshness. It does not prove backing, custody, reserves, solvency, safety, price, liquidity, or endorsement.",
            "annotations": { "readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": true },
            "inputSchema": {
                "type": "object",
                "properties": {
                    "attestation": { "type": "object", "description": "QED attestation payload." }
                },
                "required": ["attestation"],
            },
        },
    ])
}

pub(crate) async fn server_card(State(state): State<AppState>) -> Json<Value> {
    let tools = tool_table()
        .as_array()
        .into_iter()
        .flatten()
        .map(|tool| {
            json!({
                "name": tool["name"],
                "title": tool["title"],
                "description": tool["description"],
            })
        })
        .collect::<Vec<_>>();
    let website = state.public_url.trim_end_matches('/');
    Json(json!({
        "name": "QED",
        "description": "Read-only checks of whether a pool uses the stock-token contract published by its issuer. This does not prove backing, custody, reserves, solvency, safety, price, liquidity, or endorsement.",
        "serverInfo": { "name": "QED", "version": env!("CARGO_PKG_VERSION") },
        "remotes": [{ "type": "streamable-http", "url": format!("{website}/mcp") }],
        "tools": tools,
        "website": website,
        "repository": "https://github.com/boev/qed",
        "readOnly": true,
        "readOnlyStatement": "All QED tools are read-only; they do not submit transactions or change chain state."
    }))
}

async fn run_tool(state: &AppState, name: &str, arguments: &Value) -> Value {
    match name {
        "qed_check" => {
            let Some(address) = string_argument(arguments, "address") else {
                return tool_error("address must be a string");
            };
            let address = address.trim();
            if !check::valid_public_input(address) {
                return tool_error("Address is not a supported pool or token address.");
            }
            let result = check::check(state, address).await;
            let text = match &result.verdict {
                check::Verdict::Verified { issuer, ticker } => {
                    format!("Verified: {ticker} matches {issuer}'s registry contract.")
                }
                check::Verdict::Mismatch { claimed, actual } => {
                    format!("Mismatch: claimed {claimed}, observed {actual}.")
                }
                check::Verdict::NoMatch => "No issuer contract match was found.".to_owned(),
                check::Verdict::Unknown { reason } => format!("Check inconclusive: {reason}"),
            };
            match serde_json::to_value(result) {
                Ok(payload) => tool_result(payload, text, false),
                Err(_) => tool_error("QED could not serialize the check result."),
            }
        }
        "qed_powers" => {
            let Some(address) = string_argument(arguments, "address") else {
                return tool_error("address must be a string");
            };
            if !check::valid_public_input(address) {
                return tool_error("Address is not a supported token contract.");
            }
            let chain = match arguments.get("chain").and_then(Value::as_str) {
                Some(chain) => crate::chain::Chain::parse(chain),
                None => None,
            };
            match crate::powers::for_registered(state, address, chain).await {
                Ok(records) => {
                    let text = if records.len() == 1 {
                        let record = &records[0];
                        let proxy_status = record
                            .source_verified_proxy
                            .map(|status| format!(" Proxy source is {}.", status.as_str()))
                            .unwrap_or_default();
                        format!(
                            "Observed {} seize facts, {} transfer-blocking facts, and {} rule-change facts for {} on {}; {} is {}.{}",
                            record.can_seize.len(),
                            record.can_block.len(),
                            record.can_change_rules.len(),
                            record.contract,
                            record.chain,
                            record.source_verified_subject.label(),
                            record.source_verified.as_str(),
                            proxy_status
                        )
                    } else {
                        format!(
                            "Observed token powers for {} active registry contracts on multiple chains.",
                            records.len()
                        )
                    };
                    let payload = powers_structured_content(records);
                    tool_result(payload, text, false)
                }
                Err(crate::powers::LookupError::InvalidAddress) => {
                    tool_error("Address is not a supported token contract.")
                }
                Err(crate::powers::LookupError::NotFound) => {
                    tool_error("Address is not an active issuer registry contract.")
                }
                Err(crate::powers::LookupError::ReadFailed) => {
                    tool_error("QED could not complete the supported powers reads.")
                }
            }
        }
        "qed_wallet" => {
            let Some(address) = string_argument(arguments, "address") else {
                return tool_error("address must be a string");
            };
            let Ok(_permit) = state.wallet_concurrency.clone().try_acquire_owned() else {
                return tool_error("A wallet scan is already in progress. Try again later.");
            };
            match pages::wallet_holdings(state, address).await {
                Ok(holdings) => {
                    let text =
                        format!("Wallet scan completed with {} holding records.", holdings.len());
                    tool_result(json!({ "address": address, "holdings": holdings }), text, false)
                }
                Err(status) => tool_error(&format!(
                    "QED could not complete the wallet scan (HTTP {}).",
                    status.as_u16()
                )),
            }
        }
        "qed_registry_lookup" => {
            let Some(ticker) = string_argument(arguments, "ticker") else {
                return tool_error("ticker must be a string");
            };
            let registry = state.registry.read().await;
            let Some(canonical_ticker) = pages::canonical_ticker(&registry, ticker) else {
                return tool_error("Ticker is invalid or has no active issuer registry entries.");
            };
            let entries: Vec<_> = registry
                .iter()
                .filter(|entry| registry::matchable(entry) && entry.ticker == canonical_ticker)
                .cloned()
                .collect();
            let count = entries.len();
            let text =
                format!("Found {count} active issuer registry entries for {canonical_ticker}.");
            tool_result(json!({ "ticker": canonical_ticker, "entries": entries }), text, false)
        }
        "qed_verify" => {
            let Some(attestation) = arguments.get("attestation").filter(|value| value.is_object())
            else {
                return tool_error("attestation must be an object");
            };
            let attestation =
                match serde_json::from_value::<attest::Attestation>(attestation.clone()) {
                    Ok(attestation) => attestation,
                    Err(_) => {
                        return tool_error("Attestation payload is not a valid QED attestation.");
                    }
                };
            let Json(payload) =
                super::api::verify_attestation(State(state.clone()), Json(attestation)).await;
            let text = if payload["ok"] == true {
                "Attestation is cryptographically valid, trusted, current, and environment-matched."
            } else {
                "Attestation verification did not pass all checks."
            };
            tool_result(payload, text.to_owned(), false)
        }
        _ => tool_error("Unknown QED tool."),
    }
}

fn powers_structured_content(mut records: Vec<crate::powers::PowersRecord>) -> Value {
    if records.len() == 1 {
        return json!(records.pop().expect("one powers record"));
    }
    json!({ "records": records })
}

fn validate_tool_arguments(name: &str, arguments: &Value) -> Result<(), &'static str> {
    let string_field = match name {
        "qed_check" | "qed_powers" | "qed_wallet" => Some("address"),
        "qed_registry_lookup" => Some("ticker"),
        _ => None,
    };
    if let Some(field) = string_field
        && !arguments.get(field).is_some_and(Value::is_string)
    {
        return Err("Invalid params: required tool argument must be a string");
    }
    if name == "qed_powers"
        && arguments.get("chain").is_some_and(|chain| {
            !chain
                .as_str()
                .and_then(crate::chain::Chain::parse)
                .is_some()
        })
    {
        return Err("Invalid params: chain must be a supported chain name");
    }
    if name == "qed_verify" && !arguments.get("attestation").is_some_and(Value::is_object) {
        return Err("Invalid params: attestation must be an object");
    }
    Ok(())
}

fn string_argument<'a>(arguments: &'a Value, name: &str) -> Option<&'a str> {
    arguments.get(name).and_then(Value::as_str)
}

fn tool_result(payload: Value, text: String, is_error: bool) -> Value {
    json!({
        "resultType": "complete",
        "content": [{ "type": "text", "text": text }],
        "structuredContent": payload,
        "isError": is_error,
    })
}

fn tool_error(message: &str) -> Value {
    tool_result(json!({ "error": message }), message.to_owned(), true)
}

fn rpc_result(id: Value, mut result: Value, modern_request: bool) -> Response {
    if modern_request && let Some(result) = result.as_object_mut() {
        result.insert("resultType".to_owned(), json!("complete"));
        let metadata = result.entry("_meta").or_insert_with(|| json!({}));
        if !metadata.is_object() {
            *metadata = json!({});
        }
        if let Some(metadata) = metadata.as_object_mut() {
            metadata.insert(
                "io.modelcontextprotocol/serverInfo".to_owned(),
                json!({
                    "name": "QED",
                    "version": env!("CARGO_PKG_VERSION"),
                }),
            );
        }
    }
    (StatusCode::OK, Json(json!({ "jsonrpc": "2.0", "id": id, "result": result }))).into_response()
}

fn rpc_error(id: Value, code: i32, message: &str, status: StatusCode) -> Response {
    (
        status,
        Json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": code, "message": message },
        })),
    )
        .into_response()
}

fn header_mismatch(id: &Value, message: &str) -> Response {
    rpc_error(id.clone(), -32020, message, StatusCode::BAD_REQUEST)
}

fn unsupported_version_error(id: Value, requested: &str, status: StatusCode) -> Response {
    (
        status,
        Json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": -32022,
                "message": "Unsupported protocol version",
                "data": {
                    "supported": SUPPORTED_PROTOCOL_VERSIONS,
                    "requested": requested,
                },
            },
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::to_bytes,
        http::{Request, header},
    };
    use ed25519_dalek::SigningKey;
    use std::collections::HashMap;
    use tower::ServiceExt;

    fn test_state() -> AppState {
        AppState {
            registry: std::sync::Arc::new(tokio::sync::RwLock::new(Vec::new())),
            registry_status: std::sync::Arc::new(tokio::sync::RwLock::new(
                crate::discovery::RegistrySnapshot::default(),
            )),
            readers: std::sync::Arc::new(Vec::new()),
            http: reqwest::Client::new(),
            source_http: reqwest::Client::new(),
            check_cache: moka::future::Cache::builder().build(),
            powers_cache: moka::future::Cache::builder().build(),
            powers_retry_cache: moka::future::Cache::builder().build(),
            powers_failure_cache: moka::future::Cache::builder().build(),
            powers_locks: moka::future::Cache::builder().build(),
            powers_prefetching: std::sync::Arc::new(tokio::sync::Mutex::new(
                std::collections::HashSet::new(),
            )),
            leaderboard_check_cache: moka::future::Cache::builder().build(),
            check_inflight: std::sync::Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            featured: std::sync::Arc::new(tokio::sync::RwLock::new(Vec::new())),
            featured_status: std::sync::Arc::new(tokio::sync::RwLock::new(
                crate::discovery::FeaturedStatus::default(),
            )),
            leaderboard: std::sync::Arc::new(tokio::sync::RwLock::new(
                crate::discovery::Leaderboard::default(),
            )),
            prices: std::sync::Arc::new(tokio::sync::RwLock::new(
                crate::discovery::PriceSnapshot::default(),
            )),
            attestations: std::sync::Arc::new(std::sync::RwLock::new(HashMap::new())),
            signing_key: std::sync::Arc::new(SigningKey::from_bytes(&[7; 32])),
            dev_signer: true,
            attest_store: std::sync::Arc::new(crate::attest::FileAttestationStore::new(
                std::path::PathBuf::from("target/test-mcp-attestations"),
            )),
            board_store: std::sync::Arc::new(crate::discovery::DurableBoardStore::default()),
            registry_hash: std::sync::Arc::new(std::sync::RwLock::new(String::new())),
            registry_api_cache: std::sync::Arc::new(tokio::sync::RwLock::new(None)),
            public_url: std::sync::Arc::new("http://localhost:3000".to_owned()),
            admin_auth: std::sync::Arc::new(crate::state::AdminAuth::new(None, None)),
            usage_stats: std::sync::Arc::new(crate::state::UsageStats::new()),
            rate_limiter: std::sync::Arc::new(crate::state::RateLimiter::default()),
            expensive_concurrency: std::sync::Arc::new(tokio::sync::Semaphore::new(32)),
            powers_prefetch_concurrency: std::sync::Arc::new(tokio::sync::Semaphore::new(2)),
            wallet_concurrency: std::sync::Arc::new(tokio::sync::Semaphore::new(1)),
            registry_api_concurrency: std::sync::Arc::new(tokio::sync::Semaphore::new(4)),
        }
    }

    fn test_registry_entry() -> crate::registry::Entry {
        crate::registry::Entry {
            issuer: "Fixture issuer".to_owned(),
            ticker: "NVDA".to_owned(),
            name: "NVIDIA".to_owned(),
            chain: crate::chain::Chain::Base,
            contract: "0x0000000000000000000000000000000000000001".to_owned(),
            decimals: Some(18),
            source: "fixture".to_owned(),
            source_url: "https://example.invalid/registry".to_owned(),
            last_checked: "2026-01-01T00:00:00Z".to_owned(),
            removed_at: None,
            stale_since: None,
        }
    }

    fn powers_record(chain: crate::chain::Chain, contract: &str) -> crate::powers::PowersRecord {
        crate::powers::PowersRecord {
            chain,
            contract: contract.to_owned(),
            can_seize: Vec::new(),
            can_block: Vec::new(),
            can_change_rules: Vec::new(),
            unavailable: Vec::new(),
            source_verified_subject: crate::powers::SourceVerifiedSubject::Contract,
            source_verified: crate::powers::SourceVerified::None,
            source_verified_proxy: None,
            observed_at: "2026-10-02T00:00:00Z".to_owned(),
            block: None,
            slot: None,
            reads: Vec::new(),
        }
    }

    #[test]
    fn powers_tool_wraps_multi_chain_records_in_an_object() {
        let single = tool_result(
            powers_structured_content(vec![powers_record(
                crate::chain::Chain::Base,
                "0x0000000000000000000000000000000000000001",
            )]),
            "single".to_owned(),
            false,
        );
        assert_eq!(
            single["structuredContent"]["contract"],
            "0x0000000000000000000000000000000000000001"
        );
        assert!(single["structuredContent"]["records"].is_null());

        let multiple = tool_result(
            powers_structured_content(vec![
                powers_record(
                    crate::chain::Chain::Base,
                    "0x0000000000000000000000000000000000000001",
                ),
                powers_record(
                    crate::chain::Chain::RobinhoodChain,
                    "0x0000000000000000000000000000000000000001",
                ),
            ]),
            "multiple".to_owned(),
            false,
        );
        assert!(multiple["structuredContent"].is_object());
        assert_eq!(multiple["structuredContent"]["records"].as_array().unwrap().len(), 2);
    }

    async fn post_rpc(method: &str, params: Value) -> Response {
        post_rpc_with_state(test_state(), method, params).await
    }

    async fn post_rpc_with_protocol_header(
        method: &str,
        params: Value,
        version: &str,
    ) -> Response {
        crate::web::router(test_state())
            .oneshot(
                Request::post("/mcp")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::ACCEPT, "application/json, text/event-stream")
                    .header("MCP-Protocol-Version", version)
                    .body(axum::body::Body::from(
                        json!({ "jsonrpc": "2.0", "id": 7, "method": method, "params": params })
                            .to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("MCP response")
    }

    async fn post_rpc_with_state(state: AppState, method: &str, params: Value) -> Response {
        crate::web::router(state)
            .oneshot(
                Request::post("/mcp")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::ACCEPT, "application/json, text/event-stream")
                    .body(axum::body::Body::from(
                        json!({ "jsonrpc": "2.0", "id": 7, "method": method, "params": params })
                            .to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("MCP response")
    }

    async fn post_modern_rpc(method: &str, params: Value) -> Response {
        post_modern_version(method, params, MODERN_PROTOCOL_VERSION).await
    }

    async fn post_modern_version(method: &str, params: Value, version: &str) -> Response {
        post_modern_version_with_state(test_state(), method, params, version).await
    }

    async fn post_modern_version_with_state(
        state: AppState,
        method: &str,
        mut params: Value,
        version: &str,
    ) -> Response {
        let name = params.get("name").and_then(Value::as_str).map(str::to_owned);
        params["_meta"] = json!({
            "io.modelcontextprotocol/protocolVersion": version,
            "io.modelcontextprotocol/clientInfo": { "name": "test", "version": "1" },
            "io.modelcontextprotocol/clientCapabilities": {},
        });
        let mut request = Request::post("/mcp")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json, text/event-stream")
            .header("MCP-Protocol-Version", version)
            .header("Mcp-Method", method);
        if let Some(name) = name {
            request = request.header("Mcp-Name", name);
        }
        crate::web::router(state)
            .oneshot(
                request
                    .body(axum::body::Body::from(
                        json!({ "jsonrpc": "2.0", "id": 8, "method": method, "params": params })
                            .to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("MCP response")
    }

    async fn response_json(response: Response) -> Value {
        let body = to_bytes(response.into_body(), 64 * 1024).await.expect("response body");
        serde_json::from_slice(&body).expect("JSON-RPC response")
    }

    #[tokio::test]
    async fn modern_discovery_advertises_all_supported_protocol_revisions() {
        let value = response_json(post_modern_rpc("server/discover", json!({})).await).await;
        assert_eq!(value["result"]["supportedVersions"], json!(SUPPORTED_PROTOCOL_VERSIONS));
        assert_eq!(value["result"]["resultType"], "complete");
        assert_eq!(value["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"], "QED");
    }

    #[tokio::test]
    async fn unsupported_modern_version_reports_every_supported_revision() {
        let value =
            response_json(post_modern_version("tools/list", json!({}), "2030-01-01").await).await;
        assert_eq!(value["error"]["code"], -32022);
        assert_eq!(value["error"]["data"]["requested"], "2030-01-01");
        assert_eq!(value["error"]["data"]["supported"], json!(SUPPORTED_PROTOCOL_VERSIONS));
    }

    #[tokio::test]
    async fn initialize_echoes_each_supported_protocol_version() {
        for version in SUPPORTED_PROTOCOL_VERSIONS {
            let response = post_rpc(
                "initialize",
                json!({
                    "protocolVersion": version,
                    "capabilities": {},
                    "clientInfo": { "name": "test", "version": "1" },
                }),
            )
            .await;
            let value = response_json(response).await;
            assert_eq!(value["result"]["protocolVersion"], version);
            assert!(value["result"]["capabilities"]["tools"].is_object());
            assert_eq!(value["result"]["serverInfo"]["name"], "QED");
        }
    }

    #[tokio::test]
    async fn legacy_initialize_falls_back_and_other_methods_reject_unknown_header_versions() {
        let params = json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "old-client", "version": "1" },
        });

        let response = post_rpc("initialize", params.clone()).await;
        assert_eq!(response.status(), StatusCode::OK);
        let value = response_json(response).await;
        assert_eq!(value["result"]["protocolVersion"], LEGACY_PROTOCOL_VERSION);

        let response = post_rpc_with_protocol_header("initialize", params, "2024-11-05").await;
        assert_eq!(response.status(), StatusCode::OK);
        let value = response_json(response).await;
        assert_eq!(value["result"]["protocolVersion"], LEGACY_PROTOCOL_VERSION);

        let response = post_rpc_with_protocol_header("tools/list", json!({}), "2024-11-05").await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let value = response_json(response).await;
        assert_eq!(value["error"]["code"], -32022);
    }

    #[tokio::test]
    async fn tools_list_has_five_complete_read_only_schemas() {
        let value = response_json(post_rpc("tools/list", json!({})).await).await;
        let tools = value["result"]["tools"].as_array().expect("tools array");
        let names: Vec<_> = tools.iter().filter_map(|tool| tool["name"].as_str()).collect();
        assert_eq!(
            names,
            ["qed_check", "qed_powers", "qed_wallet", "qed_registry_lookup", "qed_verify"]
        );
        for (tool, field) in tools
            .iter()
            .zip(["address", "address", "address", "ticker", "attestation"])
        {
            assert_eq!(tool["inputSchema"]["type"], "object");
            assert_eq!(tool["inputSchema"]["required"][0], field);
            assert!(tool["title"].as_str().is_some_and(|title| !title.is_empty()));
            assert_eq!(tool["annotations"]["readOnlyHint"], true);
            assert_eq!(tool["annotations"]["destructiveHint"], false);
            assert_eq!(tool["annotations"]["idempotentHint"], true);
            assert_eq!(tool["annotations"]["openWorldHint"], true);
            assert!(tool["description"].as_str().unwrap().contains(NON_CLAIMS));
        }
        assert_eq!(
            tools[1]["inputSchema"]["properties"]["chain"]["enum"],
            json!(["solana", "robinhood", "base", "ethereum", "bnb"])
        );
        assert_eq!(tools[1]["inputSchema"]["required"], json!(["address"]));
    }
    #[tokio::test]
    async fn static_server_card_describes_the_public_read_only_remote() {
        let response = crate::web::router(test_state())
            .oneshot(
                Request::get("/.well-known/mcp/server-card.json")
                    .body(axum::body::Body::empty())
                    .expect("request"),
            )
            .await
            .expect("server card response");
        assert_eq!(response.status(), StatusCode::OK);
        let value = response_json(response).await;
        assert_eq!(value["name"], "QED");
        assert_eq!(value["serverInfo"]["name"], "QED");
        assert_eq!(value["remotes"][0]["type"], "streamable-http");
        assert_eq!(value["remotes"][0]["url"], "http://localhost:3000/mcp");
        assert_eq!(value["readOnly"], true);
        assert!(value["readOnlyStatement"].as_str().unwrap().contains("read-only"));
        assert_eq!(value["serverInfo"]["version"], env!("CARGO_PKG_VERSION"));
        let manifest: Value =
            serde_json::from_str(include_str!("../../server.json")).expect("manifest JSON");
        assert_eq!(manifest["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(value["tools"].as_array().unwrap().len(), 5);
    }

    #[tokio::test]
    async fn powers_tool_invalid_address_returns_a_tool_error() {
        let response = post_modern_rpc(
            "tools/call",
            json!({ "name": "qed_powers", "arguments": { "address": "not-an-address" } }),
        )
        .await;
        let value = response_json(response).await;
        assert_eq!(value["result"]["isError"], true);
        assert!(
            value["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("not a supported")
        );
    }

    #[tokio::test]
    async fn powers_tool_rejects_unknown_chain_as_invalid_params() {
        let response = post_modern_rpc(
            "tools/call",
            json!({
                "name": "qed_powers",
                "arguments": {
                    "address": "0x0000000000000000000000000000000000000001",
                    "chain": "avalanche"
                }
            }),
        )
        .await;
        let value = response_json(response).await;
        assert_eq!(value["error"]["code"], -32602);
    }

    #[tokio::test]
    async fn invalid_check_is_a_tool_error_with_structured_payload() {
        let response = post_modern_rpc(
            "tools/call",
            json!({ "name": "qed_check", "arguments": { "address": "not-an-address" } }),
        )
        .await;
        let value = response_json(response).await;
        assert_eq!(value["result"]["resultType"], "complete");
        assert_eq!(value["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"], "QED");
        assert_eq!(value["result"]["isError"], true);
        assert!(
            value["result"]["content"][0]["text"].as_str().unwrap().contains("not a supported")
        );
        assert!(value["result"]["structuredContent"]["error"].is_string());
    }

    #[tokio::test]
    async fn wallet_tool_returns_the_wallet_api_payload() {
        let response = post_modern_version(
            "tools/call",
            json!({
                "name": "qed_wallet",
                "arguments": { "address": "0x0000000000000000000000000000000000000001" }
            }),
            MODERN_PROTOCOL_VERSION,
        )
        .await;
        let value = response_json(response).await;
        assert_eq!(value["result"]["isError"], false);
        assert_eq!(
            value["result"]["structuredContent"]["address"],
            "0x0000000000000000000000000000000000000001"
        );
        assert_eq!(value["result"]["structuredContent"]["holdings"], json!([]));
    }

    #[tokio::test]
    async fn registry_lookup_uses_canonical_ticker_and_active_entries() {
        let state = test_state();
        state.registry.write().await.push(test_registry_entry());
        let response = post_modern_version_with_state(
            state,
            "tools/call",
            json!({ "name": "qed_registry_lookup", "arguments": { "ticker": " nvda " } }),
            MODERN_PROTOCOL_VERSION,
        )
        .await;
        let value = response_json(response).await;
        assert_eq!(value["result"]["isError"], false);
        assert_eq!(value["result"]["structuredContent"]["ticker"], "NVDA");
        assert_eq!(value["result"]["structuredContent"]["entries"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn verify_tool_returns_a_passing_verification_payload() {
        let attestation = crate::attest::signed_test_attestation([7; 32], true);
        let response = post_modern_version(
            "tools/call",
            json!({
                "name": "qed_verify",
                "arguments": { "attestation": serde_json::to_value(attestation).unwrap() }
            }),
            MODERN_PROTOCOL_VERSION,
        )
        .await;
        let value = response_json(response).await;
        assert_eq!(value["result"]["isError"], false);
        assert_eq!(value["result"]["structuredContent"]["ok"], true);
    }

    #[tokio::test]
    async fn verify_tool_accepts_awkward_float_round_trips() {
        for quote_share in [1.1129609814871755e-8, 0.1 + 0.2] {
            let attestation =
                crate::attest::signed_test_attestation_with_quote_share([7; 32], true, quote_share);
            let id = attestation.id.clone();
            let response = post_modern_version(
                "tools/call",
                json!({
                    "name": "qed_verify",
                    "arguments": { "attestation": serde_json::to_value(&attestation).unwrap() }
                }),
                MODERN_PROTOCOL_VERSION,
            )
            .await;
            let value = response_json(response).await;
            assert_eq!(value["result"]["isError"], false);
            assert_eq!(value["result"]["structuredContent"]["id"], id);
            assert_eq!(value["result"]["structuredContent"]["cryptographic"], true);
            assert_eq!(value["result"]["structuredContent"]["trusted_signer"], true);
            assert_eq!(value["result"]["structuredContent"]["fresh"], true);
            assert_eq!(value["result"]["structuredContent"]["ok"], true);
        }
    }

    #[tokio::test]
    async fn wallet_tool_refuses_concurrent_scans() {
        let state = test_state();
        let permit = state.wallet_concurrency.clone().acquire_owned().await.unwrap();
        let result = run_tool(
            &state,
            "qed_wallet",
            &json!({ "address": "0x0000000000000000000000000000000000000001" }),
        )
        .await;
        drop(permit);
        assert_eq!(result["isError"], true);
        assert_eq!(
            result["structuredContent"]["error"],
            "A wallet scan is already in progress. Try again later."
        );
    }

    #[tokio::test]
    async fn invalid_long_ticker_is_rejected_without_echoing_input() {
        let result = run_tool(
            &test_state(),
            "qed_registry_lookup",
            &json!({ "ticker": "N".repeat(100_000) }),
        )
        .await;
        assert_eq!(result["isError"], true);
        assert!(
            result["content"][0]["text"].as_str().unwrap().len() < 128,
            "invalid ticker input is not echoed"
        );
    }

    #[tokio::test]
    async fn bad_tool_arguments_use_invalid_params_error() {
        let value = response_json(
            post_rpc("tools/call", json!({ "name": "qed_check", "arguments": {} })).await,
        )
        .await;
        assert_eq!(value["error"]["code"], -32602);
    }

    #[tokio::test]
    async fn legacy_progress_token_does_not_trigger_modern_validation() {
        let response = crate::web::router(test_state())
            .oneshot(
                Request::post("/mcp")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::ACCEPT, "application/json, text/event-stream")
                    .header("MCP-Protocol-Version", LEGACY_PROTOCOL_VERSION)
                    .body(axum::body::Body::from(
                        json!({
                            "jsonrpc": "2.0",
                            "id": 11,
                            "method": "tools/list",
                            "params": { "_meta": { "progressToken": 1 } }
                        })
                        .to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("MCP response");
        assert_eq!(response.status(), StatusCode::OK);
        let value = response_json(response).await;
        assert!(value["result"]["tools"].is_array());
        assert!(value["result"].get("_meta").is_none());
    }

    #[tokio::test]
    async fn modern_metadata_shape_errors_are_invalid_params_with_bad_request() {
        let invalid_params = [
            json!({}),
            json!({ "_meta": null }),
            json!({ "_meta": {} }),
            json!({ "_meta": { "io.modelcontextprotocol/protocolVersion": 7 } }),
        ];
        for params in invalid_params {
            let response = crate::web::router(test_state())
                .oneshot(
                    Request::post("/mcp")
                        .header(header::CONTENT_TYPE, "application/json")
                        .header("MCP-Protocol-Version", MODERN_PROTOCOL_VERSION)
                        .header("Mcp-Method", "tools/list")
                        .body(axum::body::Body::from(
                            json!({
                                "jsonrpc": "2.0",
                                "id": 12,
                                "method": "tools/list",
                                "params": params,
                            })
                            .to_string(),
                        ))
                        .expect("request"),
                )
                .await
                .expect("MCP response");
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            assert_eq!(response_json(response).await["error"]["code"], -32602);
        }
    }

    #[tokio::test]
    async fn modern_protocol_header_mismatch_uses_header_error() {
        let response = crate::web::router(test_state())
            .oneshot(
                Request::post("/mcp")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("MCP-Protocol-Version", LEGACY_PROTOCOL_VERSION)
                    .header("Mcp-Method", "tools/list")
                    .body(axum::body::Body::from(
                        json!({
                            "jsonrpc": "2.0",
                            "id": 13,
                            "method": "tools/list",
                            "params": {
                                "_meta": {
                                    "io.modelcontextprotocol/protocolVersion": MODERN_PROTOCOL_VERSION,
                                    "io.modelcontextprotocol/clientCapabilities": {},
                                }
                            }
                        })
                        .to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("MCP response");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(response_json(response).await["error"]["code"], -32020);
    }

    #[tokio::test]
    async fn modern_initialized_request_has_result_envelope_metadata() {
        let value =
            response_json(post_modern_rpc("notifications/initialized", json!({})).await).await;
        assert_eq!(value["result"]["resultType"], "complete");
        assert_eq!(value["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"], "QED");
    }

    #[tokio::test]
    async fn origin_mismatch_is_forbidden() {
        let response = crate::web::router(test_state())
            .oneshot(
                Request::post("/mcp")
                    .header("Origin", "https://evil.example")
                    .header("Host", "qed.example")
                    .body(axum::body::Body::from(r#"{"jsonrpc":"2.0","id":14,"method":"ping"}"#))
                    .expect("request"),
            )
            .await
            .expect("MCP response");
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn mcp_requests_use_the_shared_per_ip_rate_limit() {
        let state = test_state();
        let mut allowed = 0;
        let mut limited = 0;
        for _ in 0..70 {
            let response = crate::web::router(state.clone())
                .oneshot(
                    Request::post("/mcp")
                        .header(header::CONTENT_TYPE, "application/json")
                        .header("X-Forwarded-For", "198.51.100.77")
                        .body(axum::body::Body::from(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#))
                        .expect("request"),
                )
                .await
                .expect("MCP response");
            match response.status() {
                StatusCode::OK => allowed += 1,
                StatusCode::TOO_MANY_REQUESTS => limited += 1,
                status => panic!("unexpected HTTP status {status}"),
            }
        }
        assert_eq!(allowed, 60);
        assert_eq!(limited, 10);
    }

    #[tokio::test]
    async fn mcp_usage_counts_tools_by_mcp_name_and_other_calls_as_api() {
        let state = test_state();
        let usage_stats = state.usage_stats.clone();
        let check = post_modern_version_with_state(
            state.clone(),
            "tools/call",
            json!({ "name": "qed_check", "arguments": { "address": "invalid" } }),
            MODERN_PROTOCOL_VERSION,
        )
        .await;
        assert_eq!(response_json(check).await["result"]["isError"], true);

        let wallet = post_modern_version_with_state(
            state.clone(),
            "tools/call",
            json!({
                "name": "qed_wallet",
                "arguments": { "address": "0x0000000000000000000000000000000000000001" }
            }),
            MODERN_PROTOCOL_VERSION,
        )
        .await;
        assert_eq!(response_json(wallet).await["result"]["isError"], false);

        let _ = post_rpc_with_state(state, "tools/list", json!({})).await;
        let snapshot = usage_stats.snapshot();
        assert_eq!(snapshot.api_requests, 3);
        assert_eq!(snapshot.checks, 1);
        assert_eq!(snapshot.wallet_requests, 1);
    }

    #[tokio::test]
    async fn malformed_and_invalid_requests_use_jsonrpc_error_codes() {
        let malformed = crate::web::router(test_state())
            .oneshot(Request::post("/mcp").body(axum::body::Body::from("{")).expect("request"))
            .await
            .expect("MCP response");
        assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);
        assert_eq!(response_json(malformed).await["error"]["code"], -32700);

        let invalid = crate::web::router(test_state())
            .oneshot(
                Request::post("/mcp")
                    .body(axum::body::Body::from(r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#))
                    .expect("request"),
            )
            .await
            .expect("MCP response");
        assert_eq!(response_json(invalid).await["error"]["code"], -32600);
    }

    #[tokio::test]
    async fn unknown_method_returns_method_not_found() {
        let response = post_rpc("not/a/method", json!({})).await;
        let value = response_json(response).await;
        assert_eq!(value["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn get_is_method_not_allowed_and_declares_post() {
        let response = crate::web::router(test_state())
            .oneshot(Request::get("/mcp").body(axum::body::Body::empty()).expect("request"))
            .await
            .expect("MCP response");
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(response.headers().get(header::ALLOW).unwrap(), "POST");
    }

    #[tokio::test]
    async fn initialized_notification_returns_empty_accepted_response() {
        let response = crate::web::router(test_state())
            .oneshot(
                Request::post("/mcp")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(axum::body::Body::from(
                        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
                    ))
                    .expect("request"),
            )
            .await
            .expect("MCP response");
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let body = to_bytes(response.into_body(), 1024).await.expect("empty body");
        assert!(body.is_empty());
    }
}
