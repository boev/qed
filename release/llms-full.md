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
Supported chain values use the lowercase slugs `solana`, `robinhood`, `base`, `ethereum`, and `bnb` in URLs, JSON, and OpenAPI. QED verifies existing signed records that used the previous variant spellings.

- `GET /healthz` returns `{"status":"ok"}`.
- `GET /` returns the home page and live pools.
- `GET /check` permanently redirects to `/guard`.
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
- `GET /stats` serves leaderboard, issuer-registry, and publisher deployment-catalog watch statistics, including scan method/time, counts last seen within the last 7 days, and entries first seen this UTC week.
- `GET /stats.json` downloads the signed point-in-time `kind: "stats"` JSON document.
- `GET /stats.csv` downloads CSV whose first record embeds that exact signed JSON document.
- `GET /api/stats?page={page}` returns the leaderboard summary and one page of signed reducer inputs: leaderboard rows, complete retained publisher-watch and unsupported-chain candidates, active registry rows, and their hashes. Pages hold up to 50 rows per input.
  Each successful six-hour watch refresh reserves 20 searches for the 10 highest-volume active registry tickers (one ticker and one ticker+x query apiece); up to 30 remaining searches rotate through the rest, within a budget of 50 searches and 50 sequential QED Guard evaluations including rechecks. Guard evaluations prioritize candidate tickers in the same highest-volume registry order, then higher-volume pairs within each ticker. Exact on-chain symbol-and-name matches absent from a publisher's deployment catalog are coverage observations, not conclusions about intent. The watch retains at most 128 supported and 256 unsupported observations, evicts oldest-last-seen rows first, and counts evictions. Supported observations retain only identity-determining reads at the queried token (EVM `symbol()`/`name()` calls or Solana metadata reads) and the registry-source snapshot hash. Read params and raw results are each capped at 512 serialized bytes; `evidence_truncated` marks omitted reads and clipped read or text details. Overlong labels, reasons, on-chain values, and timestamps are clipped rather than rejected; supported observations are rejected only for invalid chain, token address, or catalog hash. Full Guard evidence remains available at each `guard_url`. Unsupported candidates are clipped the same way and rejected only for an invalid chain id or token address; rejected rows are counted and dropped. The signed summary contains counts, not a duplicate observation list. The HTML table shows at most 20 observations last seen within the last 7 days; reported DexScreener 24-hour volume is not independently verified.
  A failed individual watch search is logged and skipped while the scan continues. If every watch search fails or returns no pairs, QED does not record a fresh scan: retained observations and `last_scanned_at` stay from the last successful search, `source_unavailable_since` records when the outage began, and the stats headline says the impostor search source is unavailable since that time. One-character tickers are searched only in their ticker+x form because DexScreener rejects one-character queries.
  DexScreener is queried first for discovery, featured pools, price refresh, and watch search. GeckoTerminal fallback is used on a DexScreener error or a cached USDC canary returning no pairs for five minutes; a legitimate empty result does not trigger another source. GeckoTerminal requests wait for a 10-per-minute process budget, back off 60 seconds before one retry on `429`, and use a 10-second timeout and 4 MiB response cap. Registry discovery is capped at 30 HTTP attempts and 300 distinct validated pools per refresh. Watch fallback searches every distinct active registry product name for top-10 tickers across `robinhood`, `solana`, `eth`, `base`, `bsc`, `arc`, and `ton`, capped at 70 HTTP attempts per scan; unsupported networks are counted, not judged. Leaderboard rows and watch observations record `source: "dexscreener" | "geckoterminal"`; volume and liquidity are reported by that source, and its exact pool page is used for the Market link (`https://www.geckoterminal.com/{network}/pools/{pool}`). The stats page and site footer say “Market data: DexScreener / GeckoTerminal”; see [CoinGecko API Terms](https://www.coingecko.com/en/api_terms).
  Registry discovery divides the 30-attempt budget across the five supported networks: at most four `/tokens/multi` and two `/pools/multi` HTTP attempts per network, with no more than 60 distinct pool IDs per network.
  The retained Backed xStocks deployment records cover Arbitrum, BSC, Ethereum, HyperEVM, Ink, Mantle, Monad, Optimism, Solana, Ton, Tron, and XLayer, including source-provided `wrapperAddress` and `wrapperAddressV2` fields. A catalog match is accepted only on the deployment's published network; QED issues verdicts only on supported chains.
  Leaderboard entries preserve `verdict` and add `read_status` (`checked`, `not_read_yet`, or `unsupported_venue`) plus `read_reason` (`rpc_limit`, `transient`, or `unsupported`).
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
- `GET /statements/{id}/download.csv` downloads statement rows as CSV.
- `GET /statements/{id}/verify` checks the statement signature, trusted signer, and environment.
- `POST /statements/{id}/recheck` re-runs the same wallet and chain selection, then redirects to a new statement with a comparison against the prior record.
- `GET /api/statement/{id}` returns signed statement JSON from the same 24-hour in-process cache.
- `GET /api/attest/{id}` returns a signed attestation JSON document.
- `POST /verify` accepts a signed attestation, statement, Guard, or stats snapshot and reports `kind`, `ok`, `cryptographic`, `trusted_signer`, and `environment_match`; `fresh` applies to attestations and is null for the other document kinds.
- `GET /v/{id}/verify` checks the certificate signature, signer, environment, and freshness; expired valid records remain historical records and should be re-checked.
- `POST /mcp` exposes exactly seven tools—`qed_check`, `qed_powers`, `qed_wallet`, `qed_statement`, `qed_registry_lookup`, `qed_verify`, and `qed_guard`—over stateless Streamable HTTP.
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

QED implements the current MCP Streamable HTTP revision `2026-07-28` without sessions or SSE streams. Each current-protocol request carries `_meta` with protocol, client, and capability information plus a matching `Mcp-Method` header; `tools/call` also requires the matching `Mcp-Name`. It answers `server/discover`, `notifications/initialized`, `ping`, `tools/list`, and `tools/call`; legacy `2025-11-25` clients can initialize and call the same seven read-only tools. `GET /mcp` returns `405 Method Not Allowed`. The tools are `qed_check` (issuer contract comparison), `qed_powers` (observed token controls and source status), `qed_wallet` (wallet holdings), `qed_statement` (signed holdings at a block height), `qed_registry_lookup` (issuer-published deployments by ticker), `qed_verify` (signature, signer, environment, and applicable freshness), and `qed_guard` (combined signed token-or-pool review).

QED is read-only by design: it never holds keys, submits transactions, or recommends. It does not prove backing, custody, reserves, solvency, safety, price, liquidity, or endorsement. Re-check a certificate after expiry or when the issuer registry changes.

Connect Claude Code:

```sh
claude mcp add --transport http qed https://qed.web3-energy.com/mcp
```

Claude Desktop and Cursor can use this remote HTTP server configuration:

```json
{
  "mcpServers": {
    "qed": {
      "type": "http",
      "url": "https://qed.web3-energy.com/mcp"
    }
  }
}
```

Current Streamable HTTP (`2026-07-28`) requests carry protocol, client, and capability metadata in `params._meta` on every request, plus a matching `Mcp-Method` header; `tools/call` also carries a matching `Mcp-Name`.

Modern raw curl example for discovery followed by a registry lookup:

```sh
curl -sS https://qed.example/mcp \
  -H 'Accept: application/json, text/event-stream' \
  -H 'Content-Type: application/json' \
  -H 'MCP-Protocol-Version: 2026-07-28' \
  -H 'Mcp-Method: server/discover' \
  -d '{"jsonrpc":"2.0","id":1,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientInfo":{"name":"curl","version":"1"},"io.modelcontextprotocol/clientCapabilities":{}}}}'

curl -sS https://qed.example/mcp \
  -H 'Accept: application/json, text/event-stream' \
  -H 'Content-Type: application/json' \
  -H 'MCP-Protocol-Version: 2026-07-28' \
  -H 'Mcp-Method: tools/call' \
  -H 'Mcp-Name: qed_registry_lookup' \
  -d '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"qed_registry_lookup","arguments":{"ticker":"NVDA"},"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientInfo":{"name":"curl","version":"1"},"io.modelcontextprotocol/clientCapabilities":{}}}}'
```

Legacy Streamable HTTP (`2025-11-25`) remains supported:

```sh
curl -sS https://qed.example/mcp \
  -H 'Accept: application/json, text/event-stream' \
  -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":3,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"curl","version":"1"}}}'

curl -sS https://qed.example/mcp \
  -H 'Accept: application/json, text/event-stream' \
  -H 'Content-Type: application/json' \
  -H 'MCP-Protocol-Version: 2025-11-25' \
  -d '{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"qed_registry_lookup","arguments":{"ticker":"NVDA"}}}'
```






## Human pages
- [Home](https://qed.example/)
- [Guard](https://qed.example/guard)
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
