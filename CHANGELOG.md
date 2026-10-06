# Changelog

Notable QED changes, in reverse chronological order. Entries describe shipped behavior; QED does not prove backing, custody, reserves, solvency, safety, price, liquidity, or endorsement.

## [Release 8] - 2026-10-06

**QED Guard and Statement make issuer checks usable as signed evidence.**

- Guard gives a signed, read-only review of issuer identity, observed token controls, source status, optional wallet restrictions, and known pool facts. It denies only when evidence contradicts the issuer's published contract.
- Guard compares likely publisher candidates with fresh on-chain metadata inside each request deadline (up to eight candidates); incomplete reads cannot deny by themselves and leave identity unknown unless another candidate contradicts.
- A Statement records registry-token balances for chosen wallets at one point in time. Download its JSON to share the evidence and check the signature separately.
- The Docs hub explains each tool in plain words and offers three short paths: an AI agent, an app, or an auditor. API, MCP, and LLM links are available from the footer and docs sidebar.
- The home page and Docs hub show the newest released headline and, when present, the newest published blog post.
- Blog images fit inside the article frame in light and dark themes.

## [Release 7] - 2026-10-03

**Powers stop guessing: a reverted getter is now 'absent', and pause state is probed both ways.**

- A contract that reverts on a capability query is reported as "no such getter", not as a transient error.
- Pause state is read via both `paused()` and `isPaused()`; whichever answers wins.
- Rate limits and missing headers from RPC providers are retried as transient instead of being recorded as facts.

## [Release 6] - 2026-10-03

**Hot tokens stay warm, and flaky re-reads keep the last good facts.**

- The warm pass starts as soon as the leaderboard loads and is capped so it never starves live requests.
- When a re-read fails, the last good observation stays on the page with its original timestamp.

## [Release 5] - 2026-10-03

**Token pages answer within a fixed deadline.**

- Powers for every contract on a token page are filled within a 2.5 s budget; slower reads continue in the background and appear on the next load.
- Frequently viewed tokens are kept warm so the badges are usually ready before you ask.

## [Release 4] - 2026-10-03

**Issuer powers at a glance on every token page.**

- Four badges per contract: can seize, can block, can change rules, source verified — open by default, details below.
- The homepage now says what QED shows about each token; the two protected statements stay exactly as they were.

## [Release 3] - 2026-10-02

**QED Powers: see what the issuer can do to a token.**

- `/api/powers/{address}` and the `qed_powers` MCP tool read freeze, seize and rule-change authority straight from the chain, on Solana (Token-2022 extensions) and EVM (proxy admin, pauser, blocklist).
- Source verification status comes from Sourcify, so "verified" means the published code matches the deployed bytecode.

## [Release 2] - 2026-10-02

**Certificates verify reliably for every number format.**

- Canonical JSON now handles floating-point fields the same way on signing and verification, so a certificate with awkward numbers no longer fails to verify.

## [Release 1] - 2026-10-02

**QED speaks MCP: any AI agent can ask whether a pool is the real token.**

- `POST /mcp` with the tools `qed_check`, `qed_wallet`, `qed_registry_lookup` and `qed_verify`; server card at `/.well-known/mcp/server-card.json`.
- Ondo added as an issuer registry source next to xStocks and Robinhood.
- Every contract links to its chain explorer.
