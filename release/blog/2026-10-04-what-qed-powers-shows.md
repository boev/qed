---
title: What QED Powers shows
date: 2026-10-04
summary: A factual guide to the control signals and source status returned for registered token contracts.
draft: true
---

`GET /api/powers/{address}` reports control signals observed for active issuer-registry token contracts. The response groups findings into `can_seize`, `can_block`, and `can_change_rules`, and lists unavailable facts separately. It also records the observation time and available block or slot.

The `source_verified` field reports the applicable source check: the Solana Token-2022 token-program build or the EVM contract or resolved proxy implementation. A source match describes source correspondence; it does not prove how a token will be operated.

A missing or reverted read is not proof that a capability does not exist. QED Powers does not prove backing, custody, reserves, solvency, safety, price, liquidity, or endorsement.
