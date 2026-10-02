use crate::{state::AppState, web::views};
use axum::{
    extract::State,
    http::header,
    response::{IntoResponse, Response},
};

const LLMS_FULL: &str = include_str!("../../release/llms-full.md");

fn text_response(content_type: &'static str, body: String) -> Response {
    ([(header::CONTENT_TYPE, content_type)], body).into_response()
}

pub(crate) async fn llms(State(state): State<AppState>) -> Response {
    let base = state.public_url.as_str();
    text_response(
        "text/plain; charset=utf-8",
        format!(
            "# QED\n\nQED publishes signed, re-checkable contract-match verdicts for stock-paired liquidity pools. It checks whether a pool uses the stock-token contract published by its issuer, comparing the observed quote contract with issuer registry entries. It does not prove backing, custody, reserves, solvency, safety, price, liquidity, or endorsement.\n\n## Docs\n- [Full machine-readable guide]({base}/llms-full.txt): product, verdicts, trust model, limits, and API examples.\n- [OpenAPI 3.1]({base}/openapi.json): API schemas and routes.\n- [API guide]({base}/api): short curl examples.\n- [MCP endpoint]({base}/mcp): supports versions `2026-07-28`, `2025-11-25`, `2025-06-18`, and `2025-03-26` over stateless Streamable HTTP, with `qed_check`, `qed_powers`, `qed_wallet`, `qed_registry_lookup`, and `qed_verify`.\n- [MCP server card]({base}/.well-known/mcp/server-card.json): read-only endpoint and tool summary.\n- [Token powers API]({base}/api/powers/{{address}}): observed control signals; optional `?chain=...` filters cross-chain registry matches, which otherwise return as an array. EVM proxies report implementation and proxy source status separately.\n- [Issuer registry]({base}/registry): contracts QED compares against.\n- [Validated directory]({base}/validated): current contract-match pages.\n- [Token directory]({base}/tokens/NVDA): contracts and current pools for a ticker.\n- [Chain directory]({base}/chains/solana): contracts and current pools by chain.\n- [Glossary]({base}/glossary): plain-language definitions.\n- [Verification guide]({base}/guide/verify-a-stock-token): contract-first manual checks.\n- [Privacy]({base}/privacy): request and retention details."
        ),
    )
}

pub(crate) async fn llms_full(State(state): State<AppState>) -> Response {
    text_response(
        "text/plain; charset=utf-8",
        LLMS_FULL.replace("https://qed.example", state.public_url.as_str()),
    )
}

pub(crate) async fn api_docs(State(state): State<AppState>) -> Response {
    let base = state.public_url.as_str();
    let html = format!(
        "<!doctype html><html lang=\"en\" data-theme=\"light\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><title>QED API | API guide</title><meta name=\"description\" content=\"QED API routes and curl examples for checks, attestations, registry data, wallet holdings, and verification.\"><link rel=\"canonical\" href=\"{base}/api\"><link rel=\"stylesheet\" href=\"/static/style.css\"><script src=\"/static/app.js\" defer></script></head><body><header class=\"site-header page-shell\"><a class=\"site-brand\" href=\"/\" aria-label=\"QED home\"><img src=\"/static/logo.svg\" alt=\"QED\" width=\"68\" height=\"30\"></a><nav class=\"site-nav\" aria-label=\"Main navigation\"><a href=\"/\">Home</a><a href=\"/check\">Check</a><a href=\"/registry\">Registry</a><a href=\"/validated\">Validated</a></nav><div class=\"header-actions\"><a class=\"wallet-nav\" href=\"/wallet\">Check holdings</a><button id=\"theme-toggle\" class=\"theme-toggle\" type=\"button\" aria-label=\"Switch colour theme\" title=\"Switch colour theme\"><svg class=\"theme-sun\" aria-hidden=\"true\"><use href=\"/static/icons.svg#sun\"></use></svg><svg class=\"theme-moon\" aria-hidden=\"true\"><use href=\"/static/icons.svg#moon\"></use></svg></button></div></header><main class=\"page-shell page-content\"><h1 class=\"page-title\">QED API</h1><p>Read-only routes for checks, signed attestations, registry data, wallet holdings, and verification metadata.</p><p><a href=\"/openapi.json\">OpenAPI 3.1 JSON</a> <a href=\"/llms-full.txt\">Full guide</a></p><h2>Examples</h2><ul><li><code>curl {base}/healthz</code></li><li><code>curl {base}/api/registry</code></li><li><code>curl '{base}/api/check/POOL_OR_TOKEN_ADDRESS'</code></li><li><code>curl -X POST {base}/api/wallet -H 'content-type: application/json' -d @wallet.json</code></li><li><code>curl {base}/api/attest/ATTESTATION_ID</code></li><li><code>curl {base}/.well-known/qed.json</code></li><li><code>curl {base}/openapi.json</code></li></ul><h2>Routes</h2><dl><dt>GET /api/registry</dt><dd>Issuer registry JSON.</dd><dt>GET /api/pools/featured</dt><dd>Curated pools.</dd><dt>GET /api/leaderboard</dt><dd>Ranked pool page.</dd><dt>GET /api/prices</dt><dd>Price snapshot for selected pools.</dd><dt>GET /api/status</dt><dd>Freshness and service state.</dd><dt>GET /api/check/{{address}}</dt><dd>Pool or token check.</dd><dt>POST /api/wallet</dt><dd>Stock-token holdings from a JSON request body; the address is not part of the URL.</dd><dt>GET /api/attest/{{id}}</dt><dd>Signed attestation JSON.</dd><dt>POST /verify</dt><dd>Cryptographic, signer, environment, and freshness verification.</dd></dl><p>See <a href=\"/imprint\">Imprint</a>, <a href=\"/privacy\">Privacy</a>, and <a href=\"/terms\">Terms</a>.</p></main><footer class=\"site-footer page-shell\"><nav aria-label=\"Legal and project links\"><a href=\"/tokens/NVDA\">Token directory</a><a href=\"/chains/solana\">Chain directory</a><a href=\"/glossary\">Glossary</a><a href=\"/guide/verify-a-stock-token\">Verification guide</a><a href=\"/imprint\">Imprint</a><a href=\"/privacy\">Privacy</a><a href=\"/terms\">Terms</a></nav><a class=\"repository-link\" href=\"https://github.com/boev/qed\" target=\"_blank\" rel=\"noopener noreferrer\" aria-label=\"QED source code on GitHub\"><svg viewBox=\"0 0 24 24\" aria-hidden=\"true\" focusable=\"false\"><path fill=\"currentColor\" d=\"M12 .5a12 12 0 0 0-3.79 23.39c.6.11.82-.26.82-.58v-2.26c-3.34.73-4.04-1.61-4.04-1.61-.55-1.39-1.34-1.76-1.34-1.76-1.09-.75.08-.74.08-.74 1.2.08 1.83 1.24 1.83 1.24 1.07 1.83 2.8 1.3 3.49 1 .11-.78.42-1.3.76-1.6-2.67-.3-5.47-1.34-5.47-5.93 0-1.31.47-2.38 1.24-3.22-.12-.3-.54-1.52.12-3.17 0 0 1.01-.32 3.3 1.23a11.45 11.45 0 0 1 6 0c2.29-1.55 3.3-1.23 3.3-1.23.66 1.65.24 2.87.12 3.17.77.84 1.24 1.91 1.24 3.22 0 4.6-2.8 5.62-5.48 5.92.43.37.81 1.1.81 2.22v3.29c0 .32.22.69.83.58A12 12 0 0 0 12 .5\"/></svg></a></footer></body></html>"
    );
    let mcp_section = format!(
        "<h2>MCP for AI agents</h2><p><code>POST /mcp</code> supports MCP versions <code>2026-07-28</code>, <code>2025-11-25</code>, <code>2025-06-18</code>, and <code>2025-03-26</code> over stateless Streamable HTTP, exposing read-only tools <code>qed_check</code>, <code>qed_powers</code>, <code>qed_wallet</code>, <code>qed_registry_lookup</code>, and <code>qed_verify</code>.</p><p><code>GET /api/powers/{{address}}</code> returns a record for a single active issuer-registry match or an array when the same address is registered on multiple chains; optional <code>?chain=solana</code> filters the lookup. Solana <code>source_verified</code> covers the Token-2022 token-program build. For EVM proxies, source verification targets the resolved implementation and reports the proxy's source status separately. These source matches do not imply backing or endorsement. <a href=\"{base}/.well-known/mcp/server-card.json\">Read the MCP server card</a>; the registry manifest is in <code>server.json</code>.</p><p>Connect Claude Code with <code>claude mcp add --transport http qed {base}/mcp</code>.</p>"
    );
    let html = html.replace("</main>", &format!("{mcp_section}</main>"));
    text_response("text/html; charset=utf-8", html)
}

pub(crate) async fn validated_feed(State(state): State<AppState>) -> Response {
    let items = views::verified_attestations(&state)
        .into_iter()
        .map(|attestation| {
            let chain = views::chain_slug(attestation.chain);
            let base = attestation.pool.base.symbol.as_deref().unwrap_or("pool");
            let quote = attestation.pool.quote.symbol.as_deref().unwrap_or("quote");
            let title = format!("{base}/{quote} on {}: contract match", attestation.pool.dex);
            let link = format!("{}/validated/{chain}/{}", state.public_url, attestation.subject);
            format!(
                "<item><title>{}</title><link>{}</link><guid isPermaLink=\"true\">{}</guid><pubDate>{}</pubDate></item>",
                xml_escape(&title),
                xml_escape(&link),
                xml_escape(&link),
                xml_escape(&attestation.checked_at),
            )
        })
        .collect::<Vec<_>>()
        .join("");
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><rss version=\"2.0\"><channel><title>QED verified pools</title><link>{}</link><description>QED contract-match attestations.</description>{items}</channel></rss>",
        xml_escape(&format!("{}/validated", state.public_url)),
    );
    text_response("application/rss+xml; charset=utf-8", body)
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
