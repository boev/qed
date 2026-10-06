# Changelog

Notable QED changes, in reverse chronological order. Entries describe shipped behavior; QED does not prove backing, custody, reserves, solvency, safety, price, liquidity, or endorsement.

## [Release 8.1] - 2026-10-06

**Pages open at the top; changelog gets shorter.**

- **Navigation** — page links open at the top; Back and Forward restore saved scroll positions.
- **Release notes** — concise bullets lead each entry, with longer context tucked into native disclosures.
- **Blog** — screenshot steps stay compact, open full-size in a new tab, and match light or dark theme.

## [Release 8] - 2026-10-06

**Signed reviews and wallet statements**

- **Guard** — one signed review covers issuer identity, observed powers, source status, optional wallet restrictions, and known pools.
- **Statement** — sign registry-token balances for a chosen wallet set at one point in time.
- **Publisher checks** — compare likely clones with fresh on-chain metadata; incomplete reads cannot deny alone.
- **Docs** — the hub links short paths for agents, apps, and auditors.

### Details

Guard reviews supported-chain tokens and recognized pools with a signed verdict and machine-readable reasons. An unindexed Solana account is reviewed as a token.

Statements record observed balances at a block height and can be downloaded and verified; balances do not prove ownership or solvency. Blog images fit their article frame in light and dark themes.

## [Release 7] - 2026-10-03

**Power reads distinguish absence from uncertainty**

- **Fallbacks** — reverted capability getters report absence rather than a transient read error.
- **Pause state** — probe both `paused()` and `isPaused()` before reporting a token's pause status.
- **Provider faults** — retry rate limits and missing headers as transient, not token facts.

## [Release 6] - 2026-10-03

**Warm reads preserve their last good observations**

- **Warm-up** — start prefetching with leaderboard loads without starving live requests.
- **Refreshes** — keep the last successful observation and timestamp after a failed re-read.

## [Release 5] - 2026-10-03

**Token pages answer within a fixed deadline**

- **Deadline** — resolve powers for every token contract within 2.5 seconds.
- **Slow reads** — continue in the background and appear on a later page visit.
- **Warm cache** — prepare frequently viewed tokens before visitors open their pages.

## [Release 4] - 2026-10-03

**Issuer powers appear on every token page**

- **Badges** — show seize, block, rule-change, and source-verification signals by default.
- **Homepage** — explain issuer powers while preserving both protected statements verbatim.

## [Release 3] - 2026-10-02

**See token powers across supported chains**

- **Solana** — report Token-2022 controls, including freeze, seize, and rule changes.
- **EVM** — inspect proxy administration, pause state, and blocklist controls.
- **Source** — compare deployed contract code with Sourcify's published verification.

## [Release 2] - 2026-10-02

**Certificates verify across number formats**

- **Signing** — canonical JSON encodes floating-point fields consistently for verification.
- **Compatibility** — certificates with awkward decimal numbers now verify without payload changes.

### Details

Canonical JSON now handles floating-point fields identically while signing and verifying certificates.

## [Release 1] - 2026-10-02

**Ask QED issuer checks over MCP**

- **MCP** — call `qed_check`, `qed_wallet`, `qed_registry_lookup`, or `qed_verify` at `POST /mcp`.
- **Registry** — add Ondo beside xStocks and Robinhood issuer sources.
- **Explorers** — link every contract to its chain explorer.
