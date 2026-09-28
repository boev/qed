# Terms of use and disclaimer

Operator and copyright owner: Web3 Energy Ltd., 49 Angel Voyvoda Street, Poduyane District, 1510 Sofia, Bulgaria. Contact: contact@web3-energy.com with subject "Legal".

QED is operated as one Rust process in AWS ECS using Fargate behind an AWS Application Load Balancer in eu-central-1 (Frankfurt). The public service is reached directly through that load balancer.

## 1. What a QED record is

A signed, timestamped record states whether a pool uses the stock-token contract published by its issuer, based on the pool data observed at the time of the check and QED's public issuer registry. "Re-check" runs the same reads live. Anyone can verify the signature with the key published at /.well-known/qed.json. A contract match does not prove backing, custody, reserves, solvency, safety, price, liquidity, or endorsement.

## 2. Not investment advice

A verdict and its evidence describe contract addresses only. They are not investment, financial, legal, or tax advice, not a recommendation to buy, sell, or hold any token, and not a statement about price or value.

## 3. No endorsement

"Verified" means that the pool's quote-side contract equals an address in QED's registry for a named issuer at the time of the check. It is a contract match only, not an endorsement of the issuer, the pool, the trading venue, or any token's economics, backing, custody, safety, or legality.

## 4. What "no match" and "mismatch" mean

They state only that an address was not found in, or differs from, QED's registry at the time of the check. They are not an accusation of fraud or illegality and say nothing about the people behind a project.

## 5. Trade links and re-checks

Pool pages link to the trading venue and the block explorer for the exact pool that was checked. QED builds these links from the pool's on-chain addresses and does not send you to a venue's front page. The venues are third parties; QED does not operate them, does not hold your funds, and is not a party to any trade. You may run a new check at any time with "Re-check"; the result you act on should be the one you have just seen, for the address you have just compared.

## 6. Your responsibility

You alone decide whether to buy, sell, hold, or trade any token, and you alone bear the outcome, including any loss. QED gives you facts about contracts; it does not know your situation, does not manage risk for you, and cannot prevent a venue, a token, or a market from behaving against you. Before you trade, compare the address shown on QED with the address you were given, and re-check if any time has passed. By using QED you accept that all gains and losses from your actions are yours only.

## 7. No warranty

The service is provided as is. Registry entries are curated from public issuer sources and may lag. Reads depend on third-party RPC infrastructure that QED does not control. A QED record describes a moment in time; pool contents can change afterwards.

## 8. Liability

To the maximum extent permitted by law, Web3 Energy Ltd.'s aggregate liability arising from the free QED service is limited to EUR 100, except where liability cannot legally be limited. Nothing in these terms excludes liability that cannot be excluded under applicable consumer law.

## 9. Linking a QED record

You may link to a QED record and its pool page on your own pages, for example with the text "Checked on QED". QED provides no images or badges for embedding. A link is not an endorsement; your visitors must compare the address shown on QED with the address they were given before they trade. You may not alter the record, present a record for a different pool, or imply that QED endorses your token. A certificate remains available through its stated expiry and may remain available for about 30 days afterwards while expiration pruning runs.

## 10. Governing law

Bulgarian law. For business users, the courts of Sofia, Bulgaria.

## 11. Correction and takedown

If an issuer, project, or user believes a registry entry, an evidence line, or a verdict is wrong, outdated, or mislabels a real project, write to contact@web3-energy.com with subject "Takedown request", including the contract address and your evidence. Reports are reviewed, and records may be corrected or receive a dated explanation.
