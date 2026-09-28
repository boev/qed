# QED

QED is a read-only contract comparison and directory for stock-paired liquidity pools. It checks whether a pool uses the stock-token contract published by its issuer, based on the observed quote contract and QED's issuer registry, and publishes a signed, reproducible record when the check can produce a certificate. It does not prove backing or custody, and it does not verify reserves, solvency, safety, price, liquidity, or endorsement.

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

## Public routes

Examples below use `https://qed.example`; replace it with the deployed public URL.

- `GET /healthz` returns `{"status":"ok"}`.
- `GET /` returns the home page and live pools.
- `GET, POST /check` serves the check form and result fragment.
- `GET /wallet` serves the wallet holdings form.
- `GET /registry` and `GET /registry/table` serve the issuer registry and table fragment.
- `GET /tokens/{ticker}` and `GET /chains/{chain_name}` serve token and chain directories.
- `GET /glossary` and `GET /guide/verify-a-stock-token` serve explanatory pages.
- `GET /validated` and `GET /validated/{chain}/{subject}` serve current contract matches and pool summaries.
- `GET /validated.xml` returns the current contract-match feed.
- `GET /v/{id}` serves a signed certificate; `POST /v/{id}/recheck` re-runs its recorded reads.
- `GET /imprint`, `/privacy`, and `/terms` return the legal pages.
- `GET /robots.txt` and `/sitemap.xml` return crawler metadata.
- `GET /api` returns the HTML API guide.
- `GET /llms.txt` and `GET /llms-full.txt` return machine-readable service guides.
- `GET /openapi.json` returns the OpenAPI 3.1 route document.
- `GET /pools/featured` returns the featured-pool HTML page.
- `GET /api/registry` returns the issuer registry JSON.
- `GET /api/pools/featured` returns curated pools.
- `GET /api/leaderboard?page=1&per=50&sort=volume&dir=desc` returns the ranked page and global ranks.
- `GET /api/prices?ids=solana:POOL` returns current price data.
- `GET /api/status` returns registry, leaderboard, featured, and price freshness state.
- `GET /api/check/{address}` checks a token or pool and returns a point-in-time `CheckResult`.
- `POST /api/wallet` with `{"address":"…"}` in the request body checks stock-token holdings without putting an address in the URL. Results are not cached by address.
- `GET /api/attest/{id}` returns a signed attestation JSON document.
- `POST /verify` with an attestation JSON body returns `ok`, `cryptographic`, `trusted_signer`, `environment_match`, and `fresh` booleans.
- `GET /.well-known/qed.json` returns the public signing key and algorithm metadata.

For example:

```text
curl https://qed.example/healthz
curl 'https://qed.example/api/check/POOL_OR_TOKEN_ADDRESS'
curl https://qed.example/api/attest/ATTESTATION_ID
curl https://qed.example/openapi.json
```

## Human pages
- [Home](https://qed.example/)
- [Check](https://qed.example/check)
- [Issuer registry](https://qed.example/registry)
- [Current contract matches](https://qed.example/validated)
- [Token directory](https://qed.example/tokens/NVDA)
- [Chain directory](https://qed.example/chains/solana)
- [Glossary](https://qed.example/glossary)
- [Contract verification guide](https://qed.example/guide/verify-a-stock-token)
- [Current contract-match feed](https://qed.example/validated.xml)
- [Imprint](https://qed.example/imprint)
- [Privacy](https://qed.example/privacy)
- [Terms](https://qed.example/terms)

QED uses plain-text `Checked on QED` links to public records rather than badges or embeds. Plain-text destinations are inspectable and reduce badge-impersonation risk. Compare the exact pool and contract address before using a record or its external trade link.

## Registry sources and limits

The committed seed is `registry/registry.json`. Runtime adapters refresh xStocks, Ondo, and Robinhood data into the writable data directory. Current issuer coverage is limited to entries present in those sources; configured readers for Solana, Robinhood Chain, Base, Ethereum, and BNB Chain do not imply that every issuer has a registry contract on every chain. Public checks are rate-limited and share a bounded concurrency pool. Leaderboard pages return at most 50 rows, price queries cap IDs and input bytes, registry filters cap query length, and persisted attestation/index data is bounded. A missing or uncertain read is never presented as verified.

## Legal and project links

QED's [Imprint](https://qed.example/imprint), [Privacy](https://qed.example/privacy), and [Terms](https://qed.example/terms) pages describe the service and data handling. The public source repository is [github.com/boev/qed](https://github.com/boev/qed).
