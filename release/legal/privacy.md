# Privacy notice

Controller: Web3 Energy Ltd., 49 Angel Voyvoda Street, Poduyane District, 1510 Sofia, Bulgaria. Email: contact@web3-energy.com.

## No accounts, no cookies

QED has no sign-in and no user accounts. It sets no cookies, including analytics or advertising cookies. Your theme choice (light or dark) is stored only in your own browser's local storage and is never sent to us.

## Server logs and service metrics

QED does not routinely retain access-log request content, including query strings, headers, request bodies, or client IP addresses. It temporarily processes the client IP address only in memory for abuse prevention; the maximum retention is 70 seconds, and it is never sent to analytics. QED has no unique-visitor or per-user analytics. Exceptional application errors may include public on-chain pool or record identifiers. AWS and the Application Load Balancer may process technical connection metadata needed to route and protect requests under their own policies. Operational logs are retained for 14 days and must not contain secrets, request bodies, or client IP addresses.

## Hosting

QED application hosting is in Amazon Web Services, region eu-central-1 (Frankfurt, Germany), as one ECS service using AWS Fargate behind an AWS Application Load Balancer. The public endpoint is served directly by that load balancer. The QED task listens on port 8080, and network rules permit that service port only from the load balancer. Hosting in Frankfurt does not mean that all processing stays in the EU: AWS support or security operations, and configured RPC or data providers, may process data outside the EEA under their own policies and applicable safeguards, including EU Standard Contractual Clauses where required.

## Third-party lookups

To answer a check, our server sends the address you pasted to the operator-configured blockchain RPC endpoint for the relevant chain (Solana, Robinhood Chain, Base, Ethereum, or BNB Chain) and to the DexScreener public API when it needs pool data. In production, QED rejects its built-in public RPC defaults and uses only configured RPC endpoints. These requests carry only that on-chain address as lookup data, not your IP address, browser data, or any personal identifier from QED; the providers may still process the server's network metadata under their own policies. Wallet checks are submitted in a POST request body; results are not cached by address.

## No analytics, no advertising

QED runs no Google Analytics, advertising networks, or comparable trackers.

## Retention

Operational logs are retained for 14 days. No routine access-log request content is retained. Exceptional application errors may include public on-chain pool or record identifiers, but logs must not contain secrets, request bodies, or client IP addresses. Client IP addresses used only for in-memory abuse prevention are discarded within a maximum of 70 seconds and are never sent to analytics. Published attestations describe public on-chain facts used to evidence a contract check. Public blockchain addresses and transaction facts may constitute personal data if they can be linked to an individual. QED stores attestations in encrypted, versioned storage until about 30 days after each record's stated expiry, when expiration pruning deletes them.

## Your rights

You may exercise access, rectification, erasure, restriction, portability, and objection rights under Articles 15 to 21 GDPR. You may complain to Bulgaria's Commission for Personal Data Protection (KZLD, Sofia) or to the supervisory authority of your home country. Contact: contact@web3-energy.com with subject "Privacy rights".
