use crate::{chain::Chain, registry, state::{AdminAuth, AppState}};
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
mod discoverability;
mod legal;
mod mcp;
mod openapi;
mod pages;
mod views;
pub(crate) use api::{
    admin_stats, api_attestation, api_check, api_featured, api_leaderboard, api_powers, api_prices,
    api_registry, api_status, api_wallet, healthz, verify_attestation, well_known,
};
pub(crate) use discoverability::{api_docs, llms, llms_full, validated_feed};
pub(crate) use legal::{imprint, privacy, terms};
pub(crate) use pages::{
    certificate, chain_page, check_form, check_page, featured, glossary_page, guide_verify_page,
    index, recheck_certificate, registry_page, registry_table, token_lookup, token_page, validated,
    validated_detail, wallet_holdings_page, wallet_page,
};

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
        .route("/check", get(check_page).post(check_form))
        .route("/registry", get(registry_page))
        .route("/registry/table", get(registry_table))
        .route("/tokens", get(token_lookup))
        .route("/tokens/{ticker}", get(token_page))
        .route("/chains/{chain_name}", get(chain_page))
        .route("/wallet", get(wallet_page).post(wallet_holdings_page))
        .route("/glossary", get(glossary_page))
        .route("/guide/verify-a-stock-token", get(guide_verify_page))
        .route("/validated", get(validated))
        .route("/validated/{chain}/{subject}", get(validated_detail))
        .route("/imprint", get(imprint))
        .route("/privacy", get(privacy))
        .route("/terms", get(terms))
        .route("/robots.txt", get(robots))
        .route("/sitemap.xml", get(sitemap))
        .route("/validated.xml", get(validated_feed))
        .route("/llms.txt", get(llms))
        .route("/llms-full.txt", get(llms_full))
        .route("/api", get(api_docs))
        .route("/.well-known/mcp/server-card.json", get(mcp::server_card))
        .route("/mcp", post(mcp::handle))
        .route("/openapi.json", get(openapi::document))
        .route("/pools/featured", get(featured))
        .route("/healthz", get(healthz))
        .route("/admin/stats", get(admin_stats))
        .route("/api/registry", get(api_registry))
        .route("/api/pools/featured", get(api_featured))
        .route("/api/leaderboard", get(api_leaderboard))
        .route("/api/prices", get(api_prices))
        .route("/api/status", get(api_status))
        .route("/api/check/{address}", get(api_check))
        .route("/api/powers/{address}", get(api_powers))
        .route("/api/wallet", post(api_wallet))
        .route("/api/attest/{id}", get(api_attestation))
        .route("/verify", post(verify_attestation))
        .route("/v/{id}", get(certificate))
        .route("/v/{id}/recheck", post(recheck_certificate))
        .route("/.well-known/qed.json", get(well_known))
        .nest_service("/static", ServeDir::new("static"))
        .with_state(state.clone())
        .layer(CompressionLayer::new())
        .layer(middleware::from_fn(cache_static))
        .layer(middleware::from_fn(security_headers))
        .layer(middleware::from_fn_with_state(state.clone(), admin_auth))
        .layer(middleware::from_fn_with_state(state.clone(), rate_limit))
        .layer(middleware::from_fn_with_state(state, usage_metrics))
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
    if encoded.len() < 6
        || !encoded[..5].eq_ignore_ascii_case(b"Basic")
        || encoded[5] != b' '
    {
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
        || (method == Method::POST && (path == "/check" || path == "/verify"))
        || (method == Method::POST && path.starts_with("/v/") && path.ends_with("/recheck"));
    let is_wallet = path == "/wallet" || path == "/api/wallet";
    let is_health = path == "/healthz";
    let is_static_asset = path == "/static" || path.starts_with("/static/");
    state
        .usage_stats
        .record_request(is_api, is_check, is_wallet, is_admin, is_health, is_static_asset);

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
    let is_expensive = (request.method() == Method::POST && path == "/mcp")
        || (request.method() == Method::POST && path == "/check")
        || path.starts_with("/api/check/")
        || path.starts_with("/api/powers/")
        || path == "/verify"
        || (request.method() == Method::GET
            && (path.starts_with("/validated/") || path.starts_with("/v/")))
        || (request.method() == Method::POST
            && path.starts_with("/v/")
            && path.ends_with("/recheck"));
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
async fn sitemap(State(state): State<AppState>) -> impl IntoResponse {
    let now = chrono::Utc::now().date_naive().to_string();
    let mut urls = vec![
        ("/".to_owned(), now.clone()),
        ("/check".to_owned(), now.clone()),
        ("/registry".to_owned(), now.clone()),
        ("/validated".to_owned(), now.clone()),
        ("/glossary".to_owned(), now.clone()),
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
    for attestation in views::verified_attestations(&state) {
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
        assert_eq!(unauthorized.headers().get(header::WWW_AUTHENTICATE).unwrap(), "Basic realm=\"qed-admin\"");
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
}
