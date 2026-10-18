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

Production requires `QED_SIGNING_KEY`. Development keeps its signing key in `QED_DATA_DIR/dev-signing-key` and marks records as development records. Signing keys are never imported into Postgres. Keep production seeds in an encrypted secret manager and never commit them.

## Run it

Build and run the public tree:

```text
docker build -t qed . && docker run -p 8080:8080 qed
```

For development:

```text
cargo run
```

The container listens on port `8080`. Mount writable durable storage at `/data`
to retain the outage outboxes and local working caches across task replacement.

## Testing

Run the Rust unit tests:

```text
cargo test
```

Set `QED_TEST_DATABASE_URL` to a disposable local Postgres database to exercise the isolated-schema import, retention, outage-replay and S3 overflow tests. Without it those database scenarios are skipped. The S3 overflow test uses a local HTTP object store, never AWS.

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
| `QED_DATABASE_URL` | unset | Postgres primary store for durable public state and observation history; URL is never logged. Unset retains legacy file/S3 storage and explicitly non-durable in-memory history. |
| `QED_DATABASE_CA_FILE` | `/etc/qed/rds-global-bundle.pem` when present | Additional PEM trust roots for rustls, with full certificate and hostname verification. |
| `QED_DB_BLOB_MAX_BYTES` | `1048576` | Maximum inline Postgres payload bytes; larger JSON uses the configured S3 bucket and a stored key plus SHA-256. |
| `QED_REGISTRY_PATH` | `registry/registry.json` | Read-only seed registry path. |
| `QED_PUBLIC_URL` | `http://localhost:3000` | Canonical URL in pages, robots, and sitemap. |
| `QED_ENV` | `development` | Set `production` to require a signing seed. |
| `QED_SIGNING_KEY` | unset | Base64-encoded 32-byte Ed25519 seed for production. |
| `QED_ATTEST_BUCKET` | unset | Existing S3 bucket: overflow blobs and legacy import/fallback with Postgres; legacy durable store without Postgres. |
| `QED_PREVIOUS_KEYS` | unset | Comma-separated base58 public signing keys trusted during key rotation. |
| `QED_REGISTRY_XSTOCKS_URL` | `https://api.xstocks.fi/api/v2/public/assets` | xStocks registry source endpoint. |
| `QED_REGISTRY_ONDO_URL` | `https://api.gm.ondo.finance/v1/assets/all/metadata` | Ondo authenticated API endpoint, used when an API key is set. |
| `QED_REGISTRY_ONDO_API_KEY` | unset | Optional Ondo API key, sent only as the `x-api-key` header; without it QED uses Ondo's public catalog. |
| `QED_REGISTRY_ROBINHOOD_URL` | `https://api.robinhood.com/rhj/assets` | Robinhood registry source endpoint. |
| `QED_RPC_SOLANA` | Solana mainnet public RPC | Solana JSON-RPC endpoint. |
| `QED_RPC_ROBINHOOD` | Robinhood Chain public RPC | Robinhood Chain JSON-RPC endpoint. |
| `QED_RPC_BASE` | `https://mainnet.base.org` | Base JSON-RPC endpoint. |
| `QED_RPC_ETHEREUM` | `https://ethereum-rpc.publicnode.com` | Ethereum JSON-RPC endpoint. |
| `QED_RPC_BNB` | `https://bsc-dataseed.binance.org` | BNB Chain JSON-RPC endpoint. |
| `QED_RPC_RPS_SOLANA` | `4` | Maximum shared Solana RPC requests per second. |
| `QED_RPC_RPS_EVM` | `8` | Maximum requests per second per EVM-chain provider. |
| `QED_RPC_DAILY_CU_LIMIT` | `1500000` | Estimated Alchemy compute-unit budget per process per UTC day. |
| `QED_RPC_DAILY_CU_LIMIT_ON_DEMAND` | `500000` | Portion of the daily budget reserved as the on-demand read ceiling. |
| `QED_ON_DEMAND_REQUESTS_PER_MINUTE` | `300` | Process-wide eligible on-demand HTTP/tool-call admission cap per minute, shared across clients and routes. |

Without an API key, QED reads the public address catalog linked from Ondo's documentation and its CSV. Accepted snapshots and a separate last-attempt marker use Postgres when configured, with a local working cache in `QED_DATA_DIR`. The attempt is captured before the network fetch, records the QED version, and suppresses another fetch for 24 hours in that version; a version change or corrupt marker permits an immediate fetch. During a same-version lock, QED uses the accepted snapshot if valid.

Daily CU accounting is process-local: counters reset at UTC midnight and are lost on restart; each replica has its own limits. CU estimates use Alchemy's [compute-unit weights](https://www.alchemy.com/docs/reference/compute-unit-costs); unknown methods default to 26 CU. Exhausted daily budgets reject synchronous HTTP chain-read APIs with `503` (MCP reports a tool error); the on-demand admission cap returns `429`. Only eligible chain-read HTTP requests and validated MCP `tools/call` consume admission; MCP discovery/list/ping/verify and crawler token cache reads do not. Startup and registry-triggered stats work uses background CU accounting. Provider-specific RPS and daily CU settings remain separate.

Cached Guard results remain available. Automated crawlers receive cached Guard and token-detail data only: a Guard cache miss returns `403`, while token-detail pages stay `200` and show cached or unavailable powers when uncached or budget-blocked. Exact `/assets/*` views never initiate chain reads or prefetch, for people or crawlers; use their Guard action for an explicit read. All asset views, observation details and exports share the per-client read limiter. Robots excludes Guard/API/review paths while `/tokens/` remains crawlable. Statement wallet addresses are validated against every selected chain before any reads.

Per-client request limits use the rightmost `X-Forwarded-For` address. The production load balancer must append the client address at the right; do not expose the app directly to untrusted clients.

## Primary storage

With `QED_DATABASE_URL`, Postgres is authoritative for signed attestations, registry and Ondo snapshots/attempt markers, leaderboard and publisher-watch observations, featured pools, signed statistics, and asset observations/projections. History startup hydrates only one metadata projection per asset/facet, not the journal. It retains at most 512 recent observation summaries, without evidence or signed documents; history RAM therefore grows with distinct asset/facet count, not observation count. Published registry/board snapshots and bounded serving caches still occupy memory. Wallet statements, wallet-specific Guard documents, prices, RPC counters and ordinary read caches remain memory-only; the development signing key remains a private local file.

| Durable data | Before 1.1.0 | With Postgres |
| --- | --- | --- |
| Signed attestations and quarantine | `attestations/*.json`, local quarantine or S3 `attestations/` | Exact original bytes, indexed ID/chain/pool/time/expiry, SHA-256, quarantine and retention tombstones |
| Registry, public Ondo catalog and daily attempt | Runtime JSON files | Postgres snapshots; files are working caches and legacy fallback |
| Leaderboard, publisher watch, featured pools | Local JSON and S3 `discovery/` snapshots | Postgres snapshots; watch observations remain part of the leaderboard document |
| Signed statistics | Rebuilt in memory | Exact signed snapshot in Postgres, restored without an RPC refresh |
| Asset journal and facet projection | Slice 11 Postgres tables plus JSONL outbox | One observation document plus queryable metadata; lean asset/facet projection; oversized documents reference checked durable blobs |

The importer runs in the background, uses persisted disk/S3 checkpoints and bounded S3 pages/concurrency, and resumes idempotently after interruption. S3 resumes with `StartAfter` the last committed object key, never a stored opaque continuation token. It walks legacy attestation objects directly rather than trusting stale S3 indexes/repair manifests. Records must have matching filenames/content IDs, valid signatures and trusted configured signers, including those listed in `QED_PREVIOUS_KEYS`; invalid or unknown-signer records remain quarantined. Expired valid proofs retain their historical bytes and times. Legacy reads remain available while import is incomplete. Thirty-day attestation pruning creates database tombstones so fallback and backfill cannot resurrect removed records; it never deletes asset history.

Writes first capture a per-key, latest-write-wins local outbox in `QED_DATA_DIR/storage-outbox`; only the background worker commits to Postgres. The outbox is bounded to 50,000 records / 512 MiB, with a 32 MiB per-object cap. Its capture mutex never spans database or S3 network I/O. A five-second worker probes/replays after outages; request reads skip the database while degraded and individual database calls have three-second deadlines. Startup attestation query failures use disk/outbox instead of aborting the listener. Leaderboard/featured working files advance with captured writes; outage restart prefers the latest outbox/disk board and never restores the pre-cutover S3 board. Unavailable or dated cached data remains explicitly degraded. `/healthz` includes `storage.primary`, `storage.degraded`, `storage.capture_gap` and `history_degraded_since` without waiting for the database. Disk failure or a full outbox can create a capture gap; `storage-capture-gap` remains sticky across reconnect/restart and needs operator review.

JSON above `QED_DB_BLOB_MAX_BYTES` uses content-addressed `attestations/blobs/` or `discovery/blobs/` keys in the existing bucket. Postgres stores the key and SHA-256 and reads verify the bytes. Without S3, oversized writes remain in the bounded outbox and storage reports degraded; they are not silently forced inline. For an S3-free local demo with multi-MiB registry snapshots, explicitly raise the threshold (for example `33554432`).

Set `QED_DATABASE_URL` to the PostgreSQL 17 database URL; managed databases should use `sslmode=verify-full`. `QED_DATABASE_CA_FILE` optionally adds a CA bundle, such as the bundled Amazon RDS CA bundle. The application can run embedded migrations on an existing database.

Each process uses up to eight database connections (four storage and four history). Deployment-specific database placement, credentials, network access and migration approval belong to the operator.

The image ships the public [Amazon RDS global CA bundle](https://truststore.pki.rds.amazonaws.com/global/global-bundle.pem) at `/etc/qed/rds-global-bundle.pem`. The committed `rds-global-bundle.pem` is build-pinned to SHA-256 `90a6f1a7978a1b1b096dca6c57511ca48d3b8cf052f8e704baf358f34ea2f21b`. Its roots supplement system roots; `sslmode=verify-full` retains chain and hostname checks. Never use an insecure verifier.


## API and routes

The machine-readable entry points are `/llms.txt`, `/llms-full.txt`, `/openapi.json`, `/api`, `/mcp`, and `/.well-known/mcp/server-card.json`. `POST /mcp` is stateless Streamable HTTP with exactly seven tools: `qed_check` compares an issuer claim, `qed_powers` reports observed token controls, `qed_wallet` reads holdings, `qed_statement` creates a signed wallet statement, `qed_registry_lookup` lists issuer deployments, `qed_verify` checks signed documents, and `qed_guard` returns the combined signed review; `GET /mcp` returns 405. The public docs include `/docs`, `/docs/llm`, `/docs/api-quick-start`, `/about`, `/security`, `/changelog`, and `/blog`; `/api` is generated from OpenAPI. Statement pages support signed JSON and CSV download, record verification, browser Print/Save as PDF, and re-run with comparison; records are public to anyone with the link and retained in process memory for up to 24 hours.
Supported chain values use the same lowercase slugs in routes, JSON, and OpenAPI: `solana`, `robinhood`, `base`, `ethereum`, and `bnb`. QED also verifies existing signed records that used the previous chain-variant spellings.

`GET /stats` and `GET /api/stats?page={page}` summarize leaderboard counts, issuer-registry coverage, and publisher-catalog observations. Statistics distinguish unsupported pool venues from checks that have not run. The shared six-hour discovery pass rechecks the top 50 leaderboard pools by reported 24-hour volume plus featured pools; other existing leaderboard rows are checked after 24 hours, retain their real `checked_at`, and new rows get an initial read. Publisher-watch successful scans are at least 24 hours apart (the scan is retried on the next six-hour discovery pass after a source failure), with limits of 50 searches and 50 Guard evaluations including rechecks. Each scan reserves two searches for each of the 10 highest-volume active issuer tickers and rotates up to 30 remaining searches through the registry tail; Guard evaluations prioritize ticker rank and then higher-volume pairs. Powers warming refreshes top-25 leaderboard tickers every 100 minutes and cold groups after four warm intervals (400 minutes, about 6 hours 40 minutes) or immediately when uncached. Complete powers observations and published-contract metadata for background watch/statistics are cached for six hours; direct user Guard metadata reads remain live. Exact on-chain symbol-and-name matches absent from a published deployment catalog are coverage observations, not conclusions about intent. The watch retains up to 128 supported and 256 unsupported observations, evicts oldest-last-seen rows first, and counts evictions. Supported observations retain only identity-determining reads at the queried token (EVM `symbol()`/`name()` calls or Solana metadata reads), with a registry-source snapshot hash; each retained read's params and raw result are capped at 512 serialized bytes. `evidence_truncated` marks omitted reads and clipped read or text details. Overlong labels, reasons, on-chain values, and observation timestamps are clipped rather than rejected; a supported observation is rejected only for an invalid chain, token address, or catalog hash. Full Guard evidence remains available at `guard_url`. Unsupported candidates are clipped the same way and rejected only for an invalid chain id or token address; rejected rows are counted and dropped; signed summaries report counts while signed inputs contain the bounded compact evidence once. API pages hold up to 50 rows per input. The HTML table shows at most 20 most recently seen entries. DexScreener volume is not independently verified.

Publisher-watch tiers use the same domain publisher-product classifier as Guard. `confirmed` and `caution` retain their existing signed string format; an absent `tier` means classification is pending, never implicit Caution. `tier_derived: true` records an actual Guard/classifier result. Legacy missing/unknown tiers and pre-1.1.1 defaulted Caution are re-derived at startup and on the existing hourly registry/statistics refresh. A failed or budget-rejected metadata read retains only a previous derived tier (legacy explicit Confirmed was never a default); otherwise `/copies` and `/stats` show Pending and retry. A valid exact-publisher match can still confirm a copy when a different candidate publisher's read fails. CSV leaves the tier cell empty while pending. Old signed snapshots remain verifiable; their bytes are never rewritten.

Leaderboard prices and contract checks have separate timestamps. The page shows the price update time and each verdict's last-confirmed UTC time in its tooltip; asset pages show that confirmation time explicitly. Version 1.1.0 uses these dated last-confirmed states rather than a separate 24-hour stale marker. A retained status does not extend a signed record's expiry: verify or re-check the record for freshness. Any row whose `read_status` is not `checked` retries on the next six-hour discovery pass regardless of rank.

A failed individual watch search is logged and skipped while the scan continues. If every watch search fails or returns no pairs, QED does not record a fresh scan: retained observations and `last_scanned_at` stay from the last successful search, `source_unavailable_since` records when the outage began, and the stats headline says the impostor search source is unavailable since that time. One-character tickers are searched only in their ticker+x form because DexScreener rejects one-character queries.

If market-data searches return pairs but every attempted Guard read fails, QED retains the observations and last successful scan time, clears `source_unavailable_since` because the search source responded, and retries during the next discovery pass. No search-source outage message is shown.

Market-data calls query DexScreener first for discovery, featured pools, prices, and publisher-watch searches. GeckoTerminal is used after a DexScreener error or a cached USDC canary returning no pairs (five minutes); a legitimate empty query does not trigger a second request. GeckoTerminal calls wait for the process-wide 10-per-minute budget and back off for 60 seconds on `429`, with 10-second timeouts and a 4 MiB response cap. Each registry refresh is capped at 30 HTTP attempts and 300 distinct validated pools. Watch fallback searches each distinct active registry product name for the top 10 tickers across `robinhood`, `solana`, `eth`, `base`, `bsc`, `arc`, and `ton`, capped at 70 HTTP attempts per scan; unsupported networks are counted, not judged. Market rows identify their source, report volume and liquidity as provided by that source, and link to its exact pool page (`https://www.geckoterminal.com/{network}/pools/{pool}` for GeckoTerminal). The stats page and site footer attribute “Market data: DexScreener / GeckoTerminal”; see the [CoinGecko API Terms](https://www.coingecko.com/en/api_terms).
Each refresh budgets at most four `/tokens/multi` and two `/pools/multi` HTTP attempts per supported network, with no more than 60 distinct pool IDs per network.

`POST /api/guard` accepts `{ "address": "…", "chain": "base", "wallet": "…" }`; prefer it over the legacy `GET /api/guard/{address}?chain=…&wallet=…` because query-string wallets may be exposed in browser history or request URLs. The Guard `reasons[].code` enum is `publisher_contract_match`, `publisher_contract_mismatch`, `name_resembles_registry_entry`, `publisher_metadata_unavailable`, `no_publisher`, `registry_stale`, `registry_removed`, `token_paused`, `powers_incomplete`, `powers_unavailable`, `wallet_check_not_applicable`, `wallet_check_unavailable`, `wallet_frozen`, `wallet_blocked`, `wallet_sanctioned`, `source_unverified`, `source_unavailable`, `pool_unavailable`, and `no_contract_at_address`.

`publisher_metadata_unavailable` means QED could not read complete current publisher metadata for a resembling registry candidate; it is not a mismatch.

| Method | Route | Result |
| --- | --- | --- |
| GET | `/` | Home page and live pools. |
| GET | `/stats` | Leaderboard, registry, and publisher deployment-catalog watch statistics, with confirmed/caution tiers, explicit pending classification, volume totals, method, and scan time. |
| GET | `/stats.json`, `/stats.csv` | Download the signed point-in-time `kind: "stats"` snapshot as JSON or CSV (CSV embeds the exact signed JSON document). |
| GET | `/check` | Permanent redirect to `/guard`. |
| GET | `/registry`, `/registry/table` | Registry directory and table fragment. |
| GET, POST | `/guard` | Signed Guard review form and submission. |
| GET | `/guard/{chain}/{address}` | Human-readable signed Guard review result. |
| GET | `/tokens?ticker={ticker}`, `/tokens/{ticker}`, `/chains/{chain_name}` | Canonical ticker lookup plus token and chain directories. |
| GET | `/assets/{chain}/{kind}/{address}` | Exact pool/token facts and public observation timeline; `kind` is `pool` or `token`; `?days=1` (24h), `7`, `30`, or `90`. The default is the smallest window containing recorded data, capped at 90 days. |
| GET | `/assets/{chain}/{kind}/{address}/history.csv`, `/assets/{chain}/{kind}/{address}/history.json` | Paginated public observation metadata, **not a signed attestation**; `days`, `limit` (1–200), `before_time` and `before_id`; next-page URL in `Link`, also `next` in JSON. Retrieve full evidence/document through the exact asset's `/observations/{id}` route. |
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
| GET | `/validated`, `/validated/{chain}/{subject}` | Existing validated directory and permanent exact-address redirects to Guard; unchanged. |
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

## Asset history and stable status

Pool contract-match, token controls and public wallet-free Guard have separate state per exact asset. EVM hex is canonicalized; Solana base58 remains case-sensitive. Real completed results on any path apply immediately. Only scheduled attempts advance public state, at most once per asset/facet/UTC hour, including a due attempt blocked by the daily budget: one or two misses retain the result and original confirmation time; the third displays **Unverified**, with the historical confirmation retained beneath. On-demand misses remain journalled without advancing that counter. On-demand zero-provider budget/admission/crawler rejections are not misses; attributed rejections are labelled `rejected`. Non-confirming on-demand results for untracked assets remain journal-only, without creating public state. This includes every Guard `Unknown`, including `no_contract_at_address`; Guard `Allow` and `Deny` still create or advance state immediately. Registry staleness/removal and unsupported venues are not RPC misses. Partial controls remain visible alongside explicitly dated prior controls. Cache hits, shared featured/leaderboard consumers and the token page's 2.5-second render deadline do not add observations.

Failed supported facets retry hourly at their known chain/address, without DEX search. An exhausted daily budget prevents provider access but records one miss for the due UTC-hour slot, never one miss per worker tick. Scheduled Guard retries bypass a cached allow/deny. Healthy 6-hour hot and daily cold cadences are unchanged. Last-confirmed display state never changes a signed attestation, Guard document, stats snapshot, or expiry; `/stats` labels its unsigned retained-state summary separately from latest-attempt signed counts.

Exact asset pages lead with a large read-status badge, three miss dots, human-readable confirmation/attempt times, facts and copy/export/Guard/verification buttons. A token’s **Controls observed** badge describes a completed controls read, not a Verified identity claim; registry ticks are separate. Healthy next-check estimates span the existing 6–24-hour cadence, not a new hourly promise. Ticker pages show per-contract chain chips, one combined contract/pool timeline and a compact, mobile-stacked address table. Timeline rows stay horizontal: close points merge with a count and all constituent outcomes in the title, with every record still available in the details table. Only present shapes appear in the legend.

Chain-detection transport failures are journalled only for already-known asset/facet identities on the chain attempted. Unknown-chain failures and empty-bytecode probes never create phantom asset states. Wallet-specific Guard documents are excluded; independently executed wallet-free controls reads can still enter their own public facet.

With `QED_DATABASE_URL`, either store applies the same ordered, embedded, checksummed migrations `0001_asset_history.sql` through `0006_guard_unknown_tracking.sql` under a shared Postgres advisory transaction lock. Migration 3 removes duplicate document columns; migration 4 independently ensures the storage recent-record index; migration 5 repairs scheduled budget slots and journal-only projection eligibility. Migration 6 removes legacy Unknown-only public state for assets without a confirming result or scheduled attempt, retaining their entire journal and signed documents. Startup hydrates lean asset/facet state in 512-row keyset pages, with per-query deadlines; an interrupted initial attempt retries in the background. History pages/exports query at most 200 metadata rows plus one keyset look-ahead; full observation detail is loaded only on request. These database reads have three-second deadlines. The database must already exist. Non-loopback hosts require TLS; rustls 0.23 uses system roots plus `QED_DATABASE_CA_FILE` with full certificate/hostname verification, including RDS `sslmode=verify-full`. The connection URL is not logged.

Each new event is durably appended to `QED_DATA_DIR/asset-history-outbox.jsonl` before capture is acknowledged. A background writer replays FIFO using unique `event_id`, updating the observation and eligible projection in one transaction. Database outages do not stop serving the prepared memory projection. Pending payloads stay on disk; only summaries and spool offsets remain in memory. The outbox is capped at 50,000 events / 512 MiB, with a 32 MiB event cap; disk failure/full continues serving but explicitly marks history recording degraded, so a capture gap is possible. Database-unavailable exports return 503, not an empty success. No observation TTL/rollup is applied.

An invalid/truncated outbox is preserved for local repair rather than overwritten, and serving continues degraded. A possible capture gap creates an `asset-history-outbox.gap` marker (best effort if the disk itself is unavailable); that warning survives normal database recovery and restart. A capture-gap notice does not block healthy database history, inventory, detail or export reads; JSON exports expose `recording_degraded_since`. A separate database-unavailable circuit gates reads after database failure and clears after successful replay/probing. Clearing a known capture-gap warning requires operator review, not automatic success after reconnect.

Both outboxes need durable local storage to survive task replacement; ephemeral container disks do not provide cross-task durability. Durable outbox volume is an operator decision. QED remains fail-open if capture is impossible and reports the resulting degradation or capture gap. Without a database, only the latest 512 observation summaries are available in memory and reset on restart; full evidence requires durable history.

Local backfill (no S3 access):

```text
QED_DATA_DIR=/path/to/runtime QED_DATABASE_URL=<approved-database-url> \
  qed history-backfill --dir /path/to/local/attestations
```

The configured current signer and explicitly configured `QED_PREVIOUS_KEYS` form the trusted public-key allowlist. The signed development/production environment must also match. Never populate that allowlist from the files being imported. Unset `QED_ATTEST_BUCKET` for this local-only command. The importer reports imported/duplicate/rejected counts and rejection categories, verifies signatures and content IDs, preserves original JSON bytes, true times and valid expired records, and is idempotent. Existing bytes and retention tombstones are never overwritten. It imports pool facts only, never fabricating token-power reads or historical misses. Run Postgres integration coverage with `QED_TEST_DATABASE_URL`; without it, integration tests skip database work.


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
