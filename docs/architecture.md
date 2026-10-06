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

    C --> RPC[Solana and EVM RPC providers]
    W --> RPC
    G -->|publisher, powers, pools, optional wallet| RPC
    D --> RPC
    G --> S
    T --> RPC
    T --> S

    S --> OBJ[Attestation store and bounded index]

Ordinary page and API reads use prepared in-memory snapshots. They do not trigger pool discovery or blockchain reads unless the route explicitly performs a check, Guard review, wallet scan, token-power observation, signed wallet statement, re-check, or verification operation. Token pages wait up to 2.5 seconds for concurrent token-power observations, then render completed cached results and mark unresolved observations unavailable while deduplicated background reads continue. Signed wallet-statement creation is independently bounded to 15 seconds; its EVM block selector is exact, while Solana uses `minContextSlot` and records the returned slot rather than claiming an exact historical snapshot.

The powers warm pass is low priority and reports `warmed=false` when there are no targets, so an empty startup pass does not suppress later discovery-triggered work. It covers featured tickers first, then leaderboard tickers by rank, admitting whole ticker groups up to 160 contracts. Eligible passes run at startup, after registry changes and discovery refreshes, and every 25 minutes; records at least five minutes old are refreshed before the 30-minute cache TTL. Summaries include covered ticker count, cap status, per-chain results, source-unavailable counts, and the top transient reason codes. A Sourcify/source-unavailable result does not discard successfully read on-chain powers.
Complete on-chain records remain cached for 30 minutes even when `source_verified: unavailable`; warm refreshes revisit them before expiry.

Complete observations use the 30-minute cache, while incomplete RPC reads and hard failures are retained for 30 seconds before retry.

The shared discovery pass fetches candidate pairs once and derives both the leaderboard and featured-pool candidates from that response. Results common to both lists reuse the same on-chain check result. A failed refresh preserves the previous valid snapshot.

| Work | Normal cadence | Failure behavior |
| --- | --- | --- |
| Issuer registry refresh | 1 hour | Retain the last accepted source data and record the source failure. |
| Shared pool discovery | 1 hour | Retain the previous leaderboard and featured pools. Retry after 30 seconds when the initial refresh attempt is blocked. |
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
- upstream HTTP requests have a 30-second timeout.

`POST /api/statement` and the plain HTML `POST /statements` form use the shared 60-requests-per-client window and expensive-work semaphore; the form calls the same statement API handler and redirects to its signed page. The semaphore remains held through the complete chain read and signing response. `POST /mcp` uses the same router middleware and expensive-work concurrency boundary for tools that perform chain reads. `GET /api/guard/{address}`, `POST /api/guard`, `GET /guard/{chain}/{address}`, and `POST /guard` use that same boundary and the shared Guard application flow; each review has a 15-second deadline that includes reader selection, pool detection, cache and lock waits, power/source probes, and wallet restriction reads. Guard uses route-level expensive concurrency without acquiring a second power-prefetch permit. `GET /api/powers/{address}` and `qed_powers` accept any supported-chain token contract and apply a 15-second deadline to chain detection, cache and lock waits, source probes, and the shared permit.

The wallet path scans supported chains concurrently under one 20-second request deadline. An incomplete scan is returned as an error instead of a silently partial portfolio.

## Module map and dependency rule

- `src/main.rs` composes configuration, adapters, background tasks, and the listener; `src/config.rs` parses environment inputs.
- `src/ports.rs` defines chain, registry, signer, cache, storage, and source-verification boundaries.
- `src/domain/` owns chain/data models and pure matching, power, Guard, attestation, and statement rules.
- `src/app/` owns check, wallet, powers, Guard, attestation, and statement application flows; `warm.rs` schedules powers observations.
- `src/adapters/evm.rs` and `solana.rs` implement chain reads; `registry.rs` fetches and atomically publishes issuer data.
- `src/adapters/state.rs` composes runtime state, caches, rate limits, and concurrency controls.
- `src/adapters/web.rs` owns routes and middleware; `/docs`, `/docs/llm`, `/docs/api-quick-start`, `/api`, `/statements`, `/guard`, `/mcp`, `/api/statement`, `/api/guard`, `/api/guard/{address}`, `/api/powers/{address}`, and `/verify` use this shared router.
- `src/adapters/web/api.rs` and `pages.rs` adapt REST and HTML requests; `views.rs` owns presentation models and formatting.
- `src/adapters/web/mcp.rs` implements the Streamable HTTP JSON-RPC transport and QED tools, including `qed_guard`.
- `src/adapters/web/docs.rs` serves discoverability, API guidance, and OpenAPI; `src/adapters/content.rs` renders Markdown and blog and changelog feeds; `src/bin/qed-healthcheck.rs` is the standalone probe.

Dependency rule: `domain` depends on neither `app` nor adapters; `app` depends on domain models and ports, never on HTTP, templates, chain SDKs, or concrete adapters; adapters implement ports and compose application behavior.

## State and persistence

Memory contains rebuildable caches and the currently published registry, leaderboard, featured pools, prices, and recent attestation index. Durable attestation storage is selected by configuration. On startup QED restores available snapshots, verifies persisted attestations before indexing them, and quarantines invalid records.

A signed attestation proves the integrity and signer attribution of one point-in-time record. It does not prove custody, reserves, solvency, safety, price, endorsement, or correctness of an issuer registry.