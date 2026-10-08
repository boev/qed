# Changelog

Notable QED changes, in reverse chronological order. Entries describe shipped behavior; QED does not prove backing, custody, reserves, solvency, safety, price, liquidity, or endorsement.

## [1.0] - 2026-10-07

**Impostor watch, reliable reads, and easier QED workflows**

- **Guard** — lead with the token-or-pool review, showing issuer identity, powers, source status, and known pools together.
- **Reliability** — separate failed reads from token verdicts, resolve v4 pools through PositionManager pool keys with verified hashes and bounded log fallback, and retry transient observations on the next refresh.
- **Two market sources** — fall back to GeckoTerminal on DexScreener errors or an unavailable USDC canary, label reported values by source, and separately count official deployments on unsupported networks.
- **Publisher watch** — compare exact on-chain symbol-and-name matches against the publisher's full deployment catalog; catalog observations are not conclusions about intent. Reserve searches for the 10 highest-volume issuer tickers and rotate the remaining search budget through the registry tail. Guard evaluations follow that ticker order and prioritize higher-volume pairs within each ticker. Retain only bounded identity-determining reads with the registry-source snapshot hash, mark omitted or clipped evidence, and keep full Guard details at each review URL.
- **Statistics** — distinguish unsupported pool venues from unread checks, report catalog observations currently seen within 7 days and those first seen this UTC week, and sign complete bounded reducer inputs at `/api/stats?page=N`; keep the catalog table in its own reading-order section after summary metrics.
- **Pair display** — consistently show the issuer-matched token before its paired asset on certificates, the homepage leaderboard, and token pages.
- **Usability** — add real examples, plain-language results, standalone wallet statements, two-minute paths for agents, apps, and compliance, explicit Guard chain selection, and readable JSON errors for API requests.

### Details

QED's publisher watch reserves both ticker and ticker+x searches for the 10 highest-volume active issuers; up to 30 remaining searches rotate through the rest of the registry. Each successful six-hour refresh performs at most 50 searches and 50 sequential Guard evaluations, including rechecks. Guard evaluations prioritize candidate tickers in the same highest-volume registry order, then higher-volume pairs within each ticker. It retains at most 128 supported and 256 unsupported observations, evicts oldest-last-seen rows first, and counts evictions. Each supported observation retains only identity-determining reads at the queried token (EVM `symbol()`/`name()` calls or Solana metadata reads) and the registry-source snapshot hash; read params and raw results are each capped at 512 serialized bytes. `evidence_truncated` marks omitted reads or clipped text/read details. Overlong labels, reasons, on-chain values, and observation timestamps are clipped and set that flag rather than rejected; supported observations are rejected only for invalid chain, token address, or catalog hash. Overlong unsupported-candidate text is clipped the same way; an unsupported candidate is rejected only for an invalid chain id or token address. Full Guard evidence remains available at the review URL. Invalid supported observations and unsupported candidates are counted and dropped. The signed stats summary reports counts only, while signed reducer inputs contain the bounded retained evidence once. The HTML table shows at most 20 recent entries. Unsupported-chain candidates remain unique chain/address observations and are never judged. Official catalog deployments, including wrappers, match only on the chain where they were published. DexScreener labels and volumes are reported as received, not independently verified.

A failed individual watch search is logged and skipped while the scan continues. If every watch search fails or returns no pairs, the scan is recorded as search-source unavailable rather than as a fresh scan: retained observations and the last successful scan time stay unchanged, and the stats headline reports the impostor search source as unavailable since the first failed scan until a search succeeds. One-character tickers are searched only in their ticker+x form because DexScreener rejects one-character queries.

Uniswap v4 resolution checks at most 20 recent `Initialize` log windows within a 10-second deadline; exhausted fallback is reported as `rpc_limit`. Evidence names either `PositionManager.poolKeys` or the bounded recent log source.

The statement page now labels its signed snapshot, offers CSV and print-to-PDF downloads, links directly to signature verification, and can re-run the same wallet set for comparison. Signed-record verification pages link to the QED public verification key; certificate freshness is defined by its recorded expiry interval and distinguished from current issuer-registry or chain state, and statements are explicitly identified as point-in-time snapshots. A historical certificate remains evidence of its recorded point-in-time check after expiry; re-check it for a new observation before relying on current state.

POST `/verify` keeps non-stats documents under 2 MiB while allowing signed stats snapshots up to 16 MiB of canonical payload, plus their JSON envelope.

QED does not prove backing, custody, reserves, solvency, safety, price, liquidity, or endorsement.
## [RC-8.1] - 2026-10-06

**Pages open at the top; changelog gets shorter.**

- **Navigation** — page links open at the top; Back and Forward restore saved scroll positions.
- **Release notes** — concise bullets lead each entry, with longer context tucked into native disclosures.
- **Blog** — screenshot steps stay compact, open full-size in a new tab, and match light or dark theme.

## [RC-8] - 2026-10-06

**Signed reviews and wallet statements**

- **Guard** — one signed review covers issuer identity, observed powers, source status, optional wallet restrictions, and known pools.
- **Statement** — sign registry-token balances for a chosen wallet set at one point in time.
- **Publisher checks** — compare likely clones with fresh on-chain metadata; incomplete reads cannot deny alone.
- **Docs** — the hub links short paths for agents, apps, and auditors.

### Details

Guard reviews supported-chain tokens and recognized pools with a signed verdict and machine-readable reasons. An unindexed Solana account is reviewed as a token.

Statements record observed balances at a block height and can be downloaded and verified; balances do not prove ownership or solvency. Blog images fit their article frame in light and dark themes.

## [RC-7] - 2026-10-03

**Power reads distinguish absence from uncertainty**

- **Fallbacks** — reverted capability getters report absence rather than a transient read error.
- **Pause state** — probe both `paused()` and `isPaused()` before reporting a token's pause status.
- **Provider faults** — retry rate limits and missing headers as transient, not token facts.

## [RC-6] - 2026-10-03

**Warm reads preserve their last good observations**

- **Warm-up** — start prefetching with leaderboard loads without starving live requests.
- **Refreshes** — keep the last successful observation and timestamp after a failed re-read.

## [RC-5] - 2026-10-03

**Token pages answer within a fixed deadline**

- **Deadline** — resolve powers for every token contract within 2.5 seconds.
- **Slow reads** — continue in the background and appear on a later page visit.
- **Warm cache** — prepare frequently viewed tokens before visitors open their pages.

## [RC-4] - 2026-10-03

**Issuer powers appear on every token page**

- **Badges** — show seize, block, rule-change, and source-verification signals by default.
- **Homepage** — explain issuer powers while preserving both protected statements verbatim.

## [RC-3] - 2026-10-02

**See token powers across supported chains**

- **Solana** — report Token-2022 controls, including freeze, seize, and rule changes.
- **EVM** — inspect proxy administration, pause state, and blocklist controls.
- **Source** — compare deployed contract code with Sourcify's published verification.

## [RC-2] - 2026-10-02

**Certificates verify across number formats**

- **Signing** — canonical JSON encodes floating-point fields consistently for verification.
- **Compatibility** — certificates with awkward decimal numbers now verify without payload changes.

### Details

Canonical JSON now handles floating-point fields identically while signing and verifying certificates.

## [RC-1] - 2026-10-02

**Ask QED issuer checks over MCP**

- **MCP** — call `qed_check`, `qed_powers`, `qed_wallet`, `qed_statement`, `qed_registry_lookup`, `qed_verify`, or `qed_guard` at `POST /mcp`.
- **Registry** — add Ondo beside xStocks and Robinhood issuer sources.
- **Explorers** — link every contract to its chain explorer.
