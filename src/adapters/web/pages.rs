#[cfg(test)]
use super::views::format_wallet_amount;
use super::views::{
    CertificateResultTemplate, CertificateTemplate, CertificateView, ChainTemplate,
    ContentPageTemplate, DirectoryContractView, DirectoryPoolView, DirectoryPowersView,
    DocsPageTemplate, FeaturedTemplate, GlossaryTemplate, GuardResultTemplate, GuardTemplate,
    GuideVerifyTemplate, IndexTemplate, IntegrationChipView, RegistryTableQuery,
    RegistryTableTemplate, RegistryTemplate, TokenTemplate, ValidatedCardView, ValidatedTemplate,
};
use super::{ASSET_VERSION, render_page, wants_fragment};
#[cfg(test)]
use crate::adapters::registry as registry_adapter;
#[cfg(test)]
use crate::app::wallet::wallet_chains;
use crate::{
    adapters::{content as content_adapter, state::AppState},
    app::attestation as attest,
    domain::{
        chain::Chain,
        registry::{self, Entry},
    },
};
use askama::Template;
use axum::{
    extract::{Form, Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{Html, IntoResponse, Redirect, Response},
};
use serde::Deserialize;
use serde_json::json;
use std::time::Duration;
const POWERS_PAGE_DEADLINE: Duration = Duration::from_millis(2500);
const BLOG_PAGE_SIZE: usize = 6;
#[derive(Clone, Copy)]
pub(crate) struct PageMetadata<'a> {
    pub(crate) page_title: &'a str,
    pub(crate) heading: &'a str,
    pub(crate) eyebrow: &'a str,
    pub(crate) intro: &'a str,
    pub(crate) meta_description: &'a str,
    pub(crate) canonical_path: &'a str,
}
pub(crate) async fn index(
    State(state): State<AppState>,
    _headers: HeaderMap,
) -> Result<Html<String>, StatusCode> {
    let leaderboard = state.leaderboard.read().await.clone();
    let mut value =
        serde_json::to_value(&leaderboard).unwrap_or_else(|_| serde_json::json!({ "entries": [] }));
    super::asset::decorate_entries(&state.app, &mut value);
    let registry = state.registry.read().await.clone();
    let mut sourcify_verified = 0;
    for entry in
        registry.iter().filter(|entry| entry.removed_at.is_none() && entry.stale_since.is_none())
    {
        let key = (entry.chain, entry.contract.clone());
        if state.app.powers_cache.get(&key).await.is_some_and(|record| {
            matches!(
                record.source_verified,
                crate::domain::powers::SourceVerified::ExactMatch
                    | crate::domain::powers::SourceVerified::Match
            )
        }) {
            sourcify_verified += 1;
        }
    }
    let stats = super::api::leaderboard_stats(&leaderboard, &registry);
    let source_count = |source: crate::adapters::discovery::MarketSource| {
        leaderboard
            .entries
            .iter()
            .filter(|entry| entry.source == source && entry.price_usd.is_some_and(f64::is_finite))
            .count()
    };
    let chain_count = |chain: Chain| {
        let label = crate::adapters::discovery::chain_label(chain);
        stats
            .by_chain
            .iter()
            .find(|entry| entry.chain_label == label)
            .map(|entry| entry.counts.pools_checked)
            .unwrap_or_default()
    };
    let issuer_count = |issuer: &str| {
        stats
            .registry
            .by_issuer
            .iter()
            .find(|entry| entry.issuer == issuer)
            .map(|entry| entry.entries)
            .unwrap_or_default()
    };
    let uniswap_count = leaderboard
        .entries
        .iter()
        .filter(|entry| entry.dex.to_ascii_lowercase().contains("uniswap"))
        .count();
    let positive_count = |count: usize| (count > 0).then(|| count.to_string());
    let integrations = vec![
        IntegrationChipView {
            category: "Chain",
            name: "Solana",
            href: "https://solana.com/",
            mark: "",
            icon: "solana",
            logo_src: "",
            number: positive_count(chain_count(Chain::Solana)),
            unit: "pools checked",
            empty_unit: "supported chain",
        },
        IntegrationChipView {
            category: "Chain",
            name: "Robinhood Chain",
            href: "https://docs.robinhood.com/chain/connecting/",
            mark: "",
            icon: "robinhood",
            logo_src: "",
            number: positive_count(chain_count(Chain::RobinhoodChain)),
            unit: "pools checked",
            empty_unit: "supported chain",
        },
        IntegrationChipView {
            category: "Chain",
            name: "Ethereum",
            href: "https://ethereum.org/",
            mark: "",
            icon: "ethereum",
            logo_src: "",
            number: positive_count(chain_count(Chain::Ethereum)),
            unit: "pools checked",
            empty_unit: "supported chain",
        },
        IntegrationChipView {
            category: "Chain",
            name: "Base",
            href: "https://base.org/",
            mark: "",
            icon: "base",
            logo_src: "",
            number: positive_count(chain_count(Chain::Base)),
            unit: "pools checked",
            empty_unit: "supported chain",
        },
        IntegrationChipView {
            category: "Chain",
            name: "BNB Chain",
            href: "https://www.bnbchain.org/",
            mark: "",
            icon: "bnb",
            logo_src: "",
            number: positive_count(chain_count(Chain::Bnb)),
            unit: "pools checked",
            empty_unit: "supported chain",
        },
        IntegrationChipView {
            category: "Issuer",
            name: "Backed xStocks",
            href: "https://xstocks.fi/",
            mark: "",
            icon: "xstocks",
            logo_src: "/static/brand/xstocks-mark.svg",
            number: positive_count(issuer_count("Backed xStocks")),
            unit: "contracts tracked",
            empty_unit: "issuer catalog",
        },
        IntegrationChipView {
            category: "Issuer",
            name: "Robinhood",
            href: "https://robinhood.com/us/en/",
            mark: "",
            icon: "robinhood-logo",
            logo_src: "/static/brand/robinhood-mark.ico",
            number: positive_count(issuer_count("Robinhood")),
            unit: "contracts tracked",
            empty_unit: "issuer catalog",
        },
        IntegrationChipView {
            category: "Issuer",
            name: "Ondo",
            href: "https://ondo.finance/",
            mark: "",
            icon: "ondo",
            logo_src: "/static/brand/ondo-mark.svg",
            number: positive_count(issuer_count("Ondo")),
            unit: "contracts tracked",
            empty_unit: "issuer catalog",
        },
        IntegrationChipView {
            category: "Source",
            name: "DexScreener",
            href: "https://dexscreener.com/",
            mark: "",
            icon: "dexscreener",
            logo_src: "/static/brand/dexscreener-mark.ico",
            number: positive_count(source_count(
                crate::adapters::discovery::MarketSource::Dexscreener,
            )),
            unit: "pools priced",
            empty_unit: "price source",
        },
        IntegrationChipView {
            category: "Source",
            name: "GeckoTerminal",
            href: "https://www.geckoterminal.com/",
            mark: "",
            icon: "geckoterminal",
            logo_src: "/static/brand/geckoterminal-mark.ico",
            number: positive_count(source_count(
                crate::adapters::discovery::MarketSource::Geckoterminal,
            )),
            unit: "pools priced",
            empty_unit: "price source",
        },
        IntegrationChipView {
            category: "Verifier",
            name: "Sourcify",
            href: "https://sourcify.dev/",
            mark: "",
            icon: "sourcify",
            logo_src: "/static/brand/sourcify-mark.png",
            number: positive_count(sourcify_verified),
            unit: "contracts verified",
            empty_unit: "source verification",
        },
        IntegrationChipView {
            category: "Venue",
            name: "Uniswap",
            href: "https://uniswap.org/",
            mark: "",
            icon: "uniswap",
            logo_src: "/static/brand/uniswap-mark.ico",
            number: positive_count(uniswap_count),
            unit: "pools listed",
            empty_unit: "trading venue",
        },
    ];
    if let Some(object) = value.as_object_mut() {
        object.remove("impostors");
        object.insert(
            "prices_updated_at".to_owned(),
            serde_json::Value::String(state.prices.read().await.updated_at.clone()),
        );
    }
    render_page(
        IndexTemplate {
            asset_version: ASSET_VERSION,
            public_url: state.public_url.to_string(),
            leaderboard: super::views::LeaderboardPageView::from_value(value),
            integration_chips: integrations,
        },
        false,
    )
}
pub(crate) async fn stats_page(State(state): State<AppState>) -> Result<Response, StatusCode> {
    if state.stats_snapshot.read().await.is_none() {
        super::api::refresh_stats_snapshot(&state)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
    let snapshot =
        state.stats_snapshot.read().await.clone().ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
    let summary = super::asset::stats_summary(&state.app);
    let html = String::from_utf8_lossy(&snapshot.html).replace(
        "<section class=\"stats-overview\"",
        &format!("{summary}<section class=\"stats-overview\""),
    );
    let mut response = Response::new(axum::body::Body::from(html));
    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("text/html; charset=utf-8"),
    );
    Ok(response)
}
#[derive(Debug, Deserialize, Default)]
pub(crate) struct CopiesQuery {
    chain: Option<String>,
}

pub(crate) async fn copies_page(
    State(state): State<AppState>,
    Query(query): Query<CopiesQuery>,
) -> Result<Html<String>, StatusCode> {
    let registry = state.registry.read().await.clone();
    let board = state.leaderboard.read().await;
    let entries = super::api::recent_catalog_entries(
        &board.impostors.entries,
        chrono::Utc::now().timestamp(),
    );
    render_copies_page(&state, &entries, &registry, query.chain.as_deref())
}

fn render_copies_page(
    state: &AppState,
    entries: &[&crate::adapters::discovery::ImpostorEntry],
    registry: &[Entry],
    requested_chain: Option<&str>,
) -> Result<Html<String>, StatusCode> {
    use crate::domain::guard::PublisherProductTier as Tier;

    let mut confirmed =
        entries.iter().copied().filter(|entry| entry.tier == Tier::Confirmed).collect::<Vec<_>>();
    confirmed.sort_by(|left, right| compare_copy_volume(left.volume_24h_usd, right.volume_24h_usd));
    let mut caution =
        entries.iter().copied().filter(|entry| entry.tier == Tier::Caution).collect::<Vec<_>>();
    caution.sort_by(|left, right| compare_copy_volume(left.volume_24h_usd, right.volume_24h_usd));

    let mut chains = std::collections::BTreeMap::<String, (String, usize)>::new();
    for entry in &confirmed {
        let item = chains.entry(entry.chain.clone()).or_insert((entry.chain_label.clone(), 0));
        item.1 += 1;
    }
    let selected_chain =
        requested_chain.and_then(crate::domain::chain::Chain::from_network_name).filter(|_| {
            requested_chain
                .is_some_and(|slug| chains.keys().any(|known| known.eq_ignore_ascii_case(slug)))
        });
    let filtered_confirmed = confirmed
        .iter()
        .copied()
        .filter(|entry| {
            selected_chain.is_none_or(|chain| {
                crate::domain::chain::Chain::from_network_name(&entry.chain) == Some(chain)
            })
        })
        .collect::<Vec<_>>();
    let filtered_caution = caution
        .iter()
        .copied()
        .filter(|entry| {
            selected_chain.is_none_or(|chain| {
                crate::domain::chain::Chain::from_network_name(&entry.chain) == Some(chain)
            })
        })
        .collect::<Vec<_>>();
    let tabs = std::iter::once(format!(
        "<a href=\"/copies\"{}>All <span>{}</span></a>",
        if selected_chain.is_none() { " aria-current=\"page\"" } else { "" },
        confirmed.len()
    ))
    .chain(chains.iter().map(|(slug, (label, count))| {
        let active = selected_chain.is_some_and(|selected| {
            crate::domain::chain::Chain::from_network_name(slug) == Some(selected)
        });
        format!(
            "<a href=\"/copies?chain={}\"{}>{} <span>{}</span></a>",
            html_escape(slug),
            if active { " aria-current=\"page\"" } else { "" },
            html_escape(label),
            count
        )
    }))
    .collect::<String>();
    let hero = filtered_confirmed.first().map_or_else(
        || {
            "<section class=\"copies-hero\"><p>No confirmed copies currently shown.</p></section>"
                .to_owned()
        },
        |entry| render_copy_hero(entry, registry),
    );
    let table = render_copies_table(&filtered_confirmed, false);
    let caution_table = render_copies_table(&filtered_caution, true);
    let total = reported_volume(&filtered_confirmed);
    let summary = format!(
        "{} confirmed copies · {} reported 24h volume",
        filtered_confirmed.len(),
        format_reported_usd(total),
    );
    let table_section = if filtered_confirmed.is_empty() {
        "<p class=\"copies-empty\">No copies currently flagged. See <a href=\"/stats\">leaderboard statistics</a>.</p>".to_owned()
    } else {
        format!("<p class=\"copies-summary\">{summary}</p>{table}")
    };
    let body_html = format!(
        r#"<section class="copies-page">
{hero}
<nav class="copies-tabs" aria-label="Filter copies by chain">{tabs}</nav>
{table_section}
<details class="copies-near-matches"><summary>Near matches (caution)</summary>{caution_table}</details>
<p class="copies-note">Reported volume comes from DexScreener/GeckoTerminal, not verified. First seen = QED watch time. See <a href="/stats">stats</a> and the <a href="/guide/verify-a-stock-token">verification guide</a>.</p>
</section>"#
    );
    content_page(
        state,
        PageMetadata {
            page_title: "Copied stock tokens",
            heading: "Copied stock tokens",
            eyebrow: "Publisher watch",
            intro: "Tokens whose on-chain name and symbol exactly match an issuer's product, at addresses the issuer never published. Found by QED's daily publisher watch.",
            meta_description: "Copies found by QED's daily publisher watch, with chain, reported volume, first-seen time, and Guard verdict.",
            canonical_path: "/copies",
        },
        body_html,
    )
}

fn compare_copy_volume(left: Option<f64>, right: Option<f64>) -> std::cmp::Ordering {
    let left = left.filter(|value| value.is_finite() && *value >= 0.0);
    let right = right.filter(|value| value.is_finite() && *value >= 0.0);
    match (left, right) {
        (Some(left), Some(right)) => right.total_cmp(&left),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

fn render_copy_hero(
    entry: &crate::adapters::discovery::ImpostorEntry,
    registry: &[Entry],
) -> String {
    let chain = crate::domain::chain::Chain::from_network_name(&entry.chain);
    let official = chain.and_then(|chain| {
        registry
            .iter()
            .filter(|candidate| {
                registry::matchable(candidate)
                    && candidate.issuer.eq_ignore_ascii_case(&entry.publisher)
                    && candidate.ticker.eq_ignore_ascii_case(&entry.ticker)
            })
            .flat_map(|candidate| &candidate.official_deployments)
            .find(|deployment| {
                crate::domain::chain::Chain::from_network_name(&deployment.network) == Some(chain)
            })
            .map(|deployment| deployment.address.as_str())
    });
    let symbol = entry.on_chain_symbol.as_deref().unwrap_or(&entry.symbol);
    let name = entry.on_chain_name.as_deref().unwrap_or(&entry.name);
    let address = html_escape(&entry.address);
    let guard_url = html_escape(&entry.guard_url);
    let comparison = match official {
        Some(official) => address_comparison_html(
            &entry.address,
            official,
            &format!("Real · published by {}", entry.publisher),
            "Copy · not in the issuer's catalog",
            chain == Some(crate::domain::chain::Chain::Solana),
        ),
        None => format!(
            "<div class=\"addr-compare\"><div class=\"addr-row addr-row--copy\"><span class=\"addr-label\">Copy · not in the issuer's catalog</span><code>{address}</code></div></div>"
        ),
    };
    format!(
        "<section class=\"copies-hero\" aria-labelledby=\"copies-hero-title\"><p class=\"eyebrow\">Largest confirmed copy by reported 24h volume</p><h2 id=\"copies-hero-title\">{} <span>({})</span></h2><p>Product published by {} · {} · {}</p>{}<p class=\"copies-hero-volume\">{}<small>reported 24h volume</small></p><a class=\"deny-badge\" href=\"{}\" rel=\"nofollow\">Deny · View Guard</a> {}</section>",
        html_escape(name),
        html_escape(symbol),
        html_escape(&entry.publisher),
        html_escape(&entry.ticker),
        html_escape(&entry.chain_label),
        comparison,
        entry
            .volume_24h_usd
            .filter(|value| value.is_finite() && *value >= 0.0)
            .map_or("—".to_owned(), format_card_reported_usd),
        guard_url,
        flagged_copy_button(),
    )
}

/// Two addresses stacked in the same type size: the issuer-published one first, then the
/// reviewed one, with every character of the reviewed address that differs marked.
/// EVM addresses compare case-insensitively; Solana base58 addresses compare exactly.
pub(crate) fn address_comparison_html(
    observed: &str,
    official: &str,
    official_label: &str,
    observed_label: &str,
    case_sensitive: bool,
) -> String {
    let same = |left: char, right: char| {
        if case_sensitive { left == right } else { left.eq_ignore_ascii_case(&right) }
    };
    let official_chars = official.chars().collect::<Vec<_>>();
    let mut marked = String::with_capacity(observed.len() * 2);
    let mut in_mark = false;
    for (index, character) in observed.chars().enumerate() {
        let differs = official_chars.get(index).is_none_or(|expected| !same(*expected, character));
        if differs && !in_mark {
            marked.push_str("<mark>");
            in_mark = true;
        } else if !differs && in_mark {
            marked.push_str("</mark>");
            in_mark = false;
        }
        marked.push_str(&html_escape(&character.to_string()));
    }
    if in_mark {
        marked.push_str("</mark>");
    }
    let matches = observed.chars().count() == official_chars.len()
        && observed.chars().zip(official_chars.iter()).all(|(left, right)| same(*right, left));
    format!(
        "<div class=\"addr-compare{}\"><div class=\"addr-row addr-row--real\"><span class=\"addr-label\">{}</span><code>{}</code></div><div class=\"addr-row addr-row--copy\"><span class=\"addr-label\">{}</span><code>{}</code></div></div>",
        if matches { " addr-compare--match" } else { "" },
        html_escape(official_label),
        html_escape(official),
        html_escape(observed_label),
        marked,
    )
}

/// Copy control for addresses QED flags: visibly disabled, never copies, and says why.
fn flagged_copy_button() -> &'static str {
    "<span class=\"copy-disabled\" title=\"Caution: copy disabled for a flagged address\"><button class=\"copy-button\" type=\"button\" aria-disabled=\"true\" aria-label=\"Caution: copy disabled for a flagged address\" tabindex=\"-1\"><svg aria-hidden=\"true\" focusable=\"false\"><use href=\"/static/icons.svg#copy\"></use></svg></button></span>"
}

fn render_copies_table(
    entries: &[&crate::adapters::discovery::ImpostorEntry],
    caution: bool,
) -> String {
    let rows = entries.iter().enumerate().map(|(index, entry)| {
        let name = html_escape(entry.on_chain_name.as_deref().unwrap_or(&entry.name));
        let symbol = html_escape(entry.on_chain_symbol.as_deref().unwrap_or(&entry.symbol));
        let guard_url = html_escape(&entry.guard_url);
        let explorer_url = crate::domain::chain::Chain::from_network_name(&entry.chain)
            .map(|chain| super::views::explorer_link(chain, &entry.address))
            .unwrap_or_default();
        let address_link = if explorer_url.is_empty() {
            format!("<code>{}</code>", html_escape(&short_address(&entry.address)))
        } else {
            format!("<a href=\"{}\" target=\"_blank\" rel=\"noopener noreferrer\"><code>{}</code> ↗</a>", html_escape(&explorer_url), html_escape(&short_address(&entry.address)))
        };
        let volume = entry.volume_24h_usd.filter(|value| value.is_finite() && *value >= 0.0)
            .map_or("—".to_owned(), format_reported_usd);
        let verdict = if caution {
            "<span class=\"caution-badge\">Unknown / Caution</span>".to_owned()
        } else {
            format!("<a class=\"deny-badge\" href=\"{guard_url}\" rel=\"nofollow\">Deny</a>")
        };
        format!(
            "<tr><td data-label=\"#\">{}</td><td data-label=\"Copies\"><strong>{} ({})</strong><small>{}</small></td><td data-label=\"Chain\">{}</td><td data-label=\"Address\"><span class=\"copies-address\">{} {}</span></td><td data-label=\"Reported 24h volume\">{}</td><td data-label=\"First seen\">{}</td><td data-label=\"Verdict\">{}</td></tr>",
            index + 1, name, symbol, html_escape(&entry.publisher), html_escape(&entry.chain_label),
            address_link, flagged_copy_button(), volume, human_utc_time(&entry.first_seen_at), verdict
        )
    }).collect::<String>();
    format!(
        "<div class=\"table-wrap copies-table-wrap\"><table class=\"stack-table copies-table\"><thead><tr><th scope=\"col\">#</th><th scope=\"col\">Copies</th><th scope=\"col\">Chain</th><th scope=\"col\">Address</th><th scope=\"col\">Reported 24h volume</th><th scope=\"col\">First seen</th><th scope=\"col\">Verdict</th></tr></thead><tbody>{rows}</tbody></table></div>"
    )
}
pub(crate) fn render_stats_page(
    state: &AppState,
    stats: &super::api::LeaderboardStats,
    all_catalog_entries: &[&crate::adapters::discovery::ImpostorEntry],
    board: &crate::adapters::discovery::Leaderboard,
    registry: &[crate::domain::registry::Entry],
) -> Result<bytes::Bytes, StatusCode> {
    let chain_rows = stats
        .by_chain
        .iter()
        .map(|chain| {
            let unread = &chain.counts.not_read_yet;
            format!(
                "<li><strong>{}</strong>: {} checked, {} issuer matches, {} mismatches, {} unsupported venues; {} not read yet ({} RPC limit, {} transient, {} unsupported).</li>",
                html_escape(&chain.chain_label),
                chain.counts.pools_checked,
                chain.counts.issuer_matches,
                chain.counts.mismatches,
                chain.counts.unsupported_venue,
                unread.count,
                unread.rpc_limit,
                unread.transient,
                unread.unsupported
            )
        })
        .collect::<String>();
    let issuer_rows = stats
        .registry
        .by_issuer
        .iter()
        .map(|issuer| {
            let chains = if issuer.chains.is_empty() {
                "none".to_owned()
            } else {
                issuer.chains.iter().map(|chain| html_escape(chain)).collect::<Vec<_>>().join(", ")
            };
            format!(
                "<li><strong>{}</strong>: {} active entries across {chains}.</li>",
                html_escape(&issuer.issuer),
                issuer.entries
            )
        })
        .collect::<String>();
    let watch = &stats.publisher_catalog_watch;
    let recent_entries = all_catalog_entries;
    let confirmed_entries = recent_entries
        .iter()
        .copied()
        .filter(|entry| entry.tier == crate::domain::guard::PublisherProductTier::Confirmed)
        .collect::<Vec<_>>();
    let caution_entries = recent_entries
        .iter()
        .copied()
        .filter(|entry| entry.tier == crate::domain::guard::PublisherProductTier::Caution)
        .collect::<Vec<_>>();
    let confirmed_volume = reported_volume(&confirmed_entries);
    let caution_volume = reported_volume(&caution_entries);
    let total_volume_line = format!(
        "<p class=\"stats-flagged-total\">Reported 24h volume across all flagged tokens: {} (DexScreener/GeckoTerminal, not verified)</p>",
        format_reported_usd(reported_volume(recent_entries)),
    );
    let copies_link = "<p><a href=\"/copies\">See all copies by chain → /copies</a></p>";
    let confirmed_table = render_flagged_table(
        &confirmed_entries,
        crate::domain::guard::PublisherProductTier::Confirmed,
        registry,
        &board.entries,
    );
    let caution_table = render_flagged_table(
        &caution_entries,
        crate::domain::guard::PublisherProductTier::Caution,
        registry,
        &board.entries,
    );
    let last_scanned_at = if watch.last_scanned_at.is_empty() {
        "Not yet scanned".to_owned()
    } else {
        super::timestamp_html(&watch.last_scanned_at)
    };
    let source_note = watch
        .source_unavailable_since
        .as_deref()
        .map(|since| {
            format!(
                "<p>The impostor search source has been unavailable since {}; the observations below are from the last successful search.</p>",
                super::timestamp_html(since)
            )
        })
        .unwrap_or_default();
    let generated_at = super::timestamp_html(&stats.generated_at);
    let method = html_escape(&watch.method);
    let confirmed_card = format!(
        "<article class=\"stats-number-card stats-number-card--deny\"><p class=\"stats-number\"><span class=\"deny-badge\">Deny</span><span class=\"deny-icon\" role=\"img\" aria-label=\"Deny — claims a product its publisher does not list\"><svg viewBox=\"0 0 24 24\" aria-hidden=\"true\" focusable=\"false\"><path d=\"M12 2.5 21.5 21H2.5L12 2.5Z\"/><path d=\"M12 8v6m0 3h.01\"/></svg></span> {}</p><p>Confirmed mismatches</p><p class=\"stats-number-context\">{} reported 24h volume</p></article>",
        confirmed_entries.len(),
        format_card_reported_usd(confirmed_volume),
    );
    let caution_card = format!(
        "<article class=\"stats-number-card stats-number-card--caution\"><p class=\"stats-number\"><span class=\"caution-badge\">Caution</span> {}</p><p>Near matches</p><p class=\"stats-number-context\">{} reported 24h volume</p></article>",
        caution_entries.len(),
        format_card_reported_usd(caution_volume),
    );
    let unread_card = format!(
        "<article class=\"stats-number-card\"><p class=\"stats-number\">{}</p><p>Pools not checked: {} not read yet · {} unsupported venues</p></article>",
        stats.not_read_yet.count + stats.unsupported_venue,
        stats.not_read_yet.count,
        stats.unsupported_venue
    );
    let body_html = format!(
        r#"<section class="stats-overview" aria-label="Leaderboard statistics at a glance">
  <article class="stats-number-card"><p class="stats-number">{} of {}</p><p>Pools matching issuer-published contract</p></article>
  {}
  {}
  {}
</section>
<section class="stats-evidence" aria-labelledby="stats-evidence-title">
  <h2 id="stats-evidence-title">Currently flagged tokens</h2>
  {total_volume_line}
  {copies_link}
  <h3>Confirmed mismatches</h3>
  <p class="stats-evidence-note">On-chain symbol and name exactly match an issuer product; the address is absent from the issuer's catalog.</p>
  {confirmed_table}
  <h3>Caution — near matches</h3>
  <p class="stats-evidence-note">Similar to an issuer product, but QED could not confirm a mismatch.</p>
  {caution_table}
</section>
<details class="stats-details">
  <summary>Method, counts and downloads</summary>
  <h3>Publisher deployment catalog watch</h3>
  <p>Generated at {}. <a href="/stats.json">Download signed JSON</a> · <a href="/stats.csv">Download signed CSV</a></p>
  <p>Last searched at {}. {} catalog observations are currently flagged (seen in the last 7 days); {} were first seen this UTC week. {} candidates on unsupported chains were seen but not judged; {} official deployments on unsupported chains were counted separately and excluded from candidates. {} supported and {} unsupported candidates were evicted; {} invalid supported and {} invalid unsupported candidates were rejected.</p>
  {}<p>Method: {}</p>
  <h3>Leaderboard counts by chain</h3><ul>{}</ul>
  <h3>Active registry coverage</h3>
  <p>{} active registry entries across the tracked issuers.</p><ul>{}</ul>
  <p>Other counts: {} listed pools, {} pools checked, {} issuer matches, {} mismatches, {} unsupported venues, and {} not read yet ({} RPC limit, {} transient, {} unsupported).</p>
  <p>Market data: <a href="https://dexscreener.com" target="_blank" rel="noopener noreferrer">DexScreener</a> / <a href="https://www.geckoterminal.com" target="_blank" rel="noopener noreferrer">GeckoTerminal</a> · <a href="https://www.coingecko.com/en/api_terms" target="_blank" rel="noopener noreferrer">Powered by CoinGecko</a>.</p>
  <p>These are point-in-time pool reads. A contract match does not prove backing, custody, reserves, solvency, safety, price, liquidity, or endorsement.</p>
</details>"#,
        stats.issuer_matches,
        stats.listed_pools,
        confirmed_card,
        caution_card,
        unread_card,
        generated_at,
        last_scanned_at,
        watch.currently_flagged_last_7_days,
        watch.first_flagged_this_week,
        watch.unsupported_chain_candidates_seen,
        watch.official_on_unsupported_chain,
        watch.evicted_entries,
        watch.evicted_unsupported_candidates,
        watch.rejected_oversize_entries,
        watch.rejected_oversize_unsupported_candidates,
        source_note,
        method,
        chain_rows,
        stats.registry.active_entries,
        issuer_rows,
        stats.listed_pools,
        stats.pools_checked,
        stats.issuer_matches,
        stats.mismatches,
        stats.unsupported_venue,
        stats.not_read_yet.count,
        stats.not_read_yet.rpc_limit,
        stats.not_read_yet.transient,
        stats.not_read_yet.unsupported,
    );
    docs_content_page(
        state,
        PageMetadata {
            page_title: "Leaderboard statistics",
            heading: "Leaderboard statistics",
            eyebrow: "REFERENCE",
            intro: "Pool checks, flagged catalog observations, and work not read yet.",
            meta_description: "Point-in-time counts from QED's current leaderboard checks and active issuer registry.",
            canonical_path: "/stats",
        },
        "stats",
        body_html,
    )
    .map(|Html(body)| bytes::Bytes::from(body))
}

fn reported_volume(entries: &[&crate::adapters::discovery::ImpostorEntry]) -> f64 {
    entries
        .iter()
        .filter_map(|entry| entry.volume_24h_usd.filter(|value| value.is_finite() && *value >= 0.0))
        .sum()
}

fn render_flagged_table(
    entries: &[&crate::adapters::discovery::ImpostorEntry],
    tier: crate::domain::guard::PublisherProductTier,
    registry: &[crate::domain::registry::Entry],
    leaderboard: &[crate::adapters::discovery::LeaderboardEntry],
) -> String {
    let visible = entries.iter().take(super::api::IMPOSTOR_STATS_TABLE_LIMIT);
    let rows =
        visible.map(|entry| render_flagged_row(entry, registry, leaderboard)).collect::<String>();
    let rows = if rows.is_empty() {
        let label = match tier {
            crate::domain::guard::PublisherProductTier::Confirmed => "confirmed mismatches",
            crate::domain::guard::PublisherProductTier::Caution => "caution near matches",
        };
        format!("<tr><td colspan=\"5\">No {label} are currently shown.</td></tr>")
    } else {
        rows
    };
    let showing = if entries.len() > super::api::IMPOSTOR_STATS_TABLE_LIMIT {
        format!(" Showing {} of {}.", super::api::IMPOSTOR_STATS_TABLE_LIMIT, entries.len())
    } else {
        String::new()
    };
    format!(
        "<div class=\"table-wrap\"><table class=\"stats-table\"><thead><tr><th scope=\"col\">Chain</th><th scope=\"col\">Token name and symbol</th><th scope=\"col\">Address</th><th scope=\"col\">Reported 24h volume</th><th scope=\"col\">First seen</th></tr></thead><tbody>{rows}</tbody><tfoot><tr><td colspan=\"5\">Total reported 24h volume ({} tokens): {}{} (DexScreener/GeckoTerminal, not verified)</td></tr></tfoot></table></div>",
        entries.len(),
        format_reported_usd(reported_volume(entries)),
        showing,
    )
}

fn display_catalog_reason(reason: &str) -> String {
    if reason.len() >= crate::adapters::discovery::MAX_IMPOSTOR_REASON_BYTES
        && !reason.ends_with('…')
    {
        format!("{reason}…")
    } else {
        reason.to_owned()
    }
}

fn render_flagged_row(
    entry: &crate::adapters::discovery::ImpostorEntry,
    registry: &[crate::domain::registry::Entry],
    leaderboard: &[crate::adapters::discovery::LeaderboardEntry],
) -> String {
    let source = match entry.source {
        crate::adapters::discovery::MarketSource::Dexscreener => "DexScreener",
        crate::adapters::discovery::MarketSource::Geckoterminal => "GeckoTerminal",
    };
    let ticker_registry = registry.iter().filter(|candidate| {
        crate::domain::registry::matchable(candidate)
            && candidate.issuer.eq_ignore_ascii_case(&entry.publisher)
            && candidate.ticker.eq_ignore_ascii_case(&entry.ticker)
    });
    let official_networks = ticker_registry
        .flat_map(crate::domain::registry::official_networks)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let reason = match (entry.on_chain_symbol.as_deref(), entry.on_chain_name.as_deref()) {
        (Some(symbol), Some(name))
            if !symbol.is_empty()
                && !name.is_empty()
                && !entry.ticker.is_empty()
                && !entry.publisher.is_empty()
                && !official_networks.is_empty() =>
        {
            let identity = format!(
                "on-chain symbol “{}” and name “{}”",
                html_escape(symbol),
                html_escape(name),
            );
            match entry.tier {
                crate::domain::guard::PublisherProductTier::Confirmed => format!(
                    "The {identity} exactly match the {} product published by {}. The publisher catalog lists official deployments on {}; this address is absent from those deployments.",
                    html_escape(&entry.ticker),
                    html_escape(&entry.publisher),
                    html_escape(&official_networks.join(", ")),
                ),
                crate::domain::guard::PublisherProductTier::Caution => format!(
                    "The {identity} resemble a product listed by {} for {}. QED could not confirm a mismatch because the match is not byte-exact or the publisher catalog snapshot is incomplete.",
                    html_escape(&entry.publisher),
                    html_escape(&entry.ticker),
                ),
            }
        }
        _ => html_escape(&display_catalog_reason(&entry.reason)),
    };
    let (row_class, status_mark) = match entry.tier {
        crate::domain::guard::PublisherProductTier::Confirmed => (
            "stats-token-row--deny",
            "<span class=\"deny-icon\" role=\"img\" aria-label=\"Deny — claims a product its publisher does not list\"><svg viewBox=\"0 0 24 24\" aria-hidden=\"true\" focusable=\"false\"><path d=\"M12 2.5 21.5 21H2.5L12 2.5Z\"/><path d=\"M12 8v6m0 3h.01\"/></svg></span>",
        ),
        crate::domain::guard::PublisherProductTier::Caution => {
            ("stats-token-row--caution", "<span class=\"caution-badge\">Caution</span>")
        }
    };
    let listed_name = html_escape(&entry.name);
    let listed_symbol = html_escape(&entry.symbol);
    let address = html_escape(&entry.address);
    let short = html_escape(&short_address(&entry.address));
    let volume = entry.volume_24h_usd.filter(|value| value.is_finite() && *value >= 0.0);
    let volume_html = volume.map_or_else(
        || format!("—<small>reported by {source}</small>"),
        |value| format!("{}<small>reported by {source}</small>", format_reported_usd(value)),
    );
    let guard_url = html_escape(&entry.guard_url);
    let explorer_url = crate::domain::chain::Chain::from_network_name(&entry.chain)
        .map(|chain| super::views::explorer_link(chain, &entry.address))
        .unwrap_or_default();
    let explorer_html = if explorer_url.is_empty() {
        format!("<code>{address}</code>")
    } else {
        format!(
            "<a href=\"{}\" target=\"_blank\" rel=\"noopener noreferrer\"><code>{address}</code><span aria-hidden=\"true\"> ↗</span></a>",
            html_escape(&explorer_url)
        )
    };
    let first_seen = human_utc_time(&entry.first_seen_at);
    let liquidity_html =
        entry.liquidity_24h_usd.filter(|value| value.is_finite() && *value >= 0.0).map_or_else(
            || "Not retained in this observation".to_owned(),
            |value| format!("{} reported by {source}", format_reported_usd(value)),
        );
    let pool_count_html = entry.pool_count.map_or_else(
        || "Not retained in this observation".to_owned(),
        |count| format!("{count} reported in this observation"),
    );
    let last_seen = human_utc_time(&entry.last_seen_at);

    let volume_value = volume.map_or("—".to_owned(), format_reported_usd);
    let diagram = flagged_volume_diagram(volume, entry, leaderboard);
    format!(
        "<tr class=\"stats-token-row {row_class}\"><td>{status_mark} {}</td><td><strong>Listed as {}</strong><small>{} · reported by {}</small></td><td class=\"stats-address\"><a href=\"{}\" target=\"_blank\" rel=\"noopener noreferrer\"><code title=\"Full address: {address}\">{short}</code><span aria-hidden=\"true\"> ↗</span></a></td><td class=\"stats-market-value\">{volume_html}</td><td>{}</td></tr><tr class=\"stats-expansion-row\"><td colspan=\"5\"><details class=\"stats-row-details\"><summary>Details</summary><div class=\"stats-row-detail-content\"><p><strong>Why flagged:</strong> {reason}</p><p><strong>Full address:</strong> {explorer_html}</p><p><strong>First seen:</strong> {} · <strong>Last seen:</strong> {}</p><p><strong>Reported 24h volume:</strong> {} · reported by {}</p><p><strong>Liquidity:</strong> {} · <strong>Pool count:</strong> {}</p>{diagram}<p><a href=\"{}\">Open Guard</a></p></div></details></td></tr>",
        html_escape(&entry.chain_label),
        listed_name,
        listed_symbol,
        source,
        html_escape(&explorer_url),
        first_seen,
        first_seen,
        last_seen,
        volume_value,
        source,
        liquidity_html,
        pool_count_html,
        guard_url,
    )
}

fn flagged_volume_diagram(
    token_volume: Option<f64>,
    entry: &crate::adapters::discovery::ImpostorEntry,
    leaderboard: &[crate::adapters::discovery::LeaderboardEntry],
) -> String {
    let official_volumes = leaderboard
        .iter()
        .filter(|pool| {
            pool.verdict == "verified"
                && pool
                    .ticker
                    .as_deref()
                    .is_some_and(|ticker| ticker.eq_ignore_ascii_case(&entry.ticker))
                && pool
                    .issuer
                    .as_deref()
                    .is_some_and(|issuer| issuer.eq_ignore_ascii_case(&entry.publisher))
        })
        .filter_map(|pool| pool.volume_24h_usd)
        .filter(|volume| volume.is_finite() && *volume >= 0.0)
        .collect::<Vec<_>>();
    let token_value = token_volume.map_or("—".to_owned(), format_reported_usd);
    let token_bar = token_volume.unwrap_or(0.0);
    if official_volumes.is_empty() {
        let width = if token_bar > 0.0 { 100.0 } else { 0.0 };
        return format!(
            "<figure class=\"stats-volume-chart\"><figcaption>Reported 24h volume comparison</figcaption><svg viewBox=\"0 0 400 58\" role=\"img\" aria-label=\"Token reported 24h volume {}; no official pool volume data available on the QED leaderboard for this ticker\"><text x=\"0\" y=\"15\">Flagged token</text><rect x=\"112\" y=\"5\" width=\"{width}\" height=\"12\" rx=\"3\"/><text x=\"396\" y=\"15\" text-anchor=\"end\">{}</text></svg><p>No official pool volume data available on the QED leaderboard for this ticker.</p></figure>",
            html_escape(&token_value),
            html_escape(&token_value),
        );
    }
    let official_volume = official_volumes.iter().sum::<f64>();
    let max_value = token_bar.max(official_volume);
    let token_width = if max_value > 0.0 { token_bar / max_value * 150.0 } else { 0.0 };
    let official_width = if max_value > 0.0 { official_volume / max_value * 150.0 } else { 0.0 };
    let official_label = format_reported_usd(official_volume);
    format!(
        "<figure class=\"stats-volume-chart\"><figcaption>Reported 24h volume comparison</figcaption><svg viewBox=\"0 0 400 58\" role=\"img\" aria-label=\"Flagged token volume {}; official publisher pool volume {}\"><text x=\"0\" y=\"15\">Flagged token</text><rect x=\"112\" y=\"5\" width=\"{token_width:.1}\" height=\"12\" rx=\"3\"/><text x=\"396\" y=\"15\" text-anchor=\"end\">{}</text><text x=\"0\" y=\"43\">Official pools</text><rect x=\"112\" y=\"33\" width=\"{official_width:.1}\" height=\"12\" rx=\"3\"/><text x=\"396\" y=\"43\" text-anchor=\"end\">{}</text></svg><p>Reported volumes; not verified.</p></figure>",
        html_escape(&token_value),
        html_escape(&official_label),
        html_escape(&token_value),
        html_escape(&official_label),
    )
}

fn format_reported_usd(value: f64) -> String {
    if value.is_finite() { super::views::usd_label(Some(value)) } else { "—".to_owned() }
}

fn format_card_reported_usd(value: f64) -> String {
    if !value.is_finite() {
        return "—".to_owned();
    }
    if value >= 1_000_000.0 {
        format!("${:.1}m", value / 1_000_000.0)
    } else if value >= 1_000.0 {
        format!("${:.1}k", value / 1_000.0)
    } else {
        format_reported_usd(value)
    }
}

fn human_utc_time(value: &str) -> String {
    let escaped = html_escape(value);
    let display = chrono::DateTime::parse_from_rfc3339(value)
        .map(|time| time.with_timezone(&chrono::Utc).format("%-d %b %Y, %H:%M UTC").to_string())
        .unwrap_or_else(|_| escaped.clone());
    format!("<time datetime=\"{escaped}\">{display}</time>")
}

fn short_address(address: &str) -> String {
    let length = address.chars().count();
    if length <= 18 {
        return address.to_owned();
    }
    format!(
        "{}…{}",
        address.chars().take(8).collect::<String>(),
        address.chars().skip(length - 6).collect::<String>()
    )
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct TokenLookupQuery {
    pub(crate) ticker: Option<String>,
}

pub(crate) fn canonical_ticker(registry: &registry::Registry, query: &str) -> Option<String> {
    let query = query.trim();
    if query.is_empty()
        || query.len() > 32
        || !query.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    {
        return None;
    }
    registry
        .iter()
        .filter(|entry| registry::matchable(entry))
        .find(|entry| entry.ticker.eq_ignore_ascii_case(query))
        .map(|entry| entry.ticker.clone())
        .filter(|ticker| {
            ticker.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        })
}

fn token_redirect(
    registry: &registry::Registry,
    ticker: Option<&str>,
) -> Result<Redirect, StatusCode> {
    let Some(ticker) = ticker.and_then(|ticker| canonical_ticker(registry, ticker)) else {
        return Err(StatusCode::NOT_FOUND);
    };
    Ok(Redirect::to(&format!("/tokens/{ticker}")))
}

pub(crate) async fn token_lookup(
    State(state): State<AppState>,
    Query(query): Query<TokenLookupQuery>,
) -> Result<Redirect, StatusCode> {
    let registry = state.registry.read().await;
    token_redirect(&registry, query.ticker.as_deref())
}

pub(crate) async fn registry_page(
    State(state): State<AppState>,
    _headers: HeaderMap,
) -> Result<Html<String>, StatusCode> {
    let registry = state.registry.read().await;
    let registry_status = state.registry_status.read().await.clone();
    let value = serde_json::json!({ "registry": registry_status });
    let registry_status_tooltip = super::views::registry_status_tooltip(&value);
    render_page(
        RegistryTemplate {
            asset_version: ASSET_VERSION,
            registry_count: registry::active_count(&registry),
            registry_status_line: super::views::registry_status_line(
                &value,
                registry::active_count(&registry),
            ),
            registry_status_tooltip,
            public_url: state.public_url.to_string(),
        },
        false,
    )
}

pub(crate) async fn token_page(
    State(state): State<AppState>,
    Path(ticker): Path<String>,
    _headers: HeaderMap,
    Query(history_query): Query<super::asset::HistoryQuery>,
) -> Result<Html<String>, StatusCode> {
    let ticker_query = ticker.trim();
    let registry = state.registry.read().await.clone();
    let Some(canonical_ticker) = canonical_ticker(&registry, ticker_query) else {
        return Err(StatusCode::NOT_FOUND);
    };
    let mut contracts: Vec<_> = registry
        .iter()
        .filter(|entry| registry::matchable(entry))
        .filter(|entry| entry.ticker.eq_ignore_ascii_case(&canonical_ticker))
        .map(contract_view)
        .collect();
    let crawler =
        crate::ports::current_rpc_request_context().is_some_and(|context| context.crawler);
    let mut pending = Vec::new();
    for (index, contract) in contracts.iter_mut().enumerate() {
        if let Some(record) =
            crate::app::powers::cached_record(&state.app, contract.chain_kind, &contract.contract)
                .await
        {
            contract.powers = display_powers(&state.app, record).await;
        } else if !crawler {
            crate::app::powers::schedule_prefetch(
                &state.app,
                contract.chain_kind,
                &contract.contract,
            )
            .await;
            let pending_key =
                crate::app::powers::canonical_contract(contract.chain_kind, &contract.contract)
                    .ok()
                    .map(|canonical| (contract.chain_kind, canonical));
            pending.push((index, contract.chain_kind, contract.contract.clone(), pending_key));
        }
    }
    if !pending.is_empty() {
        let _ = tokio::time::timeout(POWERS_PAGE_DEADLINE, async {
            loop {
                let prefetching = state.app.powers_prefetching.lock().await;
                let still_pending = pending.iter().any(|(_, _, _, key)| {
                    key.as_ref().is_some_and(|key| prefetching.contains(key))
                });
                if !still_pending {
                    break;
                }
                drop(prefetching);
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
        })
        .await;
        for (index, chain, contract, _) in pending {
            contracts[index].powers =
                match crate::app::powers::cached_record(&state.app, chain, &contract).await {
                    Some(record) => display_powers(&state.app, record).await,
                    None => DirectoryPowersView::unavailable(),
                };
        }
    }
    sort_directory_contracts(&mut contracts);
    let leaderboard = state.leaderboard.read().await;
    let mut assets: Vec<_> = contracts
        .iter()
        .filter_map(|contract| {
            crate::domain::asset_history::AssetKey::new(
                contract.chain_kind,
                crate::domain::asset_history::SubjectKind::Token,
                &contract.contract,
            )
        })
        .chain(
            leaderboard
                .entries
                .iter()
                .filter(|entry| {
                    entry
                        .ticker
                        .as_deref()
                        .is_some_and(|ticker| ticker.eq_ignore_ascii_case(&canonical_ticker))
                })
                .filter_map(|entry| {
                    crate::domain::chain::Chain::parse(&entry.chain).and_then(|chain| {
                        crate::domain::asset_history::AssetKey::new(
                            chain,
                            crate::domain::asset_history::SubjectKind::Pool,
                            &entry.pool,
                        )
                    })
                }),
        )
        .collect();
    assets.extend(
        state
            .app
            .history
            .states()
            .into_iter()
            .filter(|(_, _, projection)| {
                [&projection.latest, &projection.last_confirmed].into_iter().flatten().any(
                    |event| {
                        event
                            .ticker
                            .as_deref()
                            .is_some_and(|ticker| ticker.eq_ignore_ascii_case(&canonical_ticker))
                    },
                )
            })
            .map(|(key, _, _)| key),
    );
    assets.sort_by(|left, right| {
        crate::adapters::discovery::chain_slug(left.chain)
            .cmp(crate::adapters::discovery::chain_slug(right.chain))
            .then_with(|| {
                (left.kind == crate::domain::asset_history::SubjectKind::Pool)
                    .cmp(&(right.kind == crate::domain::asset_history::SubjectKind::Pool))
            })
            .then_with(|| left.address.cmp(&right.address))
    });
    assets.dedup();
    let mut history = super::asset::prepare(
        &state.app,
        assets,
        format!("/tokens/{canonical_ticker}"),
        &history_query,
    )
    .await;
    super::asset::fill_pool_fallbacks(&state.app, &leaderboard.entries, &mut history);
    super::asset::fill_facts(&state.app, &registry, &contracts, &leaderboard.entries, &mut history);
    let name = registry
        .iter()
        .find(|entry| entry.ticker == canonical_ticker)
        .map(|entry| entry.name.clone())
        .unwrap_or_default();
    let issuers = contracts
        .iter()
        .map(|contract| contract.issuer.as_str())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(" · ");
    let title = canonical_ticker;
    render_page(
        TokenTemplate {
            asset_version: ASSET_VERSION,
            public_url: state.public_url.to_string(),
            title,
            contracts,
            name,
            issuers,
            history,
        },
        false,
    )
}

pub(crate) async fn chain_page(
    State(state): State<AppState>,
    Path(chain_name): Path<String>,
    _headers: HeaderMap,
) -> Result<Html<String>, StatusCode> {
    let chain = super::views::parse_chain(&chain_name).ok_or(StatusCode::NOT_FOUND)?;
    let registry = state.registry.read().await.clone();
    let mut contracts: Vec<_> = registry
        .iter()
        .filter(|entry| entry.chain == chain && registry::matchable(entry))
        .map(contract_view)
        .collect();
    contracts.sort_by(|left, right| {
        left.ticker.cmp(&right.ticker).then_with(|| left.issuer.cmp(&right.issuer))
    });
    let leaderboard = state.leaderboard.read().await;
    let mut pools: Vec<_> = leaderboard
        .entries
        .iter()
        .filter(|entry| super::views::parse_chain(&entry.chain) == Some(chain))
        .map(pool_view)
        .collect();
    pools.sort_by(|left, right| left.pair.cmp(&right.pair));
    let chain_label = chain.to_string();
    let chain_slug = super::views::chain_slug(chain).to_owned();
    let json_ld = safe_json_ld(json!({
        "@context": "https://schema.org",
        "@type": "CollectionPage",
        "name": format!("{} token contracts and pools · QED", chain_label),
        "url": format!("{}/chains/{}", state.public_url, chain_slug),
        "description": format!("Issuer contracts and currently listed stock-paired pools on {chain_label}."),
        "mainEntity": {
            "@type": "ItemList",
            "numberOfItems": contracts.len(),
            "itemListElement": contracts.iter().enumerate().map(|(index, contract)| json!({
                "@type": "ListItem",
                "position": index + 1,
                "name": format!("{} {} on {}", contract.issuer, contract.ticker, contract.chain),
                "url": contract.explorer_url
            })).collect::<Vec<_>>()
        }
    }));
    render_page(
        ChainTemplate {
            asset_version: ASSET_VERSION,
            public_url: state.public_url.to_string(),
            chain: chain_label,
            chain_slug,
            contract_count: contracts.len(),
            contracts,
            pools,
            json_ld,
        },
        false,
    )
}

pub(crate) async fn glossary_page(
    State(state): State<AppState>,
    _headers: HeaderMap,
) -> Result<Html<String>, StatusCode> {
    render_page(
        GlossaryTemplate {
            asset_version: ASSET_VERSION,
            public_url: state.public_url.to_string(),
            page_title: "Glossary".to_owned(),
            heading: "Glossary.".to_owned(),
            eyebrow: "REFERENCE".to_owned(),
            intro: "Short definitions for the terms used in QED pages and checks.".to_owned(),
            meta_description: "Plain-language definitions for stock-paired pools, issuer registries, tokenized stocks, and QED verdicts.".to_owned(),
            canonical_path: "/glossary".to_owned(),
            active_sidebar: String::new(),
        },
        false,
    )
}

fn guide_verify_template(
    state: &AppState,
    canonical_path: &str,
    page_title: &str,
) -> Result<Html<String>, StatusCode> {
    let is_docs_landing = canonical_path == "/docs";
    render_page(
        GuideVerifyTemplate {
            asset_version: ASSET_VERSION,
            public_url: state.public_url.to_string(),
            page_title: page_title.to_owned(),
            heading: if is_docs_landing {
                "QED tools, in plain words.".to_owned()
            } else {
                "How do I check if a stock token matches what its issuer published?".to_owned()
            },
            eyebrow: if is_docs_landing { "DOCUMENTATION".to_owned() } else { "GUIDE".to_owned() },
            intro: if is_docs_landing {
                "Choose the question you need answered, then open a real example. QED reads public chain data; it does not recommend trades."
                    .to_owned()
            } else {
                "Start with the contract, not the ticker. Compare the address you were given with the contract published by the issuer and the chain's observed state. This comparison does not prove backing or custody."
                    .to_owned()
            },
            meta_description: if is_docs_landing {
                "Plain-language answers for each QED tool, with quick paths for agents, apps, and auditors."
                    .to_owned()
            } else {
                "A practical contract-first method for comparing a stock-token address with the contract published by its issuer, followed by QED's signed comparison."
                    .to_owned()
            },
            canonical_path: canonical_path.to_owned(),
            active_sidebar: if is_docs_landing { "overview" } else { "verify" }.to_owned(),
        },
        false,
    )
}

pub(crate) async fn guide_verify_page(
    State(state): State<AppState>,
    _headers: HeaderMap,
) -> Result<Html<String>, StatusCode> {
    guide_verify_template(
        &state,
        "/guide/verify-a-stock-token",
        "Contract-first verification guide",
    )
}

fn directory_chain_rank(chain: &str) -> u8 {
    match super::views::parse_chain(chain) {
        Some(Chain::Solana) => 0,
        Some(Chain::RobinhoodChain) => 1,
        Some(Chain::Ethereum) => 2,
        Some(Chain::Bnb) => 3,
        _ => 4,
    }
}

fn sort_directory_contracts(contracts: &mut [DirectoryContractView]) {
    contracts.sort_by(|left, right| {
        directory_chain_rank(&left.chain)
            .cmp(&directory_chain_rank(&right.chain))
            .then_with(|| left.chain.cmp(&right.chain))
            .then_with(|| left.issuer.cmp(&right.issuer))
            .then_with(|| left.contract.cmp(&right.contract))
    });
}

pub(super) fn contract_view(entry: &Entry) -> DirectoryContractView {
    DirectoryContractView {
        issuer: entry.issuer.clone(),
        ticker: entry.ticker.clone(),
        chain: entry.chain.to_string(),
        chain_kind: entry.chain,
        chain_icon: super::views::chain_icon(entry.chain),
        contract: entry.contract.clone(),
        explorer_url: super::views::explorer_link(entry.chain, &entry.contract),
        source_url: entry.source_url.clone(),
        powers: DirectoryPowersView::unavailable(),
    }
}

pub(crate) fn powers_view(record: crate::domain::powers::PowersRecord) -> DirectoryPowersView {
    fn details(reasons: Vec<crate::domain::powers::Reason>) -> Vec<String> {
        reasons.into_iter().map(|reason| format!("{}: {}", reason.code, reason.detail)).collect()
    }
    fn status(source: crate::domain::powers::SourceVerified) -> &'static str {
        match source {
            crate::domain::powers::SourceVerified::ExactMatch => "Exact match",
            crate::domain::powers::SourceVerified::Match => "Match",
            crate::domain::powers::SourceVerified::None => "No match",
            crate::domain::powers::SourceVerified::Unavailable => "Unavailable",
        }
    }
    let has_control_signals = !record.can_seize.is_empty()
        || !record.can_block.is_empty()
        || !record.can_change_rules.is_empty();
    let controls = [
        (!record.can_seize.is_empty(), "the token can be seized"),
        (!record.can_block.is_empty(), "transfers can be blocked"),
        (!record.can_change_rules.is_empty(), "token rules can be changed"),
    ];
    let control_count = controls.iter().filter(|(observed, _)| *observed).count();
    let mut summary_sentence = String::with_capacity(192);
    use std::fmt::Write as _;
    write!(&mut summary_sentence, "On {}: ", record.chain)
        .expect("writing into a String cannot fail");
    if control_count == 0 {
        if record.unavailable.is_empty() {
            summary_sentence.push_str(
                "No control signals were observed; this is not proof that no authority exists.",
            );
        } else {
            summary_sentence
                .push_str("Some control checks were unavailable; retry for a complete reading.");
        }
    } else {
        summary_sentence.push_str("Observed controls: ");
        let mut included = 0;
        for (observed, description) in controls {
            if !observed {
                continue;
            }
            if included > 0 {
                if included + 1 == control_count {
                    if control_count > 2 {
                        summary_sentence.push(',');
                    }
                    summary_sentence.push_str(" and ");
                } else {
                    summary_sentence.push_str(", ");
                }
            }
            summary_sentence.push_str(description);
            included += 1;
        }
        summary_sentence.push('.');
        if !record.unavailable.is_empty() {
            summary_sentence
                .push_str(" Other control checks were unavailable; retry for a complete reading.");
        }
    }
    let mut summary_badges = Vec::with_capacity(4);
    if !record.can_seize.is_empty() {
        summary_badges.push("Can seize".to_owned());
    }
    if !record.can_block.is_empty() {
        summary_badges.push("Can block".to_owned());
    }
    if !record.can_change_rules.is_empty() {
        summary_badges.push("Can change rules".to_owned());
    }
    if !record.unavailable.is_empty() {
        summary_badges.push("Signals unavailable (transient)".to_owned());
    } else if !has_control_signals {
        summary_badges.push("No control signals observed".to_owned());
    }
    let source_badge = match record.source_verified {
        crate::domain::powers::SourceVerified::ExactMatch => "Source verified (exact match)",
        crate::domain::powers::SourceVerified::Match => "Source verified (match)",
        crate::domain::powers::SourceVerified::None => "Source unverified",
        crate::domain::powers::SourceVerified::Unavailable => "Source unavailable",
    };

    DirectoryPowersView {
        available: true,
        can_seize: details(record.can_seize),
        can_block: details(record.can_block),
        can_change_rules: details(record.can_change_rules),
        unavailable: details(record.unavailable),
        summary_sentence,
        summary_badges,
        source_verified_subject: record.source_verified_subject.label().to_owned(),
        source_verified: status(record.source_verified).to_owned(),
        source_verified_proxy: record
            .source_verified_proxy
            .map(status)
            .unwrap_or_default()
            .to_owned(),
        source_badge: source_badge.to_owned(),
        observed_at: record.observed_at,
        last_confirmed_at: String::new(),
        last_confirmed_signals: String::new(),
    }
}

pub(super) fn pool_view(entry: &crate::adapters::discovery::LeaderboardEntry) -> DirectoryPoolView {
    let verdict = entry.verdict.to_ascii_lowercase();
    let verdict_class = match verdict.as_str() {
        "verified" => "is-verified",
        "mismatch" | "nomatch" => "is-mismatch",
        _ => "is-unknown",
    }
    .to_owned();
    let verdict = super::asset::fallback_summary(entry);
    let trade_url = super::views::parse_chain(&entry.chain)
        .map(|chain| {
            crate::adapters::discovery::canonical_market_url_for_source(
                chain,
                &entry.pool,
                Some(&entry.trade_url),
                entry.source,
            )
        })
        .unwrap_or_default();
    DirectoryPoolView {
        dex: entry.dex.clone(),
        pair: super::views::ordered_pair_label(
            &entry.base_symbol,
            &entry.quote_symbol,
            entry.issuer_on_base,
        ),
        verdict,
        verdict_class,
        detail_url: entry.detail_url.clone(),
        trade_url,
    }
}

fn safe_json_ld(value: serde_json::Value) -> String {
    serde_json::to_string(&value)
        .unwrap_or_else(|_| "{}".to_owned())
        .replace('&', "\\u0026")
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
}

pub(crate) async fn validated(
    State(state): State<AppState>,
    _headers: HeaderMap,
) -> Result<Html<String>, StatusCode> {
    let mut cards = super::views::verified_attestations(&state.app)
        .await
        .iter()
        .map(ValidatedCardView::from_attestation)
        .collect::<Vec<_>>();
    let rebuilding = if cards.is_empty() {
        let leaderboard = state.leaderboard.read().await;
        if leaderboard.restored && !leaderboard.entries.is_empty() {
            cards = leaderboard
                .entries
                .iter()
                .filter(|entry| entry.verdict.eq_ignore_ascii_case("verified"))
                .map(ValidatedCardView::from_leaderboard_entry)
                .collect();
            !cards.is_empty()
        } else {
            false
        }
    } else {
        false
    };
    render_page(
        ValidatedTemplate {
            asset_version: ASSET_VERSION,
            public_url: state.public_url.to_string(),
            cards,
            rebuilding,
        },
        false,
    )
}

pub(crate) async fn validated_detail(
    Path((chain_name, subject)): Path<(String, String)>,
) -> Result<Redirect, StatusCode> {
    let chain = super::views::parse_chain(&chain_name).ok_or(StatusCode::NOT_FOUND)?;
    if !crate::domain::check::valid_public_input(&subject) {
        return Err(StatusCode::NOT_FOUND);
    }
    Ok(Redirect::permanent(&format!("/guard/{}/{}", super::views::chain_slug(chain), subject)))
}
pub(crate) async fn registry_table(
    State(state): State<AppState>,
    Query(query): Query<RegistryTableQuery>,
) -> Result<Html<String>, StatusCode> {
    if query.q.as_deref().is_some_and(|query| query.len() > 64) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let registry = state.registry.read().await.clone();
    RegistryTableTemplate::from_registry(&registry, query)
        .render()
        .map(|html| Html(super::format_html_timestamps(&html)))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct RecheckQuery {
    live: Option<String>,
}

impl RecheckQuery {
    fn is_live(&self) -> bool {
        self.live.as_deref().is_some_and(|value| value.eq_ignore_ascii_case("true") || value == "1")
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct GuardForm {
    #[serde(default)]
    address: String,
    #[serde(default)]
    chain: String,
    #[serde(default)]
    wallet: Option<String>,
}

pub(crate) async fn guard_page(State(state): State<AppState>) -> Result<Html<String>, StatusCode> {
    render_page(
        guard_form_template(&state, String::new(), String::new(), String::new(), String::new()),
        false,
    )
}

pub(crate) async fn guard_form_submit(
    State(state): State<AppState>,
    Form(form): Form<GuardForm>,
) -> Result<Response, StatusCode> {
    let Some(chain) = Chain::parse(&form.chain) else {
        return guard_form_error(
            &state,
            &form,
            "Select a supported chain.",
            StatusCode::BAD_REQUEST,
        );
    };
    let document =
        match crate::app::guard::create(&state.app, &form.address, chain, form.wallet.as_deref())
            .await
        {
            Ok(document) => document,
            Err(error) => {
                return guard_form_error(
                    &state,
                    &form,
                    guard_error_message(&error),
                    super::guard_error_status(&error),
                );
            }
        };
    if form.wallet.as_deref().is_none_or(|wallet| wallet.trim().is_empty()) {
        let location = format!("/guard/{}/{}", super::views::chain_slug(chain), document.address);
        return Ok(Redirect::to(&location).into_response());
    }
    render_page(
        GuardResultTemplate::from_document(ASSET_VERSION, state.public_url.to_string(), &document),
        false,
    )
    .map(IntoResponse::into_response)
}

pub(crate) async fn guard_result_page(
    State(state): State<AppState>,
    Path((chain, address)): Path<(String, String)>,
) -> Result<Html<String>, StatusCode> {
    let chain = Chain::parse(&chain).ok_or(StatusCode::BAD_REQUEST)?;
    let document = crate::app::guard::create(&state.app, &address, chain, None)
        .await
        .map_err(|error| super::guard_error_status(&error))?;
    render_page(
        GuardResultTemplate::from_document(ASSET_VERSION, state.public_url.to_string(), &document),
        false,
    )
}

fn guard_form_template(
    state: &AppState,
    address: String,
    chain: String,
    wallet: String,
    error: String,
) -> GuardTemplate {
    GuardTemplate {
        asset_version: ASSET_VERSION,
        public_url: state.public_url.to_string(),
        address,
        chain,
        wallet,
        error,
    }
}

fn guard_form_error(
    state: &AppState,
    form: &GuardForm,
    error: &str,
    status: StatusCode,
) -> Result<Response, StatusCode> {
    let html = render_page(
        guard_form_template(
            state,
            form.address.clone(),
            form.chain.clone(),
            form.wallet.clone().unwrap_or_default(),
            error.to_owned(),
        ),
        false,
    )?;
    Ok((status, html).into_response())
}
pub(crate) fn guard_error_response(
    state: &AppState,
    path: &str,
    status: StatusCode,
    message: &str,
) -> Result<Response, StatusCode> {
    let (chain, address) = path
        .strip_prefix("/guard/")
        .and_then(|path| path.split_once('/'))
        .map(|(chain, address)| (chain.to_owned(), address.to_owned()))
        .unwrap_or_default();
    let html = render_page(
        guard_form_template(state, address, chain, String::new(), message.to_owned()),
        false,
    )?;
    Ok((status, html).into_response())
}

fn guard_error_message(error: &crate::app::guard::GuardError) -> &'static str {
    match error {
        crate::app::guard::GuardError::InvalidAddress => {
            "Enter a valid token or supported pool address for this chain."
        }
        crate::app::guard::GuardError::InvalidWallet => {
            "Enter a valid wallet address for this chain."
        }
        crate::app::guard::GuardError::ReaderUnavailable => {
            "QED could not read the selected chain. Try again."
        }
        crate::app::guard::GuardError::DeadlineExceeded => {
            "The 30-second Guard review deadline elapsed. Try again."
        }
        crate::app::guard::GuardError::DailyBudgetExceeded => {
            "QED daily read budget reached - cached results still served; try again after 00:00 UTC"
        }
        crate::app::guard::GuardError::CrawlerCacheMiss => {
            "Automated crawlers get cached results only."
        }
        crate::app::guard::GuardError::Signing => "QED could not sign this Guard review.",
    }
}

pub(crate) async fn featured(State(state): State<AppState>) -> Result<Html<String>, StatusCode> {
    let featured = state.featured.read().await;
    FeaturedTemplate {
        pools: featured
            .iter()
            .take(4)
            .map(|pool| super::views::projected_featured(pool, &state.app))
            .collect(),
    }
    .render()
    .map(|html| Html(super::format_html_timestamps(&html)))
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

const ABOUT_MARKDOWN: &str = r#"QED is a live, read-only checker: it checks whether a pool uses the stock-token contract published by its issuer. It does not prove backing, custody, reserves, solvency, safety, price, liquidity, or endorsement. QED is operated by Web3 Energy Ltd (Bulgaria); see the [imprint](/imprint). The public source repository is [github.com/boev/qed](https://github.com/boev/qed). The two protected homepage statements are “A ticker is not a contract. Compare the pool with the issuer's published stock-token contract.” and “QED checks whether a pool uses the stock-token contract published by its issuer.”"#;

pub(crate) async fn docs_page(State(state): State<AppState>) -> Result<Html<String>, StatusCode> {
    guide_verify_template(&state, "/docs", "Documentation")
}

const LLM_FLOW_SVG: &str = r#"<div class="guide-flow-scroll">
<svg class="guide-flow" viewBox="0 0 900 220" role="img" aria-labelledby="llm-flow-title llm-flow-desc">
  <title id="llm-flow-title">QED's read-only MCP check flow</title>
  <desc id="llm-flow-desc">An agent asks QED to check NVDA, QED reads the chain through RPC, and returns a signed match.</desc>
  <defs>
    <marker id="llm-flow-arrow" markerWidth="8" markerHeight="8" refX="7" refY="4" orient="auto">
      <path d="M0 0 L8 4 L0 8 Z" fill="var(--accent)"/>
    </marker>
  </defs>
  <g class="flow-text">
    <rect class="flow-node" x="32" y="64" width="148" height="64" rx="12"/>
    <rect class="flow-node flow-node-emphasis" x="242" y="64" width="150" height="64" rx="12"/>
    <rect class="flow-node" x="452" y="64" width="150" height="64" rx="12"/>
    <rect class="flow-node" x="662" y="64" width="196" height="64" rx="12"/>
    <text class="flow-node-label" x="106" y="91" text-anchor="middle">LLM</text>
    <text class="flow-detail" x="106" y="111" text-anchor="middle">Is this the real NVDA?</text>
    <text class="flow-node-label" x="317" y="99" text-anchor="middle">MCP · /mcp</text>
    <text class="flow-node-label" x="527" y="99" text-anchor="middle">QED</text>
    <text class="flow-detail" x="527" y="116" text-anchor="middle">issuer match</text>
    <text class="flow-node-label" x="760" y="99" text-anchor="middle">Chain RPC</text>
    <text class="flow-detail" x="760" y="116" text-anchor="middle">read-only</text>
  </g>
  <path class="flow-path" d="M180 96 H242" marker-end="url(#llm-flow-arrow)"/>
  <path class="flow-path" d="M392 96 H452" marker-end="url(#llm-flow-arrow)"/>
  <path class="flow-path" d="M602 96 H662" marker-end="url(#llm-flow-arrow)"/>
  <path class="flow-path" d="M760 128 V176 H106 V128" marker-end="url(#llm-flow-arrow)"/>
  <text class="flow-text flow-return-label" x="433" y="166" text-anchor="middle">Match, signed</text>
  <circle class="flow-dot flow-dot-static" cx="760" cy="128" r="5"/>
  <circle class="flow-dot flow-dot-moving" r="5">
    <animateMotion dur="7s" repeatCount="indefinite" path="M760 128 V176 H106 V128"/>
  </circle>
</svg>
</div>"#;

pub(crate) async fn docs_llm_page(
    State(state): State<AppState>,
) -> Result<Html<String>, StatusCode> {
    let body = r#"QED exposes a stateless, read-only MCP server at `https://qed.web3-energy.com/mcp`. The [server card](https://qed.web3-energy.com/.well-known/mcp/server-card.json) describes the seven tools.

In Claude Code, connect with:

```sh
claude mcp add --transport http qed https://qed.web3-energy.com/mcp
```

For ChatGPT, Cursor, Claude Desktop, and other MCP clients, add an HTTP MCP server in your client’s MCP settings using `https://qed.web3-energy.com/mcp`. Product labels and availability vary by client.

Tools:
- `qed_check` answers whether a pool or token uses its issuer's published contract.
- `qed_powers` answers what controls QED observed on the token.
- `qed_wallet` answers which issuer-published holdings are visible in a wallet.
- `qed_statement` answers what a selected wallet set held at a block height.
- `qed_registry_lookup` answers which contracts an issuer published for a ticker.
- `qed_verify` checks whether a signed QED document verifies.
- `qed_guard` answers whether the token matches its issuer's published identity and what controls QED observed.

Try asking:
- “Does this pool use the issuer's published stock-token contract?”
- “What could the issuer change on this token?”
- “Which contracts did this issuer publish for NVDA?”
- “What did these wallets hold, provably?”

Re-check certificates after expiry or when the issuer registry changes.

QED does not prove backing, custody, reserves, solvency, safety, price, liquidity, or endorsement."#;

    docs_content_page(
        &state,
        PageMetadata {
            page_title: "Use QED with an LLM",
            heading: "Use QED with an LLM",
            eyebrow: "GUIDE",
            intro: "Connect an AI agent to QED’s issuer-check, token-power, statement, registry, verification, and Guard tools over MCP.",
            meta_description: "Seven read-only tools answer plain questions about issuer publications, token controls, wallet holdings, and signed records.",
            canonical_path: "/docs/llm",
        },
        "llm",
        format!("{}{LLM_FLOW_SVG}", content_adapter::render_markdown(body)),
    )
}

pub(crate) async fn docs_api_quick_start_page(
    State(state): State<AppState>,
) -> Result<Html<String>, StatusCode> {
    let body = r#"Set `BASE=https://qed.web3-energy.com`, then make three requests:

```sh
curl -sS "$BASE/api/check/POOL_OR_TOKEN_ADDRESS"
curl -sS "$BASE/api/registry?ticker=NVDA"
curl -sS "$BASE/api/status"
```

The check endpoint returns a point-in-time issuer-contract comparison. Registry lookup returns the issuer entries for a ticker; status reports service freshness. See the [generated API reference](/api), [OpenAPI document](/openapi.json), or [MCP guide](/docs/llm) for complete contracts."#;
    docs_content_page(
        &state,
        PageMetadata {
            page_title: "API quick start",
            heading: "API quick start",
            eyebrow: "GUIDE",
            intro: "Make your first QED requests with three curl commands.",
            meta_description: "Three curl examples for checks, registry lookup, and service status.",
            canonical_path: "/docs/api-quick-start",
        },
        "api-quick-start",
        content_adapter::render_markdown(body),
    )
}

fn about_project_content() -> String {
    let release_date = content_adapter::changelog_entries()
        .first()
        .and_then(|release| release.date.as_deref())
        .map(html_escape)
        .unwrap_or_default();
    let version = env!("CARGO_PKG_VERSION");
    format!(
        r#"<section class="lookup lookup-secondary" aria-labelledby="about-facts-title">
  <h2 id="about-facts-title">Project facts</h2>
  <dl class="certificate-facts about-facts">
    <div><dt>Version</dt><dd><a href="/changelog">v{version} · released {release_date}</a></dd></div>
    <div><dt>Source code</dt><dd><a href="https://github.com/boev/qed" target="_blank" rel="noopener noreferrer">github.com/boev/qed</a></dd></div>
    <div><dt>Contact</dt><dd><a href="mailto:contact@web3-energy.com">contact@web3-energy.com</a></dd></div>
    <div><dt>Security</dt><dd><a href="/security">Security policy</a></dd></div>
  </dl>
</section>
<section class="lookup lookup-secondary about-contact" aria-labelledby="about-contact-title">
  <h2 id="about-contact-title">Contact</h2>
  <p>Questions, issuer corrections or takedown requests: <a href="mailto:contact@web3-energy.com">contact@web3-energy.com</a>.</p>
</section>
{}"#,
        content_adapter::render_markdown(ABOUT_MARKDOWN),
    )
}

pub(crate) async fn about_page(State(state): State<AppState>) -> Result<Html<String>, StatusCode> {
    docs_content_page(
        &state,
        PageMetadata {
            page_title: "About",
            heading: "About QED",
            eyebrow: "ABOUT",
            intro: "What QED checks, what it does not claim, and who operates the service.",
            meta_description: "QED checks whether a pool uses the stock-token contract published by its issuer and states the limits of that check.",
            canonical_path: "/about",
        },
        "about",
        about_project_content(),
    )
}

pub(crate) async fn security_page(
    State(state): State<AppState>,
) -> Result<Html<String>, StatusCode> {
    docs_content_page(
        &state,
        PageMetadata {
            page_title: "Security",
            heading: "Security",
            eyebrow: "SECURITY",
            intro: "How to report suspected QED security issues and coordinate disclosure.",
            meta_description: "How to report suspected QED security issues and coordinate disclosure.",
            canonical_path: "/security",
        },
        "security",
        content_adapter::security_html(),
    )
}

pub(crate) async fn changelog_page(
    State(state): State<AppState>,
) -> Result<Html<String>, StatusCode> {
    docs_content_page(
        &state,
        PageMetadata {
            page_title: "Changelog",
            heading: "Changelog",
            eyebrow: "CHANGELOG",
            intro: "Release history for QED routes, observations, and documentation.",
            meta_description: "Release history for QED routes, observations, and documentation.",
            canonical_path: "/changelog",
        },
        "changelog",
        content_adapter::changelog_html(),
    )
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct BlogPageQuery {
    page: Option<usize>,
}

pub(crate) async fn blog_page(
    State(state): State<AppState>,
    Query(query): Query<BlogPageQuery>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    let posts = content_adapter::blog_posts().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let posts = content_adapter::published_blog_posts(posts);
    let html = render_blog_page(&state, &posts, query.page, wants_fragment(&headers))?;
    let mut response = html.into_response();
    response
        .headers_mut()
        .append(header::VARY, HeaderValue::from_static("HX-Request, HX-History-Restore-Request"));
    Ok(response)
}

pub(crate) fn render_blog_page(
    state: &AppState,
    posts: &[content_adapter::BlogPost],
    requested_page: Option<usize>,
    htmx_request: bool,
) -> Result<Html<String>, StatusCode> {
    let page = requested_page.unwrap_or(1).max(1);
    let page_count = posts.len().div_ceil(BLOG_PAGE_SIZE).max(1);
    if page > page_count {
        return Err(StatusCode::NOT_FOUND);
    }
    let start = page.saturating_sub(1).saturating_mul(BLOG_PAGE_SIZE);
    let end = start.saturating_add(BLOG_PAGE_SIZE).min(posts.len());
    let page_posts = posts.get(start..end).unwrap_or(&[]);
    let has_next = end < posts.len();
    let items = content_adapter::blog_index_html(page_posts);
    let sentinel = if has_next {
        format!(
            "<div class=\"blog-scroll-sentinel\" hx-get=\"/blog?page={}\" hx-trigger=\"revealed\" hx-push-url=\"false\" hx-swap=\"outerHTML\" aria-hidden=\"true\"></div>",
            page.saturating_add(1)
        )
    } else {
        String::new()
    };
    let fragment = format!("{items}{sentinel}");
    if requested_page.is_some() && htmx_request {
        return Ok(Html(super::format_html_timestamps(&fragment)));
    }

    let pagination = if page > 1 || has_next {
        format!(
            "<nav class=\"blog-pagination\" aria-label=\"Blog pages\">{}<span>Page {page} of {page_count}</span>{}</nav>",
            if page > 1 {
                format!("<a href=\"/blog?page={}\" hx-boost=\"false\">Previous posts</a>", page - 1)
            } else {
                String::new()
            },
            if has_next {
                format!(
                    "<a href=\"/blog?page={}\" hx-boost=\"false\">Next posts</a>",
                    page.saturating_add(1)
                )
            } else {
                String::new()
            }
        )
    } else {
        String::new()
    };
    let body = format!("<div class=\"blog-list\" id=\"blog-list\">{fragment}</div>{pagination}");
    docs_content_page(
        state,
        PageMetadata {
            page_title: "Blog",
            heading: "Blog",
            eyebrow: "BLOG",
            intro: "Published factual notes about QED checks, powers observations, and signed statements.",
            meta_description: "Published factual notes about QED checks, powers observations, and signed statements.",
            canonical_path: "/blog",
        },
        "blog",
        body,
    )
}

pub(crate) async fn blog_post_page(
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> Result<Html<String>, StatusCode> {
    let posts = content_adapter::blog_posts().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let post =
        posts.iter().find(|post| post.slug == slug && !post.draft).ok_or(StatusCode::NOT_FOUND)?;
    docs_content_page(
        &state,
        PageMetadata {
            page_title: &post.title,
            heading: &post.title,
            eyebrow: "BLOG",
            intro: &post.summary,
            meta_description: &post.summary,
            canonical_path: &format!("/blog/{}", post.slug),
        },
        "blog",
        content_adapter::blog_post_html(post),
    )
}

fn statement_request_from_form(
    fields: Vec<(String, String)>,
) -> Result<crate::app::statement::StatementRequest, StatusCode> {
    let mut wallets = None;
    let mut label = None;
    let mut chains = Vec::new();
    let mut block = None;
    for (name, value) in fields {
        match name.as_str() {
            "wallets" => {
                if wallets.is_some() {
                    return Err(StatusCode::BAD_REQUEST);
                }
                wallets = Some(
                    value
                        .lines()
                        .map(str::trim)
                        .filter(|wallet| !wallet.is_empty())
                        .map(str::to_owned)
                        .collect(),
                );
            }
            "chains" => chains.push(value),
            "block" => {
                if block.is_some() {
                    return Err(StatusCode::BAD_REQUEST);
                }
                block = Some(value);
            }
            "label" => {
                if label.is_some() {
                    return Err(StatusCode::BAD_REQUEST);
                }
                label = Some(value);
            }
            _ => {}
        }
    }
    let wallets = wallets.ok_or(StatusCode::BAD_REQUEST)?;
    let block = block
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.parse::<u64>().map_err(|_| StatusCode::BAD_REQUEST))
        .transpose()?;
    Ok(crate::app::statement::StatementRequest {
        label: label.unwrap_or_default(),
        wallets,
        chains,
        block,
    })
}

const STATEMENT_PURPOSE: &str = "A signed, re-checkable record of what a wallet set holds in registry tokens at a block height — for audits, reporting and counterparties.";

pub(crate) async fn statements_page(
    State(state): State<AppState>,
) -> Result<Html<String>, StatusCode> {
    let form = r#"<section class="lookup statement-form-card" aria-labelledby="statement-form-heading">
  <h2 id="statement-form-heading">Create a statement</h2>
  <p class="lookup-note">QED records registry-token balances and chain positions at the observed height. Balances are on-chain facts, not proof of ownership.</p>
  <form class="statement-form" action="/statements" method="post" hx-boost="false">
    <label for="statement-label">Label (optional)</label>
    <input id="statement-label" name="label" type="text" maxlength="80" autocomplete="off" placeholder="e.g. Q3 holdings report">
    <label for="statement-wallets">Wallet addresses, one per line</label>
    <textarea id="statement-wallets" name="wallets" rows="5" maxlength="1439" aria-describedby="statement-wallets-hint" placeholder="Paste wallet addresses here" required></textarea>
    <p id="statement-wallets-hint" class="lookup-note" aria-live="polite">Up to 32 EVM or Solana addresses; one per line.</p>
    <fieldset class="statement-chain-set">
      <legend>Chains</legend>
      <div class="statement-chain-options">
        <label class="statement-chain-option"><input type="checkbox" name="chains" value="solana" checked><span>Solana</span></label>
        <label class="statement-chain-option"><input type="checkbox" name="chains" value="robinhood"><span>Robinhood Chain</span></label>
        <label class="statement-chain-option"><input type="checkbox" name="chains" value="base"><span>Base</span></label>
        <label class="statement-chain-option"><input type="checkbox" name="chains" value="ethereum"><span>Ethereum</span></label>
        <label class="statement-chain-option"><input type="checkbox" name="chains" value="bnb"><span>BNB Chain</span></label>
      </div>
    </fieldset>
    <p class="lookup-note">Select Solana for a Solana address, or the EVM chains where you want balances checked.</p>
    <div class="statement-block-field">
      <label for="statement-block">Optional block or slot</label>
      <input id="statement-block" name="block" type="number" min="0" step="1">
      <p class="lookup-note">An EVM block is exact; a Solana block value is a minimum context slot.</p>
    </div>
    <button class="button primary-button" type="submit">Sign statement <span aria-hidden="true">→</span></button>
  </form>
  <p class="lookup-note statement-nonclaims">QED does not prove backing, custody, reserves, solvency, safety, price, liquidity, or endorsement. Statement pages are public to anyone with their link.</p>
</section>
<section class="lookup lookup-secondary statement-example" aria-labelledby="statement-example-heading">
  <h2 id="statement-example-heading">One-click public example</h2>
  <p>Generate a read-only statement for a public Robinhood pool contract address.</p>
  <form action="/statements" method="post" hx-boost="false">
    <input type="hidden" name="label" value="Public SPY pool address example">
    <input type="hidden" name="wallets" value="0xDDCBBa3666f578E3F09516f21Ff85BFee859AB5e">
    <input type="hidden" name="chains" value="robinhood">
    <button class="button secondary-button" type="submit">Create the public example statement</button>
  </form>
</section>"#;
    content_page(
        &state,
        PageMetadata {
            page_title: "Statement",
            heading: "What did these wallets hold, provably?",
            eyebrow: "STATEMENT",
            intro: STATEMENT_PURPOSE,
            meta_description: "Create a signed, re-checkable statement of registry-token balances for selected wallets.",
            canonical_path: "/statements",
        },
        form.to_owned(),
    )
}

pub(crate) fn statement_error_response(
    state: &AppState,
    status: StatusCode,
    message: &str,
) -> Result<Response, StatusCode> {
    let body = format!(
        "<section class=\"lookup\" role=\"alert\"><p>{}</p><p><a href=\"/statements\">Return to statements</a></p></section>",
        html_escape(message)
    );
    let html = content_page(
        state,
        PageMetadata {
            page_title: "Statement unavailable",
            heading: "Statement unavailable",
            eyebrow: "STATEMENT",
            intro: STATEMENT_PURPOSE,
            meta_description: "Statement reads are temporarily unavailable.",
            canonical_path: "/statements",
        },
        body,
    )?;
    Ok((status, html).into_response())
}

pub(crate) async fn create_statement_form(
    State(state): State<AppState>,
    Form(fields): Form<Vec<(String, String)>>,
) -> Result<Redirect, StatusCode> {
    let request = statement_request_from_form(fields)?;
    let axum::Json(statement) = super::api_statement(State(state), axum::Json(request))
        .await
        .map_err(|(status, _)| status)?;
    Ok(Redirect::to(&format!("/statements/{}", statement.id)))
}

fn content_page(
    state: &AppState,
    metadata: PageMetadata<'_>,
    body_html: String,
) -> Result<Html<String>, StatusCode> {
    render_page(
        ContentPageTemplate {
            asset_version: ASSET_VERSION,
            public_url: state.public_url.to_string(),
            page_title: metadata.page_title.to_owned(),
            heading: metadata.heading.to_owned(),
            eyebrow: metadata.eyebrow.to_owned(),
            intro: metadata.intro.to_owned(),
            meta_description: metadata.meta_description.to_owned(),
            canonical_path: metadata.canonical_path.to_owned(),
            body_html,
        },
        false,
    )
}

pub(crate) fn docs_content_page(
    state: &AppState,
    metadata: PageMetadata<'_>,
    active_sidebar: &str,
    body_html: String,
) -> Result<Html<String>, StatusCode> {
    render_page(
        DocsPageTemplate {
            asset_version: ASSET_VERSION,
            public_url: state.public_url.to_string(),
            page_title: metadata.page_title.to_owned(),
            heading: metadata.heading.to_owned(),
            eyebrow: metadata.eyebrow.to_owned(),
            intro: metadata.intro.to_owned(),
            meta_description: metadata.meta_description.to_owned(),
            canonical_path: metadata.canonical_path.to_owned(),
            active_sidebar: active_sidebar.to_owned(),
            body_html,
        },
        false,
    )
}
#[derive(Debug, Deserialize, Default)]
pub(crate) struct StatementPageQuery {
    compare: Option<String>,
}

pub(crate) async fn statement_page(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<StatementPageQuery>,
) -> Result<Html<String>, StatusCode> {
    let statement =
        crate::app::statement::get(&state.app, &id).await.ok_or(StatusCode::NOT_FOUND)?;
    let wallets_html = statement
        .wallets
        .iter()
        .map(|wallet| {
            format!(
                "<li><strong>{}</strong> <code>{}</code></li>",
                html_escape(&wallet.chain.to_string()),
                html_escape(&wallet.address)
            )
        })
        .collect::<String>();
    let wallet_count = statement
        .wallets
        .iter()
        .map(|wallet| wallet.address.as_str())
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    let chains = statement
        .wallets
        .iter()
        .map(|wallet| wallet.chain.to_string())
        .collect::<std::collections::BTreeSet<_>>();
    let chains_html =
        chains.iter().map(|chain| format!("<li>{}</li>", html_escape(chain))).collect::<String>();
    let positions_html = if statement.positions.is_empty() {
        "<li>No positions recorded.</li>".to_owned()
    } else {
        statement.positions.iter().map(statement_position_html).collect::<String>()
    };
    let assets_html = statement
        .assets
        .iter()
        .map(|asset| {
            let unavailable = asset
                .powers_summary
                .unavailable
                .iter()
                .map(|reason| {
                    format!(
                        "<li><strong>{}</strong>: {}</li>",
                        html_escape(&reason.code),
                        html_escape(&reason.detail)
                    )
                })
                .collect::<String>();
            let unavailable_html = if unavailable.is_empty() {
                "<p>No unavailable power observations were reported.</p>".to_owned()
            } else {
                format!(
                    "<details><summary>Unavailable reasons ({})</summary><ul>{unavailable}</ul></details>",
                    asset.powers_summary.unavailable.len()
                )
            };
            let powers_observed_at = asset
                .powers_observed_at
                .as_deref()
                .map(super::timestamp_html)
                .unwrap_or_else(|| "Not cached".to_owned());
            let powers = format!(
                "<p>{} seize · {} block · {} rule-change signals</p><p>Observed: {powers_observed_at}</p>{unavailable_html}",
                asset.powers_summary.can_seize.len(),
                asset.powers_summary.can_block.len(),
                asset.powers_summary.can_change_rules.len()
            );
            let issuer_match = if asset.issuer_match { "Match" } else { "Mismatch" };
            let balance =
                super::views::format_wallet_amount(&asset.balance, Some(asset.decimals));
            let contract = html_escape(&asset.contract);
            let position_facts_html = statement_asset_position_html(asset);
            format!(
                "<tr class=\"statement-holding-row\"><td>{}</td><td class=\"statement-ticker\"><strong>{}</strong><small>{}</small></td><td class=\"statement-contract\"><code title=\"{contract}\">{contract}</code></td><td><strong>{}</strong><small>Raw {} · {} decimals</small></td><td>{issuer_match}</td></tr><tr class=\"statement-detail-row\"><td colspan=\"5\"><div class=\"statement-row-details\"><div><strong>Token powers</strong>{powers}</div><div><strong>Position and method notes</strong>{position_facts_html}</div></div></td></tr>",
                html_escape(&asset.chain.to_string()),
                html_escape(&asset.ticker),
                html_escape(&asset.issuer),
                html_escape(&balance),
                html_escape(&asset.balance),
                asset.decimals
            )
        })
        .collect::<String>();
    let assets_html = if assets_html.is_empty() {
        "<tr><td colspan=\"5\">No registered token holdings were reported.</td></tr>".to_owned()
    } else {
        assets_html
    };
    let id = html_escape(&statement.id);
    let json_url = format!("/api/statement/{id}");
    let csv_url = format!("/statements/{id}/download.csv");
    let verify_url = format!("/statements/{id}/verify");
    let recheck_url = format!("/statements/{id}/recheck");
    let label =
        if statement.label.trim().is_empty() { "Wallet statement" } else { statement.label.trim() };
    let label = html_escape(label);
    let rows_word = if statement.assets.len() == 1 { "row" } else { "rows" };
    let wallets_word = if wallet_count == 1 { "wallet" } else { "wallets" };
    let chain_count = chains.len();
    let chains_word = if chain_count == 1 { "chain" } else { "chains" };
    let fact_sentence = format!(
        "This signed record reports {} registry-token balance {rows_word} for {wallet_count} {wallets_word} across {chain_count} {chains_word}, observed at {}.",
        statement.assets.len(),
        super::timestamp_html(&statement.observed_at)
    );
    let compare_html = if let Some(previous_id) = query.compare.as_deref() {
        let previous = crate::app::statement::get(&state.app, previous_id)
            .await
            .ok_or(StatusCode::NOT_FOUND)?;
        statement_comparison_html(&previous, &statement)
    } else {
        String::new()
    };

    let signer = html_escape(&statement.signer);
    let signer_notice = if statement.dev {
        "Development signer: this ephemeral signature is not a production trust signal."
    } else {
        "Production signer."
    };
    let mut body_html = format!(
        concat!(
            "<p class=\"statement-fact\">{fact_sentence}</p><section class=\"lookup statement-summary\" aria-labelledby=\"statement-summary-heading\"><div class=\"section-heading\"><h2 id=\"statement-summary-heading\">Statement summary</h2></div><p class=\"statement-label\"><strong>Label:</strong> {label}</p><dl class=\"statement-summary-grid\"><div><dt>Wallets</dt><dd><ul>{wallets_html}</ul></dd></div><div><dt>Chains</dt><dd><ul>{chains_html}</ul></dd></div><div><dt>Observed block and slot range</dt><dd><ul>{positions_html}</ul></dd></div>",
            "<div><dt>Signer</dt><dd><code>{signer}</code><p class=\"lookup-note\">{signer_notice}</p></dd></div><div><dt>Statement ID</dt><dd><code>{id}</code></dd></div><div><dt>Observed at</dt><dd>{}</dd></div></dl>",
            "<div class=\"statement-actions\"><a class=\"button secondary-button\" href=\"{json_url}\" download>Download JSON</a><a class=\"button secondary-button\" href=\"{csv_url}\" download>Download CSV</a><a class=\"button secondary-button\" href=\"{verify_url}\">Verify this record</a><button class=\"button secondary-button\" type=\"button\" data-print-statement>Print / Save as PDF</button><form action=\"{recheck_url}\" method=\"post\" hx-boost=\"false\"><button class=\"button primary-button\" type=\"submit\">Re-run and compare</button></form></div><p class=\"lookup-note\">Save the signed JSON for independent verification; cached statement pages are retained for up to 24 hours.</p></section>{compare_html}",
            "<section class=\"registry-panel statement-holdings\" aria-labelledby=\"statement-holdings-heading\"><div class=\"section-heading\"><h2 id=\"statement-holdings-heading\">Registered token holdings</h2><span class=\"count-badge\">{} assets</span></div><div class=\"table-wrap\"><table class=\"statement-table\"><thead><tr><th>Chain</th><th>Ticker</th><th>Contract</th><th>Balance</th><th>Issuer match</th></tr></thead><tbody>{assets_html}</tbody></table></div></section>",
            "<p class=\"lookup-note statement-nonclaims\">Balances are on-chain facts at a height, not proof of ownership. QED does not prove backing, custody, reserves, solvency, safety, price, liquidity, or endorsement. Statement pages are public to anyone with their link.</p>"
        ),
        super::timestamp_html(&statement.observed_at),
        statement.assets.len(),
        fact_sentence = fact_sentence,
        label = label,
        wallets_html = wallets_html,
        chains_html = chains_html,
        positions_html = positions_html,
        signer = signer,
        signer_notice = signer_notice,
        id = id,
        json_url = json_url,
        csv_url = csv_url,
        verify_url = verify_url,
        recheck_url = recheck_url,
        compare_html = compare_html,
        assets_html = assets_html
    );
    use std::fmt::Write as _;
    write!(
        &mut body_html,
        "<p class=\"statement-print-verify\">Verify this record: <a href=\"{verify_url}\">{verify_url}</a></p>"
    )
    .expect("writing into a String cannot fail");
    let title = format!("Wallet statement {id}");
    let canonical_path = format!("/statements/{id}");
    content_page(
        &state,
        PageMetadata {
            page_title: &title,
            heading: "Wallet statement",
            eyebrow: "STATEMENT",
            intro: STATEMENT_PURPOSE,
            meta_description: "Signed point-in-time token balances and chain positions for selected wallets.",
            canonical_path: &canonical_path,
        },
        body_html,
    )
}

fn statement_comparison_html(
    previous: &crate::domain::statement::Statement,
    current: &crate::domain::statement::Statement,
) -> String {
    use std::collections::{BTreeMap, BTreeSet};

    let previous_assets = previous
        .assets
        .iter()
        .map(|asset| (statement_asset_key(asset), asset))
        .collect::<BTreeMap<_, _>>();
    let current_assets = current
        .assets
        .iter()
        .map(|asset| (statement_asset_key(asset), asset))
        .collect::<BTreeMap<_, _>>();
    let keys =
        previous_assets.keys().chain(current_assets.keys()).cloned().collect::<BTreeSet<_>>();
    let total = keys.len();
    let mut changes = Vec::new();
    for key in keys {
        let previous_asset = previous_assets.get(&key).copied();
        let current_asset = current_assets.get(&key).copied();
        let changed = match (previous_asset, current_asset) {
            (Some(old), Some(new)) => {
                old.balance != new.balance
                    || old.decimals != new.decimals
                    || old.issuer_match != new.issuer_match
            }
            _ => true,
        };
        if !changed {
            continue;
        }
        let ticker =
            current_asset.or(previous_asset).map(|asset| asset.ticker.as_str()).unwrap_or("token");
        let before = previous_asset.map_or_else(
            || "Not reported".to_owned(),
            |asset| super::views::format_wallet_amount(&asset.balance, Some(asset.decimals)),
        );
        let after = current_asset.map_or_else(
            || "Not reported".to_owned(),
            |asset| super::views::format_wallet_amount(&asset.balance, Some(asset.decimals)),
        );
        changes.push(format!(
            "<li><strong>{}</strong> on {} for <code>{}</code> at <code>{}</code>: {} → {}.</li>",
            html_escape(ticker),
            html_escape(&key.0),
            html_escape(&key.1),
            html_escape(&key.2),
            html_escape(&before),
            html_escape(&after)
        ));
    }
    let summary = if changes.is_empty() {
        format!("No listed balance rows changed across {total} compared contracts.")
    } else {
        format!(
            "{} of {total} listed balance rows changed, were added, or were no longer reported.",
            changes.len()
        )
    };
    format!(
        "<section class=\"lookup statement-comparison\" aria-labelledby=\"statement-comparison-heading\"><h2 id=\"statement-comparison-heading\">Comparison with the previous statement</h2><p>{}</p><p>Previous observation: {}. New observation: {}.</p>{}</section>",
        html_escape(&summary),
        super::timestamp_html(&previous.observed_at),
        super::timestamp_html(&current.observed_at),
        if changes.is_empty() { String::new() } else { format!("<ul>{}</ul>", changes.join("")) }
    )
}

fn statement_asset_key(
    asset: &crate::domain::statement::StatementAsset,
) -> (String, String, String) {
    (asset.chain.to_string(), asset.wallet.clone(), asset.contract.to_ascii_lowercase())
}

pub(crate) async fn verify_statement_page(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Html<String>, StatusCode> {
    let statement =
        crate::app::statement::get(&state.app, &id).await.ok_or(StatusCode::NOT_FOUND)?;
    let result = super::api::verify_statement_document(&state, statement.clone());
    let cryptographic = result["cryptographic"].as_bool().unwrap_or(false);
    let trusted = result["trusted_signer"].as_bool().unwrap_or(false);
    let environment = result["environment_match"].as_bool().unwrap_or(false);
    let summary = if cryptographic && trusted && environment {
        "The statement hash and Ed25519 signature verify for this QED signer and environment."
    } else if cryptographic && !trusted {
        "The signature verifies, but this signing key is not trusted by this QED instance."
    } else if cryptographic {
        "The signature verifies, but this record was signed for a different QED environment."
    } else {
        "The statement signature or content hash does not verify; do not rely on this record."
    };
    let id = html_escape(&statement.id);
    let verification_json = serde_json::to_string_pretty(&result)
        .map(|value| html_escape(&value))
        .unwrap_or_else(|_| "{}".to_owned());
    let body = format!(
        "<section class=\"lookup statement-verification\"><p>{}</p><dl><dt>Content hash and signature</dt><dd>{}</dd><dt>Trusted signer</dt><dd>{}</dd><dt>Environment match</dt><dd>{}</dd><dt>Current status</dt><dd>Not applicable: a wallet statement is a point-in-time snapshot.</dd></dl><p>QED public verification key: <a href=\"/.well-known/qed.json\">/.well-known/qed.json</a></p><p><a class=\"button secondary-button\" href=\"/statements/{id}\">Back to statement</a> <a class=\"button secondary-button\" href=\"/api/statement/{id}\" download>Download signed JSON</a></p><details><summary>Verification response</summary><pre><code>{verification_json}</code></pre></details></section>",
        html_escape(summary),
        cryptographic,
        trusted,
        environment
    );
    content_page(
        &state,
        PageMetadata {
            page_title: "Statement verification",
            heading: "Verify this statement.",
            eyebrow: "STATEMENT",
            intro: summary,
            meta_description: "Verify the signed statement hash, signature, trusted signer, and environment.",
            canonical_path: &format!("/statements/{id}/verify"),
        },
        body,
    )
}

pub(crate) async fn statement_csv(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, StatusCode> {
    let statement =
        crate::app::statement::get(&state.app, &id).await.ok_or(StatusCode::NOT_FOUND)?;
    let mut response = Response::new(axum::body::Body::from(statement_csv_body(&statement)));
    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("text/csv; charset=utf-8"),
    );
    let disposition = format!("attachment; filename=\"qed-statement-{}.csv\"", statement.id);
    let disposition = axum::http::HeaderValue::from_str(&disposition)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    response.headers_mut().insert(axum::http::header::CONTENT_DISPOSITION, disposition);
    Ok(response)
}
fn statement_csv_body(statement: &crate::domain::statement::Statement) -> String {
    let mut csv = String::with_capacity(512 + statement.assets.len().saturating_mul(160));
    csv_row(
        &mut csv,
        &[
            "record_type",
            "statement_id",
            "label",
            "observed_at",
            "signer",
            "signature",
            "dev",
            "wallet",
            "chain",
            "block",
            "min_slot",
            "max_slot",
            "slot",
            "ticker",
            "issuer",
            "contract",
            "balance",
            "decimals",
            "issuer_match",
            "verify_url",
        ],
    );
    let dev = statement.dev.to_string();
    let verify_url = format!("/statements/{}/verify", statement.id);
    csv_row(
        &mut csv,
        &[
            "statement",
            &statement.id,
            &statement.label,
            &statement.observed_at,
            &statement.signer,
            &statement.signature,
            &dev,
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            &verify_url,
        ],
    );
    for position in &statement.positions {
        let chain = position.chain.to_string();
        let block = match position.block {
            Some(0) => "not observed".to_owned(),
            Some(value) => value.to_string(),
            None => String::new(),
        };
        let min_slot = position.min_slot.map(|value| value.to_string()).unwrap_or_default();
        let max_slot = position.max_slot.map(|value| value.to_string()).unwrap_or_default();
        csv_row(
            &mut csv,
            &[
                "position",
                &statement.id,
                &statement.label,
                &statement.observed_at,
                "",
                "",
                &dev,
                &position.wallet,
                &chain,
                &block,
                &min_slot,
                &max_slot,
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
            ],
        );
    }
    for asset in &statement.assets {
        let chain = asset.chain.to_string();
        let slot = asset.slot.map(|value| value.to_string()).unwrap_or_default();
        let decimals = asset.decimals.to_string();
        let issuer_match = asset.issuer_match.to_string();
        csv_row(
            &mut csv,
            &[
                "asset",
                &statement.id,
                &statement.label,
                &statement.observed_at,
                "",
                "",
                &dev,
                &asset.wallet,
                &chain,
                "",
                "",
                "",
                &slot,
                &asset.ticker,
                &asset.issuer,
                &asset.contract,
                &asset.balance,
                &decimals,
                &issuer_match,
                "",
            ],
        );
    }
    csv
}

fn csv_row(output: &mut String, fields: &[&str]) {
    for (index, field) in fields.iter().enumerate() {
        if index > 0 {
            output.push(',');
        }
        output.push('"');
        if csv_formula_injection(field) {
            output.push('\'');
        }
        for character in field.chars() {
            if character == '"' {
                output.push_str("\"\"");
            } else {
                output.push(character);
            }
        }
        output.push('"');
    }
    output.push_str("\r\n");
}

fn csv_formula_injection(field: &str) -> bool {
    field
        .trim_start_matches(|character: char| {
            character.is_whitespace() && !matches!(character, '\t' | '\r')
        })
        .chars()
        .next()
        .is_some_and(|character| matches!(character, '=' | '+' | '-' | '@' | '\t' | '\r'))
}

pub(crate) async fn recheck_statement(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Redirect, StatusCode> {
    use std::collections::BTreeSet;

    let previous =
        crate::app::statement::get(&state.app, &id).await.ok_or(StatusCode::NOT_FOUND)?;
    let wallets = previous
        .wallets
        .iter()
        .map(|wallet| wallet.address.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let chains = previous
        .wallets
        .iter()
        .map(|wallet| super::views::chain_slug(wallet.chain).to_owned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let label = if previous.label.trim().is_empty() {
        let short_id = previous.id.chars().take(8).collect::<String>();
        format!("Re-run of {short_id}")
    } else {
        let prefix = previous.label.trim().chars().take(71).collect::<String>();
        format!("{prefix} (re-run)")
    };
    let request = crate::app::statement::StatementRequest { label, wallets, chains, block: None };
    let axum::Json(current) = super::api_statement(State(state), axum::Json(request))
        .await
        .map_err(|(status, _)| status)?;
    Ok(Redirect::to(&format!("/statements/{}?compare={}", current.id, previous.id)))
}
fn statement_asset_position_html(asset: &crate::domain::statement::StatementAsset) -> String {
    use std::fmt::Write as _;

    let mut facts = String::with_capacity(96);
    if let Some(slot) = asset.slot {
        write!(&mut facts, "Asset slot: {slot}").expect("writing into a String cannot fail");
    }
    if let Some(block) = asset.powers_block {
        if !facts.is_empty() {
            facts.push_str("<br>");
        }
        if block == 0 {
            facts.push_str("Power block: not observed");
        } else {
            write!(&mut facts, "Power block: {block}").expect("writing into a String cannot fail");
        }
    }
    if let Some(slot) = asset.powers_slot {
        if !facts.is_empty() {
            facts.push_str("<br>");
        }
        write!(&mut facts, "Power slot: {slot}").expect("writing into a String cannot fail");
    }
    facts
}

fn statement_position_html(position: &crate::domain::statement::StatementPosition) -> String {
    use std::fmt::Write as _;

    let mut row = String::with_capacity(position.wallet.len().saturating_add(96));
    let chain = position.chain.to_string();
    write!(
        &mut row,
        "<li><strong>{}</strong> for <code>{}</code>: ",
        html_escape(&chain),
        html_escape(&position.wallet),
    )
    .expect("writing into a String cannot fail");
    let block = position.block.filter(|block| *block != 0);
    match (block, position.min_slot, position.max_slot) {
        (Some(block), Some(min_slot), Some(max_slot)) => {
            write!(&mut row, "block {block}, slots {min_slot}–{max_slot}")
        }
        (Some(block), Some(slot), None) => write!(&mut row, "block {block}, minimum slot {slot}"),
        (Some(block), None, Some(slot)) => write!(&mut row, "block {block}, maximum slot {slot}"),
        (Some(block), None, None) => write!(&mut row, "block {block}"),
        (None, Some(min_slot), Some(max_slot)) if position.block == Some(0) => {
            write!(&mut row, "block not observed; slots {min_slot}–{max_slot}")
        }
        (None, Some(min_slot), Some(max_slot)) => {
            write!(&mut row, "slots {min_slot}–{max_slot}")
        }
        (None, Some(slot), None) if position.block == Some(0) => {
            write!(&mut row, "block not observed; minimum slot {slot}")
        }
        (None, Some(slot), None) => write!(&mut row, "minimum slot {slot}"),
        (None, None, Some(slot)) if position.block == Some(0) => {
            write!(&mut row, "block not observed; maximum slot {slot}")
        }
        (None, None, Some(slot)) => write!(&mut row, "maximum slot {slot}"),
        (None, None, None) if position.block == Some(0) => write!(&mut row, "not observed"),
        (None, None, None) => write!(&mut row, "no block or slot recorded"),
    }
    .expect("writing into a String cannot fail");
    row.push_str("</li>");
    row
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

pub(crate) async fn certificate(
    State(state): State<AppState>,
    Path(id): Path<String>,
    _headers: HeaderMap,
) -> Result<Html<String>, StatusCode> {
    let Some(attestation) = attest::get_certificate_async(&state.app, &id).await else {
        return Err(StatusCode::NOT_FOUND);
    };
    render_page(
        CertificateTemplate {
            asset_version: ASSET_VERSION,
            public_url: state.public_url.to_string(),
            certificate: CertificateView::from_attestation(attestation),
        },
        false,
    )
}

pub(crate) async fn verify_certificate_page(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Html<String>, StatusCode> {
    let attestation =
        attest::get_certificate_async(&state.app, &id).await.ok_or(StatusCode::NOT_FOUND)?;
    let result = super::api::verify_attestation_document(&state, attestation.clone());
    let cryptographic = result["cryptographic"].as_bool().unwrap_or(false);
    let trusted = result["trusted_signer"].as_bool().unwrap_or(false);
    let environment = result["environment_match"].as_bool().unwrap_or(false);
    let fresh = result["fresh"].as_bool().unwrap_or(false);
    let summary = if !cryptographic {
        "The certificate signature or content hash does not verify; do not rely on this record."
    } else if !trusted {
        "The signature verifies, but this signing key is not trusted by this QED instance."
    } else if !environment {
        "The signature verifies, but this record was signed for a different QED environment."
    } else if fresh {
        "The certificate hash and Ed25519 signature verify, the signer and environment match, and its observation is within the recorded expiry interval."
    } else {
        "The certificate is a valid historical record, but it is outside its recorded expiry interval. Re-check to create a fresh observation before relying on current facts."
    };
    let id = html_escape(&attestation.id);
    let verification_json = serde_json::to_string_pretty(&result)
        .map(|value| html_escape(&value))
        .unwrap_or_else(|_| "{}".to_owned());
    let body = format!(
        "<section class=\"lookup certificate-verification\"><p>{}</p><dl><dt>Content hash and signature</dt><dd>{}</dd><dt>Trusted signer</dt><dd>{}</dd><dt>Environment match</dt><dd>{}</dd><dt>Fresh</dt><dd>{}</dd></dl><p>Fresh means the check time is not in the future and the recorded expiry has not passed. It describes the record's time window, not current issuer-registry or chain state.</p><p>QED public verification key: <a href=\"/.well-known/qed.json\">/.well-known/qed.json</a></p><p><a class=\"button secondary-button\" href=\"/v/{id}\">Back to certificate</a> <a class=\"button secondary-button\" href=\"/api/attest/{id}\" download>Download JSON</a></p><details><summary>Verification response</summary><pre><code>{verification_json}</code></pre></details></section>",
        html_escape(summary),
        cryptographic,
        trusted,
        environment,
        fresh
    );
    content_page(
        &state,
        PageMetadata {
            page_title: "Certificate verification",
            heading: "Verify this record.",
            eyebrow: "CERTIFICATE",
            intro: summary,
            meta_description: "Check a QED certificate signature, trusted signer, environment, and freshness.",
            canonical_path: &format!("/v/{id}/verify"),
        },
        body,
    )
}

pub(crate) async fn recheck_certificate(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<RecheckQuery>,
) -> Result<Html<String>, StatusCode> {
    let recheck = attest::recheck(&state.app, &id).await;
    let selected = if recheck.fresh_id.is_empty() { id } else { recheck.fresh_id.clone() };
    let Some(attestation) = attest::get_async(&state.app, &selected).await else {
        return Err(StatusCode::NOT_FOUND);
    };
    let mut certificate = if query.is_live() {
        CertificateView::from_live_attestation(attestation)
    } else {
        CertificateView::from_attestation(attestation)
    };
    certificate.notice = if recheck.equal {
        "Re-check matched every recorded read.".to_owned()
    } else if recheck.changed.is_empty() {
        "Re-check completed with a new certificate.".to_owned()
    } else {
        format!("Changed: {}.", recheck.changed.join(", "))
    };
    CertificateResultTemplate { certificate }
        .render()
        .map(|html| Html(super::format_html_timestamps(&html)))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::pool::PoolError;

    /// The powers warm interval offset by `minutes` (positive = still fresh, negative = due).
    fn warm_interval_minus(minutes: i64) -> chrono::Duration {
        chrono::Duration::from_std(crate::app::warm::POWERS_WARM_INTERVAL).expect("interval")
            - chrono::Duration::minutes(minutes)
    }
    #[test]
    fn stats_flagged_rows_render_complete_fact_reason_totals_and_native_details() {
        let state = AppState::for_tests(Vec::new(), Vec::new(), true);
        let registry = vec![crate::domain::registry::Entry {
            issuer: "Backed xStocks".to_owned(),
            ticker: "NVDA".to_owned(),
            name: "NVIDIA".to_owned(),
            chain: Chain::Base,
            contract: "0x0000000000000000000000000000000000000002".to_owned(),
            decimals: Some(18),
            source: "test".to_owned(),
            source_url: "https://issuer.example".to_owned(),
            last_checked: "2026-10-07T00:00:00Z".to_owned(),
            removed_at: None,
            stale_since: None,
            official_deployments: vec![crate::domain::registry::OfficialDeployment {
                network: "Base".to_owned(),
                address: "0x0000000000000000000000000000000000000002".to_owned(),
                wrapper_address: None,
                wrapper_address_v2: None,
            }],
        }];
        let entry = crate::adapters::discovery::ImpostorEntry {
            chain: "base".to_owned(),
            chain_label: "Base".to_owned(),
            ticker: "NVDA".to_owned(),
            publisher: "Backed xStocks".to_owned(),
            symbol: "WRONG-LISTING-SYMBOL".to_owned(),
            name: "Wrong Market Listing".to_owned(),
            address: "0x0000000000000000000000000000000000000001".to_owned(),
            first_seen_at: "2026-10-06T01:02:00Z".to_owned(),
            last_seen_at: "2026-10-07T03:04:00Z".to_owned(),
            tier: crate::domain::guard::PublisherProductTier::Confirmed,
            volume_24h_usd: Some(1234.5),
            liquidity_24h_usd: Some(456.78),
            pool_count: Some(2),
            source: crate::adapters::discovery::MarketSource::Geckoterminal,
            guard_url: "/guard/base/0x0000000000000000000000000000000000000001".to_owned(),
            reason: "truncated reason".to_owned(),
            reads: Vec::new(),
            on_chain_symbol: Some("NVDA".to_owned()),
            on_chain_name: Some("NVIDIA xStock".to_owned()),
            publisher_catalog_snapshot_hash: Some("a".repeat(64)),
            evidence_truncated: false,
            guard_document: None,
        };
        let mut board = crate::adapters::discovery::Leaderboard::default();
        board.entries.push(crate::adapters::discovery::LeaderboardEntry {
            rank: 1,
            chain: "base".to_owned(),
            chain_label: "Base".to_owned(),
            dex: "uniswap-v3".to_owned(),
            pool: "0xpool".to_owned(),
            base_symbol: "NVDA".to_owned(),
            quote_symbol: "USDC".to_owned(),
            source: crate::adapters::discovery::MarketSource::Dexscreener,
            issuer: Some("Backed xStocks".to_owned()),
            ticker: Some("NVDA".to_owned()),
            issuer_on_base: Some(true),
            verdict: "verified".to_owned(),
            read_status: "checked".to_owned(),
            read_reason: None,
            price_usd: None,
            change_24h_pct: None,
            volume_24h_usd: Some(300.0),
            liquidity_usd: None,
            txns_24h: None,
            detail_url: "/validated/base/0xpool".to_owned(),
            trade_url: "https://dexscreener.com/base/0xpool".to_owned(),
            explorer_url: "https://basescan.org/address/0xpool".to_owned(),
            attestation_id: None,
            checked_at: None,
        });
        let stats = super::super::api::leaderboard_stats(&board, &registry);
        let rendered = render_stats_page(&state, &stats, &[&entry], &board, &registry)
            .expect("stats page renders");
        let rendered = String::from_utf8(rendered.to_vec()).expect("stats HTML is UTF-8");

        assert!(rendered.contains("on-chain symbol “NVDA” and name “NVIDIA xStock”"));
        assert!(rendered.contains("Listed as Wrong Market Listing"));
        assert!(rendered.contains("WRONG-LISTING-SYMBOL · reported by GeckoTerminal"));
        assert!(!rendered.contains("on-chain symbol “WRONG-LISTING-SYMBOL”"));
        assert!(rendered.contains("exactly match the NVDA product published by Backed xStocks"));
        assert!(rendered.contains("official deployments on Base"));
        assert!(rendered.contains("<h3>Confirmed mismatches</h3>"));
        assert!(rendered.contains("<h3>Caution — near matches</h3>"));
        assert_eq!(rendered.matches("class=\"stats-flagged-total\"").count(), 1);
        assert!(rendered.contains(
            "Reported 24h volume across all flagged tokens: $1,234.5 (DexScreener/GeckoTerminal, not verified)"
        ));
        assert!(rendered.contains(
            "On-chain symbol and name exactly match an issuer product; the address is absent from the issuer's catalog."
        ));
        assert!(
            rendered
                .contains("Similar to an issuer product, but QED could not confirm a mismatch.")
        );
        assert!(
            rendered.contains(
                "Total reported 24h volume (1 tokens): $1,234.5 (DexScreener/GeckoTerminal"
            )
        );
        assert!(!rendered.contains("Total reported 24h volume (1 tokens): $1,234.5. "));
        assert!(rendered.contains("$1.2k reported 24h volume"));
        assert!(
            rendered.contains("<details class=\"stats-row-details\"><summary>Details</summary>")
        );
        assert!(rendered.contains("0x0000000000000000000000000000000000000001</code>"));
        assert!(rendered.contains("2026-10-06T01:02:00Z\">6 Oct 2026, 01:02 UTC"));
        assert!(rendered.contains("2026-10-07T03:04:00Z\">7 Oct 2026, 03:04 UTC"));
        assert!(rendered.contains(
            "role=\"img\" aria-label=\"Deny — claims a product its publisher does not list\""
        ));
        assert!(rendered.contains("official publisher pool volume $300"));
        assert!(rendered.contains("Liquidity:</strong> $456.78 reported by GeckoTerminal"));
        assert!(rendered.contains("Pool count:</strong> 2 reported in this observation"));
        assert!(!rendered.contains("same scan"));
        let mut no_metrics = entry.clone();
        no_metrics.liquidity_24h_usd = None;
        no_metrics.pool_count = None;
        let no_metrics_row = render_flagged_row(&no_metrics, &registry, &board.entries);
        assert!(no_metrics_row.contains("Liquidity:</strong> Not retained in this observation"));
        assert!(no_metrics_row.contains("Pool count:</strong> Not retained in this observation"));
        let mut missing_facts = entry.clone();
        missing_facts.on_chain_symbol = None;
        missing_facts.on_chain_name = None;
        missing_facts.reason = "x".repeat(crate::adapters::discovery::MAX_IMPOSTOR_REASON_BYTES);
        let fallback_row = render_flagged_row(&missing_facts, &registry, &board.entries);
        assert!(fallback_row.contains(&format!(
            "Why flagged:</strong> {}…",
            "x".repeat(crate::adapters::discovery::MAX_IMPOSTOR_REASON_BYTES)
        )));
        assert!(!rendered.contains("<details class=\"stats-row-details\" open"));
        assert!(!rendered.contains("truncated reason"));
    }

    struct GatedPowerReader {
        started: std::sync::Arc<tokio::sync::Notify>,
        release: std::sync::Arc<tokio::sync::Notify>,
        facts: crate::domain::powers::PowerFacts,
    }

    #[async_trait::async_trait]
    impl crate::ports::ChainReader for GatedPowerReader {
        fn chain(&self) -> Chain {
            Chain::Solana
        }

        async fn read_pool(
            &self,
            _address: &str,
        ) -> Result<crate::domain::pool::PoolInfo, PoolError> {
            Err(PoolError::Reader("unused token-page test reader".to_owned()))
        }

        async fn power_facts(
            &self,
            _address: &str,
        ) -> Result<crate::domain::powers::PowerFacts, PoolError> {
            self.started.notify_one();
            self.release.notified().await;
            Ok(self.facts.clone())
        }
    }
    struct CountingPowerReader {
        calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl crate::ports::ChainReader for CountingPowerReader {
        fn chain(&self) -> Chain {
            Chain::Solana
        }

        async fn read_pool(
            &self,
            _address: &str,
        ) -> Result<crate::domain::pool::PoolInfo, PoolError> {
            Err(PoolError::Reader("unused token-page test reader".to_owned()))
        }

        async fn power_facts(
            &self,
            _address: &str,
        ) -> Result<crate::domain::powers::PowerFacts, PoolError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(crate::domain::powers::PowerFacts::default())
        }
    }
    struct IncompletePowerReader {
        calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl crate::ports::ChainReader for IncompletePowerReader {
        fn chain(&self) -> Chain {
            Chain::Solana
        }

        async fn read_pool(
            &self,
            _address: &str,
        ) -> Result<crate::domain::pool::PoolInfo, PoolError> {
            Err(PoolError::Reader("unused token-page test reader".to_owned()))
        }

        async fn power_facts(
            &self,
            _address: &str,
        ) -> Result<crate::domain::powers::PowerFacts, PoolError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(crate::domain::powers::PowerFacts {
                unavailable: vec![crate::domain::powers::Reason::new(
                    "extension_unreadable",
                    "A token extension could not be decoded.",
                )],
                ..Default::default()
            })
        }
    }

    struct TransientPowerReader;

    #[async_trait::async_trait]
    impl crate::ports::ChainReader for TransientPowerReader {
        fn chain(&self) -> Chain {
            Chain::Solana
        }

        async fn read_pool(
            &self,
            _address: &str,
        ) -> Result<crate::domain::pool::PoolInfo, PoolError> {
            Err(PoolError::Reader("unused token-page test reader".to_owned()))
        }

        async fn power_facts(
            &self,
            _address: &str,
        ) -> Result<crate::domain::powers::PowerFacts, PoolError> {
            let mut facts =
                crate::domain::powers::PowerFacts { transient_failure: true, ..Default::default() };
            facts
                .unavailable
                .push(crate::domain::powers::Reason::new("rpc_timeout", "RPC timed out."));
            Ok(facts)
        }
    }

    fn token_page_state(
        reader: Box<dyn crate::ports::ChainReader>,
        entry: registry::Entry,
    ) -> AppState {
        AppState::for_tests(vec![entry], vec![reader], false)
    }

    #[tokio::test]
    async fn token_page_returns_transient_at_deadline_while_prefetch_continues() {
        use axum::{body::Body, http::Request};
        use tower::ServiceExt;

        let address = "11111111111111111111111111111111";
        let started = std::sync::Arc::new(tokio::sync::Notify::new());
        let release = std::sync::Arc::new(tokio::sync::Notify::new());
        let state = token_page_state(
            Box::new(GatedPowerReader {
                started: started.clone(),
                release: release.clone(),
                facts: crate::domain::powers::PowerFacts::default(),
            }),
            registry::Entry {
                issuer: "Issuer".to_owned(),
                ticker: "NVDA".to_owned(),
                name: "Issuer NVDA".to_owned(),
                chain: Chain::Solana,
                contract: address.to_owned(),
                decimals: Some(9),
                source: "test".to_owned(),
                source_url: "https://issuer.example/nvda".to_owned(),
                last_checked: registry_adapter::now_rfc3339(),
                removed_at: None,
                stale_since: None,
                official_deployments: Vec::new(),
            },
        );
        let response = tokio::time::timeout(
            POWERS_PAGE_DEADLINE + Duration::from_secs(1),
            crate::adapters::web::router(state.clone())
                .oneshot(Request::get("/tokens/NVDA").body(Body::empty()).unwrap()),
        )
        .await
        .expect("token page returns by its power deadline")
        .expect("token page response");
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            state.app.history.states().is_empty(),
            "a page deadline is not a completed observation"
        );

        tokio::time::timeout(std::time::Duration::from_secs(1), started.notified())
            .await
            .expect("cache miss schedules background power read");
        release.notify_one();
        let key = (Chain::Solana, address.to_owned());
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if state.app.powers_cache.get(&key).await.is_some() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("background observation enters complete powers cache");
        let states = state.app.history.states();
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].1, crate::domain::asset_history::Facet::Powers);
        assert_eq!(states[0].2.consecutive_misses, 0);
    }
    #[tokio::test]
    async fn token_page_renders_power_badges_when_read_finishes_before_deadline() {
        use axum::{body::Body, http::Request};
        use tower::ServiceExt;

        let address = "11111111111111111111111111111111";
        let started = std::sync::Arc::new(tokio::sync::Notify::new());
        let release = std::sync::Arc::new(tokio::sync::Notify::new());
        let mut facts = crate::domain::powers::PowerFacts::default();
        facts.can_block.push(crate::domain::powers::Reason::new(
            "freeze_authority",
            "Freeze authority can block transfers.",
        ));
        let state = token_page_state(
            Box::new(GatedPowerReader {
                started: started.clone(),
                release: release.clone(),
                facts,
            }),
            registry::Entry {
                issuer: "Issuer".to_owned(),
                ticker: "NVDA".to_owned(),
                name: "Issuer NVDA".to_owned(),
                chain: Chain::Solana,
                contract: address.to_owned(),
                decimals: Some(9),
                source: "test".to_owned(),
                source_url: "https://issuer.example/nvda".to_owned(),
                last_checked: registry_adapter::now_rfc3339(),
                removed_at: None,
                stale_since: None,
                official_deployments: Vec::new(),
            },
        );
        let request = tokio::spawn(
            crate::adapters::web::router(state)
                .oneshot(Request::get("/tokens/NVDA").body(Body::empty()).unwrap()),
        );
        tokio::time::timeout(Duration::from_secs(1), started.notified())
            .await
            .expect("power read starts");
        release.notify_one();
        let response = tokio::time::timeout(POWERS_PAGE_DEADLINE, request)
            .await
            .expect("read completes within the page deadline")
            .expect("token request task")
            .expect("token page response");
        assert_eq!(response.status(), StatusCode::OK);
        let body =
            axum::body::to_bytes(response.into_body(), 1024 * 1024).await.expect("token page body");
        let body = String::from_utf8_lossy(&body);
        assert!(body.contains("Can block"));
        assert!(body.contains("Source unavailable"));
        assert!(!body.contains("Signals unavailable (transient)"));
    }

    #[tokio::test]
    async fn complete_power_facts_are_reused_by_user_inspections() {
        let address = "11111111111111111111111111111111";
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let state = token_page_state(
            Box::new(CountingPowerReader { calls: calls.clone() }),
            registry::Entry {
                issuer: "Issuer".to_owned(),
                ticker: "NVDA".to_owned(),
                name: "Issuer NVDA".to_owned(),
                chain: Chain::Solana,
                contract: address.to_owned(),
                decimals: Some(9),
                source: "test".to_owned(),
                source_url: "https://issuer.example/nvda".to_owned(),
                last_checked: registry_adapter::now_rfc3339(),
                removed_at: None,
                stale_since: None,
                official_deployments: Vec::new(),
            },
        );
        let key = (Chain::Solana, address.to_owned());

        let record = crate::app::powers::inspect(&state.app, Chain::Solana, address)
            .await
            .expect("complete observation");
        assert!(record.unavailable.is_empty());
        assert_eq!(record.source_verified, crate::domain::powers::SourceVerified::Unavailable);
        assert!(state.app.powers_cache.get(&key).await.is_some());
        assert!(state.app.powers_retry_cache.get(&key).await.is_none());

        let cached = crate::app::powers::inspect(&state.app, Chain::Solana, address)
            .await
            .expect("cached complete observation");
        assert_eq!(cached.observed_at, record.observed_at);
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn incomplete_power_facts_use_only_the_retry_cache() {
        let address = "11111111111111111111111111111111";
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let state = token_page_state(
            Box::new(IncompletePowerReader { calls: calls.clone() }),
            registry::Entry {
                issuer: "Issuer".to_owned(),
                ticker: "NVDA".to_owned(),
                name: "Issuer NVDA".to_owned(),
                chain: Chain::Solana,
                contract: address.to_owned(),
                decimals: Some(9),
                source: "test".to_owned(),
                source_url: "https://issuer.example/nvda".to_owned(),
                last_checked: registry_adapter::now_rfc3339(),
                removed_at: None,
                stale_since: None,
                official_deployments: Vec::new(),
            },
        );
        let key = (Chain::Solana, address.to_owned());

        let record = crate::app::powers::inspect(&state.app, Chain::Solana, address)
            .await
            .expect("incomplete observation");
        assert_eq!(record.unavailable[0].code, "extension_unreadable");
        assert!(state.app.powers_cache.get(&key).await.is_none());
        assert!(state.app.powers_retry_cache.get(&key).await.is_some());

        let retry_cached = crate::app::powers::inspect(&state.app, Chain::Solana, address)
            .await
            .expect("short retry-cache observation");
        assert_eq!(retry_cached.observed_at, record.observed_at);
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn queued_power_prefetch_does_not_occupy_user_check_capacity() {
        let address = "11111111111111111111111111111111";
        let state = token_page_state(
            Box::new(GatedPowerReader {
                started: std::sync::Arc::new(tokio::sync::Notify::new()),
                release: std::sync::Arc::new(tokio::sync::Notify::new()),
                facts: crate::domain::powers::PowerFacts::default(),
            }),
            registry::Entry {
                issuer: "Issuer".to_owned(),
                ticker: "NVDA".to_owned(),
                name: "Issuer NVDA".to_owned(),
                chain: Chain::Solana,
                contract: address.to_owned(),
                decimals: Some(9),
                source: "test".to_owned(),
                source_url: "https://issuer.example/nvda".to_owned(),
                last_checked: registry_adapter::now_rfc3339(),
                removed_at: None,
                stale_since: None,
                official_deployments: Vec::new(),
            },
        );
        let _prefetch_permits = [
            state.app.powers_prefetch_concurrency.clone().acquire_owned().await.unwrap(),
            state.app.powers_prefetch_concurrency.clone().acquire_owned().await.unwrap(),
        ];

        crate::app::powers::schedule_prefetch(&state.app, Chain::Solana, address).await;
        let key = (Chain::Solana, address.to_owned());
        assert!(state.app.powers_prefetching.lock().await.contains(&key));
        let _user_check_permit = state
            .expensive_concurrency
            .clone()
            .try_acquire_owned()
            .expect("queued prefetch leaves the user-check permit available");
    }

    #[tokio::test]
    async fn warm_pass_refreshes_stale_current_tickers_and_skips_fresh_ones() {
        let nvda = "11111111111111111111111111111111";
        let tsla = "So11111111111111111111111111111111111111112";
        let gme = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
        let make_entry = |ticker: &str, contract: &str| registry::Entry {
            issuer: "Issuer".to_owned(),
            ticker: ticker.to_owned(),
            name: format!("Issuer {ticker}"),
            chain: Chain::Solana,
            contract: contract.to_owned(),
            decimals: Some(9),
            source: "test".to_owned(),
            source_url: "https://issuer.example/token".to_owned(),
            last_checked: registry_adapter::now_rfc3339(),
            removed_at: None,
            stale_since: None,
            official_deployments: Vec::new(),
        };
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let state = token_page_state(
            Box::new(CountingPowerReader { calls: calls.clone() }),
            make_entry("NVDA", nvda),
        );
        *state.registry.write().await = std::sync::Arc::new(vec![
            make_entry("NVDA", nvda),
            make_entry("TSLA", tsla),
            make_entry("GME", gme),
        ]);

        let mut leaderboard = crate::adapters::discovery::Leaderboard::default();
        leaderboard.entries.push(crate::adapters::discovery::LeaderboardEntry {
            rank: 1,
            chain: "solana".to_owned(),
            chain_label: "Solana".to_owned(),
            dex: "raydium".to_owned(),
            pool: "leaderboard-pool".to_owned(),
            base_symbol: "NVDA".to_owned(),
            quote_symbol: "USDC".to_owned(),
            source: crate::adapters::discovery::MarketSource::Dexscreener,
            issuer: Some("Issuer".to_owned()),
            ticker: Some("NVDA".to_owned()),
            issuer_on_base: Some(true),
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
            explorer_url: "https://solscan.io/account/pool".to_owned(),
            attestation_id: None,
            checked_at: None,
        });
        *state.leaderboard.write().await = leaderboard;
        *state.featured.write().await = vec![crate::adapters::discovery::FeaturedPool {
            chain: Chain::Solana,
            dex: "raydium".to_owned(),
            pool: "featured-pool".to_owned(),
            base_symbol: "TSLA".to_owned(),
            base_address: tsla.to_owned(),
            quote_symbol: "USDC".to_owned(),
            quote_address: "11111111111111111111111111111111".to_owned(),
            issuer: Some("Issuer".to_owned()),
            ticker: Some("TSLA".to_owned()),
            verdict: "verified".to_owned(),
            quote_balance: None,
            quote_share_of_supply: None,
            volume_24h_usd: None,
            liquidity_usd: None,
            curated: false,
            note: None,
            updated_at: registry_adapter::now_rfc3339(),
        }];

        let mut cached = powers_record(crate::domain::powers::SourceVerified::Match);
        cached.chain = Chain::Solana;
        cached.contract = nvda.to_owned();
        cached.observed_at = (chrono::Utc::now() - warm_interval_minus(1)).to_rfc3339();
        state.app.powers_cache.insert((Chain::Solana, nvda.to_owned()), cached).await;
        let mut stale_cached = powers_record(crate::domain::powers::SourceVerified::Match);
        stale_cached.chain = Chain::Solana;
        stale_cached.contract = tsla.to_owned();
        stale_cached.observed_at = (chrono::Utc::now() - warm_interval_minus(-1)).to_rfc3339();
        state.app.powers_cache.insert((Chain::Solana, tsla.to_owned()), stale_cached).await;

        let summary = crate::app::warm::warm_current_pool_powers(&state.app).await;
        assert!(summary.warmed);
        assert_eq!(summary.target_count, 2);
        assert_eq!(summary.tickers_covered, 2);
        assert!(!summary.cap_hit);
        assert_eq!(summary.skipped, 1);
        assert_eq!(summary.transient, 0);
        assert_eq!(summary.ok, 1);
        assert_eq!(summary.by_chain[0].ok, 1);
        assert_eq!(summary.by_chain[0].transient, 0);
        assert_eq!(summary.by_chain[0].source_unavailable, 1);
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1);
        let refreshed = state
            .app
            .powers_cache
            .get(&(Chain::Solana, tsla.to_owned()))
            .await
            .expect("stale cached record was re-inspected");
        let observed_at = chrono::DateTime::parse_from_rfc3339(&refreshed.observed_at)
            .expect("refreshed observation timestamp")
            .with_timezone(&chrono::Utc);
        assert!(
            chrono::Utc::now().signed_duration_since(observed_at) < chrono::Duration::minutes(1)
        );
        assert!(state.app.powers_cache.get(&(Chain::Solana, gme.to_owned())).await.is_none());
    }

    #[tokio::test]
    async fn partial_controls_are_current_and_prior_controls_are_separately_dated() {
        let address = "11111111111111111111111111111111";
        let state = token_page_state(
            Box::new(TransientPowerReader),
            registry::Entry {
                issuer: "Issuer".to_owned(),
                ticker: "NVDA".to_owned(),
                name: "Issuer NVDA".to_owned(),
                chain: Chain::Solana,
                contract: address.to_owned(),
                decimals: Some(9),
                source: "test".to_owned(),
                source_url: "https://issuer.example/nvda".to_owned(),
                last_checked: registry_adapter::now_rfc3339(),
                removed_at: None,
                stale_since: None,
                official_deployments: Vec::new(),
            },
        );
        *state.featured.write().await = vec![crate::adapters::discovery::FeaturedPool {
            chain: Chain::Solana,
            dex: "raydium".to_owned(),
            pool: "featured-pool".to_owned(),
            base_symbol: "NVDA".to_owned(),
            base_address: address.to_owned(),
            quote_symbol: "USDC".to_owned(),
            quote_address: "So11111111111111111111111111111111111111112".to_owned(),
            issuer: Some("Issuer".to_owned()),
            ticker: Some("NVDA".to_owned()),
            verdict: "verified".to_owned(),
            quote_balance: None,
            quote_share_of_supply: None,
            volume_24h_usd: None,
            liquidity_usd: None,
            curated: false,
            note: None,
            updated_at: registry_adapter::now_rfc3339(),
        }];

        let mut cached = powers_record(crate::domain::powers::SourceVerified::Match);
        cached.chain = Chain::Solana;
        cached.contract = address.to_owned();
        cached.source_verified_subject = crate::domain::powers::SourceVerifiedSubject::TokenProgram;
        cached.can_seize.push(crate::domain::powers::Reason::new(
            "permanent_delegate",
            "Permanent delegate can transfer or burn tokens.",
        ));
        cached.observed_at = (chrono::Utc::now() - warm_interval_minus(-1)).to_rfc3339();
        let cached_time = cached.observed_at.clone();
        let key = (Chain::Solana, address.to_owned());
        state.app.powers_cache.insert(key.clone(), cached).await;

        let summary = crate::app::warm::warm_current_pool_powers(&state.app).await;
        assert_eq!(summary.transient, 1);
        assert_eq!(summary.skipped, 0);
        assert_eq!(summary.ok, 0);
        let retained = state
            .app
            .powers_cache
            .get(&key)
            .await
            .expect("prior complete observation remains cached");
        assert_eq!(retained.observed_at, cached_time);
        assert_eq!(retained.can_seize[0].code, "permanent_delegate");

        let partial =
            state.app.powers_retry_cache.get(&key).await.expect("latest partial observation");
        let view = display_powers(&state.app, retained).await;
        assert_eq!(view.observed_at, partial.observed_at);
        assert!(
            view.can_seize.is_empty(),
            "prior seizure capability is not relabelled as a current partial fact"
        );
        assert_eq!(view.last_confirmed_at, cached_time);
        assert!(
            view.last_confirmed_signals.contains("permanent_delegate"),
            "prior facts remain inspectable alongside their original time"
        );
    }

    #[tokio::test]
    async fn warm_notification_triggers_when_first_leaderboard_ticker_loads() {
        let address = "11111111111111111111111111111111";
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let state = token_page_state(
            Box::new(CountingPowerReader { calls: calls.clone() }),
            registry::Entry {
                issuer: "Issuer".to_owned(),
                ticker: "NVDA".to_owned(),
                name: "NVIDIA".to_owned(),
                chain: Chain::Solana,
                contract: address.to_owned(),
                decimals: Some(9),
                source: "test".to_owned(),
                source_url: "https://issuer.example/nvda".to_owned(),
                last_checked: registry_adapter::now_rfc3339(),
                removed_at: None,
                stale_since: None,
                official_deployments: Vec::new(),
            },
        );
        let notify = std::sync::Arc::new(tokio::sync::Notify::new());
        assert!(!crate::app::warm::notify_powers_warm_if_targets(&state.app, &notify).await);
        let empty_summary = crate::app::warm::warm_current_pool_powers(&state.app).await;
        assert!(!empty_summary.warmed);
        assert_eq!(empty_summary.target_count, 0);

        let warm_state = state.clone();
        let warm_notify = notify.clone();
        let warm_task = tokio::spawn(async move {
            warm_notify.notified().await;
            crate::app::warm::warm_current_pool_powers(&warm_state.app).await
        });
        let mut leaderboard = crate::adapters::discovery::Leaderboard::default();
        leaderboard.entries.push(crate::adapters::discovery::LeaderboardEntry {
            rank: 1,
            chain: "solana".to_owned(),
            chain_label: "Solana".to_owned(),
            dex: "raydium".to_owned(),
            pool: "leaderboard-pool".to_owned(),
            base_symbol: "NVDA".to_owned(),
            quote_symbol: "USDC".to_owned(),
            issuer: Some("Issuer".to_owned()),
            source: crate::adapters::discovery::MarketSource::Dexscreener,
            ticker: Some("NVDA".to_owned()),
            issuer_on_base: Some(true),
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
            explorer_url: "https://solscan.io/account/pool".to_owned(),
            attestation_id: None,
            checked_at: None,
        });
        *state.leaderboard.write().await = leaderboard;
        assert!(crate::app::warm::notify_powers_warm_if_targets(&state.app, &notify).await);

        let summary = tokio::time::timeout(Duration::from_secs(1), warm_task)
            .await
            .expect("first-board notification starts warm pass")
            .expect("warm task");
        assert!(summary.warmed);
        assert_eq!(summary.target_count, 1);
        assert_eq!(summary.ok, 1);
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    #[test]
    fn wallet_address_routing_keeps_solana_distinct_and_checks_all_evm_chains() {
        assert_eq!(wallet_chains("11111111111111111111111111111111"), Some(vec![Chain::Solana]));
        assert_eq!(
            wallet_chains("0x0000000000000000000000000000000000000001"),
            Some(vec![Chain::RobinhoodChain, Chain::Base, Chain::Ethereum, Chain::Bnb])
        );
        assert_eq!(wallet_chains("not-an-address"), None);
    }

    #[test]
    fn wallet_amount_formatting_preserves_decimal_precision() {
        assert_eq!(format_wallet_amount("1234500", Some(4)), "123.45");
        assert_eq!(format_wallet_amount("7", Some(3)), "0.007");
        assert_eq!(format_wallet_amount("7", None), "7");
    }

    #[test]
    fn ticker_lookup_returns_canonical_active_issuer_ticker() {
        let entry = registry::Entry {
            issuer: "Backed".to_owned(),
            ticker: "NVDA".to_owned(),
            name: "Backed NVIDIA".to_owned(),
            chain: Chain::Ethereum,
            contract: "0xc845b2894dBddd03858fd2D643B4eF725fE0849d".to_owned(),
            decimals: Some(18),
            source: "xstocks".to_owned(),
            source_url: "https://issuer.example/nvda".to_owned(),
            last_checked: "2026-01-01T00:00:00Z".to_owned(),
            removed_at: None,
            stale_since: None,
            official_deployments: Vec::new(),
        };
        assert_eq!(canonical_ticker(&vec![entry.clone()], " nvda "), Some("NVDA".to_owned()));
        let response =
            token_redirect(&vec![entry.clone()], Some(" nvda ")).unwrap().into_response();
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(response.headers().get("location").unwrap(), "/tokens/NVDA");
        assert!(matches!(
            token_redirect(&vec![entry.clone()], Some("unknown")),
            Err(StatusCode::NOT_FOUND)
        ));

        let mut stale = entry;
        stale.stale_since = Some("2026-01-02T00:00:00Z".to_owned());
        assert_eq!(canonical_ticker(&vec![stale], "NVDA"), None);
        assert_eq!(canonical_ticker(&Vec::new(), "NVDA"), None);
    }

    #[test]
    fn token_directory_contracts_have_intentional_chain_order() {
        let view = |chain: &str, issuer: &str| DirectoryContractView {
            issuer: issuer.to_owned(),
            ticker: "NVDA".to_owned(),
            chain: chain.to_owned(),
            chain_kind: match chain {
                "Solana" => Chain::Solana,
                "Robinhood Chain" => Chain::RobinhoodChain,
                "Ethereum" => Chain::Ethereum,
                "BNB Chain" => Chain::Bnb,
                _ => Chain::Base,
            },
            chain_icon: "ethereum",
            contract: format!("0x{issuer}"),
            explorer_url: "https://explorer.example".to_owned(),
            source_url: "https://issuer.example".to_owned(),
            powers: DirectoryPowersView::unavailable(),
        };
        let mut contracts = vec![
            view("Base", "base"),
            view("BNB Chain", "bnb"),
            view("Ethereum", "eth"),
            view("Robinhood Chain", "rh"),
            view("Solana", "sol"),
        ];
        sort_directory_contracts(&mut contracts);
        assert_eq!(
            contracts.iter().map(|contract| contract.chain.as_str()).collect::<Vec<_>>(),
            ["Solana", "Robinhood Chain", "Ethereum", "BNB Chain", "Base"],
        );
    }

    fn powers_record(
        source_verified: crate::domain::powers::SourceVerified,
    ) -> crate::domain::powers::PowersRecord {
        crate::domain::powers::PowersRecord {
            chain: Chain::Base,
            contract: "0x0000000000000000000000000000000000000002".to_owned(),
            can_seize: Vec::new(),
            can_block: Vec::new(),
            can_change_rules: Vec::new(),
            token_paused: None,
            sanctions_list: None,
            unavailable: Vec::new(),
            source_verified_subject: crate::domain::powers::SourceVerifiedSubject::Contract,
            source_verified,
            source_verified_proxy: None,
            observed_at: "2026-10-01T00:00:00Z".to_owned(),
            block: None,
            slot: None,
            reads: Vec::new(),
        }
    }

    #[test]
    fn statement_form_collects_repeated_chain_checkboxes_and_label() {
        let request = statement_request_from_form(vec![
            ("label".to_owned(), "Q3 2026 holdings".to_owned()),
            ("wallets".to_owned(), " wallet-one\nwallet-two ".to_owned()),
            ("chains".to_owned(), "solana".to_owned()),
            ("chains".to_owned(), "base".to_owned()),
            ("block".to_owned(), "123".to_owned()),
        ])
        .expect("valid statement form");
        assert_eq!(request.label, "Q3 2026 holdings");
        assert_eq!(request.wallets, ["wallet-one", "wallet-two"]);
        assert_eq!(request.chains, ["solana", "base"]);
        assert_eq!(request.block, Some(123));
    }

    #[test]
    fn statement_csv_quotes_metadata_and_exports_rows() {
        let statement = crate::domain::statement::Statement {
            id: "statement-id".to_owned(),
            kind: "statement".to_owned(),
            version: 1,
            label: "Q3, \"draft\"".to_owned(),
            wallets: Vec::new(),
            assets: Vec::new(),
            positions: vec![crate::domain::statement::StatementPosition {
                chain: Chain::Base,
                wallet: "wallet".to_owned(),
                block: Some(0),
                min_slot: None,
                max_slot: None,
            }],
            block: None,
            observed_at: "2026-10-07T00:00:00Z".to_owned(),
            reads: Vec::new(),
            reads_truncated: false,
            signer: "signer".to_owned(),
            signature: "signature".to_owned(),
            dev: true,
        };
        let csv = statement_csv_body(&statement);
        assert!(csv.starts_with("\"record_type\",\"statement_id\",\"label\",\"observed_at\""));
        assert!(csv.contains(
            "\"statement\",\"statement-id\",\"Q3, \"\"draft\"\"\",\"2026-10-07T00:00:00Z\""
        ));
        assert!(csv.contains("\"verify_url\""));
        assert!(csv.contains("\"/statements/statement-id/verify\""));
        assert!(csv.contains("\"not observed\""));
    }

    #[test]
    fn formula_like_statement_fields_are_prefixed_in_csv_only() {
        let statement = crate::domain::statement::Statement {
            id: "statement-id".to_owned(),
            kind: "statement".to_owned(),
            version: 1,
            label: "   =1+1".to_owned(),
            wallets: Vec::new(),
            assets: vec![crate::domain::statement::StatementAsset {
                wallet: "wallet".to_owned(),
                chain: crate::domain::chain::Chain::Base,
                contract: "0x0000000000000000000000000000000000000001".to_owned(),
                ticker: "\t=TOKEN".to_owned(),
                issuer: "-TOKEN".to_owned(),
                issuer_match: false,
                balance: "0".to_owned(),
                decimals: 18,
                powers_observed_at: None,
                powers_block: None,
                powers_slot: None,
                slot: None,
                powers_summary: crate::domain::statement::PowersSummary {
                    can_seize: Vec::new(),
                    can_block: Vec::new(),
                    can_change_rules: Vec::new(),
                    unavailable: Vec::new(),
                },
            }],
            positions: Vec::new(),
            block: None,
            observed_at: "2026-10-07T00:00:00Z".to_owned(),
            reads: Vec::new(),
            reads_truncated: false,
            signer: "signer".to_owned(),
            signature: "signed-original-payload".to_owned(),
            dev: true,
        };
        let signed_json = serde_json::to_vec(&statement).expect("signed record JSON");
        let csv = statement_csv_body(&statement);
        assert!(csv.contains("\"'   =1+1\""));
        assert!(csv.contains("\"'\t=TOKEN\""));
        assert!(csv.contains("\"'-TOKEN\""));
        assert_eq!(
            serde_json::to_vec(&statement).expect("signed record remains unchanged"),
            signed_json
        );
    }

    #[tokio::test]
    async fn homepage_does_not_embed_publisher_watch_candidate_records() {
        use axum::{body::Body, http::Request};
        use tower::ServiceExt;

        let state = AppState::for_tests(Vec::new(), Vec::new(), true);
        state.leaderboard.write().await.impostors.unsupported_candidates.push(
            crate::adapters::discovery::UnsupportedImpostorCandidate {
                dex_chain_id: "unsupported".to_owned(),
                ticker: "NVDA".to_owned(),
                publisher: "Issuer".to_owned(),
                symbol: "NVDAx".to_owned(),
                name: "HOMEPAGE_WATCH_ROW_MUST_NOT_APPEAR".to_owned(),
                address: "0x0000000000000000000000000000000000000001".to_owned(),
                volume_24h_usd: None,
                source: crate::adapters::discovery::MarketSource::Dexscreener,
                first_seen_at: "2026-10-06T00:00:00Z".to_owned(),
                last_seen_at: "2026-10-06T00:00:00Z".to_owned(),
                evidence_truncated: false,
            },
        );
        let response = crate::adapters::web::router(state)
            .oneshot(Request::get("/").body(Body::empty()).unwrap())
            .await
            .expect("homepage response");
        assert_eq!(response.status(), StatusCode::OK);
        let body =
            axum::body::to_bytes(response.into_body(), 1024 * 1024).await.expect("homepage body");
        assert!(!String::from_utf8_lossy(&body).contains("HOMEPAGE_WATCH_ROW_MUST_NOT_APPEAR"));
    }

    #[test]
    fn zero_statement_block_is_rendered_as_not_observed() {
        let position = crate::domain::statement::StatementPosition {
            chain: Chain::Ethereum,
            wallet: "0x0000000000000000000000000000000000000001".to_owned(),
            block: Some(0),
            min_slot: None,
            max_slot: None,
        };
        assert!(statement_position_html(&position).contains(": not observed</li>"));
    }

    #[test]
    fn recheck_live_query_accepts_boolean_and_numeric_values() {
        for value in ["true", "TRUE", "1"] {
            assert!(RecheckQuery { live: Some(value.to_owned()) }.is_live());
        }
        for value in ["false", "FALSE", "0", "unexpected"] {
            assert!(!RecheckQuery { live: Some(value.to_owned()) }.is_live());
        }
        assert!(!RecheckQuery::default().is_live());
    }
    #[tokio::test]
    async fn statement_page_attributes_assets_and_escapes_unavailable_facts() {
        let state = AppState::for_tests(Vec::new(), Vec::new(), true);
        let id = "statement-id".to_owned();
        state
            .app
            .statement_cache
            .insert(
                id.clone(),
                crate::domain::statement::Statement {
                    id: id.clone(),
                    kind: "statement".to_owned(),
                    version: 1,
                    label: "Q3 <investor> report".to_owned(),
                    wallets: vec![
                        crate::domain::statement::StatementWallet {
                            chain: Chain::Solana,
                            address: "wallet<one>".to_owned(),
                        },
                        crate::domain::statement::StatementWallet {
                            chain: Chain::Base,
                            address: "0x0000000000000000000000000000000000000001".to_owned(),
                        },
                    ],
                    assets: vec![
                        crate::domain::statement::StatementAsset {
                            wallet: "wallet<one>".to_owned(),
                            chain: Chain::Solana,
                            contract: "mint&one".to_owned(),
                            ticker: "NVDA".to_owned(),
                            issuer: "issuer".to_owned(),
                            issuer_match: true,
                            balance: "12".to_owned(),
                            decimals: 0,
                            powers_observed_at: Some("2026-10-04T00:00:00Z".to_owned()),
                            powers_block: Some(800),
                            powers_slot: Some(44),
                            slot: Some(42),
                            powers_summary: crate::domain::statement::PowersSummary {
                                can_seize: Vec::new(),
                                can_block: Vec::new(),
                                can_change_rules: Vec::new(),
                                unavailable: vec![crate::domain::powers::Reason {
                                    code: "rpc<timeout>".to_owned(),
                                    detail: "read & retry".to_owned(),
                                }],
                            },
                        },
                        crate::domain::statement::StatementAsset {
                            wallet: "0x0000000000000000000000000000000000000001".to_owned(),
                            chain: Chain::Base,
                            contract: "0x0000000000000000000000000000000000000002".to_owned(),
                            ticker: "NVDA".to_owned(),
                            issuer: "issuer".to_owned(),
                            issuer_match: true,
                            balance: "0".to_owned(),
                            decimals: 18,
                            powers_observed_at: None,
                            powers_block: None,
                            powers_slot: None,
                            slot: None,
                            powers_summary: crate::domain::statement::PowersSummary {
                                can_seize: Vec::new(),
                                can_block: Vec::new(),
                                can_change_rules: Vec::new(),
                                unavailable: Vec::new(),
                            },
                        },
                    ],
                    positions: vec![
                        crate::domain::statement::StatementPosition {
                            chain: Chain::Solana,
                            wallet: "wallet<one>".to_owned(),
                            block: None,
                            min_slot: Some(42),
                            max_slot: Some(44),
                        },
                        crate::domain::statement::StatementPosition {
                            chain: Chain::Base,
                            wallet: "0x0000000000000000000000000000000000000001".to_owned(),
                            block: Some(79879642),
                            min_slot: None,
                            max_slot: None,
                        },
                    ],
                    block: None,
                    observed_at: "<script>alert(1)</script>".to_owned(),
                    reads: Vec::new(),
                    reads_truncated: false,
                    signer: "signer".to_owned(),
                    signature: "signature".to_owned(),
                    dev: true,
                },
            )
            .await;
        let Html(verification) = verify_statement_page(State(state.clone()), Path(id.clone()))
            .await
            .expect("statement verification page");
        assert!(
            verification
                .contains("Not applicable: a wallet statement is a point-in-time snapshot.")
        );
        assert!(verification.contains(
            "QED public verification key: <a href=\"/.well-known/qed.json\">/.well-known/qed.json</a>"
        ));

        let Html(html) =
            statement_page(State(state), Path(id), Query(StatementPageQuery::default()))
                .await
                .expect("statement page");

        assert!(html.contains("<code>wallet&lt;one&gt;</code>"));
        assert!(html.contains("rpc&lt;timeout&gt;</strong>: read &amp; retry"));
        assert!(html.contains("<strong>12</strong><small>Raw 12 · 0 decimals</small>"));
        assert!(html.contains("<strong>NVDA</strong><small>issuer</small>"));
        assert!(html.contains(
            "<td class=\"statement-ticker\"><strong>NVDA</strong><small>issuer</small></td>"
        ));
        assert!(html.contains(
            "<td class=\"statement-contract\"><code title=\"mint&amp;one\">mint&amp;one</code></td>"
        ));
        assert!(html.contains("<td>Match</td>"));
        for heading in ["Chain", "Ticker", "Contract", "Balance", "Issuer match"] {
            assert!(html.contains(&format!("<th>{heading}</th>")));
        }
        assert!(html.contains("<tr class=\"statement-holding-row\">"));
        assert!(html.contains("<tr class=\"statement-detail-row\"><td colspan=\"5\">"));
        assert!(!html.contains("<th>Powers summary</th>"));
        assert!(html.contains("<strong>Label:</strong> Q3 &lt;investor&gt; report"));
        assert!(html.contains("href=\"/statements/statement-id/download.csv\" download"));
        assert!(html.contains("href=\"/statements/statement-id/verify\""));
        assert!(html.contains(
            "Verify this record: <a href=\"/statements/statement-id/verify\">/statements/statement-id/verify</a>"
        ));
        assert!(html.contains("action=\"/statements/statement-id/recheck\""));
        assert!(html.contains("data-print-statement"));
        assert!(html.contains("Observed: <time datetime=\"2026-10-04T00:00:00Z\" title=\"2026-10-04T00:00:00Z\">4 Oct 2026, 00:00 UTC</time>"));
        assert!(html.contains("slots 42–44"));
        assert!(html.contains("block 79879642"));
        assert!(!html.contains("block —"));
        assert!(!html.contains("slots —"));
        assert!(html.contains("Asset slot: 42<br>Power block: 800<br>Power slot: 44"));
        assert!(!html.contains("Asset slot: —"));
        assert!(!html.contains("Power block: —"));
        assert!(!html.contains("Power slot: —"));
        assert!(html.contains("Statement pages are public to anyone with their link"));
        assert!(html.contains(
            "Development signer: this ephemeral signature is not a production trust signal."
        ));
        assert!(html.contains("/api/statement/statement-id"));
        assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(!html.contains("<script>alert(1)</script>"));
    }
    #[tokio::test]
    async fn certificate_verification_page_shows_freshness_and_public_key() {
        let state = AppState::for_tests(Vec::new(), Vec::new(), true);
        let subject = "0x0000000000000000000000000000000000000001".to_owned();
        let now = chrono::Utc::now();
        let mut attestation = crate::domain::attestation::Attestation {
            id: String::new(),
            version: 1,
            chain: Chain::Base,
            subject: subject.clone(),
            verdict: crate::domain::check::Verdict::Unknown { reason: "test".to_owned() },
            issuer: None,
            ticker: None,
            pool: crate::domain::pool::PoolInfo {
                chain: Chain::Base,
                pool: subject,
                dex: "uniswap-v3".to_owned(),
                base: crate::domain::pool::TokenSide {
                    address: "0x0000000000000000000000000000000000000002".to_owned(),
                    symbol: Some("BASE".to_owned()),
                    decimals: Some(18),
                    balance: None,
                },
                quote: crate::domain::pool::TokenSide {
                    address: "0x0000000000000000000000000000000000000003".to_owned(),
                    symbol: Some("QUOTE".to_owned()),
                    decimals: Some(18),
                    balance: None,
                },
            },
            quote_share_of_supply: None,
            registry_entry: None,
            registry_hash: String::new(),
            reads: Vec::new(),
            block: Some(1),
            slot: None,
            checked_at: now.to_rfc3339(),
            expires_at: (now + chrono::Duration::hours(1)).to_rfc3339(),
            signer: attest::public_key_b58(&state.app),
            signature: String::new(),
            dev: true,
        };
        let payload = crate::domain::attestation::canonical_json(&attestation.payload()).unwrap();
        let (id, signature) = attest::sign_document_payload(&state.app, &payload).unwrap();
        attestation.id.clone_from(&id);
        attestation.signature = signature;
        state
            .app
            .attestations
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id.clone(), attestation);

        let Html(html) = verify_certificate_page(State(state), Path(id)).await.unwrap();
        assert!(html.contains("<dt>Fresh</dt><dd>true</dd>"));
        assert!(html.contains(
            "Fresh means the check time is not in the future and the recorded expiry has not passed."
        ));
        assert!(html.contains(
            "QED public verification key: <a href=\"/.well-known/qed.json\">/.well-known/qed.json</a>"
        ));
    }
    #[test]
    fn inverted_v4_pair_orders_issuer_token_first_on_certificate_homepage_and_token_page() {
        let token_address = "0x0000000000000000000000000000000000000003";
        let attestation = crate::domain::attestation::Attestation {
            id: "inverted-v4-pair".to_owned(),
            version: 1,
            chain: Chain::Base,
            subject: "0x0000000000000000000000000000000000000001".to_owned(),
            verdict: crate::domain::check::Verdict::Unknown { reason: "fixture".to_owned() },
            issuer: Some("Issuer".to_owned()),
            ticker: Some("NVDA".to_owned()),
            pool: crate::domain::pool::PoolInfo {
                chain: Chain::Base,
                pool: "0x0000000000000000000000000000000000000001".to_owned(),
                dex: "uniswap-v4".to_owned(),
                base: crate::domain::pool::TokenSide {
                    address: "0x0000000000000000000000000000000000000002".to_owned(),
                    symbol: Some("USDG".to_owned()),
                    decimals: Some(18),
                    balance: None,
                },
                quote: crate::domain::pool::TokenSide {
                    address: token_address.to_owned(),
                    symbol: Some("NVDA".to_owned()),
                    decimals: Some(18),
                    balance: None,
                },
            },
            quote_share_of_supply: None,
            registry_entry: Some(crate::domain::registry::Entry {
                issuer: "Issuer".to_owned(),
                ticker: "NVDA".to_owned(),
                name: "NVIDIA".to_owned(),
                chain: Chain::Base,
                contract: token_address.to_owned(),
                decimals: Some(18),
                source: "fixture".to_owned(),
                source_url: "https://issuer.example".to_owned(),
                last_checked: "2026-10-07T00:00:00Z".to_owned(),
                removed_at: None,
                stale_since: None,
                official_deployments: Vec::new(),
            }),
            registry_hash: String::new(),
            reads: Vec::new(),
            block: Some(1),
            slot: None,
            checked_at: "2026-10-07T00:00:00Z".to_owned(),
            expires_at: "2026-10-07T01:00:00Z".to_owned(),
            signer: String::new(),
            signature: String::new(),
            dev: true,
        };
        let board_entry = crate::adapters::discovery::LeaderboardEntry {
            rank: 1,
            chain: "base".to_owned(),
            chain_label: "Base".to_owned(),
            dex: "uniswap-v4".to_owned(),
            pool: "0x0000000000000000000000000000000000000001".to_owned(),
            base_symbol: "USDG".to_owned(),
            quote_symbol: "NVDA".to_owned(),
            issuer: Some("Issuer".to_owned()),
            ticker: Some("NVDA".to_owned()),
            issuer_on_base: Some(false),
            verdict: "verified".to_owned(),
            read_status: "checked".to_owned(),
            read_reason: None,
            price_usd: Some(1.0),
            change_24h_pct: None,
            volume_24h_usd: None,
            liquidity_usd: None,
            source: crate::adapters::discovery::MarketSource::Dexscreener,
            txns_24h: None,
            detail_url: "/validated/base/pool".to_owned(),
            trade_url: "https://dexscreener.com/base/pool".to_owned(),
            explorer_url: String::new(),
            attestation_id: None,
            checked_at: None,
        };
        let certificate = super::super::views::CertificateView::from_attestation(attestation);
        let homepage = super::super::views::LeaderboardPageView::from_value(serde_json::json!({
            "entries": [serde_json::to_value(&board_entry).unwrap()],
            "refreshing": false,
            "empty_successful": false
        }));

        assert_eq!(certificate.pair, "NVDA / USDG");
        assert_eq!(homepage.rows[0].base_symbol, "NVDA");
        assert_eq!(homepage.rows[0].quote_symbol, "USDG");
        assert_eq!(pool_view(&board_entry).pair, "NVDA / USDG");
    }
}

#[cfg(test)]
mod copies_page_tests {
    use super::*;
    use crate::{
        adapters::discovery::{ImpostorEntry, MarketSource},
        domain::guard::PublisherProductTier,
    };

    fn entry(
        name: &str,
        chain: &str,
        volume: Option<f64>,
        tier: PublisherProductTier,
    ) -> ImpostorEntry {
        ImpostorEntry {
            chain: chain.to_owned(),
            chain_label: if chain == "solana" { "Solana" } else { "Robinhood Chain" }.to_owned(),
            ticker: "NVDA".to_owned(),
            publisher: "Robinhood".to_owned(),
            symbol: "NVDA".to_owned(),
            name: name.to_owned(),
            address: format!("address-{name}"),
            first_seen_at: "2026-10-06T01:02:00Z".to_owned(),
            last_seen_at: "2026-10-07T03:04:00Z".to_owned(),
            tier,
            volume_24h_usd: volume,
            liquidity_24h_usd: None,
            pool_count: None,
            source: MarketSource::Dexscreener,
            guard_url: format!("/guard/{chain}/address-{name}"),
            reason: "test observation".to_owned(),
            reads: Vec::new(),
            on_chain_symbol: Some("NVDA".to_owned()),
            on_chain_name: Some(name.to_owned()),
            publisher_catalog_snapshot_hash: None,
            evidence_truncated: false,
            guard_document: None,
        }
    }

    fn render(entries: &[ImpostorEntry], chain: Option<&str>) -> String {
        let state = AppState::for_tests(Vec::new(), Vec::new(), false);
        let refs = entries.iter().collect::<Vec<_>>();
        render_copies_page(&state, &refs, &[], chain).expect("copies page").0
    }

    #[test]
    fn copies_leaderboard_sorts_by_volume_and_displays_ranks() {
        let body = render(
            &[
                entry("Lower", "solana", Some(10.0), PublisherProductTier::Confirmed),
                entry("Highest", "robinhood", Some(90.0), PublisherProductTier::Confirmed),
                entry("No volume", "solana", None, PublisherProductTier::Confirmed),
            ],
            None,
        );
        let high = body.find("Highest (NVDA)").expect("highest row");
        let low = body.find("Lower (NVDA)").expect("lower row");
        let missing = body.find("No volume (NVDA)").expect("missing-volume row");
        assert!(high < low && low < missing);
        assert!(body.contains("<td data-label=\"#\">1</td>"));
        assert!(body.contains("<td data-label=\"#\">2</td>"));
        assert!(body.contains("<td data-label=\"#\">3</td>"));
    }

    #[test]
    fn copies_chain_filter_counts_tabs_and_unknown_falls_back_to_all() {
        let entries = [
            entry("RobinhoodOne", "robinhood", Some(9.0), PublisherProductTier::Confirmed),
            entry("RobinhoodTwo", "robinhood", Some(8.0), PublisherProductTier::Confirmed),
            entry("SolanaOne", "solana", Some(7.0), PublisherProductTier::Confirmed),
            entry("SolanaNear", "solana", Some(6.0), PublisherProductTier::Caution),
        ];
        let filtered = render(&entries, Some("solana"));
        assert!(filtered.contains("aria-current=\"page\">Solana <span>1</span>"));
        assert!(filtered.contains("All <span>3</span>"));
        assert!(!filtered.contains("RobinhoodOne (NVDA)"));
        assert!(filtered.contains("SolanaOne (NVDA)"));

        let unknown = render(&entries, Some("unknown"));
        assert!(
            unknown.contains("<a href=\"/copies\" aria-current=\"page\">All <span>3</span></a>")
        );
        assert!(unknown.contains("RobinhoodOne (NVDA)"));
        assert!(unknown.contains("SolanaOne (NVDA)"));
    }

    #[test]
    fn copies_hero_uses_the_max_volume_confirmed_entry() {
        let body = render(
            &[
                entry("Lower", "solana", Some(10.0), PublisherProductTier::Confirmed),
                entry("HeroMaximum", "robinhood", Some(900.0), PublisherProductTier::Confirmed),
                entry("CautionMaximum", "robinhood", Some(5000.0), PublisherProductTier::Caution),
            ],
            None,
        );
        let hero = body.find("id=\"copies-hero-title\">HeroMaximum").expect("maximum hero");
        let caution = body.find("CautionMaximum (NVDA)").expect("caution row");
        assert!(hero < caution);
        assert!(body.contains("Largest confirmed copy by reported 24h volume"));
        assert!(body.contains("Deny · View Guard"));
    }

    #[test]
    fn copies_empty_state_links_back_to_stats() {
        let body = render(&[], None);
        assert!(body.contains("No copies currently flagged"));
        assert!(body.contains("<a href=\"/stats\">leaderboard statistics</a>"));
    }
}

pub(super) async fn display_powers(
    state: &crate::app::context::Context,
    record: crate::domain::powers::PowersRecord,
) -> DirectoryPowersView {
    let key = (record.chain, record.contract.clone());
    let confirmed_at = record.observed_at.clone();
    let mut view = powers_view(record);
    if let Some(partial) = state.powers_retry_cache.get(&key).await
        && partial.observed_at >= confirmed_at
        && view.unavailable.is_empty()
    {
        let mut latest = powers_view(partial);
        latest.last_confirmed_at = confirmed_at;
        latest.last_confirmed_signals = format!(
            "Seizure: {}. Blocking: {}. Rule changes: {}. {}: {}.",
            if view.can_seize.is_empty() {
                "No evidence observed".into()
            } else {
                view.can_seize.join("; ")
            },
            if view.can_block.is_empty() {
                "No evidence observed".into()
            } else {
                view.can_block.join("; ")
            },
            if view.can_change_rules.is_empty() {
                "No evidence observed".into()
            } else {
                view.can_change_rules.join("; ")
            },
            view.source_verified_subject,
            view.source_verified
        );
        view = latest;
    }
    view
}
