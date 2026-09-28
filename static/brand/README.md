# QED brand assets

QED uses the following official, unmodified brand references for chain and DEX labels. The page keeps logos in their source colours only where the source publishes them; interface icons remain `currentColor` and the QED palette stays limited to Solana green (`#14F195`), Solana purple (`#9945FF`), coral (`#FF5C5C`) and neutrals.

| File / label | Official source | Terms / handling |
| --- | --- | --- |
| `solana-mark.svg` | https://solana.com/branding and https://solana.com/src/img/branding/solanaLogoMark.svg | Solana Foundation brand guidance. The source mark is reproduced unmodified. Follow the linked clear-space and no-distortion rules. |
| Robinhood Chain | https://docs.robinhood.com/chain/connecting/ | The docs publish a feather mark but no redistributable asset licence. QED uses a one-colour chain glyph in the UI, not a copied logo. |
| Base | https://base.org/brand | Coinbase/Base brand assets are subject to their brand guidelines. QED uses a one-colour glyph until an approved downloadable asset is available. |
| Ethereum | https://ethereum.org/assets/ | Ethereum.org lists the diamond SVG for download. QED uses a one-colour diamond glyph in the UI to keep the palette and avoid altering the source artwork. |
| BNB Chain | https://www.bnbchain.org/en/brand-guidelines | BNB Chain's yellow logo is published under its brand guidelines. QED uses a one-colour glyph in the UI rather than recolouring or modifying it. |
| PumpSwap / Raydium / Uniswap / PancakeSwap | https://pump.fun/ , https://raydium.io/ , https://uniswap.org/brand , https://www.pancakeswap.finance/brand | Brand ownership remains with each project. QED uses text labels and `currentColor` DEX glyphs; no project logo is altered or redistributed. |

`logo.svg` in the parent directory is QED's own wordmark and is not an issuer or chain logo.

## Vendored runtime

`../vue.global.prod.js` is Vue 3.5.21's global production build, downloaded
from https://unpkg.com/vue@3.5.21/dist/vue.global.prod.js. Vue is distributed
under the MIT License. Full text: `licenses/VUE-MIT.txt`.

`../htmx.min.js` is htmx 2.0.6's production build, downloaded from
https://unpkg.com/htmx.org@2.0.6/dist/htmx.min.js. htmx is distributed under
the Zero-Clause BSD (0BSD) License. Full text: `licenses/HTMX-0BSD.txt`.

## Vendored asset integrity

The vendored browser assets are reviewed before release. Their SHA-256 hashes
are recorded here so an update is explicit and reproducible:

| File | Source | SHA-256 |
| --- | --- | --- |
| `../htmx.min.js` | https://unpkg.com/htmx.org@2.0.6/dist/htmx.min.js | `e209dda5c8235479f3166defc7750e1dbcd5a5c1808b7792fc2e6733768fb447` |
| `../vue.global.prod.js` | https://unpkg.com/vue@3.5.21/dist/vue.global.prod.js | `4a715c2f7bdc8a3309c2f82bc3291f33f8f4d8e7ab02a7d4b6588b5f7650f772` |
