use crate::adapters::web::{ASSET_VERSION, render_page, wants_fragment};

mod discoverability {
    use crate::{
        adapters::web::views,
        adapters::{content, state::AppState},
    };
    use axum::{
        extract::State,
        http::{StatusCode, header},
        response::{IntoResponse, Response},
    };

    const LLMS_FULL: &str = include_str!("../../../release/llms-full.md");

    fn text_response(content_type: &'static str, body: String) -> Response {
        ([(header::CONTENT_TYPE, content_type)], body).into_response()
    }

    pub(crate) async fn llms(State(state): State<AppState>) -> Response {
        let base = state.public_url.as_str();
        let body = format!(
            r#"# QED

QED checks tokens that claim to be something against the contract their issuer publishes. Re-check certificates after expiry or when the issuer registry changes. It is read-only by design: it never holds keys, submits transactions, or recommends. It does not prove backing, custody, reserves, solvency, safety, price, liquidity, or endorsement.

## Docs
- [Documentation hub]({base}/docs): contract-first verification guide.
- [API quick start]({base}/docs/api-quick-start): three curl examples.
- [API reference]({base}/api): server-rendered routes, parameters, and examples.
- [OpenAPI 3.1]({base}/openapi.json): API schemas and routes.
- [Use QED with an LLM]({base}/docs/llm): MCP connection details and tool descriptions.
- [Leaderboard statistics]({base}/stats): leaderboard read counts, issuer-registry coverage, publisher-catalog observations, scan method/time, counts last seen within the last 7 days, and entries first seen this UTC week. Signed snapshots are available as JSON at `{base}/stats.json` and CSV at `{base}/stats.csv`.
- GET `{base}/api/stats?page=N`: paginated machine-readable leaderboard and registry totals, plus the signed snapshot's reducer inputs. Summary counts distinguish unsupported venues from checks that have not run; pages contain up to 50 rows per input.
- MCP endpoint: POST `{base}/mcp`; stateless Streamable HTTP with seven tools: `qed_check`, `qed_powers`, `qed_wallet`, `qed_statement`, `qed_registry_lookup`, `qed_verify`, and `qed_guard`. Use the [MCP guide]({base}/docs/llm) for setup.
- Claude Code connection: `claude mcp add --transport http qed {base}/mcp`; Claude Desktop and Cursor HTTP settings are in the [MCP guide]({base}/docs/llm).
- Statement records at `{base}/statements` support signed JSON and CSV downloads, a signature-verification page, browser Print/Save as PDF, and re-run with comparison; records are public to anyone with the link and retained for up to 24 hours.
- Chain values use matching lowercase slugs in route paths, JSON, and OpenAPI: `solana`, `robinhood`, `base`, `ethereum`, and `bnb`. Existing signed records with previous variant names still verify.
- [MCP server card]({base}/.well-known/mcp/server-card.json): endpoint and tool summary.
- [Guard review]({base}/guard): signed review form for supported-chain tokens and pools.
- [Guard API (GET)]({base}/api/guard/{{address}}?chain={{chain}}): signed issuer, powers, source, and known-pool review; pool identity requires the known-pool index or verified factory/derivation checks, and unindexed Solana accounts are reviewed as tokens. A wallet query may be exposed in browser history or request URLs.
- [Guard API (POST)]({base}/api/guard): same signed review with an optional wallet in a JSON request body, keeping it out of the URL.
- [Token powers API]({base}/api/powers/{{address}}): observed control signals for any supported-chain contract; optional `?chain=...` filters results.
- [Issuer registry]({base}/registry): contracts QED compares against.
- [Validated directory]({base}/validated): current contract-match pages.
- [Glossary]({base}/glossary): plain-language definitions.
- [Verification guide]({base}/guide/verify-a-stock-token): contract-first manual checks.

## Site pages
- [About]({base}/about): product scope and non-claims.
- [Security]({base}/security): reporting and disclosure policy.
- [Changelog]({base}/changelog) and [Atom feed]({base}/changelog.xml).
- [Blog]({base}/blog) and [Atom feed]({base}/blog.xml): published posts only.
- [Create a wallet statement]({base}/statements): signed point-in-time balances.
- [Privacy]({base}/privacy): request and retention details."#
        );
        text_response("text/plain; charset=utf-8", body)
    }
    pub(crate) async fn llms_full(State(state): State<AppState>) -> Response {
        text_response(
            "text/plain; charset=utf-8",
            LLMS_FULL.replace("https://qed.example", state.public_url.as_str()),
        )
    }
    pub(crate) async fn api_docs(
        State(state): State<AppState>,
    ) -> Result<axum::response::Html<String>, StatusCode> {
        let body_html = super::openapi::render_api_reference_html();
        super::super::pages::docs_content_page(
            &state,
            "API reference",
            "REFERENCE",
            "How can software check an issuer-published token contract?",
            "QED checks tokens that claim to be something against the contract their issuer publishes. This API guide documents the machine-readable checks, signed reviews, and verification routes.",
            "QED API operations, parameters, and JSON examples generated from the OpenAPI document.",
            "/api",
            "api",
            body_html,
        )
    }

    pub(crate) async fn validated_feed(State(state): State<AppState>) -> Response {
        let items = views::verified_attestations(&state.app).await
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
    pub(crate) async fn changelog_feed(State(state): State<AppState>) -> Response {
        text_response(
            "application/atom+xml; charset=utf-8",
            content::changelog_atom(state.public_url.as_str()),
        )
    }

    pub(crate) async fn blog_feed(State(state): State<AppState>) -> Result<Response, StatusCode> {
        let posts = content::blog_posts().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let posts = content::published_blog_posts(posts);
        Ok(text_response(
            "application/atom+xml; charset=utf-8",
            content::blog_atom(state.public_url.as_str(), &posts),
        ))
    }

    fn xml_escape(value: &str) -> String {
        value
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&apos;")
    }
}

mod legal {
    use super::{ASSET_VERSION, render_page, wants_fragment};
    use crate::adapters::state::AppState;
    use askama::Template;
    use axum::{
        extract::State,
        http::{HeaderMap, StatusCode},
        response::Html,
    };

    pub(crate) async fn imprint(
        State(state): State<AppState>,
        headers: HeaderMap,
    ) -> Result<Html<String>, StatusCode> {
        legal_page(
            &state,
            &headers,
            "Imprint",
            "/imprint",
            include_str!("../../../release/legal/imprint.md"),
        )
    }

    pub(crate) async fn privacy(
        State(state): State<AppState>,
        headers: HeaderMap,
    ) -> Result<Html<String>, StatusCode> {
        legal_page(
            &state,
            &headers,
            "Privacy",
            "/privacy",
            include_str!("../../../release/legal/privacy.md"),
        )
    }

    pub(crate) async fn terms(
        State(state): State<AppState>,
        headers: HeaderMap,
    ) -> Result<Html<String>, StatusCode> {
        legal_page(
            &state,
            &headers,
            "Terms",
            "/terms",
            include_str!("../../../release/legal/terms.md"),
        )
    }

    fn legal_page(
        state: &AppState,
        headers: &HeaderMap,
        title: &str,
        canonical_path: &str,
        source: &str,
    ) -> Result<Html<String>, StatusCode> {
        render_page(
            LegalTemplate {
                asset_version: ASSET_VERSION,
                public_url: state.public_url.to_string(),
                title: title.to_owned(),
                canonical_path: canonical_path.to_owned(),
                body: markdownish(source),
            },
            wants_fragment(headers),
        )
    }
    #[derive(Debug, Template)]
    #[template(path = "legal.html")]
    struct LegalTemplate {
        asset_version: u64,
        public_url: String,
        title: String,
        canonical_path: String,
        body: String,
    }

    fn markdownish(source: &str) -> String {
        source
            .lines()
            .map(|line| {
                if let Some(title) = line.strip_prefix("# ") {
                    format!("<h1 class=\"page-title\">{title}</h1>")
                } else if let Some(title) = line.strip_prefix("## ") {
                    format!("<h2>{title}</h2>")
                } else if line.is_empty() {
                    String::new()
                } else {
                    format!("<p>{line}</p>")
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

mod openapi {
    use axum::{
        http::header,
        response::{IntoResponse, Response},
    };
    use serde_json::{Map, Value, json};
    use std::{collections::BTreeMap, sync::LazyLock};
    const DOCUMENT: &str = r##"{
  "openapi": "3.1.0",
  "info": {
    "title": "QED verification API",
    "version": "1.0.0",
    "description": "QED checks tokens that claim to be something against the contract their issuer publishes. Read-only pool checks, signed attestations, registry data, token-power observations, and verification metadata."
  },
  "paths": {
    "/healthz": {
      "get": {
        "summary": "Health",
        "responses": {
          "200": {
            "description": "Service is healthy",
            "content": {
              "application/json": {
                "schema": {
                  "type": "object",
                  "required": ["status"],
                  "properties": { "status": { "const": "ok" } }
                },
                "example": { "status": "ok" }
              }
            }
          }
        }
      }
    },
    "/api/registry": {
      "get": {
        "summary": "Issuer registry",
        "responses": {
          "200": {
            "description": "Registry entries",
            "content": {
              "application/json": {
                "schema": { "type": "array", "items": { "type": "object" } },
                "example": [
                  {
                    "issuer": "Backed xStocks",
                    "ticker": "NVDA",
                    "name": "NVIDIA",
                    "chain": "bnb",
                    "contract": "0xc845b2894dBddd03858fd2D643B4eF725fE0849d",
                    "decimals": null,
                    "source": "xstocks-api",
                    "source_url": "https://api.xstocks.fi/api/v2/public/assets",
                    "last_checked": "2026-09-22T21:26:41Z"
                  },
                  {
                    "issuer": "Backed xStocks",
                    "ticker": "NVDA",
                    "name": "NVIDIA",
                    "chain": "ethereum",
                    "contract": "0xc845b2894dBddd03858fd2D643B4eF725fE0849d",
                    "decimals": null,
                    "source": "xstocks-api",
                    "source_url": "https://api.xstocks.fi/api/v2/public/assets",
                    "last_checked": "2026-09-22T21:26:41Z"
                  }
                ]
              }
            }
          }
        }
      }
    },
    "/api/pools/featured": {
      "get": {
        "summary": "Featured pools",
        "responses": {
          "200": {
            "description": "Curated pool list",
            "content": {
              "application/json": {
                "schema": { "type": "array", "items": { "type": "object" } },
                "example": [
                  {
                    "chain": "solana",
                    "dex": "raydium",
                    "pool": "featured-pool",
                    "base_symbol": "TSLA",
                    "base_address": "XsDoVfqeBukxuZHWhdvWHBhgEHjGNst4MLodqsJHzoB",
                    "quote_symbol": "USDC",
                    "quote_address": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
                    "issuer": "Backed xStocks",
                    "ticker": "TSLA",
                    "verdict": "verified",
                    "quote_balance": "1200000000",
                    "quote_share_of_supply": 0.2,
                    "volume_24h_usd": 125000.0,
                    "liquidity_usd": 450000.0,
                    "curated": false,
                    "note": null,
                    "updated_at": "2026-10-04T12:00:00Z"
                  }
                ]
              }
            }
          }
        }
      }
    },
    "/api/leaderboard": {
      "get": {
        "summary": "Leaderboard page",
        "parameters": [
          { "name": "page", "in": "query", "schema": { "type": "integer", "minimum": 1 } },
          { "name": "per", "in": "query", "schema": { "type": "integer", "minimum": 1, "maximum": 50 } },
          { "name": "sort", "in": "query", "schema": { "type": "string", "enum": ["volume", "price", "change", "liquidity"] } },
          { "name": "dir", "in": "query", "schema": { "type": "string", "enum": ["asc", "desc"] } }
        ],
        "responses": {
          "200": {
            "description": "Ranked leaderboard page",
            "content": {
              "application/json": {
                "schema": { "type": "object" },
                "example": {
                  "page": 1,
                  "per": 10,
                  "total": 1,
                  "updated_at": "2026-10-04T12:00:00Z",
                  "next_refresh_at": "2026-10-04T18:00:00Z",
                  "restored": true,
                  "refreshing": false,
                  "empty_successful": false,
                  "prices_updated_at": "2026-10-04T12:00:00Z",
                  "source": "DexScreener / GeckoTerminal + on-chain reads",
                  "registry": {
                    "entries": 2,
                    "issuers": 1,
                    "updated_at": "2026-10-04T12:00:00Z",
                    "next_refresh_at": "2026-10-04T13:00:00Z",
                    "restored": true,
                    "refreshing": false
                  },
                  "entries": [
                    {
                      "rank": 1,
                      "chain": "solana",
                      "chain_label": "Solana",
                      "dex": "raydium",
                      "pool": "leaderboard-pool",
                      "source": "dexscreener",
                      "base_symbol": "NVDA",
                      "quote_symbol": "USDC",
                      "issuer": "Backed xStocks",
                      "ticker": "NVDA",
                      "verdict": "verified",
                      "read_status": "checked",
                      "read_reason": null,
                      "price_usd": 132.5,
                      "change_24h_pct": 1.25,
                      "volume_24h_usd": 500000.0,
                      "liquidity_usd": 1250000.0,
                      "txns_24h": 120,
                      "detail_url": "/validated/solana/pool",
                      "trade_url": "https://dexscreener.com/solana/pool",
                      "explorer_url": "https://solscan.io/account/pool",
                      "attestation_id": null,
                      "checked_at": null
                    }
                  ]
                }
              }
            }
          }
        }
      }
    },
    "/stats.json": {
      "get": {
        "summary": "Download signed statistics snapshot",
        "responses": {
          "200": {
            "description": "Strict signed point-in-time statistics document",
            "content": {
              "application/json": {
                "schema": { "$ref": "#/components/schemas/StatsDocument" },
                "description": "Schematic document shape only; the illustrative ID and signature are not a verifiable signed snapshot.",
                "example": {
                  "id": "0000000000000000000000000000000000000000000000000000000000000000",
                  "kind": "stats",
                  "stats": {
                    "generated_at": "2026-10-07T00:00:00Z",
                    "headline": "QED checked 0 of 0 listed pools: 0 issuer matches, 0 mismatches, 0 unsupported venues, and 0 not read yet. 0 catalog observations are currently flagged (seen in the last 7 days); 0 were first seen this UTC week. 0 tokens on unsupported chains were seen but not judged. 0 supported observations and 0 unsupported candidates were evicted; 0 invalid supported and 0 invalid unsupported candidates were rejected.",
                    "listed_pools": 0,
                    "pools_checked": 0,
                    "issuer_matches": 0,
                    "mismatches": 0,
                    "unsupported_venue": 0,
                    "not_read_yet": { "count": 0, "rpc_limit": 0, "transient": 0, "unsupported": 0 },
                    "by_chain": [],
                    "registry": { "active_entries": 0, "by_issuer": [] },
                    "publisher_catalog_watch": {
                      "first_flagged_this_week": 0,
                      "currently_flagged_last_7_days": 0,
                      "unsupported_chain_candidates_seen": 0,
                      "official_on_unsupported_chain": 0,
                      "evicted_entries": 0,
                      "evicted_unsupported_candidates": 0,
                      "rejected_oversize_entries": 0,
                      "rejected_oversize_unsupported_candidates": 0,
                      "last_scanned_at": "",
                      "method": "DexScreener is queried first; a cached USDC search returning no pairs marks it unavailable for five minutes. On DexScreener errors or canary unavailability, GeckoTerminal searches registry product names across supported and unsupported networks for the top 10 tickers, with 10 calls per minute.",
                      "catalog_absent_tokens_truncated": false
                    }
                  },
                  "observed_at": "2026-10-07T00:00:00Z",
                  "public_key": "11111111111111111111111111111111",
                  "signature": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==",
                  "dev": true
                }
              }
            }
          },
          "500": { "description": "Snapshot could not be signed" }
        }
      }
    },
    "/stats.csv": {
      "get": {
        "summary": "Download statistics CSV",
        "responses": {
          "200": {
            "description": "CSV whose first record embeds the exact signed stats JSON document",
            "content": {
              "text/csv": {
                "schema": { "type": "string" },
                "example": "\"snapshot\",\"signed_stats_document_json\",\"{...}\""
              }
            }
          },
          "500": { "description": "Snapshot could not be signed or serialized" }
        }
      }
    },
    "/api/stats": {
      "get": {
        "summary": "Paginated leaderboard, impostor-watch, and active-registry statistics",
        "parameters": [
          {
            "name": "page",
            "in": "query",
            "required": false,
            "description": "1-based page index; each page contains up to 50 rows from each full-history input.",
            "schema": { "type": "integer", "minimum": 1, "default": 1 }
          }
        ],
        "responses": {
          "200": {
            "description": "Prepared point-in-time summary and one page of full leaderboard, catalog-watch, unsupported-candidate, and active-registry inputs",
            "content": {
              "application/json": {
                "schema": { "$ref": "#/components/schemas/StatsApiPage" },
                "example": {
                  "stats": {
                    "generated_at": "2026-10-06T12:00:00Z",
                    "headline": "QED checked 1 of 1 listed pools: 1 issuer match, 0 mismatches, 0 unsupported venues, and 0 not read yet. 0 catalog observations are currently flagged (seen in the last 7 days); 0 were first seen this UTC week. 0 tokens on unsupported chains were seen but not judged. 0 supported observations and 0 unsupported candidates were evicted; 0 invalid supported and 0 invalid unsupported candidates were rejected.",
                    "listed_pools": 1,
                    "pools_checked": 1,
                    "issuer_matches": 1,
                    "mismatches": 0,
                    "unsupported_venue": 0,
                    "not_read_yet": { "count": 0, "rpc_limit": 0, "transient": 0, "unsupported": 0 },
                    "by_chain": [{ "chain": "base", "chain_label": "Base", "counts": { "pools_checked": 1, "issuer_matches": 1, "mismatches": 0, "unsupported_venue": 0, "not_read_yet": { "count": 0, "rpc_limit": 0, "transient": 0, "unsupported": 0 } } }],
                    "registry": { "active_entries": 1, "by_issuer": [{ "issuer": "Backed xStocks", "entries": 1, "chains": ["Base"] }] },
                    "publisher_catalog_watch": {
                      "first_flagged_this_week": 0,
                      "currently_flagged_last_7_days": 0,
                      "unsupported_chain_candidates_seen": 0,
                      "official_on_unsupported_chain": 0,
                      "evicted_entries": 0,
                      "evicted_unsupported_candidates": 0,
                      "rejected_oversize_entries": 0,
                      "rejected_oversize_unsupported_candidates": 0,
                      "last_scanned_at": "2026-10-06T11:30:00Z",
                      "method": "DexScreener is queried first; a cached USDC search returning no pairs marks it unavailable for five minutes. On DexScreener errors or canary unavailability, GeckoTerminal searches registry product names across supported and unsupported networks for the top 10 tickers, with 10 calls per minute.",
                      "catalog_absent_tokens_truncated": false
                    }
                  },
                  "page": 1,
                  "page_size": 50,
                  "pages": 1,
                  "leaderboard_total": 1,
                  "impostor_candidates_total": 0,
                  "unsupported_candidates_total": 0,
                  "active_registry_total": 1,
                  "hashes": { "registry_source": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "leaderboard": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "impostor_watch": "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc" },
                  "leaderboard": [],
                  "impostor_candidates": [],
                  "unsupported_candidates": [],
                  "active_registry": []
                }
              }
            }
          }
        }
      }
    },
    "/api/prices": {
      "get": {
        "summary": "Pool prices",
        "parameters": [
          { "name": "ids", "in": "query", "required": true, "schema": { "type": "string" } }
        ],
        "responses": {
          "200": {
            "description": "Price snapshot",
            "content": {
              "application/json": {
                "schema": { "$ref": "#/components/schemas/PriceSnapshot" },
                "example": {
                  "updated_at": "2026-10-04T12:00:00Z",
                  "prices": [
                    {
                      "chain": "solana",
                      "pool": "known",
                      "price_usd": 12.5,
                      "change_24h_pct": 1.5,
                      "volume_24h_usd": 100.0,
                      "liquidity_usd": 200.0,
                      "source": "dexscreener"
                    }
                  ]
                }
              }
            }
          }
        }
      }
    },
    "/api/status": {
      "get": {
        "summary": "Freshness status",
        "responses": {
          "200": {
            "description": "Service status",
            "content": {
              "application/json": {
                "schema": {
                  "type": "object",
                  "required": ["version"],
                  "properties": { "version": { "type": "string", "example": "1.0.0" } }
                },
                "example": {
                  "version": "1.0.0",
                  "registry": {
                    "entries": 120,
                    "issuers": 6,
                    "updated_at": "2026-10-04T12:00:00Z",
                    "next_refresh_at": "2026-10-04T13:00:00Z",
                    "restored": true,
                    "refreshing": false
                  },
                  "leaderboard": {
                    "updated_at": "2026-10-04T12:00:00Z",
                    "next_refresh_at": "2026-10-04T18:00:00Z",
                    "restored": true,
                    "refreshing": false,
                    "empty_successful": false
                  },
                  "featured": {
                    "updated_at": "2026-10-04T12:00:00Z",
                    "next_refresh_at": "2026-10-04T18:00:00Z",
                    "restored": true,
                    "refreshing": false,
                    "empty_successful": false
                  },
                  "prices": { "updated_at": "2026-10-04T12:00:00Z" }
                }
              }
            }
          }
        }
      }
    },
    "/api/check/{address}": {
      "get": {
        "summary": "Check a pool or token",
        "parameters": [
          { "name": "address", "in": "path", "required": true, "schema": { "type": "string" } }
        ],
        "responses": {
          "200": {
            "description": "Check result",
            "content": {
              "application/json": {
                "schema": { "type": "object" },
                "example": {
                  "input": "0x0000000000000000000000000000000000000001",
                  "chain": "base",
                  "pool": {
                    "chain": "base",
                    "pool": "0x0000000000000000000000000000000000000001",
                    "dex": "uniswap-v2",
                    "base": {
                      "address": "0x0000000000000000000000000000000000000002",
                      "symbol": "xNVDA",
                      "decimals": 18,
                      "balance": "1"
                    },
                    "quote": {
                      "address": "0x0000000000000000000000000000000000000003",
                      "symbol": "USDG",
                      "decimals": 6,
                      "balance": "2"
                    }
                  },
                  "verdict": "NoMatch",
                  "quote_share_of_supply": 0.2,
                  "evidence": [],
                  "checked_at": "2026-01-01T00:00:00Z",
                  "attestation_id": null,
                  "powers": null
                }
              }
            }
          }
        }
      }
    },
    "/api/guard": {
      "post": {
        "summary": "Create a signed Guard review with a JSON request body",
        "description": "The request body may include a wallet for an active restriction check. POST keeps the wallet out of the request URL; use this instead of the legacy GET query when possible. Pool identity requires QED's known-pool index or verified on-chain factory/derivation checks; unindexed Solana accounts are reviewed as tokens. QED is read-only by design: it never holds keys, submits transactions, or recommends. It does not prove backing, custody, reserves, solvency, safety, price, liquidity, or endorsement.",
        "requestBody": {
          "required": true,
          "content": {
            "application/json": {
              "schema": {
                "type": "object",
                "required": ["address", "chain"],
                "additionalProperties": false,
                "properties": {
                  "address": { "type": "string" },
                  "chain": { "type": "string", "enum": ["solana", "robinhood", "base", "ethereum", "bnb"] },
                  "wallet": { "type": "string" }
                }
              },
              "example": { "address": "0x0000000000000000000000000000000000000001", "chain": "base", "wallet": "0x0000000000000000000000000000000000000004" }
            }
          }
        },
        "responses": {
          "200": {
            "description": "Signed Guard document",
            "content": {
              "application/json": {
                "schema": { "$ref": "#/components/schemas/GuardDocument" },
                "example": {
                  "id": "0000000000000000000000000000000000000000000000000000000000000000",
                  "kind": "guard",
                  "chain": "base",
                  "address": "0x0000000000000000000000000000000000000001",
                  "subject_type": "token",
                  "subject_address": "0x0000000000000000000000000000000000000001",
                  "wallet": null,
                  "identity": { "publisher": "Example Publisher", "matched_contract": "0x0000000000000000000000000000000000000001", "ticker": "NVDA", "status": "match" },
                  "powers": null,
                  "source": { "status": "unavailable", "provider": "Sourcify" },
                  "pools": [],
                  "verdict": "unknown",
                  "reasons": [],
                  "observed_at": "2026-10-05T12:00:00Z",
                  "reads": [],
                  "reads_truncated": false,
                  "public_key": "base58-ed25519-public-key",
                  "signature": "base64-signature",
                  "dev": false
                }
              }
            }
          },
          "400": { "description": "Invalid address, chain, wallet, or request body" },
          "502": { "description": "Reader unavailable or upstream chain read failed" },
          "504": { "description": "The 15-second Guard deadline expired" }
        }
      }
    },
    "/api/guard/{address}": {
      "get": {
        "summary": "Create a signed Guard review",
        "description": "A signed point-in-time review of issuer identity, observed token powers, source status, known pool quote-side facts, and optional wallet restrictions. Pool identity requires QED's known-pool index or verified on-chain factory/derivation checks; unindexed Solana accounts are reviewed as tokens. Authority capability alone never denies. A wallet sent in the query string may be exposed in browser history or request URLs; prefer POST /api/guard. QED is read-only by design: it never holds keys, submits transactions, or recommends. It does not prove backing, custody, reserves, solvency, safety, price, liquidity, or endorsement.",
        "parameters": [
          { "name": "address", "in": "path", "required": true, "schema": { "type": "string" } },
          { "name": "chain", "in": "query", "required": true, "schema": { "type": "string", "enum": ["solana", "robinhood", "base", "ethereum", "bnb"] } },
          { "name": "wallet", "in": "query", "required": false, "description": "Optional wallet; query-string privacy tradeoff applies. Prefer POST /api/guard.", "schema": { "type": "string" } }
        ],
        "responses": {
          "200": {
            "description": "Signed Guard document. The example's key, digest, and signature are schematic placeholders; verify live documents with POST /verify.",
            "content": {
              "application/json": {
                "schema": { "$ref": "#/components/schemas/GuardDocument" },
                "example": {
                  "id": "0000000000000000000000000000000000000000000000000000000000000000",
                  "kind": "guard",
                  "chain": "base",
                  "address": "0x0000000000000000000000000000000000000001",
                  "subject_type": "pool",
                  "subject_address": "0x0000000000000000000000000000000000000001",
                  "wallet": null,
                  "identity": {
                    "publisher": "Example Publisher",
                    "matched_contract": "0x0000000000000000000000000000000000000001",
                    "ticker": "NVDA",
                    "status": "match"
                  },
                  "wallet_check": null,
                  "powers": null,
                  "source": { "status": "unavailable", "provider": "Sourcify" },
                  "pools": [],
                  "verdict": "unknown",
                  "reasons": [],
                  "observed_at": "2026-10-05T12:00:00Z",
                  "reads": [],
                  "reads_truncated": false,
                  "public_key": "base58-ed25519-public-key",
                  "signature": "base64-signature",
                  "dev": false
                }
              }
            }
          },
          "400": { "description": "Invalid address, chain, or optional wallet" },
          "502": { "description": "Reader unavailable or upstream chain read failed" },
          "504": { "description": "The 15-second Guard deadline expired" }
        }
      }
    },
    "/api/powers/{address}": {
      "get": {
        "summary": "Observed token powers for any supported-chain contract",
        "description": "Reads a supported-chain token contract, not only registry entries. Optional chain filter; without it, registry matches are returned, otherwise an EVM chain is detected.",
        "parameters": [
          { "name": "address", "in": "path", "required": true, "schema": { "type": "string" } },
          { "name": "chain", "in": "query", "required": false, "schema": { "type": "string", "enum": ["solana", "robinhood", "base", "ethereum", "bnb"] }, "description": "Optional chain filter; without it, active registry matches are returned, or the EVM chain is detected for an unregistered contract." }
        ],
        "responses": {
          "200": {
            "description": "One powers record or an array when the address has active registry matches on multiple chains",
            "content": {
              "application/json": {
                "schema": {
                  "oneOf": [
                    { "$ref": "#/components/schemas/PowersRecord" },
                    { "type": "array", "items": { "$ref": "#/components/schemas/PowersRecord" } }
                  ]
                },
                "example": {
                  "chain": "base",
                  "contract": "0x0000000000000000000000000000000000000002",
                  "can_seize": [],
                  "can_block": [],
                  "can_change_rules": [],
                  "unavailable": [],
                  "source_verified_subject": "contract",
                  "source_verified": "none",
                  "observed_at": "2026-10-02T00:00:00Z",
                  "block": 42,
                  "slot": null,
                  "reads": []
                }
              }
            }
          },
          "400": { "description": "Invalid address or chain" },
          "404": { "description": "No configured EVM chain could be detected" },
          "502": { "description": "Upstream read failed" },
          "504": { "description": "The 15-second token-powers deadline expired" }
        }
      }
    },
    "/api/wallet": {
      "post": {
        "summary": "Check stock-token holdings",
        "requestBody": {
          "required": true,
          "content": {
            "application/json": {
              "schema": {
                "type": "object",
                "required": ["address"],
                "properties": { "address": { "type": "string" } }
              },
              "example": { "address": "0x0000000000000000000000000000000000000001" }
            }
          }
        },
        "responses": {
          "200": {
            "description": "Known stock-token holdings and contract verdicts",
            "content": {
              "application/json": {
                "schema": { "type": "object" },
                "example": {
                  "address": "0x0000000000000000000000000000000000000001",
                  "holdings": [
                    {
                      "symbol": "NVDA",
                      "amount": "1.25",
                      "verdict": "Contract matches issuer registry",
                      "verdict_class": "is-verified",
                      "chain": "base",
                      "pool_url": null,
                      "trade_links": [],
                      "reason": null
                    }
                  ]
                }
              }
            }
          },
          "404": { "description": "Unsupported or invalid address" }
        }
      }
    },
    "/api/attest/{id}": {
      "get": {
        "summary": "Fetch a signed attestation",
        "parameters": [
          { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "pattern": "^[0-9a-fA-F]{64}$" } }
        ],
        "responses": {
          "200": {
            "description": "Attestation JSON",
            "content": {
              "application/json": {
                "schema": { "$ref": "#/components/schemas/Attestation" },
                "example": {
                  "id": "0e9596ff7ba53868e82291934a69a14cc67b3f1cd8625b87013199e128533ffc",
                  "version": 1,
                  "chain": "base",
                  "subject": "0x0000000000000000000000000000000000000001",
                  "verdict": "NoMatch",
                  "issuer": null,
                  "ticker": null,
                  "pool": {
                    "chain": "base",
                    "pool": "0x0000000000000000000000000000000000000001",
                    "dex": "uniswap-v2",
                    "base": {
                      "address": "0x0000000000000000000000000000000000000002",
                      "symbol": "xNVDA",
                      "decimals": 18,
                      "balance": "1"
                    },
                    "quote": {
                      "address": "0x0000000000000000000000000000000000000003",
                      "symbol": "USDG",
                      "decimals": 6,
                      "balance": "2"
                    }
                  },
                  "quote_share_of_supply": 0.2,
                  "registry_entry": null,
                  "registry_hash": "",
                  "reads": [],
                  "block": null,
                  "slot": null,
                  "checked_at": "2026-01-01T00:00:00Z",
                  "expires_at": "2099-01-01T00:00:00Z",
                  "signer": "GmaDrppBC7P5ARKV8g3djiwP89vz1jLK23V2GBjuAEGB",
                  "signature": "84oE823lfrGrxsjGsYTBf+e+SeKeBcMJfZc5KM73U4vHgfz2JQmKw0rBPYU5Y2OGPkSuasG4n2KYOjGmxy+JDA==",
                  "dev": false
                }
              }
            }
          },
          "404": { "description": "Not found" }
        }
      }
    },
    "/verify": {
      "post": {
        "summary": "Verify a signed attestation, statement, Guard, or stats snapshot",
        "requestBody": {
          "required": true,
          "description": "Attestation, statement, and Guard request bodies must be under 2 MiB; signed stats payloads may be up to 16 MiB before their JSON envelope.",
          "content": {
            "application/json": {
              "schema": {
                "oneOf": [
                  { "$ref": "#/components/schemas/Attestation" },
                  { "$ref": "#/components/schemas/Statement" },
                  { "$ref": "#/components/schemas/GuardDocument" },
                  { "$ref": "#/components/schemas/StatsDocument" }
                ]
              },
              "examples": {
                "attestation": {
                  "summary": "Signed attestation",
                  "value": {
                    "id": "0e9596ff7ba53868e82291934a69a14cc67b3f1cd8625b87013199e128533ffc",
                    "version": 1,
                    "chain": "base",
                    "subject": "0x0000000000000000000000000000000000000001",
                    "verdict": "NoMatch",
                    "issuer": null,
                    "ticker": null,
                    "pool": {
                      "chain": "base",
                      "pool": "0x0000000000000000000000000000000000000001",
                      "dex": "uniswap-v2",
                      "base": {
                        "address": "0x0000000000000000000000000000000000000002",
                        "symbol": "xNVDA",
                        "decimals": 18,
                        "balance": "1"
                      },
                      "quote": {
                        "address": "0x0000000000000000000000000000000000000003",
                        "symbol": "USDG",
                        "decimals": 6,
                        "balance": "2"
                      }
                    },
                    "quote_share_of_supply": 0.2,
                    "registry_entry": null,
                    "registry_hash": "",
                    "reads": [],
                    "block": null,
                    "slot": null,
                    "checked_at": "2026-01-01T00:00:00Z",
                    "expires_at": "2099-01-01T00:00:00Z",
                    "signer": "GmaDrppBC7P5ARKV8g3djiwP89vz1jLK23V2GBjuAEGB",
                    "signature": "84oE823lfrGrxsjGsYTBf+e+SeKeBcMJfZc5KM73U4vHgfz2JQmKw0rBPYU5Y2OGPkSuasG4n2KYOjGmxy+JDA==",
                    "dev": false
                  }
                },
                "statement": {
                  "summary": "Signed wallet statement",
                  "value": {
                    "id": "fb3a9270f0c364be29ca410bbe5074e53b42df96970f9fc4a47d7c8d63cf0905",
                    "kind": "statement",
                    "version": 1,
                    "wallets": [
                    { "chain": "base", "address": "0x0000000000000000000000000000000000000001" }
                    ],
                    "assets": [
                      {
                        "wallet": "0x0000000000000000000000000000000000000001",
                        "chain": "base",
                        "contract": "0x0000000000000000000000000000000000000002",
                        "ticker": "NVDA",
                        "issuer": "Backed xStocks",
                        "issuer_match": true,
                        "balance": "1250000000000000000",
                        "decimals": 18,
                        "powers_observed_at": "2026-09-22T12:00:00Z",
                        "powers_block": 23000000,
                        "slot": null,
                        "powers_summary": {
                          "can_seize": [],
                          "can_block": [],
                          "can_change_rules": [],
                          "unavailable": [
                            { "code": "rpc<timeout>", "detail": "read & retry" }
                          ]
                        }
                      }
                    ],
                    "positions": [
                      {
                        "chain": "base",
                        "wallet": "0x0000000000000000000000000000000000000001",
                        "block": 23000000,
                        "min_slot": null,
                        "max_slot": null
                      }
                    ],
                    "block": 23000000,
                    "observed_at": "2026-09-22T12:00:00Z",
                    "reads": [],
                    "reads_truncated": false,
                    "signer": "GmaDrppBC7P5ARKV8g3djiwP89vz1jLK23V2GBjuAEGB",
                    "signature": "vSaEZwxJjk1YVZiWC1D/umfJrw+xNkrjUbOpfB7dyJ9McigMo4RbON6ThH6YwhERblAT9H/IOLImv+h7pkH4Cg==",
                    "dev": false
                  }
                },
                "guard": {
                  "summary": "Signed Guard document (schematic signature placeholders)",
                  "value": {
                    "id": "0000000000000000000000000000000000000000000000000000000000000000",
                    "kind": "guard",
                    "chain": "base",
                    "address": "0x0000000000000000000000000000000000000001",
                    "wallet": null,
                    "identity": {
                      "publisher": "Example Publisher",
                      "matched_contract": "0x0000000000000000000000000000000000000001",
                      "ticker": "NVDA",
                      "status": "match"
                    },
                    "wallet_check": null,
                    "powers": null,
                    "source": { "status": "unavailable", "provider": "Sourcify" },
                    "pools": [],
                    "verdict": "unknown",
                    "reasons": [],
                    "observed_at": "2026-10-05T12:00:00Z",
                    "reads": [],
                    "reads_truncated": false,
                    "public_key": "base58-ed25519-public-key",
                    "signature": "base64-signature",
                    "dev": false
                  }
                },
                "stats": {
                  "summary": "Signed statistics snapshot (schematic signature placeholder)",
                  "value": {
                    "id": "0000000000000000000000000000000000000000000000000000000000000000",
                    "kind": "stats",
                    "stats": {
                      "generated_at": "2026-10-06T12:00:00Z",
                      "headline": "QED checked 0 of 0 listed pools: 0 issuer matches, 0 mismatches, 0 unsupported venues, and 0 not read yet. 0 catalog observations are currently flagged (seen in the last 7 days); 0 were first seen this UTC week. 0 tokens on unsupported chains were seen but not judged. 0 supported observations and 0 unsupported candidates were evicted; 0 invalid supported and 0 invalid unsupported candidates were rejected.",
                      "listed_pools": 0,
                      "pools_checked": 0,
                      "issuer_matches": 0,
                      "mismatches": 0,
                      "unsupported_venue": 0,
                      "not_read_yet": { "count": 0, "rpc_limit": 0, "transient": 0, "unsupported": 0 },
                      "by_chain": [],
                      "registry": { "active_entries": 0, "by_issuer": [] },
                      "publisher_catalog_watch": {
                        "first_flagged_this_week": 0,
                        "currently_flagged_last_7_days": 0,
                        "unsupported_chain_candidates_seen": 0,
                        "official_on_unsupported_chain": 0,
                        "evicted_entries": 0,
                        "evicted_unsupported_candidates": 0,
                        "rejected_oversize_entries": 0,
                        "rejected_oversize_unsupported_candidates": 0,
                        "last_scanned_at": "",
                        "catalog_absent_tokens_truncated": false,
                        "method": "Reserves two searches for each of the 10 highest-volume tickers and rotates up to 30 remaining searches through the rest; at most 50 searches and 50 sequential Guard evaluations per six-hour refresh. Guard checks prioritize candidate tickers in the same highest-volume registry order, then higher-volume pairs within each ticker."
                      }
                    },
                    "observed_at": "2026-10-06T12:00:00Z",
                    "public_key": "base58-ed25519-public-key",
                    "signature": "base64-signature",
                    "dev": false
                  }
                }
              }
            }
          }
        },
        "responses": {
          "200": {
            "description": "Verification result",
            "content": {
              "application/json": {
                "schema": { "$ref": "#/components/schemas/VerifyResult" },
                "examples": {
                  "attestation": {
                    "summary": "Attestation verification",
                    "value": {
                      "ok": true,
                      "kind": "attestation",
                      "id": "0e9596ff7ba53868e82291934a69a14cc67b3f1cd8625b87013199e128533ffc",
                      "cryptographic": true,
                      "trusted_signer": true,
                      "environment_match": true,
                      "fresh": true
                    }
                  },
                  "statement": {
                    "summary": "Statement verification",
                    "value": {
                      "ok": true,
                      "kind": "statement",
                      "id": "fb3a9270f0c364be29ca410bbe5074e53b42df96970f9fc4a47d7c8d63cf0905",
                      "cryptographic": true,
                      "trusted_signer": true,
                      "environment_match": true,
                      "fresh": null
                    }
                  },
                  "guard": {
                    "summary": "Guard verification",
                    "value": {
                      "ok": true,
                      "kind": "guard",
                      "id": "0000000000000000000000000000000000000000000000000000000000000000",
                      "cryptographic": true,
                      "trusted_signer": true,
                      "environment_match": true,
                      "fresh": null
                    }
                  },
                  "stats": {
                    "summary": "Stats snapshot verification",
                    "value": {
                      "ok": true,
                      "kind": "stats",
                      "id": "0000000000000000000000000000000000000000000000000000000000000000",
                      "cryptographic": true,
                      "trusted_signer": true,
                      "environment_match": true,
                      "fresh": null
                    }
                  }
                }
              }
            }
          },
          "400": {
            "description": "Unsupported document shape",
            "content": {
              "application/json": {
                "schema": { "type": "object", "required": ["error"] },
                "example": { "error": "Payload must match a legacy QED attestation or declare its document kind." }
              }
            }
          }
        }
      }
    },
    "/api/statement": {
      "post": {
        "summary": "Create a signed wallet statement",
        "description": "Signs registry-token balances observed for selected wallets and chains. Balances are on-chain facts at a height, not ownership, solvency or reserves. It does not prove backing, custody, reserves, solvency, safety, price, liquidity, or endorsement.",
        "requestBody": {
          "required": true,
          "content": {
            "application/json": {
              "schema": { "$ref": "#/components/schemas/StatementRequest" },
              "example": {
                "label": "Quarterly holdings report",
                "wallets": ["0x0000000000000000000000000000000000000001"],
                "chains": ["base"],
                "block": 23000000
              }
            }
          }
        },
        "responses": {
          "200": {
            "description": "Signed statement",
            "content": {
              "application/json": {
                "schema": { "$ref": "#/components/schemas/Statement" },
                "example": {
                  "id": "fb3a9270f0c364be29ca410bbe5074e53b42df96970f9fc4a47d7c8d63cf0905",
                  "kind": "statement",
                  "version": 1,
                  "wallets": [
                    { "chain": "base", "address": "0x0000000000000000000000000000000000000001" }
                  ],
                  "assets": [
                    {
                      "wallet": "0x0000000000000000000000000000000000000001",
                      "chain": "base",
                      "contract": "0x0000000000000000000000000000000000000002",
                      "ticker": "NVDA",
                      "issuer": "Backed xStocks",
                      "issuer_match": true,
                      "balance": "1250000000000000000",
                      "decimals": 18,
                      "powers_observed_at": "2026-09-22T12:00:00Z",
                      "powers_block": 23000000,
                      "slot": null,
                      "powers_summary": {
                        "can_seize": [],
                        "can_block": [],
                        "can_change_rules": [],
                        "unavailable": [
                          { "code": "rpc<timeout>", "detail": "read & retry" }
                        ]
                      }
                    }
                  ],
                  "positions": [
                    {
                      "chain": "base",
                      "wallet": "0x0000000000000000000000000000000000000001",
                      "block": 23000000,
                      "min_slot": null,
                      "max_slot": null
                    }
                  ],
                  "block": 23000000,
                  "observed_at": "2026-09-22T12:00:00Z",
                  "reads": [],
                  "reads_truncated": false,
                  "signer": "GmaDrppBC7P5ARKV8g3djiwP89vz1jLK23V2GBjuAEGB",
                  "signature": "vSaEZwxJjk1YVZiWC1D/umfJrw+xNkrjUbOpfB7dyJ9McigMo4RbON6ThH6YwhERblAT9H/IOLImv+h7pkH4Cg==",
                  "dev": false
                }
              }
            }
          },
          "400": {
            "description": "Invalid wallet set or chain selection",
            "content": { "application/json": { "schema": { "type": "object", "required": ["error"], "properties": { "error": { "type": "string" } } }, "example": { "error": "A wallet address does not match the selected chain. Check the address and chain selection." } } }
          },
          "500": {
            "description": "QED could not sign the statement",
            "content": { "application/json": { "schema": { "type": "object", "required": ["error"], "properties": { "error": { "type": "string" } } }, "example": { "error": "QED could not sign the statement. No statement was saved." } } }
          },
          "502": {
            "description": "Required chain balance read failed or no reader is configured",
            "content": { "application/json": { "schema": { "type": "object", "required": ["error"], "properties": { "error": { "type": "string" } } }, "example": { "error": "The Base balance read failed. No statement was created." } } }
          },
          "504": {
            "description": "Statement read deadline exceeded",
            "content": { "application/json": { "schema": { "type": "object", "required": ["error"], "properties": { "error": { "type": "string" } } }, "example": { "error": "QED's 15-second balance-read deadline elapsed. No statement was created; retry the request." } } }
          }
        }
      }
    },
    "/api/statement/{id}": {
      "get": {
        "summary": "Fetch a cached signed wallet statement JSON document",
        "parameters": [
          { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "pattern": "^[0-9a-fA-F]{64}$" } }
        ],
        "responses": {
          "200": {
            "description": "Signed statement JSON",
            "content": {
              "application/json": {
                "schema": { "$ref": "#/components/schemas/Statement" },
                "example": {
                  "id": "fb3a9270f0c364be29ca410bbe5074e53b42df96970f9fc4a47d7c8d63cf0905",
                  "kind": "statement",
                  "version": 1,
                  "wallets": [
                    { "chain": "base", "address": "0x0000000000000000000000000000000000000001" }
                  ],
                  "assets": [
                    {
                      "wallet": "0x0000000000000000000000000000000000000001",
                      "chain": "base",
                      "contract": "0x0000000000000000000000000000000000000002",
                      "ticker": "NVDA",
                      "issuer": "Backed xStocks",
                      "issuer_match": true,
                      "balance": "1250000000000000000",
                      "decimals": 18,
                      "powers_observed_at": "2026-09-22T12:00:00Z",
                      "powers_block": 23000000,
                      "slot": null,
                      "powers_summary": {
                        "can_seize": [],
                        "can_block": [],
                        "can_change_rules": [],
                        "unavailable": [
                          { "code": "rpc<timeout>", "detail": "read & retry" }
                        ]
                      }
                    }
                  ],
                  "positions": [
                    {
                      "chain": "base",
                      "wallet": "0x0000000000000000000000000000000000000001",
                      "block": 23000000,
                      "min_slot": null,
                      "max_slot": null
                    }
                  ],
                  "block": 23000000,
                  "observed_at": "2026-09-22T12:00:00Z",
                  "reads": [],
                  "reads_truncated": false,
                  "signer": "GmaDrppBC7P5ARKV8g3djiwP89vz1jLK23V2GBjuAEGB",
                  "signature": "vSaEZwxJjk1YVZiWC1D/umfJrw+xNkrjUbOpfB7dyJ9McigMo4RbON6ThH6YwhERblAT9H/IOLImv+h7pkH4Cg==",
                  "dev": false
                }
              }
            }
          },
          "404": { "description": "Statement is not available in this process cache" }
        }
      }
    },
    "/statements/{id}": {
      "get": {
        "summary": "View a signed wallet statement",
        "parameters": [
          { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "pattern": "^[0-9a-f]{64}$" } }
        ],
        "responses": {
          "200": {
            "description": "Human-readable statement page",
            "content": {
              "text/html": {
                "schema": { "type": "string" },
                "example": "<!doctype html><html lang=\"en\"><title>Signed QED statement</title></html>"
              }
            }
          },
          "404": { "description": "Statement is not available in this process cache" }
        }
      }
    },
    "/statements/{id}/download.csv": {
      "get": {
        "summary": "Download a signed statement as CSV",
        "parameters": [
          { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "pattern": "^[0-9a-f]{64}$" } }
        ],
        "responses": {
          "200": {
            "description": "CSV rows for the signed statement",
            "content": {
              "text/csv": {
                "schema": { "type": "string" },
                "example": "\"record_type\",\"statement_id\",\"label\",\"observed_at\",\"signer\",\"signature\",\"dev\",\"wallet\",\"chain\",\"block\",\"min_slot\",\"max_slot\",\"slot\",\"ticker\",\"issuer\",\"contract\",\"balance\",\"decimals\",\"issuer_match\"\r\n"
              }
            }
          },
          "404": { "description": "Statement is not available in this process cache" }
        }
      }
    },
    "/statements/{id}/verify": {
      "get": {
        "summary": "Verify a signed statement",
        "parameters": [
          { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "pattern": "^[0-9a-f]{64}$" } }
        ],
        "responses": {
          "200": {
            "description": "Human-readable signature, signer, and environment verification",
            "content": { "text/html": { "schema": { "type": "string" } } }
          },
          "404": { "description": "Statement is not available in this process cache" }
        }
      }
    },
    "/statements/{id}/recheck": {
      "post": {
        "summary": "Re-run a signed statement",
        "description": "Repeats the statement's wallet and chain selection and redirects to a new signed statement with a comparison to this record.",
        "parameters": [
          { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "pattern": "^[0-9a-f]{64}$" } }
        ],
        "responses": {
          "303": {
            "description": "Redirect to the new statement page with a comparison",
            "headers": {
              "Location": { "schema": { "type": "string" }, "description": "New statement page with compare={id}." }
            }
          },
          "404": { "description": "Statement is not available in this process cache" },
          "502": { "description": "Required chain balance read failed" }
        }
      }
    },
    "/v/{id}/verify": {
      "get": {
        "summary": "Verify a certificate",
        "parameters": [
          { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "pattern": "^[0-9a-fA-F]{64}$" } }
        ],
        "responses": {
          "200": {
            "description": "Human-readable certificate signature, signer, environment, and freshness verification",
            "content": { "text/html": { "schema": { "type": "string" } } }
          },
          "404": { "description": "Certificate is not available in this process cache" }
        }
      }
    },
    "/mcp": {
      "post": {
        "summary": "MCP tools (2026-07-28; legacy 2025-11-25, 2025-06-18, 2025-03-26 initialize)",
        "description": "Supports MCP versions [\"2026-07-28\", \"2025-11-25\", \"2025-06-18\", \"2025-03-26\"] statelessly. Modern calls use per-request metadata and server/discover; legacy initialize supports all three 2025 revisions.",
        "requestBody": {
          "required": true,
          "content": {
            "application/json": {
              "schema": {
                "oneOf": [
                  { "$ref": "#/components/schemas/JsonRpcRequest" },
                  { "$ref": "#/components/schemas/JsonRpcNotification" }
                ]
              },
              "example": {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                  "name": "qed_registry_lookup",
                  "arguments": { "ticker": "NVDA" }
                }
              }
            }
          }
        },
        "responses": {
          "200": {
            "description": "JSON-RPC 2.0 response",
            "content": {
              "application/json": {
                "schema": { "$ref": "#/components/schemas/JsonRpcResponse" },
                "example": {
                  "jsonrpc": "2.0",
                  "id": 1,
                  "result": {
                    "content": [
                      { "type": "text", "text": "Found 4 active issuer registry entries for NVDA." }
                    ],
                    "structuredContent": {
                      "ticker": "NVDA",
                      "entries": [
                        {
                          "issuer": "Backed xStocks",
                          "ticker": "NVDA",
                          "name": "NVIDIA",
                          "chain": "bnb",
                          "contract": "0xc845b2894dBddd03858fd2D643B4eF725fE0849d",
                          "decimals": null,
                          "source": "xstocks-api",
                          "source_url": "https://api.xstocks.fi/api/v2/public/assets",
                          "last_checked": "2026-09-22T21:26:41Z"
                        },
                        {
                          "issuer": "Backed xStocks",
                          "ticker": "NVDA",
                          "name": "NVIDIA",
                          "chain": "ethereum",
                          "contract": "0xc845b2894dBddd03858fd2D643B4eF725fE0849d",
                          "decimals": null,
                          "source": "xstocks-api",
                          "source_url": "https://api.xstocks.fi/api/v2/public/assets",
                          "last_checked": "2026-09-22T21:26:41Z"
                        },
                        {
                          "issuer": "Backed xStocks",
                          "ticker": "NVDA",
                          "name": "NVIDIA",
                          "chain": "solana",
                          "contract": "Xsc9qvGR1efVDFGLrVsmkzv3qi45LTBjeUKSPmx9qEh",
                          "decimals": null,
                          "source": "xstocks-api",
                          "source_url": "https://api.xstocks.fi/api/v2/public/assets",
                          "last_checked": "2026-09-22T21:26:41Z"
                        },
                        {
                          "issuer": "Robinhood",
                          "ticker": "NVDA",
                          "name": "NVIDIA",
                          "chain": "robinhood",
                          "contract": "0xd0601CE157Db5bdC3162BbaC2a2C8aF5320D9EEC",
                          "decimals": 18,
                          "source": "robinhood-registry",
                          "source_url": "https://api.robinhood.com/rhj/assets",
                          "last_checked": "2026-09-22T21:26:52Z"
                        }
                      ]
                    },
                    "isError": false
                  }
                }
              }
            }
          },
          "202": { "description": "Accepted notification; empty response" },
          "400": {
            "description": "Malformed request or invalid protocol metadata",
            "content": {
              "application/json": {
                "schema": { "$ref": "#/components/schemas/JsonRpcErrorResponse" },
                "example": {
                  "jsonrpc": "2.0",
                  "id": null,
                  "error": { "code": -32700, "message": "Parse error" }
                }
              }
            }
          },
          "403": { "description": "Origin does not match request host" },
          "404": {
            "description": "Unknown JSON-RPC method",
            "content": {
              "application/json": {
                "schema": { "$ref": "#/components/schemas/JsonRpcErrorResponse" },
                "example": {
                  "jsonrpc": "2.0",
                  "id": 1,
                  "error": { "code": -32601, "message": "Method not found" }
                }
              }
            }
          },
          "405": { "description": "POST is required" },
          "413": { "description": "Request body exceeds the HTTP limit" }
        }
      }
    },
    "/.well-known/qed.json": {
      "get": {
        "summary": "Public verification key",
        "responses": {
          "200": {
            "description": "Ed25519 key metadata",
            "content": {
              "application/json": {
                "schema": { "type": "object" },
                "example": {
                  "name": "QED",
                  "version": 1,
                  "algorithm": "Ed25519",
                  "public_key": "GmaDrppBC7P5ARKV8g3djiwP89vz1jLK23V2GBjuAEGB",
                  "key": "GmaDrppBC7P5ARKV8g3djiwP89vz1jLK23V2GBjuAEGB",
                  "dev": false
                }
              }
            }
          }
        }
      }
    },
    "/.well-known/mcp/server-card.json": {
      "get": {
        "summary": "MCP server card",
        "responses": {
          "200": {
            "description": "Read-only MCP server and tool summary",
            "content": {
              "application/json": {
                "schema": { "$ref": "#/components/schemas/McpServerCard" },
                "example": {
                  "name": "QED",
                  "description": "QED is read-only by design: it never holds keys, submits transactions, or recommends. It checks issuer publications and signs Guard reviews. It does not prove backing, custody, reserves, solvency, safety, price, liquidity, or endorsement.",
                  "serverInfo": { "name": "QED", "version": "1.0.0" },
                  "remotes": [
                    { "type": "streamable-http", "url": "https://qed.web3-energy.com/mcp" }
                  ],
                  "tools": [],
                  "website": "https://qed.web3-energy.com",
                  "repository": "https://github.com/boev/qed",
                  "readOnly": true,
                  "readOnlyStatement": "QED is read-only by design: it never holds keys, submits transactions, or recommends."
                }
              }
            }
          }
        }
      }
    }
  },
  "components": {
    "schemas": {
      "MarketDataSource": {
        "type": "string",
        "enum": ["dexscreener", "geckoterminal"],
        "description": "Provider that reported the market-data values; these are not independently validated."
      },
      "PricePoint": {
        "type": "object",
        "required": ["chain", "pool", "price_usd", "change_24h_pct", "volume_24h_usd", "liquidity_usd", "source"],
        "properties": {
          "chain": { "type": "string" },
          "pool": { "type": "string" },
          "price_usd": { "type": ["number", "null"] },
          "change_24h_pct": { "type": ["number", "null"] },
          "volume_24h_usd": { "type": ["number", "null"] },
          "liquidity_usd": { "type": ["number", "null"] },
          "source": { "$ref": "#/components/schemas/MarketDataSource" }
        }
      },
      "PriceSnapshot": {
        "type": "object",
        "required": ["updated_at", "prices"],
        "properties": {
          "updated_at": { "type": "string", "format": "date-time" },
          "prices": { "type": "array", "items": { "$ref": "#/components/schemas/PricePoint" } }
        }
      },
      "ReadStatusCounts": {
        "type": "object",
        "additionalProperties": false,
        "required": ["count", "rpc_limit", "transient", "unsupported"],
        "properties": {
          "count": { "type": "integer", "minimum": 0 },
          "rpc_limit": { "type": "integer", "minimum": 0 },
          "transient": { "type": "integer", "minimum": 0 },
          "unsupported": { "type": "integer", "minimum": 0 }
        }
      },
      "LeaderboardCounts": {
        "type": "object",
        "additionalProperties": false,
        "required": ["pools_checked", "issuer_matches", "mismatches", "unsupported_venue", "not_read_yet"],
        "properties": {
          "pools_checked": { "type": "integer", "minimum": 0 },
          "issuer_matches": { "type": "integer", "minimum": 0 },
          "mismatches": { "type": "integer", "minimum": 0 },
          "unsupported_venue": { "type": "integer", "minimum": 0, "description": "Leaderboard pool whose venue QED does not support; not counted as checked or unread." },
          "not_read_yet": { "$ref": "#/components/schemas/ReadStatusCounts" }
        }
      },
      "ChainLeaderboardCounts": {
        "type": "object",
        "additionalProperties": false,
        "required": ["chain", "chain_label", "counts"],
        "properties": {
          "chain": { "type": "string" },
          "chain_label": { "type": "string" },
          "counts": { "$ref": "#/components/schemas/LeaderboardCounts" }
        }
      },
      "IssuerRegistrySize": {
        "type": "object",
        "additionalProperties": false,
        "required": ["issuer", "entries", "chains"],
        "properties": {
          "issuer": { "type": "string" },
          "entries": { "type": "integer", "minimum": 0 },
          "chains": { "type": "array", "items": { "type": "string" } }
        }
      },
      "RegistrySizes": {
        "type": "object",
        "additionalProperties": false,
        "required": ["active_entries", "by_issuer"],
        "properties": {
          "active_entries": { "type": "integer", "minimum": 0 },
          "by_issuer": { "type": "array", "items": { "$ref": "#/components/schemas/IssuerRegistrySize" } }
        }
      },
      "CatalogAbsentToken": {
        "type": "object",
        "additionalProperties": false,
        "required": ["chain", "chain_label", "ticker", "publisher", "source", "symbol", "name", "address", "first_seen_at", "last_seen_at", "volume_24h_usd", "guard_url", "reason", "reads", "evidence_truncated"],
        "properties": {
          "chain": { "type": "string", "maxLength": 32 },
          "chain_label": { "type": "string", "maxLength": 64 },
          "ticker": { "type": "string", "maxLength": 64 },
          "publisher": { "type": "string", "maxLength": 64 },
          "source": { "$ref": "#/components/schemas/MarketDataSource" },
          "symbol": { "type": "string", "maxLength": 64, "description": "Reported by the source named in source." },
          "name": { "type": "string", "maxLength": 64, "description": "Reported by the source named in source." },
          "address": { "type": "string", "maxLength": 128 },
          "first_seen_at": { "type": "string", "format": "date-time", "maxLength": 64 },
          "last_seen_at": { "type": "string", "format": "date-time", "maxLength": 64 },
          "volume_24h_usd": { "type": ["number", "null"], "minimum": 0, "description": "Reported by the source named in source; not independently validated." },
          "guard_url": { "type": "string", "maxLength": 200 },
          "reason": { "type": "string", "maxLength": 256 },
          "reads": { "type": "array", "maxItems": 4, "items": { "$ref": "#/components/schemas/ObservedRead" }, "description": "Identity-determining token symbol/name reads at the queried address (or Solana metadata reads tied to that mint). Each retained params and raw_result value is bounded to 512 serialized bytes. Other Guard reads are available from guard_url and are not duplicated here." },
          "publisher_catalog_snapshot_hash": { "type": "string", "pattern": "^[0-9a-f]{64}$", "description": "SHA-256 hash of the publisher registry/catalog source snapshot used for this observation; absent on legacy restored observations." },
          "evidence_truncated": { "type": "boolean", "description": "True when unrelated reads were omitted, a retained read was bounded, the original Guard read log was truncated, supported observation text was clipped, or no publisher catalog snapshot hash was available." },
          "on_chain_symbol": { "type": ["string", "null"], "maxLength": 64, "description": "Symbol read from the candidate contract on chain, distinct from listing metadata." },
          "on_chain_name": { "type": ["string", "null"], "maxLength": 64, "description": "Name read from the candidate contract on chain, distinct from listing metadata." }
        }
      },
      "UnsupportedCatalogCandidate": {
        "type": "object",
        "additionalProperties": false,
        "required": ["dex_chain_id", "ticker", "publisher", "source", "symbol", "name", "address", "first_seen_at", "last_seen_at", "volume_24h_usd", "evidence_truncated"],
        "properties": {
          "dex_chain_id": { "type": "string", "maxLength": 32 },
          "ticker": { "type": "string", "maxLength": 64 },
          "publisher": { "type": "string", "maxLength": 64 },
          "source": { "$ref": "#/components/schemas/MarketDataSource" },
          "symbol": { "type": "string", "maxLength": 64, "description": "Reported by the source named in source." },
          "name": { "type": "string", "maxLength": 64, "description": "Reported by the source named in source." },
          "address": { "type": "string", "maxLength": 128 },
          "first_seen_at": { "type": "string", "format": "date-time", "maxLength": 64 },
          "last_seen_at": { "type": "string", "format": "date-time", "maxLength": 64 },
          "volume_24h_usd": { "type": ["number", "null"], "minimum": 0 },
          "evidence_truncated": { "type": "boolean", "description": "True when overlong candidate text or timestamps were clipped to their storage caps." }
        }
      },
      "ImpostorSnapshot": {
        "type": "object",
        "additionalProperties": false,
        "required": ["scanned_at", "next_ticker_offset", "next_entry_offset", "unsupported_seen", "official_on_unsupported_chain", "evicted_entries", "evicted_unsupported_candidates", "rejected_oversize_entries", "rejected_oversize_unsupported_candidates", "entries", "unsupported_candidates"],
        "properties": {
          "scanned_at": { "type": "string", "maxLength": 64 },
          "source_unavailable_since": { "type": "string", "maxLength": 64, "description": "RFC 3339 start of the current search-source outage, when every search failed or returned no pairs; absent after a successful scan. Retained observations and scanned_at stay from the last successful scan." },
          "next_ticker_offset": { "type": "integer", "minimum": 0 },
          "next_entry_offset": { "type": "integer", "minimum": 0 },
          "unsupported_seen": { "type": "integer", "minimum": 0 },
          "official_on_unsupported_chain": { "type": "integer", "minimum": 0, "description": "Official deployments on unsupported networks matched by listed address or wrapper; counted separately from impostor candidates." },
          "evicted_entries": { "type": "integer", "minimum": 0 },
          "evicted_unsupported_candidates": { "type": "integer", "minimum": 0 },
          "rejected_oversize_entries": { "type": "integer", "minimum": 0 },
          "rejected_oversize_unsupported_candidates": { "type": "integer", "minimum": 0 },
          "entries": { "type": "array", "maxItems": 128, "items": { "$ref": "#/components/schemas/CatalogAbsentToken" } },
          "unsupported_candidates": { "type": "array", "maxItems": 256, "items": { "$ref": "#/components/schemas/UnsupportedCatalogCandidate" } }
        }
      },
      "StatsInputHashes": {
        "type": "object",
        "additionalProperties": false,
        "required": ["registry_source", "leaderboard", "impostor_watch"],
        "properties": {
          "registry_source": { "type": "string", "description": "Registry publisher-source hash at snapshot time." },
          "leaderboard": { "type": "string", "pattern": "^[0-9a-f]{64}$" },
          "impostor_watch": { "type": "string", "pattern": "^[0-9a-f]{64}$" }
        }
      },
      "StatsReducerInputs": {
        "type": "object",
        "additionalProperties": false,
        "required": ["leaderboard", "impostor_watch", "active_registry", "hashes"],
        "properties": {
          "leaderboard": { "type": "array", "maxItems": 300, "items": { "$ref": "#/components/schemas/LeaderboardEntry" } },
          "impostor_watch": { "$ref": "#/components/schemas/ImpostorSnapshot" },
          "active_registry": { "type": "array", "items": { "$ref": "#/components/schemas/RegistryEntry" } },
          "hashes": { "$ref": "#/components/schemas/StatsInputHashes" }
        }
      },
      "LeaderboardEntry": {
        "type": "object",
        "additionalProperties": false,
        "required": ["rank", "chain", "chain_label", "dex", "pool", "source", "base_symbol", "quote_symbol", "issuer", "ticker", "issuer_on_base", "verdict", "read_status", "read_reason", "price_usd", "change_24h_pct", "volume_24h_usd", "liquidity_usd", "txns_24h", "detail_url", "trade_url", "explorer_url", "attestation_id", "checked_at"],
        "properties": {
          "rank": { "type": "integer", "minimum": 1 },
          "chain": { "type": "string", "maxLength": 32 },
          "chain_label": { "type": "string", "maxLength": 64 },
          "dex": { "type": "string", "maxLength": 64 },
          "pool": { "type": "string", "maxLength": 128 },
          "source": { "$ref": "#/components/schemas/MarketDataSource" },
          "base_symbol": { "type": "string", "maxLength": 64 },
          "quote_symbol": { "type": "string", "maxLength": 64 },
          "issuer": { "type": ["string", "null"], "maxLength": 64 },
          "ticker": { "type": ["string", "null"], "maxLength": 64 },
          "issuer_on_base": { "type": ["boolean", "null"], "description": "True when the base-side token is the contract matched to the issuer registry." },
          "verdict": { "type": "string" },
          "read_status": { "type": "string", "enum": ["checked", "not_read_yet", "unsupported_venue"], "description": "unsupported_venue indicates QED intentionally skipped an unsupported DEX venue." },
          "read_reason": { "type": ["string", "null"], "enum": ["rpc_limit", "transient", "unsupported", "unsupported_venue", null] },
          "price_usd": { "type": ["number", "null"] },
          "change_24h_pct": { "type": ["number", "null"] },
          "volume_24h_usd": { "type": ["number", "null"] },
          "liquidity_usd": { "type": ["number", "null"] },
          "txns_24h": { "type": ["integer", "null"] },
          "detail_url": { "type": "string", "maxLength": 200 },
          "trade_url": { "type": "string", "maxLength": 200 },
          "explorer_url": { "type": "string", "maxLength": 200 },
          "attestation_id": { "type": ["string", "null"] },
          "checked_at": { "type": ["string", "null"] }
        }
      },
      "OfficialDeployment": {
        "type": "object",
        "additionalProperties": false,
        "required": ["network", "address"],
        "properties": {
          "network": { "type": "string" },
          "address": { "type": "string" },
          "wrapper_address": { "type": "string" },
          "wrapper_address_v2": { "type": "string" }
        }
      },
      "RegistryEntry": {
        "type": "object",
        "additionalProperties": false,
        "required": ["issuer", "ticker", "name", "chain", "contract", "decimals", "source", "source_url", "last_checked"],
        "properties": {
          "issuer": { "type": "string" },
          "ticker": { "type": "string" },
          "name": { "type": "string" },
          "chain": { "type": "string" },
          "contract": { "type": "string" },
          "decimals": { "type": ["integer", "null"], "minimum": 0, "maximum": 255 },
          "source": { "type": "string" },
          "source_url": { "type": "string" },
          "last_checked": { "type": "string", "format": "date-time" },
          "removed_at": { "type": "string", "format": "date-time" },
          "stale_since": { "type": "string", "format": "date-time" },
          "official_deployments": { "type": "array", "items": { "$ref": "#/components/schemas/OfficialDeployment" } }
        }
      },
      "StatsApiPage": {
        "type": "object",
        "additionalProperties": false,
        "required": ["stats", "page", "page_size", "pages", "leaderboard_total", "impostor_candidates_total", "unsupported_candidates_total", "active_registry_total", "hashes", "leaderboard", "impostor_candidates", "unsupported_candidates", "active_registry"],
        "properties": {
          "stats": { "$ref": "#/components/schemas/LeaderboardStats" },
          "page": { "type": "integer", "minimum": 1 },
          "page_size": { "type": "integer", "const": 50 },
          "pages": { "type": "integer", "minimum": 1 },
          "leaderboard_total": { "type": "integer", "minimum": 0 },
          "impostor_candidates_total": { "type": "integer", "minimum": 0 },
          "unsupported_candidates_total": { "type": "integer", "minimum": 0 },
          "active_registry_total": { "type": "integer", "minimum": 0 },
          "hashes": { "$ref": "#/components/schemas/StatsInputHashes" },
          "leaderboard": { "type": "array", "items": { "$ref": "#/components/schemas/LeaderboardEntry" } },
          "impostor_candidates": { "type": "array", "items": { "$ref": "#/components/schemas/CatalogAbsentToken" } },
          "unsupported_candidates": { "type": "array", "items": { "$ref": "#/components/schemas/UnsupportedCatalogCandidate" } },
          "active_registry": { "type": "array", "items": { "$ref": "#/components/schemas/RegistryEntry" } }
        }
      },
      
      "PublisherCatalogWatch": {
        "type": "object",
        "additionalProperties": false,
        "required": ["first_flagged_this_week", "currently_flagged_last_7_days", "unsupported_chain_candidates_seen", "official_on_unsupported_chain", "evicted_entries", "evicted_unsupported_candidates", "rejected_oversize_entries", "rejected_oversize_unsupported_candidates", "last_scanned_at", "method", "catalog_absent_tokens_truncated"],
        "properties": {
          "first_flagged_this_week": { "type": "integer", "minimum": 0, "description": "Retained catalog observations first seen in the current UTC week." },
          "currently_flagged_last_7_days": { "type": "integer", "minimum": 0, "description": "Retained supported-chain catalog observations last seen within the previous seven days." },
          "unsupported_chain_candidates_seen": { "type": "integer", "minimum": 0 },
          "official_on_unsupported_chain": { "type": "integer", "minimum": 0, "description": "Unique official deployments observed on unsupported networks; separate from unsupported candidate counts and not part of the headline." },
          "evicted_entries": { "type": "integer", "minimum": 0, "description": "Supported observations evicted because retained history exceeded its bound." },
          "evicted_unsupported_candidates": { "type": "integer", "minimum": 0, "description": "Unsupported-chain observations evicted because retained history exceeded its bound." },
          "rejected_oversize_entries": { "type": "integer", "minimum": 0, "description": "Supported observations rejected for an invalid chain, token address, or publisher catalog hash; overlong text and read evidence are clipped, retained, and marked with evidence_truncated." },
          "rejected_oversize_unsupported_candidates": { "type": "integer", "minimum": 0, "description": "Unsupported-chain candidates rejected for an invalid chain id or token address; overlong text is clipped, retained, and marked with evidence_truncated." },
          "last_scanned_at": { "type": "string", "description": "RFC 3339 time of the last successful scan, or empty before the first one." },
          "source_unavailable_since": { "type": "string", "description": "RFC 3339 start of the current search-source outage; absent after a successful scan. Counts then come from the last successful scan." },
          "method": { "type": "string" },
          "catalog_absent_tokens_truncated": { "type": "boolean", "description": "True when more than 20 retained observations were seen within the last seven days and the HTML table is truncated." }
        }
      },
      "LeaderboardStats": {
        "type": "object",
        "additionalProperties": false,
        "required": ["generated_at", "headline", "listed_pools", "pools_checked", "issuer_matches", "mismatches", "unsupported_venue", "not_read_yet", "by_chain", "registry", "publisher_catalog_watch"],
        "properties": {
          "generated_at": { "type": "string", "format": "date-time" },
          "headline": { "type": "string" },
          "listed_pools": { "type": "integer", "minimum": 0 },
          "pools_checked": { "type": "integer", "minimum": 0 },
          "issuer_matches": { "type": "integer", "minimum": 0 },
          "mismatches": { "type": "integer", "minimum": 0 },
          "unsupported_venue": { "type": "integer", "minimum": 0, "description": "Pool reads omitted because QED does not support the venue." },
          "not_read_yet": { "$ref": "#/components/schemas/ReadStatusCounts" },
          "by_chain": { "type": "array", "items": { "$ref": "#/components/schemas/ChainLeaderboardCounts" } },
          "registry": { "$ref": "#/components/schemas/RegistrySizes" },
          "publisher_catalog_watch": { "$ref": "#/components/schemas/PublisherCatalogWatch" }
        }
      },
      "StatsDocument": {
        "type": "object",
        "additionalProperties": false,
        "description": "Strict Ed25519-signed point-in-time statistics snapshot accepted by POST /verify.",
        "required": ["id", "kind", "stats", "reducer_inputs", "observed_at", "public_key", "signature", "dev"],
        "properties": {
          "id": { "type": "string", "pattern": "^[0-9a-f]{64}$", "description": "SHA-256 of the canonical signed payload." },
          "kind": { "type": "string", "const": "stats" },
          "stats": { "$ref": "#/components/schemas/LeaderboardStats" },
          "reducer_inputs": { "$ref": "#/components/schemas/StatsReducerInputs" },
          "observed_at": { "type": "string", "format": "date-time" },
          "public_key": { "type": "string", "maxLength": 44, "description": "Base58 Ed25519 public key." },
          "signature": { "type": "string", "contentEncoding": "base64" },
          "dev": { "type": "boolean" }
        }
      },
      "JsonRpcRequest": {
        "type": "object",
        "example": {
          "jsonrpc": "2.0",
          "id": 1,
          "method": "tools/call",
          "params": {
            "name": "qed_registry_lookup",
            "arguments": { "ticker": "NVDA" }
          }
        },
        "required": ["jsonrpc", "id", "method"],
        "properties": {
          "jsonrpc": { "const": "2.0" },
          "id": { "oneOf": [{ "type": "string" }, { "type": "number" }] },
          "method": { "type": "string" },
          "params": { "type": "object" }
        }
      },
      "JsonRpcNotification": {
        "type": "object",
        "required": ["jsonrpc", "method"],
        "properties": {
          "jsonrpc": { "const": "2.0" },
          "method": { "type": "string" },
          "params": { "type": "object" }
        }
      },
      "JsonRpcResponse": {
        "oneOf": [
          { "$ref": "#/components/schemas/JsonRpcResultResponse" },
          { "$ref": "#/components/schemas/JsonRpcErrorResponse" }
        ]
      },
      "JsonRpcResultResponse": {
        "type": "object",
        "example": {
          "jsonrpc": "2.0",
          "id": 1,
          "result": {
            "content": [
              { "type": "text", "text": "Found 4 active issuer registry entries for NVDA." }
            ],
            "structuredContent": {
              "ticker": "NVDA",
              "entries": [
                {
                  "issuer": "Backed xStocks",
                  "ticker": "NVDA",
                  "name": "NVIDIA",
                  "chain": "bnb",
                  "contract": "0xc845b2894dBddd03858fd2D643B4eF725fE0849d",
                  "decimals": null,
                  "source": "xstocks-api",
                  "source_url": "https://api.xstocks.fi/api/v2/public/assets",
                  "last_checked": "2026-09-22T21:26:41Z"
                },
                {
                  "issuer": "Backed xStocks",
                  "ticker": "NVDA",
                  "name": "NVIDIA",
                  "chain": "ethereum",
                  "contract": "0xc845b2894dBddd03858fd2D643B4eF725fE0849d",
                  "decimals": null,
                  "source": "xstocks-api",
                  "source_url": "https://api.xstocks.fi/api/v2/public/assets",
                  "last_checked": "2026-09-22T21:26:41Z"
                },
                {
                  "issuer": "Backed xStocks",
                  "ticker": "NVDA",
                  "name": "NVIDIA",
                  "chain": "solana",
                  "contract": "Xsc9qvGR1efVDFGLrVsmkzv3qi45LTBjeUKSPmx9qEh",
                  "decimals": null,
                  "source": "xstocks-api",
                  "source_url": "https://api.xstocks.fi/api/v2/public/assets",
                  "last_checked": "2026-09-22T21:26:41Z"
                },
                {
                  "issuer": "Robinhood",
                  "ticker": "NVDA",
                  "name": "NVIDIA",
                  "chain": "robinhood",
                  "contract": "0xd0601CE157Db5bdC3162BbaC2a2C8aF5320D9EEC",
                  "decimals": 18,
                  "source": "robinhood-registry",
                  "source_url": "https://api.robinhood.com/rhj/assets",
                  "last_checked": "2026-09-22T21:26:52Z"
                }
              ]
            },
            "isError": false
          }
        },
        "required": ["jsonrpc", "id", "result"],
        "properties": {
          "jsonrpc": { "const": "2.0" },
          "id": { "oneOf": [{ "type": "string" }, { "type": "number" }] },
          "result": {
            "type": "object",
            "description": "MCP CallToolResult; structuredContent contains the JSON payload for issuer checks, wallet holdings, registry lookups, or document verification.",
            "properties": {
              "structuredContent": { "description": "Tool-specific structured JSON payload." },
              "content": { "type": "array", "items": { "type": "object" } },
              "isError": { "type": "boolean" }
            }
          }
        }
      },
      "JsonRpcErrorResponse": {
        "type": "object",
        "required": ["jsonrpc", "error"],
        "properties": {
          "jsonrpc": { "const": "2.0" },
          "id": { "oneOf": [{ "type": "string" }, { "type": "number" }, { "type": "null" }] },
          "error": {
            "type": "object",
            "required": ["code", "message"],
            "properties": {
              "code": { "type": "integer" },
              "message": { "type": "string" }
            }
          }
        }
      },
      "PowerReason": {
        "type": "object",
        "required": ["code", "detail"],
        "properties": {
          "code": { "type": "string" },
          "detail": { "type": "string" }
        }
      },
      "GuardReason": {
        "type": "object",
        "required": ["code", "detail"],
        "properties": {
          "code": {
            "type": "string",
            "enum": ["publisher_contract_match", "publisher_contract_mismatch", "claims_unpublished_publisher_product", "name_resembles_registry_entry", "publisher_metadata_unavailable", "no_publisher", "registry_stale", "registry_removed", "token_paused", "powers_incomplete", "powers_unavailable", "wallet_check_not_applicable", "wallet_check_unavailable", "wallet_frozen", "wallet_blocked", "wallet_sanctioned", "source_unverified", "source_unavailable", "pool_unavailable"]
          },
          "detail": { "type": "string" }
        }
      },
      "ObservedRead": {
        "type": "object",
        "required": ["method", "params", "result_hash", "block", "slot"],
        "properties": {
          "method": { "type": "string" },
          "params": {},
          "result_hash": { "type": "string" },
          "raw_result": {},
          "block": { "type": ["integer", "null"], "minimum": 0 },
          "slot": { "type": ["integer", "null"], "minimum": 0 }
        }
      },
      "PowersRecord": {
        "type": "object",
        "required": ["chain", "contract", "can_seize", "can_block", "can_change_rules", "unavailable", "source_verified_subject", "source_verified", "observed_at", "block", "slot", "reads"],
        "properties": {
          "chain": { "type": "string", "enum": ["solana", "robinhood", "base", "ethereum", "bnb"] },
          "contract": { "type": "string" },
          "token_paused": { "type": ["boolean", "null"], "description": "Observed token-wide pause; absent when the chain does not expose this fact or the read is unavailable." },
          "sanctions_list": { "type": ["string", "null"], "description": "Configured sanctions-list oracle used for wallet checks." },
          "can_seize": { "type": "array", "items": { "$ref": "#/components/schemas/PowerReason" } },
          "can_block": { "type": "array", "items": { "$ref": "#/components/schemas/PowerReason" } },
          "can_change_rules": { "type": "array", "items": { "$ref": "#/components/schemas/PowerReason" } },
          "unavailable": { "type": "array", "items": { "$ref": "#/components/schemas/PowerReason" } },
          "source_verified_subject": { "type": "string", "enum": ["token_program", "contract", "implementation"], "description": "OSEC verifies the Solana Token-2022 token-program build; Sourcify checks EVM contract source or a proxy's resolved implementation." },
          "source_verified": { "type": "string", "enum": ["exact_match", "match", "none", "unavailable"] },
          "source_verified_proxy": { "type": "string", "enum": ["exact_match", "match", "none", "unavailable"], "description": "Separate Sourcify source status for an EVM proxy, when applicable." },
          "observed_at": { "type": "string", "format": "date-time" },
          "block": { "type": ["integer", "null"], "minimum": 0 },
          "slot": { "type": ["integer", "null"], "minimum": 0 },
          "reads": { "type": "array", "items": { "$ref": "#/components/schemas/ObservedRead" } }
        }
      },
      "PowersRecordSet": {
        "type": "object",
        "required": ["records"],
        "properties": {
          "records": { "type": "array", "items": { "$ref": "#/components/schemas/PowersRecord" } }
        }
      },
      "GuardDocument": {
        "type": "object",
        "description": "Signed point-in-time review. The OpenAPI example uses schematic signature placeholders; verify live documents with POST /verify.",
        "required": ["id", "kind", "chain", "address", "subject_type", "subject_address", "wallet", "identity", "powers", "source", "pools", "verdict", "reasons", "observed_at", "reads", "reads_truncated", "public_key", "signature", "dev"],
        "properties": {
          "id": { "type": "string", "pattern": "^[0-9a-fA-F]{64}$" },
          "kind": { "type": "string", "const": "guard" },
          "chain": { "type": "string", "enum": ["solana", "robinhood", "base", "ethereum", "bnb"] },
          "address": { "type": "string" },
          "subject_type": { "type": "string", "enum": ["token", "pool"] },
          "subject_address": { "type": ["string", "null"] },
          "wallet": { "type": ["string", "null"] },
          "wallet_check": { "oneOf": [{ "$ref": "#/components/schemas/GuardWalletCheck" }, { "type": "null" }] },
          "identity": { "$ref": "#/components/schemas/GuardIdentity" },
          "powers": { "oneOf": [{ "$ref": "#/components/schemas/PowersRecord" }, { "type": "null" }] },
          "source": { "$ref": "#/components/schemas/GuardSource" },
          "pools": { "type": "array", "items": { "$ref": "#/components/schemas/GuardPool" } },
          "verdict": { "type": "string", "enum": ["allow", "deny", "unknown"] },
          "reasons": { "type": "array", "items": { "$ref": "#/components/schemas/GuardReason" } },
          "observed_at": { "type": "string", "format": "date-time" },
          "reads": { "type": "array", "items": { "$ref": "#/components/schemas/ObservedRead" } },
          "reads_truncated": { "type": "boolean" },
          "public_key": { "type": "string", "description": "Base58 Ed25519 public key." },
          "signature": { "type": "string", "contentEncoding": "base64" },
          "dev": { "type": "boolean" }
        }
      },
      "GuardIdentity": {
        "type": "object",
        "required": ["publisher", "matched_contract", "ticker", "status"],
        "properties": {
          "publisher": { "type": ["string", "null"] },
          "matched_contract": { "type": ["string", "null"] },
          "ticker": { "type": ["string", "null"] },
          "status": { "type": "string", "enum": ["match", "mismatch", "no_publisher", "registry_stale", "registry_removed"] },
          "candidate": { "oneOf": [{ "$ref": "#/components/schemas/GuardIdentityCandidate" }, { "type": "null" }] },
          "observed_symbol": { "type": ["string", "null"] },
          "observed_name": { "type": ["string", "null"] },
          "unpublished_product_detail": { "type": ["string", "null"] },
          "deployments": { "type": "array", "items": { "$ref": "#/components/schemas/GuardDeployment" } }
        }
      },
      "GuardIdentityCandidate": {
        "type": "object",
        "required": ["publisher", "ticker", "name"],
        "properties": {
          "publisher": { "type": "string" },
          "ticker": { "type": "string" },
          "name": { "type": "string" }
        }
      },
      "GuardWalletCheck": {
        "type": "object",
        "required": ["status", "restrictions"],
        "properties": {
          "status": { "type": "string", "enum": ["checked", "not_applicable", "unavailable"] },
          "restrictions": { "type": "array", "items": { "$ref": "#/components/schemas/GuardReason" } }
        }
      },
      "GuardSource": {
        "type": "object",
        "required": ["status", "provider"],
        "properties": {
          "status": { "type": "string", "enum": ["verified", "unverified", "unavailable"] },
          "provider": { "type": "string" }
        }
      },
      "GuardPool": {
        "type": "object",
        "required": ["address", "venue", "quote", "verdict", "observed_at"],
        "properties": {
          "address": { "type": "string" },
          "venue": { "type": "string" },
          "quote": { "$ref": "#/components/schemas/GuardQuote" },
          "verdict": { "type": "string" },
          "observed_at": { "type": ["string", "null"], "format": "date-time" }
        }
      },
      "GuardQuote": {
        "type": "object",
        "required": ["address", "symbol"],
        "properties": {
          "address": { "type": "string" },
          "symbol": { "type": ["string", "null"] }
        }
      },


      "Attestation": {
        "type": "object",
        "required": ["id", "version", "chain", "subject", "verdict", "issuer", "ticker", "pool", "quote_share_of_supply", "registry_entry", "registry_hash", "reads", "block", "slot", "checked_at", "expires_at", "signer", "signature", "dev"],
        "properties": {
          "id": { "type": "string", "pattern": "^[0-9a-fA-F]{64}$" },
          "version": { "type": "integer", "const": 1 },
          "chain": { "type": "string", "enum": ["solana", "robinhood", "base", "ethereum", "bnb"] },
          "subject": { "type": "string" },
          "verdict": { "type": "object" },
          "issuer": { "type": ["string", "null"] },
          "ticker": { "type": ["string", "null"] },
          "pool": { "type": "object" },
          "quote_share_of_supply": { "type": ["number", "null"] },
          "registry_entry": { "type": ["object", "null"] },
          "registry_hash": { "type": "string" },
          "reads": { "type": "array", "items": { "$ref": "#/components/schemas/ObservedRead" } },
          "block": { "type": ["integer", "null"], "minimum": 0 },
          "slot": { "type": ["integer", "null"], "minimum": 0 },
          "checked_at": { "type": "string", "format": "date-time" },
          "expires_at": { "type": "string", "format": "date-time" },
          "signer": { "type": "string" },
          "signature": { "type": "string", "contentEncoding": "base64" },
          "dev": { "type": "boolean" }
        }
      },
      "VerifyResult": {
        "type": "object",
        "required": ["ok", "kind", "id", "cryptographic", "trusted_signer", "environment_match", "fresh"],
        "properties": {
          "ok": { "type": "boolean" },
          "kind": { "type": "string", "enum": ["attestation", "statement", "guard", "stats"] },
          "id": { "type": "string" },
          "cryptographic": { "type": "boolean" },
          "trusted_signer": { "type": "boolean" },
          "environment_match": { "type": "boolean" },
          "fresh": { "type": ["boolean", "null"], "description": "Null for statements, Guards, and stats snapshots, which do not have attestation expiry checks." }
        }
      },
      "StatementRequest": {
        "type": "object",
        "required": ["wallets", "chains"],
        "properties": {
          "label": { "type": "string", "description": "Optional human label included in the signed statement." },
          "wallets": { "type": "array", "minItems": 1, "maxItems": 32, "items": { "type": "string" } },
          "chains": { "type": "array", "minItems": 1, "maxItems": 5, "uniqueItems": true, "items": { "type": "string", "enum": ["solana", "robinhood", "base", "ethereum", "bnb"] } },
          "block": { "type": ["integer", "null"], "minimum": 0, "description": "Optional exact EVM block; for Solana this is a minimum context slot." }
        }
      },
      "Statement": {
        "type": "object",
        "required": ["id", "kind", "version", "wallets", "assets", "positions", "block", "observed_at", "reads", "reads_truncated", "signer", "signature", "dev"],
        "properties": {
          "id": { "type": "string", "pattern": "^[0-9a-f]{64}$", "description": "SHA-256 of the canonical signed payload." },
          "kind": { "type": "string", "const": "statement", "description": "Signed document type." },
          "version": { "type": "integer", "const": 1 },
          "label": { "type": "string", "description": "Optional human label included in the signed statement." },
          "wallets": { "type": "array", "items": { "$ref": "#/components/schemas/StatementWallet" } },
          "assets": { "type": "array", "items": { "$ref": "#/components/schemas/StatementAsset" } },
          "positions": { "type": "array", "items": { "$ref": "#/components/schemas/StatementPosition" } },
          "block": { "type": ["integer", "null"], "minimum": 0 },
          "observed_at": { "type": "string", "format": "date-time" },
          "reads": { "type": "array", "items": { "$ref": "#/components/schemas/ObservedRead" } },
          "reads_truncated": { "type": "boolean", "description": "True when the read log reached its 256-entry cap." },
          "signer": { "type": "string", "description": "Base58 Ed25519 public key." },
          "signature": { "type": "string", "contentEncoding": "base64" },
          "dev": { "type": "boolean" }
        }
      },
      "StatementWallet": {
        "type": "object",
        "required": ["chain", "address"],
        "properties": {
          "chain": { "type": "string", "enum": ["solana", "robinhood", "base", "ethereum", "bnb"] },
          "address": { "type": "string" }
        }
      },
      "StatementPosition": {
        "type": "object",
        "required": ["chain", "wallet", "block", "min_slot", "max_slot"],
        "properties": {
          "chain": { "type": "string", "enum": ["solana", "robinhood", "base", "ethereum", "bnb"] },
          "wallet": { "type": "string" },
          "block": { "type": ["integer", "null"], "minimum": 0 },
          "min_slot": { "type": ["integer", "null"], "minimum": 0 },
          "max_slot": { "type": ["integer", "null"], "minimum": 0 }
        }
      },
      "StatementAsset": {
        "type": "object",
        "required": ["wallet", "chain", "contract", "ticker", "issuer", "issuer_match", "balance", "decimals", "slot", "powers_summary"],
        "properties": {
          "wallet": { "type": "string" },
          "chain": { "type": "string", "enum": ["solana", "robinhood", "base", "ethereum", "bnb"] },
          "contract": { "type": "string" },
          "ticker": { "type": "string" },
          "issuer": { "type": "string" },
          "issuer_match": { "type": "boolean" },
          "balance": { "type": "string", "pattern": "^[0-9]+$", "description": "Raw on-chain token units; apply decimals separately." },
          "decimals": { "type": "integer", "minimum": 0, "maximum": 255 },
          "slot": { "type": ["integer", "null"], "minimum": 0 },
          "powers_observed_at": { "type": "string", "format": "date-time" },
          "powers_block": { "type": "integer", "minimum": 0 },
          "powers_slot": { "type": "integer", "minimum": 0 },
          "powers_summary": { "$ref": "#/components/schemas/StatementPowersSummary" }
        }
      },
      "StatementPowersSummary": {
        "type": "object",
        "required": ["can_seize", "can_block", "can_change_rules", "unavailable"],
        "properties": {
          "can_seize": { "type": "array", "items": { "$ref": "#/components/schemas/PowerReason" } },
          "can_block": { "type": "array", "items": { "$ref": "#/components/schemas/PowerReason" } },
          "can_change_rules": { "type": "array", "items": { "$ref": "#/components/schemas/PowerReason" } },
          "unavailable": { "type": "array", "items": { "$ref": "#/components/schemas/PowerReason" } }
        }
      },
      "McpServerCard": {
        "type": "object",
        "required": ["name", "description", "serverInfo", "remotes", "tools", "website", "repository", "readOnly", "readOnlyStatement"],
        "properties": {
          "name": { "type": "string" },
          "description": { "type": "string" },
          "serverInfo": {
            "type": "object",
            "required": ["name", "version"],
            "properties": { "name": { "type": "string" }, "version": { "type": "string" } }
          },
          "remotes": {
            "type": "array",
            "items": {
              "type": "object",
              "required": ["type", "url"],
              "properties": {
                "type": { "const": "streamable-http" },
                "url": { "type": "string", "format": "uri" }
              }
            }
          },
          "tools": {
            "type": "array",
            "items": {
              "type": "object",
              "required": ["name", "title", "description"],
              "properties": {
                "name": { "type": "string" },
                "title": { "type": "string" },
                "description": { "type": "string" }
              }
            }
          },
          "website": { "type": "string", "format": "uri" },
          "repository": { "type": "string", "format": "uri" },
          "readOnly": { "type": "boolean", "const": true },
          "readOnlyStatement": { "type": "string" }
        }
      }
    }
  }
}"##;

    static DOCUMENT_VALUE: LazyLock<Value> = LazyLock::new(|| {
        let mut document: Value =
            serde_json::from_str(DOCUMENT).expect("the embedded OpenAPI document is valid JSON");
        let tools = super::super::mcp::tool_table()
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
        let card_example = &mut document["paths"]["/.well-known/mcp/server-card.json"]["get"]["responses"]
            ["200"]["content"]["application/json"]["example"];
        card_example["tools"] = json!(tools);
        card_example["description"] = json!(super::super::mcp::SERVER_CARD_DESCRIPTION);
        card_example["readOnlyStatement"] = json!(super::super::mcp::READ_ONLY_STATEMENT);
        document["components"]["schemas"]["ApiError"] = json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["error", "detail"],
            "properties": {
                "error": { "type": "string", "description": "Short error summary." },
                "detail": { "type": "string", "description": "Readable explanation of the invalid request." }
            }
        });
        for (path, method) in [
            ("/api/leaderboard", "get"),
            ("/api/stats", "get"),
            ("/api/prices", "get"),
            ("/api/check/{address}", "get"),
            ("/api/guard", "post"),
            ("/api/guard/{address}", "get"),
            ("/api/powers/{address}", "get"),
            ("/api/wallet", "post"),
            ("/api/statement", "post"),
            ("/verify", "post"),
        ] {
            let operation = &mut document["paths"][path][method];
            let previous = operation["responses"]["400"]["description"]
                .as_str()
                .unwrap_or("Invalid request.")
                .to_owned();
            operation["responses"]["400"] = json!({
                "description": previous,
                "content": {
                    "application/json": {
                        "schema": { "$ref": "#/components/schemas/ApiError" },
                        "example": {
                            "error": "Bad request",
                            "detail": "The request is missing or contains invalid fields."
                        }
                    }
                }
            });
        }
        document
    });

    fn parsed_document() -> &'static Value {
        &DOCUMENT_VALUE
    }

    pub(crate) fn render_api_reference_html() -> String {
        let document = parsed_document();
        let paths = document["paths"].as_object().expect("OpenAPI paths object");
        let mut groups: BTreeMap<String, Vec<(String, String, String)>> = BTreeMap::new();
        for (path, path_item) in paths {
            let Some(operations) = path_item.as_object() else { continue };
            for (method, operation) in operations {
                if !matches!(
                    method.as_str(),
                    "get" | "post" | "put" | "patch" | "delete" | "options" | "head"
                ) {
                    continue;
                }
                let group = operation
                    .get("tags")
                    .and_then(Value::as_array)
                    .and_then(|tags| tags.first())
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(|| path_group(path));
                let card = render_operation(path, method, path_item, operation, document);
                groups.entry(group).or_default().push((path.clone(), method.clone(), card));
            }
        }

        let mut sections = String::new();
        for (group, mut operations) in groups {
            operations
                .sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
            let cards = operations.into_iter().map(|(_, _, card)| card).collect::<String>();
            sections.push_str(&format!(
                "<section class=\"api-route-group\"><h2>{}</h2><div class=\"api-operation-list\">{cards}</div></section>",
                escape_html(&group)
            ));
        }
        format!("<div class=\"api-reference\">{sections}</div>")
    }

    fn render_operation(
        path: &str,
        method: &str,
        path_item: &Value,
        operation: &Value,
        document: &Value,
    ) -> String {
        let upper_method = method.to_ascii_uppercase();
        let method_class = method.to_ascii_lowercase();
        let anchor = operation_anchor(path, method);
        let summary = operation.get("summary").and_then(Value::as_str).unwrap_or("API operation");
        let parameters = parameters(path_item, operation);
        let parameters_html = if parameters.is_empty() {
            String::new()
        } else {
            let rows = parameters
                .iter()
                .map(|parameter| {
                    let name = parameter.get("name").and_then(Value::as_str).unwrap_or("");
                    let location = parameter.get("in").and_then(Value::as_str).unwrap_or("");
                    let required =
                        parameter.get("required").and_then(Value::as_bool).unwrap_or(false);
                    let schema = parameter.get("schema").unwrap_or(&Value::Null);
                    let description =
                        parameter.get("description").and_then(Value::as_str).unwrap_or("");
                    format!(
                        "<tr><td><code>{}</code></td><td>{}</td><td>{}</td><td>{}</td></tr>",
                        escape_html(name),
                        escape_html(location),
                        if required { "Required" } else { "Optional" },
                        escape_html(&format!(
                            "{}{}",
                            schema_type(schema),
                            if description.is_empty() {
                                String::new()
                            } else {
                                format!(" — {description}")
                            }
                        ))
                    )
                })
                .collect::<String>();
            format!(
                "<section class=\"api-parameters\"><h3>Parameters</h3><div class=\"table-wrap\"><table><thead><tr><th>Name</th><th>In</th><th>Required</th><th>Type / description</th></tr></thead><tbody>{rows}</tbody></table></div></section>"
            )
        };
        let request_media = operation
            .get("requestBody")
            .and_then(|body| body.get("content"))
            .and_then(|content| content.get("application/json"));
        let request_schema = request_media.and_then(|media| media.get("schema"));
        let request =
            request_examples(path, method, &parameters, request_media, request_schema, document);
        let (response_status, response) = response_examples(operation, document);
        let request_html = render_example_blocks(&request, render_request_example);
        let response_html = render_example_blocks(&response, render_example_body);
        let response_status_class = if response_status == "200" {
            "api-response-status api-response-status-200"
        } else {
            "api-response-status"
        };
        let response_label = format!(
            "Response · <span class=\"{response_status_class}\">{}</span>",
            escape_html(&response_status)
        );
        format!(
            r#"<article class="api-operation" id="{anchor}"><header class="api-operation-heading"><span class="method-badge method-{method_class}">{upper_method}</span><code>{}</code></header><p class="api-operation-summary">{}</p>{parameters_html}<div class="api-examples"><section class="api-code-block"><header class="api-code-header"><span>Request</span><span class="method-badge method-{method_class}">{upper_method}</span></header>{request_html}</section><section class="api-code-block"><header class="api-code-header"><span>{response_label}</span><span class="method-badge method-{method_class}">{upper_method}</span></header>{response_html}</section></div></article>"#,
            escape_html(path),
            escape_html(summary)
        )
    }

    fn parameters<'a>(path_item: &'a Value, operation: &'a Value) -> Vec<&'a Value> {
        path_item
            .get("parameters")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .chain(operation.get("parameters").and_then(Value::as_array).into_iter().flatten())
            .collect()
    }

    fn media_examples(
        media: Option<&Value>,
        schema: Option<&Value>,
        document: &Value,
    ) -> Vec<(Option<String>, Value)> {
        if let Some(examples) =
            media.and_then(|media| media.get("examples")).and_then(Value::as_object)
        {
            let examples = examples
                .iter()
                .filter_map(|(name, example)| {
                    let value = example.get("value")?.clone();
                    let label =
                        example.get("summary").and_then(Value::as_str).unwrap_or(name).to_owned();
                    Some((Some(label), value))
                })
                .collect::<Vec<_>>();
            if !examples.is_empty() {
                return examples;
            }
        }
        if let Some(example) = media.and_then(|media| media.get("example")) {
            return vec![(None, example.clone())];
        }
        if let Some(schema) = schema {
            return vec![(None, minimal_instance(schema, document, None, 0))];
        }
        Vec::new()
    }

    fn request_examples(
        path: &str,
        method: &str,
        parameters: &[&Value],
        request_media: Option<&Value>,
        request_schema: Option<&Value>,
        document: &Value,
    ) -> Vec<(Option<String>, String)> {
        let method = method.to_ascii_uppercase();
        let mut example_path = path.to_owned();
        for parameter in parameters {
            if parameter.get("in").and_then(Value::as_str) == Some("path") {
                let name = parameter.get("name").and_then(Value::as_str).unwrap_or("");
                example_path =
                    example_path.replace(&format!("{{{name}}}"), &path_parameter_example(name));
            }
        }
        let query = parameters
            .iter()
            .filter(|parameter| parameter.get("in").and_then(Value::as_str) == Some("query"))
            .filter(|parameter| parameter.get("required").and_then(Value::as_bool).unwrap_or(false))
            .map(|parameter| {
                let name = parameter.get("name").and_then(Value::as_str).unwrap_or("");
                let schema = parameter.get("schema").unwrap_or(&Value::Null);
                format!("{name}={}", query_parameter_example(name, schema))
            })
            .collect::<Vec<_>>();
        let query = if query.is_empty() { String::new() } else { format!("?{}", query.join("&")) };
        let url = format!("https://qed.web3-energy.com{example_path}{query}");
        if request_schema.is_some() {
            media_examples(request_media, request_schema, document)
                .into_iter()
                .map(|(label, value)| {
                    let body = example_json(&value);
                    (label, format!("{method} {url}\nContent-Type: application/json\n\n{body}"))
                })
                .collect()
        } else {
            vec![(None, format!("{method} {url}"))]
        }
    }

    fn response_examples(
        operation: &Value,
        document: &Value,
    ) -> (String, Vec<(Option<String>, String)>) {
        let Some(responses) = operation.get("responses").and_then(Value::as_object) else {
            return ("—".to_owned(), vec![(None, "No response documented".to_owned())]);
        };
        let (status, response) = responses
            .get("200")
            .map(|response| ("200", response))
            .or_else(|| {
                responses
                    .iter()
                    .find(|(status, _)| status.starts_with('2'))
                    .map(|(status, response)| (status.as_str(), response))
            })
            .or_else(|| {
                responses.iter().next().map(|(status, response)| (status.as_str(), response))
            })
            .expect("OpenAPI operation has a response");
        let description = response.get("description").and_then(Value::as_str).unwrap_or("");
        if let Some(content) = response.get("content").and_then(Value::as_object) {
            let media = content.get("application/json").or_else(|| content.values().next());
            if let Some(media) = media {
                let examples = media_examples(Some(media), media.get("schema"), document)
                    .into_iter()
                    .map(|(label, example)| (label, example_json(&example)))
                    .collect::<Vec<_>>();
                if !examples.is_empty() {
                    return (status.to_owned(), examples);
                }
            }
        }
        let description = if description.is_empty() {
            "No response body documented".to_owned()
        } else {
            description.to_owned()
        };
        (status.to_owned(), vec![(None, description)])
    }
    const OMITTED_ARRAY_ITEM: &str = "__QED_OMITTED_ARRAY_ITEMS__";
    const OMITTED_ARRAY_TOKEN: &str = "\"__QED_OMITTED_ARRAY_ITEMS__\"";

    fn example_json(value: &Value) -> String {
        let mut example = value.clone();
        truncate_example_arrays(&mut example);
        serde_json::to_string_pretty(&example).unwrap_or_else(|_| example.to_string())
    }

    fn truncate_example_arrays(value: &mut Value) {
        match value {
            Value::Array(items) => {
                if items.len() > 2 {
                    items.truncate(2);
                    items.push(Value::String(OMITTED_ARRAY_ITEM.to_owned()));
                }
                for item in items {
                    truncate_example_arrays(item);
                }
            }
            Value::Object(properties) => {
                for property in properties.values_mut() {
                    truncate_example_arrays(property);
                }
            }
            _ => {}
        }
    }

    fn render_request_example(request: &str) -> String {
        let Some((headers, body)) = request.split_once("\n\n") else {
            return escape_html(request);
        };
        let mut html = String::with_capacity(request.len().saturating_add(64));
        append_html_escaped(&mut html, headers);
        html.push_str("\n\n");
        html.push_str(&highlight_json(body));
        html
    }

    fn render_example_blocks(
        examples: &[(Option<String>, String)],
        render: fn(&str) -> String,
    ) -> String {
        let mut html = String::new();
        for (label, body) in examples {
            if let Some(label) = label {
                html.push_str("<p class=\"api-example-label\">");
                append_html_escaped(&mut html, label);
                html.push_str("</p>");
            }
            html.push_str("<pre class=\"code-block\"><code>");
            html.push_str(&render(body));
            html.push_str("</code></pre>");
        }
        html
    }

    fn render_example_body(body: &str) -> String {
        let trimmed = body.trim_start();
        if trimmed.starts_with('{') || trimmed.starts_with('[') {
            highlight_json(body)
        } else {
            escape_html(body)
        }
    }

    fn highlight_json(json: &str) -> String {
        let bytes = json.as_bytes();
        let mut html = String::with_capacity(json.len().saturating_add(json.len() / 3));
        let mut index = 0;
        while index < bytes.len() {
            match bytes[index] {
                b'"' => {
                    let start = index;
                    index += 1;
                    while index < bytes.len() {
                        match bytes[index] {
                            b'\\' => index = (index + 2).min(bytes.len()),
                            b'"' => {
                                index += 1;
                                break;
                            }
                            _ => index += 1,
                        }
                    }
                    let token = &json[start..index];
                    if token == OMITTED_ARRAY_TOKEN {
                        html.push_str("<span class=\"json-ellipsis\">…</span>");
                    } else {
                        let mut next = index;
                        while bytes.get(next).is_some_and(u8::is_ascii_whitespace) {
                            next += 1;
                        }
                        let class =
                            if bytes.get(next) == Some(&b':') { "json-key" } else { "json-string" };
                        html.push_str("<span class=\"");
                        html.push_str(class);
                        html.push_str("\">");
                        append_html_escaped(&mut html, token);
                        html.push_str("</span>");
                    }
                }
                b'-' | b'0'..=b'9' => {
                    let start = index;
                    index += 1;
                    while bytes.get(index).is_some_and(|byte| {
                        byte.is_ascii_digit() || matches!(*byte, b'.' | b'e' | b'E' | b'+' | b'-')
                    }) {
                        index += 1;
                    }
                    html.push_str("<span class=\"json-value\">");
                    append_html_escaped(&mut html, &json[start..index]);
                    html.push_str("</span>");
                }
                _ => {
                    let literal = ["true", "false", "null"]
                        .into_iter()
                        .find(|literal| json[index..].starts_with(literal));
                    if let Some(literal) = literal {
                        html.push_str("<span class=\"json-value\">");
                        html.push_str(literal);
                        html.push_str("</span>");
                        index += literal.len();
                    } else {
                        let character = json[index..].chars().next().expect("valid JSON UTF-8");
                        append_html_escaped(&mut html, &json[index..index + character.len_utf8()]);
                        index += character.len_utf8();
                    }
                }
            }
        }
        html
    }

    fn append_html_escaped(target: &mut String, value: &str) {
        for character in value.chars() {
            target.push_str(match character {
                '&' => "&amp;",
                '<' => "&lt;",
                '>' => "&gt;",
                '"' => "&quot;",
                '\'' => "&#39;",
                _ => {
                    target.push(character);
                    continue;
                }
            });
        }
    }

    fn minimal_instance(
        schema: &Value,
        document: &Value,
        field_name: Option<&str>,
        depth: usize,
    ) -> Value {
        if depth > 8 {
            return Value::Null;
        }
        if let Some(value) = schema.get("example").or_else(|| schema.get("const")) {
            return value.clone();
        }
        if let Some(values) = schema.get("enum").and_then(Value::as_array)
            && let Some(value) = values.first()
        {
            return value.clone();
        }
        if let Some(choices) = schema.get("oneOf").and_then(Value::as_array)
            && let Some(choice) = choices.first()
        {
            return minimal_instance(choice, document, field_name, depth + 1);
        }
        if let Some(reference) = schema.get("$ref").and_then(Value::as_str)
            && let Some(target) = resolve_reference(reference, document)
        {
            return minimal_instance(target, document, field_name, depth + 1);
        }
        let schema_type = schema
            .get("type")
            .and_then(|kind| {
                kind.as_str().or_else(|| {
                    kind.as_array().and_then(|types| {
                        types
                            .iter()
                            .find_map(|value| value.as_str().filter(|value| *value != "null"))
                    })
                })
            })
            .unwrap_or_else(
                || if schema.get("properties").is_some() { "object" } else { "string" },
            );
        match schema_type {
            "object" => {
                let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
                    return Value::Object(Map::new());
                };
                let required: Vec<String> = schema
                    .get("required")
                    .and_then(Value::as_array)
                    .map(|required| {
                        required.iter().filter_map(Value::as_str).map(str::to_owned).collect()
                    })
                    .unwrap_or_else(|| properties.keys().take(3).cloned().collect());
                let mut object = Map::new();
                for name in required {
                    if let Some(property) = properties.get(&name) {
                        object.insert(
                            name.clone(),
                            minimal_instance(property, document, Some(&name), depth + 1),
                        );
                    }
                }
                Value::Object(object)
            }
            "array" => {
                let Some(items) = schema.get("items") else { return Value::Array(Vec::new()) };
                let item_name = match field_name {
                    Some("wallets") => Some("address"),
                    Some("chains") => Some("chain"),
                    _ => None,
                };
                Value::Array(vec![minimal_instance(items, document, item_name, depth + 1)])
            }
            "string" => Value::String(string_example(field_name, schema)),
            "integer" => {
                Value::Number(schema.get("minimum").and_then(Value::as_i64).unwrap_or(1).into())
            }
            "number" => Value::Number(
                serde_json::Number::from_f64(
                    schema.get("minimum").and_then(Value::as_f64).unwrap_or(1.0),
                )
                .unwrap_or_else(|| 1.into()),
            ),
            "boolean" => Value::Bool(false),
            _ => Value::Null,
        }
    }

    fn resolve_reference<'a>(reference: &str, document: &'a Value) -> Option<&'a Value> {
        let name = reference.strip_prefix("#/components/schemas/")?;
        document.get("components")?.get("schemas")?.get(name)
    }

    fn string_example(field_name: Option<&str>, schema: &Value) -> String {
        match field_name {
            Some("address") | Some("wallet") => "YOUR_ADDRESS".to_owned(),
            Some("ticker") => "NVDA".to_owned(),
            Some("id") => "DOCUMENT_ID".to_owned(),
            Some("wallets") => "YOUR_WALLET_ADDRESS".to_owned(),
            Some("chain") => "base".to_owned(),
            _ if schema.get("format").and_then(Value::as_str) == Some("date-time") => {
                "2026-01-01T00:00:00Z".to_owned()
            }
            _ if schema
                .get("pattern")
                .and_then(Value::as_str)
                .is_some_and(|p| p.contains("{64}")) =>
            {
                "0".repeat(64)
            }
            _ => "example".to_owned(),
        }
    }

    fn schema_type(schema: &Value) -> String {
        if let Some(kind) = schema.get("type").and_then(Value::as_str) {
            return kind.to_owned();
        }
        if let Some(types) = schema.get("type").and_then(Value::as_array) {
            return types.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" | ");
        }
        if schema.get("oneOf").is_some() {
            return "oneOf".to_owned();
        }
        if schema.get("$ref").is_some() {
            return "object".to_owned();
        }
        "unknown".to_owned()
    }

    fn path_parameter_example(name: &str) -> &'static str {
        match name {
            "address" => "YOUR_ADDRESS",
            "id" => "DOCUMENT_ID",
            "chain" => "base",
            _ => "EXAMPLE",
        }
    }

    fn query_parameter_example(name: &str, schema: &Value) -> String {
        if let Some(value) = schema
            .get("enum")
            .and_then(Value::as_array)
            .and_then(|values| values.first())
            .and_then(Value::as_str)
        {
            return value.to_owned();
        }
        match name {
            "ids" => "solana:POOL_ID".to_owned(),
            "ticker" => "NVDA".to_owned(),
            "page" => "1".to_owned(),
            "per" => "10".to_owned(),
            _ => "example".to_owned(),
        }
    }

    fn path_group(path: &str) -> String {
        let parts = path.trim_start_matches('/').split('/').collect::<Vec<_>>();
        match parts.as_slice() {
            ["api", area, ..] => match *area {
                "attest" => "Attestations".to_owned(),
                "check" => "Checks".to_owned(),
                "guard" => "Guard".to_owned(),
                "leaderboard" => "Leaderboard".to_owned(),
                "pools" => "Pools".to_owned(),
                "powers" => "Token powers".to_owned(),
                "prices" => "Prices".to_owned(),
                "registry" => "Registry".to_owned(),
                "statement" => "Statements".to_owned(),
                "status" => "Status".to_owned(),
                "wallet" => "Wallet".to_owned(),
                _ => "API".to_owned(),
            },
            [".well-known", ..] => "Discovery".to_owned(),
            ["mcp", ..] => "MCP".to_owned(),
            ["verify", ..] => "Verification".to_owned(),
            ["statements", ..] => "Statements".to_owned(),
            ["healthz", ..] => "Health".to_owned(),
            _ => "Other".to_owned(),
        }
    }

    fn operation_anchor(path: &str, method: &str) -> String {
        let path_slug = path
            .trim_matches('/')
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() { character.to_ascii_lowercase() } else { '-' }
            })
            .collect::<String>();
        format!("api-{}-{path_slug}", method.to_ascii_lowercase())
    }

    fn escape_html(value: &str) -> String {
        value
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&#39;")
    }

    pub(crate) async fn document() -> Response {
        let body =
            serde_json::to_vec(parsed_document()).expect("OpenAPI document remains serializable");
        ([(header::CONTENT_TYPE, "application/json; charset=utf-8")], body).into_response()
    }

    #[cfg(test)]
    mod tests {
        use super::parsed_document;
        use serde_json::Value;

        #[test]
        fn openapi_documents_powers_guard_and_server_card_contracts() {
            let document = parsed_document();
            let paths = &document["paths"];
            let verify_post = &paths["/verify"]["post"];
            let powers_get = &paths["/api/powers/{address}"]["get"];
            let guard_get = &paths["/api/guard/{address}"]["get"];
            let guard_post = &paths["/api/guard"]["post"];
            let stats_json_get = &paths["/stats.json"]["get"];
            let stats_csv_get = &paths["/stats.csv"]["get"];
            let api_error = &document["components"]["schemas"]["ApiError"];
            assert_eq!(api_error["required"], serde_json::json!(["error", "detail"]));
            for (path, method) in [
                ("/api/leaderboard", "get"),
                ("/api/stats", "get"),
                ("/api/prices", "get"),
                ("/api/check/{address}", "get"),
                ("/api/guard", "post"),
                ("/api/guard/{address}", "get"),
                ("/api/powers/{address}", "get"),
                ("/api/wallet", "post"),
                ("/api/statement", "post"),
                ("/verify", "post"),
            ] {
                assert_eq!(
                    paths[path][method]["responses"]["400"]["content"]["application/json"]["schema"]
                        ["$ref"],
                    "#/components/schemas/ApiError"
                );
            }
            assert_eq!(
                stats_json_get["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
                "#/components/schemas/StatsDocument"
            );
            assert_eq!(
                stats_csv_get["responses"]["200"]["content"]["text/csv"]["schema"]["type"],
                "string"
            );
            assert_eq!(
                paths["/api/stats"]["get"]["responses"]["200"]["content"]["application/json"]["schema"]
                    ["$ref"],
                "#/components/schemas/StatsApiPage"
            );
            assert_eq!(
                document["components"]["schemas"]["MarketDataSource"]["enum"],
                serde_json::json!(["dexscreener", "geckoterminal"])
            );
            for schema in ["LeaderboardEntry", "CatalogAbsentToken", "UnsupportedCatalogCandidate"]
            {
                assert_eq!(
                    document["components"]["schemas"][schema]["properties"]["source"]["$ref"],
                    "#/components/schemas/MarketDataSource"
                );
            }
            assert_eq!(
                document["components"]["schemas"]["PricePoint"]["properties"]["source"]["$ref"],
                "#/components/schemas/MarketDataSource"
            );
            assert_eq!(
                document["components"]["schemas"]["StatsDocument"]["properties"]["kind"]["const"],
                "stats"
            );
            assert!(
                document["components"]["schemas"]["StatsDocument"]["properties"]["reducer_inputs"]
                    .is_object()
            );
            assert!(
                document["components"]["schemas"]["StatsReducerInputs"]["properties"]
                    ["active_registry"]
                    .is_object()
            );
            assert!(
                document["components"]["schemas"]["LeaderboardStats"]["properties"]
                    ["publisher_catalog_watch"]
                    .is_object()
            );
            assert!(
                document["components"]["schemas"]["PublisherCatalogWatch"]["required"]
                    .as_array()
                    .unwrap()
                    .contains(&serde_json::json!("catalog_absent_tokens_truncated"))
            );
            assert!(
                document["components"]["schemas"]["PublisherCatalogWatch"]["required"]
                    .as_array()
                    .unwrap()
                    .contains(&serde_json::json!("currently_flagged_last_7_days"))
            );
            assert!(
                document["components"]["schemas"]["PublisherCatalogWatch"]["required"]
                    .as_array()
                    .unwrap()
                    .contains(&serde_json::json!("official_on_unsupported_chain"))
            );
            assert_eq!(
                document["components"]["schemas"]["PublisherCatalogWatch"]["properties"]["currently_flagged_last_7_days"]
                    ["type"],
                "integer"
            );
            assert_eq!(
                document["components"]["schemas"]["PublisherCatalogWatch"]["properties"]["official_on_unsupported_chain"]
                    ["type"],
                "integer"
            );
            assert_eq!(
                document["components"]["schemas"]["PublisherCatalogWatch"]["properties"]["catalog_absent_tokens_truncated"]
                    ["type"],
                "boolean"
            );
            assert!(
                document["components"]["schemas"]["PublisherCatalogWatch"]["properties"]
                    .get("catalog_absent_tokens")
                    .is_none(),
                "catalog rows are delivered only as signed reducer inputs"
            );
            assert_eq!(
                document["components"]["schemas"]["ImpostorSnapshot"]["properties"]["entries"]["maxItems"],
                128
            );
            assert_eq!(
                document["components"]["schemas"]["ImpostorSnapshot"]["properties"]["unsupported_candidates"]
                    ["maxItems"],
                256
            );
            assert_eq!(
                guard_post["requestBody"]["content"]["application/json"]["schema"]["required"],
                serde_json::json!(["address", "chain"])
            );
            assert!(
                guard_post["description"]
                    .as_str()
                    .unwrap()
                    .contains("keeps the wallet out of the request URL")
            );
            assert!(
                document["components"]["schemas"]["GuardDocument"]["required"]
                    .as_array()
                    .unwrap()
                    .contains(&serde_json::json!("subject_type"))
            );
            assert!(
                document["components"]["schemas"]["GuardReason"]["properties"]["code"]["enum"]
                    .as_array()
                    .unwrap()
                    .contains(&serde_json::json!("registry_removed"))
            );
            assert!(
                document["components"]["schemas"]["GuardReason"]["properties"]["code"]["enum"]
                    .as_array()
                    .unwrap()
                    .contains(&serde_json::json!("publisher_metadata_unavailable"))
            );
            assert!(
                document["components"]["schemas"]["GuardReason"]["properties"]["code"]["enum"]
                    .as_array()
                    .unwrap()
                    .contains(&serde_json::json!("claims_unpublished_publisher_product"))
            );
            assert_eq!(
                document["components"]["schemas"]["GuardIdentity"]["properties"]["status"]["enum"],
                serde_json::json!([
                    "match",
                    "mismatch",
                    "no_publisher",
                    "registry_stale",
                    "registry_removed"
                ])
            );
            assert!(
                document["components"]["schemas"]["PowersRecord"]["properties"]["token_paused"]
                    .is_object()
            );
            assert_eq!(guard_get["parameters"][1]["name"], "chain");
            assert_eq!(guard_get["parameters"][1]["required"], true);
            assert_eq!(
                guard_get["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
                "#/components/schemas/GuardDocument"
            );
            assert!(
                guard_get["description"].as_str().unwrap().contains(
                    "It does not prove backing, custody, reserves, solvency, safety, price, liquidity, or endorsement."
                )
            );
            assert!(powers_get.is_object());
            assert_eq!(powers_get["parameters"][1]["name"], "chain");
            assert_eq!(
                powers_get["responses"]["200"]["content"]["application/json"]["schema"]["oneOf"]
                    .as_array()
                    .unwrap()
                    .len(),
                2
            );
            assert_eq!(
                document["components"]["schemas"]["PowersRecord"]["properties"]["source_verified_subject"]
                    ["enum"],
                serde_json::json!(["token_program", "contract", "implementation"])
            );
            assert_eq!(
                document["components"]["schemas"]["PowersRecordSet"]["required"],
                serde_json::json!(["records"])
            );
            assert_eq!(
                document["components"]["schemas"]["PowersRecordSet"]["properties"]["records"]["items"]
                    ["$ref"],
                "#/components/schemas/PowersRecord"
            );
            assert!(
                document["components"]["schemas"]["JsonRpcResultResponse"]["properties"]["result"]
                    ["properties"]["structuredContent"]
                    .is_object()
            );
            assert_eq!(
                paths["/.well-known/mcp/server-card.json"]["get"]["responses"]["200"]["content"]["application/json"]
                    ["schema"]["$ref"],
                "#/components/schemas/McpServerCard"
            );
            let card_example = &paths["/.well-known/mcp/server-card.json"]["get"]["responses"]["200"]
                ["content"]["application/json"]["example"];
            assert_eq!(
                card_example["description"],
                super::super::super::mcp::SERVER_CARD_DESCRIPTION
            );
            assert_eq!(
                card_example["readOnlyStatement"],
                super::super::super::mcp::READ_ONLY_STATEMENT
            );
            let tools = card_example["tools"].as_array().expect("MCP card tool list");
            assert_eq!(
                tools.iter().filter_map(|tool| tool["name"].as_str()).collect::<Vec<_>>(),
                [
                    "qed_check",
                    "qed_powers",
                    "qed_wallet",
                    "qed_statement",
                    "qed_registry_lookup",
                    "qed_verify",
                    "qed_guard"
                ]
            );
            let declared_tools = super::super::super::mcp::tool_table();
            let declared_tools = declared_tools.as_array().expect("MCP tool definitions");
            assert_eq!(tools.len(), declared_tools.len());
            for (card_tool, declared_tool) in tools.iter().zip(declared_tools) {
                for field in ["name", "title", "description"] {
                    assert_eq!(card_tool[field], declared_tool[field]);
                }
            }
            let statement_post = &paths["/api/statement"]["post"];
            assert_eq!(
                statement_post["requestBody"]["content"]["application/json"]["schema"]["$ref"],
                "#/components/schemas/StatementRequest"
            );
            assert_eq!(
                statement_post["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
                "#/components/schemas/Statement"
            );
            assert!(paths["/statements/{id}"]["get"]["responses"]["200"].is_object());
            assert!(
                paths["/statements/{id}/download.csv"]["get"]["responses"]["200"]["content"]
                    ["text/csv"]
                    .is_object()
            );
            assert!(paths["/statements/{id}/verify"]["get"]["responses"]["200"].is_object());
            assert_eq!(
                paths["/statements/{id}/recheck"]["post"]["responses"]["303"]["description"],
                "Redirect to the new statement page with a comparison"
            );
            assert!(paths["/v/{id}/verify"]["get"]["responses"]["200"].is_object());
            assert_eq!(
                paths["/api/statement/{id}"]["get"]["responses"]["200"]["content"]["application/json"]
                    ["schema"]["$ref"],
                "#/components/schemas/Statement"
            );
            assert_eq!(
                verify_post["requestBody"]["content"]["application/json"]["schema"]["oneOf"]
                    .as_array()
                    .unwrap()
                    .len(),
                4
            );
            assert_eq!(
                verify_post["requestBody"]["content"]["application/json"]["schema"]["oneOf"][1]["$ref"],
                "#/components/schemas/Statement"
            );
            assert_eq!(
                verify_post["requestBody"]["content"]["application/json"]["schema"]["oneOf"][2]["$ref"],
                "#/components/schemas/GuardDocument"
            );
            assert_eq!(
                verify_post["requestBody"]["content"]["application/json"]["schema"]["oneOf"][3]["$ref"],
                "#/components/schemas/StatsDocument"
            );
            assert_eq!(
                verify_post["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
                "#/components/schemas/VerifyResult"
            );
            assert_eq!(
                document["components"]["schemas"]["Statement"]["properties"]["kind"]["const"],
                "statement"
            );
            assert_eq!(
                document["components"]["schemas"]["GuardDocument"]["properties"]["kind"]["const"],
                "guard"
            );
            assert_eq!(
                document["components"]["schemas"]["StatementPosition"]["required"],
                serde_json::json!(["chain", "wallet", "block", "min_slot", "max_slot"])
            );
            assert_eq!(
                document["components"]["schemas"]["StatementAsset"]["properties"]["slot"]["type"],
                serde_json::json!(["integer", "null"])
            );
            assert_eq!(
                document["components"]["schemas"]["Statement"]["properties"]["reads_truncated"]["type"],
                "boolean"
            );
            assert_eq!(
                document["components"]["schemas"]["StatementAsset"]["properties"]["powers_observed_at"]
                    ["type"],
                "string"
            );
            assert_eq!(
                document["components"]["schemas"]["StatementRequest"]["properties"]["label"]["type"],
                "string"
            );
            assert_eq!(
                document["components"]["schemas"]["Statement"]["properties"]["label"]["type"],
                "string"
            );
            assert_eq!(
                document["components"]["schemas"]["StatementRequest"]["required"],
                serde_json::json!(["wallets", "chains"])
            );
            let chain_slugs = serde_json::json!(["solana", "robinhood", "base", "ethereum", "bnb"]);
            assert_eq!(
                document["components"]["schemas"]["StatementWallet"]["properties"]["chain"]["enum"],
                chain_slugs
            );
            assert_eq!(
                document["components"]["schemas"]["GuardDocument"]["properties"]["chain"]["enum"],
                chain_slugs
            );
            assert_eq!(
                document["components"]["schemas"]["Statement"]["properties"]["signature"]["contentEncoding"],
                "base64"
            );
            let mcp_post = &paths["/mcp"]["post"];
            assert!(
                mcp_post["requestBody"]["content"]["application/json"]["schema"]["oneOf"]
                    .is_array()
            );
            assert_eq!(
                mcp_post["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
                "#/components/schemas/JsonRpcResponse"
            );
        }

        #[test]
        fn every_json_media_response_and_request_has_a_json_example() {
            let document = parsed_document();
            let paths = document["paths"].as_object().expect("OpenAPI paths");
            for (path, path_item) in paths {
                let Some(operations) = path_item.as_object() else { continue };
                for (method, operation) in operations {
                    if !["get", "post", "put", "patch", "delete"].contains(&method.as_str()) {
                        continue;
                    }
                    let context = format!("{method} {path}");
                    if let Some(media) = operation
                        .get("requestBody")
                        .and_then(|body| body.get("content"))
                        .and_then(|content| content.get("application/json"))
                    {
                        assert_json_media_examples(media, &format!("{context} request"));
                    }
                    for (status, response) in
                        operation.get("responses").and_then(Value::as_object).into_iter().flatten()
                    {
                        if let Some(media) = response
                            .get("content")
                            .and_then(|content| content.get("application/json"))
                        {
                            assert_json_media_examples(
                                media,
                                &format!("{context} response {status}"),
                            );
                        }
                    }
                }
            }
        }

        #[test]
        fn model_backed_response_examples_deserialize_and_signed_documents_verify() {
            let document = parsed_document();
            let paths = &document["paths"];
            let registry: Vec<crate::domain::registry::Entry> = serde_json::from_value(
                paths["/api/registry"]["get"]["responses"]["200"]["content"]["application/json"]
                    ["example"]
                    .clone(),
            )
            .expect("registry response example");
            assert_eq!(registry.len(), 2);

            let featured: Vec<crate::adapters::discovery::FeaturedPool> = serde_json::from_value(
                paths["/api/pools/featured"]["get"]["responses"]["200"]["content"]
                    ["application/json"]["example"]
                    .clone(),
            )
            .expect("featured response example");
            assert_eq!(featured[0].ticker.as_deref(), Some("TSLA"));

            let leaderboard: Vec<crate::adapters::discovery::LeaderboardEntry> =
                serde_json::from_value(
                    paths["/api/leaderboard"]["get"]["responses"]["200"]["content"]
                        ["application/json"]["example"]["entries"]
                        .clone(),
                )
                .expect("leaderboard response example");
            assert_eq!(leaderboard[0].ticker.as_deref(), Some("NVDA"));

            let prices: Vec<crate::adapters::discovery::PricePoint> =
                serde_json::from_value(
                    paths["/api/prices"]["get"]["responses"]["200"]["content"]["application/json"]
                        ["example"]["prices"]
                        .clone(),
                )
                .expect("price response example");
            assert_eq!(prices[0].pool, "known");

            let check: crate::domain::check::CheckResult = serde_json::from_value(
                paths["/api/check/{address}"]["get"]["responses"]["200"]["content"]
                    ["application/json"]["example"]
                    .clone(),
            )
            .expect("check response example");
            assert_eq!(check.input, check.pool.as_ref().expect("pool").pool);

            let powers: crate::domain::powers::PowersRecord = serde_json::from_value(
                paths["/api/powers/{address}"]["get"]["responses"]["200"]["content"]
                    ["application/json"]["example"]
                    .clone(),
            )
            .expect("powers response example");
            assert_eq!(powers.source_verified.as_str(), "none");

            let attestation: crate::domain::attestation::Attestation = serde_json::from_value(
                paths["/api/attest/{id}"]["get"]["responses"]["200"]["content"]["application/json"]
                    ["example"]
                    .clone(),
            )
            .expect("attestation response example");
            crate::domain::attestation::verify(&attestation).expect("signed attestation example");

            let statement: crate::domain::statement::Statement = serde_json::from_value(
                paths["/api/statement"]["post"]["responses"]["200"]["content"]["application/json"]
                    ["example"]
                    .clone(),
            )
            .expect("statement response example");
            crate::domain::statement::verify(&statement).expect("signed statement example");
            let guard: crate::domain::guard::GuardDocument = serde_json::from_value(
                paths["/api/guard/{address}"]["get"]["responses"]["200"]["content"]
                    ["application/json"]["example"]
                    .clone(),
            )
            .expect("Guard response example");
            assert_eq!(guard.kind, "guard");
            assert_eq!(guard.wallet_check, None);
        }

        #[test]
        fn api_reference_renders_each_verify_request_and_response_example() {
            let html = super::render_api_reference_html();
            for label in [
                "Signed attestation",
                "Signed wallet statement",
                "Attestation verification",
                "Statement verification",
                "Signed Guard document (schematic signature placeholders)",
                "Guard verification",
            ] {
                assert!(html.contains(label), "API reference is missing {label}");
            }
            assert!(
                html.contains("0e9596ff7ba53868e82291934a69a14cc67b3f1cd8625b87013199e128533ffc")
            );
            assert!(
                html.contains("fb3a9270f0c364be29ca410bbe5074e53b42df96970f9fc4a47d7c8d63cf0905")
            );
            assert!(!html.contains("Attestation JSON"));
        }

        fn assert_json_media_examples(media: &Value, context: &str) {
            let mut examples = Vec::new();
            if let Some(example) = media.get("example") {
                examples.push(("example", example));
            }
            if let Some(named_examples) = media.get("examples").and_then(Value::as_object) {
                for (name, example) in named_examples {
                    let value = example
                        .get("value")
                        .unwrap_or_else(|| panic!("{context} example {name} is missing a value"));
                    examples.push((name.as_str(), value));
                }
            }
            assert!(!examples.is_empty(), "{context} is missing a JSON example");
            for (name, example) in examples {
                assert!(
                    example.is_object() || example.is_array(),
                    "{context} example {name} is not a JSON object or array"
                );
            }
        }
    }
}

pub(crate) use discoverability::{
    api_docs, blog_feed, changelog_feed, llms, llms_full, validated_feed,
};
pub(crate) use legal::{imprint, privacy, terms};
pub(crate) use openapi::document;
