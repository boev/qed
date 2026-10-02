# QED

QED is a read-only contract-to-issuer-registry checker and directory for stock-paired pools. It checks whether a pool uses the stock-token contract published by its issuer. It does not prove backing or custody, and it does not verify reserves, solvency, safety, price, liquidity, or endorsement.

Source repository: [github.com/boev/qed](https://github.com/boev/qed). Live website: [qed.web3-energy.com](https://qed.web3-energy.com).

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

The machine-readable entry points are `/llms.txt`, `/llms-full.txt`, `/openapi.json`, `/api`, and `/mcp`. `/mcp` is a stateless Streamable HTTP endpoint for the check, wallet, registry-lookup, and attestation-verification tools; it supports MCP versions `2026-07-28`, `2025-11-25`, `2025-06-18`, and `2025-03-26` (legacy initialization). The deployed service serves those paths directly. The public source repository is [github.com/boev/qed](https://github.com/boev/qed).

| Method | Route | Result |
| --- | --- | --- |
| GET | `/` | Home page and live pools. |
| GET, POST | `/check` | Check form and result fragment. |
| GET | `/registry`, `/registry/table` | Registry directory and table fragment. |
| GET, POST | `/wallet` | Wallet holdings form and read-only holdings result. |
| GET | `/tokens?ticker={ticker}`, `/tokens/{ticker}`, `/chains/{chain_name}` | Canonical ticker lookup plus token and chain directories. |
| GET | `/glossary`, `/guide/verify-a-stock-token` | Glossary and contract verification guide. |
| GET | `/llms.txt`, `/llms-full.txt` | Machine-readable product and API guides. |
| GET | `/validated`, `/validated/{chain}/{subject}` | Current contract-match directory and pool summary; records are time-bounded and should be re-checked after expiry. |
| GET | `/validated.xml` | RSS feed of current contract matches. |
| GET | `/v/{id}` | Signed certificate page with nerd mode. |
| POST | `/v/{id}/recheck` | Re-run the recorded reads. |
| GET | `/api` | HTML API guide. |
| GET | `/openapi.json` | OpenAPI 3.1 route document. |
| POST | `/mcp` | Stateless MCP Streamable HTTP tools for checks, wallet holdings, issuer-registry lookup, and attestation verification. |
| GET | `/pools/featured` | Featured-pool HTML fragment/page. |
| GET | `/api/check/{address}` | JSON check result. |
| POST | `/api/wallet` | JSON body `{ "address": "…" }`; check stock-token holdings without putting the address in the URL. |
| GET | `/api/attest/{id}` | Signed attestation JSON. |
| GET | `/api/registry`, `/api/pools/featured` | Registry and featured pool JSON. |
| GET | `/api/leaderboard`, `/api/prices`, `/api/status` | Ranked pools, prices, and freshness state. |
| POST | `/verify` | Verify an attestation payload and signature. |
| GET | `/.well-known/qed.json` | Public signer metadata and key. |
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

- `src/main.rs`: configuration, startup, refresh loops, and HTTP listener.
- `src/config.rs`: environment configuration and defaults.
- `src/state.rs`: shared application state and request limiter.
- `src/chain.rs`: address detection and chain identity.
- `src/registry/`: issuer adapters, canonical registry loading, merging, and lookup.
- `src/pool/`: chain readers for Solana and EVM JSON-RPC.
- `src/check.rs`: pool reads, registry comparison, and verdict construction.
- `src/attest.rs`: canonical payloads, signatures, stores, and re-checks.
- `src/discovery.rs`: shared pool discovery, featured-pool curation, leaderboard ranking, and price refresh.
- `src/web/mod.rs`: router, middleware, cache headers, robots, and sitemap.
- `src/web/discoverability.rs`: LLM guide, API guide, RSS feed, and crawler output.
- `src/web/openapi.rs`: hand-written OpenAPI 3.1 route document.
- `src/web/pages.rs`: HTML route handlers and certificate pages.
- `src/web/api.rs`: JSON routes, verification, and public-key metadata.
- `src/web/views.rs`: Askama view models and display formatting.
- `static/`: CSS, JavaScript, icons, logo, favicon, and OpenGraph PNG.
- `release/llms-full.md`: source Markdown included in `/llms-full.txt`.

## Registry sources

The seed is `registry/registry.json`. Runtime refresh adapters read xStocks, Ondo, and Robinhood sources, merge entries by chain and contract, and write the refreshed copy below `QED_DATA_DIR`. Current issuer coverage is limited to entries actually present in those registry sources; configured readers for Solana, Robinhood Chain, Base, Ethereum, and BNB Chain do not imply that every issuer has a contract on every chain. To add an issuer, add an adapter under `src/registry/`, map its response to every `Entry` field, add a focused fixture and test, then regenerate and sort the seed by issuer, chain, and ticker. Never edit the seed from the running service.

## Licence

Copyright © 2026 Web3 Energy Ltd.

Apache License 2.0. See [LICENSE](LICENSE).
