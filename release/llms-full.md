# QED

QED checks tokens that claim to be something against the contract their issuer publishes. QED is a read-only issuer-contract checker for stock-paired pools and a signed Guard reviewer for supported-chain tokens and pools. The pool check compares the observed quote contract with an issuer's published stock-token contract; Guard reports issuer identity, token powers, source status, and known pool facts. QED does not prove backing, custody, reserves, solvency, safety, price, liquidity, or endorsement.

## How a check works

1. QED classifies a Solana address, EVM address, or strict Uniswap v4 pool ID.
2. It reads the pool, token metadata, balances, supply, and chain position from the configured RPC.
3. It compares the observed quote contract and token claims with the committed or refreshed issuer registry.
4. It canonicalizes the observed payload, hashes it with SHA-256, and signs it with Ed25519.
5. The result is available as HTML and JSON, with a re-check action for certificates. A record is point-in-time evidence and can expire or change when registry data changes.

## Verdicts

- `Verified`: at check time, the observed quote token contract matches an issuer registry entry.
- `Mismatch`: a pool claims a ticker but the observed quote contract differs from the registry.
- `NoMatch`: neither token side claims a registry ticker.
- `Unknown`: QED could not complete a reliable read, such as a missing pool or unavailable RPC. Unknown is not a verification.

## Trust model

The attestation ID is the lowercase SHA-256 digest of recursively key-sorted payload JSON. The signature is Ed25519 over that canonical payload. Production records must use the configured current or previous trusted public key, the expected development/production environment, and a current expiry window. Fetch the public verification key from [/.well-known/qed.json](https://qed.example/.well-known/qed.json) or the instance's equivalent public URL.

## Freshness and scope

A `Verified` result is a point-in-time contract-to-registry match, not proof of backing, custody, reserves, solvency, safety, price, liquidity, or endorsement. Signed records include the check time, registry hash, and expiry. Re-check after expiry or when the issuer registry changes; a re-check is a new observation. Trade links open external venues and are provided for navigation only.

## QED Guard

`GET /api/guard/{address}?chain={chain}` and the `/guard` review page return a signed point-in-time document for a supported token or pool. It includes registry identity (`match`, `mismatch`, `no_publisher`, `registry_stale`, or `registry_removed`), observed token powers, source-verification status, known pools with quote-side facts, and an overall `allow`, `deny`, or `unknown` verdict. A deny requires an exact normalized registry ticker plus a matching registry name or issuer in on-chain token metadata on the same chain, or an observed active restriction for a supplied wallet (including an active token-wide pause). Authority capability alone does not deny. An active token-wide pause without a supplied wallet is `unknown` with the structured `token_paused` reason and the human-readable detail `The token-wide pause is active; transfers are blocked until it is lifted.` Guard is read-only: it never holds keys, submits transactions, or recommends. It does not prove backing, custody, reserves, solvency, safety, price, liquidity, or endorsement. Verify its signature with `POST /verify`.

Pool identity must be established through QED's known-pool index or on-chain factory/derivation checks; an unindexed Solana account is reviewed as a token.

Prefer `POST /api/guard` with `{"address":"…","chain":"base","wallet":"…"}` over the legacy wallet query parameter: query-string wallets may be exposed in browser history or request URLs. `reasons[].code` is an enum: `publisher_contract_match`, `publisher_contract_mismatch`, `name_resembles_registry_entry`, `publisher_metadata_unavailable`, `no_publisher`, `registry_stale`, `registry_removed`, `token_paused`, `powers_incomplete`, `powers_unavailable`, `wallet_check_not_applicable`, `wallet_check_unavailable`, `wallet_frozen`, `wallet_blocked`, `wallet_sanctioned`, `source_unverified`, `source_unavailable`, and `pool_unavailable`.

`publisher_metadata_unavailable` means QED could not read complete current publisher metadata for a resembling registry candidate; it is not a mismatch and remains unknown unless another candidate proves a contradiction.

## Public routes

Examples below use `https://qed.example`; replace it with the deployed public URL.

- `GET /healthz` returns `{"status":"ok"}`.
- `GET /` returns the home page and live pools.
- `GET, POST /check` serves the check form and result fragment.
- `GET /wallet` serves the wallet holdings form.
- `GET /registry` and `GET /registry/table` serve the issuer registry and table fragment.
- `GET /tokens/{ticker}` and `GET /chains/{chain_name}` serve token and chain directories.
- `GET /glossary` and `GET /guide/verify-a-stock-token` serve explanatory pages.
- `GET /docs`, `/docs/llm`, and `/docs/api-quick-start` serve the contract-first guide, MCP client guide, and API quick start; `/about` and `/security` provide product and security information.
- `GET /changelog` and `GET /changelog.xml` serve the anchored release history and its Atom feed.
- `GET /blog` and `GET /blog/{slug}` serve published blog pages; `GET /blog.xml` is the Atom feed. Draft posts are excluded from the index, article pages, feed, and sitemap.
- `GET /validated` and `GET /validated/{chain}/{subject}` serve current contract matches and pool summaries.
- `GET /validated.xml` returns the current contract-match feed.
- `GET /v/{id}` serves a signed certificate; `POST /v/{id}/recheck` re-runs its recorded reads.
- `GET /imprint`, `/privacy`, and `/terms` return the legal pages.
- `GET /robots.txt` and `/sitemap.xml` return crawler metadata.
- `GET /api` serves the server-rendered API reference generated from the OpenAPI document.
- `GET /guard` serves the signed review form; `POST /guard` submits it; `GET /guard/{chain}/{address}` renders a signed review result.
- `GET /llms.txt` and `GET /llms-full.txt` return machine-readable service guides.
- `GET /openapi.json` returns the OpenAPI 3.1 route document.
- `GET /pools/featured` returns the featured-pool HTML page.
- `GET /api/registry` returns the issuer registry JSON.
- `GET /api/pools/featured` returns curated pools.
- `GET /api/leaderboard?page=1&per=50&sort=volume&dir=desc` returns the ranked page and global ranks.
- `GET /api/prices?ids=solana:POOL` returns current price data.
- `GET /api/status` returns registry, leaderboard, featured, and price freshness state.
- `GET /api/check/{address}` checks a token or pool and returns a point-in-time `CheckResult`, including token-power observations for the registry-matched pool side when available.
- `GET /api/guard/{address}?chain={chain}&wallet={wallet}` returns a signed Guard document; `chain` is required and `wallet` is optional. Wallet query values may be exposed in browser history or request URLs; prefer `POST /api/guard`.
- `POST /api/guard` with `{"address":"…","chain":"base","wallet":"…"}` returns the same signed document with the optional wallet in a JSON request body.
- `GET /api/powers/{address}?chain={chain}` returns observed control signals and source-verification status for any supported-chain contract. `chain` is optional: without it, active registry matches are returned or the EVM chain is detected for an unregistered contract. Solana `source_verified` covers the Token-2022 token-program build; EVM proxy records verify the resolved implementation source and report proxy source status separately. These statuses describe source-repository matching, not backing or issuer endorsement.
- `POST /api/wallet` with `{"address":"…"}` in the request body checks stock-token holdings without putting an address in the URL. Results are not cached by address.
- `POST /api/statement` with `{"wallets":["WALLET_ADDRESS"],"chains":["base"],"block":123}` signs active issuer-registry token balances and observed chain positions as a signed `kind: "statement"` document. Each asset includes its observation slot; EVM `block` selects an exact block, and for Solana it is a minimum context slot with the returned slot range recorded.
- `GET /statements/{id}` renders a statement page that is public to anyone with its link and retained in process memory for up to 24 hours; its ID is a content hash, not an access control.
- `GET /statements` serves a plain HTML wallet statement form; `POST /statements` uses the existing statement creation path and redirects to the resulting page.
- `GET /api/statement/{id}` returns signed statement JSON from the same 24-hour in-process cache.
- `GET /api/attest/{id}` returns a signed attestation JSON document.
- `POST /verify` accepts a signed attestation, statement, or Guard and reports `kind`, `ok`, `cryptographic`, `trusted_signer`, and `environment_match`; `fresh` applies to attestations and is null for statements and Guards.
- `POST /mcp` exposes `qed_check`, `qed_guard`, `qed_powers`, `qed_wallet`, `qed_statement`, `qed_registry_lookup`, and `qed_verify` over stateless Streamable HTTP.
- `GET /.well-known/qed.json` returns the public signing key and algorithm metadata.
- `GET /.well-known/mcp/server-card.json` returns the read-only MCP server card. The repository includes an MCP registry manifest at `server.json`.

For example:

```text
curl https://qed.example/healthz
curl 'https://qed.example/api/check/POOL_OR_TOKEN_ADDRESS'
curl https://qed.example/api/attest/ATTESTATION_ID
curl https://qed.example/openapi.json
```

## MCP

QED implements the current MCP Streamable HTTP protocol revision `2026-07-28` and advertises supported versions `["2026-07-28","2025-11-25","2025-06-18","2025-03-26"]` through `server/discover`: modern requests carry per-request protocol metadata and receive one JSON response, with no sessions or SSE streams. For compatibility with clients using the initialization lifecycle, QED answers legacy `initialize` requests for `2025-11-25`, `2025-06-18`, and `2025-03-26` without creating a session. `GET /mcp` returns `405 Method Not Allowed`. All seven tools are read-only; `qed_guard` signs the current identity, powers, source, known-pool, and optional wallet-restriction observations. Authority capability alone never denies. Its `wallet` argument is optional, and an unavailable wallet check is reported as unknown. `qed_statement` signs selected registry-token balances and reports them as on-chain facts at a height, not ownership, solvency or reserves. `qed_powers` reports observed seizure, blocking, rule-change, and source-verification signals for registered issuer contracts. Source-verification statuses describe source matching, not backing or issuer endorsement.

The `qed_verify` tool accepts signed attestations, statements, and Guard documents and reports their `kind`; statement and Guard verification have no freshness check.

Connect Claude Code:

```sh
claude mcp add --transport http qed https://qed.web3-energy.com/mcp
```

Raw curl example using the legacy initialization lifecycle:

```sh
curl -sS https://qed.example/mcp \
  -H 'Accept: application/json, text/event-stream' \
  -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"curl","version":"1"}}}'

curl -sS https://qed.example/mcp \
  -H 'Accept: application/json, text/event-stream' \
  -H 'Content-Type: application/json' \
  -H 'MCP-Protocol-Version: 2025-11-25' \
  -d '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"qed_registry_lookup","arguments":{"ticker":"NVDA"}}}'
curl -sS https://qed.example/mcp \
  -H 'Accept: application/json, text/event-stream' \
  -H 'Content-Type: application/json' \
  -H 'MCP-Protocol-Version: 2025-11-25' \
  -d '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"qed_powers","arguments":{"address":"REGISTERED_ISSUER_TOKEN_ADDRESS","chain":"base"}}}'
curl -sS https://qed.example/mcp \
  -H 'Accept: application/json, text/event-stream' \
  -H 'Content-Type: application/json' \
  -H 'MCP-Protocol-Version: 2025-11-25' \
  -d '{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"qed_statement","arguments":{"wallets":["EVM_WALLET_ADDRESS"],"chains":["base"]}}}'
curl -sS https://qed.example/mcp \
  -H 'Accept: application/json, text/event-stream' \
  -H 'Content-Type: application/json' \
  -H 'MCP-Protocol-Version: 2025-11-25' \
  -d '{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"qed_guard","arguments":{"address":"0xc845b2894dBddd03858fd2D643B4eF725fE0849d","chain":"ethereum"}}}'

```

The seven tools are `qed_check`, `qed_guard`, `qed_powers`, `qed_wallet`, `qed_statement`, `qed_registry_lookup`, and `qed_verify`. QED is read-only by design: it never holds keys, submits transactions, or recommends. It does not prove backing, custody, reserves, solvency, safety, price, liquidity, or endorsement. Re-check a certificate after expiry or when the issuer registry changes.

For `qed_powers`, `structuredContent` is a `PowersRecord` object when one registered-chain match exists. If multiple chains match and `chain` is omitted, it is an object shaped as `{ "records": [ ... ] }`, never a top-level array. `GET /api/powers/{address}` retains its separate REST shape: one record or a multi-chain array.



## Human pages
- [Home](https://qed.example/)
- [Check](https://qed.example/check)
- [Issuer registry](https://qed.example/registry)
- [Current contract matches](https://qed.example/validated)
- [Token directory](https://qed.example/tokens/NVDA)
- [Chain directory](https://qed.example/chains/solana)
- [Glossary](https://qed.example/glossary)
- [Contract verification guide](https://qed.example/guide/verify-a-stock-token)
- [Documentation hub](https://qed.example/docs)
- [About QED](https://qed.example/about)
- [Security policy](https://qed.example/security)
- [Changelog](https://qed.example/changelog)
- [Blog](https://qed.example/blog)
- [Create a wallet statement](https://qed.example/statements)
- [Current contract-match feed](https://qed.example/validated.xml)
- [Imprint](https://qed.example/imprint)
- [Privacy](https://qed.example/privacy)
- [Terms](https://qed.example/terms)

QED uses plain-text `Checked on QED` links to public records rather than badges or embeds. Plain-text destinations are inspectable and reduce badge-impersonation risk. Compare the exact pool and contract address before using a record or its external trade link.

## Registry sources and limits

The committed seed is `registry/registry.json`. Runtime adapters refresh xStocks, Ondo, and Robinhood data into the writable data directory. Current issuer coverage is limited to entries present in those sources; configured readers for Solana, Robinhood Chain, Base, Ethereum, and BNB Chain do not imply that every issuer has a registry contract on every chain. Public checks are rate-limited and share a bounded concurrency pool. Leaderboard pages return at most 50 rows, price queries cap IDs and input bytes, registry filters cap query length, and persisted attestation/index data is bounded. A missing or uncertain read is never presented as verified.

## Legal and project links

QED's [Imprint](https://qed.example/imprint), [Privacy](https://qed.example/privacy), and [Terms](https://qed.example/terms) pages describe the service and data handling. The public source repository is [github.com/boev/qed](https://github.com/boev/qed).
