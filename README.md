# QED

QED is a read-only contract-to-issuer-registry checker and directory for stock-paired pools, plus a signed Guard reviewer for supported-chain tokens and pools. Its pool check reports whether the quote contract matches an issuer's published stock-token contract. It does not prove backing, custody, reserves, solvency, safety, price, liquidity, or endorsement.

Source repository: [github.com/boev/qed](https://github.com/boev/qed). Live website: [qed.web3-energy.com](https://qed.web3-energy.com).

## What QED can do

QED checks tokens that claim to be something against the contract their issuer publishes.

- **Check a pool or token address** on Solana, Robinhood Chain, Base, Ethereum, or BNB Chain and report whether the quote token is the contract the issuer published. Verdicts: Verified, Mismatch, No match, or Unknown with the reason. Every check lists its evidence: chain detection, the pool and its sides, token metadata, supply share, and the registry entries compared.
- **Issue a signed certificate** for a check: canonical JSON, SHA-256 identifier, Ed25519 signature, registry hash and expiry, chain position of each read. Anyone can verify it with `POST /verify` and the published key at `/.well-known/qed.json`, or re-run the reads with **Re-check**.
- **Keep an issuer registry** merged from xStocks, Ondo Global Markets, and Robinhood Chain sources (seeded, refreshed at runtime), browsable by ticker, chain, and issuer.
- **List current contract matches**: a directory, per-pool summaries, featured pools, a ranked leaderboard with prices, and an RSS feed, with exact venue and explorer deep links.
- **Scan a wallet** for stock-token holdings and show which contracts match the registry, without putting the address in a URL.
- **Create a signed wallet statement** for selected wallets and chains, with registry-token balances, observed chain positions, issuer-match rows, and token-power summaries. Balances are point-in-time on-chain facts, not proof of ownership or issuer solvency.
- **Review a token or pool with Guard** across supported chains: return a signed document with issuer identity, token-power observations, source-verification status, known pools and quote-side facts, and an overall allow/deny/unknown verdict. Pool identity must be established through QED's known-pool index or on-chain factory/derivation checks; an unindexed Solana account is reviewed as a token. An optional wallet check reports only observed active restrictions; no authority capability alone is a denial.
- **Serve machines**: JSON API, OpenAPI 3.1, `llms.txt`/`llms-full.txt`, and a stateless MCP endpoint (`POST /mcp`) with seven tools: `qed_check`, `qed_powers`, `qed_wallet`, `qed_statement`, `qed_registry_lookup`, `qed_verify`, and `qed_guard`. `/.well-known/mcp/server-card.json` advertises the read-only server to agent clients.

What QED does **not** do: it does not prove backing, custody, reserves, solvency, safety, price, liquidity, or endorsement, and it does not give investment advice. A certificate proves what QED read and signed at one point in time.

## How a check works

- Detect the address as Solana, an EVM address, or a strict Uniswap v4 pool ID.
- Read the pool, token metadata, balances, and supply from the configured RPC endpoint.
- Compare the observed quote contract and token claims with the committed or refreshed issuer registry.
- For a check that produces a certificate, canonicalise the payload, hash it with SHA-256, and sign it with Ed25519.
- Publish the QED record, JSON attestation, and re-check action. A record is point-in-time evidence and can expire or change when registry data changes.

## Trust model

Attestation payloads use compact canonical JSON with recursively sorted object keys. The identifier is the lowercase SHA-256 digest of that payload, and the signature is a base64 Ed25519 signature. Each read records its method, parameters, result hash, and chain position where available. The current registry hash and expiry are included in the attestation. Anyone can fetch the signer key from `/.well-known/qed.json`, verify `POST /verify`, repeat the reads, or use **Re-check** on a certificate. A signature proves payload integrity and signer attribution; it does not prove custody, reserves, solvency, safety, price, endorsement, or that the issuer registry is correct.

Production requires `QED_SIGNING_KEY`. Development generates an ephemeral key and marks attestations as development attestations. Keep production seeds in an encrypted secret manager and never commit them.

## Run it

Build and run the public tree:

```text
docker build -t qed . && docker run -p 8080:8080 qed
```

For development:

```text
cargo run
```

The container listens on port `8080`. Mount `./data` at `/data` when using
Docker Compose to retain local attestations and refreshed registry data.

## Testing

Run the Rust unit tests:

```text
cargo test
```

Run the browser crawl against an isolated data directory:

```text
tests/crawl.sh
```

The crawl checks every full HTML page route, fragment and API status, verifies
complete documents for HTMX requests, audits the hero's open-state CSS, and
writes reduced-motion Firefox screenshots to `/tmp/qed-crawl/`.

## Configuration

| Variable | Default | Purpose |
| --- | --- | --- |
| `QED_BIND` | `127.0.0.1:3000` | Listen address. |
| `QED_DATA_DIR` | `./data` | Writable runtime data and attestations. |
| `QED_REGISTRY_PATH` | `registry/registry.json` | Read-only seed registry path. |
| `QED_PUBLIC_URL` | `http://localhost:3000` | Canonical URL in pages, robots, and sitemap. |
| `QED_ENV` | `development` | Set `production` to require a signing seed. |
| `QED_SIGNING_KEY` | unset | Base64-encoded 32-byte Ed25519 seed for production. |
| `QED_ATTEST_BUCKET` | unset | S3 bucket for durable attestations; local files are used when unset. |
| `QED_PREVIOUS_KEYS` | unset | Comma-separated base58 public signing keys trusted during key rotation. |
| `QED_REGISTRY_XSTOCKS_URL` | `https://api.xstocks.fi/api/v2/public/assets` | xStocks registry source endpoint. |
| `QED_REGISTRY_ONDO_URL` | `https://api.gm.ondo.finance/v1/assets/all/metadata` | Ondo registry source endpoint. |
| `QED_REGISTRY_ONDO_API_KEY` | unset | Required API key from Ondo onboarding; sent only as the `x-api-key` header. |
| `QED_REGISTRY_ROBINHOOD_URL` | `https://api.robinhood.com/rhj/assets` | Robinhood registry source endpoint. |
| `QED_RPC_SOLANA` | Solana mainnet public RPC | Solana JSON-RPC endpoint. |
| `QED_RPC_ROBINHOOD` | Robinhood Chain public RPC | Robinhood Chain JSON-RPC endpoint. |
| `QED_RPC_BASE` | `https://mainnet.base.org` | Base JSON-RPC endpoint. |
| `QED_RPC_ETHEREUM` | `https://ethereum-rpc.publicnode.com` | Ethereum JSON-RPC endpoint. |
| `QED_RPC_BNB` | `https://bsc-dataseed.binance.org` | BNB Chain JSON-RPC endpoint. |
| `QED_RPC_RPS_SOLANA` | `4` | Maximum shared Solana RPC requests per second. |
| `QED_RPC_RPS_EVM` | `8` | Maximum requests per second per EVM-chain provider. |

## API and routes

The machine-readable entry points are `/llms.txt`, `/llms-full.txt`, `/openapi.json`, `/api`, `/mcp`, and `/.well-known/mcp/server-card.json`. `POST /mcp` is stateless Streamable HTTP with exactly seven tools: `qed_check` compares an issuer claim, `qed_powers` reports observed token controls, `qed_wallet` reads holdings, `qed_statement` creates a signed wallet statement, `qed_registry_lookup` lists issuer deployments, `qed_verify` checks signed documents, and `qed_guard` returns the combined signed review; `GET /mcp` returns 405. The public docs include `/docs`, `/docs/llm`, `/docs/api-quick-start`, `/about`, `/security`, `/changelog`, and `/blog`; `/api` is generated from OpenAPI. Statement pages support signed JSON and CSV download, record verification, browser Print/Save as PDF, and re-run with comparison; records are public to anyone with the link and retained in process memory for up to 24 hours.
Supported chain values use the same lowercase slugs in routes, JSON, and OpenAPI: `solana`, `robinhood`, `base`, `ethereum`, and `bnb`. QED also verifies existing signed records that used the previous chain-variant spellings.

`GET /stats` and `GET /api/stats?page={page}` summarize leaderboard counts, issuer-registry coverage, and publisher-catalog observations. Statistics distinguish pools whose venues QED does not support from checks that have not run. Each successful six-hour watch refresh reserves two searches for each of the 10 highest-volume active issuer tickers and rotates up to 30 remaining searches through the registry tail, within limits of 50 searches and 50 Guard evaluations including rechecks. Guard evaluations prioritize candidate tickers in the same highest-volume registry order, then higher-volume pairs within each ticker. Exact on-chain symbol-and-name matches absent from a published deployment catalog are coverage observations, not conclusions about intent. The watch retains up to 128 supported and 256 unsupported observations, evicts oldest-last-seen rows first, and counts evictions. Supported observations retain only identity-determining reads at the queried token (EVM `symbol()`/`name()` calls or Solana metadata reads), with a registry-source snapshot hash; each retained read's params and raw result are capped at 512 serialized bytes. `evidence_truncated` marks omitted reads and clipped read or text details. Overlong labels, reasons, on-chain values, and observation timestamps are clipped rather than rejected; a supported observation is rejected only for an invalid chain, token address, or catalog hash. Full Guard evidence remains available at `guard_url`. Unsupported candidates are clipped the same way and rejected only for an invalid chain id or token address; rejected rows are counted and dropped; signed summaries report counts while signed inputs contain the bounded compact evidence once. API pages hold up to 50 rows per input. The HTML table shows at most 20 most recently seen entries. DexScreener volume is not independently verified.

A failed individual watch search is logged and skipped while the scan continues. If every watch search fails or returns no pairs, QED does not record a fresh scan: retained observations and `last_scanned_at` stay from the last successful search, `source_unavailable_since` records when the outage began, and the stats headline says the impostor search source is unavailable since that time. One-character tickers are searched only in their ticker+x form because DexScreener rejects one-character queries.

Market-data calls query DexScreener first for discovery, featured pools, prices, and publisher-watch searches. GeckoTerminal is used after a DexScreener error or a cached USDC canary returning no pairs (five minutes); a legitimate empty query does not trigger a second request. GeckoTerminal calls wait for the process-wide 10-per-minute budget and back off for 60 seconds on `429`, with 10-second timeouts and a 4 MiB response cap. Each registry refresh is capped at 30 HTTP attempts and 300 distinct validated pools. Watch fallback searches each distinct active registry product name for the top 10 tickers across `robinhood`, `solana`, `eth`, `base`, `bsc`, `arc`, and `ton`, capped at 70 HTTP attempts per scan; unsupported networks are counted, not judged. Market rows identify their source, report volume and liquidity as provided by that source, and link to its exact pool page (`https://www.geckoterminal.com/{network}/pools/{pool}` for GeckoTerminal). The stats page and site footer attribute “Market data: DexScreener / GeckoTerminal”; see the [CoinGecko API Terms](https://www.coingecko.com/en/api_terms).
Each refresh budgets at most four `/tokens/multi` and two `/pools/multi` HTTP attempts per supported network, with no more than 60 distinct pool IDs per network.

`POST /api/guard` accepts `{ "address": "…", "chain": "base", "wallet": "…" }`; prefer it over the legacy `GET /api/guard/{address}?chain=…&wallet=…` because query-string wallets may be exposed in browser history or request URLs. The Guard `reasons[].code` enum is `publisher_contract_match`, `publisher_contract_mismatch`, `name_resembles_registry_entry`, `publisher_metadata_unavailable`, `no_publisher`, `registry_stale`, `registry_removed`, `token_paused`, `powers_incomplete`, `powers_unavailable`, `wallet_check_not_applicable`, `wallet_check_unavailable`, `wallet_frozen`, `wallet_blocked`, `wallet_sanctioned`, `source_unverified`, `source_unavailable`, and `pool_unavailable`.

`publisher_metadata_unavailable` means QED could not read complete current publisher metadata for a resembling registry candidate; it is not a mismatch.

| Method | Route | Result |
| --- | --- | --- |
| GET | `/` | Home page and live pools. |
| GET | `/stats` | Leaderboard, registry, and publisher deployment-catalog watch statistics, including method and scan time. |
| GET | `/stats.json`, `/stats.csv` | Download the signed point-in-time `kind: "stats"` snapshot as JSON or CSV (CSV embeds the exact signed JSON document). |
| GET | `/check` | Permanent redirect to `/guard`. |
| GET | `/registry`, `/registry/table` | Registry directory and table fragment. |
| GET, POST | `/wallet` | Wallet holdings form and read-only holdings result. |
| GET, POST | `/guard` | Signed Guard review form and submission. |
| GET | `/guard/{chain}/{address}` | Human-readable signed Guard review result. |
| GET | `/tokens?ticker={ticker}`, `/tokens/{ticker}`, `/chains/{chain_name}` | Canonical ticker lookup plus token and chain directories. |
| GET | `/glossary`, `/guide/verify-a-stock-token` | Glossary and contract-first verification guide. |
| GET | `/docs`, `/docs/llm`, `/docs/api-quick-start` | Documentation hub and guides for verification, MCP clients, and the API. |
| GET | `/about`, `/security` | QED scope and operator, and security policy. |
| GET | `/changelog`, `/blog`, `/blog/{slug}` | Release history, blog index, and article pages; drafts are not listed, served, or fed. |
| GET | `/changelog.xml`, `/blog.xml` | Atom feeds for the changelog and published blog posts. |
| GET | `/llms.txt`, `/llms-full.txt` | Machine-readable product and API guides. |
| GET | `/statements` | Signed wallet statement form. |
| POST | `/statements` | Create a statement through the existing statement API and redirect to its page. |
| GET | `/statements/{id}/download.csv` | Download statement rows as CSV. |
| GET | `/statements/{id}/verify` | Verify the statement signature, signer, and environment. |
| POST | `/statements/{id}/recheck` | Re-run selected wallets and redirect to the new statement with a comparison. |
| GET | `/validated`, `/validated/{chain}/{subject}` | Current contract-match directory and pool summary; records are time-bounded and should be re-checked after expiry. |
| GET | `/validated.xml` | RSS feed of current contract matches. |
| GET | `/v/{id}` | Signed certificate page with nerd mode. |
| POST | `/v/{id}/recheck` | Re-run the recorded reads. |
| GET | `/v/{id}/verify` | Verify a certificate signature, signer, environment, and freshness. |
| GET | `/api` | Server-rendered API reference generated from the OpenAPI document. |
| GET | `/openapi.json` | OpenAPI 3.1 route document. |
| POST | `/mcp` | Stateless MCP Streamable HTTP with exactly seven tools: issuer check, token powers, wallet holdings, signed wallet statement, registry lookup, document verification, and Guard review; `GET /mcp` returns 405. |
| GET | `/pools/featured` | Featured-pool HTML fragment/page. |
| GET | `/api/check/{address}` | JSON check result with token-power observations for the issuer-registry-matched pool side when available. |
| GET | `/api/powers/{address}?chain={chain}` | Observed control signals and source status for any supported-chain contract; optional `chain` filters, otherwise active registry matches or the detected EVM chain are used. Proxy records report implementation source verification plus the proxy's separate status. |
| GET | `/api/guard/{address}?chain={chain}&wallet={wallet}` | Signed Guard document; `chain` is required and `wallet` is optional. Wallet query values may be exposed in browser history or request URLs; prefer `POST /api/guard`. |
| POST | `/api/guard` | JSON body `{ "address": "…", "chain": "base", "wallet": "…" }`; signed Guard review without a wallet in the URL. |
| POST | `/api/wallet` | JSON body `{ "address": "…" }`; check stock-token holdings without putting the address in the URL. |
| POST | `/api/statement` | JSON `{ "wallets": ["…"], "chains": ["base"], "block": 123 }`; signs active-registry token balances and observed chain positions as a `kind: "statement"` payload, with per-asset observation slots. EVM `block` is exact; for Solana it is a minimum context slot. |
| GET | `/api/statement/{id}` | Signed statement JSON held in process memory for up to 24 hours; statement IDs are content hashes, not access controls. |
| GET | `/statements/{id}` | Human-readable statement page, public to anyone with its link, held in process memory for up to 24 hours. |
| GET | `/api/attest/{id}` | Signed attestation JSON. |
| GET | `/api/registry`, `/api/pools/featured` | Registry and featured pool JSON. |
| GET | `/api/leaderboard`, `/api/prices`, `/api/status` | Ranked pools, prices, and freshness state. |
| GET | `/api/stats?page={page}` | Paginated statistics summary and full reducer inputs, including unsupported-venue counts and publisher-catalog observations. |
| POST | `/verify` | Verify signed attestation, statement, Guard, or stats documents; report kind, cryptographic validity, signer trust, environment, and freshness where applicable. Stats snapshots are strict signed documents; freshness applies only to attestations.
| GET | `/.well-known/qed.json` | Public signer metadata and key. |
| GET | `/.well-known/mcp/server-card.json` | Read-only MCP server card with remote endpoint and tool summaries. |
| GET | `/healthz` | Service health. |
| GET | `/imprint`, `/privacy`, `/terms` | Legal pages. |
| GET | `/robots.txt`, `/sitemap.xml` | Crawler metadata. |

Leaderboard Market links open exact external pair pages. Pool-detail pages may also offer venue-native actions. These links are provided for navigation only; QED does not assess custody, reserves, solvency, safety, price, liquidity, execution, or endorsement. Compare the exact pool and contract address and re-check the record before interacting.

### Exact external-link patterns

| Venue | Exact market or action URL | Source |
| --- | --- | --- |
| Uniswap | Pool `https://app.uniswap.org/explore/pools/{chain}/{pool}`; swap `https://app.uniswap.org/swap?chain={robinhood\|base\|ethereum\|bnb}&inputCurrency={base}&outputCurrency={quote}` | [Uniswap app](https://app.uniswap.org/) |
| pump.fun | `https://pump.fun/coin/{mint}` | [pump.fun](https://pump.fun/) |
| Raydium | `https://raydium.io/swap/?inputMint={base}&outputMint={quote}` | [Raydium](https://raydium.io/) |
| Orca | `https://www.orca.so/pools/{pool}` | [Orca](https://www.orca.so/) |
| Meteora DLMM | `https://app.meteora.ag/dlmm/{pool}` | [Meteora](https://app.meteora.ag/) |
| Long.xyz | No exact pool deep link published, so QED shows no Long.xyz homepage link. | — |
| DexScreener | `https://dexscreener.com/{chain}/{pool}`, using DexScreener chain IDs such as `robinhood`, `bsc`, `ethereum`, `base`, and `solana` | [DexScreener](https://dexscreener.com/) |
| GeckoTerminal | `https://www.geckoterminal.com/{network}/pools/{pool}` | [GeckoTerminal](https://www.geckoterminal.com/) |
| Explorer | Solana `https://solscan.io/account/{pool}`; Robinhood `https://robinhoodchain.blockscout.com/address/{pool}`; Base `https://basescan.org/address/{pool}`; Ethereum `https://etherscan.io/address/{pool}`; BNB `https://bscscan.com/address/{pool}` | Chain explorer |

## Links and presentation

QED uses plain-text `Checked on QED` links to public records rather than badges or embeds. Plain-text destinations are inspectable and reduce badge-impersonation risk. Partners and visitors should compare the exact address in the QED record with the address they were given.

The home page uses restrained ambient background motion: two slow, faint
purple and green color washes replace decorative geometry and stay static
when reduced motion is enabled. Its headline remains “QED checks whether a
pool uses the stock-token contract published by its issuer.” and its
explanation remains “A ticker is not a contract. Compare the pool with the
issuer's published stock-token contract.” The Check page lazily loads active
registry tickers on first focus, filters them case-insensitively, supports
pointer and keyboard selection, and keeps manual ticker lookup available if
suggestions are unavailable.


## Architecture

See [Runtime architecture and request flow](docs/architecture.md) for the
single-process diagram, background refresh cadence, user-check sequence, and
request boundaries.

- `src/main.rs`: configuration, application composition, refresh loops, and HTTP listener.
- `src/config.rs`: environment configuration and defaults.
- `src/ports.rs`: chain, registry, signer, cache, storage, and source-verification boundaries.
- `src/domain/`: chain and data models plus pure matching, power, Guard, attestation, and statement rules.
- `src/app/`: check, wallet, powers, Guard, attestation, statement, and warm-up flows.
- `src/adapters/`: chain readers, issuer registry refresh, discovery, signing/storage, and runtime state.
- `src/adapters/web.rs`: router, middleware, cache headers, robots, and sitemap.
- `src/adapters/web/api.rs`, `pages.rs`, and `mcp.rs`: JSON, HTML, and Streamable HTTP MCP request adapters.
- `src/adapters/web/views.rs` and `src/adapters/web/templates/`: Askama view models, display formatting, and templates.
- `src/adapters/web/docs.rs`: discoverability pages, the API guide, and the OpenAPI route document.
- `static/`: CSS, JavaScript, icons, logo, favicon, and OpenGraph PNG.
- `release/llms-full.md`: source Markdown included in `/llms-full.txt`.

## Registry sources

The seed is `registry/registry.json`. Runtime refresh adapters read xStocks, Ondo, and Robinhood sources, merge entries by chain and contract, and write the refreshed copy below `QED_DATA_DIR`. Current issuer coverage is limited to entries actually present in those registry sources; configured readers for Solana, Robinhood Chain, Base, Ethereum, and BNB Chain do not imply that every issuer has a contract on every chain. To add an issuer, add an adapter in `src/adapters/registry.rs`, map its response to every `Entry` field, add a focused fixture and test, then regenerate and sort the seed by issuer, chain, and ticker. Never edit the seed from the running service.

## Licence

Copyright © 2026 Web3 Energy Ltd.

Apache License 2.0. See [LICENSE](LICENSE).
