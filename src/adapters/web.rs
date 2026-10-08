use crate::{
    adapters::state::{AdminAuth, AppState},
    domain::{chain::Chain, registry},
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
mod docs;
mod mcp;
mod pages;
mod views;
pub(crate) use api::{
    admin_stats, api_attestation, api_check, api_featured, api_guard, api_guard_post,
    api_leaderboard, api_powers, api_prices, api_registry, api_statement, api_statement_get,
    api_stats, api_status, api_wallet, healthz, refresh_stats_snapshot, stats_snapshot_csv,
    stats_snapshot_json, verify_attestation, well_known,
};
pub(crate) use docs::{api_docs, imprint, llms, llms_full, privacy, terms, validated_feed};
pub(crate) use pages::{
    certificate, chain_page, featured, glossary_page, guide_verify_page, index,
    recheck_certificate, registry_page, registry_table, stats_page, token_lookup, token_page,
    validated, validated_detail, wallet_holdings_page, wallet_page,
};
pub(crate) fn wallet_error_status(error: crate::app::wallet::WalletError) -> StatusCode {
    match error {
        crate::app::wallet::WalletError::InvalidAddress => StatusCode::NOT_FOUND,
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
    let hash = hash_bytes(hash, include_bytes!("../../static/wallet.js"));
    let hash = hash_bytes(hash, include_bytes!("../../static/leaderboard.js"));
    hash_bytes(hash, include_bytes!("../../static/vue.global.prod.js"))
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
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
        .route("/chains/{chain_name}", get(chain_page))
        .route("/wallet", get(wallet_page).post(wallet_holdings_page))
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
    let is_wallet = path == "/wallet" || path == "/api/wallet";
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
    let is_wallet =
        request.method() == Method::POST && (path == "/wallet" || path == "/api/wallet");
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
            && (path.starts_with("/validated/") || path.starts_with("/v/")))
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
        || is_registry_api;
    if is_wallet || is_expensive || is_read_limited {
        let ip = trusted_client_ip(&request);
        if !state.rate_limiter.allow(ip) {
            return (StatusCode::TOO_MANY_REQUESTS, "Rate limit exceeded. Try again later.\n")
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
            return next.run(request).await;
        }
        if is_registry_api {
            let Ok(_permit) = state.registry_api_concurrency.clone().try_acquire_owned() else {
                return (
                    StatusCode::TOO_MANY_REQUESTS,
                    "The registry endpoint is busy. Try again later.\n",
                )
                    .into_response();
            };
            return next.run(request).await;
        }
        if is_expensive {
            let Ok(_permit) = state.expensive_concurrency.clone().try_acquire_owned() else {
                return (
                    StatusCode::TOO_MANY_REQUESTS,
                    "Too many checks in progress. Try again later.\n",
                )
                    .into_response();
            };
            return next.run(request).await;
        }
    }
    next.run(request).await
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
    headers.get("HX-Request").is_some_and(|value| value.as_bytes().eq_ignore_ascii_case(b"true"))
}

fn render_page<T: askama::Template>(
    template: T,
    _fragment: bool,
) -> Result<axum::response::Html<String>, StatusCode> {
    let page = template.render().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(axum::response::Html(page))
}

async fn robots(State(state): State<AppState>) -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        format!(
            "User-agent: *\nAllow: /\nDisallow: /v/*/recheck\nDisallow: /verify\n\nUser-agent: GPTBot\nAllow: /\nDisallow: /v/*/recheck\nDisallow: /verify\n\nUser-agent: ClaudeBot\nAllow: /\nDisallow: /v/*/recheck\nDisallow: /verify\n\nUser-agent: PerplexityBot\nAllow: /\nDisallow: /v/*/recheck\nDisallow: /verify\n\nUser-agent: Google-Extended\nAllow: /\nDisallow: /v/*/recheck\nDisallow: /verify\n\nUser-agent: CCBot\nAllow: /\nDisallow: /v/*/recheck\nDisallow: /verify\n\nSitemap: {}/sitemap.xml\n",
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
    for attestation in views::verified_attestations(&state.app).await {
        urls.push(format!(
            "<url><loc>{}/validated/{}/{}</loc><lastmod>{}</lastmod></url>",
            xml_escape(&state.public_url),
            views::chain_slug(attestation.chain),
            xml_escape(&attestation.subject),
            xml_escape(&attestation.checked_at),
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
            assert_eq!(nav.matches("<a ").count(), 7, "{path}");
            let mut previous = 0;
            for label in ["Guard", "Registry", "Statement", "Docs", "API", "MCP", "llms.txt"] {
                let position = nav.find(&format!(">{label}</a>")).expect("navigation label");
                assert!(position >= previous, "{path}: {label}");
                previous = position;
            }
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
            if path == "/statements" {
                assert!(body.contains(
                    "<h1 class=\"page-title\">What did these wallets hold, provably?</h1>"
                ));
                assert!(body.contains(
                    "A signed, re-checkable record of what a wallet set holds in registry tokens at a block height — for audits, reporting and counterparties."
                ));
                assert!(body.contains(
                    "<form class=\"statement-form\" action=\"/statements\" method=\"post\""
                ));
                assert!(body.contains("<textarea id=\"statement-wallets\""));
                assert!(body.contains("statement-chain-option"));
                assert!(body.contains("statement-block"));
                assert!(body.contains("class=\"button primary-button\""));
                assert!(body.contains("Sign statement"));
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
        assert!(home.contains(
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
        assert!(!sitemap.contains("/blog/what-qed-powers-shows"));
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
                let nav = body
                    .split_once("<nav class=\"site-nav\"")
                    .expect("API site navigation")
                    .1
                    .split("</nav>")
                    .next()
                    .expect("API navigation close");
                assert_eq!(nav.matches("<a ").count(), 7);
                let mut previous = 0;
                for label in ["Guard", "Registry", "Statement", "Docs", "API", "MCP", "llms.txt"] {
                    let position =
                        nav.find(&format!(">{label}</a>")).expect("API navigation label");
                    assert!(position >= previous);
                    previous = position;
                }
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
        let entry = crate::adapters::content::latest_released_changelog_entry()
            .expect("changelog is readable")
            .expect("a released changelog entry exists");
        let expected_news = format!("New in {}: {}", entry.label, entry.headline);
        let posts = crate::adapters::content::blog_posts().expect("blog posts are readable");
        let latest_blog = crate::adapters::content::latest_published_blog_post(&posts);

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
        assert!(llms.contains("- MCP endpoint: POST `http://localhost:3000/mcp`"));
        assert!(!llms.contains("[MCP endpoint]"));

        for body in [&home, &docs] {
            let banner_start =
                body.find("<p class=\"release-whats-new\">").expect("release banner");
            let banner_end =
                body[banner_start..].find("</p>").expect("release banner end") + banner_start;
            let banner = &body[banner_start..banner_end];
            assert!(banner.contains(&expected_news), "expected {expected_news:?} in {banner:?}");
            assert!(!banner.contains("Navigation"));
            assert!(!banner.contains("Release notes"));
            assert!(body.contains(&format!("/changelog#{}", entry.anchor)));
            assert!(!body.contains("Unreleased"));
            match latest_blog {
                Some(post) => {
                    assert!(body.contains(&format!("href=\"/blog/{}\"", post.slug)));
                    assert!(body.contains(&post.title));
                }
                None => assert!(!body.contains("Latest post:")),
            }
            assert!(body.contains("For developers &amp; AI agents"));
            assert!(body.contains("href=\"/api\""));
            assert!(!body.contains("href=\"/mcp\""));
            assert!(body.contains("href=\"/docs/llm\""));
            assert!(body.contains("href=\"/llms.txt\""));
        }
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
