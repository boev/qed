use super::pages;
use crate::{
    attest::{self, Attestation},
    check,
    discovery::{self, FeaturedPool, LeaderboardEntry},
    state::{AppState, RegistryApiCache, current_registry_version},
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
use std::sync::Arc;
pub(crate) async fn healthz() -> impl IntoResponse {
    Json(serde_json::json!({ "status": "ok" }))
}
pub(crate) async fn api_registry(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let registry_hash = state.registry_hash.read().map(|hash| hash.clone()).unwrap_or_default();
    let registry_hash = format!("{registry_hash}:{}", current_registry_version());
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
) -> Result<Json<check::CheckResult>, StatusCode> {
    let address = address.trim();
    if !check::valid_public_input(address) {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(Json(check::check(&state, address).await))
}

pub(crate) async fn api_wallet(
    State(state): State<AppState>,
    Json(request): Json<pages::WalletRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let holdings = pages::wallet_holdings(&state, &request.address).await?;
    Ok(Json(serde_json::json!({
        "address": request.address,
        "holdings": holdings,
    })))
}
pub(crate) async fn api_attestation(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, StatusCode> {
    let attestation = attest::get_async(&state, &id).await.ok_or(StatusCode::NOT_FOUND)?;
    Ok(([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(attestation)).into_response())
}
pub(crate) async fn well_known(State(state): State<AppState>) -> Response {
    (
        [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")],
        Json(serde_json::json!({
            "name": "QED",
            "version": 1,
            "algorithm": "Ed25519",
            "public_key": attest::public_key_b58(&state),
            "key": attest::public_key_b58(&state),
            "dev": state.dev_signer,
        })),
    )
        .into_response()
}
pub(crate) async fn verify_attestation(
    State(state): State<AppState>,
    Json(attestation): Json<Attestation>,
) -> Json<serde_json::Value> {
    let cryptographic = attest::verify(&attestation).is_ok();
    let trusted_signer = cryptographic && attest::signer_trusted(&state, &attestation);
    let environment_match = attestation.dev == state.dev_signer;
    let fresh = cryptographic && attest::fresh(&attestation);
    Json(serde_json::json!({
        "ok": cryptographic && trusted_signer && environment_match && fresh,
        "id": attestation.id,
        "cryptographic": cryptographic,
        "trusted_signer": trusted_signer,
        "environment_match": environment_match,
        "fresh": fresh,
    }))
}
#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use std::collections::HashMap;

    fn test_state(dev_signer: bool) -> AppState {
        AppState {
            registry: std::sync::Arc::new(tokio::sync::RwLock::new(Vec::new())),
            registry_status: std::sync::Arc::new(tokio::sync::RwLock::new(
                crate::discovery::RegistrySnapshot::default(),
            )),
            readers: std::sync::Arc::new(Vec::new()),
            http: reqwest::Client::new(),
            check_cache: moka::future::Cache::builder().build(),
            leaderboard_check_cache: moka::future::Cache::builder().build(),
            check_inflight: std::sync::Arc::new(tokio::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
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
            dev_signer,
            attest_store: std::sync::Arc::new(crate::attest::FileAttestationStore::new(
                std::path::PathBuf::from("target/test-api-attestations"),
            )),
            board_store: std::sync::Arc::new(crate::discovery::DurableBoardStore::default()),
            registry_hash: std::sync::Arc::new(std::sync::RwLock::new(String::new())),
            registry_api_cache: std::sync::Arc::new(tokio::sync::RwLock::new(None)),
            public_url: std::sync::Arc::new("http://localhost:3000".to_owned()),
            rate_limiter: std::sync::Arc::new(crate::state::RateLimiter::default()),
            expensive_concurrency: std::sync::Arc::new(tokio::sync::Semaphore::new(32)),
            wallet_concurrency: std::sync::Arc::new(tokio::sync::Semaphore::new(1)),
            registry_api_concurrency: std::sync::Arc::new(tokio::sync::Semaphore::new(4)),
        }
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
        state.prices.write().await.prices.push(crate::discovery::PricePoint {
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
        let attestation = crate::attest::signed_test_attestation([7; 32], false);
        let body = verify_attestation(State(state), Json(attestation)).await.0;
        assert_eq!(body["ok"], true);
        assert_eq!(body["cryptographic"], true);
        assert_eq!(body["trusted_signer"], true);
        assert_eq!(body["environment_match"], true);
        assert_eq!(body["fresh"], true);
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
        let mut bad_signature = crate::attest::signed_test_attestation([7; 32], false);
        bad_signature.signature.push('x');
        let body = verify_attestation(State(test_state(false)), Json(bad_signature)).await.0;
        assert_negative_dimensions(&body, false, false, true, false);

        let untrusted = crate::attest::signed_test_attestation([9; 32], false);
        let body = verify_attestation(State(test_state(false)), Json(untrusted)).await.0;
        assert_negative_dimensions(&body, true, false, true, true);

        let environment_mismatch = crate::attest::signed_test_attestation([7; 32], true);
        let body = verify_attestation(State(test_state(false)), Json(environment_mismatch)).await.0;
        assert_negative_dimensions(&body, true, true, false, true);

        let expired = crate::attest::expired_signed_test_attestation([7; 32], false);
        let body = verify_attestation(State(test_state(false)), Json(expired)).await.0;
        assert_negative_dimensions(&body, true, true, true, false);
    }

    #[tokio::test]
    async fn check_api_rejects_malformed_public_input_before_reader_access() {
        let result = api_check(State(test_state(false)), Path("not-an-address".to_owned())).await;
        assert!(matches!(result, Err(StatusCode::BAD_REQUEST)));
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
