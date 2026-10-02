use super::REGISTRY_PAGE_SIZE;
use crate::{
    attest,
    attest::{Attestation, Read},
    chain::Chain,
    check::{CheckResult, Verdict},
    discovery::{self, FeaturedPool, LeaderboardEntry, format_balance},
    pool::{PoolInfo, TokenSide},
    registry::{self, Entry, Registry},
    state::AppState,
};
use askama::Template;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;

pub(crate) fn verified_attestations(state: &AppState) -> Vec<Attestation> {
    let Ok(index) = state.attestations.read() else { return Vec::new() };
    let mut latest = HashMap::<(Chain, String), Attestation>::new();
    for attestation in index.values() {
        let key = (attestation.chain, attestation.pool.pool.clone());
        let replace =
            latest.get(&key).is_none_or(|current| attestation.checked_at > current.checked_at);
        if replace {
            latest.insert(key, attestation.clone());
        }
    }
    let mut attestations: Vec<_> = latest
        .into_values()
        .filter(|attestation| {
            matches!(attestation.verdict, Verdict::Verified { .. })
                && attest::valid_for_state(state, &attestation.id, attestation)
        })
        .collect();
    attestations.sort_by(|left, right| {
        right
            .checked_at
            .cmp(&left.checked_at)
            .then_with(|| left.chain.to_string().cmp(&right.chain.to_string()))
            .then_with(|| left.pool.pool.cmp(&right.pool.pool))
    });
    attestations
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct RegistryTableQuery {
    pub(crate) q: Option<String>,
    pub(crate) page: Option<usize>,
}

fn registry_entry_matches(entry: &Entry, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let chain = entry.chain.to_string();
    [
        entry.issuer.as_str(),
        entry.ticker.as_str(),
        entry.name.as_str(),
        chain.as_str(),
        entry.contract.as_str(),
        entry.source.as_str(),
    ]
    .iter()
    .any(|field| field.to_ascii_lowercase().contains(query))
}
#[derive(Debug, Template)]
#[template(path = "validated.html")]
pub(crate) struct ValidatedTemplate {
    pub(crate) asset_version: u64,
    pub(crate) public_url: String,
    pub(crate) cards: Vec<ValidatedCardView>,
    pub(crate) rebuilding: bool,
}

#[derive(Debug)]
pub(crate) struct ValidatedCardView {
    pub(crate) chain: String,
    pub(crate) chain_icon: &'static str,
    pub(crate) dex: String,
    pub(crate) base_symbol: String,
    pub(crate) quote_symbol: String,
    pub(crate) verdict_class: &'static str,
    pub(crate) verdict_icon: &'static str,
    pub(crate) verdict_label: &'static str,
    pub(crate) issuer_ticker: String,
    pub(crate) quote_balance: String,
    pub(crate) share_label: String,
    pub(crate) share_value: String,
    pub(crate) updated_at: String,
    pub(crate) detail_url: String,
}

impl ValidatedCardView {
    pub(crate) fn from_attestation(attestation: &Attestation) -> Self {
        let chain = chain_slug(attestation.chain);
        let share = attestation.quote_share_of_supply.filter(|value| value.is_finite());
        let issuer_ticker = match (&attestation.issuer, &attestation.ticker) {
            (Some(issuer), Some(ticker)) => format!("{issuer} · {ticker}"),
            (Some(issuer), None) => issuer.clone(),
            (None, Some(ticker)) => ticker.clone(),
            (None, None) => String::new(),
        };
        Self {
            chain: attestation.chain.to_string(),
            chain_icon: chain_icon(attestation.chain),
            dex: prettify_dex(&attestation.pool.dex),
            base_symbol: attestation
                .pool
                .base
                .symbol
                .clone()
                .unwrap_or_else(|| "Unknown".to_owned()),
            quote_symbol: attestation
                .pool
                .quote
                .symbol
                .clone()
                .unwrap_or_else(|| "Unknown".to_owned()),
            verdict_class: "is-verified",
            verdict_icon: "qed-seal-verified",
            verdict_label: "Verified",
            issuer_ticker,
            quote_balance: format_token_balance(&attestation.pool.quote),
            share_label: share.map(format_share).unwrap_or_else(|| "n/a".to_owned()),
            share_value: share
                .map(|value| format!("{}", (value * 100.0).clamp(0.0, 100.0)))
                .unwrap_or_else(|| "0".to_owned()),
            updated_at: attestation.checked_at.clone(),
            detail_url: format!("/validated/{}/{}", chain, attestation.pool.pool),
        }
    }

    pub(crate) fn from_leaderboard_entry(entry: &LeaderboardEntry) -> Self {
        let chain = parse_chain(&entry.chain);
        let chain_label = if entry.chain_label.is_empty() {
            entry.chain.clone()
        } else {
            entry.chain_label.clone()
        };
        let chain_icon = chain.map(chain_icon).unwrap_or("qed-seal");
        let issuer_ticker = match (&entry.issuer, &entry.ticker) {
            (Some(issuer), Some(ticker)) => format!("{issuer} · {ticker}"),
            (Some(issuer), None) => issuer.clone(),
            (None, Some(ticker)) => ticker.clone(),
            (None, None) => String::new(),
        };
        Self {
            chain: chain_label,
            chain_icon,
            dex: prettify_dex(&entry.dex),
            base_symbol: entry.base_symbol.clone(),
            quote_symbol: entry.quote_symbol.clone(),
            verdict_class: "is-verified",
            verdict_icon: "qed-seal-verified",
            verdict_label: "Provisional",
            issuer_ticker,
            quote_balance: "n/a".to_owned(),
            share_label: "n/a".to_owned(),
            share_value: "0".to_owned(),
            updated_at: entry.checked_at.clone().unwrap_or_default(),
            detail_url: entry.detail_url.clone(),
        }
    }
}

fn pair_label(attestation: &Attestation) -> String {
    format!(
        "{}/{}",
        attestation.pool.base.symbol.as_deref().unwrap_or("Unknown"),
        attestation.pool.quote.symbol.as_deref().unwrap_or("Unknown")
    )
}

#[derive(Debug)]
pub(crate) struct LeaderboardPageView {
    pub(crate) rows: Vec<LeaderboardRowView>,
    pub(crate) json: String,
    pub(crate) status_line: String,
    pub(crate) status_tooltip: String,
    pub(crate) show_empty: bool,
}

#[derive(Debug)]
pub(crate) struct LeaderboardRowView {
    pub(crate) rank: String,
    pub(crate) chain_icon: String,
    pub(crate) chain_label: String,
    pub(crate) dex: String,
    pub(crate) base_symbol: String,
    pub(crate) quote_symbol: String,
    pub(crate) verdict: String,
    pub(crate) verdict_label: String,
    pub(crate) seal_icon: String,
    pub(crate) price: String,
    pub(crate) change: String,
    pub(crate) change_class: String,
    pub(crate) volume: String,
    pub(crate) liquidity: String,
    pub(crate) detail_url: String,
    pub(crate) trade_url: String,
}

fn html_safe_json(value: &str) -> String {
    value
        .replace('&', "\\u0026")
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
}

impl LeaderboardPageView {
    pub(crate) fn from_value(value: Value) -> Self {
        let rows = value
            .get("entries")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .take(10)
            .map(LeaderboardRowView::from_value)
            .collect();
        let updated_at = value.get("updated_at").and_then(Value::as_str).unwrap_or("");
        let next_refresh_at = value.get("next_refresh_at").and_then(Value::as_str).unwrap_or("");
        let prices_updated_at =
            value.get("prices_updated_at").and_then(Value::as_str).unwrap_or("");
        let source = value.get("source").and_then(Value::as_str).unwrap_or("QED");
        let refreshing = value.get("refreshing").and_then(Value::as_bool).unwrap_or(false);
        let empty_successful =
            value.get("empty_successful").and_then(Value::as_bool).unwrap_or(false);
        let has_entries = value
            .get("entries")
            .and_then(Value::as_array)
            .is_some_and(|entries| !entries.is_empty());
        let status_line = if refreshing {
            "Updating…".to_owned()
        } else if !has_entries && !empty_successful {
            "Building the first board…".to_owned()
        } else {
            let freshness = relative_time(updated_at);
            if is_stale(updated_at) {
                format!("Updated {freshness} · stale")
            } else {
                format!("Updated {freshness}")
            }
        };
        let status_tooltip = format!(
            "{source} · next refresh {} · prices updated {}",
            refresh_time(next_refresh_at, refreshing),
            relative_time(prices_updated_at)
        );
        let json = serde_json::to_string(&value)
            .map(|json| html_safe_json(&json))
            .unwrap_or_else(|_| "{\"entries\":[]}".to_owned());
        Self { rows, json, status_line, status_tooltip, show_empty: empty_successful }
    }
}

impl LeaderboardRowView {
    fn from_value(value: &Value) -> Self {
        let verdict = text(value, "verdict");
        let chain = text(value, "chain");
        let pool = text(value, "pool");
        let chain_icon =
            if chain == "robinhoodchain" { "robinhood".to_owned() } else { chain.clone() };
        let source_url = text(value, "trade_url");
        let trade_url = discovery::chain_from_dex_id(&chain)
            .map(|chain| {
                discovery::canonical_market_url(
                    chain,
                    &pool,
                    (!source_url.is_empty()).then_some(source_url.as_str()),
                )
            })
            .unwrap_or_default();
        let detail_url = discovery::chain_from_dex_id(&chain)
            .map(|chain| format!("/validated/{}/{}", discovery::chain_slug(chain), pool))
            .unwrap_or_default();
        let change_value = number(value, "change_24h_pct");
        let change_class = if change_value.is_some_and(|number| number > 0.0) {
            "is-positive"
        } else if change_value.is_some_and(|number| number < 0.0) {
            "is-negative"
        } else {
            ""
        };
        Self {
            rank: text(value, "rank"),
            chain_icon,
            chain_label: text(value, "chain_label"),
            dex: prettify_dex(&text(value, "dex")),
            base_symbol: text(value, "base_symbol"),
            quote_symbol: text(value, "quote_symbol"),
            verdict: verdict.clone(),
            verdict_label: match verdict.as_str() {
                "verified" => "Verified".to_owned(),
                "mismatch" => "Mismatch".to_owned(),
                "nomatch" => "No match".to_owned(),
                _ => "Unknown".to_owned(),
            },
            seal_icon: match verdict.as_str() {
                "verified" => "qed-seal-verified".to_owned(),
                "mismatch" | "nomatch" => "qed-seal-broken".to_owned(),
                _ => "qed-seal-unknown".to_owned(),
            },
            price: usd_label(number(value, "price_usd")),
            change: percent_label(change_value),
            change_class: change_class.to_owned(),
            volume: usd_label(number(value, "volume_24h_usd")),
            liquidity: usd_label(number(value, "liquidity_usd")),
            detail_url,
            trade_url,
        }
    }
}

fn text(value: &Value, key: &str) -> String {
    value.get(key).map_or_else(String::new, |value| match value {
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        Value::Null => String::new(),
        other => other.to_string(),
    })
}

fn number(value: &Value, key: &str) -> Option<f64> {
    value.get(key).and_then(Value::as_f64).filter(|number| number.is_finite())
}

fn usd_label(value: Option<f64>) -> String {
    value.map(|number| format!("${}", format_number(number))).unwrap_or_else(|| "—".to_owned())
}

fn percent_label(value: Option<f64>) -> String {
    value
        .map(|number| format!("{}{number:.2}%", if number >= 0.0 { "+" } else { "" }))
        .unwrap_or_else(|| "—".to_owned())
}

fn relative_text(seconds: i64) -> String {
    let amount = seconds.unsigned_abs();
    if amount < 60 {
        return "just now".to_owned();
    }
    let (value, unit) = if amount < 3_600 {
        (amount.div_ceil(60), "min")
    } else if amount < 86_400 {
        (amount.div_ceil(3_600), "h")
    } else {
        (amount.div_ceil(86_400), "d")
    };
    format!("{value} {unit}{}", if value == 1 { "" } else { "s" })
}

fn relative_time(value: &str) -> String {
    let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(value) else {
        return "—".to_owned();
    };
    let seconds = (chrono::Utc::now() - parsed.with_timezone(&chrono::Utc)).num_seconds();
    let text = relative_text(seconds);
    if text == "just now" {
        text
    } else if seconds >= 0 {
        format!("{text} ago")
    } else {
        format!("in {text}")
    }
}

fn is_stale(value: &str) -> bool {
    let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(value) else {
        return false;
    };
    (chrono::Utc::now() - parsed.with_timezone(&chrono::Utc)).num_seconds() >= 3_600
}

fn refresh_time(value: &str, refreshing: bool) -> String {
    let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(value) else {
        return "updating now".to_owned();
    };
    let seconds = (parsed.with_timezone(&chrono::Utc) - chrono::Utc::now()).num_seconds();
    if refreshing || seconds <= 0 {
        "updating now".to_owned()
    } else {
        format!("in {}", relative_text(seconds))
    }
}

fn grouped_count(value: usize) -> String {
    let digits = value.to_string();
    let mut output = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, byte) in digits.bytes().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            output.push(',');
        }
        output.push(char::from(byte));
    }
    output
}

pub(crate) fn registry_status_line(value: &Value, fallback_entries: usize) -> String {
    let registry = value.get("registry").unwrap_or(&Value::Null);
    let entries = registry
        .get("entries")
        .and_then(Value::as_u64)
        .map_or(fallback_entries, |number| number as usize);
    let updated_at = registry.get("updated_at").and_then(Value::as_str).unwrap_or("");
    let refreshing = registry.get("refreshing").and_then(Value::as_bool).unwrap_or(false);
    let restored = registry.get("restored").and_then(Value::as_bool).unwrap_or(false);
    let status = if refreshing || restored {
        "updating…".to_owned()
    } else {
        format!("updated {}", relative_time(updated_at))
    };
    format!("{} contracts · {status}", grouped_count(entries))
}

pub(crate) fn registry_status_tooltip(value: &Value) -> String {
    let registry = value.get("registry").unwrap_or(&Value::Null);
    let next_refresh_at = registry.get("next_refresh_at").and_then(Value::as_str).unwrap_or("");
    let refreshing = registry.get("refreshing").and_then(Value::as_bool).unwrap_or(false);
    format!("Issuer registry · next refresh {}", refresh_time(next_refresh_at, refreshing))
}
#[derive(Debug, Template)]
#[template(path = "index.html")]
pub(crate) struct IndexTemplate {
    pub(crate) asset_version: u64,
    pub(crate) public_url: String,
    pub(crate) leaderboard: LeaderboardPageView,
}

#[derive(Debug, Template)]
#[template(path = "check.html")]
pub(crate) struct CheckTemplate {
    pub(crate) asset_version: u64,
    pub(crate) public_url: String,
}

#[derive(Debug, Template)]
#[template(path = "check_result_page.html")]
pub(crate) struct CheckResultPageTemplate {
    pub(crate) asset_version: u64,
    pub(crate) public_url: String,
    pub(crate) canonical_path: String,
    pub(crate) result: String,
}

#[derive(Debug, Template)]
#[template(path = "registry.html")]
pub(crate) struct RegistryTemplate {
    pub(crate) asset_version: u64,
    pub(crate) registry_count: usize,
    pub(crate) registry_status_line: String,
    pub(crate) registry_status_tooltip: String,
    pub(crate) public_url: String,
}
#[derive(Debug, Template)]
#[template(path = "token.html")]
pub(crate) struct TokenTemplate {
    pub(crate) asset_version: u64,
    pub(crate) public_url: String,
    pub(crate) ticker: String,
    pub(crate) title: String,
    pub(crate) contracts: Vec<DirectoryContractView>,
    pub(crate) pools: Vec<DirectoryPoolView>,
    pub(crate) json_ld: String,
}

#[derive(Debug, Template)]
#[template(path = "wallet.html")]
pub(crate) struct WalletTemplate {
    pub(crate) asset_version: u64,
    pub(crate) public_url: String,
}

#[derive(Debug, Template)]
#[template(path = "wallet_holdings.html")]
pub(crate) struct WalletHoldingsTemplate {
    pub(crate) asset_version: u64,
    pub(crate) public_url: String,
    pub(crate) address: String,
    pub(crate) holdings: Vec<WalletHoldingView>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct WalletHoldingView {
    pub(crate) symbol: String,
    pub(crate) amount: String,
    pub(crate) verdict: String,
    pub(crate) verdict_class: String,
    pub(crate) chain: String,
    pub(crate) pool_url: Option<String>,
    pub(crate) trade_links: Vec<WalletTradeLinkView>,
    pub(crate) reason: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct WalletTradeLinkView {
    pub(crate) label: String,
    pub(crate) url: String,
}

#[derive(Debug, Template)]
#[template(path = "chain.html")]
pub(crate) struct ChainTemplate {
    pub(crate) asset_version: u64,
    pub(crate) public_url: String,
    pub(crate) chain: String,
    pub(crate) chain_slug: String,
    pub(crate) contract_count: usize,
    pub(crate) contracts: Vec<DirectoryContractView>,
    pub(crate) pools: Vec<DirectoryPoolView>,
    pub(crate) json_ld: String,
}

#[derive(Debug)]
pub(crate) struct DirectoryContractView {
    pub(crate) issuer: String,
    pub(crate) ticker: String,
    pub(crate) chain: String,
    pub(crate) chain_kind: Chain,
    pub(crate) chain_icon: &'static str,
    pub(crate) contract: String,
    pub(crate) explorer_url: String,
    pub(crate) source_url: String,
    pub(crate) powers: DirectoryPowersView,
}

#[derive(Debug)]
pub(crate) struct DirectoryPowersView {
    pub(crate) available: bool,
    pub(crate) can_seize: Vec<String>,
    pub(crate) can_block: Vec<String>,
    pub(crate) can_change_rules: Vec<String>,
    pub(crate) unavailable: Vec<String>,
    pub(crate) summary_badges: Vec<String>,
    pub(crate) source_verified_subject: String,
    pub(crate) source_verified: String,
    pub(crate) source_verified_proxy: String,
    pub(crate) source_badge: String,
    pub(crate) observed_at: String,
}

impl DirectoryPowersView {
    pub(crate) fn unavailable() -> Self {
        Self {
            available: false,
            can_seize: Vec::new(),
            can_block: Vec::new(),
            can_change_rules: Vec::new(),
            unavailable: Vec::new(),
            summary_badges: vec!["Signals unavailable (transient)".to_owned()],
            source_verified_proxy: String::new(),
            source_verified_subject: "Source verification".to_owned(),
            source_badge: "Source unavailable".to_owned(),
            source_verified: "Unavailable".to_owned(),
            observed_at: String::new(),
        }
    }
}

#[derive(Debug)]
pub(crate) struct DirectoryPoolView {
    pub(crate) chain: String,
    pub(crate) dex: String,
    pub(crate) pair: String,
    pub(crate) verdict: String,
    pub(crate) verdict_class: String,
    pub(crate) detail_url: String,
    pub(crate) trade_url: String,
}

#[derive(Debug, Template)]
#[template(path = "glossary.html")]
pub(crate) struct GlossaryTemplate {
    pub(crate) asset_version: u64,
    pub(crate) public_url: String,
}

#[derive(Debug, Template)]
#[template(path = "guide_verify.html")]
pub(crate) struct GuideVerifyTemplate {
    pub(crate) asset_version: u64,
    pub(crate) public_url: String,
}

#[derive(Debug)]
pub(crate) struct RegistryEntryView {
    pub(crate) issuer: String,
    pub(crate) ticker: String,
    pub(crate) chain: String,
    pub(crate) contract: String,
    pub(crate) source: String,
    pub(crate) source_url: String,
    pub(crate) last_checked: String,
}

impl From<&Entry> for RegistryEntryView {
    fn from(entry: &Entry) -> Self {
        Self {
            issuer: entry.issuer.clone(),
            ticker: entry.ticker.clone(),
            chain: entry.chain.to_string(),
            contract: entry.contract.clone(),
            source: entry.source.clone(),
            source_url: entry.source_url.clone(),
            last_checked: entry.last_checked.clone(),
        }
    }
}
#[derive(Debug, Template)]
#[template(path = "registry_table.html")]
pub(crate) struct RegistryTableTemplate {
    pub(crate) entries: Vec<RegistryEntryView>,
    pub(crate) query: String,
    pub(crate) page: usize,
    pub(crate) page_count: usize,
    pub(crate) first_entry: usize,
    pub(crate) last_entry: usize,
    pub(crate) total_entries: usize,
    pub(crate) previous_page: usize,
    pub(crate) next_page: usize,
    pub(crate) has_previous: bool,
    pub(crate) has_next: bool,
}

impl RegistryTableTemplate {
    pub(crate) fn from_registry(registry: &Registry, query: RegistryTableQuery) -> Self {
        let RegistryTableQuery { q, page: requested_page } = query;
        let query = q.unwrap_or_default().trim().to_ascii_lowercase();
        let mut entries: Vec<_> = registry
            .iter()
            .filter(|entry| registry::matchable(entry))
            .filter(|entry| registry_entry_matches(entry, &query))
            .map(RegistryEntryView::from)
            .collect();
        entries.sort_by(|left, right| {
            left.issuer
                .cmp(&right.issuer)
                .then_with(|| left.ticker.cmp(&right.ticker))
                .then_with(|| left.chain.cmp(&right.chain))
                .then_with(|| left.contract.cmp(&right.contract))
        });
        let total_entries = entries.len();
        let page_count = total_entries.div_ceil(REGISTRY_PAGE_SIZE).max(1);
        let page = requested_page.unwrap_or(1).max(1).min(page_count);
        let start = (page - 1) * REGISTRY_PAGE_SIZE;
        let entries = entries.into_iter().skip(start).take(REGISTRY_PAGE_SIZE).collect();
        let first_entry = if total_entries == 0 { 0 } else { start + 1 };
        let last_entry = (start + REGISTRY_PAGE_SIZE).min(total_entries);
        Self {
            entries,
            query,
            page,
            page_count,
            first_entry,
            last_entry,
            total_entries,
            previous_page: page.saturating_sub(1),
            next_page: (page + 1).min(page_count),
            has_previous: page > 1,
            has_next: page < page_count,
        }
    }
}

#[derive(Debug)]
pub(crate) struct FeaturedView {
    pub(crate) chain: String,
    pub(crate) chain_icon: &'static str,
    pub(crate) dex: String,
    pub(crate) pool: String,
    pub(crate) curated: bool,
    pub(crate) base_symbol: String,
    pub(crate) quote_symbol: String,
    pub(crate) verdict_class: &'static str,
    pub(crate) verdict_icon: &'static str,
    pub(crate) verdict_label: &'static str,
    pub(crate) issuer_ticker: String,
    pub(crate) quote_balance: String,
    pub(crate) share_label: String,
    pub(crate) share_value: String,
    pub(crate) liquidity: String,
    pub(crate) volume: String,
    pub(crate) has_liquidity: bool,
    pub(crate) has_volume: bool,
    pub(crate) updated_at: String,
}

impl FeaturedView {
    pub(crate) fn from_pool(pool: &FeaturedPool) -> Self {
        let (verdict_class, verdict_icon, verdict_label) = match pool.verdict.as_str() {
            "verified" => ("is-verified", "qed-seal-verified", "Verified"),
            "mismatch" => ("is-mismatch", "qed-seal-broken", "Mismatch"),
            "nomatch" => ("is-mismatch", "qed-seal-broken", "No match"),
            _ => ("is-unknown", "qed-seal-unknown", "Unknown"),
        };
        let issuer_ticker = match (&pool.issuer, &pool.ticker) {
            (Some(issuer), Some(ticker)) => format!("{issuer} · {ticker}"),
            (Some(issuer), None) => issuer.clone(),
            (None, Some(ticker)) => ticker.clone(),
            (None, None) => String::new(),
        };
        let share = pool.quote_share_of_supply.filter(|value| value.is_finite());
        let share_label = share.map(format_share).unwrap_or_else(|| "n/a".to_owned());
        let share_value = share
            .map(|value| format!("{}", (value * 100.0).clamp(0.0, 100.0)))
            .unwrap_or_else(|| "0".to_owned());
        Self {
            chain: pool.chain.to_string(),
            chain_icon: chain_icon(pool.chain),
            dex: prettify_dex(&pool.dex),
            pool: pool.pool.clone(),
            curated: pool.curated,
            base_symbol: pool.base_symbol.clone(),
            quote_symbol: pool.quote_symbol.clone(),
            verdict_class,
            verdict_icon,
            verdict_label,
            issuer_ticker,
            quote_balance: pool
                .quote_balance
                .as_deref()
                .map(format_token_display)
                .unwrap_or_else(|| "Unavailable".to_owned()),
            share_label,
            share_value,
            liquidity: pool.liquidity_usd.map(format_usd).unwrap_or_default(),
            volume: pool.volume_24h_usd.map(format_usd).unwrap_or_default(),
            has_liquidity: pool.liquidity_usd.is_some(),
            has_volume: pool.volume_24h_usd.is_some(),
            updated_at: pool.updated_at.clone(),
        }
    }
}
#[derive(Debug, Template)]
#[template(path = "featured.html")]
pub(crate) struct FeaturedTemplate {
    pub(crate) pools: Vec<FeaturedView>,
}

#[derive(Debug, Template)]
#[template(path = "result.html")]
pub(crate) struct ResultTemplate {
    pub(crate) invalid: bool,
    pub(crate) error: String,
    pub(crate) result: ResultView,
}

impl ResultTemplate {
    pub(crate) fn invalid() -> Self {
        Self {
            invalid: true,
            error: "Enter a valid Solana or EVM address.".to_owned(),
            result: ResultView::empty(),
        }
    }
    pub(crate) fn from_check(check: CheckResult, attestation: Option<Attestation>) -> Self {
        Self {
            invalid: false,
            error: String::new(),
            result: ResultView::from_check(check, attestation),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct TradeLink {
    pub(crate) label: String,
    pub(crate) url: String,
}

fn allowed_external_url(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else { return false };
    parsed.scheme() == "https"
        && matches!(
            parsed.host_str(),
            Some(
                "dexscreener.com"
                    | "www.dexscreener.com"
                    | "geckoterminal.com"
                    | "www.geckoterminal.com"
                    | "app.uniswap.org"
                    | "pancakeswap.finance"
                    | "pump.fun"
                    | "raydium.io"
                    | "www.orca.so"
                    | "app.meteora.ag"
                    | "solscan.io"
                    | "basescan.org"
                    | "etherscan.io"
                    | "bscscan.com"
                    | "robinhoodchain.blockscout.com"
            )
        )
}
fn trade_links(pool: &PoolInfo, dexscreener_url: Option<String>) -> Vec<TradeLink> {
    let dex = pool.dex.to_ascii_lowercase();
    let base = &pool.base.address;
    let quote = &pool.quote.address;
    let market_url =
        discovery::canonical_market_url(pool.chain, &pool.pool, dexscreener_url.as_deref());
    let mut links = Vec::new();
    if dex.contains("uniswap") {
        let chain_slug = match pool.chain {
            Chain::RobinhoodChain => "robinhood",
            Chain::Base => "base",
            Chain::Ethereum => "ethereum",
            Chain::Bnb => "bnb",
            Chain::Solana => "",
        };
        if !chain_slug.is_empty() {
            links.push(TradeLink {
                label: "Uniswap pool".to_owned(),
                url: format!("https://app.uniswap.org/explore/pools/{chain_slug}/{}", pool.pool),
            });
            links.push(TradeLink {
                label: "Uniswap swap".to_owned(),
                url: format!(
                    "https://app.uniswap.org/swap?chain={chain_slug}&inputCurrency={base}&outputCurrency={quote}"
                ),
            });
        }
    } else if pool.chain == Chain::Solana && dex.contains("pump") {
        links.push(TradeLink {
            label: "pump.fun".to_owned(),
            url: format!("https://pump.fun/coin/{base}"),
        });
    } else if pool.chain == Chain::Solana && dex.contains("raydium") {
        links.push(TradeLink {
            label: "Raydium swap".to_owned(),
            url: format!("https://raydium.io/swap/?inputMint={base}&outputMint={quote}"),
        });
    } else if pool.chain == Chain::Solana && dex.contains("orca") {
        links.push(TradeLink {
            label: "Orca pool".to_owned(),
            url: format!("https://www.orca.so/pools/{}", pool.pool),
        });
    } else if pool.chain == Chain::Solana && dex.contains("meteora") {
        links.push(TradeLink {
            label: "Meteora DLMM".to_owned(),
            url: format!("https://app.meteora.ag/dlmm/{}", pool.pool),
        });
    }
    links.push(TradeLink { label: "DexScreener".to_owned(), url: market_url });
    links.push(TradeLink {
        label: "Explorer".to_owned(),
        url: explorer_link(pool.chain, &pool.pool),
    });
    links
}
#[derive(Debug)]
pub(crate) struct ResultView {
    pub(crate) chain: String,
    pub(crate) chain_icon: &'static str,
    pub(crate) dex: String,
    pub(crate) base_symbol: String,
    pub(crate) quote_symbol: String,
    pub(crate) verdict_class: &'static str,
    pub(crate) seal_icon: &'static str,
    pub(crate) verdict_label: String,
    pub(crate) sentence: String,
    pub(crate) share_label: String,
    pub(crate) share_value: String,
    pub(crate) checked_at: String,
    pub(crate) has_attestation: bool,
    pub(crate) attestation_id: String,
    pub(crate) has_pool: bool,
    pub(crate) pool_address: String,
    pub(crate) base_address: String,
    pub(crate) quote_address: String,
    pub(crate) trade_links: Vec<TradeLink>,
    pub(crate) reads: Vec<ReadView>,
    pub(crate) evidence: Vec<String>,
    pub(crate) block_label: String,
    pub(crate) slot_label: String,
    pub(crate) registry_entry: String,
    pub(crate) registry_hash: String,
    pub(crate) signer: String,
    pub(crate) signature: String,
}

impl ResultView {
    fn empty() -> Self {
        Self {
            chain: String::new(),
            chain_icon: "?",
            dex: String::new(),
            base_symbol: String::new(),
            quote_symbol: String::new(),
            verdict_class: "is-unknown",
            seal_icon: "qed-seal",
            verdict_label: String::new(),
            sentence: String::new(),
            share_label: String::new(),
            share_value: "0".to_owned(),
            checked_at: String::new(),
            has_attestation: false,
            attestation_id: String::new(),
            has_pool: false,
            pool_address: String::new(),
            base_address: String::new(),
            quote_address: String::new(),
            trade_links: Vec::new(),
            evidence: Vec::new(),
            reads: Vec::new(),
            block_label: String::new(),
            slot_label: String::new(),
            registry_entry: String::new(),
            registry_hash: String::new(),
            signer: String::new(),
            signature: String::new(),
        }
    }
    fn from_check(check: CheckResult, attestation: Option<Attestation>) -> Self {
        let mut view = Self::empty();
        view.chain = check.chain.to_string();
        view.chain_icon = chain_icon(check.chain);
        view.verdict_class = match &check.verdict {
            Verdict::Verified { .. } => "is-verified",
            Verdict::Mismatch { .. } | Verdict::NoMatch => "is-mismatch",
            Verdict::Unknown { .. } => "is-unknown",
        };
        view.seal_icon = match &check.verdict {
            Verdict::Verified { .. } => "qed-seal-verified",
            Verdict::Unknown { .. } => "qed-seal-unknown",
            Verdict::Mismatch { .. } | Verdict::NoMatch => "qed-seal-broken",
        };
        view.verdict_label = match &check.verdict {
            Verdict::Verified { issuer, ticker } => format!("Verified · {issuer} {ticker}"),
            Verdict::Mismatch { .. } | Verdict::NoMatch => "Not verified".to_owned(),
            Verdict::Unknown { .. } => "Not verified".to_owned(),
        };
        let mismatch_issuer = attestation
            .as_ref()
            .and_then(|value| value.registry_entry.as_ref())
            .map(|entry| entry.issuer.as_str())
            .unwrap_or("the issuer");
        view.sentence = match &check.verdict {
            Verdict::Verified { issuer, ticker } => {
                format!("The pool's quote contract matches {issuer}'s registry entry for {ticker}.")
            }
            Verdict::Mismatch { claimed, .. } => format!(
                "The token claims {claimed}, but its quote contract differs from {mismatch_issuer}'s registry entry."
            ),
            Verdict::NoMatch => {
                "The pool's quote contract is not in any issuer registry QED knows.".to_owned()
            }
            Verdict::Unknown { reason } => format!("QED could not read this pool: {reason}."),
        };
        view.share_label = check
            .quote_share_of_supply
            .filter(|value| value.is_finite())
            .map(format_share)
            .unwrap_or_else(|| "n/a".to_owned());
        view.share_value = check
            .quote_share_of_supply
            .filter(|value| value.is_finite())
            .map(|value| format!("{}", (value * 100.0).clamp(0.0, 100.0)))
            .unwrap_or_else(|| "0".to_owned());
        view.checked_at = check.checked_at;
        view.attestation_id = check.attestation_id.unwrap_or_default();
        view.has_attestation = !view.attestation_id.is_empty();
        view.evidence = check.evidence;
        if let Some(pool) = check.pool {
            view.trade_links = trade_links(&pool, None);
            view.base_symbol = pool.base.symbol.clone().unwrap_or_else(|| "Unknown".to_owned());
            view.quote_symbol = pool.quote.symbol.clone().unwrap_or_else(|| "Unknown".to_owned());
            view.pool_address = pool.pool;
            view.base_address = pool.base.address;
            view.quote_address = pool.quote.address;
            view.has_pool = true;
        }
        if let Some(attestation) = attestation {
            view.reads = attestation.reads.iter().map(ReadView::from_read).collect();
            view.block_label = attestation
                .block
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unknown".to_owned());
            view.slot_label = attestation
                .slot
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unknown".to_owned());
            view.registry_entry = attestation
                .registry_entry
                .as_ref()
                .map(|entry| format!("{} {} · {}", entry.issuer, entry.ticker, entry.contract))
                .unwrap_or_else(|| "None".to_owned());
            view.registry_hash = attestation.registry_hash;
            view.signer = attestation.signer;
            view.signature = attestation.signature;
        }
        view
    }
}
#[derive(Debug, Template)]
#[template(path = "certificate.html")]
pub(crate) struct CertificateTemplate {
    pub(crate) asset_version: u64,
    pub(crate) public_url: String,
    pub(crate) certificate: CertificateView,
}

#[derive(Debug, Template)]
#[template(path = "certificate_result.html")]
pub(crate) struct CertificateResultTemplate {
    pub(crate) certificate: CertificateView,
}
#[derive(Debug)]
pub(crate) struct CertificateView {
    pub(crate) id: String,
    pub(crate) id_short: String,
    pub(crate) serial: String,
    pub(crate) chain: String,
    pub(crate) chain_icon: &'static str,
    pub(crate) subject: String,
    pub(crate) pair: String,
    pub(crate) ticker: String,
    pub(crate) dex: String,
    pub(crate) pool_address: String,
    pub(crate) trade_links: Vec<TradeLink>,
    pub(crate) issuer_match: String,
    pub(crate) verdict_class: &'static str,
    pub(crate) seal_icon: &'static str,
    pub(crate) verdict_label: String,
    pub(crate) sentence: String,
    pub(crate) signature_status_class: &'static str,
    pub(crate) signature_status_label: String,
    pub(crate) share_label: String,
    pub(crate) share_value: String,
    pub(crate) checked_at: String,
    pub(crate) expires_at: String,
    pub(crate) signer: String,
    pub(crate) signer_key_id: String,
    pub(crate) registry_hash: String,
    pub(crate) signature: String,
    pub(crate) canonical_json: String,
    pub(crate) public_key_url: String,
    pub(crate) attestation_api_url: String,
    pub(crate) block_label: String,
    pub(crate) slot_label: String,
    pub(crate) reads: Vec<ReadView>,
    pub(crate) notice: String,
    pub(crate) live_url: String,
    pub(crate) json_ld: String,
    pub(crate) is_live: bool,
}

impl CertificateView {
    pub(crate) fn from_attestation(attestation: Attestation) -> Self {
        let chain = chain_slug(attestation.chain);
        let subject = attestation.pool.pool.clone();
        let id = attestation.id.clone();
        let serial = short_id(&id);
        let signature_valid = attest::verify(&attestation).is_ok();
        let (signature_status_class, signature_status_label) = if !signature_valid {
            ("is-unknown", "Signature could not be verified".to_owned())
        } else if matches!(attestation.verdict, Verdict::Verified { .. }) {
            ("is-verified", "Signature valid · contract match".to_owned())
        } else {
            ("is-mismatch", "Contract match not verified".to_owned())
        };
        let id_short = serial.clone();
        let verdict_class = match &attestation.verdict {
            Verdict::Verified { .. } => "is-verified",
            Verdict::Mismatch { .. } | Verdict::NoMatch => "is-mismatch",
            Verdict::Unknown { .. } => "is-unknown",
        };
        let verdict_label = match &attestation.verdict {
            Verdict::Verified { issuer, ticker } => format!("Verified · {issuer} {ticker}"),
            Verdict::Mismatch { .. } | Verdict::NoMatch | Verdict::Unknown { .. } => {
                "Not verified".to_owned()
            }
        };
        let sentence = match &attestation.verdict {
            Verdict::Verified { issuer, ticker } => {
                format!("The pool's quote contract matches {issuer}'s registry entry for {ticker}.")
            }
            Verdict::Mismatch { claimed, .. } => {
                let issuer = attestation
                    .registry_entry
                    .as_ref()
                    .map(|entry| entry.issuer.as_str())
                    .unwrap_or("the issuer");
                format!(
                    "The token claims {claimed}, but its quote contract differs from {issuer}'s registry entry."
                )
            }
            Verdict::NoMatch => {
                "The pool's quote contract is not in any issuer registry QED knows.".to_owned()
            }
            Verdict::Unknown { reason } => format!("QED could not read this pool: {reason}."),
        };
        let share = attestation.quote_share_of_supply.filter(|value| value.is_finite());
        let issuer_match = attestation
            .registry_entry
            .as_ref()
            .map(|entry| format!("{} · {}", entry.issuer, entry.ticker))
            .unwrap_or_else(|| "none".to_owned());
        let pair = pair_label(&attestation);
        let canonical_json =
            attest::canonical_payload_json(&attestation).unwrap_or_else(|_| "{}".to_owned());
        let signer_key_id = attestation.signer.chars().take(8).collect();
        let json_ld = html_safe_json(
            &serde_json::json!({
                "@context": "https://schema.org",
                "@type": "WebPage",
                "name": format!("{} on {}: contract match · QED", pair, attestation.chain),
                "url": format!("/validated/{chain}/{subject}"),
                "mainEntity": {
                    "@type": "Dataset",
                    "name": format!("QED attestation for {pair}"),
                    "description": sentence.clone(),
                    "identifier": id,
                    "subjectOf": {
                        "@type": "Claim",
                        "claimReviewed": sentence
                    },
                    "distribution": {
                        "@type": "DataDownload",
                        "contentUrl": format!("/api/attest/{id}"),
                        "encodingFormat": "application/json"
                    }
                }
            })
            .to_string(),
        );
        Self {
            id: id.clone(),
            id_short,
            serial,
            chain: attestation.chain.to_string(),
            chain_icon: chain_icon(attestation.chain),
            subject: attestation.subject.clone(),
            pair: pair.clone(),
            ticker: attestation.ticker.clone().unwrap_or_default(),
            dex: prettify_dex(&attestation.pool.dex),
            pool_address: subject.clone(),
            trade_links: trade_links(
                &attestation.pool,
                Some(discovery::canonical_market_url(attestation.chain, &subject, None)),
            ),
            issuer_match,
            verdict_class,
            seal_icon: match &attestation.verdict {
                Verdict::Verified { .. } => "qed-seal-verified",
                Verdict::Unknown { .. } => "qed-seal-unknown",
                Verdict::Mismatch { .. } | Verdict::NoMatch => "qed-seal-broken",
            },
            verdict_label,
            sentence: sentence.clone(),
            signature_status_class,
            signature_status_label,
            share_label: share.map(format_share).unwrap_or_else(|| "n/a".to_owned()),
            share_value: share
                .map(|value| format!("{}", (value * 100.0).clamp(0.0, 100.0)))
                .unwrap_or_else(|| "0".to_owned()),
            checked_at: attestation.checked_at.clone(),
            expires_at: attestation.expires_at.clone(),
            signer: attestation.signer,
            signer_key_id,
            registry_hash: attestation.registry_hash,
            signature: attestation.signature,
            canonical_json,
            public_key_url: "/.well-known/qed.json".to_owned(),
            attestation_api_url: format!("/api/attest/{id}"),
            block_label: attestation
                .block
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unknown".to_owned()),
            slot_label: attestation
                .slot
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unknown".to_owned()),
            reads: attestation.reads.iter().map(ReadView::from_read).collect(),
            notice: String::new(),
            live_url: format!("/validated/{chain}/{subject}"),
            json_ld,
            is_live: false,
        }
    }

    pub(crate) fn from_live_attestation(attestation: Attestation) -> Self {
        let mut view = Self::from_attestation(attestation);
        view.is_live = true;
        view
    }
}

pub(crate) fn explorer_link(chain: Chain, subject: &str) -> String {
    let base = match chain {
        Chain::Solana => "https://solscan.io/account/",
        Chain::RobinhoodChain => "https://robinhoodchain.blockscout.com/address/",
        Chain::Base => "https://basescan.org/address/",
        Chain::Ethereum => "https://etherscan.io/address/",
        Chain::Bnb => "https://bscscan.com/address/",
    };
    format!("{base}{subject}")
}

#[derive(Debug)]
pub(crate) struct ReadView {
    pub(crate) method: String,
    pub(crate) params: String,
    pub(crate) result_hash: String,
    pub(crate) result_hash_short: String,
    pub(crate) raw_result: String,
    pub(crate) block_label: String,
    pub(crate) slot_label: String,
}

impl ReadView {
    fn from_read(read: &Read) -> Self {
        Self {
            method: read.method.clone(),
            params: serde_json::to_string(&read.params).unwrap_or_else(|_| "{}".to_owned()),
            result_hash: read.result_hash.clone(),
            result_hash_short: short_id(&read.result_hash),
            raw_result: read
                .raw_result
                .as_ref()
                .and_then(|value| serde_json::to_string(value).ok())
                .unwrap_or_default(),
            block_label: read
                .block
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unknown".to_owned()),
            slot_label: read
                .slot
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unknown".to_owned()),
        }
    }
}

pub(crate) fn short_id(value: &str) -> String {
    value.chars().take(12).collect()
}

fn format_number(value: f64) -> String {
    if !value.is_finite() {
        return "Unavailable".to_owned();
    }
    let value = if value.abs() < 1.0 {
        if value == 0.0 {
            return "0".to_owned();
        }
        let decimal_places = (3 - value.abs().log10().floor() as i32).max(0) as usize;
        let mut text = format!("{value:.decimal_places$}");
        trim_decimal_zeros(&mut text);
        text
    } else {
        let mut text = format!("{value:.2}");
        trim_decimal_zeros(&mut text);
        text
    };
    group_integer(&value)
}

fn trim_decimal_zeros(value: &mut String) {
    if let Some((integer, fraction)) = value.split_once('.') {
        let fraction = fraction.trim_end_matches('0');
        if fraction.is_empty() {
            value.truncate(integer.len());
        } else {
            value.truncate(integer.len() + 1 + fraction.len());
        }
    }
}

fn group_integer(value: &str) -> String {
    let (sign, body) = value.strip_prefix('-').map_or(("", value), |body| ("-", body));
    let (integer, fraction) = body.split_once('.').unwrap_or((body, ""));
    let mut grouped = String::with_capacity(value.len() + integer.len() / 3);
    grouped.push_str(sign);
    for (index, byte) in integer.bytes().enumerate() {
        if index > 0 && (integer.len() - index) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(char::from(byte));
    }
    if !fraction.is_empty() {
        grouped.push('.');
        grouped.push_str(fraction);
    }
    grouped
}

fn format_token_display(value: &str) -> String {
    let mut parts = value.splitn(2, char::is_whitespace);
    let number = parts.next().unwrap_or_default().replace(',', "");
    let Some(number_value) = number.parse::<f64>().ok().filter(|value| value.is_finite()) else {
        return value.to_owned();
    };
    let formatted = format_number(number_value);
    let symbol = parts.next().map(str::trim).filter(|symbol| !symbol.is_empty());
    symbol.map_or(formatted.clone(), |symbol| format!("{formatted} {symbol}"))
}

fn format_token_balance(side: &TokenSide) -> String {
    format_balance(
        side.balance.as_deref().unwrap_or_default(),
        side.decimals,
        side.symbol.as_deref(),
    )
    .map(|value| format_token_display(&value))
    .unwrap_or_else(|| "Unavailable".to_owned())
}

fn format_usd(value: f64) -> String {
    format!("${}", format_number(value))
}

fn format_share(value: f64) -> String {
    let percent = value * 100.0;
    if percent.is_finite() && percent > 0.0 && percent < 0.01 {
        "under 0.01%".to_owned()
    } else if percent.is_finite() {
        format!("{percent:.2}%")
    } else {
        "n/a".to_owned()
    }
}
pub(crate) fn parse_chain(value: &str) -> Option<Chain> {
    Chain::parse(value)
}

pub(crate) fn chain_slug(chain: Chain) -> &'static str {
    match chain {
        Chain::Solana => "solana",
        Chain::RobinhoodChain => "robinhood",
        Chain::Base => "base",
        Chain::Ethereum => "ethereum",
        Chain::Bnb => "bnb",
    }
}

pub(crate) fn chain_icon(chain: Chain) -> &'static str {
    match chain {
        Chain::Solana => "solana",
        Chain::RobinhoodChain => "robinhood",
        Chain::Base => "base",
        Chain::Ethereum => "ethereum",
        Chain::Bnb => "bnb",
    }
}

fn prettify_dex(dex: &str) -> String {
    match dex {
        "meteora" => "Meteora".to_owned(),
        "orca" => "Orca".to_owned(),
        "pancakeswap" => "PancakeSwap".to_owned(),
        "pancake-v3" => "PancakeSwap v3".to_owned(),
        "pumpswap" => "PumpSwap".to_owned(),
        "ramses" => "Ramses".to_owned(),
        "raydium" => "Raydium".to_owned(),
        "raydium-amm" => "Raydium AMM".to_owned(),
        "raydium-cpmm" => "Raydium CPMM".to_owned(),
        "uniswap" => "Uniswap".to_owned(),
        "uniswap-v2" => "Uniswap v2".to_owned(),
        "uniswap-v3" => "Uniswap v3".to_owned(),
        "uniswap-v4" => "Uniswap v4".to_owned(),
        other => other.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        LeaderboardPageView, LeaderboardRowView, ValidatedCardView, allowed_external_url,
        explorer_link, html_safe_json, trade_links,
    };
    use crate::discovery::LeaderboardEntry;
    use crate::pool::{PoolInfo, TokenSide};
    use serde_json::json;
    #[test]
    fn html_safe_json_escapes_script_breakout_characters() {
        let escaped = html_safe_json(r#"{"token":"</script><meta>&"}"#);
        let escaped = html_safe_json(&format!("{escaped}\u{2028}\u{2029}"));
        assert!(!escaped.contains("</script>"));
        assert!(escaped.contains("\\u003c/script\\u003e"));
        assert!(escaped.contains("\\u0026"));
        assert!(escaped.contains("\\u2028"));
        assert!(escaped.contains("\\u2029"));
    }

    #[test]
    fn external_links_require_known_https_hosts() {
        assert!(allowed_external_url("https://dexscreener.com/base/0x1"));
        assert!(!allowed_external_url("http://dexscreener.com/base/0x1"));
        assert!(!allowed_external_url("https://evil.example/base/0x1"));
        assert!(allowed_external_url("https://robinhoodchain.blockscout.com/address/0x1"));
        assert!(!allowed_external_url("https://explorer.mainnet.chain.robinhood.com/address/0x1"));
    }

    #[test]
    fn robinhood_explorer_link_uses_blockscout() {
        assert_eq!(
            explorer_link(crate::chain::Chain::RobinhoodChain, "0xpool"),
            "https://robinhoodchain.blockscout.com/address/0xpool"
        );
    }

    #[test]
    fn stale_robinhood_market_url_is_repaired_to_dexscreener_slug() {
        let row = LeaderboardRowView::from_value(&json!({
            "chain": "robinhoodchain",
            "pool": "0xd4EB21209C4D6093f80B5b84f5C45cc093EA14a3",
            "trade_url": "https://dexscreener.com/robinhoodchain/0xd4EB21209C4D6093f80B5b84f5C45cc093EA14a3",
        }));
        assert_eq!(
            row.trade_url,
            "https://dexscreener.com/robinhood/0xd4EB21209C4D6093f80B5b84f5C45cc093EA14a3"
        );
    }
    #[test]
    fn uniswap_pool_link_includes_chain_segment() {
        let links = trade_links(
            &PoolInfo {
                chain: crate::chain::Chain::Ethereum,
                pool: "0xpool".to_owned(),
                dex: "uniswap-v4".to_owned(),
                base: TokenSide {
                    address: "0xbase".to_owned(),
                    symbol: Some("NVDAx".to_owned()),
                    decimals: Some(18),
                    balance: None,
                },
                quote: TokenSide {
                    address: "0xquote".to_owned(),
                    symbol: Some("USDC".to_owned()),
                    decimals: Some(6),
                    balance: None,
                },
            },
            None,
        );
        assert!(links.iter().any(|link| {
            link.label == "Uniswap pool"
                && link.url == "https://app.uniswap.org/explore/pools/ethereum/0xpool"
        }));
    }
    #[test]
    fn empty_first_board_shows_building_state() {
        let view = LeaderboardPageView::from_value(json!({
            "updated_at": "2026-09-26T00:00:00Z",
            "next_refresh_at": "2026-09-26T00:05:00Z",
            "entries": [],
            "refreshing": false,
            "empty_successful": false,
        }));
        assert_eq!(view.status_line, "Building the first board…");
        assert!(!view.show_empty);
    }

    #[test]
    fn successful_empty_board_shows_empty_state() {
        let view = LeaderboardPageView::from_value(json!({
            "updated_at": "2026-09-26T00:00:00Z",
            "next_refresh_at": "2026-09-26T00:05:00Z",
            "entries": [],
            "refreshing": false,
            "empty_successful": true,
        }));
        assert!(view.status_line.starts_with("Updated "));
        assert!(view.show_empty);
    }

    #[test]
    fn stale_restored_board_keeps_rows_and_marks_stale() {
        let view = LeaderboardPageView::from_value(json!({
            "updated_at": "2020-01-01T00:00:00Z",
            "next_refresh_at": "2026-09-26T00:05:00Z",
            "entries": [{ "pool": "pool-1" }],
            "refreshing": false,
            "empty_successful": false,
        }));
        assert!(view.status_line.contains("stale"));
        assert!(!view.show_empty);
    }

    #[test]
    fn restored_verified_row_becomes_provisional_directory_card() {
        let entry = LeaderboardEntry {
            rank: 1,
            chain: "solana".to_owned(),
            chain_label: "Solana".to_owned(),
            dex: "raydium-amm".to_owned(),
            pool: "pool".to_owned(),
            base_symbol: "NVDA".to_owned(),
            quote_symbol: "USDG".to_owned(),
            issuer: Some("Issuer".to_owned()),
            ticker: Some("NVDA".to_owned()),
            verdict: "verified".to_owned(),
            price_usd: None,
            change_24h_pct: None,
            volume_24h_usd: None,
            liquidity_usd: None,
            txns_24h: None,
            detail_url: "/validated/solana/pool".to_owned(),
            trade_url: "https://dexscreener.com/solana/pool".to_owned(),
            explorer_url: String::new(),
            attestation_id: None,
            checked_at: Some("2026-09-26T00:00:00Z".to_owned()),
        };
        let card = ValidatedCardView::from_leaderboard_entry(&entry);
        assert_eq!(card.verdict_label, "Provisional");
        assert_eq!(card.share_label, "n/a");
        assert_eq!(card.detail_url, entry.detail_url);
    }
}
