# Runtime architecture

QED checks tokens that claim to be something against the contract their issuer publishes. QED is one Rust process. It serves the website and API, refreshes public pool data, performs direct chain reads, and signs point-in-time attestations. It is read-only with respect to supported chains and never submits transactions.

## Process and background work

```mermaid
flowchart TB
    U[Browser or API client] --> H[QED HTTP server]

    subgraph Q["One QED Rust process"]
        H --> M[In-memory registry, leaderboard, featured pools]
        H --> C[Direct pool checker]
        H --> W[Wallet scanner]
        H --> G[Signed Guard reviewer]
        H --> S[Ed25519 attestation, statement, and Guard signer]
        H --> T[Signed wallet statement reads]

        R[Registry refresh] -->|publish atomically| M
        R -->|sleep 1 hour| R

        D[Shared pool discovery] -->|publish leaderboard and featured pools| M
        D -->|sleep 1 hour| D

        P[Price refresh] -->|update current rows| M
        P -->|sleep 5 minutes| P

        X[Attestation expiry] -->|remove older than 30 days| X
        X -->|sleep 24 hours| X
    end

    R --> ISS[Issuer registries]
    D --> DEX[DexScreener]
    P --> DEX
    D -->|on error or unavailable five-minute USDC canary| GT[GeckoTerminal]
    P -->|on error or unavailable five-minute USDC canary| GT

    C --> RPC[Solana and EVM RPC providers]
    W --> RPC
    G -->|publisher, powers, pools, optional wallet| RPC
    D --> RPC
    G --> S
    T --> RPC
    T --> S

    S --> OBJ[Attestation store and bounded index]

Ordinary page and API reads use prepared in-memory snapshots. They do not trigger pool discovery or blockchain reads unless the route explicitly performs a check, Guard review, wallet scan, token-power observation, signed wallet statement, re-check, or verification operation. Token pages wait up to 2.5 seconds for concurrent token-power observations, then render completed cached results and mark unresolved observations unavailable while deduplicated background reads continue. Signed wallet-statement creation is independently bounded to 15 seconds; its EVM block selector is exact, while Solana uses `minContextSlot` and records the returned slot rather than claiming an exact historical snapshot.

The powers warm pass is low priority and reports `warmed=false` when there are no targets, so an empty startup pass does not suppress later discovery-triggered work. It orders leaderboard tickers by reported 24-hour volume, with featured-only tickers following, and admits whole ticker groups up to 160 contracts. The top 25 registry tickers are considered on each 100-minute pass and after registry/discovery notifications; hot groups refresh every 100 minutes, while other groups are eligible on first read and after four warm intervals (400 minutes, about 6 hours 40 minutes). Complete powers records remain cached for six hours, including `source_verified: unavailable`, so user-triggered powers and Guard requests can reuse a cached observation for up to six hours. Summaries include covered ticker count, cap status, per-chain results, source-unavailable counts, and the top transient reason codes. A Sourcify/source-unavailable result does not discard successfully read on-chain powers.
Incomplete RPC reads and hard failures are retained for 30 seconds before retry.

The shared discovery pass fetches candidate pairs once and derives both the leaderboard and featured-pool candidates from that response. Results common to both lists reuse the same on-chain check result. A failed refresh preserves the previous valid snapshot.

The 50 highest-volume leaderboard pools and featured pools retain the six-hour chain-check cadence. Existing cold leaderboard rows are rechecked after four discovery intervals (24 hours), while new rows always receive an initial read; skipped rows retain their actual `checked_at`. The publisher watch also waits four discovery intervals between successful scans. Stats tier re-derivation and background watch reuse complete published-contract metadata for six hours; direct user Guard requests continue reading publisher metadata live.

If market-data queries return pairs but every attempted Guard evaluation fails, retain the previous observations and `scanned_at` without a `source_unavailable_since` value; retry the still-due scan on the next discovery pass. This is not a market-data source outage.

Leaderboard rows whose `read_status` is not `checked` (including RPC limits and unsupported venues) retry on the next six-hour discovery pass regardless of rank. Successfully checked cold rows retry when their last `checked_at` reaches 24 hours. The UI reports price freshness separately and shows each check timestamp, marking checks older than 24 hours stale. Only RPC checks are throttled by the 200 ms inter-check delay.

**Source-derived RPC budget.** The formulas below count on-chain check/observation units, not individual RPC methods; multiply each unit by that chain and venue's reader cost. For each chain `c`, `N_c` is the unique leaderboard-plus-featured pool count (at most 312 total), `H_c` is its top-50/featured hot subset, `P_c` is selected warm contracts in the top 25 ticker groups, and `C_c` is selected cold contracts (`sum(P_c+C_c) <= 160` per pass).

| Chain | Pool check units/day before → after (ratio) | Powers observations/day before → after (ratio) |
| --- | --- | --- |
| Solana | `4N_sol → N_sol+3H_sol` (`4N_sol/(N_sol+3H_sol)`) | `57.6(P_sol+C_sol) → 14.4P_sol+3.6C_sol` (`4(P_sol+C_sol)/(P_sol+0.25C_sol)`) |
| Robinhood Chain | `4N_rh → N_rh+3H_rh` (`4N_rh/(N_rh+3H_rh)`) | `57.6(P_rh+C_rh) → 14.4P_rh+3.6C_rh` (`4(P_rh+C_rh)/(P_rh+0.25C_rh)`) |
| Base | `4N_base → N_base+3H_base` (`4N_base/(N_base+3H_base)`) | `57.6(P_base+C_base) → 14.4P_base+3.6C_base` (`4(P_base+C_base)/(P_base+0.25C_base)`) |
| Ethereum | `4N_eth → N_eth+3H_eth` (`4N_eth/(N_eth+3H_eth)`) | `57.6(P_eth+C_eth) → 14.4P_eth+3.6C_eth` (`4(P_eth+C_eth)/(P_eth+0.25C_eth)`) |
| BNB | `4N_bnb → N_bnb+3H_bnb` (`4N_bnb/(N_bnb+3H_bnb)`) | `57.6(P_bnb+C_bnb) → 14.4P_bnb+3.6C_bnb` (`4(P_bnb+C_bnb)/(P_bnb+0.25C_bnb)`) |

Discovery runs four times/day: all pool rows previously received four checks/day; now hot rows remain at four and cold rows receive one, so the capped board-only rate is 1,200→450 check units/day (2.67× fewer; up to 12 unique featured-only pools change 1,248→498, about 2.51×). Warm passes change from 57.6 to 14.4/day: hot powers groups are 4× less frequent and cold groups, due every fourth pass (400 minutes), are 16× less frequent. The per-chain powers ratio is therefore 4×–16× for any nonempty selected set; the warmer's high per-observation RPC cost makes this the dominant periodic reduction. Publisher watch changes from four successful-scan opportunities/day to one, with at most 50 Guard evaluations across chains per scan (4× fewer). Stats reclassification runs about 32 times/day; complete positive metadata is reused for six hours (up to eightfold fewer repeat reads per stable key), while failed or incomplete metadata still retries. Registry refresh (hourly) and prices (five-minute cadence) are market-data HTTP work; daily attestation pruning is storage work, not chain RPC.

| Work | Normal cadence | Failure behavior |
| --- | --- | --- |
| Powers warm | Top-25 ticker groups every 100 minutes; cold groups every 400 minutes or on first read | Reuse complete powers records for up to six hours; incomplete/hard failures retry after 30 seconds. |
| Issuer registry refresh | 1 hour | Retain the last accepted source data and record the source failure. |
| Shared pool discovery | 6 hours | Retain the previous leaderboard and featured pools. Retry after 30 seconds when the initial refresh attempt is blocked. |
| Publisher watch | At least 24 hours between successful scans; attempted during discovery | Retain last successful observations; report a source outage only when every search fails or returns no pairs, and retry on the next six-hour discovery pass. |
| Stats tier re-derivation | Startup, hourly registry refresh, and discovery refresh | Reuse published-contract metadata cached for six hours. |
| Current-pool prices | 5 minutes | Preserve previous price points; retry a blocked cycle after 1 minute. |
| Attestation expiry | 24 hours | Keep serving and retry on the next run. |

## One user check

```mermaid
sequenceDiagram
    participant U as User
    participant Q as QED
    participant C as 30-second cache
    participant R as Chain RPC
    participant S as Signer
    participant O as Attestation store

    U->>Q: Pool, token, or v4 pool ID
    Q->>Q: Validate input and enforce 60 requests/minute
    Q->>C: Look for canonical cached result

    alt Cache hit
        C-->>Q: Existing signed result
    else Cache miss
        Q->>R: Detect chain and read block/slot
        Q->>R: Read pool structure and balances
        Q->>R: Read token metadata and bytecode
        Q->>Q: Compare addresses with issuer registry
        Q->>S: Sign point-in-time attestation
        S->>O: Store attestation and update bounded index
        Q->>C: Cache result for 30 seconds
    end

    Q-->>U: Verdict, evidence, signature, exact-record URL
```

Concurrent checks for the same canonical subject share one in-flight execution. A completed result remains cached for 30 seconds. Background leaderboard validation has a separate 30-minute cache.

## Request boundaries

QED bounds work before contacting external services:

- protected routes allow 60 requests per client per 60 seconds;
- at most 32 expensive checks run concurrently;
- one wallet scan runs at a time;
- four registry API serializations run concurrently;
- EVM providers are limited to 8 requests per second per chain;
- Solana is limited to 4 requests per second;
- DexScreener requests share a 240-per-minute process budget and a 120-second endpoint backoff after a `429`;
- GeckoTerminal requests use an awaited 10-per-minute process budget, a 60-second backoff before one retry on `429`, a 10-second timeout, and the shared 4 MiB response cap;
- GeckoTerminal registry discovery caps each refresh at 30 HTTP attempts and 300 distinct validated pools; publisher-watch fallback caps each scan at 70 HTTP attempts across distinct top-ticker product names and seven networks.
- The 30-attempt registry budget is divided across the five supported networks: at most four `/tokens/multi` and two `/pools/multi` HTTP attempts each, with at most 60 distinct pool IDs per network.
- upstream HTTP requests have a 30-second timeout.

Discovery, price refresh, and publisher-watch queries use DexScreener first. GeckoTerminal is used only after a DexScreener error or when the cached USDC canary returns no pairs; valid empty query results do not fan out. Each market row records its provider and links to that provider's pool page. Market values are source-reported, and the stats page and site footer attribute “Market data: DexScreener / GeckoTerminal”; see the [CoinGecko API Terms](https://www.coingecko.com/en/api_terms).

`POST /api/statement` and the plain HTML `POST /statements` form use the shared 60-requests-per-client window and expensive-work semaphore; the form calls the same statement API handler and redirects to its signed page. The semaphore remains held through the complete chain read and signing response. `POST /mcp` uses the same router middleware and expensive-work concurrency boundary for tools that perform chain reads. `GET /api/guard/{address}`, `POST /api/guard`, `GET /guard/{chain}/{address}`, and `POST /guard` use that same boundary and the shared Guard application flow; each review has a 15-second deadline that includes reader selection, pool detection, cache and lock waits, power/source probes, and wallet restriction reads. Guard uses route-level expensive concurrency without acquiring a second power-prefetch permit. `GET /api/powers/{address}` and `qed_powers` accept any supported-chain token contract and apply a 15-second deadline to chain detection, cache and lock waits, source probes, and the shared permit.

The wallet path scans supported chains concurrently under one 20-second request deadline. An incomplete scan is returned as an error instead of a silently partial portfolio.

## Module map and dependency rule

- `src/main.rs` composes configuration, adapters, background tasks, and the listener; `src/config.rs` parses environment inputs.
- `src/ports.rs` defines chain, registry, signer, cache, storage, and source-verification boundaries.
- `src/domain/` owns chain/data models and pure matching, power, Guard, attestation, and statement rules.
- `src/app/` owns check, wallet, powers, Guard, attestation, and statement application flows; `warm.rs` schedules powers observations.
- `src/adapters/evm.rs` and `solana.rs` implement chain reads; `registry.rs` fetches and atomically publishes issuer data, retaining all supplied xStocks deployments and wrapper addresses.
- `src/adapters/discovery.rs` runs bounded leaderboard discovery and rotates rate-limited publisher-ticker searches; supported-chain candidates go through the shared Guard flow, while unsupported chains are counted without a verdict.
- `src/adapters/state.rs` composes runtime state, caches, rate limits, and concurrency controls.
- `src/adapters/web.rs` owns routes and middleware; `/docs`, `/docs/llm`, `/docs/api-quick-start`, `/api`, `/stats`, `/stats.json`, `/stats.csv`, `/api/stats?page={page}`, `/statements`, `/guard`, `/mcp`, `/api/statement`, `/api/guard`, `/api/guard/{address}`, `/api/powers/{address}`, and `/verify` use this shared router.
- The statement route family also includes `GET /statements/{id}/download.csv`, `GET /statements/{id}/verify`, and `POST /statements/{id}/recheck`; these use the shared router middleware, and re-check submits through the same statement application flow. `GET /v/{id}/verify` renders certificate verification.
- `src/adapters/web/api.rs` and `pages.rs` adapt REST and HTML requests; `views.rs` owns presentation models and formatting.
- `src/adapters/web/mcp.rs` implements stateless Streamable HTTP JSON-RPC and seven read-only QED tools: issuer check, token powers, wallet holdings, signed statements, registry lookup, document verification, and combined Guard review.
- `src/adapters/web/docs.rs` serves discoverability, API guidance, and OpenAPI; `src/adapters/content.rs` renders Markdown and blog and changelog feeds; `src/bin/qed-healthcheck.rs` is the standalone probe.

Dependency rule: `domain` depends on neither `app` nor adapters; `app` depends on domain models and ports, never on HTTP, templates, chain SDKs, or concrete adapters; adapters implement ports and compose application behavior.

## State and persistence

Memory contains rebuildable caches and the currently published registry, leaderboard, featured pools, prices, and recent attestation index. Durable attestation storage is selected by configuration. On startup QED restores available snapshots, verifies persisted attestations before indexing them, and quarantines invalid records.

A signed attestation proves the integrity and signer attribution of one point-in-time record. It does not prove custody, reserves, solvency, safety, price, endorsement, or correctness of an issuer registry.