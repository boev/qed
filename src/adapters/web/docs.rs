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
- MCP endpoint: POST `{base}/mcp`; stateless Streamable HTTP tools for issuer-match checks, signed Guard reviews, wallet holdings, statements, registry lookup, and verification. Use the [MCP guide]({base}/docs/llm) for connection setup.
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
            "API reference",
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
                    "chain": "Bnb",
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
                    "chain": "Ethereum",
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
                    "chain": "Solana",
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
                  "source": "DexScreener + on-chain reads",
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
                      "base_symbol": "NVDA",
                      "quote_symbol": "USDC",
                      "issuer": "Backed xStocks",
                      "ticker": "NVDA",
                      "verdict": "verified",
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
                "schema": { "type": "object" },
                "example": {
                  "updated_at": "2026-10-04T12:00:00Z",
                  "prices": [
                    {
                      "chain": "solana",
                      "pool": "known",
                      "price_usd": 12.5,
                      "change_24h_pct": 1.5,
                      "volume_24h_usd": 100.0,
                      "liquidity_usd": 200.0
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
                "schema": { "type": "object" },
                "example": {
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
                  "chain": "Base",
                  "pool": {
                    "chain": "Base",
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
                  "chain": "Base",
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
                  "chain": "Base",
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
                  "chain": "Base",
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
                      "chain": "Base",
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
                  "chain": "Base",
                  "subject": "0x0000000000000000000000000000000000000001",
                  "verdict": "NoMatch",
                  "issuer": null,
                  "ticker": null,
                  "pool": {
                    "chain": "Base",
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
        "summary": "Verify a signed attestation, statement, or Guard",
        "requestBody": {
          "required": true,
          "content": {
            "application/json": {
              "schema": {
                "oneOf": [
                  { "$ref": "#/components/schemas/Attestation" },
                  { "$ref": "#/components/schemas/Statement" },
                  { "$ref": "#/components/schemas/GuardDocument" }
                ]
              },
              "examples": {
                "attestation": {
                  "summary": "Signed attestation",
                  "value": {
                    "id": "0e9596ff7ba53868e82291934a69a14cc67b3f1cd8625b87013199e128533ffc",
                    "version": 1,
                    "chain": "Base",
                    "subject": "0x0000000000000000000000000000000000000001",
                    "verdict": "NoMatch",
                    "issuer": null,
                    "ticker": null,
                    "pool": {
                      "chain": "Base",
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
                      { "chain": "Base", "address": "0x0000000000000000000000000000000000000001" }
                    ],
                    "assets": [
                      {
                        "wallet": "0x0000000000000000000000000000000000000001",
                        "chain": "Base",
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
                        "chain": "Base",
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
                    "chain": "Base",
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
                    { "chain": "Base", "address": "0x0000000000000000000000000000000000000001" }
                  ],
                  "assets": [
                    {
                      "wallet": "0x0000000000000000000000000000000000000001",
                      "chain": "Base",
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
                      "chain": "Base",
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
          "400": { "description": "Invalid wallet set or chain selection" },
          "502": { "description": "Required chain balance read failed" },
          "504": { "description": "Statement read deadline exceeded" }
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
                    { "chain": "Base", "address": "0x0000000000000000000000000000000000000001" }
                  ],
                  "assets": [
                    {
                      "wallet": "0x0000000000000000000000000000000000000001",
                      "chain": "Base",
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
                      "chain": "Base",
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
                          "chain": "Bnb",
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
                          "chain": "Ethereum",
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
                          "chain": "Solana",
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
                          "chain": "RobinhoodChain",
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
                  "serverInfo": { "name": "QED", "version": "0.1.0" },
                  "remotes": [
                    { "type": "streamable-http", "url": "https://qed.web3-energy.com/mcp" }
                  ],
                  "tools": [
                    {
                      "name": "qed_check",
                      "title": "Check issuer contract match",
                      "description": "Check whether a pool or token address matches an issuer's published stock-token contract. It does not prove backing, custody, reserves, solvency, safety, price, liquidity, or endorsement."
                    },
                    {
                      "name": "qed_guard",
                      "title": "Review a token or pool",
                      "description": "Create a signed read-only review of issuer identity, token powers, source status, and known pool facts. QED never holds keys, submits transactions, or recommends. It does not prove backing, custody, reserves, solvency, safety, price, liquidity, or endorsement."
                    },
                    {
                      "name": "qed_powers",
                      "title": "Read token powers",
                      "description": "Read supported token authority settings and source-verification status for an active issuer registry contract. It does not prove backing, custody, reserves, solvency, safety, price, liquidity, or endorsement."
                    },
                    {
                      "name": "qed_wallet",
                      "title": "Read wallet holdings",
                      "description": "Read stock-token holdings for a wallet address. It does not prove backing, custody, reserves, solvency, safety, price, liquidity, or endorsement."
                    },
                    {
                      "name": "qed_registry_lookup",
                      "title": "Look up issuer contracts",
                      "description": "Look up active issuer registry contracts for a ticker. It does not prove backing, custody, reserves, solvency, safety, price, liquidity, or endorsement."
                    },
                    {
                      "name": "qed_verify",
                      "title": "Verify QED document",
                      "description": "Verify an attestation, signed wallet statement, or Guard document, including signature, trusted signer, environment and freshness where applicable. It does not prove backing, custody, reserves, solvency, safety, price, liquidity, or endorsement."
                    },
                    {
                      "name": "qed_statement",
                      "title": "Create a signed wallet statement",
                      "description": "Sign registry-token balances observed for a selected wallet set and chain set. Balances are on-chain facts at a height, not ownership, solvency or reserves. It does not prove backing, custody, reserves, solvency, safety, price, liquidity, or endorsement."
                    }
                  ],
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
                  "chain": "Bnb",
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
                  "chain": "Ethereum",
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
                  "chain": "Solana",
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
                  "chain": "RobinhoodChain",
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
            "description": "MCP CallToolResult; qed_guard structuredContent is a GuardDocument, qed_powers is a PowersRecord for one match or an object with records (PowersRecordSet) for multiple matches, and qed_statement returns a Statement.",
            "properties": {
              "structuredContent": { "description": "Tool-specific structured JSON; see GuardDocument for qed_guard, PowersRecord and PowersRecordSet for qed_powers, and Statement for qed_statement." },
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
            "enum": ["publisher_contract_match", "publisher_contract_mismatch", "name_resembles_registry_entry", "publisher_metadata_unavailable", "no_publisher", "registry_stale", "registry_removed", "token_paused", "powers_incomplete", "powers_unavailable", "wallet_check_not_applicable", "wallet_check_unavailable", "wallet_frozen", "wallet_blocked", "wallet_sanctioned", "source_unverified", "source_unavailable", "pool_unavailable"]
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
          "chain": { "type": "string", "enum": ["Solana", "RobinhoodChain", "Base", "Ethereum", "Bnb"] },
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
          "chain": { "type": "string", "enum": ["Solana", "RobinhoodChain", "Base", "Ethereum", "Bnb"] },
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
          "candidate": { "oneOf": [{ "$ref": "#/components/schemas/GuardIdentityCandidate" }, { "type": "null" }] }
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
          "chain": { "type": "string", "enum": ["Solana", "RobinhoodChain", "Base", "Ethereum", "Bnb"] },
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
          "kind": { "type": "string", "enum": ["attestation", "statement", "guard"] },
          "id": { "type": "string" },
          "cryptographic": { "type": "boolean" },
          "trusted_signer": { "type": "boolean" },
          "environment_match": { "type": "boolean" },
          "fresh": { "type": ["boolean", "null"], "description": "Null for statements and Guards, which do not have an attestation expiry check." }
        }
      },
      "StatementRequest": {
        "type": "object",
        "required": ["wallets", "chains"],
        "properties": {
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
          "chain": { "type": "string", "enum": ["Solana", "RobinhoodChain", "Base", "Ethereum", "Bnb"] },
          "address": { "type": "string" }
        }
      },
      "StatementPosition": {
        "type": "object",
        "required": ["chain", "wallet", "block", "min_slot", "max_slot"],
        "properties": {
          "chain": { "type": "string", "enum": ["Solana", "RobinhoodChain", "Base", "Ethereum", "Bnb"] },
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
          "chain": { "type": "string", "enum": ["Solana", "RobinhoodChain", "Base", "Ethereum", "Bnb"] },
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
        fn openapi_documents_mcp_powers_and_server_card_contracts() {
            let document = parsed_document();
            let paths = &document["paths"];
            let verify_post = &paths["/verify"]["post"];
            let powers_get = &paths["/api/powers/{address}"]["get"];
            let guard_get = &paths["/api/guard/{address}"]["get"];
            let guard_post = &paths["/api/guard"]["post"];
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
                    ["description"]
                    .as_str()
                    .unwrap()
                    .contains("PowersRecordSet")
            );
            assert!(
                document["components"]["schemas"]["JsonRpcResultResponse"]["properties"]["result"]
                    ["description"]
                    .as_str()
                    .unwrap()
                    .contains("GuardDocument")
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
            assert_eq!(tools.len(), 7);
            assert!(tools.iter().any(|tool| tool["name"] == "qed_guard"));
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
                3
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
                document["components"]["schemas"]["StatementRequest"]["required"],
                serde_json::json!(["wallets", "chains"])
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
