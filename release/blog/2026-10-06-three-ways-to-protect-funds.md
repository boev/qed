---
title: "Is it the real NVDA? 3 checks in 30 seconds"
date: 2026-10-06
summary: Three questions to answer before you touch a token — is it the real one, who can freeze it, and can you prove what you hold.
draft: false
---

A ticker is not a contract. Check the facts first, then decide for yourself.

**1. Is it the real one?**
Guard compares the token with the contract its issuer published.

![QED Guard showing the NVDA Ethereum contract and issuer match](/static/blog/2026-10-06/1-guard.png)
[Try Guard](/guard/ethereum/0xc845b2894dBddd03858fd2D643B4eF725fE0849d)

**2. Who can freeze it?**
See in plain words whether the issuer can pause, freeze, blocklist or upgrade it.

![QED Guard Powers section listing token control capabilities](/static/blog/2026-10-06/2-powers.png)
[Try Powers](/guard/ethereum/0xc845b2894dBddd03858fd2D643B4eF725fE0849d)

**3. Can you prove what you hold?**
Create a signed Statement of your wallets at a block height. Anyone can check it at `/verify`.

![Signed QED Statement with a link to verify the record](/static/blog/2026-10-06/3-statement.png)
[Try Statement](/statements)

Read-only by design: QED never holds keys, submits transactions, or makes recommendations.
Use QED from an AI agent via [MCP](/docs/llm).
A Statement records balances at a block height; it does not establish ownership, reserves, or solvency.
