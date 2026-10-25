use crate::{
    adapters::state::{AdminAuth, AppState},
    domain::{chain::Chain, registry},
    ports::{RpcRequestContext, RpcRequestOutcome, with_rpc_request_context},
};
use axum::{
    Router,
    extract::State,
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use std::net::{IpAddr, SocketAddr};
use tower_http::{compression::CompressionLayer, services::ServeDir};

mod api;
mod asset;
mod docs;
mod mcp;
mod pages;
mod views;
pub(crate) use api::{
    admin_stats, api_attestation, api_check, api_featured, api_guard, api_guard_post,
    api_leaderboard, api_powers, api_prices, api_registry, api_statement, api_statement_get,
    api_stats, api_status, api_wallet, healthz, refresh_stats_snapshot, restore_stats_snapshot,
    stats_snapshot_csv, stats_snapshot_json, verify_attestation, well_known,
};
pub(crate) use docs::{api_docs, imprint, llms, llms_full, privacy, terms, validated_feed};
pub(crate) use pages::{
    certificate, chain_page, copies_page, featured, glossary_page, guide_verify_page, index,
    recheck_certificate, registry_page, registry_table, stats_page, token_lookup, token_page,
    validated, validated_detail,
};
pub(crate) fn wallet_error_status(error: crate::app::wallet::WalletError) -> StatusCode {
    match error {
        crate::app::wallet::WalletError::InvalidAddress => StatusCode::BAD_REQUEST,
        crate::app::wallet::WalletError::DeadlineExceeded => StatusCode::GATEWAY_TIMEOUT,
        crate::app::wallet::WalletError::BudgetExceeded => StatusCode::SERVICE_UNAVAILABLE,
        crate::app::wallet::WalletError::ReaderUnavailable => StatusCode::BAD_GATEWAY,
    }
}
pub(crate) fn guard_error_status(error: &crate::app::guard::GuardError) -> StatusCode {
    match error {
        crate::app::guard::GuardError::InvalidAddress
        | crate::app::guard::GuardError::InvalidWallet => StatusCode::BAD_REQUEST,
        crate::app::guard::GuardError::ReaderUnavailable => StatusCode::BAD_GATEWAY,
        crate::app::guard::GuardError::DeadlineExceeded => StatusCode::GATEWAY_TIMEOUT,
        crate::app::guard::GuardError::DailyBudgetExceeded => StatusCode::SERVICE_UNAVAILABLE,
        crate::app::guard::GuardError::CrawlerCacheMiss => StatusCode::FORBIDDEN,
        crate::app::guard::GuardError::Signing => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

const ASSET_VERSION: u64 = asset_version();
const REGISTRY_PAGE_SIZE: usize = 200;

const fn hash_bytes(mut hash: u64, bytes: &[u8]) -> u64 {
    let mut index = 0;
    while index < bytes.len() {
        hash ^= bytes[index] as u64;
        hash = hash.wrapping_mul(0x100000001b3);
        index += 1;
    }
    hash
}
const fn asset_version() -> u64 {
    let hash = hash_bytes(0xcbf29ce484222325, include_bytes!("../../static/style.css"));
    let hash = hash_bytes(hash, include_bytes!("../../static/home.css"));
    let hash = hash_bytes(hash, include_bytes!("../../static/htmx.min.js"));
    let hash = hash_bytes(hash, include_bytes!("../../static/icons.svg"));
    let hash = hash_bytes(hash, include_bytes!("../../static/logo.svg"));
    let hash = hash_bytes(hash, include_bytes!("../../static/app.js"));
    let hash = hash_bytes(hash, include_bytes!("../../static/leaderboard.js"));
    let hash = hash_bytes(hash, include_bytes!("../../static/vue.global.prod.js"));
    hash_bytes(hash, include_bytes!("../../static/hex-comets.js"))
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/copies", get(copies_page))
        .route("/stats", get(stats_page))
        .route("/stats.json", get(stats_snapshot_json))
        .route("/stats.csv", get(stats_snapshot_csv))
        .route("/check", get(|| async { axum::response::Redirect::permanent("/guard") }))
        .route("/guard", get(pages::guard_page).post(pages::guard_form_submit))
        .route("/guard/{chain}/{address}", get(pages::guard_result_page))
        .route("/registry", get(registry_page))
        .route("/registry/table", get(registry_table))
        .route("/tokens", get(token_lookup))
        .route("/tokens/{ticker}", get(token_page))
        .route("/assets/{chain}/{kind}/{address}", get(asset::page))
        .route("/assets/{chain}/{kind}/{address}/history.json", get(asset::export_json))
        .route("/assets/{chain}/{kind}/{address}/history.csv", get(asset::export_csv))
        .route("/assets/{chain}/{kind}/{address}/observations/{id}", get(asset::observation))
        .route("/chains/{chain_name}", get(chain_page))
        .route("/glossary", get(glossary_page))
        .route("/docs", get(pages::docs_page))
        .route("/docs/llm", get(pages::docs_llm_page))
        .route("/docs/api-quick-start", get(pages::docs_api_quick_start_page))
        .route("/about", get(pages::about_page))
        .route("/security", get(pages::security_page))
        .route("/changelog", get(pages::changelog_page))
        .route("/blog", get(pages::blog_page))
        .route("/blog.xml", get(docs::blog_feed))
        .route("/blog/{slug}", get(pages::blog_post_page))
        .route("/guide/verify-a-stock-token", get(guide_verify_page))
        .route("/validated", get(validated))
        .route("/validated/{chain}/{subject}", get(validated_detail))
        .route("/imprint", get(imprint))
        .route("/privacy", get(privacy))
        .route("/terms", get(terms))
        .route("/robots.txt", get(robots))
        .route("/sitemap.xml", get(sitemap))
        .route("/validated.xml", get(validated_feed))
        .route("/changelog.xml", get(docs::changelog_feed))
        .route("/llms.txt", get(llms))
        .route("/llms-full.txt", get(llms_full))
        .route("/api", get(api_docs))
        .route("/.well-known/mcp/server-card.json", get(mcp::server_card))
        .route("/mcp", post(mcp::handle))
        .route("/openapi.json", get(docs::document))
        .route("/pools/featured", get(featured))
        .route("/healthz", get(healthz))
        .route("/admin/stats", get(admin_stats))
        .route("/api/registry", get(api_registry))
        .route("/api/pools/featured", get(api_featured))
        .route("/api/leaderboard", get(api_leaderboard))
        .route("/api/stats", get(api_stats))
        .route("/api/prices", get(api_prices))
        .route("/api/status", get(api_status))
        .route("/api/check/{address}", get(api_check))
        .route("/api/powers/{address}", get(api_powers))
        .route("/api/guard/{address}", get(api_guard))
        .route("/api/guard", post(api_guard_post))
        .route("/api/wallet", post(api_wallet))
        .route("/api/statement/{id}", get(api_statement_get))
        .route("/api/statement", post(api_statement))
        .route("/statements", get(pages::statements_page).post(pages::create_statement_form))
        .route("/statements/{id}/download.csv", get(pages::statement_csv))
        .route("/statements/{id}/verify", get(pages::verify_statement_page))
        .route("/statements/{id}/recheck", post(pages::recheck_statement))
        .route("/statements/{id}", get(pages::statement_page))
        .route("/api/attest/{id}", get(api_attestation))
        .route(
            "/verify",
            post(verify_attestation)
                .layer(axum::extract::DefaultBodyLimit::disable())
                .layer(middleware::from_fn(api::verify_body_size_limit)),
        )
        .route("/v/{id}", get(certificate))
        .route("/v/{id}/verify", get(pages::verify_certificate_page))
        .route("/v/{id}/recheck", post(recheck_certificate))
        .route("/.well-known/qed.json", get(well_known))
        .nest_service("/static", ServeDir::new("static"))
        .with_state(state.clone())
        .layer(middleware::from_fn(format_api_bad_request))
        .layer(CompressionLayer::new())
        .layer(middleware::from_fn(cache_static))
        .layer(middleware::from_fn(security_headers))
        .layer(middleware::from_fn_with_state(state.clone(), admin_auth))
        .layer(middleware::from_fn_with_state(state.clone(), rate_limit))
        .layer(middleware::from_fn_with_state(state, usage_metrics))
}

async fn format_api_bad_request(request: axum::extract::Request, next: Next) -> Response {
    let path = request.uri().path();
    let is_json_api = path.starts_with("/api/") || path == "/verify";
    let response = next.run(request).await;
    if !is_json_api || response.status() != StatusCode::BAD_REQUEST {
        return response;
    }

    let (mut parts, body) = response.into_parts();
    let body = axum::body::to_bytes(body, 16 * 1024).await.unwrap_or_default();
    let detail = serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .and_then(|value| {
            value
                .get("detail")
                .or_else(|| value.get("error"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .or_else(|| {
            let text = String::from_utf8_lossy(&body).trim().to_owned();
            (!text.is_empty()).then_some(text)
        })
        .unwrap_or_else(|| "The request is missing or contains invalid fields.".to_owned());
    let body = serde_json::to_vec(&serde_json::json!({
        "error": "Bad request",
        "detail": detail,
    }))
    .expect("API error response serializes");

    parts.headers.remove(header::CONTENT_LENGTH);
    parts
        .headers
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json; charset=utf-8"));
    let mut response = Response::new(axum::body::Body::from(body));
    *response.status_mut() = parts.status;
    *response.headers_mut() = parts.headers;
    response
}
async fn admin_auth(
    State(state): State<AppState>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    if request.uri().path() == "/admin/stats"
        && !valid_admin_authorization(request.headers(), &state.admin_auth)
    {
        return (
            StatusCode::UNAUTHORIZED,
            [
                (header::WWW_AUTHENTICATE, HeaderValue::from_static("Basic realm=\"qed-admin\"")),
                (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
            ],
            "Unauthorized\n",
        )
            .into_response();
    }
    next.run(request).await
}

fn valid_admin_authorization(headers: &HeaderMap, auth: &AdminAuth) -> bool {
    let Some(value) = headers.get(header::AUTHORIZATION) else {
        return false;
    };
    let encoded = value.as_bytes();
    if encoded.len() < 6 || !encoded[..5].eq_ignore_ascii_case(b"Basic") || encoded[5] != b' ' {
        return false;
    }
    let Ok(decoded) = STANDARD.decode(&encoded[6..]) else {
        return false;
    };
    let Some(separator) = decoded.iter().position(|byte| *byte == b':') else {
        return false;
    };
    auth.matches(&decoded[..separator], &decoded[separator + 1..])
}

async fn usage_metrics(
    State(state): State<AppState>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    let path = request.uri().path();
    let method = request.method();
    let is_mcp = path == "/mcp";
    let is_admin = path == "/admin/stats";
    let is_api = path == "/api" || path.starts_with("/api/") || is_mcp;
    let is_check = (method == Method::GET && path.starts_with("/api/check/"))
        || (method == Method::GET && path.starts_with("/api/powers/"))
        || (method == Method::GET
            && (path.starts_with("/api/guard/") || path.starts_with("/guard/")))
        || (method == Method::POST
            && (path == "/check" || path == "/verify" || path == "/api/guard" || path == "/guard"))
        || (method == Method::POST && path.starts_with("/v/") && path.ends_with("/recheck"));
    let is_wallet = path == "/api/wallet";
    let is_health = path == "/healthz";
    let is_static_asset = path == "/static" || path.starts_with("/static/");
    state.usage_stats.record_request(
        is_api,
        is_check,
        is_wallet,
        is_admin,
        is_health,
        is_static_asset,
    );

    let response = next.run(request).await;
    if response
        .headers()
        .get(header::CONTENT_TYPE)
        .is_some_and(|value| value.as_bytes().starts_with(b"text/html"))
    {
        state.usage_stats.record_html_page_view();
    }
    state.usage_stats.record_response(response.status().as_u16());
    response
}

async fn rate_limit(
    State(state): State<AppState>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    let path = request.uri().path();
    let is_wallet = request.method() == Method::POST && path == "/api/wallet";
    let is_registry_api = request.method() == Method::GET && path == "/api/registry";
    let is_expensive = (request.method() == Method::POST
        && (path == "/mcp" || path == "/api/statement" || path == "/statements"))
        || (request.method() == Method::POST && (path == "/check" || path == "/api/guard"))
        || path.starts_with("/api/powers/")
        || path.starts_with("/api/check/")
        || path == "/guard"
        || path.starts_with("/guard/")
        || path.starts_with("/api/guard/")
        || path == "/verify"
        || (request.method() == Method::GET
            && (path.starts_with("/tokens/")
                || path.starts_with("/validated/")
                || path.starts_with("/v/")))
        || (request.method() == Method::POST
            && (path.starts_with("/v/") || path.starts_with("/statements/"))
            && path.ends_with("/recheck"))
        || (request.method() == Method::GET
            && (path == "/stats"
                || path == "/stats.json"
                || path == "/stats.csv"
                || path == "/api/stats"
                || path.starts_with("/api/statement/")
                || path.starts_with("/api/attest/")
                || path.starts_with("/statements/")));
    let is_read_limited = path.starts_with("/api/attest/")
        || path == "/.well-known/qed.json"
        || path == "/admin/stats"
        || is_registry_api
        || (request.method() == Method::GET && path.starts_with("/assets/"));
    let is_on_demand = is_on_demand_rpc_request(request.method(), path);
    let crawler = request
        .headers()
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .is_some_and(is_automated_crawler);
    let global_on_demand =
        is_global_on_demand_request(request.method(), path, crawler, is_on_demand);

    if is_wallet || is_expensive || is_read_limited || is_on_demand {
        let ip = trusted_client_ip(&request);
        if !state.rate_limiter.allow(ip) {
            return (StatusCode::TOO_MANY_REQUESTS, "Rate limit exceeded. Try again later.\n")
                .into_response();
        }
    }
    if global_on_demand && !state.rate_limiter.allow_on_demand_request() {
        return (StatusCode::TOO_MANY_REQUESTS, "QED is busy — try again in a minute")
            .into_response();
    }
    if is_wallet {
        let Ok(_permit) = state.wallet_concurrency.clone().try_acquire_owned() else {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                "A wallet scan is already in progress. Try again later.\n",
            )
                .into_response();
        };
        return run_with_rpc_context(&state, request, next, is_on_demand, crawler).await;
    }
    if is_registry_api {
        let Ok(_permit) = state.registry_api_concurrency.clone().try_acquire_owned() else {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                "The registry endpoint is busy. Try again later.\n",
            )
                .into_response();
        };
        return run_with_rpc_context(&state, request, next, is_on_demand, crawler).await;
    }
    if is_expensive {
        let Ok(_permit) = state.expensive_concurrency.clone().try_acquire_owned() else {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                "Too many checks in progress. Try again later.\n",
            )
                .into_response();
        };
        return run_with_rpc_context(&state, request, next, is_on_demand, crawler).await;
    }
    run_with_rpc_context(&state, request, next, is_on_demand, crawler).await
}

fn is_on_demand_rpc_request(method: &Method, path: &str) -> bool {
    (method == Method::GET
        && (path.starts_with("/api/check/")
            || path.starts_with("/api/powers/")
            || path.starts_with("/api/guard/")
            || path.starts_with("/guard/")
            || path.starts_with("/tokens/")))
        || (method == Method::POST
            && (path == "/mcp"
                || path == "/guard"
                || path == "/api/guard"
                || path == "/api/wallet"
                || path == "/api/statement"
                || path == "/statements"
                || (path.starts_with("/v/") && path.ends_with("/recheck"))
                || (path.starts_with("/statements/") && path.ends_with("/recheck"))))
}

fn is_global_on_demand_request(
    method: &Method,
    path: &str,
    crawler: bool,
    is_on_demand: bool,
) -> bool {
    is_on_demand
        && !(method == Method::POST && path == "/mcp")
        && !(crawler && method == Method::GET && path.starts_with("/tokens/"))
}

fn is_automated_crawler(user_agent: &str) -> bool {
    let user_agent = user_agent.to_ascii_lowercase();
    [
        "crawler/",
        "crawl/",
        "spider/",
        "slurp",
        "googlebot",
        "bingbot",
        "duckduckbot",
        "yandexbot",
        "baiduspider",
        "applebot",
        "gptbot",
        "claudebot",
        "ccbot",
        "perplexitybot",
        "bytespider",
        "amazonbot",
        "facebookexternalhit",
        "twitterbot",
        "linkedinbot",
        "petalbot",
        "semrushbot",
        "ahrefsbot",
        "mj12bot",
    ]
    .iter()
    .any(|marker| user_agent.contains(marker))
}

async fn run_with_rpc_context(
    state: &AppState,
    request: axum::extract::Request,
    next: Next,
    on_demand: bool,
    crawler: bool,
) -> Response {
    if !on_demand {
        return next.run(request).await;
    }
    let path = request.uri().path().to_owned();
    let (response, outcome): (Response, RpcRequestOutcome) =
        with_rpc_request_context(RpcRequestContext::new(true, crawler), next.run(request)).await;
    if path == "/mcp" || path.starts_with("/tokens/") {
        return response;
    }
    if outcome.crawler_blocked() || (crawler && response.status() == StatusCode::FORBIDDEN) {
        return crawler_block_response(state, &path);
    }
    if outcome.budget_rejected() {
        return daily_budget_response(state, &path);
    }
    response
}

const DAILY_RPC_BUDGET_MESSAGE: &str =
    "QED daily read budget reached - cached results still served; try again after 00:00 UTC";

fn crawler_block_response(state: &AppState, path: &str) -> Response {
    const MESSAGE: &str = "Automated crawlers get cached results only.";
    if path == "/guard" || path.starts_with("/guard/") {
        return pages::guard_error_response(state, path, StatusCode::FORBIDDEN, MESSAGE)
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }
    if is_statement_form_or_recheck(path) {
        return pages::statement_error_response(state, StatusCode::FORBIDDEN, MESSAGE)
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }
    (StatusCode::FORBIDDEN, axum::Json(serde_json::json!({"error": MESSAGE}))).into_response()
}

fn daily_budget_response(state: &AppState, path: &str) -> Response {
    if path == "/guard" || path.starts_with("/guard/") {
        return pages::guard_error_response(
            state,
            path,
            StatusCode::SERVICE_UNAVAILABLE,
            DAILY_RPC_BUDGET_MESSAGE,
        )
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }
    if is_statement_form_or_recheck(path) {
        return pages::statement_error_response(
            state,
            StatusCode::SERVICE_UNAVAILABLE,
            DAILY_RPC_BUDGET_MESSAGE,
        )
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }
    (
        StatusCode::SERVICE_UNAVAILABLE,
        axum::Json(serde_json::json!({"error": DAILY_RPC_BUDGET_MESSAGE})),
    )
        .into_response()
}

fn is_statement_form_or_recheck(path: &str) -> bool {
    path == "/statements"
        || (path.starts_with("/statements/") && path.ends_with("/recheck"))
        || (path.starts_with("/v/") && path.ends_with("/recheck"))
}

fn trusted_client_ip(request: &axum::extract::Request) -> IpAddr {
    let unspecified = IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED);
    let Some(value) = request.headers().get("x-forwarded-for") else {
        return unspecified;
    };
    let Ok(value) = value.to_str() else {
        return unspecified;
    };
    let rightmost = value.rsplit(',').next().map(str::trim).unwrap_or_default();
    rightmost
        .parse::<IpAddr>()
        .or_else(|_| rightmost.parse::<SocketAddr>().map(|address| address.ip()))
        .unwrap_or(unspecified)
}

async fn security_headers(request: axum::extract::Request, next: Next) -> Response {
    let path = request.uri().path();
    let cors = (request.method() == Method::POST && path == "/verify")
        || path.starts_with("/api/attest/")
        || path == "/.well-known/qed.json";
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; object-src 'none'; base-uri 'self'; frame-ancestors 'none'; form-action 'self'; frame-src 'none'",
        ),
    );
    response
        .headers_mut()
        .insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("strict-origin-when-cross-origin"),
    );
    response.headers_mut().insert(
        header::STRICT_TRANSPORT_SECURITY,
        HeaderValue::from_static("max-age=31536000; includeSubDomains"),
    );
    if cors {
        response
            .headers_mut()
            .insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
    }
    response
}

async fn cache_static(request: axum::extract::Request, next: Next) -> Response {
    let cacheable = request.method() == Method::GET;
    let is_static = request.uri().path().starts_with("/static/");
    let has_version = request
        .uri()
        .query()
        .is_some_and(|query| query.split('&').any(|part| part == "v" || part.starts_with("v=")));
    let mut response = next.run(request).await;
    if cacheable && is_static && response.status().is_success() {
        let cache_control =
            if has_version { "public, max-age=31536000, immutable" } else { "no-cache" };
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static(cache_control));
    } else if cacheable
        && response
            .headers()
            .get(header::CONTENT_TYPE)
            .is_some_and(|value| value.as_bytes().starts_with(b"text/html"))
        && response.status().is_success()
    {
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=60"));
        let etag = format!("\"qed-html-{ASSET_VERSION}\"");
        if let Ok(value) = HeaderValue::try_from(etag) {
            response.headers_mut().insert(header::ETAG, value);
        }
    }
    response
}

fn wants_fragment(headers: &axum::http::HeaderMap) -> bool {
    if headers
        .get("HX-History-Restore-Request")
        .is_some_and(|value| value.as_bytes().eq_ignore_ascii_case(b"true"))
    {
        return false;
    }
    headers.get("HX-Request").is_some_and(|value| value.as_bytes().eq_ignore_ascii_case(b"true"))
}

pub(super) fn timestamp_html(value: &str) -> String {
    let value = crate::adapters::content::html_escape(value);
    format!("<time datetime=\"{value}\">{value}</time>")
}

fn format_html_timestamp(value: &str) -> Option<String> {
    if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(value) {
        return Some(parsed.with_timezone(&chrono::Utc).format("%-d %b %Y, %H:%M UTC").to_string());
    }
    chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .ok()
        .map(|date| date.format("%-d %b %Y").to_string())
}

fn time_attribute<'a>(opening_tag: &'a str, name: &str) -> Option<&'a str> {
    let marker = match name {
        "datetime" => " datetime=\"",
        "title" => " title=\"",
        _ => return None,
    };
    opening_tag.split_once(marker)?.1.split_once('"').map(|(value, _)| value)
}

fn latest_release_menu_item() -> &'static str {
    static RELEASE_ITEM: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
        crate::adapters::content::changelog_entries()
            .into_iter()
            .next()
            .map(|entry| {
                let anchor = crate::adapters::content::html_escape(&entry.anchor);
                let label = crate::adapters::content::html_escape(&entry.label);
                let headline = crate::adapters::content::html_escape(&entry.headline);
                format!(
                    "<a class=\"docs-menu-release\" href=\"/changelog#{anchor}\" title=\"New in {label} — {headline} → Changelog\"><strong>New in {label}</strong><span class=\"docs-menu-release-text\"><span class=\"docs-menu-release-headline\">{headline}</span><span class=\"docs-menu-release-link\">→ Changelog</span></span></a>"
                )
            })
            .unwrap_or_default()
    });
    RELEASE_ITEM.as_str()
}

pub(super) fn format_html_timestamps(html: &str) -> String {
    const LATEST_RELEASE_MARKER: &str = "<!-- QED_LATEST_RELEASE -->";
    const PACKAGE_VERSION_MARKER: &str = "<!-- QED_PACKAGE_VERSION -->";
    let release_item = latest_release_menu_item();
    let mut output = String::with_capacity(html.len() + release_item.len() * 2);
    let mut remaining = html;
    loop {
        let mut next = (usize::MAX, 0);
        if let Some(index) = remaining.find("<time") {
            next = (index, 1);
        }
        if let Some(index) = remaining.find(LATEST_RELEASE_MARKER)
            && index < next.0
        {
            next = (index, 2);
        }
        if let Some(index) = remaining.find(PACKAGE_VERSION_MARKER)
            && index < next.0
        {
            next = (index, 3);
        }
        if next.0 == usize::MAX {
            output.push_str(remaining);
            break;
        }
        output.push_str(&remaining[..next.0]);
        let remainder = &remaining[next.0..];
        match next.1 {
            2 => {
                output.push_str(release_item);
                remaining = &remainder[LATEST_RELEASE_MARKER.len()..];
            }
            3 => {
                output.push_str(env!("CARGO_PKG_VERSION"));
                remaining = &remainder[PACKAGE_VERSION_MARKER.len()..];
            }
            _ => {
                let Some(opening_length) = remainder.find('>') else {
                    output.push_str(remainder);
                    break;
                };
                let opening_end = opening_length + 1;
                let Some(closing_offset) = remainder[opening_end..].find("</time>") else {
                    output.push_str(remainder);
                    break;
                };
                let closing_start = opening_end + closing_offset;
                let closing_end = closing_start + "</time>".len();
                let opening_tag = &remainder[..opening_end];
                let contents = &remainder[opening_end..closing_start];
                let existing_datetime = time_attribute(opening_tag, "datetime");
                let datetime = existing_datetime.unwrap_or_else(|| contents.trim());
                if let Some(human) = format_html_timestamp(datetime) {
                    output.push_str(&opening_tag[..opening_end - 1]);
                    if existing_datetime.is_none() {
                        output.push_str(" datetime=\"");
                        output.push_str(datetime);
                        output.push('"');
                    }
                    if time_attribute(opening_tag, "title").is_none() {
                        output.push_str(" title=\"");
                        output.push_str(datetime);
                        output.push('"');
                    }
                    output.push('>');
                    if let Some(start) = contents.find(datetime) {
                        output.push_str(&contents[..start]);
                        output.push_str(&human);
                        output.push_str(&contents[start + datetime.len()..]);
                    } else {
                        output.push_str(contents);
                    }
                    output.push_str("</time>");
                } else {
                    output.push_str(&remainder[..closing_end]);
                }
                remaining = &remainder[closing_end..];
            }
        }
    }
    output
}

fn render_page<T: askama::Template>(
    template: T,
    _fragment: bool,
) -> Result<axum::response::Html<String>, StatusCode> {
    let page = template.render().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(axum::response::Html(format_html_timestamps(&page)))
}

async fn robots(State(state): State<AppState>) -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        format!(
            "User-agent: *\nAllow: /\nDisallow: /guard/\nDisallow: /api/\nDisallow: /mcp\nDisallow: /validated/\nDisallow: /v/*/recheck\nDisallow: /verify\nDisallow: /statements/\n\nSitemap: {}/sitemap.xml\n",
            state.public_url
        ),
    )
}
async fn sitemap(State(state): State<AppState>) -> Response {
    let now = chrono::Utc::now().date_naive().to_string();
    let mut urls = vec![
        ("/".to_owned(), now.clone()),
        ("/check".to_owned(), now.clone()),
        ("/registry".to_owned(), now.clone()),
        ("/guard".to_owned(), now.clone()),
        ("/validated".to_owned(), now.clone()),
        ("/glossary".to_owned(), now.clone()),
        ("/docs".to_owned(), now.clone()),
        ("/docs/llm".to_owned(), now.clone()),
        ("/docs/api-quick-start".to_owned(), now.clone()),
        ("/about".to_owned(), now.clone()),
        ("/security".to_owned(), now.clone()),
        ("/changelog".to_owned(), now.clone()),
        ("/blog".to_owned(), now.clone()),
        ("/statements".to_owned(), now.clone()),
        ("/copies".to_owned(), now.clone()),
        ("/guide/verify-a-stock-token".to_owned(), now.clone()),
        ("/imprint".to_owned(), now.clone()),
        ("/privacy".to_owned(), now.clone()),
        ("/terms".to_owned(), now.clone()),
    ]
    .into_iter()
    .map(|(path, lastmod)| {
        format!(
            "<url><loc>{}{}</loc><lastmod>{}</lastmod></url>",
            xml_escape(&state.public_url),
            path,
            lastmod
        )
    })
    .collect::<Vec<_>>();
    let posts = match crate::adapters::content::blog_posts() {
        Ok(posts) => crate::adapters::content::published_blog_posts(posts),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    for post in posts {
        urls.push(format!(
            "<url><loc>{}/blog/{}</loc><lastmod>{}</lastmod></url>",
            xml_escape(&state.public_url),
            xml_escape(&post.slug),
            post.date
        ));
    }
    for chain in [Chain::Solana, Chain::RobinhoodChain, Chain::Base, Chain::Ethereum, Chain::Bnb] {
        urls.push(format!(
            "<url><loc>{}/chains/{}</loc><lastmod>{}</lastmod></url>",
            xml_escape(&state.public_url),
            views::chain_slug(chain),
            now
        ));
    }
    let registry = state.registry.read().await;
    let mut tickers: Vec<_> = registry
        .iter()
        .filter(|entry| registry::matchable(entry))
        .map(|entry| entry.ticker.clone())
        .collect();
    tickers.sort();
    tickers.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
    for ticker in tickers {
        urls.push(format!(
            "<url><loc>{}/tokens/{}</loc><lastmod>{}</lastmod></url>",
            xml_escape(&state.public_url),
            xml_escape(&ticker),
            now
        ));
    }
    drop(registry);
    let mut assets = std::collections::BTreeMap::new();
    for (key, _, projection) in state.app.history.states() {
        if let Some(latest) = projection.latest {
            assets
                .entry(key.path())
                .and_modify(|time: &mut chrono::DateTime<chrono::Utc>| {
                    *time = (*time).max(latest.observed_at)
                })
                .or_insert(latest.observed_at);
        }
    }
    for (path, time) in assets {
        urls.push(format!(
            "<url><loc>{}{}</loc><lastmod>{}</lastmod></url>",
            xml_escape(&state.public_url),
            xml_escape(&path),
            time.to_rfc3339()
        ));
    }
    (
        [(header::CONTENT_TYPE, "application/xml; charset=utf-8")],
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">{}</urlset>",
            urls.join("")
        ),
    )
        .into_response()
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::state::RateLimiter;
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;

    struct BlockingStatementReader {
        started: std::sync::Arc<tokio::sync::Notify>,
        release: std::sync::Arc<tokio::sync::Notify>,
    }

    #[async_trait::async_trait]
    impl crate::ports::ChainReader for BlockingStatementReader {
        fn chain(&self) -> crate::domain::chain::Chain {
            crate::domain::chain::Chain::Solana
        }

        async fn read_pool(
            &self,
            _address: &str,
        ) -> Result<crate::domain::pool::PoolInfo, crate::domain::pool::PoolError> {
            Err(crate::domain::pool::PoolError::Reader("unused".to_owned()))
        }

        async fn statement_holdings(
            &self,
            owner: &str,
            _entries: &[crate::domain::registry::Entry],
            block: Option<u64>,
        ) -> Result<
            (
                Vec<crate::domain::statement::StatementHolding>,
                crate::domain::statement::StatementPosition,
            ),
            crate::domain::pool::PoolError,
        > {
            self.started.notify_one();
            self.release.notified().await;
            Ok((
                Vec::new(),
                crate::domain::statement::StatementPosition {
                    chain: crate::domain::chain::Chain::Solana,
                    wallet: owner.to_owned(),
                    block,
                    min_slot: None,
                    max_slot: None,
                },
            ))
        }
    }
    struct RejectedStatementReader;
    #[async_trait::async_trait]
    impl crate::ports::ChainReader for RejectedStatementReader {
        fn chain(&self) -> crate::domain::chain::Chain {
            crate::domain::chain::Chain::Solana
        }
        async fn read_pool(
            &self,
            _address: &str,
        ) -> Result<crate::domain::pool::PoolInfo, crate::domain::pool::PoolError> {
            Err(crate::domain::pool::PoolError::Reader("unused".to_owned()))
        }
        async fn statement_holdings(
            &self,
            owner: &str,
            _entries: &[crate::domain::registry::Entry],
            block: Option<u64>,
        ) -> Result<
            (
                Vec<crate::domain::statement::StatementHolding>,
                crate::domain::statement::StatementPosition,
            ),
            crate::domain::pool::PoolError,
        > {
            let context = crate::ports::current_rpc_request_context()
                .expect("statement request has RPC context");
            context.mark(if context.crawler {
                crate::ports::RPC_CRAWLER_BLOCKED
            } else {
                crate::ports::RPC_BUDGET_REJECTED
            });
            Ok((
                Vec::new(),
                crate::domain::statement::StatementPosition {
                    chain: crate::domain::chain::Chain::Solana,
                    wallet: owner.to_owned(),
                    block,
                    min_slot: None,
                    max_slot: None,
                },
            ))
        }
    }

    fn statement_request() -> Request<Body> {
        Request::post("/api/statement")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                r#"{"wallets":["11111111111111111111111111111111"],"chains":["solana"]}"#,
            ))
            .expect("statement request")
    }

    fn statement_form_request() -> Request<Body> {
        Request::post("/statements")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from("wallets=11111111111111111111111111111111&chains=solana"))
            .expect("statement form request")
    }

    #[test]
    fn basic_auth_rejects_missing_malformed_and_wrong_values() {
        let auth = AdminAuth::new(Some("unit-admin"), Some("unit-password"));
        let mut headers = HeaderMap::new();
        assert!(!valid_admin_authorization(&headers, &auth));

        headers.insert(header::AUTHORIZATION, HeaderValue::from_static("Bearer token"));
        assert!(!valid_admin_authorization(&headers, &auth));

        let encoded = STANDARD.encode(b"unit-admin:wrong");
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Basic {encoded}")).expect("header value"),
        );
        assert!(!valid_admin_authorization(&headers, &auth));
    }

    #[test]
    fn basic_auth_accepts_exact_pair_without_exposing_credentials() {
        let auth = AdminAuth::new(Some("unit-admin"), Some("unit-password"));
        let encoded = STANDARD.encode(b"unit-admin:unit-password");
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Basic {encoded}")).expect("header value"),
        );
        assert!(valid_admin_authorization(&headers, &auth));

        let unauthorized = (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, HeaderValue::from_static("Basic realm=\"qed-admin\""))],
            "Unauthorized\n",
        )
            .into_response();
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            unauthorized.headers().get(header::WWW_AUTHENTICATE).unwrap(),
            "Basic realm=\"qed-admin\""
        );
    }
    #[test]
    fn trusted_client_ip_uses_only_rightmost_forwarded_ip() {
        let request = Request::builder()
            .header("x-forwarded-for", "198.51.100.10, 203.0.113.5")
            .body(Body::empty())
            .unwrap();
        assert_eq!(trusted_client_ip(&request), "203.0.113.5".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn trusted_client_ip_accepts_rightmost_socket_address() {
        let request = Request::builder()
            .header("x-forwarded-for", "198.51.100.10, 203.0.113.5:4567")
            .body(Body::empty())
            .unwrap();
        assert_eq!(trusted_client_ip(&request), "203.0.113.5".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn trusted_client_ip_does_not_fall_back_to_an_attacker_supplied_left_value() {
        let request = Request::builder()
            .header("x-forwarded-for", "198.51.100.10, invalid")
            .body(Body::empty())
            .unwrap();
        assert_eq!(trusted_client_ip(&request), IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED));
    }

    #[test]
    fn crawler_detection_avoids_unrelated_product_names() {
        assert!(!is_automated_crawler("CUBOT/1.0"));
        assert!(is_automated_crawler("Googlebot/2.1"));
        assert!(is_automated_crawler("GenericCrawler/1.0"));
    }

    #[tokio::test]
    async fn statement_post_uses_shared_ip_limit_and_holds_expensive_permit_through_completion() {
        use std::sync::Arc;
        use tokio::sync::{Notify, Semaphore};

        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let mut state = AppState::for_tests(
            Vec::new(),
            vec![Box::new(BlockingStatementReader {
                started: Arc::clone(&started),
                release: Arc::clone(&release),
            })],
            false,
        );
        state.expensive_concurrency = Arc::new(Semaphore::new(1));
        let app = router(state.clone());
        let first_app = app.clone();
        let first = tokio::spawn(async move {
            first_app.oneshot(statement_request()).await.expect("first statement response")
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), started.notified())
            .await
            .expect("first request enters its reader");

        let concurrent =
            app.clone().oneshot(statement_request()).await.expect("concurrent statement response");
        assert_eq!(concurrent.status(), StatusCode::TOO_MANY_REQUESTS);
        release.notify_one();
        let first = tokio::time::timeout(std::time::Duration::from_secs(2), first)
            .await
            .expect("first statement completes")
            .expect("first request task");
        assert_eq!(first.status(), StatusCode::OK);

        let ip = IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED);
        for _ in 0..58 {
            assert!(state.rate_limiter.allow(ip));
        }
        let rate_limited =
            app.oneshot(statement_request()).await.expect("rate-limited statement response");
        assert_eq!(rate_limited.status(), StatusCode::TOO_MANY_REQUESTS);
    }
    #[tokio::test]
    async fn api_guard_post_uses_shared_ip_limit_expensive_permit_and_check_metrics() {
        let client_ip: IpAddr = "198.51.100.42".parse().expect("client IP");
        let guard_request = || {
            Request::post("/api/guard")
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-forwarded-for", client_ip.to_string())
                .body(Body::from(
                    r#"{"address":"0x0000000000000000000000000000000000000001","chain":"base"}"#,
                ))
                .expect("Guard request")
        };
        let state = AppState::for_tests(Vec::new(), Vec::new(), false);
        for _ in 0..60 {
            assert!(state.rate_limiter.allow(client_ip));
        }
        let response =
            router(state.clone()).oneshot(guard_request()).await.expect("Guard response");
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(state.usage_stats.snapshot().checks, 1);

        let mut state = AppState::for_tests(Vec::new(), Vec::new(), false);
        state.expensive_concurrency = std::sync::Arc::new(tokio::sync::Semaphore::new(0));
        let response =
            router(state.clone()).oneshot(guard_request()).await.expect("Guard response");
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(state.usage_stats.snapshot().checks, 1);
    }
    #[tokio::test]
    async fn api_check_get_uses_shared_ip_limit_expensive_permit_and_check_metrics() {
        let client_ip: IpAddr = "198.51.100.43".parse().expect("client IP");
        let check_request = || {
            Request::get("/api/check/0x0000000000000000000000000000000000000001")
                .header("x-forwarded-for", client_ip.to_string())
                .body(Body::empty())
                .expect("API check request")
        };
        let state = AppState::for_tests(Vec::new(), Vec::new(), false);
        for _ in 0..60 {
            assert!(state.rate_limiter.allow(client_ip));
        }
        let response =
            router(state.clone()).oneshot(check_request()).await.expect("check response");
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(state.usage_stats.snapshot().checks, 1);

        let mut state = AppState::for_tests(Vec::new(), Vec::new(), false);
        state.expensive_concurrency = std::sync::Arc::new(tokio::sync::Semaphore::new(0));
        let response =
            router(state.clone()).oneshot(check_request()).await.expect("check response");
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(state.usage_stats.snapshot().checks, 1);
    }

    #[tokio::test]
    async fn global_on_demand_limit_rejects_requests_across_client_ips() {
        let mut state = AppState::for_tests(Vec::new(), Vec::new(), false);
        state.rate_limiter =
            std::sync::Arc::new(RateLimiter::with_rpc_limits(4, 8, 10_000, 10_000, 1));
        let check_request = |client_ip: &str| {
            Request::get("/api/check/0x0000000000000000000000000000000000000001")
                .header("x-forwarded-for", client_ip)
                .body(Body::empty())
                .expect("API check request")
        };

        let first = router(state.clone())
            .oneshot(check_request("198.51.100.10"))
            .await
            .expect("first check response");
        assert_eq!(first.status(), StatusCode::OK);

        let second = router(state)
            .oneshot(check_request("203.0.113.20"))
            .await
            .expect("second check response");
        assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
    }
    #[tokio::test]
    async fn crawler_token_cache_hit_does_not_consume_global_on_demand_admission() {
        use crate::domain::{
            chain::Chain,
            powers::{PowersRecord, SourceVerified, SourceVerifiedSubject},
            registry::Entry,
        };
        use std::sync::Arc;
        let address = "11111111111111111111111111111111";
        let mut state = AppState::for_tests(
            vec![Entry {
                issuer: "Fixture issuer".to_owned(),
                ticker: "NVDA".to_owned(),
                name: "Fixture NVDA".to_owned(),
                chain: Chain::Solana,
                contract: address.to_owned(),
                decimals: Some(9),
                source: "fixture".to_owned(),
                source_url: "https://example.invalid/registry".to_owned(),
                last_checked: "2026-10-17T00:00:00Z".to_owned(),
                removed_at: None,
                stale_since: None,
                official_deployments: Vec::new(),
            }],
            Vec::new(),
            false,
        );
        state
            .app
            .powers_cache
            .insert(
                (Chain::Solana, address.to_owned()),
                PowersRecord {
                    chain: Chain::Solana,
                    contract: address.to_owned(),
                    can_seize: Vec::new(),
                    can_block: Vec::new(),
                    can_change_rules: Vec::new(),
                    token_paused: None,
                    sanctions_list: None,
                    unavailable: Vec::new(),
                    source_verified_subject: SourceVerifiedSubject::TokenProgram,
                    source_verified: SourceVerified::Unavailable,
                    source_verified_proxy: None,
                    observed_at: chrono::Utc::now().to_rfc3339(),
                    block: None,
                    slot: None,
                    reads: Vec::new(),
                },
            )
            .await;
        state.rate_limiter = Arc::new(RateLimiter::with_rpc_limits(4, 8, 10_000, 10_000, 1));
        let page = router(state.clone())
            .oneshot(
                Request::get("/tokens/NVDA")
                    .header(header::USER_AGENT, "Googlebot/2.1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(page.status(), StatusCode::OK);
        let check = router(state)
            .oneshot(
                Request::get("/api/check/0x0000000000000000000000000000000000000001")
                    .header("x-forwarded-for", "198.51.100.45")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(check.status(), StatusCode::OK);
    }
    #[tokio::test]
    async fn statement_form_and_recheck_rejections_use_the_page_frame() {
        let state = AppState::for_tests(Vec::new(), Vec::new(), false);
        for path in ["/statements", "/statements/example/recheck", "/v/example/recheck"] {
            for (response, status) in [
                (crawler_block_response(&state, path), StatusCode::FORBIDDEN),
                (daily_budget_response(&state, path), StatusCode::SERVICE_UNAVAILABLE),
            ] {
                assert_eq!(response.status(), status, "{path}");
                assert!(
                    response
                        .headers()
                        .get(header::CONTENT_TYPE)
                        .and_then(|v| v.to_str().ok())
                        .is_some_and(|v| v.starts_with("text/html"))
                );
                let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
                let html = std::str::from_utf8(&body).unwrap();
                assert!(html.starts_with("<!doctype html>"));
                assert_eq!(html.matches("<main ").count(), 1);
                assert!(html.contains("<nav class=\"site-nav\""));
                assert!(html.trim_end().ends_with("</html>"));
            }
        }
    }
    #[tokio::test]
    async fn statement_form_budget_and_crawler_rejections_render_the_page_frame() {
        for (crawler, status) in
            [(false, StatusCode::SERVICE_UNAVAILABLE), (true, StatusCode::FORBIDDEN)]
        {
            let state =
                AppState::for_tests(Vec::new(), vec![Box::new(RejectedStatementReader)], false);
            let mut request = Request::post("/statements")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
            if crawler {
                request = request.header(header::USER_AGENT, "Googlebot/2.1");
            }
            let response = router(state)
                .oneshot(
                    request
                        .body(Body::from("wallets=11111111111111111111111111111111&chains=solana"))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), status);
            assert!(
                response
                    .headers()
                    .get(header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .is_some_and(|v| v.starts_with("text/html"))
            );
            let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let html = std::str::from_utf8(&body).unwrap();
            assert!(html.starts_with("<!doctype html>"));
            assert_eq!(html.matches("<main ").count(), 1);
            assert!(html.contains("<nav class=\"site-nav\""));
            assert!(html.trim_end().ends_with("</html>"));
        }
    }

    #[tokio::test]
    async fn wallet_page_routes_are_removed_without_removing_wallet_api() {
        let app = router(AppState::for_tests(Vec::new(), Vec::new(), false));
        for request in [
            Request::get("/wallet").body(Body::empty()).unwrap(),
            Request::post("/wallet")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from("address="))
                .unwrap(),
        ] {
            let response =
                app.clone().oneshot(request).await.expect("removed wallet route response");
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
        }

        let address = "0x0000000000000000000000000000000000000001";
        let response = app
            .oneshot(
                Request::post("/api/wallet")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(format!(r#"{{"address":"{address}"}}"#)))
                    .unwrap(),
            )
            .await
            .expect("wallet API response");
        assert_eq!(response.status(), StatusCode::OK);
        let body =
            axum::body::to_bytes(response.into_body(), 1024 * 1024).await.expect("wallet API body");
        let value: serde_json::Value = serde_json::from_slice(&body).expect("wallet API JSON");
        assert_eq!(value, serde_json::json!({"address": address, "holdings": []}));
    }

    #[tokio::test]
    async fn new_pages_have_metadata_nav_and_draft_exclusions() {
        let app = router(AppState::for_tests(Vec::new(), Vec::new(), false));
        let pages = [
            ("/registry", "QED | Issuer registry"),
            ("/guard", "QED | Guard review"),
            ("/docs", "QED | Documentation"),
            ("/docs/llm", "QED | Use QED with an LLM"),
            ("/docs/api-quick-start", "QED | API quick start"),
            ("/api", "QED | API reference"),
            ("/about", "QED | About"),
            ("/security", "QED | Security"),
            ("/changelog", "QED | Changelog"),
            ("/blog", "QED | Blog"),
            ("/imprint", "QED | Imprint"),
            ("/privacy", "QED | Privacy"),
            ("/terms", "QED | Terms"),
            ("/statements", "QED | Statement"),
            ("/copies", "QED | Copied stock tokens"),
            ("/stats", "QED | Leaderboard statistics"),
            ("/guide/verify-a-stock-token", "QED | Contract-first verification guide"),
            ("/glossary", "QED | Glossary"),
        ];
        for (path, expected_title) in pages {
            let response = app
                .clone()
                .oneshot(Request::get(path).body(Body::empty()).expect("page request"))
                .await
                .expect("page response");
            assert_eq!(response.status(), StatusCode::OK, "{path}");
            let body =
                axum::body::to_bytes(response.into_body(), usize::MAX).await.expect("page body");
            let body = String::from_utf8(body.to_vec()).expect("UTF-8 page");
            assert!(
                body.contains("<html lang=\"en\" data-theme=\"auto\">"),
                "{path}: theme default"
            );
            assert!(
                body.contains(&format!("<title>{expected_title}</title>")),
                "{path}: page title",
            );
            assert!(body.contains("<p class=\"eyebrow\">"), "{path}");
            assert!(body.contains("<h1 class=\"page-title\">"), "{path}");
            assert!(body.contains("<meta name=\"description\" content="), "{path}");
            assert!(
                body.contains("Contact: <a href=\"mailto:contact@web3-energy.com\">contact@web3-energy.com</a>"),
                "{path}: shared footer contact"
            );
            assert!(body.contains("<meta property=\"og:description\" content="), "{path}");
            assert!(body.contains("<meta property=\"og:url\" content="), "{path}");
            let canonical = body
                .split_once("<link rel=\"canonical\" href=\"")
                .expect("canonical link")
                .1
                .split('"')
                .next()
                .expect("canonical URL");
            assert!(canonical.ends_with(path), "{path}: {canonical}");
            let nav = body
                .split_once("<nav class=\"site-nav\"")
                .expect("site navigation")
                .1
                .split("</nav>")
                .next()
                .expect("navigation close");
            let top_level =
                nav.split_once("<details class=\"docs-menu\">").expect("Docs dropdown").0;
            let mut previous = 0;
            for path in ["/guard", "/registry", "/statements", "/copies", "/stats"] {
                let position = top_level.find(&format!("href=\"{path}\"")).expect("main nav link");
                assert!(position >= previous, "{path}");
                previous = position;
            }
            let mobile_nav = body
                .split_once("<nav aria-label=\"Mobile navigation\"")
                .expect("mobile navigation")
                .1
                .split("</nav>")
                .next()
                .expect("mobile navigation close");
            let mut mobile_previous = 0;
            for path in ["/statements", "/copies", "/stats"] {
                let position =
                    mobile_nav.find(&format!("href=\"{path}\"")).expect("mobile nav link");
                assert!(position >= mobile_previous, "mobile {path}");
                mobile_previous = position;
            }
            let docs_menu = nav.split_once("<details class=\"docs-menu\">").unwrap().1;
            for path in ["/docs", "/api", "/docs/llm", "/llms.txt", "/blog", "/changelog#"] {
                assert!(docs_menu.contains(&format!("href=\"{path}")), "{path}");
            }
            assert!(!nav.contains("version-pill"), "version pill sits outside the centered nav");
            let actions = body
                .split_once("<div class=\"header-actions\">")
                .expect("header actions")
                .1
                .split("</div>")
                .next()
                .expect("header actions close");
            assert!(actions.contains(&format!(">v{}</a>", env!("CARGO_PKG_VERSION"))));
            let version = actions.find("class=\"version-pill\"").expect("version pill");
            let github = actions.find("class=\"repository-link\"").expect("GitHub link");
            let theme = actions.find("data-theme-toggle").expect("theme toggle");
            assert!(version < github && github < theme, "header action order");
            assert!(!actions.contains("My Wallet"), "removed wallet-page link");
            assert!(!body.contains("href=\"/wallet\""), "removed wallet-page URL");
            let footer = body
                .split_once("<nav aria-label=\"Legal and project links\"")
                .expect("footer navigation")
                .1
                .split("</nav>")
                .next()
                .expect("footer navigation close");
            assert_eq!(footer.matches("<a ").count(), 9, "{path}");
            for label in [
                "Changelog",
                "Glossary",
                "Blog",
                "About",
                "Security",
                "Imprint",
                "Privacy",
                "Terms",
                "GitHub",
            ] {
                assert!(footer.contains(&format!(">{label}</a>")), "{path}: {label}");
            }
            if path == "/about" {
                let releases = crate::adapters::content::changelog_entries();
                let release_date = releases
                    .first()
                    .and_then(|entry| entry.date.as_deref())
                    .expect("current changelog release date");
                assert!(body.contains("Project facts"));
                assert!(body.contains(&format!(
                    "v{} · released {release_date}",
                    env!("CARGO_PKG_VERSION")
                )));
                assert!(body.contains("Questions, issuer corrections or takedown requests:"));
                assert!(body.contains("github.com/boev/qed"));
                assert!(body.contains(">Security policy</a>"));
            }
            if path == "/statements" {
                assert!(body.contains(
                    "<h1 class=\"page-title\">What did these wallets hold, provably?</h1>"
                ));
                assert!(body.contains(
                    "<form class=\"statement-form\" action=\"/statements\" method=\"post\""
                ));
                assert!(body.contains("<textarea id=\"statement-wallets\""));
                assert!(body.contains("statement-chain-option"));
                assert!(body.contains("statement-block"));
                assert!(body.contains("class=\"button primary-button\""));
            }
        }
        let legacy_check = app
            .clone()
            .oneshot(Request::get("/check").body(Body::empty()).expect("legacy check request"))
            .await
            .expect("legacy check redirect");
        assert_eq!(legacy_check.status(), StatusCode::PERMANENT_REDIRECT);
        assert_eq!(legacy_check.headers()["location"], "/guard");

        let posts = crate::adapters::content::blog_posts().expect("blog sources parse");
        let published_slugs = posts
            .iter()
            .filter(|post| !post.draft)
            .map(|post| post.slug.clone())
            .collect::<Vec<_>>();
        for slug in published_slugs {
            let path = format!("/blog/{slug}");
            let response = app
                .clone()
                .oneshot(
                    Request::get(path.clone()).body(Body::empty()).expect("published blog request"),
                )
                .await
                .expect("published blog response");
            assert_eq!(response.status(), StatusCode::OK, "{path}");
        }
        let draft_slugs =
            posts.into_iter().filter(|post| post.draft).map(|post| post.slug).collect::<Vec<_>>();
        assert!(!draft_slugs.is_empty(), "draft route regression needs a draft fixture");
        for slug in draft_slugs {
            let response = app
                .clone()
                .oneshot(
                    Request::get(format!("/blog/{slug}"))
                        .body(Body::empty())
                        .expect("draft blog request"),
                )
                .await
                .expect("draft blog response");
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
        }

        let about = app
            .clone()
            .oneshot(Request::get("/about").body(Body::empty()).expect("about request"))
            .await
            .expect("about response");
        let about = axum::body::to_bytes(about.into_body(), usize::MAX).await.unwrap();
        let about = String::from_utf8(about.to_vec()).unwrap();
        assert!(about.contains(
            "A ticker is not a contract. Compare the pool with the issuer's published stock-token contract."
        ));
        assert!(about.contains(
            "QED checks whether a pool uses the stock-token contract published by its issuer."
        ));
        let home = app
            .clone()
            .oneshot(Request::get("/").body(Body::empty()).expect("home request"))
            .await
            .expect("home response");
        let home = axum::body::to_bytes(home.into_body(), usize::MAX).await.unwrap();
        let home = String::from_utf8(home.to_vec()).unwrap();
        assert!(home.contains(
            "A ticker is not a contract. Compare the pool with the issuer's published stock-token contract."
        ));
        let visible_home = home.replace("<span class=\"nowrap\">", "").replace("</span>", "");
        assert!(visible_home.contains(
            "QED checks whether a pool uses the stock-token contract published by its issuer."
        ));
        let docs = app
            .clone()
            .oneshot(Request::get("/docs").body(Body::empty()).expect("docs request"))
            .await
            .expect("docs response");
        let docs = axum::body::to_bytes(docs.into_body(), usize::MAX).await.unwrap();
        let docs = String::from_utf8(docs.to_vec()).unwrap();
        assert!(docs.contains("Start with a question"));
        assert!(!docs.contains("Module map and dependency rule"));
        assert!(!docs.contains("Runtime architecture"));
        assert!(docs.contains("Documentation navigation"));
        assert!(docs.contains("href=\"/docs\" aria-current=\"page\">Documentation overview</a>"));
        let security = app
            .clone()
            .oneshot(Request::get("/security").body(Body::empty()).expect("security request"))
            .await
            .expect("security response");
        let security = axum::body::to_bytes(security.into_body(), usize::MAX).await.unwrap();
        assert!(
            String::from_utf8(security.to_vec())
                .unwrap()
                .contains("Report suspected security issues privately")
        );

        let blog = app
            .clone()
            .oneshot(Request::get("/blog").body(Body::empty()).expect("blog request"))
            .await
            .expect("blog response");
        let blog = axum::body::to_bytes(blog.into_body(), usize::MAX).await.unwrap();
        let blog = String::from_utf8(blog.to_vec()).unwrap();
        assert!(!blog.contains("what-qed-powers-shows"));
        assert!(!blog.contains("What QED Powers shows"));
        let blog_feed = app
            .clone()
            .oneshot(Request::get("/blog.xml").body(Body::empty()).expect("blog feed request"))
            .await
            .expect("blog feed response");
        assert_eq!(
            blog_feed.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/atom+xml; charset=utf-8"
        );
        let blog_feed = axum::body::to_bytes(blog_feed.into_body(), usize::MAX).await.unwrap();
        let blog_feed = String::from_utf8(blog_feed.to_vec()).unwrap();
        assert!(!blog_feed.contains("What QED Powers shows"));
        let changelog_feed = app
            .clone()
            .oneshot(
                Request::get("/changelog.xml").body(Body::empty()).expect("changelog feed request"),
            )
            .await
            .expect("changelog feed response");
        let changelog_feed =
            axum::body::to_bytes(changelog_feed.into_body(), usize::MAX).await.unwrap();
        let changelog_feed = String::from_utf8(changelog_feed.to_vec()).unwrap();
        assert_eq!(
            changelog_feed.matches("<entry>").count(),
            crate::adapters::content::changelog_entries().len()
        );
        let sitemap = app
            .clone()
            .oneshot(Request::get("/sitemap.xml").body(Body::empty()).expect("sitemap request"))
            .await
            .expect("sitemap response");
        let sitemap = axum::body::to_bytes(sitemap.into_body(), usize::MAX).await.unwrap();
        let sitemap = String::from_utf8(sitemap.to_vec()).unwrap();
        assert!(sitemap.contains("/docs"));
        assert!(sitemap.contains("/docs/llm"));
        assert!(sitemap.contains("/docs/api-quick-start"));
        assert!(sitemap.contains("/statements"));
        assert!(sitemap.contains("/copies"));
        assert!(!sitemap.contains("/blog/what-qed-powers-shows"));
        assert!(!sitemap.contains("/validated/"));
        let robots = app
            .clone()
            .oneshot(Request::get("/robots.txt").body(Body::empty()).expect("robots request"))
            .await
            .expect("robots response");
        let robots = axum::body::to_bytes(robots.into_body(), usize::MAX).await.unwrap();
        let robots = String::from_utf8(robots.to_vec()).unwrap();
        assert!(robots.contains("Disallow: /guard/"));
        assert!(robots.contains("Disallow: /api/"));
        assert!(!robots.contains("Disallow: /tokens"));
        let llms = app
            .clone()
            .oneshot(Request::get("/llms.txt").body(Body::empty()).expect("llms request"))
            .await
            .expect("llms response");
        let llms = axum::body::to_bytes(llms.into_body(), usize::MAX).await.unwrap();
        let llms = String::from_utf8(llms.to_vec()).unwrap();
        assert!(llms.contains("http://localhost:3000/copies"));
        let llms_full = app
            .clone()
            .oneshot(Request::get("/llms-full.txt").body(Body::empty()).expect("full llms request"))
            .await
            .expect("full llms response");
        let llms_full = axum::body::to_bytes(llms_full.into_body(), usize::MAX).await.unwrap();
        let llms_full = String::from_utf8(llms_full.to_vec()).unwrap();
        assert!(llms_full.contains("GET /copies"));
        assert!(!sitemap.contains("/blog/what-a-qed-statement-is"));

        for path in [
            "/registry",
            "/statements",
            "/docs",
            "/docs/llm",
            "/docs/api-quick-start",
            "/changelog",
            "/about",
            "/api",
            "/openapi.json",
            "/guide/verify-a-stock-token",
            "/glossary",
            "/llms.txt",
            "/security",
            "/blog",
            "/imprint",
            "/privacy",
            "/terms",
            "/validated",
            "/chains/solana",
            "/.well-known/mcp/server-card.json",
        ] {
            let response = app
                .clone()
                .oneshot(Request::get(path).body(Body::empty()).expect("linked route request"))
                .await
                .expect("linked route response");
            assert_eq!(response.status(), StatusCode::OK, "{path}");
            if path == "/api" {
                let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
                let body = String::from_utf8(body.to_vec()).unwrap();
                assert!(body.contains("<a href=\"/terms\">Terms</a>"));
            }
        }
    }
    #[tokio::test]
    async fn documentation_guides_render_with_active_navigation_and_examples() {
        let app = router(AppState::for_tests(Vec::new(), Vec::new(), false));
        let llm = app
            .clone()
            .oneshot(Request::get("/docs/llm").body(Body::empty()).expect("LLM guide request"))
            .await
            .expect("LLM guide response");
        let llm = axum::body::to_bytes(llm.into_body(), usize::MAX).await.unwrap();
        let llm = String::from_utf8(llm.to_vec()).unwrap();
        assert_eq!(llm.matches("<svg class=\"guide-flow\"").count(), 1);
        let llm_flow = llm
            .split_once("<svg class=\"guide-flow\"")
            .expect("LLM flow SVG")
            .1
            .split_once("</svg>")
            .expect("LLM flow close")
            .0;
        assert!(llm_flow.len() < 6 * 1024);
        assert!(llm_flow.contains("flow-dot flow-dot-moving"));
        assert!(llm_flow.contains("flow-dot flow-dot-static"));
        assert!(llm_flow.contains("<animateMotion"));
        assert!(llm.contains("Is this the real NVDA?"));
        assert!(llm.contains("Match, signed"));
        assert!(llm.contains("https://qed.web3-energy.com/mcp"));
        assert!(llm.contains("https://qed.web3-energy.com/.well-known/mcp/server-card.json"));
        assert!(!llm.contains("href=\"https://qed.web3-energy.com/mcp\""));
        assert!(llm.contains("<code>https://qed.web3-energy.com/mcp</code>"));
        assert!(
            llm.contains("claude mcp add --transport http qed https://qed.web3-energy.com/mcp")
        );
        assert_eq!(llm.matches("aria-current=\"page\"").count(), 1);
        for tool in [
            "qed_check",
            "qed_powers",
            "qed_wallet",
            "qed_statement",
            "qed_registry_lookup",
            "qed_verify",
            "qed_guard",
        ] {
            assert!(llm.contains(tool), "missing {tool}");
        }
        assert!(llm.contains("Does this pool use the issuer's published stock-token contract?"));

        let verification = app
            .clone()
            .oneshot(
                Request::get("/guide/verify-a-stock-token")
                    .body(Body::empty())
                    .expect("verification guide request"),
            )
            .await
            .expect("verification guide response");
        let verification =
            axum::body::to_bytes(verification.into_body(), usize::MAX).await.unwrap();
        let verification = String::from_utf8(verification.to_vec()).unwrap();
        assert_eq!(verification.matches("<svg class=\"guide-flow\"").count(), 1);
        let verify_flow = verification
            .split_once("<svg class=\"guide-flow\"")
            .expect("verification flow SVG")
            .1
            .split_once("</svg>")
            .expect("verification flow close")
            .0;
        assert!(verify_flow.len() < 6 * 1024);
        assert!(verify_flow.contains("flow-dot flow-dot-moving"));
        assert!(verify_flow.contains("flow-dot flow-dot-static"));
        assert!(verify_flow.contains("<animateMotion"));
        for label in ["Wallet address", "QED check", "Issuer registry", "Chain", "Certificate"] {
            assert!(verification.contains(label), "missing flow label: {label}");
        }

        let docs = app
            .clone()
            .oneshot(Request::get("/docs").body(Body::empty()).expect("docs page request"))
            .await
            .expect("docs page response");
        let docs = axum::body::to_bytes(docs.into_body(), usize::MAX).await.unwrap();
        let docs = String::from_utf8(docs.to_vec()).unwrap();
        assert!(!docs.contains("<svg class=\"guide-flow\""));

        let quick_start = app
            .oneshot(
                Request::get("/docs/api-quick-start")
                    .body(Body::empty())
                    .expect("API quick-start request"),
            )
            .await
            .expect("API quick-start response");
        let quick_start = axum::body::to_bytes(quick_start.into_body(), usize::MAX).await.unwrap();
        let quick_start = String::from_utf8(quick_start.to_vec()).unwrap();
        assert_eq!(quick_start.matches("curl -sS").count(), 3);
        assert!(quick_start.contains("<pre><code"));
        assert!(quick_start.contains("curl -sS \"$BASE/api/registry?ticker=NVDA\""));
        assert_eq!(quick_start.matches("aria-current=\"page\"").count(), 1);
    }

    #[tokio::test]
    async fn home_and_docs_expose_release_news_and_agent_quick_paths() {
        let app = router(AppState::for_tests(Vec::new(), Vec::new(), false));
        let entry = crate::adapters::content::changelog_entries()
            .into_iter()
            .next()
            .expect("a released changelog entry exists");
        let home = app
            .clone()
            .oneshot(Request::get("/").body(Body::empty()).expect("home request"))
            .await
            .expect("home response");
        let home = axum::body::to_bytes(home.into_body(), usize::MAX).await.unwrap();
        let home = String::from_utf8(home.to_vec()).unwrap();

        let docs = app
            .clone()
            .oneshot(Request::get("/docs").body(Body::empty()).expect("docs request"))
            .await
            .expect("docs response");
        let docs = axum::body::to_bytes(docs.into_body(), usize::MAX).await.unwrap();
        let docs = String::from_utf8(docs.to_vec()).unwrap();
        let llms = app
            .oneshot(Request::get("/llms.txt").body(Body::empty()).expect("LLM text request"))
            .await
            .expect("LLM text response");
        let llms = axum::body::to_bytes(llms.into_body(), usize::MAX).await.unwrap();
        let llms = String::from_utf8(llms.to_vec()).unwrap();
        assert!(llms.contains("Contact: contact@web3-energy.com"));
        assert!(llms.contains("- MCP endpoint: POST `http://localhost:3000/mcp`"));
        assert!(!llms.contains("[MCP endpoint]"));

        for body in [&home, &docs] {
            assert!(!body.contains("release-whats-new"));
            assert!(body.contains("class=\"docs-menu-release\""));
            assert!(body.contains(&format!("New in {}", entry.label)));
            assert!(body.contains(&entry.headline));
            assert!(body.contains("→ Changelog"));
            assert!(body.contains(&format!("/changelog#{}", entry.anchor)));
            assert!(!body.contains("Unreleased"));
            assert!(body.contains("For developers &amp; AI agents"));
            assert!(body.contains("href=\"/api\""));
            assert!(!body.contains("href=\"/mcp\""));
            assert!(body.contains("href=\"/docs/llm\""));
            assert!(body.contains("href=\"/llms.txt\""));
        }
        assert!(!home.contains("Latest post:"));
        assert!(!docs.contains("MCP endpoint: POST"));

        for question in [
            "Is this token what it claims to be, and what can its issuer do to it?",
            "Does this pool use the issuer's published contract?",
            "What can the issuer do to this token?",
            "What did these wallets hold, provably?",
            "Which contracts did each issuer publish?",
            "Is this QED record genuine and still fresh?",
        ] {
            assert!(docs.contains(question), "missing tool question: {question}");
        }
        for example in [
            "/guard/ethereum/0xc845b2894dBddd03858fd2D643B4eF725fE0849d",
            "/guard/robinhood/0x6444a8e0b267406a15db74ca00c4a24bdfa81ed3180f5b6d0851f8ed6f4f29c5",
            "/tokens/NVDA",
            "/statements",
            "/validated",
        ] {
            assert!(docs.contains(example), "missing example link: {example}");
        }
        for text in [
            "Three two-minute paths",
            "claude mcp add --transport http qed https://qed.web3-energy.com/mcp",
            "\"mcpServers\"",
            "Before I swap, check whether NVDA on Ethereum",
            "curl --request POST https://qed.web3-energy.com/api/guard",
            "Create a Statement",
            "Download the resulting JSON record.",
            "POST that JSON to <code>/verify</code>",
            "Pool:</strong> a contract where two tokens are traded",
            "Registry:</strong> the list of contract addresses each issuer has published",
        ] {
            assert!(docs.contains(text), "missing docs quick path: {text}");
        }
    }

    #[tokio::test]
    async fn stats_page_shows_four_overview_numbers_and_collapsed_details() {
        let app = router(AppState::for_tests(Vec::new(), Vec::new(), false));
        let api = app
            .clone()
            .oneshot(Request::get("/api/stats").body(Body::empty()).expect("stats API request"))
            .await
            .expect("stats API response");
        let api = axum::body::to_bytes(api.into_body(), usize::MAX).await.unwrap();
        let api: serde_json::Value = serde_json::from_slice(&api).unwrap();
        let headline = api["stats"]["headline"].as_str().expect("API headline");
        assert!(!headline.is_empty());

        let response = app
            .oneshot(Request::get("/stats").body(Body::empty()).expect("stats request"))
            .await
            .expect("stats response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(body.contains("href=\"/copies\""));
        assert_eq!(body.matches("<article class=\"stats-number-card").count(), 4);
        assert!(body.contains("0 of 0"));
        assert!(
            body.contains("aria-label=\"Deny — claims a product its publisher does not list\"")
        );
        assert!(body.contains(
            "<p class=\"stats-number\">0</p><p>Pools not checked: 0 not read yet · 0 unsupported venues</p>"
        ));
        assert!(body.contains("<details class=\"stats-details\">"));
        assert!(!body.contains("<details class=\"stats-details\" open>"));
        assert!(body.contains("Method, counts and downloads"));
        assert!(!body.contains(headline));
    }

    #[tokio::test]
    async fn blog_route_returns_human_dates_and_htmx_page_fragments() {
        let app = router(AppState::for_tests(Vec::new(), Vec::new(), false));
        let full = app
            .clone()
            .oneshot(Request::get("/blog").body(Body::empty()).expect("blog request"))
            .await
            .expect("blog response");
        assert_eq!(full.status(), StatusCode::OK);
        assert_eq!(
            full.headers().get(header::VARY).expect("blog response Vary header"),
            "HX-Request, HX-History-Restore-Request"
        );
        let full = axum::body::to_bytes(full.into_body(), usize::MAX).await.unwrap();
        let full = String::from_utf8(full.to_vec()).unwrap();
        assert!(full.contains(
            "<article class=\"blog-listing\"><a href=\"/blog/three-ways-to-protect-funds\"><time class=\"blog-date\" datetime=\"2026-10-06\" title=\"2026-10-06\">6 Oct 2026</time>"
        ));
        assert!(full.contains("Three questions to answer before you touch a token"));
        assert!(!full.contains("What QED Powers shows"));
        assert!(!full.contains("What a QED Statement is"));

        let fragment = app
            .clone()
            .oneshot(
                Request::get("/blog?page=1")
                    .header("HX-Request", "true")
                    .body(Body::empty())
                    .expect("blog fragment request"),
            )
            .await
            .expect("blog fragment response");
        assert_eq!(fragment.status(), StatusCode::OK);
        assert_eq!(
            fragment.headers().get(header::VARY).expect("fragment Vary header"),
            "HX-Request, HX-History-Restore-Request"
        );
        let fragment = axum::body::to_bytes(fragment.into_body(), usize::MAX).await.unwrap();
        let fragment = String::from_utf8(fragment.to_vec()).unwrap();
        assert!(!fragment.contains("<html"));
        assert!(fragment.contains("Is it the real NVDA? 3 checks in 30 seconds"));
        let restored = app
            .clone()
            .oneshot(
                Request::get("/blog?page=1")
                    .header("HX-Request", "true")
                    .header("HX-History-Restore-Request", "true")
                    .body(Body::empty())
                    .expect("history restore request"),
            )
            .await
            .expect("history restore response");
        assert_eq!(restored.status(), StatusCode::OK);
        let restored = axum::body::to_bytes(restored.into_body(), usize::MAX).await.unwrap();
        assert!(String::from_utf8(restored.to_vec()).unwrap().contains("<html"));

        let out_of_range = app
            .oneshot(
                Request::get("/blog?page=2")
                    .body(Body::empty())
                    .expect("out-of-range blog page request"),
            )
            .await
            .expect("out-of-range blog page response");
        assert_eq!(out_of_range.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn blog_paging_keeps_plain_links_and_returns_the_next_six_posts() {
        let posts = (0..7)
            .map(|index| {
                let date = chrono::NaiveDate::from_ymd_opt(2026, 10, 8 - index).unwrap();
                let file_name = format!("{date}-blog-item-{index}.md");
                let source = format!(
                    "---\ntitle: Blog item {index}\ndate: {date}\nsummary: Summary for item {index}.\ndraft: false\n---\n\nBody for item {index}."
                );
                crate::adapters::content::parse_blog_post(&file_name, &source).unwrap()
            })
            .collect::<Vec<_>>();
        let state = AppState::for_tests(Vec::new(), Vec::new(), false);
        let first = pages::render_blog_page(&state, &posts, None, false).unwrap().0;
        for index in 0..6 {
            assert!(first.contains(&format!("Blog item {index}")));
        }
        assert!(!first.contains("Blog item 6"));
        assert!(first.contains("hx-get=\"/blog?page=2\""));
        assert!(first.contains("href=\"/blog?page=2\" hx-boost=\"false\">Next posts</a>"));
        assert!(first.contains("hx-push-url=\"false\""));

        let second_fragment = pages::render_blog_page(&state, &posts, Some(2), true).unwrap().0;
        assert!(second_fragment.contains("Blog item 6"));
        assert!(!second_fragment.contains("Blog item 0"));
        assert!(!second_fragment.contains("hx-get="));
        assert!(!second_fragment.contains("<html"));

        let second_page = pages::render_blog_page(&state, &posts, Some(2), false).unwrap().0;
        assert!(second_page.contains("Blog item 6"));
        assert!(second_page.contains("Previous posts"));
        assert!(!second_page.contains("Next posts"));
    }

    #[tokio::test]
    async fn html_timestamps_show_utc_human_text_and_keep_full_iso_metadata() {
        let raw = "2026-10-08T02:52:27Z";
        assert_eq!(
            format_html_timestamps(&format!("<time datetime=\"{raw}\">{raw}</time>")),
            format!("<time datetime=\"{raw}\" title=\"{raw}\">8 Oct 2026, 02:52 UTC</time>")
        );
        let response = router(AppState::for_tests(Vec::new(), Vec::new(), false))
            .oneshot(Request::get("/changelog").body(Body::empty()).expect("changelog request"))
            .await
            .expect("changelog response");
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(body.contains(
            "<time class=\"release-date\" datetime=\"2026-10-08\" title=\"2026-10-08\">8 Oct 2026</time>"
        ));
        assert!(!body.contains(">2026-10-08</time>"));
    }

    #[tokio::test]
    async fn api_reference_lists_every_openapi_path() {
        let app = router(AppState::for_tests(Vec::new(), Vec::new(), false));
        let specification = app
            .clone()
            .oneshot(Request::get("/openapi.json").body(Body::empty()).expect("OpenAPI request"))
            .await
            .expect("OpenAPI response");
        let specification =
            axum::body::to_bytes(specification.into_body(), usize::MAX).await.unwrap();
        let specification: serde_json::Value =
            serde_json::from_slice(&specification).expect("OpenAPI JSON");
        let paths = specification["paths"].as_object().expect("OpenAPI paths");

        let response = app
            .oneshot(Request::get("/api").body(Body::empty()).expect("API reference request"))
            .await
            .expect("API reference response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(body.to_vec()).expect("API reference HTML");
        for path in paths.keys() {
            assert!(body.contains(path), "API reference is missing {path}");
        }
        assert!(body.contains("method-get"));
        assert!(body.contains("method-post"));
        assert!(body.contains("Parameters"));
        assert!(body.contains("Request</span>"));
        assert!(body.contains("Response · <span class=\"api-response-status"));
        assert!(body.contains("<pre class=\"code-block\"><code>"));
        assert!(body.contains("class=\"json-key\""));
        assert!(body.contains("class=\"json-string\""));
        assert!(body.contains("class=\"json-value\""));
        assert!(body.contains("class=\"json-ellipsis\">…</span>"));
        assert!(body.contains("\n  <span class=\"json-key\">"));
        assert!(body.contains("<a href=\"/openapi.json\">OpenAPI JSON</a>"));
        assert!(body.contains("<a href=\"/openapi.json\" download>Download</a>"));
        assert!(!body.contains("<a href=\"/openapi.json\">OpenAPI</a>"));
    }

    #[tokio::test]
    async fn statement_form_uses_api_rate_limit_and_expensive_permit() {
        use std::sync::Arc;
        use tokio::sync::{Notify, Semaphore};

        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let mut state = AppState::for_tests(
            Vec::new(),
            vec![Box::new(BlockingStatementReader {
                started: Arc::clone(&started),
                release: Arc::clone(&release),
            })],
            false,
        );
        state.expensive_concurrency = Arc::new(Semaphore::new(1));
        let app = router(state.clone());
        let first_app = app.clone();
        let first =
            tokio::spawn(async move { first_app.oneshot(statement_form_request()).await.unwrap() });
        tokio::time::timeout(std::time::Duration::from_secs(2), started.notified())
            .await
            .expect("form enters shared statement reader");
        let concurrent =
            app.clone().oneshot(statement_request()).await.expect("concurrent API response");
        assert_eq!(concurrent.status(), StatusCode::TOO_MANY_REQUESTS);
        release.notify_one();
        let first = tokio::time::timeout(std::time::Duration::from_secs(2), first)
            .await
            .expect("form completes")
            .expect("form task");
        assert_eq!(first.status(), StatusCode::SEE_OTHER);
        let location = first.headers().get(header::LOCATION).unwrap().to_str().unwrap().to_owned();
        assert!(location.starts_with("/statements/"));

        let ip = IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED);
        for _ in 0..58 {
            assert!(state.rate_limiter.allow(ip));
        }
        let rate_limited =
            app.oneshot(statement_form_request()).await.expect("rate-limited form response");
        assert_eq!(rate_limited.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn guard_requires_an_explicit_chain_and_api_bad_requests_are_readable() {
        let app = router(AppState::for_tests(Vec::new(), Vec::new(), false));
        let guard_page = app
            .clone()
            .oneshot(Request::get("/guard").body(Body::empty()).unwrap())
            .await
            .expect("Guard page response");
        assert_eq!(guard_page.status(), StatusCode::OK);
        let guard_page = axum::body::to_bytes(guard_page.into_body(), usize::MAX).await.unwrap();
        let guard_page = String::from_utf8(guard_page.to_vec()).unwrap();
        assert!(
            guard_page.contains(r#"<option value="" selected disabled>Select a chain</option>"#)
        );
        assert!(!guard_page.contains(r#"value="solana" selected"#));

        let guard_error = app
            .clone()
            .oneshot(
                Request::get("/api/guard/0x0000000000000000000000000000000000000001")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("Guard API response");
        assert_eq!(guard_error.status(), StatusCode::BAD_REQUEST);
        assert!(
            guard_error
                .headers()
                .get(header::CONTENT_TYPE)
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("application/json")
        );
        let guard_error = axum::body::to_bytes(guard_error.into_body(), usize::MAX).await.unwrap();
        let guard_error: serde_json::Value = serde_json::from_slice(&guard_error).unwrap();
        assert_eq!(guard_error["error"], "Bad request");
        assert!(guard_error["detail"].as_str().is_some_and(|detail| !detail.trim().is_empty()));

        let statement_error = app
            .oneshot(
                Request::post("/api/statement")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"wallets":[],"chains":[]}"#))
                    .unwrap(),
            )
            .await
            .expect("Statement API response");
        assert_eq!(statement_error.status(), StatusCode::BAD_REQUEST);
        let statement_error =
            axum::body::to_bytes(statement_error.into_body(), usize::MAX).await.unwrap();
        let statement_error: serde_json::Value = serde_json::from_slice(&statement_error).unwrap();
        assert_eq!(statement_error["error"], "Bad request");
        assert!(
            statement_error["detail"]
                .as_str()
                .is_some_and(|detail| detail.contains("wallet addresses"))
        );
    }
    #[tokio::test]
    async fn export_and_recheck_routes_share_the_expensive_permit() {
        let mut state = AppState::for_tests(Vec::new(), Vec::new(), false);
        state.expensive_concurrency = std::sync::Arc::new(tokio::sync::Semaphore::new(0));
        let app = router(state);
        let requests = [
            Request::get("/stats").body(Body::empty()).unwrap(),
            Request::get("/stats.json").body(Body::empty()).unwrap(),
            Request::get("/stats.csv").body(Body::empty()).unwrap(),
            Request::get("/api/stats").body(Body::empty()).unwrap(),
            Request::get("/api/statement/missing").body(Body::empty()).unwrap(),
            Request::get("/statements/missing/download.csv").body(Body::empty()).unwrap(),
            Request::get("/statements/missing/verify").body(Body::empty()).unwrap(),
            Request::post("/statements/missing/recheck").body(Body::empty()).unwrap(),
        ];
        for request in requests {
            let response = app.clone().oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        }
    }
}
