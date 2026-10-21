# Privacy notice

Controller: Web3 Energy Ltd., 49 Angel Voyvoda Street, Poduyane District, 1510 Sofia, Bulgaria. Email: contact@web3-energy.com.

## No accounts, no cookies

QED has no sign-in and no user accounts. It sets no cookies, including analytics or advertising cookies. Your theme choice (light or dark) is stored only in your own browser's local storage and is never sent to us.

## Server logs and service metrics

QED application logs do not routinely retain request URLs, headers, request bodies, or client IP addresses. The shared AWS Application Load Balancer does not keep access logs. It kept them from 18 to 21 October 2026 only; they recorded the full request URL (including query strings), client IP address, User-Agent, request time, and response status. The S3 copies expire under a 14-day lifecycle (deletion is asynchronous). The operator keeps a restricted offline copy of those three days for traffic and abuse analysis and deletes it when that analysis is no longer needed. This is distinct from QED's in-memory abuse-prevention limiter, which processes a client IP for at most 70 seconds and does not write that address to QED application logs. Do not put secrets in URLs.

## Hosting

QED application hosting is in Amazon Web Services, region eu-central-1 (Frankfurt, Germany), behind an AWS Application Load Balancer. Hosting in Frankfurt does not mean that all processing stays in the EU: AWS support or security operations, and configured RPC or data providers, may process data outside the EEA under their own policies and applicable safeguards, including EU Standard Contractual Clauses where required.

## Third-party lookups

To answer a check, our server sends the address you pasted to the operator-configured blockchain RPC endpoint for the relevant chain (Solana, Robinhood Chain, Base, Ethereum, or BNB Chain) and to the DexScreener public API when it needs pool data. In production, QED rejects its built-in public RPC defaults and uses only configured RPC endpoints. These requests carry only that on-chain address as lookup data, not your IP address, browser data, or any personal identifier from QED; the providers may still process the server's network metadata under their own policies. Wallet checks through the `/guard` form, `POST /api/guard`, `/api/wallet`, or MCP use POST request bodies. The legacy `GET /api/guard/{address}?chain=...&wallet=...` places the wallet in the request URL, where it may be exposed in browser history or other URL handling; prefer `POST /api/guard`. Results are not cached by address.
From 18 to 21 October 2026, the legacy `GET /api/guard/{address}?chain=...&wallet=...` request's full URL, including its `wallet` query value, was captured in ALB access logs. Prefer the POST endpoint for wallet checks. Avoid putting confidential values in any URL.

## No analytics, no advertising

QED runs no Google Analytics, advertising networks, or comparable trackers.

## Retention

QED application logs do not routinely retain request URLs, headers, request bodies, or client IP addresses; exceptional application errors may include public on-chain pool or record identifiers, but logs must not contain secrets or request bodies. ALB access logs from 18 to 21 October 2026 are handled as described under "Server logs and service metrics". The separate in-memory abuse-prevention limiter discards client IP addresses within at most 70 seconds. Published attestations describe public on-chain facts used to evidence a contract check. Public blockchain addresses and transaction facts may constitute personal data if they can be linked to an individual. QED stores attestations in encrypted, versioned storage until about 30 days after each record's stated expiry, when expiration pruning deletes them.

QED's public-check history records checks for each token and pool (time, outcome, public contract or pool address, and the public on-chain reads behind it) so each asset page can show how its status developed. This history contains no wallet addresses from Guard wallet checks or wallet statements and no client IP addresses, and it is kept without a fixed time limit. This history is separate from the October 2026 ALB access logs described above. Requests to remove data that relates to you can be sent to contact@web3-energy.com.

Wallet statement pages are public to anyone with the link; QED keeps them in an in-process memory cache for up to 24 hours. Statement IDs are content hashes, not access controls.


## Your rights

You may exercise access, rectification, erasure, restriction, portability, and objection rights under Articles 15 to 21 GDPR. You may complain to Bulgaria's Commission for Personal Data Protection (KZLD, Sofia) or to the supervisory authority of your home country. Contact: contact@web3-energy.com with subject "Privacy rights".
