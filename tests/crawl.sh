#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="${QED_BIN:-$ROOT/target/debug/qed}"
HOST="127.0.0.1"
PORT="18086"
RPC_PORT="18546"
BASE="http://${HOST}:${PORT}"
DATA_DIR="$(mktemp -d /tmp/qed-crawl-data.XXXXXX)"
OUT_DIR="${QED_CRAWL_OUT:-/tmp/qed-crawl}"
SERVER_LOG="$OUT_DIR/server.log"
mkdir -p "$OUT_DIR"
rm -f "$OUT_DIR"/*.png "$SERVER_LOG"

if [[ ! -x "$BIN" ]]; then
  printf 'missing executable: %s\n' "$BIN" >&2
  exit 1
fi

# Keep this crawl isolated from the captain's live data directory while still
# exercising a real validated pool detail page.
mkdir -p "$DATA_DIR/attestations"
fixture_path="$DATA_DIR/attestations/generated.json"
node "$ROOT/../qed-infra/tests/e2e/generate-attestation.js" \
  "$ROOT/tests/fixtures/attest/fc6147d4cd42374b72246cc6340d23e26f5c411e63f613322a18413c3da243bb.json" \
  "$fixture_path" "$DATA_DIR/fixture-meta.json" >/dev/null
fixture_id="$(jq -r '.id' "$DATA_DIR/fixture-meta.json")"
fixture_signer="$(jq -r '.signer' "$DATA_DIR/fixture-meta.json")"
fixture_seed="$(jq -r '.seed' "$DATA_DIR/fixture-meta.json")"
mv "$fixture_path" "$DATA_DIR/attestations/$fixture_id.json"

for cache in leaderboard.json featured.json; do
  if [[ -f "$ROOT/data/$cache" ]]; then
    cp "$ROOT/data/$cache" "$DATA_DIR/$cache"
  fi
done

if ! jq -e '.entries | length > 0' "$DATA_DIR/leaderboard.json" >/dev/null 2>&1; then
  crawl_now="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  crawl_next="$(date -u -d '+5 minutes' +%Y-%m-%dT%H:%M:%SZ)"
  cat > "$DATA_DIR/leaderboard.json" <<JSON
{
  "updated_at": "$crawl_now",
  "next_refresh_at": "$crawl_next",
  "source": "crawl fixture",
  "registry": {
    "entries": 1,
    "issuers": 1,
    "updated_at": "$crawl_now",
    "next_refresh_at": "$crawl_next",
    "restored": true,
    "refreshing": false
  },
  "total": 1,
  "entries": [{
    "rank": 1,
    "chain": "solana",
    "chain_label": "Solana",
    "dex": "raydium",
    "pool": "CrawlPool11111111111111111111111111111111111",
    "base_symbol": "QED",
    "quote_symbol": "USDC",
    "issuer": "Crawl issuer",
    "ticker": "QED",
    "verdict": "verified",
    "price_usd": 1.0,
    "change_24h_pct": 1.2,
    "volume_24h_usd": 12000.0,
    "liquidity_usd": 50000.0,
    "txns_24h": 42,
    "detail_url": "/validated/solana/CrawlPool11111111111111111111111111111111111",
    "trade_url": "https://example.com/crawl-pool",
    "explorer_url": "https://example.com/crawl-explorer",
    "attestation_id": null,
    "checked_at": "$crawl_now"
  }],
  "restored": true,
  "refreshing": false
}
JSON
fi

server_pid=""
rpc_pid=""
cleanup() {
  if [[ -n "$server_pid" ]]; then
    kill "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
  fi
  if [[ -n "$rpc_pid" ]]; then
    kill "$rpc_pid" 2>/dev/null || true
    wait "$rpc_pid" 2>/dev/null || true
  fi
  rm -rf "$DATA_DIR"
}
trap cleanup EXIT INT TERM

QED_E2E_RPC_PORT="$RPC_PORT" \
  node "$ROOT/../qed-infra/tests/e2e/mock-rpc.js" >"$OUT_DIR/rpc.log" 2>&1 &
rpc_pid=$!
for _ in $(seq 1 40); do
  if curl -fsS "http://${HOST}:${RPC_PORT}/registry/xstocks" >/dev/null 2>&1; then
    break
  fi
  sleep 0.25
done
curl -fsS "http://${HOST}:${RPC_PORT}/registry/xstocks" >/dev/null

QED_BIND="${HOST}:${PORT}" \
QED_DATA_DIR="$DATA_DIR" \
QED_PUBLIC_URL="$BASE" \
QED_SIGNING_KEY="$fixture_seed" \
QED_PREVIOUS_KEYS="$fixture_signer" \
QED_REGISTRY_XSTOCKS_URL="http://${HOST}:${RPC_PORT}/registry/xstocks" \
QED_REGISTRY_ONDO_URL="http://${HOST}:${RPC_PORT}/registry/ondo" \
QED_REGISTRY_ROBINHOOD_URL="http://${HOST}:${RPC_PORT}/registry/robinhood" \
  "$BIN" >"$SERVER_LOG" 2>&1 &
server_pid=$!

for _ in $(seq 1 60); do
  if curl -fsS "$BASE/healthz" >/dev/null 2>&1; then
    break
  fi
  sleep 0.25
done
curl -fsS "$BASE/healthz" >/dev/null

request() {
  local path="$1" label="$2" headers="${3:-}"
  local response code body
  if [[ -n "$headers" ]]; then
    response="$(curl -sS -H "$headers" -w '\n%{http_code}' "$BASE$path")"
  else
    response="$(curl -sS -w '\n%{http_code}' "$BASE$path")"
  fi
  code="${response##*$'\n'}"
  body="${response%$'\n'*}"
  [[ "$code" == "200" ]] || { printf 'FAIL %s: HTTP %s\n' "$path" "$code" >&2; return 1; }
  [[ "$body" == *'<header class="site-header page-shell">'* ]] || {
    printf 'FAIL %s: missing site header\n' "$path" >&2
    return 1
  }
  printf '%s %s\n' "$label" "$path"
}

# Every full HTML page route must serve the shared header markup.
while IFS=$'\t' read -r path label; do
  request "$path" "$label"
done <<ROUTES
/	home
/wallet	wallet
/check	check
/registry	registry
/validated	validated
/tokens/NVDA	token-directory
/guide/verify-a-stock-token	verification-guide
/chains/solana	chain-directory-solana
/chains/robinhood	chain-directory-robinhood
/chains/base	chain-directory-base
/chains/ethereum	chain-directory-ethereum
/chains/bnb	chain-directory-bnb
/glossary	glossary
/validated/solana/5DnhFALRYpbTjLViVSEaqGsUoHw5DY8XLyyF3qaMkgqT	pool-detail
/v/$fixture_id	certificate
/imprint	imprint
/privacy	privacy
/terms	terms
/api	api-guide
ROUTES

api_html="$(curl -fsS "$BASE/api")"
[[ "$api_html" == *'href="/guide/verify-a-stock-token"'* ]] || {
  printf 'FAIL /api: verification guide link missing\n' >&2
  exit 1
}
printf 'api guide link check passed\n'

wallet_html="$(curl -fsS "$BASE/wallet")"
[[ "$wallet_html" == *'Paste your address, any of the five chains'* ]] || {
  printf 'FAIL /wallet: address prompt missing\n' >&2
  exit 1
}
[[ "$wallet_html" == *'Connecting only reads your address. QED never asks for a signature or a transaction.'* ]] || {
  printf 'FAIL /wallet: safety copy missing\n' >&2
  exit 1
}
[[ "$wallet_html" == *'No wallet detected. Paste your address.'* ]] || {
  printf 'FAIL /wallet: no-provider fallback copy missing\n' >&2
  exit 1
}
printf 'wallet landing copy and read-only safety check passed\n'
wallet_post_code="$(curl -sS -o /dev/null -w '%{http_code}' -X POST --data-urlencode 'address=' "$BASE/wallet")"
[[ "$wallet_post_code" == "404" ]] || {
  printf 'FAIL POST /wallet: expected body-form validation HTTP 404, got %s\n' "$wallet_post_code" >&2
  exit 1
}
api_wallet_code="$(curl -sS -o /dev/null -w '%{http_code}' -X POST -H 'content-type: application/json' --data '{"address":""}' "$BASE/api/wallet")"
[[ "$api_wallet_code" == "404" ]] || {
  printf 'FAIL POST /api/wallet: expected JSON-body validation HTTP 404, got %s\n' "$api_wallet_code" >&2
  exit 1
}
old_wallet_code="$(curl -sS -o /dev/null -w '%{http_code}' "$BASE/wallet/0x0000000000000000000000000000000000000000")"
[[ "$old_wallet_code" == "404" ]] || {
  printf 'FAIL /wallet/<address>: address-in-URL route unexpectedly exists (%s)\n' "$old_wallet_code" >&2
  exit 1
}
printf 'wallet POST-body routes and address privacy check passed\n'

# HTMX requests still return complete documents so direct navigation and
# browser refreshes cannot produce a fragment-only page.
for path in / /validated; do
  response="$(curl -sS -H 'HX-Request: true' -w '\n%{http_code}' "$BASE$path")"
  code="${response##*$'\n'}"
  body="${response%$'\n'*}"
  [[ "$code" == "200" && "$body" == *'<html'* && "$body" == *'<header class="site-header page-shell">'* ]] || {
    printf 'FAIL HX %s: expected complete HTML document\n' "$path" >&2
    exit 1
  }
  printf 'hx-document %s\n' "$path"
done

# Render the expanded proof phrase and audit every proof span rule. The
# inline-block baseline must remain visible in Chromium as well as Firefox.
open_html="$(curl -fsS "$BASE/?hero=open")"
[[ "$open_html" == *'aria-label="Quod erat demonstrandum"'* ]] || {
  printf 'FAIL /?hero=open: proof phrase missing\n' >&2
  exit 1
}
hero_rules="$(awk '
  /^\.proof-phrase/ { capture = 1 }
  capture { print }
  capture && /^}/ { capture = 0 }
' "$ROOT/static/home.css")"
[[ "$hero_rules" == *'vertical-align: baseline'* ]] || {
  printf 'FAIL hero CSS: spans are not baseline-aligned\n' >&2
  exit 1
}
if [[ "$hero_rules" == *'overflow: hidden'* || "$hero_rules" == *'overflow: clip'* ]]; then
  printf 'FAIL hero CSS: proof spans must keep visible overflow\n' >&2
  exit 1
fi
printf 'hero-open DOM/CSS check passed\n'

# Fragment, metadata, discoverability, and API routes are status-checked separately.
for path in /registry/table /pools/featured /robots.txt /sitemap.xml /validated.xml /llms.txt /llms-full.txt /api /openapi.json /api/registry /api/pools/featured /api/leaderboard /api/prices /api/status; do
  code="$(curl -sS -o /dev/null -w '%{http_code}' "$BASE$path")"
  [[ "$code" == "200" ]] || { printf 'FAIL %s: HTTP %s\n' "$path" "$code" >&2; exit 1; }
  printf 'status %s %s\n' "$code" "$path"
done
verify_code="$(curl -sS -o /dev/null -w '%{http_code}' -X POST -H 'content-type: application/json' --data '{}' "$BASE/verify")"
[[ "$verify_code" != "404" ]] || { printf 'FAIL /verify: endpoint missing\n' >&2; exit 1; }
printf 'status %s POST /verify\n' "$verify_code"
for removed_path in /seal /badge; do
  code="$(curl -sS -o /dev/null -w '%{http_code}' "$BASE$removed_path")"
  [[ "$code" == "404" ]] || { printf 'FAIL %s: HTTP %s\n' "$removed_path" "$code" >&2; exit 1; }
  printf 'status %s %s\n' "$code" "$removed_path"
done

profile="$OUT_DIR/firefox-profile"
rm -rf "$profile"
mkdir -p "$profile"
reduced_profile="$OUT_DIR/firefox-reduced-profile"
rm -rf "$reduced_profile"
mkdir -p "$reduced_profile"
printf 'user_pref("ui.prefersReducedMotion", 1);\n' > "$reduced_profile/user.js"
for size in 1440x900 390x844; do
  firefox --headless --no-remote --profile "$reduced_profile" \
    --screenshot="$OUT_DIR/home-reduced-${size}.png" \
    --window-size="${size/x/,}" "$BASE/" >/dev/null 2>&1
  firefox --headless --no-remote --profile "$reduced_profile" \
    --screenshot="$OUT_DIR/home-open-reduced-${size}.png" \
    --window-size="${size/x/,}" "$BASE/?hero=open" >/dev/null 2>&1
  for entry in home check validated registry token chain glossary guide pool certificate; do
    case "$entry" in
      home) path=/ ;;
      check) path=/check ;;
      validated) path=/validated ;;
      registry) path=/registry ;;
      token) path=/tokens/NVDA ;;
      chain) path=/chains/solana ;;
      glossary) path=/glossary ;;
      guide) path=/guide/verify-a-stock-token ;;
      pool) path=/validated/solana/5DnhFALRYpbTjLViVSEaqGsUoHw5DY8XLyyF3qaMkgqT ;;
      certificate) path=/v/$fixture_id ;;
    esac
    firefox --headless --no-remote --profile "$profile" --screenshot="$OUT_DIR/${entry}-${size}.png" \
      --window-size="${size/x/,}" "$BASE$path" >/dev/null 2>&1
  done
  firefox --headless --no-remote --profile "$profile" \
    --screenshot="$OUT_DIR/home-open-${size}.png" \
    --window-size="${size/x/,}" "$BASE/?hero=open" >/dev/null 2>&1
done

printf 'crawl passed; screenshots: %s\n' "$OUT_DIR"
