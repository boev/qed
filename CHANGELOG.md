# Changelog

Notable QED changes, in reverse chronological order. Entries describe shipped releases and current unreleased work; QED does not prove backing, custody, reserves, solvency, safety, price, liquidity, or endorsement.

## [1.1.2] - 2026-10-18

- **Copies** — Look-alike addresses now highlight the matching start and end.
- **Homepage** — Overview counts use the prepared snapshot; integration links reach their destinations.

## [1.1.1] - 2026-10-18

**Fix: copy tiers after the 1.1.0 import**

- **Copies** — Some confirmed copies showed as Caution after 1.1.0. Tiers are now derived again by the same rule Guard uses. Rows not yet classified show as Pending.

## [1.1.0] - 2026-10-17

**Postgres storage and asset history**

- **Storage** — Records now live in Postgres; large files stay in S3.
- **Status** — A missed check no longer changes a status. After three missed scheduled checks it shows Unverified.
- **Asset pages** — Each pool and token has a page with its check history.
- **Limits** — Daily read budget, request limits, cached-only answers for crawlers.
- **Input checks** — Addresses are validated before any chain read.

## [1.0.9] - 2026-10-15

**Token powers kept across registry updates**

- **Powers cache** — Token powers are chain facts per contract, so issuer-catalog updates no longer clear them; only their own 6-hour (top tickers) or daily refresh reads the chain again.

## [1.0.8] - 2026-10-15

**Fewer chain reads, same answers**

- **Registry refresh** — The hourly issuer-catalog refresh counts as a change only when contracts or deployments change. Before, every refresh cleared the cached token powers and Guard answers, so all 156 tracked contracts were re-read from the chain every hour.
- **Powers cadence** — Token powers for the top 25 tickers refresh every 6 hours (was 100 minutes) and the rest daily (was about 6 h 40 min); every answer still shows when it was observed.
- **Guard answers** — Allow and deny answers are reused for up to 6 hours (was 1 hour); unknown answers are always checked again.

## [1.0.7] - 2026-10-15

**Steady leaderboard columns**

- **Venue names** — Long venue names (raw contract addresses) are shortened, with the full address on hover, so they no longer shift the leaderboard's verdict column.

## [1.0.6] - 2026-10-13

**Clearer verdicts**

- **Verdict card** — Guard results open with a large Allow/Deny/Unknown badge and key facts (issuer identity, publisher, ticker, source); the explanation folds away.
- **Address comparison** — Guard and `/copies` show the issuer-published address above the reviewed one in the same size, with every differing character marked.
- **No copying flagged addresses** — The copy button is disabled for flagged addresses on `/copies`.

## [1.0.5] - 2026-10-13

**Steadier answers**

- **Final answers only** — Guard reuses only allow and deny answers; unknown answers are always checked again.
- **Memory** — The Guard answer cache holds at most 256 answers, and the service runs with 2 GB instead of 1 GB of memory.

## [1.0.4] - 2026-10-12

**Copied stock tokens, ranked by chain**

- **Copies page** — `/copies` ranks confirmed publisher-watch copies by reported 24h volume, with chain filters, the largest copy beside its issuer-published address, and a Guard link per row.

## [1.0.3] - 2026-10-12

**Faster repeat Guard checks**

- **Repeat checks** — Reuse successful signed Guard answers without a wallet for up to one hour; transient pool and publisher-metadata failures are retried.
- **Longer limit** — Allow Guard reads up to 30 seconds for slower chains.
- **Faster start** — Load persisted evidence in parallel; run expired-evidence cleanup after the server starts.

## [1.0.2] - 2026-10-09

**Lighter background reads with honest check times**

- **Background RPC budget** — Keep six-hour checks for the top 50 leaderboard pools by reported 24-hour volume and featured pools; refresh cold pool rows after 24 hours, preserve their actual check timestamps, and give new rows an initial read. Limit successful publisher-watch scans to once per 24 hours. Refresh top-25 powers ticker groups every 100 minutes and cold groups every four warm intervals (400 minutes, about 6 hours 40 minutes; immediately if uncached). Retain complete powers and published-contract metadata for six hours. Cold leaderboard/watch observations can be up to 24 hours old; powers observations can be reused for up to six hours.
- **Evidence freshness** — Separate price-update time from leaderboard contract checks; each verdict exposes its UTC check time and marks observations older than 24 hours stale. Retry all leaderboard rows not marked `checked` on the next discovery pass, preserve publisher-watch results and scan time when every Guard read fails, and keep incomplete power facts in the 30-second retry cache instead of the six-hour cache.

## [1.0.1] - 2026-10-08

**A calmer QED with clearer evidence**

- **Navigation** — group reference links under a no-JavaScript Docs menu with the latest released headline, and mark the current page with an outline that glides between menu items.
- **Pages** — streamline the homepage and blog, add a linked integration marquee and ambient hex background, icon copy controls, and compact vertical screenshot steps.
- **Evidence** — foreground three useful statistics, separate long table details, show human UTC timestamps, and load Ondo's public address catalog daily from its documentation and CSV; keep omitted deployments active and the catalog incomplete until a second consecutive clean snapshot confirms their removal.
- **Flagged tokens** — re-derive retained observation tiers with Guard's classifier and published on-chain product metadata from the current accepted registry at startup, registry refresh, and signed stats generation; distinguish confirmed mismatches from caution near matches, with compact reported-volume summaries and expandable details.
- **Page polish** — align page titles, add Stats to navigation and the version pill, and clarify leaderboard freshness and phone layout.
- **Wallet page** — remove the My Wallet page for now; it returns as Connect Wallet.

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
