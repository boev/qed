use super::REGISTRY_PAGE_SIZE;
use crate::{
    adapters::discovery::{self, FeaturedPool, LeaderboardEntry, format_balance},
    app::{attestation as attest, context::Context, wallet::WalletHoldingRow},
    domain::{
        attestation::{Attestation, Read},
        chain::Chain,
        check::Verdict,
        pool::{PoolInfo, TokenSide},
        registry::{self, Entry, Registry},
    },
};
use askama::Template;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Template)]
#[template(path = "guard.html")]
pub(crate) struct GuardTemplate {
    pub(crate) asset_version: u64,
    pub(crate) public_url: String,
    pub(crate) address: String,
    pub(crate) chain: String,
    pub(crate) wallet: String,
    pub(crate) error: String,
}
#[derive(Debug)]
pub(crate) struct GuardReasonView {
    pub(crate) detail: String,
}

#[derive(Debug)]
pub(crate) struct GuardDeploymentView {
    pub(crate) network: String,
    pub(crate) address: String,
}

#[derive(Debug)]
pub(crate) struct GuardPoolView {
    pub(crate) address: String,
    pub(crate) venue: String,
    pub(crate) quote: String,
    pub(crate) verdict: String,
    pub(crate) observed_at: String,
}

#[derive(Debug, Template)]
#[template(path = "guard_result.html")]
pub(crate) struct GuardResultTemplate {
    pub(crate) asset_version: u64,
    pub(crate) public_url: String,
    pub(crate) result_url: String,
    pub(crate) chain: String,
    pub(crate) address: String,
    pub(crate) verdict_class: String,
    pub(crate) headline: String,
    pub(crate) identity_sentence: String,
    pub(crate) powers_sentence: String,
    pub(crate) source_sentence: String,
    pub(crate) seizure_summary: String,
    pub(crate) blocking_summary: String,
    pub(crate) rule_change_summary: String,
    pub(crate) identity_status: String,
    pub(crate) publisher: String,
    pub(crate) ticker: String,
    pub(crate) matched_contract: String,
    pub(crate) source_status: String,
    pub(crate) source_provider: String,
    pub(crate) wallet_check_status: String,
    pub(crate) wallet_check_restrictions: Vec<GuardReasonView>,
    pub(crate) powers: DirectoryPowersView,
    pub(crate) reasons: Vec<GuardReasonView>,
    pub(crate) pools: Vec<GuardPoolView>,
    pub(crate) deployments: Vec<GuardDeploymentView>,
    pub(crate) document_json: String,
}
fn guard_identity_sentence(document: &crate::domain::guard::GuardDocument) -> String {
    use crate::domain::guard::{GuardSubjectType, IdentityStatus};

    let publisher = document
        .identity
        .publisher
        .as_deref()
        .or(document.identity.candidate.as_ref().map(|candidate| candidate.publisher.as_str()))
        .unwrap_or("the issuer");
    let ticker = document
        .identity
        .ticker
        .as_deref()
        .or(document.identity.candidate.as_ref().map(|candidate| candidate.ticker.as_str()))
        .unwrap_or("the claimed ticker");
    let chain = document.chain.to_string();
    if document.subject_type == GuardSubjectType::Pool
        && document.reasons.iter().any(|reason| reason.code == "pool_unavailable")
    {
        return "QED could not read this pool — retry".to_owned();
    }

    match document.identity.status {
        IdentityStatus::Match => match document.subject_type {
            GuardSubjectType::Token => {
                format!("This is {publisher}'s published {ticker} contract on {chain}.")
            }
            GuardSubjectType::Pool => {
                format!("This pool includes {publisher}'s published {ticker} contract on {chain}.")
            }
        },
        IdentityStatus::Mismatch => document
            .identity
            .unpublished_product_detail
            .clone()
            .unwrap_or_else(|| match document.subject_type {
                GuardSubjectType::Token => format!(
                    "This token presents itself as {ticker}, but {publisher} publishes a different contract on {chain}."
                ),
                GuardSubjectType::Pool => format!(
                    "This pool includes a token that presents itself as {ticker}, but {publisher} publishes a different contract on {chain}."
                ),
            }),
        IdentityStatus::NoPublisher => {
            "QED knows no issuer that publishes this contract.".to_owned()
        }
        IdentityStatus::RegistryStale => {
            format!("The issuer registry entry for this contract on {chain} is stale.")
        }
        IdentityStatus::RegistryRemoved => {
            format!("This contract is no longer listed in the issuer registry on {chain}.")
        }
    }
}

fn power_control_label(
    code: &str,
    powers: Option<&crate::domain::powers::PowersRecord>,
) -> &'static str {
    match code {
        "permanent_delegate" => "permanent delegate",
        "pausable" => match powers.and_then(|record| record.token_paused) {
            Some(true) => "pausable (currently paused)",
            Some(false) => "pausable (currently not paused)",
            None => "pausable",
        },
        "pauser" => "pauser control",
        "sanctions_list" => "sanctions list",
        "freeze_authority" => "freeze authority",
        "default_account_state_frozen" => "new accounts start frozen",
        "pausable_authority" => "pause authority",
        "mint_paused" => "mint currently paused",
        "transfer_hook" => "transfer hook",
        "confidential_transfer_approval" => "confidential-transfer approval",
        "eip1967_implementation" => "proxy implementation",
        "eip1967_admin" => "proxy admin",
        "eip1967_beacon" => "proxy beacon",
        "owner_getter" => "owner role",
        "mint_authority" => "mint authority",
        "transfer_hook_authority" => "transfer-hook authority",
        "token_program_upgrade_authority" => "token-program upgrade authority",
        "wallet_blacklist" => "wallet blacklist",
        _ => "additional observed control",
    }
}

fn power_category_summary(
    label: &str,
    reasons: &[crate::domain::powers::Reason],
    powers: Option<&crate::domain::powers::PowersRecord>,
) -> String {
    if reasons.is_empty() {
        return if powers.is_some_and(|record| record.unavailable.is_empty()) {
            format!("{label}: no evidence observed.")
        } else {
            format!("{label}: not determined — a read was unavailable.")
        };
    }

    let mut labels = String::new();
    for reason in reasons {
        let label = power_control_label(&reason.code, powers);
        if labels.split(", ").any(|existing| existing == label) {
            continue;
        }
        if !labels.is_empty() {
            labels.push_str(", ");
        }
        labels.push_str(label);
    }
    format!("{label}: yes — {labels}.")
}

fn guard_powers_sentence(document: &crate::domain::guard::GuardDocument) -> String {
    let Some(powers) = document.powers.as_ref() else {
        return "QED could not determine issuer controls because power evidence was unavailable."
            .to_owned();
    };
    let block = !powers.can_block.is_empty();
    let change_rules = !powers.can_change_rules.is_empty();
    let seize = !powers.can_seize.is_empty();
    let actions = match (block, change_rules, seize) {
        (true, false, false) => "block transfers",
        (false, true, false) => "change token rules",
        (false, false, true) => "seize tokens",
        (true, true, false) => "block transfers and change token rules",
        (true, false, true) => "block transfers and seize tokens",
        (false, true, true) => "change token rules and seize tokens",
        (true, true, true) => "block transfers, change token rules, and seize tokens",
        (false, false, false) if powers.unavailable.is_empty() => {
            return "QED observed no issuer controls.".to_owned();
        }
        (false, false, false) => {
            return "QED could not determine issuer controls because some reads were unavailable."
                .to_owned();
        }
    };
    if document.identity.status == crate::domain::guard::IdentityStatus::Match {
        format!("The issuer can {actions}.")
    } else {
        format!("Observed token controls can {actions}.")
    }
}

impl GuardResultTemplate {
    pub(crate) fn from_document(
        asset_version: u64,
        public_url: String,
        document: &crate::domain::guard::GuardDocument,
    ) -> Self {
        use crate::domain::guard::{GuardVerdict, IdentityStatus, SourceStatus};
        let identity_status = match document.identity.status {
            IdentityStatus::Match => "match",
            IdentityStatus::Mismatch => "mismatch",
            IdentityStatus::NoPublisher => "no publisher known",
            IdentityStatus::RegistryStale => "registry stale",
            IdentityStatus::RegistryRemoved => "registry removed",
        };
        let source_status = match document.source.status {
            SourceStatus::Verified => "verified",
            SourceStatus::Unverified => "unverified",
            SourceStatus::Unavailable => "unavailable",
        };
        let (verdict, headline) = match document.verdict {
            GuardVerdict::Allow => ("allow", "Allow"),
            GuardVerdict::Deny => ("deny", "Deny"),
            GuardVerdict::Unknown => ("unknown", "Unknown"),
        };
        let chain_slug = match document.chain {
            Chain::Solana => "solana",
            Chain::RobinhoodChain => "robinhood",
            Chain::Base => "base",
            Chain::Ethereum => "ethereum",
            Chain::Bnb => "bnb",
        };
        let wallet_check_status = match document.wallet_check.as_ref().map(|check| check.status) {
            Some(crate::domain::guard::WalletCheckStatus::Checked) => "checked".to_owned(),
            Some(crate::domain::guard::WalletCheckStatus::NotApplicable) => {
                "not applicable".to_owned()
            }
            Some(crate::domain::guard::WalletCheckStatus::Unavailable) => "unavailable".to_owned(),
            None => "not requested".to_owned(),
        };
        let identity_sentence = guard_identity_sentence(document);
        let powers_record = document.powers.as_ref();
        let powers_sentence = guard_powers_sentence(document);
        let source_sentence = match document.source.status {
            SourceStatus::Verified => "Source verified.",
            SourceStatus::Unavailable => "QED could not check source verification.",
            SourceStatus::Unverified => "Source not verified.",
        }
        .to_owned();
        let seizure_summary = power_category_summary(
            "Can seize tokens",
            powers_record.map(|record| record.can_seize.as_slice()).unwrap_or(&[]),
            powers_record,
        );
        let blocking_summary = power_category_summary(
            "Can block transfers",
            powers_record.map(|record| record.can_block.as_slice()).unwrap_or(&[]),
            powers_record,
        );
        let rule_change_summary = power_category_summary(
            "Can change token rules",
            powers_record.map(|record| record.can_change_rules.as_slice()).unwrap_or(&[]),
            powers_record,
        );
        let result_url =
            format!("{}/guard/{chain_slug}/{}", public_url.trim_end_matches('/'), document.address);
        Self {
            asset_version,
            public_url,
            result_url,
            chain: document.chain.to_string(),
            address: document.address.clone(),
            verdict_class: verdict.to_owned(),
            headline: headline.to_owned(),
            identity_sentence,
            powers_sentence,
            source_sentence,
            seizure_summary,
            blocking_summary,
            rule_change_summary,
            identity_status: identity_status.to_owned(),
            publisher: document
                .identity
                .publisher
                .clone()
                .unwrap_or_else(|| "No publisher known".to_owned()),
            ticker: document.identity.ticker.clone().unwrap_or_else(|| "—".to_owned()),
            matched_contract: document
                .identity
                .matched_contract
                .clone()
                .unwrap_or_else(|| "—".to_owned()),
            deployments: document
                .identity
                .deployments
                .iter()
                .map(|deployment| GuardDeploymentView {
                    network: deployment.network.clone(),
                    address: deployment.address.clone(),
                })
                .collect(),
            source_status: source_status.to_owned(),
            source_provider: document.source.provider.clone(),
            wallet_check_status,
            wallet_check_restrictions: document
                .wallet_check
                .as_ref()
                .map(|check| {
                    check
                        .restrictions
                        .iter()
                        .map(|reason| GuardReasonView { detail: reason.detail.clone() })
                        .collect()
                })
                .unwrap_or_default(),
            powers: document
                .powers
                .clone()
                .map(super::pages::powers_view)
                .unwrap_or_else(DirectoryPowersView::unavailable),
            reasons: document
                .reasons
                .iter()
                .map(|reason| GuardReasonView { detail: reason.detail.clone() })
                .collect(),
            pools: document
                .pools
                .iter()
                .map(|pool| GuardPoolView {
                    address: pool.address.clone(),
                    venue: pool.venue.clone(),
                    quote: pool
                        .quote
                        .symbol
                        .as_ref()
                        .map(|symbol| format!("{symbol} · {}", pool.quote.address))
                        .unwrap_or_else(|| pool.quote.address.clone()),
                    verdict: pool.verdict.clone(),
                    observed_at: pool.observed_at.clone().unwrap_or_else(|| "unknown".to_owned()),
                })
                .collect(),
            document_json: serde_json::to_string_pretty(document).unwrap_or_default(),
        }
    }
}

pub(crate) async fn verified_attestations(state: &Context) -> Vec<Attestation> {
    let latest = {
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
        latest
    };
    let registry = state.registry.snapshot().await;
    let mut attestations = Vec::new();
    for attestation in latest.into_values() {
        if matches!(&attestation.verdict, Verdict::Verified { .. })
            && attest::valid_for_state_with_registry(
                state,
                &registry,
                &attestation.id,
                &attestation,
            )
        {
            attestations.push(attestation);
        }
    }
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

pub(crate) fn order_pair_symbols<'a>(
    base: &'a str,
    quote: &'a str,
    issuer_on_base: Option<bool>,
) -> (&'a str, &'a str) {
    if issuer_on_base == Some(false) { (quote, base) } else { (base, quote) }
}

pub(crate) fn ordered_pair_label(base: &str, quote: &str, issuer_on_base: Option<bool>) -> String {
    let (first, second) = order_pair_symbols(base, quote, issuer_on_base);
    format!("{first} / {second}")
}

fn attestation_pair_label(attestation: &Attestation) -> String {
    let issuer_on_base = attestation.registry_entry.as_ref().and_then(|entry| {
        let matches = |address: &str| {
            if attestation.chain == Chain::Solana {
                address == entry.contract
            } else {
                address.eq_ignore_ascii_case(&entry.contract)
            }
        };
        if matches(&attestation.pool.base.address) {
            Some(true)
        } else if matches(&attestation.pool.quote.address) {
            Some(false)
        } else {
            None
        }
    });
    ordered_pair_label(
        attestation.pool.base.symbol.as_deref().unwrap_or("Unknown"),
        attestation.pool.quote.symbol.as_deref().unwrap_or("Unknown"),
        issuer_on_base,
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
    pub(crate) source_label: String,
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
            if is_stale(next_refresh_at) {
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
        let read_status = text(value, "read_status");
        let chain = text(value, "chain");
        let pool = text(value, "pool");
        let chain_icon =
            if chain == "robinhoodchain" { "robinhood".to_owned() } else { chain.clone() };
        let source_url = text(value, "trade_url");
        let source = if text(value, "source") == "geckoterminal" {
            discovery::MarketSource::Geckoterminal
        } else {
            discovery::MarketSource::Dexscreener
        };
        let source_label = source.label().to_owned();
        let trade_url = discovery::chain_from_dex_id(&chain)
            .map(|chain| {
                discovery::canonical_market_url_for_source(
                    chain,
                    &pool,
                    (!source_url.is_empty()).then_some(source_url.as_str()),
                    source,
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
        let base_symbol = text(value, "base_symbol");
        let quote_symbol = text(value, "quote_symbol");
        let (base_symbol, quote_symbol) = order_pair_symbols(
            &base_symbol,
            &quote_symbol,
            value.get("issuer_on_base").and_then(Value::as_bool),
        );
        Self {
            rank: text(value, "rank"),
            chain_icon,
            chain_label: text(value, "chain_label"),
            dex: prettify_dex(&text(value, "dex")),
            base_symbol: base_symbol.to_owned(),
            quote_symbol: quote_symbol.to_owned(),
            verdict: verdict.clone(),
            verdict_label: if read_status == "not_read_yet" && verdict == "unknown" {
                "Not read yet".to_owned()
            } else {
                match verdict.as_str() {
                    "verified" => "Verified".to_owned(),
                    "mismatch" => "Mismatch".to_owned(),
                    "nomatch" => "No match".to_owned(),
                    _ => "Unknown".to_owned(),
                }
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
            source_label,
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
    format!("{value} {unit}")
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

fn is_stale(next_refresh_at: &str) -> bool {
    let Ok(next_refresh) = chrono::DateTime::parse_from_rfc3339(next_refresh_at) else {
        return false;
    };
    chrono::Utc::now() >= next_refresh.with_timezone(&chrono::Utc)
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
#[template(path = "content.html")]
pub(crate) struct ContentPageTemplate {
    pub(crate) asset_version: u64,
    pub(crate) public_url: String,
    pub(crate) page_title: String,
    pub(crate) heading: String,
    pub(crate) eyebrow: String,
    pub(crate) intro: String,
    pub(crate) meta_description: String,
    pub(crate) canonical_path: String,
    pub(crate) body_html: String,
}

#[derive(Debug, Default)]
pub(crate) struct WhatsNewView {
    pub(crate) available: bool,
    pub(crate) release_label: String,
    pub(crate) headline: String,
    pub(crate) changelog_href: String,
    pub(crate) has_latest_blog: bool,
    pub(crate) latest_blog_title: String,
    pub(crate) latest_blog_href: String,
}

#[derive(Debug, Template)]
#[template(path = "docs.html")]
pub(crate) struct DocsPageTemplate {
    pub(crate) asset_version: u64,
    pub(crate) public_url: String,
    pub(crate) page_title: String,
    pub(crate) heading: String,
    pub(crate) eyebrow: String,
    pub(crate) intro: String,
    pub(crate) meta_description: String,
    pub(crate) canonical_path: String,
    pub(crate) active_sidebar: String,
    pub(crate) body_html: String,
    pub(crate) whats_new: WhatsNewView,
}

#[derive(Debug, Template)]
#[template(path = "index.html")]
pub(crate) struct IndexTemplate {
    pub(crate) asset_version: u64,
    pub(crate) public_url: String,
    pub(crate) stats_line: String,
    pub(crate) leaderboard: LeaderboardPageView,
    pub(crate) whats_new: WhatsNewView,
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

#[derive(Debug, Clone, Serialize)]
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

#[derive(Debug, Clone, Serialize)]
pub(crate) struct WalletTradeLinkView {
    pub(crate) label: String,
    pub(crate) url: String,
}

pub(crate) fn wallet_holding_views(rows: Vec<WalletHoldingRow>) -> Vec<WalletHoldingView> {
    let mut views = rows.into_iter().map(wallet_holding_view).collect::<Vec<_>>();
    views.sort_by(|left, right| {
        left.chain.cmp(&right.chain).then_with(|| left.symbol.cmp(&right.symbol))
    });
    views
}

fn wallet_holding_view(row: WalletHoldingRow) -> WalletHoldingView {
    let WalletHoldingRow { holding, entry, pools, verdict } = row;
    let mut pool_url = pools.first().map(|pool| pool.pool_url.clone());
    let mut trade_links = pools
        .iter()
        .filter(|pool| !pool.trade_url.is_empty())
        .map(|pool| WalletTradeLinkView {
            label: format!("Market {}", pool.symbol.as_deref().unwrap_or("pool")),
            url: pool.trade_url.clone(),
        })
        .collect::<Vec<_>>();
    trade_links.sort_by(|left, right| left.url.cmp(&right.url));
    trade_links.dedup_by(|left, right| left.url == right.url);
    if let Some(entry) = entry {
        return WalletHoldingView {
            symbol: entry.ticker.clone(),
            amount: format_wallet_amount(&holding.amount, holding.decimals.or(entry.decimals)),
            verdict: "Contract matches issuer registry".to_owned(),
            verdict_class: "is-verified".to_owned(),
            chain: holding.chain.to_string(),
            pool_url,
            trade_links,
            reason: None,
        };
    }
    if let (Some(pool), Some(checked)) = (pools.first(), verdict) {
        let (verdict, verdict_class, reason) = match checked.verdict {
            Verdict::Verified { issuer, ticker } => {
                (format!("Verified · {issuer} {ticker}"), "is-verified".to_owned(), None)
            }
            Verdict::Mismatch { claimed, actual } => (
                "Mismatch".to_owned(),
                "is-negative".to_owned(),
                Some(format!("Claimed {claimed}; registry contract is {actual}.")),
            ),
            Verdict::NoMatch => (
                "No match".to_owned(),
                "is-negative".to_owned(),
                Some("The pool quote did not match a registry issuer.".to_owned()),
            ),
            Verdict::Unknown { reason } => {
                ("Unknown".to_owned(), "is-unknown".to_owned(), Some(reason))
            }
        };
        return WalletHoldingView {
            symbol: holding
                .symbol
                .or_else(|| pool.symbol.clone())
                .unwrap_or_else(|| "Unknown".to_owned()),
            amount: format_wallet_amount(&holding.amount, holding.decimals),
            verdict,
            verdict_class,
            chain: holding.chain.to_string(),
            pool_url: pool_url.take(),
            trade_links,
            reason,
        };
    }
    WalletHoldingView {
        symbol: holding.symbol.unwrap_or_else(|| "Unknown".to_owned()),
        amount: format_wallet_amount(&holding.amount, holding.decimals),
        verdict: "Unknown".to_owned(),
        verdict_class: "is-unknown".to_owned(),
        chain: holding.chain.to_string(),
        pool_url: None,
        trade_links: Vec::new(),
        reason: Some("No issuer registry or known pool match.".to_owned()),
    }
}

pub(crate) fn format_wallet_amount(raw: &str, decimals: Option<u8>) -> String {
    let Some(decimals) = decimals else { return raw.to_owned() };
    if decimals == 0 {
        return raw.to_owned();
    }
    let Ok(value) = raw.parse::<u128>() else { return raw.to_owned() };
    let mut digits = value.to_string();
    let places = usize::from(decimals);
    if digits.len() <= places {
        digits = format!("{}{}", "0".repeat(places + 1 - digits.len()), digits);
    }
    let split = digits.len() - places;
    let integer = &digits[..split];
    let fraction = digits[split..].trim_end_matches('0');
    if fraction.is_empty() { integer.to_owned() } else { format!("{integer}.{fraction}") }
}

#[derive(Debug, Template)]
#[template(path = "wallet_holdings.html")]
pub(crate) struct WalletHoldingsTemplate {
    pub(crate) asset_version: u64,
    pub(crate) public_url: String,
    pub(crate) address: String,
    pub(crate) holdings: Vec<WalletHoldingView>,
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
    pub(crate) summary_sentence: String,
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
            summary_sentence: "QED could not read token controls for this contract; retry."
                .to_owned(),
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
    pub(crate) page_title: String,
    pub(crate) heading: String,
    pub(crate) eyebrow: String,
    pub(crate) intro: String,
    pub(crate) meta_description: String,
    pub(crate) canonical_path: String,
    pub(crate) active_sidebar: String,
    pub(crate) whats_new: WhatsNewView,
}

#[derive(Debug, Template)]
#[template(path = "guide_verify.html")]
pub(crate) struct GuideVerifyTemplate {
    pub(crate) asset_version: u64,
    pub(crate) public_url: String,
    pub(crate) page_title: String,
    pub(crate) heading: String,
    pub(crate) eyebrow: String,
    pub(crate) intro: String,
    pub(crate) meta_description: String,
    pub(crate) canonical_path: String,
    pub(crate) active_sidebar: String,
    pub(crate) whats_new: WhatsNewView,
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
    pub(crate) chain_slug: &'static str,
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
            chain_slug: crate::adapters::discovery::chain_slug(pool.chain),
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

#[derive(Debug, Clone)]
pub(crate) struct TradeLink {
    pub(crate) label: String,
    pub(crate) url: String,
}

#[cfg(test)]
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
        let signature_valid = crate::domain::attestation::verify(&attestation).is_ok();
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
            Verdict::Unknown { .. } => "QED could not read this pool — retry".to_owned(),
        };
        let share = attestation.quote_share_of_supply.filter(|value| value.is_finite());
        let issuer_match = attestation
            .registry_entry
            .as_ref()
            .map(|entry| format!("{} · {}", entry.issuer, entry.ticker))
            .unwrap_or_else(|| "none".to_owned());
        let pair = attestation_pair_label(&attestation);
        let canonical_json = crate::domain::attestation::canonical_payload_json(&attestation)
            .unwrap_or_else(|_| "{}".to_owned());
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
    discovery::chain_slug(chain)
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
        GuardResultTemplate, LeaderboardPageView, LeaderboardRowView, ValidatedCardView,
        allowed_external_url, explorer_link, guard_identity_sentence, html_safe_json,
        relative_text, trade_links,
    };
    use crate::adapters::discovery::LeaderboardEntry;
    use crate::domain::{
        chain::Chain,
        guard::{
            GuardDocument, GuardIdentity, GuardReason, GuardSource, GuardSubjectType, GuardVerdict,
            IdentityStatus, SourceStatus,
        },
        pool::{PoolInfo, TokenSide},
        powers::{PowersRecord, Reason, SourceVerified, SourceVerifiedSubject},
    };
    use askama::Template;
    use serde_json::json;
    fn guard_document() -> GuardDocument {
        GuardDocument {
            id: "guard-id".to_owned(),
            kind: "guard".to_owned(),
            chain: Chain::Ethereum,
            address: "0x0000000000000000000000000000000000000001".to_owned(),
            subject_type: GuardSubjectType::Token,
            subject_address: Some("0x0000000000000000000000000000000000000001".to_owned()),
            wallet: None,
            wallet_check: None,
            identity: GuardIdentity {
                publisher: Some("Backed xStocks".to_owned()),
                matched_contract: Some("0x0000000000000000000000000000000000000001".to_owned()),
                ticker: Some("NVDA".to_owned()),
                status: IdentityStatus::Match,
                candidate: None,
                unpublished_product_detail: None,
                observed_symbol: None,
                observed_name: None,
                deployments: Vec::new(),
            },
            powers: Some(PowersRecord {
                chain: Chain::Ethereum,
                contract: "0x0000000000000000000000000000000000000001".to_owned(),
                can_seize: vec![Reason::new(
                    "permanent_delegate",
                    "Permanent delegate may transfer or burn units.",
                )],
                can_block: vec![
                    Reason::new("pausable", "pausable() is implemented; currently not paused."),
                    Reason::new("sanctions_list", "sanctionsList() returned a configured address."),
                ],
                can_change_rules: vec![Reason::new(
                    "eip1967_admin",
                    "EIP-1967 admin slot contains an observed address.",
                )],
                token_paused: Some(false),
                sanctions_list: Some("0x0000000000000000000000000000000000000002".to_owned()),
                unavailable: Vec::new(),
                source_verified_subject: SourceVerifiedSubject::Contract,
                source_verified: SourceVerified::ExactMatch,
                source_verified_proxy: None,
                observed_at: "2026-10-05T00:00:00Z".to_owned(),
                block: Some(123),
                slot: None,
                reads: Vec::new(),
            }),
            source: GuardSource { status: SourceStatus::Verified, provider: "Sourcify".to_owned() },
            pools: Vec::new(),
            verdict: GuardVerdict::Allow,
            reasons: vec![GuardReason {
                code: "publisher_contract_match".to_owned(),
                detail: "The issuer registry publishes this token.".to_owned(),
            }],
            observed_at: "2026-10-05T00:00:00Z".to_owned(),
            reads: Vec::new(),
            reads_truncated: false,
            public_key: "test-public-key".to_owned(),
            signature: "test-signature".to_owned(),
            dev: true,
        }
    }

    #[test]
    fn unread_guard_pool_leads_with_retry_not_a_token_verdict() {
        let mut document = guard_document();
        document.subject_type = crate::domain::guard::GuardSubjectType::Pool;
        document.reasons = vec![crate::domain::guard::GuardReason {
            code: "pool_unavailable".to_owned(),
            detail: "QED could not read this pool — retry".to_owned(),
        }];
        assert_eq!(guard_identity_sentence(&document), "QED could not read this pool — retry");
    }

    #[test]
    fn guard_result_leads_with_plain_facts_and_hides_power_details() {
        let document = guard_document();
        let view =
            GuardResultTemplate::from_document(1, "https://qed.example".to_owned(), &document);
        assert_eq!(
            view.identity_sentence,
            "This is Backed xStocks's published NVDA contract on Ethereum."
        );
        assert_eq!(
            view.powers_sentence,
            "The issuer can block transfers, change token rules, and seize tokens."
        );
        assert_eq!(
            view.blocking_summary,
            "Can block transfers: yes — pausable (currently not paused), sanctions list."
        );
        assert_eq!(view.source_sentence, "Source verified.");
        let rendered = view.render().expect("Guard result template renders");
        assert!(
            rendered.contains(
                "Allow means completed checks found no contradiction or active restriction"
            )
        );
        assert!(rendered.contains("Deny means QED observed a contradiction or active restriction"));
        assert!(rendered.contains("Unknown means a check was incomplete or could not be verified"));
        assert!(rendered.contains("These are not safety ratings."));
        let mut unavailable_source = guard_document();
        unavailable_source.source.status = SourceStatus::Unavailable;
        let unavailable_source_view = GuardResultTemplate::from_document(
            1,
            "https://qed.example".to_owned(),
            &unavailable_source,
        );
        assert_eq!(
            unavailable_source_view.source_sentence,
            "QED could not check source verification."
        );
        let mut not_applicable_wallet = guard_document();
        not_applicable_wallet.wallet_check = Some(crate::domain::guard::GuardWalletCheck {
            status: crate::domain::guard::WalletCheckStatus::NotApplicable,
            restrictions: Vec::new(),
        });
        let not_applicable_wallet_view = GuardResultTemplate::from_document(
            1,
            "https://qed.example".to_owned(),
            &not_applicable_wallet,
        );
        assert_eq!(not_applicable_wallet_view.wallet_check_status, "not applicable");

        let html = view.render().expect("Guard result renders");
        assert!(!html.contains("<strong>Verdict:</strong> allow"));
        assert!(html.contains("<dt>Reviewed address</dt>"));
        let technical_details = html
            .find("<summary>Technical details</summary>")
            .expect("technical details disclosure");
        let raw_detail = html
            .find("pausable: pausable() is implemented; currently not paused.")
            .expect("raw power detail");
        assert!(raw_detail > technical_details);
        let signed_document = html
            .find("<summary>Signed document (JSON)</summary>")
            .expect("signed document disclosure");
        let reason_code =
            html.find("publisher_contract_match").expect("machine-readable reason code");
        assert!(reason_code > signed_document);

        let mut no_publisher = guard_document();
        no_publisher.identity.status = IdentityStatus::NoPublisher;
        no_publisher.identity.publisher = None;
        no_publisher.identity.matched_contract = None;
        no_publisher.identity.ticker = None;
        no_publisher.powers = Some(PowersRecord {
            can_seize: Vec::new(),
            can_block: Vec::new(),
            can_change_rules: Vec::new(),
            unavailable: Vec::new(),
            ..no_publisher.powers.take().expect("test powers record")
        });
        let no_publisher_view =
            GuardResultTemplate::from_document(1, "https://qed.example".to_owned(), &no_publisher);
        assert_eq!(
            no_publisher_view.identity_sentence,
            "QED knows no issuer that publishes this contract."
        );
        assert_eq!(no_publisher_view.powers_sentence, "QED observed no issuer controls.");

        let mut mismatch = guard_document();
        mismatch.identity.status = IdentityStatus::Mismatch;
        mismatch.identity.ticker = Some("NVDAx".to_owned());
        let mismatch_view =
            GuardResultTemplate::from_document(1, "https://qed.example".to_owned(), &mismatch);
        assert_eq!(
            mismatch_view.identity_sentence,
            "This token presents itself as NVDAx, but Backed xStocks publishes a different contract on Ethereum."
        );
    }
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
            explorer_link(crate::domain::chain::Chain::RobinhoodChain, "0xpool"),
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
    fn geckoterminal_leaderboard_row_keeps_its_market_page_and_source_label() {
        let pool = "0x0000000000000000000000000000000000000001";
        let trade_url = format!("https://www.geckoterminal.com/base/pools/{pool}");
        let row = LeaderboardRowView::from_value(&json!({
            "chain": "base",
            "pool": pool,
            "source": "geckoterminal",
            "trade_url": trade_url.clone(),
        }));
        assert_eq!(row.source_label, "GeckoTerminal");
        assert_eq!(row.trade_url, trade_url);
    }
    #[test]
    fn uniswap_pool_link_includes_chain_segment() {
        let links = trade_links(
            &PoolInfo {
                chain: crate::domain::chain::Chain::Ethereum,
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
    fn board_is_not_stale_before_its_scheduled_refresh() {
        let now = chrono::Utc::now();
        let updated_at = (now - chrono::Duration::hours(6)).to_rfc3339();
        let next_refresh_at = (now + chrono::Duration::hours(1)).to_rfc3339();
        let view = LeaderboardPageView::from_value(json!({
            "updated_at": updated_at,
            "next_refresh_at": next_refresh_at,
            "entries": [{ "pool": "stale-but-scheduled", "verdict": "verified" }],
            "refreshing": false
        }));
        assert!(view.status_line.starts_with("Updated "));
        assert!(!view.status_line.contains("stale"));
    }

    #[test]
    fn relative_hour_labels_do_not_add_an_invalid_plural_suffix() {
        assert_eq!(relative_text(6 * 60 * 60), "6 h");
    }
    #[test]
    fn leaderboard_rows_label_unread_checks_without_changing_the_verdict() {
        let view = LeaderboardPageView::from_value(json!({
            "entries": [{
                "pool": "pool-1",
                "chain": "base",
                "chain_label": "Base",
                "verdict": "unknown",
                "read_status": "not_read_yet",
                "read_reason": "transient"
            }],
            "refreshing": false,
            "empty_successful": false,
        }));
        assert_eq!(view.rows[0].verdict_label, "Not read yet");
        assert_eq!(view.rows[0].verdict, "unknown");
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
            source: crate::adapters::discovery::MarketSource::Dexscreener,
            issuer: Some("Issuer".to_owned()),
            ticker: Some("NVDA".to_owned()),
            issuer_on_base: None,
            verdict: "verified".to_owned(),
            read_status: "checked".to_owned(),
            read_reason: None,
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
