use super::pages;
use crate::{
    adapters::{
        discovery::{self, FeaturedPool, LeaderboardEntry},
        state::{AppState, RegistryApiCache},
    },
    app::{attestation as attest, check},
    domain::{
        attestation::Attestation,
        check::{CheckResult, valid_public_input},
        statement::Statement,
    },
};
use axum::{
    Json,
    body::Body,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use bytes::Bytes;
use serde::Deserialize;
use std::cmp::Ordering;
use std::collections::HashSet;
use std::sync::{Arc, atomic::Ordering as AtomicOrdering};
pub(crate) async fn healthz() -> impl IntoResponse {
    Json(serde_json::json!({ "status": "ok" }))
}

pub(crate) async fn admin_stats(State(state): State<AppState>) -> Response {
    ([(header::CACHE_CONTROL, "no-store")], Json(state.usage_stats.snapshot())).into_response()
}
pub(crate) async fn api_registry(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let registry_hash = state.app.registry_hash.read().map(|hash| hash.clone()).unwrap_or_default();
    let registry_hash =
        format!("{registry_hash}:{}", state.app.registry_version.load(AtomicOrdering::Acquire));
    let body = {
        let mut cache = state.registry_api_cache.write().await;
        if let Some(cached) = cache.as_ref().filter(|cached| cached.registry_hash == registry_hash)
        {
            Arc::clone(&cached.body)
        } else {
            let body = Arc::new(Bytes::from(
                serde_json::to_vec(&*state.registry.read().await)
                    .expect("registry serialization must remain valid"),
            ));
            *cache = Some(RegistryApiCache {
                registry_hash: registry_hash.clone(),
                body: Arc::clone(&body),
            });
            body
        }
    };
    let etag = format!("\"qed-registry-{registry_hash}\"");
    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == etag)
    {
        return StatusCode::NOT_MODIFIED.into_response();
    }
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CACHE_CONTROL, "public, max-age=60")
        .header(header::ETAG, etag)
        .body(Body::from(body.as_ref().clone()))
        .expect("registry response headers are valid")
}
pub(crate) async fn api_featured(State(state): State<AppState>) -> Json<Vec<FeaturedPool>> {
    Json(state.featured.read().await.clone())
}
#[derive(Debug, Deserialize, Default)]
pub(crate) struct LeaderboardQuery {
    page: Option<usize>,
    per: Option<usize>,
    sort: Option<String>,
    dir: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct PowersQuery {
    chain: Option<String>,
}

fn metric(entry: &LeaderboardEntry, sort: &str) -> Option<f64> {
    match sort {
        "change" => entry.change_24h_pct,
        "liquidity" => entry.liquidity_usd,
        "price" => entry.price_usd,
        _ => entry.volume_24h_usd,
    }
}

fn sort_entries(entries: &mut [LeaderboardEntry], sort: &str, descending: bool) {
    entries.sort_by(|left, right| {
        let ordering = match (metric(left, sort), metric(right, sort)) {
            (Some(left), Some(right)) => {
                let ordering = left.total_cmp(&right);
                if descending { ordering.reverse() } else { ordering }
            }
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        };
        ordering
            .then_with(|| left.pool.to_ascii_lowercase().cmp(&right.pool.to_ascii_lowercase()))
            .then_with(|| left.rank.cmp(&right.rank))
    });
}

fn page_entries(entries: Vec<LeaderboardEntry>, page: usize, per: usize) -> Vec<LeaderboardEntry> {
    let start = page.saturating_sub(1).saturating_mul(per);
    entries
        .into_iter()
        .skip(start)
        .take(per)
        .enumerate()
        .map(|(offset, mut entry)| {
            entry.rank = start + offset + 1;
            entry
        })
        .collect()
}

pub(crate) async fn api_leaderboard(
    State(state): State<AppState>,
    Query(query): Query<LeaderboardQuery>,
) -> Json<serde_json::Value> {
    let page = query.page.unwrap_or(1).max(1);
    let per = query.per.unwrap_or(discovery::LEADERBOARD_PAGE_SIZE).clamp(1, 50);
    let sort = query.sort.as_deref().unwrap_or("volume");
    let descending = !query.dir.as_deref().is_some_and(|dir| dir.eq_ignore_ascii_case("asc"));
    let board = state.leaderboard.read().await.clone();
    let prices_updated_at = state.prices.read().await.updated_at.clone();
    let total = board.total.max(board.entries.len());
    let mut entries = board.entries;
    for entry in &mut entries {
        entry.trade_url = discovery::chain_from_dex_id(&entry.chain)
            .map(|chain| {
                discovery::canonical_market_url(chain, &entry.pool, Some(&entry.trade_url))
            })
            .unwrap_or_default();
    }
    sort_entries(&mut entries, sort, descending);
    let entries = page_entries(entries, page, per);
    Json(serde_json::json!({
        "page": page,
        "per": per,
        "total": total,
        "updated_at": board.updated_at,
        "next_refresh_at": board.next_refresh_at,
        "restored": board.restored,
        "refreshing": board.refreshing,
        "empty_successful": board.empty_successful,
        "prices_updated_at": prices_updated_at,
        "source": board.source,
        "registry": board.registry,
        "entries": entries,
    }))
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct PricesQuery {
    ids: Option<String>,
}
pub(crate) async fn api_prices(
    State(state): State<AppState>,
    Query(query): Query<PricesQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let ids = query.ids.as_deref().unwrap_or("");
    if ids.len() > 2048 {
        return Err(StatusCode::BAD_REQUEST);
    }
    let snapshot = state.prices.read().await.clone();
    let mut prices = Vec::new();
    let mut seen = HashSet::new();
    for id in ids.split(',') {
        if id.len() > 128 || !seen.insert(id) {
            return Err(StatusCode::BAD_REQUEST);
        }
        let Some((chain_id, pool)) = id.split_once(':') else { continue };
        let Some(chain) = discovery::chain_from_dex_id(chain_id) else { continue };
        if pool.is_empty() {
            continue;
        }
        let chain = discovery::chain_slug(chain).to_owned();
        if let Some(point) = snapshot
            .prices
            .iter()
            .find(|point| {
                point.chain.eq_ignore_ascii_case(&chain) && point.pool.eq_ignore_ascii_case(pool)
            })
            .cloned()
        {
            prices.push(point);
        }
        if prices.len() > 100 {
            return Err(StatusCode::BAD_REQUEST);
        }
    }
    Ok(Json(serde_json::json!({ "updated_at": snapshot.updated_at, "prices": prices })))
}

pub(crate) async fn api_status(State(state): State<AppState>) -> Json<serde_json::Value> {
    let registry = state.registry_status.read().await.clone();
    let leaderboard = state.leaderboard.read().await;
    let featured = state.featured_status.read().await.clone();
    let prices = state.prices.read().await.clone();
    Json(serde_json::json!({
        "registry": registry,
        "leaderboard": {
            "updated_at": leaderboard.updated_at,
            "next_refresh_at": leaderboard.next_refresh_at,
            "restored": leaderboard.restored,
            "refreshing": leaderboard.refreshing,
            "empty_successful": leaderboard.empty_successful,
        },
        "featured": featured,
        "prices": {
            "updated_at": prices.updated_at,
        },
    }))
}

pub(crate) async fn api_check(
    State(state): State<AppState>,
    Path(address): Path<String>,
) -> Result<Json<CheckResult>, StatusCode> {
    let address = address.trim();
    if !valid_public_input(address) {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(Json(check::check(&state.app, address).await))
}
pub(crate) async fn api_statement(
    State(state): State<AppState>,
    Json(request): Json<crate::app::statement::StatementRequest>,
) -> Result<Json<crate::domain::statement::Statement>, StatusCode> {
    crate::app::statement::create(&state.app, request).await.map(Json).map_err(
        |error| match error {
            crate::app::statement::StatementError::InvalidRequest
            | crate::app::statement::StatementError::UnknownWallet => StatusCode::BAD_REQUEST,
            crate::app::statement::StatementError::ReaderUnavailable(_)
            | crate::app::statement::StatementError::ReadFailed(_) => StatusCode::BAD_GATEWAY,
            crate::app::statement::StatementError::DeadlineExceeded => StatusCode::GATEWAY_TIMEOUT,
            crate::app::statement::StatementError::Signing => StatusCode::INTERNAL_SERVER_ERROR,
        },
    )
}

pub(crate) async fn api_statement_get(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Statement>, StatusCode> {
    crate::app::statement::get(&state.app, &id).await.map(Json).ok_or(StatusCode::NOT_FOUND)
}

pub(crate) async fn api_powers(
    State(state): State<AppState>,
    Path(address): Path<String>,
    Query(query): Query<PowersQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let chain = match query.chain.as_deref() {
        Some(chain) => {
            Some(crate::domain::chain::Chain::parse(chain).ok_or(StatusCode::BAD_REQUEST)?)
        }
        None => None,
    };
    let records = crate::app::powers::for_any(&state.app, &address, chain).await;
    match records {
        Ok(mut records) => {
            let value = if records.len() == 1 {
                serde_json::to_value(records.pop().expect("one powers record"))
            } else {
                serde_json::to_value(records)
            }
            .expect("powers records serialize");
            Ok(Json(value))
        }
        Err(crate::app::powers::LookupError::InvalidAddress) => Err(StatusCode::BAD_REQUEST),
        Err(crate::app::powers::LookupError::NotFound) => Err(StatusCode::NOT_FOUND),
        Err(crate::app::powers::LookupError::ReadFailed) => Err(StatusCode::BAD_GATEWAY),
        Err(crate::app::powers::LookupError::DeadlineExceeded) => Err(StatusCode::GATEWAY_TIMEOUT),
    }
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct GuardQuery {
    chain: Option<String>,
    wallet: Option<String>,
}

pub(crate) async fn api_guard(
    State(state): State<AppState>,
    Path(address): Path<String>,
    Query(query): Query<GuardQuery>,
) -> Result<Json<crate::domain::guard::GuardDocument>, StatusCode> {
    let chain = query
        .chain
        .as_deref()
        .and_then(crate::domain::chain::Chain::parse)
        .ok_or(StatusCode::BAD_REQUEST)?;
    crate::app::guard::create(&state.app, &address, chain, query.wallet.as_deref())
        .await
        .map(Json)
        .map_err(|error| super::guard_error_status(&error))
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GuardPostRequest {
    address: String,
    chain: String,
    wallet: Option<String>,
}

pub(crate) async fn api_guard_post(
    State(state): State<AppState>,
    Json(request): Json<GuardPostRequest>,
) -> Result<Json<crate::domain::guard::GuardDocument>, StatusCode> {
    let chain =
        crate::domain::chain::Chain::parse(&request.chain).ok_or(StatusCode::BAD_REQUEST)?;
    crate::app::guard::create(&state.app, &request.address, chain, request.wallet.as_deref())
        .await
        .map(Json)
        .map_err(|error| super::guard_error_status(&error))
}

pub(crate) async fn api_wallet(
    State(state): State<AppState>,
    Json(request): Json<pages::WalletRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let holdings = crate::app::wallet::wallet_holdings(&state.app, &request.address)
        .await
        .map_err(super::wallet_error_status)?;
    let holdings = super::views::wallet_holding_views(holdings);
    Ok(Json(serde_json::json!({
        "address": request.address,
        "holdings": holdings,
    })))
}
pub(crate) async fn api_attestation(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, StatusCode> {
    let attestation = attest::get_async(&state.app, &id).await.ok_or(StatusCode::NOT_FOUND)?;
    Ok(([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(attestation)).into_response())
}
pub(crate) async fn well_known(State(state): State<AppState>) -> Response {
    (
        [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")],
        Json(serde_json::json!({
            "name": "QED",
            "version": 1,
            "algorithm": "Ed25519",
            "public_key": attest::public_key_b58(&state.app),
            "key": attest::public_key_b58(&state.app),
            "dev": state.app.dev_signer,
        })),
    )
        .into_response()
}
pub(crate) async fn verify_attestation(
    State(state): State<AppState>,
    Json(document): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let kind = document.get("kind").and_then(serde_json::Value::as_str);
    if document.get("kind").is_some() && kind.is_none() {
        return Err(verify_bad_request("QED document kind must be a string."));
    }
    let has_statement_fields =
        ["wallets", "assets", "positions"].iter().any(|field| document.get(*field).is_some());
    let has_attestation_fields = ["pool", "quote_share_of_supply", "registry_hash"]
        .iter()
        .any(|field| document.get(*field).is_some());
    let has_guard_fields = ["identity", "powers", "source", "pools", "reasons", "wallet"]
        .iter()
        .any(|field| document.get(*field).is_some());
    if (has_statement_fields && (has_attestation_fields || has_guard_fields))
        || (has_attestation_fields && has_guard_fields)
    {
        return Err(verify_bad_request("Hybrid QED document payloads are not accepted."));
    }

    match kind {
        Some("statement") => {
            if has_attestation_fields || has_guard_fields {
                return Err(verify_bad_request(
                    "Payload kind is statement but the payload contains fields from another QED document kind.",
                ));
            }
            let Ok(statement) = serde_json::from_value::<Statement>(document) else {
                return Err(verify_bad_request(
                    "Payload kind is statement but the payload does not match a QED statement.",
                ));
            };
            Ok(Json(verify_statement_document(&state, statement)))
        }
        Some("attestation") => {
            if has_statement_fields || has_guard_fields {
                return Err(verify_bad_request(
                    "Payload kind is attestation but the payload contains fields from another QED document kind.",
                ));
            }
            let Ok(attestation) = serde_json::from_value::<Attestation>(document) else {
                return Err(verify_bad_request(
                    "Payload kind is attestation but the payload does not match a QED attestation.",
                ));
            };
            Ok(Json(verify_attestation_document(&state, attestation)))
        }
        Some("guard") => {
            if has_statement_fields || has_attestation_fields {
                return Err(verify_bad_request(
                    "Payload kind is guard but the payload contains fields from another QED document kind.",
                ));
            }
            let Ok(guard) = serde_json::from_value::<crate::domain::guard::GuardDocument>(document)
            else {
                return Err(verify_bad_request(
                    "Payload kind is guard but the payload does not match a QED Guard document.",
                ));
            };
            Ok(Json(verify_guard_document(&state, guard)))
        }
        Some(_) => Err(verify_bad_request("Unsupported QED document kind.")),
        None if has_statement_fields => {
            Err(verify_bad_request("Signed statements must include kind=\"statement\"."))
        }
        None if has_guard_fields => {
            Err(verify_bad_request("Signed Guards must include kind=\"guard\"."))
        }
        None => {
            let Ok(attestation) = serde_json::from_value::<Attestation>(document) else {
                return Err(verify_bad_request(
                    "Payload must match a legacy QED attestation or declare its document kind.",
                ));
            };
            Ok(Json(verify_attestation_document(&state, attestation)))
        }
    }
}

fn verify_bad_request(message: &str) -> (StatusCode, Json<serde_json::Value>) {
    (StatusCode::BAD_REQUEST, Json(serde_json::json!({ "error": message })))
}

fn verify_attestation_document(state: &AppState, attestation: Attestation) -> serde_json::Value {
    let cryptographic = crate::domain::attestation::verify(&attestation).is_ok();
    let trusted_signer = cryptographic && attest::signer_trusted(&state.app, &attestation);
    let environment_match = attestation.dev == state.app.dev_signer;
    let fresh = cryptographic && attest::fresh(&attestation);
    serde_json::json!({
        "ok": cryptographic && trusted_signer && environment_match && fresh,
        "kind": "attestation",
        "id": attestation.id,
        "cryptographic": cryptographic,
        "trusted_signer": trusted_signer,
        "environment_match": environment_match,
        "fresh": fresh,
    })
}

fn verify_statement_document(state: &AppState, statement: Statement) -> serde_json::Value {
    let cryptographic = crate::domain::statement::verify(&statement).is_ok();
    let trusted_signer = cryptographic
        && (state.app.trusted_signers.contains(&statement.signer)
            || statement.signer == state.app.signer.public_key());
    let environment_match = statement.dev == state.app.dev_signer;
    serde_json::json!({
        "ok": cryptographic && trusted_signer && environment_match,
        "kind": "statement",
        "id": statement.id,
        "cryptographic": cryptographic,
        "trusted_signer": trusted_signer,
        "environment_match": environment_match,
        "fresh": null,
    })
}

fn verify_guard_document(
    state: &AppState,
    guard: crate::domain::guard::GuardDocument,
) -> serde_json::Value {
    let cryptographic = crate::domain::guard::verify(&guard).is_ok();
    let trusted_signer = cryptographic
        && (state.app.trusted_signers.contains(&guard.public_key)
            || guard.public_key == state.app.signer.public_key());
    let environment_match = guard.dev == state.app.dev_signer;
    serde_json::json!({
        "ok": cryptographic && trusted_signer && environment_match,
        "kind": "guard",
        "id": guard.id,
        "cryptographic": cryptographic,
        "trusted_signer": trusted_signer,
        "environment_match": environment_match,
        "fresh": null,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use tower::ServiceExt;
    fn test_state(dev_signer: bool) -> AppState {
        AppState::for_tests(Vec::new(), Vec::new(), dev_signer)
    }

    struct GuardReviewReader;

    #[async_trait::async_trait]
    impl crate::ports::ChainReader for GuardReviewReader {
        fn chain(&self) -> crate::domain::chain::Chain {
            crate::domain::chain::Chain::Base
        }

        async fn read_pool(
            &self,
            _address: &str,
        ) -> Result<crate::domain::pool::PoolInfo, crate::domain::pool::PoolError> {
            Err(crate::domain::pool::PoolError::Unknown("not a pool".to_owned()))
        }

        async fn code_at(&self, _address: &str) -> Result<Vec<u8>, crate::domain::pool::PoolError> {
            Ok(vec![0x60])
        }
    }

    #[tokio::test]
    async fn guard_get_and_json_post_documents_verify_over_the_rest_routes() {
        let address = "0x0000000000000000000000000000000000000001";
        let wallet = "0x0000000000000000000000000000000000000004";
        let app = crate::adapters::web::router(AppState::for_tests(
            Vec::new(),
            vec![Box::new(GuardReviewReader)],
            false,
        ));
        let requests = [
            Request::get(format!("/api/guard/{address}?chain=base")).body(Body::empty()).unwrap(),
            Request::post("/api/guard")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({ "address": address, "chain": "base", "wallet": wallet })
                        .to_string(),
                ))
                .unwrap(),
        ];
        for request in requests {
            let response = app.clone().oneshot(request).await.expect("Guard route response");
            assert_eq!(response.status(), StatusCode::OK);
            let body = to_bytes(response.into_body(), 1024 * 1024).await.expect("Guard response");
            let guard: serde_json::Value = serde_json::from_slice(&body).expect("Guard JSON");
            assert_eq!(guard["subject_type"], "token");
            assert_eq!(guard["subject_address"], address);
            if guard["wallet"].is_null() {
                assert!(guard["wallet_check"].is_null());
            } else {
                assert_eq!(guard["wallet"], wallet);
                assert_eq!(guard["wallet_check"]["status"], "unavailable");
            }
            let verify = app
                .clone()
                .oneshot(
                    Request::post("/verify")
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(guard.to_string()))
                        .unwrap(),
                )
                .await
                .expect("verification response");
            assert_eq!(verify.status(), StatusCode::OK);
            let body =
                to_bytes(verify.into_body(), 64 * 1024).await.expect("verification JSON body");
            let verified: serde_json::Value =
                serde_json::from_slice(&body).expect("verification JSON");
            assert_eq!(verified["kind"], "guard");
            assert_eq!(verified["ok"], true);
        }
    }

    #[tokio::test]
    async fn guard_api_rejects_missing_chain_invalid_token_and_invalid_wallet() {
        let app = crate::adapters::web::router(test_state(false));
        let address = "0x0000000000000000000000000000000000000001";
        for path in [
            format!("/api/guard/{address}"),
            "/api/guard/not-a-contract?chain=base".to_owned(),
            format!("/api/guard/{address}?chain=base&wallet=not-a-wallet"),
        ] {
            let response = app
                .clone()
                .oneshot(Request::get(path).body(Body::empty()).unwrap())
                .await
                .expect("Guard validation response");
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        }
    }

    #[tokio::test]

    async fn powers_api_accepts_an_unregistered_evm_contract_without_any_filter() {
        let address = "0x0000000000000000000000000000000000000011";
        let state = AppState::for_tests(Vec::new(), vec![Box::new(GuardReviewReader)], false);
        let version = state.app.registry_version.load(AtomicOrdering::Acquire);
        state
            .app
            .powers_cache
            .insert(
                (crate::domain::chain::Chain::Base, address.to_owned(), version),
                crate::domain::powers::PowersRecord {
                    chain: crate::domain::chain::Chain::Base,
                    contract: address.to_owned(),
                    can_seize: Vec::new(),
                    can_block: Vec::new(),
                    can_change_rules: Vec::new(),
                    token_paused: Some(false),
                    sanctions_list: None,
                    unavailable: Vec::new(),
                    source_verified_subject: crate::domain::powers::SourceVerifiedSubject::Contract,
                    source_verified: crate::domain::powers::SourceVerified::None,
                    source_verified_proxy: None,
                    observed_at: "2026-10-01T00:00:00Z".to_owned(),
                    block: Some(42),
                    slot: None,
                    reads: Vec::new(),
                },
            )
            .await;
        let response = crate::adapters::web::router(state)
            .oneshot(Request::get(format!("/api/powers/{address}")).body(Body::empty()).unwrap())
            .await
            .expect("any-contract powers response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 4096).await.expect("powers response");
        let value: serde_json::Value = serde_json::from_slice(&body).expect("powers JSON");
        assert_eq!(value["chain"], "Base");
        assert_eq!(value["contract"], address);
        assert_eq!(value["token_paused"], false);
    }
    #[tokio::test]
    async fn powers_api_returns_all_registered_chain_records_or_a_filtered_record() {
        let address = "0x0000000000000000000000000000000000000011";
        let state = test_state(false);
        *state.registry.write().await = std::sync::Arc::new(
            [crate::domain::chain::Chain::RobinhoodChain, crate::domain::chain::Chain::Base]
                .into_iter()
                .map(|chain| crate::adapters::registry::Entry {
                    issuer: "Issuer".to_owned(),
                    ticker: "NVDA".to_owned(),
                    name: "Issuer NVDA".to_owned(),
                    chain,
                    contract: address.to_owned(),
                    decimals: Some(18),
                    source: "test".to_owned(),
                    source_url: "https://issuer.example".to_owned(),
                    last_checked: crate::adapters::registry::now_rfc3339(),
                    removed_at: None,
                    stale_since: None,
                })
                .collect(),
        );
        let version = state.app.registry_version.load(std::sync::atomic::Ordering::Acquire);
        for chain in
            [crate::domain::chain::Chain::RobinhoodChain, crate::domain::chain::Chain::Base]
        {
            state
                .app
                .powers_cache
                .insert(
                    (chain, address.to_owned(), version),
                    crate::domain::powers::PowersRecord {
                        chain,
                        contract: address.to_owned(),
                        can_seize: Vec::new(),
                        can_block: Vec::new(),
                        can_change_rules: Vec::new(),
                        unavailable: Vec::new(),
                        token_paused: None,
                        sanctions_list: None,
                        source_verified_subject:
                            crate::domain::powers::SourceVerifiedSubject::Contract,
                        source_verified: crate::domain::powers::SourceVerified::None,
                        source_verified_proxy: None,
                        observed_at: "2026-01-01T00:00:00Z".to_owned(),
                        block: Some(42),
                        slot: None,
                        reads: Vec::new(),
                    },
                )
                .await;
        }

        let app = crate::adapters::web::router(state);
        let response = app
            .clone()
            .oneshot(Request::get(format!("/api/powers/{address}")).body(Body::empty()).unwrap())
            .await
            .expect("multi-chain powers response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 4096).await.unwrap();
        let all: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            all.as_array()
                .unwrap()
                .iter()
                .map(|record| record["chain"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["RobinhoodChain", "Base"]
        );

        let response = app
            .oneshot(
                Request::get(format!("/api/powers/{address}?chain=base"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("filtered powers response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 4096).await.unwrap();
        let filtered: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(filtered["chain"], "Base");
    }
    fn entry(pool: &str, rank: usize, volume: Option<f64>, price: Option<f64>) -> LeaderboardEntry {
        LeaderboardEntry {
            rank,
            chain: "solana".to_owned(),
            chain_label: "Solana".to_owned(),
            dex: "raydium".to_owned(),
            pool: pool.to_owned(),
            base_symbol: "NVDAx".to_owned(),
            quote_symbol: "USDC".to_owned(),
            issuer: Some("Issuer".to_owned()),
            ticker: Some("NVDA".to_owned()),
            verdict: "verified".to_owned(),
            price_usd: price,
            change_24h_pct: Some(1.0),
            volume_24h_usd: volume,
            liquidity_usd: Some(5.0),
            txns_24h: Some(1),
            detail_url: "/validated/solana/pool".to_owned(),
            trade_url: "https://dexscreener.com/solana/pool".to_owned(),
            explorer_url: "https://solscan.io/account/pool".to_owned(),
            attestation_id: None,
            checked_at: None,
        }
    }
    #[tokio::test]
    async fn admin_stats_reports_instance_schema_and_counters() {
        let state = test_state(false);
        state.usage_stats.record_request(true, true, false, false, false, false);
        state.usage_stats.record_html_page_view();
        state.usage_stats.record_response(200);

        let response = admin_stats(State(state)).await;
        assert_eq!(response.headers().get(header::CACHE_CONTROL).unwrap(), "no-store");
        let body = to_bytes(response.into_body(), 4096).await.expect("stats body");
        let value: serde_json::Value = serde_json::from_slice(&body).expect("stats serialize");
        assert_eq!(value["scope"], "instance");
        assert!(value["started_at"].as_str().is_some_and(|value| !value.is_empty()));
        assert!(value["uptime_seconds"].is_u64());
        assert_eq!(value["total_requests"], 1);
        assert_eq!(value["html_page_views"], 1);
        assert_eq!(value["api_requests"], 1);
        assert_eq!(value["checks"], 1);
        assert_eq!(value["health_requests"], 0);
        assert_eq!(value["static_asset_requests"], 0);
        assert_eq!(value["responses"]["2xx"], 1);
    }
    #[tokio::test]
    async fn admin_route_rejects_without_auth_and_accepts_basic_auth() {
        let state = test_state(false);
        let app = crate::adapters::web::router(state.clone());

        let rejected = app
            .clone()
            .oneshot(Request::get("/admin/stats").body(Body::empty()).unwrap())
            .await
            .expect("unauthorized response");
        assert_eq!(rejected.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(rejected.headers().get(header::CACHE_CONTROL).unwrap(), "no-store");
        let rejected_body = to_bytes(rejected.into_body(), 1024).await.expect("body");
        assert_eq!(rejected_body.as_ref(), b"Unauthorized\n");
        let after_rejection = state.usage_stats.snapshot();
        assert_eq!(after_rejection.total_requests, 1);
        assert_eq!(after_rejection.admin_requests, 1);
        assert_eq!(after_rejection.responses.class_4xx, 1);

        let encoded = STANDARD.encode(b"test-admin:test-password");
        let accepted = app
            .oneshot(
                Request::get("/admin/stats")
                    .header("authorization", format!("Basic {encoded}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("authorized response");
        assert_eq!(accepted.status(), StatusCode::OK);
        assert_eq!(accepted.headers().get(header::CACHE_CONTROL).unwrap(), "no-store");
        let body = to_bytes(accepted.into_body(), 4096).await.expect("body");
        let value: serde_json::Value = serde_json::from_slice(&body).expect("stats JSON");
        assert_eq!(value["scope"], "instance");
        assert_eq!(value["admin_requests"], 2);
        assert_eq!(state.usage_stats.snapshot().responses.class_2xx, 1);
        assert_eq!(state.usage_stats.snapshot().html_page_views, 0);
        let mut unavailable_state = test_state(false);
        unavailable_state.admin_auth =
            std::sync::Arc::new(crate::adapters::state::AdminAuth::new(None, Some("ignored")));
        let unavailable = crate::adapters::web::router(unavailable_state)
            .oneshot(
                Request::get("/admin/stats")
                    .header("authorization", format!("Basic {encoded}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("fail-closed response");
        assert_eq!(unavailable.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(unavailable.headers().get(header::CACHE_CONTROL).unwrap(), "no-store");

        let public_status = crate::adapters::web::router(test_state(false))
            .oneshot(Request::get("/api/status").body(Body::empty()).unwrap())
            .await
            .expect("public status response");
        assert_eq!(public_status.status(), StatusCode::OK);
    }
    #[tokio::test]
    async fn admin_auth_attempts_are_rate_limited_before_authentication() {
        let app = crate::adapters::web::router(test_state(false));
        for _ in 0..60 {
            let response = app
                .clone()
                .oneshot(Request::get("/admin/stats").body(Body::empty()).unwrap())
                .await
                .expect("auth response");
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }
        let response = app
            .oneshot(Request::get("/admin/stats").body(Body::empty()).unwrap())
            .await
            .expect("rate-limited response");
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[test]
    fn sorting_supports_metric_direction_and_nulls_last() {
        let mut entries = vec![
            entry("low", 1, Some(10.0), Some(3.0)),
            entry("high", 2, Some(30.0), Some(1.0)),
            entry("missing", 3, None, Some(2.0)),
        ];
        sort_entries(&mut entries, "volume", true);
        assert_eq!(
            entries.iter().map(|entry| entry.pool.as_str()).collect::<Vec<_>>(),
            vec!["high", "low", "missing"]
        );
        sort_entries(&mut entries, "price", false);
        assert_eq!(
            entries.iter().map(|entry| entry.pool.as_str()).collect::<Vec<_>>(),
            vec!["high", "missing", "low"]
        );
    }

    #[test]
    fn pagination_returns_the_requested_ranked_window() {
        let entries = (0..125)
            .map(|index| entry(&format!("pool-{index}"), index + 1, Some(index as f64), None))
            .collect();
        let page = page_entries(entries, 2, 50);
        assert_eq!(page.len(), 50);
        assert_eq!(page.first().map(|entry| entry.rank), Some(51));
        assert_eq!(page.last().map(|entry| entry.rank), Some(100));
    }
    #[tokio::test]
    async fn price_api_omits_unknown_pools_instead_of_zeroing_them() {
        let state = test_state(false);
        state.prices.write().await.prices.push(crate::adapters::discovery::PricePoint {
            chain: "solana".to_owned(),
            pool: "known".to_owned(),
            price_usd: Some(12.5),
            change_24h_pct: Some(1.5),
            volume_24h_usd: Some(100.0),
            liquidity_usd: Some(200.0),
        });

        let body = api_prices(
            State(state),
            Query(PricesQuery { ids: Some("solana:known,solana:missing".to_owned()) }),
        )
        .await
        .expect("valid price query")
        .0;
        let prices = body["prices"].as_array().expect("price array");
        assert_eq!(prices.len(), 1);
        assert_eq!(prices[0]["pool"], "known");
        assert_eq!(prices[0]["price_usd"], 12.5);
    }
    #[tokio::test]
    async fn verify_handler_reports_all_validation_dimensions() {
        let state = test_state(false);
        let attestation = crate::app::attestation::signed_test_attestation([7; 32], false);
        let body = verify_attestation(
            State(state),
            Json(serde_json::to_value(attestation).expect("serialize attestation")),
        )
        .await
        .expect("attestation verifies")
        .0;
        assert_eq!(body["kind"], "attestation");
        assert_eq!(body["ok"], true);
        assert_eq!(body["cryptographic"], true);
        assert_eq!(body["trusted_signer"], true);
        assert_eq!(body["environment_match"], true);
        assert_eq!(body["fresh"], true);
    }

    #[tokio::test]
    async fn verify_accepts_signed_statements_and_guards_and_rejects_cross_kind_tampering() {
        let state = test_state(false);
        let statement = crate::domain::statement::signed_test_statement([7; 32], false);
        let body = verify_attestation(
            State(state.clone()),
            Json(serde_json::to_value(statement.clone()).unwrap()),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(body["kind"], "statement");
        assert_eq!(body["id"], statement.id);
        assert_eq!(body["cryptographic"], true);
        assert_eq!(body["trusted_signer"], true);
        assert_eq!(body["environment_match"], true);
        assert!(body["fresh"].is_null());
        assert_eq!(body["ok"], true);

        let mut cross_kind = statement.clone();
        cross_kind.kind = "attestation".to_owned();
        let error = verify_attestation(
            State(state.clone()),
            Json(serde_json::to_value(cross_kind).unwrap()),
        )
        .await
        .unwrap_err();
        assert_eq!(error.0, StatusCode::BAD_REQUEST);
        assert!(error.1.0["error"].is_string());

        let mut hybrid = serde_json::to_value(statement.clone()).unwrap();
        hybrid["pool"] = serde_json::json!({});
        let error = verify_attestation(State(state.clone()), Json(hybrid)).await.unwrap_err();
        assert_eq!(error.0, StatusCode::BAD_REQUEST);
        assert!(error.1.0["error"].is_string());
        let guard = crate::domain::guard::signed_test_guard([7; 32]);
        let body =
            verify_attestation(State(state.clone()), Json(serde_json::to_value(guard).unwrap()))
                .await
                .unwrap()
                .0;
        assert_eq!(body["kind"], "guard");
        assert_eq!(body["cryptographic"], true);
        assert_eq!(body["trusted_signer"], true);
        assert_eq!(body["environment_match"], true);
        assert!(body["fresh"].is_null());
        assert_eq!(body["ok"], true);

        let mut tampered = statement;
        tampered.observed_at.push_str(" altered");
        let body = verify_attestation(State(state), Json(serde_json::to_value(tampered).unwrap()))
            .await
            .unwrap()
            .0;
        assert_eq!(body["cryptographic"], false);
        assert_eq!(body["ok"], false);
    }

    #[tokio::test]
    async fn verify_rejects_unknown_guard_fields_at_every_depth() {
        let state = test_state(false);
        let guard = crate::domain::guard::signed_test_guard([7; 32]);
        let valid = serde_json::to_value(guard).expect("Guard JSON");
        for path in ["", "/identity"] {
            let mut value = valid.clone();
            let target =
                if path.is_empty() { &mut value } else { value.pointer_mut(path).unwrap() };
            target
                .as_object_mut()
                .unwrap()
                .insert("unexpected".to_owned(), serde_json::json!(true));
            let error = verify_attestation(State(state.clone()), Json(value)).await.unwrap_err();
            assert_eq!(error.0, StatusCode::BAD_REQUEST, "unknown field at {path}");
        }
    }
    #[tokio::test]
    async fn verify_rejects_unknown_attestation_and_statement_fields_recursively() {
        let state = test_state(false);

        let attestation = crate::app::attestation::signed_test_attestation([7; 32], false);
        let mut value = serde_json::to_value(attestation).expect("attestation JSON");
        value["pool"]["base"]["unexpected"] = serde_json::json!(true);
        let error = verify_attestation(State(state.clone()), Json(value)).await.unwrap_err();
        assert_eq!(error.0, StatusCode::BAD_REQUEST);
        let attestation = crate::app::attestation::signed_test_attestation([7; 32], false);
        let mut value = serde_json::to_value(attestation).expect("attestation JSON");
        value["verdict"] = serde_json::json!({
            "Mismatch": {
                "claimed": "NVDA",
                "actual": "NVDA",
                "unexpected": true
            }
        });
        let error = verify_attestation(State(state.clone()), Json(value)).await.unwrap_err();
        assert_eq!(error.0, StatusCode::BAD_REQUEST);
        let attestation = crate::app::attestation::signed_test_attestation([7; 32], false);
        let mut value = serde_json::to_value(attestation).expect("attestation JSON");
        value["registry_entry"] = serde_json::json!({
            "issuer": "Example Publisher",
            "ticker": "NVDA",
            "name": "NVIDIA",
            "chain": "Base",
            "contract": "0x0000000000000000000000000000000000000001",
            "decimals": 18,
            "source": "fixture",
            "source_url": "https://example.invalid/registry",
            "last_checked": "2026-10-01T00:00:00Z",
            "removed_at": null,
            "stale_since": null,
            "unexpected": true
        });
        let error = verify_attestation(State(state.clone()), Json(value)).await.unwrap_err();
        assert_eq!(error.0, StatusCode::BAD_REQUEST);

        let statement = crate::domain::statement::signed_test_statement([7; 32], false);
        let mut value = serde_json::to_value(statement).expect("statement JSON");
        value["assets"] = serde_json::json!([{
            "wallet": "0x0000000000000000000000000000000000000001",
            "chain": "Base",
            "contract": "0x0000000000000000000000000000000000000002",
            "ticker": "NVDA",
            "issuer": "Example Publisher",
            "issuer_match": true,
            "balance": "1",
            "decimals": 18,
            "powers_observed_at": null,
            "powers_block": null,
            "powers_slot": null,
            "slot": null,
            "powers_summary": {
                "can_seize": [],
                "can_block": [{ "code": "wallet_blocked", "detail": "Observed", "unexpected": true }],
                "can_change_rules": [],
                "unavailable": []
            }
        }]);
        let error = verify_attestation(State(state), Json(value)).await.unwrap_err();
        assert_eq!(error.0, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn verify_route_accepts_awkward_float_round_trips() {
        for quote_share in [1.1129609814871755e-8, 0.1 + 0.2] {
            let attestation = crate::app::attestation::signed_test_attestation_with_quote_share(
                [7; 32],
                false,
                quote_share,
            );
            let id = attestation.id.clone();
            let serialized = serde_json::to_vec(&attestation).expect("serialize attestation");
            let response = crate::adapters::web::router(test_state(false))
                .oneshot(
                    Request::post("/verify")
                        .header(axum::http::header::CONTENT_TYPE, "application/json")
                        .body(Body::from(serialized))
                        .expect("verify request"),
                )
                .await
                .expect("verify response");

            assert_eq!(response.status(), axum::http::StatusCode::OK);
            let body =
                to_bytes(response.into_body(), 64 * 1024).await.expect("verify response body");
            let value: serde_json::Value =
                serde_json::from_slice(&body).expect("verify response JSON");
            assert_eq!(value["id"], id);
            assert_eq!(value["cryptographic"], true, "quote share: {quote_share:?}");
            assert_eq!(value["trusted_signer"], true);
            assert_eq!(value["fresh"], true);
            assert_eq!(value["ok"], true);
        }
    }

    fn assert_negative_dimensions(
        body: &serde_json::Value,
        cryptographic: bool,
        trusted_signer: bool,
        environment_match: bool,
        fresh: bool,
    ) {
        assert_eq!(body["ok"], false);
        assert_eq!(body["cryptographic"], cryptographic);
        assert_eq!(body["trusted_signer"], trusted_signer);
        assert_eq!(body["environment_match"], environment_match);
        assert_eq!(body["fresh"], fresh);
    }

    #[tokio::test]
    async fn verify_handler_reports_each_negative_validation_dimension() {
        let mut bad_signature = crate::app::attestation::signed_test_attestation([7; 32], false);
        bad_signature.signature.push('x');
        let body = verify_attestation(
            State(test_state(false)),
            Json(serde_json::to_value(bad_signature).unwrap()),
        )
        .await
        .unwrap()
        .0;
        assert_negative_dimensions(&body, false, false, true, false);

        let untrusted = crate::app::attestation::signed_test_attestation([9; 32], false);
        let body = verify_attestation(
            State(test_state(false)),
            Json(serde_json::to_value(untrusted).unwrap()),
        )
        .await
        .unwrap()
        .0;
        assert_negative_dimensions(&body, true, false, true, true);

        let environment_mismatch = crate::app::attestation::signed_test_attestation([7; 32], true);
        let body = verify_attestation(
            State(test_state(false)),
            Json(serde_json::to_value(environment_mismatch).unwrap()),
        )
        .await
        .unwrap()
        .0;
        assert_negative_dimensions(&body, true, true, false, true);

        let expired = crate::app::attestation::expired_signed_test_attestation([7; 32], false);
        let body = verify_attestation(
            State(test_state(false)),
            Json(serde_json::to_value(expired).unwrap()),
        )
        .await
        .unwrap()
        .0;
        assert_negative_dimensions(&body, true, true, true, false);
    }

    #[tokio::test]
    async fn check_api_rejects_malformed_public_input_before_reader_access() {
        let result = api_check(State(test_state(false)), Path("not-an-address".to_owned())).await;
        assert!(matches!(result, Err(StatusCode::BAD_REQUEST)));
    }
    #[tokio::test]
    async fn statement_api_rejects_wallets_that_do_not_match_selected_chains() {
        let response = crate::adapters::web::router(test_state(false))
            .oneshot(
                Request::post("/api/statement")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "wallets": ["not-a-wallet"],
                            "chains": ["solana"]
                        })
                        .to_string(),
                    ))
                    .expect("statement request"),
            )
            .await
            .expect("statement response");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn uncached_statement_page_returns_not_found() {
        let response = crate::adapters::web::router(test_state(false))
            .oneshot(
                Request::get("/statements/not-cached")
                    .body(Body::empty())
                    .expect("statement page request"),
            )
            .await
            .expect("statement page response");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn cached_statement_json_route_returns_the_signed_payload() {
        let state = test_state(false);
        let statement = crate::domain::statement::signed_test_statement([7; 32], false);
        let id = statement.id.clone();
        state.app.statement_cache.insert(id.clone(), statement).await;
        let response = crate::adapters::web::router(state)
            .oneshot(
                Request::get(format!("/api/statement/{id}"))
                    .body(Body::empty())
                    .expect("statement JSON request"),
            )
            .await
            .expect("statement JSON response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["id"], id);
        assert_eq!(value["kind"], "statement");
    }

    #[tokio::test]
    async fn powers_api_routes_invalid_and_unregistered_contracts_without_chain_reads() {
        let app = crate::adapters::web::router(test_state(false));
        for (address, expected) in [
            ("not-an-address", StatusCode::BAD_REQUEST),
            ("0x0000000000000000000000000000000000000001", StatusCode::NOT_FOUND),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::get(format!("/api/powers/{address}"))
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("powers API response");
            assert_eq!(response.status(), expected);
        }
    }
    #[tokio::test]
    async fn mcp_legacy_tools_are_counted_from_the_bounded_json_body() {
        let state = test_state(false);
        let app = crate::adapters::web::router(state.clone());
        for name in ["qed_check", "qed_wallet"] {
            let body = serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": { "name": name, "arguments": { "address": "not-an-address" } }
            })
            .to_string();
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/mcp")
                        .header(header::CONTENT_TYPE, "application/json")
                        .header(header::CONTENT_LENGTH, body.len().to_string())
                        .body(Body::from(body))
                        .expect("MCP request"),
                )
                .await
                .expect("MCP response");
            assert_eq!(response.status(), StatusCode::OK);
            let body = to_bytes(response.into_body(), 64 * 1024).await.expect("MCP tool body");
            let value: serde_json::Value =
                serde_json::from_slice(&body).expect("JSON-RPC response");
            assert_eq!(value["result"]["isError"], true);
        }
        let metrics = state.usage_stats.snapshot();
        assert_eq!(metrics.checks, 1);
        assert_eq!(metrics.wallet_requests, 1);
    }

    #[tokio::test]
    async fn registry_api_reuses_etag_for_unchanged_snapshot() {
        let state = test_state(false);
        let first = api_registry(State(state.clone()), HeaderMap::new()).await;
        assert_eq!(first.status(), StatusCode::OK);
        let etag = first.headers().get(header::ETAG).cloned().expect("registry response has ETag");

        let mut headers = HeaderMap::new();
        headers.insert(header::IF_NONE_MATCH, etag);
        let second = api_registry(State(state), headers).await;
        assert_eq!(second.status(), StatusCode::NOT_MODIFIED);
    }
}
