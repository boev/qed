---
title: What a QED Statement is
date: 2026-10-04
summary: What the signed wallet statement records, and what its point-in-time balances do not establish.
draft: true
---

`POST /api/statement` signs a point-in-time record for selected wallets and chains. It includes balances of active issuer-registry token contracts and observed chain positions. An optional block selects an exact EVM block; for Solana it is a minimum context slot, with the returned slot range recorded.

The statement is a signed record of what QED read. Balances are on-chain facts at a height, not proof of ownership, custody, reserves, or solvency. A statement page is public to anyone with its link, retained in process memory for up to 24 hours; its content-hash ID is not an access control.
