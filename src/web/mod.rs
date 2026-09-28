use crate::{chain::Chain, registry, state::AppState};
use axum::{
    Router,
    extract::State,
    http::{HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use std::net::IpAddr;
use tower_http::{compression::CompressionLayer, services::ServeDir};

mod api;
mod discoverability;
mod legal;
mod openapi;
mod pages;
mod views;

pub(crate) use api::{
    api_attestation, api_check, api_featured, api_leaderboard, api_prices, api_registry,
    api_status, api_wallet, healthz, verify_attestation, well_known,
};
pub(crate) use discoverability::{api_docs, llms, llms_full, validated_feed};
pub(crate) use legal::{imprint, privacy, terms};
pub(crate) use pages::{
    certificate, chain_page, check_form, check_page, featured, glossary_page, guide_verify_page,
    index, recheck_certificate, registry_page, registry_table, token_page, validated,
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
        .route("/openapi.json", get(openapi::document))
        .route("/pools/featured", get(featured))
        .route("/healthz", get(healthz))
        .route("/api/registry", get(api_registry))
        .route("/api/pools/featured", get(api_featured))
        .route("/api/leaderboard", get(api_leaderboard))
        .route("/api/prices", get(api_prices))
        .route("/api/status", get(api_status))
        .route("/api/check/{address}", get(api_check))
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
        .layer(middleware::from_fn_with_state(state, rate_limit))
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
    let is_expensive = (request.method() == Method::POST && path == "/check")
        || path.starts_with("/api/check/")
        || path == "/verify"
        || (request.method() == Method::GET
            && (path.starts_with("/validated/") || path.starts_with("/v/")))
        || (request.method() == Method::POST
            && path.starts_with("/v/")
            && path.ends_with("/recheck"));
    let is_read_limited =
        path.starts_with("/api/attest/") || path == "/.well-known/qed.json" || is_registry_api;
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
    request
        .headers()
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.rsplit(',').find_map(|part| part.trim().parse().ok()))
        .unwrap_or(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED))
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
