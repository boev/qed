use super::views::{
    CertificateResultTemplate, CertificateTemplate, CertificateView, ChainTemplate,
    ContentPageTemplate, DirectoryContractView, DirectoryPoolView, DirectoryPowersView,
    DocsPageTemplate, FeaturedTemplate, FeaturedView, GlossaryTemplate, GuardResultTemplate,
    GuardTemplate, GuideVerifyTemplate, IndexTemplate, RegistryTableQuery, RegistryTableTemplate,
    RegistryTemplate, TokenTemplate, ValidatedCardView, ValidatedTemplate, WalletHoldingsTemplate,
    WalletTemplate, WhatsNewView,
};
#[cfg(test)]
use super::views::{WalletHoldingView, WalletTradeLinkView, format_wallet_amount};
use super::{ASSET_VERSION, render_page};
#[cfg(test)]
use crate::adapters::registry as registry_adapter;
#[cfg(test)]
use crate::app::wallet::wallet_chains;
use crate::{
    adapters::{content as content_adapter, state::AppState},
    app::{attestation as attest, wallet::wallet_holdings},
    domain::{
        chain::Chain,
        registry::{self, Entry},
    },
};
use askama::Template;
use axum::{
    extract::{Form, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Redirect, Response},
};
use serde::Deserialize;
use serde_json::json;
use std::time::Duration;
const POWERS_PAGE_DEADLINE: Duration = Duration::from_millis(2500);
pub(crate) async fn index(
    State(state): State<AppState>,
    _headers: HeaderMap,
) -> Result<Html<String>, StatusCode> {
    let leaderboard = state.leaderboard.read().await.clone();
    let mut value =
        serde_json::to_value(&leaderboard).unwrap_or_else(|_| serde_json::json!({ "entries": [] }));
    if let Some(object) = value.as_object_mut() {
        object.remove("impostors");
        object.insert(
            "prices_updated_at".to_owned(),
            serde_json::Value::String(state.prices.read().await.updated_at.clone()),
        );
    }
    let registry = state.registry.read().await.clone();
    let stats = super::api::leaderboard_stats(&leaderboard, &registry);
    let watch = &stats.publisher_catalog_watch;
    let stats_line = format!(
        "{} top pools · {} issuer matches · {} mismatches · {} unsupported venues · {} not read yet · {} currently flagged (seen in the last 7 days) · {} new this UTC week · {} unsupported-chain sightings · catalog scan {}",
        stats.listed_pools,
        stats.issuer_matches,
        stats.mismatches,
        stats.unsupported_venue,
        stats.not_read_yet.count,
        watch.currently_flagged_last_7_days,
        watch.first_flagged_this_week,
        watch.unsupported_chain_candidates_seen,
        if watch.last_scanned_at.is_empty() { "not yet run" } else { &watch.last_scanned_at }
    );
    render_page(
        IndexTemplate {
            asset_version: ASSET_VERSION,
            public_url: state.public_url.to_string(),
            stats_line,
            leaderboard: super::views::LeaderboardPageView::from_value(value),
            whats_new: release_news_view()?,
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
    let mut response = Response::new(axum::body::Body::from(snapshot.html.clone()));
    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("text/html; charset=utf-8"),
    );
    Ok(response)
}

pub(crate) fn render_stats_page(
    state: &AppState,
    stats: &super::api::LeaderboardStats,
    catalog_entries: &[&crate::adapters::discovery::ImpostorEntry],
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
    let mut catalog_rows = catalog_entries
        .iter()
        .map(|entry| {
            let volume = entry
                .volume_24h_usd
                .map(|volume| {
                    format!("${volume:.2} · reported by {}", entry.source.label())
                })
                .unwrap_or_else(|| "not reported".to_owned());
            format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td><td>{} ({})</td><td><code>{}</code></td><td>{}</td><td>{}</td><td>{}</td><td><a href=\"{}\">Guard</a></td><td>{}</td></tr>",
                html_escape(&entry.chain_label),
                html_escape(&entry.ticker),
                html_escape(&entry.publisher),
                html_escape(&entry.symbol),
                html_escape(&entry.name),
                html_escape(&entry.address),
                html_escape(&volume),
                html_escape(&entry.first_seen_at),
                html_escape(&entry.last_seen_at),
                html_escape(&entry.guard_url),
                html_escape(&entry.reason),
            )
        })
        .collect::<String>();
    if catalog_rows.is_empty() {
        catalog_rows.push_str(
            "<tr><td colspan=\"10\">No catalog observations were seen during the last 7 days.</td></tr>",
        );
    }
    let last_scanned_at = if watch.last_scanned_at.is_empty() {
        "Not yet scanned"
    } else {
        watch.last_scanned_at.as_str()
    };
    let catalog_table_note = if watch.catalog_absent_tokens_truncated {
        "Showing the 20 most recently seen observations; the count includes all retained entries seen in the last 7 days."
    } else {
        "All retained observations seen in the last 7 days are shown."
    };
    let source_note = watch
        .source_unavailable_since
        .as_deref()
        .map(|since| {
            format!(
                "<p>The impostor search source has been unavailable since <time>{}</time>; the observations below are from the last successful search.</p>\n",
                html_escape(since)
            )
        })
        .unwrap_or_default();
    let body_html = format!(
        r#"<section class="statement-summary">
<p>Generated at <time>{}</time>.</p>
<p><a href="/stats.json">Download signed JSON</a> · <a href="/stats.csv">Download signed CSV</a></p>
<p>Market data: <a href="https://dexscreener.com" target="_blank" rel="noopener noreferrer">DexScreener</a> / <a href="https://www.geckoterminal.com" target="_blank" rel="noopener noreferrer">GeckoTerminal</a> · <a href="https://www.coingecko.com/en/api_terms" target="_blank" rel="noopener noreferrer">Powered by CoinGecko</a></p>
<dl class="statement-summary-grid">
  <div><dt>Listed pools</dt><dd>{}</dd></div>
  <div><dt>Pools checked</dt><dd>{}</dd></div>
  <div><dt>Issuer matches</dt><dd>{}</dd></div>
  <div><dt>Mismatches</dt><dd>{}</dd></div>
  <div><dt>Unsupported venues</dt><dd>{}</dd></div>
  <div><dt>Not read yet</dt><dd>{} total: {} RPC limit, {} transient, {} unsupported</dd></div>
</dl>
<h2>Leaderboard by chain</h2>
<ul>{}</ul>
<h2>Active registry coverage</h2>
<p>{} active registry entries across the tracked issuers.</p>
<ul>{}</ul>
<h2>Publisher deployment catalog watch</h2>
{}<p>Last searched at <time>{}</time>. {} catalog observations are currently flagged (seen in the last 7 days); {} were first seen this UTC week. {} candidates on unsupported chains were seen but not judged; {} official deployments on unsupported chains were counted separately and excluded from candidates. {} supported and {} unsupported candidates were evicted; {} invalid supported and {} invalid unsupported candidates were rejected.</p>
<p>Method: {}</p>
<table><caption>Catalog observations currently flagged (last seen within the last 7 days); ticker, symbol, name, and 24-hour volume are reported by the source recorded for each row. On-chain identity reads are available from the linked Guard review. {}</caption>
<thead><tr><th>Chain</th><th>Ticker reported by source</th><th>Publisher</th><th>Listing symbol and name</th><th>Address</th><th>Reported 24h volume</th><th>First seen</th><th>Last seen</th><th>Guard</th><th>Reason</th></tr></thead>
<tbody>{}</tbody></table>
<p>These are point-in-time pool reads. These exact symbol-and-name matches are catalog-absence observations, not conclusions about intent. A contract match does not prove backing, custody, reserves, solvency, safety, price, liquidity, or endorsement.</p>
</section>"#,
        html_escape(&stats.generated_at),
        stats.listed_pools,
        stats.pools_checked,
        stats.issuer_matches,
        stats.mismatches,
        stats.unsupported_venue,
        stats.not_read_yet.count,
        stats.not_read_yet.rpc_limit,
        stats.not_read_yet.transient,
        stats.not_read_yet.unsupported,
        chain_rows,
        stats.registry.active_entries,
        issuer_rows,
        source_note,
        html_escape(last_scanned_at),
        watch.currently_flagged_last_7_days,
        watch.first_flagged_this_week,
        watch.unsupported_chain_candidates_seen,
        watch.official_on_unsupported_chain,
        watch.evicted_entries,
        watch.evicted_unsupported_candidates,
        watch.rejected_oversize_entries,
        watch.rejected_oversize_unsupported_candidates,
        html_escape(&watch.method),
        html_escape(catalog_table_note),
        catalog_rows,
    );
    docs_content_page(
        &state,
        "Leaderboard statistics",
        "REFERENCE",
        "Leaderboard statistics",
        &stats.headline,
        "Point-in-time counts from QED's current leaderboard checks and active issuer registry.",
        "/stats",
        "stats",
        body_html,
    )
    .map(|Html(body)| bytes::Bytes::from(body))
}
fn release_news_view() -> Result<WhatsNewView, StatusCode> {
    let Some(entry) = content_adapter::latest_released_changelog_entry()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    else {
        return Ok(WhatsNewView::default());
    };
    let posts = content_adapter::blog_posts().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let latest_blog = content_adapter::latest_published_blog_post(&posts);
    Ok(WhatsNewView {
        available: true,
        release_label: entry.label,
        headline: entry.headline,
        changelog_href: format!("/changelog#{}", entry.anchor),
        has_latest_blog: latest_blog.is_some(),
        latest_blog_title: latest_blog.map(|post| post.title.clone()).unwrap_or_default(),
        latest_blog_href: latest_blog
            .map(|post| format!("/blog/{}", post.slug))
            .unwrap_or_default(),
    })
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

#[derive(Debug, Deserialize, Default)]
pub(crate) struct WalletRequest {
    pub(crate) address: String,
}

pub(crate) async fn wallet_page(
    State(state): State<AppState>,
    _headers: HeaderMap,
) -> Result<Response, StatusCode> {
    render_page(
        WalletTemplate { asset_version: ASSET_VERSION, public_url: state.public_url.to_string() },
        false,
    )
    .map(IntoResponse::into_response)
}

pub(crate) async fn wallet_holdings_page(
    State(state): State<AppState>,
    Form(request): Form<WalletRequest>,
) -> Result<Html<String>, StatusCode> {
    let address = request.address;
    let holdings = super::views::wallet_holding_views(
        wallet_holdings(&state.app, &address).await.map_err(super::wallet_error_status)?,
    );
    render_page(
        WalletHoldingsTemplate {
            asset_version: ASSET_VERSION,
            public_url: state.public_url.to_string(),
            address,
            holdings,
        },
        false,
    )
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
    let mut pending = Vec::new();
    for (index, contract) in contracts.iter_mut().enumerate() {
        if let Some(record) =
            crate::app::powers::cached_record(&state.app, contract.chain_kind, &contract.contract)
                .await
        {
            contract.powers = powers_view(record);
        } else {
            crate::app::powers::schedule_prefetch(
                &state.app,
                contract.chain_kind,
                &contract.contract,
            )
            .await;
            pending.push((index, contract.chain_kind, contract.contract.clone()));
        }
    }
    if !pending.is_empty() {
        let mut inspections = tokio::task::JoinSet::new();
        for (_, chain, contract) in &pending {
            let state = state.clone();
            let chain = *chain;
            let contract = contract.clone();
            inspections.spawn(async move {
                let _ = crate::app::powers::inspect_prefetched(&state.app, chain, &contract).await;
            });
        }
        let _ = tokio::time::timeout(POWERS_PAGE_DEADLINE, async {
            while inspections.join_next().await.is_some() {}
        })
        .await;
        for (index, chain, contract) in pending {
            contracts[index].powers =
                crate::app::powers::cached_record(&state.app, chain, &contract)
                    .await
                    .map(powers_view)
                    .unwrap_or_else(DirectoryPowersView::unavailable);
        }
    }
    sort_directory_contracts(&mut contracts);
    let leaderboard = state.leaderboard.read().await;
    let mut pools: Vec<_> = leaderboard
        .entries
        .iter()
        .filter(|entry| {
            entry
                .ticker
                .as_deref()
                .is_some_and(|value| value.eq_ignore_ascii_case(&canonical_ticker))
        })
        .map(pool_view)
        .collect();
    pools.sort_by(|left, right| {
        left.chain.cmp(&right.chain).then_with(|| left.pair.cmp(&right.pair))
    });
    let title = format!("{canonical_ticker} tokenized: issuer contracts and pools");
    let json_ld = safe_json_ld(json!({
        "@context": "https://schema.org",
        "@type": "CollectionPage",
        "name": title,
        "url": format!("{}/tokens/{}", state.public_url, canonical_ticker),
        "description": format!("Issuer registry contracts and currently listed pools for tokenized {canonical_ticker}."),
        "mainEntity": {
            "@type": "ItemList",
            "numberOfItems": contracts.len(),
            "itemListElement": contracts.iter().enumerate().map(|(index, contract)| json!({
                "@type": "ListItem",
                "position": index + 1,
                "name": format!("{} {} on {}", contract.issuer, canonical_ticker, contract.chain),
                "url": contract.explorer_url
            })).collect::<Vec<_>>()
        }
    }));
    render_page(
        TokenTemplate {
            asset_version: ASSET_VERSION,
            public_url: state.public_url.to_string(),
            ticker: canonical_ticker,
            title,
            contracts,
            pools,
            json_ld,
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
            whats_new: super::views::WhatsNewView::default(),
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
            whats_new: if is_docs_landing { release_news_view()? } else { WhatsNewView::default() },
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

fn contract_view(entry: &Entry) -> DirectoryContractView {
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
    }
}

fn pool_view(entry: &crate::adapters::discovery::LeaderboardEntry) -> DirectoryPoolView {
    let verdict = entry.verdict.to_ascii_lowercase();
    let (verdict, verdict_class) = match verdict.as_str() {
        "verified" => ("Verified".to_owned(), "is-verified".to_owned()),
        "mismatch" => ("Mismatch".to_owned(), "is-mismatch".to_owned()),
        "nomatch" => ("No match".to_owned(), "is-mismatch".to_owned()),
        _ => ("Unknown".to_owned(), "is-unknown".to_owned()),
    };
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
        chain: entry.chain_label.clone(),
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
        .map(Html)
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
            "The 15-second Guard review deadline elapsed. Try again."
        }
        crate::app::guard::GuardError::Signing => "QED could not sign this Guard review.",
    }
}

pub(crate) async fn featured(State(state): State<AppState>) -> Result<Html<String>, StatusCode> {
    let featured = state.featured.read().await;
    FeaturedTemplate { pools: featured.iter().take(4).map(FeaturedView::from_pool).collect() }
        .render()
        .map(Html)
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
        "Use QED with an LLM",
        "GUIDE",
        "Use QED with an LLM",
        "Connect an AI agent to QED’s issuer-check, token-power, statement, registry, verification, and Guard tools over MCP.",
        "Seven read-only tools answer plain questions about issuer publications, token controls, wallet holdings, and signed records.",
        "/docs/llm",
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
        "API quick start",
        "GUIDE",
        "API quick start",
        "Make your first QED requests with three curl commands.",
        "Three curl examples for checks, registry lookup, and service status.",
        "/docs/api-quick-start",
        "api-quick-start",
        content_adapter::render_markdown(body),
    )
}

pub(crate) async fn about_page(State(state): State<AppState>) -> Result<Html<String>, StatusCode> {
    docs_content_page(
        &state,
        "About",
        "ABOUT",
        "About QED",
        "What QED checks, what it does not claim, and who operates the service.",
        "QED checks whether a pool uses the stock-token contract published by its issuer and states the limits of that check.",
        "/about",
        "about",
        content_adapter::render_markdown(ABOUT_MARKDOWN),
    )
}

pub(crate) async fn security_page(
    State(state): State<AppState>,
) -> Result<Html<String>, StatusCode> {
    docs_content_page(
        &state,
        "Security",
        "SECURITY",
        "Security",
        "How to report suspected QED security issues and coordinate disclosure.",
        "How to report suspected QED security issues and coordinate disclosure.",
        "/security",
        "security",
        content_adapter::security_html(),
    )
}

pub(crate) async fn changelog_page(
    State(state): State<AppState>,
) -> Result<Html<String>, StatusCode> {
    docs_content_page(
        &state,
        "Changelog",
        "CHANGELOG",
        "Changelog",
        "Release history for QED routes, observations, and documentation.",
        "Release history for QED routes, observations, and documentation.",
        "/changelog",
        "changelog",
        content_adapter::changelog_html(),
    )
}

pub(crate) async fn blog_page(State(state): State<AppState>) -> Result<Html<String>, StatusCode> {
    let posts = content_adapter::blog_posts().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let posts = content_adapter::published_blog_posts(posts);
    docs_content_page(
        &state,
        "Blog",
        "BLOG",
        "Blog",
        "Published factual notes about QED checks, powers observations, and signed statements.",
        "Published factual notes about QED checks, powers observations, and signed statements.",
        "/blog",
        "blog",
        content_adapter::blog_index_html(&posts),
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
        &post.title,
        "BLOG",
        &post.title,
        &post.summary,
        &post.summary,
        &format!("/blog/{}", post.slug),
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
    <textarea id="statement-wallets" name="wallets" rows="5" placeholder="Paste wallet addresses here" required></textarea>
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
        "Statement",
        "What did these wallets hold, provably?",
        "STATEMENT",
        STATEMENT_PURPOSE,
        "Create a signed, re-checkable statement of registry-token balances for selected wallets.",
        "/statements",
        form.to_owned(),
    )
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
    page_title: &str,
    heading: &str,
    eyebrow: &str,
    intro: &str,
    meta_description: &str,
    canonical_path: &str,
    body_html: String,
) -> Result<Html<String>, StatusCode> {
    render_page(
        ContentPageTemplate {
            asset_version: ASSET_VERSION,
            public_url: state.public_url.to_string(),
            page_title: page_title.to_owned(),
            heading: heading.to_owned(),
            eyebrow: eyebrow.to_owned(),
            intro: intro.to_owned(),
            meta_description: meta_description.to_owned(),
            canonical_path: canonical_path.to_owned(),
            body_html,
        },
        false,
    )
}

pub(crate) fn docs_content_page(
    state: &AppState,
    page_title: &str,
    eyebrow: &str,
    heading: &str,
    intro: &str,
    meta_description: &str,
    canonical_path: &str,
    active_sidebar: &str,
    body_html: String,
) -> Result<Html<String>, StatusCode> {
    render_page(
        DocsPageTemplate {
            asset_version: ASSET_VERSION,
            public_url: state.public_url.to_string(),
            page_title: page_title.to_owned(),
            heading: heading.to_owned(),
            eyebrow: eyebrow.to_owned(),
            intro: intro.to_owned(),
            meta_description: meta_description.to_owned(),
            canonical_path: canonical_path.to_owned(),
            active_sidebar: active_sidebar.to_owned(),
            body_html,
            whats_new: if canonical_path == "/docs" {
                release_news_view()?
            } else {
                WhatsNewView::default()
            },
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
                .map(html_escape)
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
                "<tr><td>{}</td><td class=\"statement-ticker\"><strong>{}</strong><small>{}</small></td><td class=\"statement-contract\"><code title=\"{contract}\">{contract}</code></td><td><strong>{}</strong><small>Raw {} · {} decimals</small></td><td>{issuer_match}</td><td>{powers}</td><td>{position_facts_html}</td></tr>",
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
        "<tr><td colspan=\"7\">No registered token holdings were reported.</td></tr>".to_owned()
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
        html_escape(&statement.observed_at)
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
        "<p class=\"statement-fact\">{fact_sentence}</p><section class=\"lookup statement-summary\" aria-labelledby=\"statement-summary-heading\"><div class=\"section-heading\"><h2 id=\"statement-summary-heading\">Statement summary</h2></div><p class=\"statement-label\"><strong>Label:</strong> {label}</p><dl class=\"statement-summary-grid\"><div><dt>Wallets</dt><dd><ul>{wallets_html}</ul></dd></div><div><dt>Chains</dt><dd><ul>{chains_html}</ul></dd></div><div><dt>Observed block and slot range</dt><dd><ul>{positions_html}</ul></dd></div><div><dt>Signer</dt><dd><code>{signer}</code><p class=\"lookup-note\">{signer_notice}</p></dd></div><div><dt>Statement ID</dt><dd><code>{id}</code></dd></div><div><dt>Observed at</dt><dd>{}</dd></div></dl><div class=\"statement-actions\"><a class=\"button secondary-button\" href=\"{json_url}\" download>Download JSON</a><a class=\"button secondary-button\" href=\"{csv_url}\" download>Download CSV</a><a class=\"button secondary-button\" href=\"{verify_url}\">Verify this record</a><button class=\"button secondary-button\" type=\"button\" data-print-statement>Print / Save as PDF</button><form action=\"{recheck_url}\" method=\"post\" hx-boost=\"false\"><button class=\"button primary-button\" type=\"submit\">Re-run and compare</button></form></div><p class=\"lookup-note\">Save the signed JSON for independent verification; cached statement pages are retained for up to 24 hours.</p></section>{compare_html}<section class=\"registry-panel statement-holdings\" aria-labelledby=\"statement-holdings-heading\"><div class=\"section-heading\"><h2 id=\"statement-holdings-heading\">Registered token holdings</h2><span class=\"count-badge\">{} assets</span></div><div class=\"table-wrap\"><table class=\"statement-table\"><thead><tr><th>Chain</th><th>Ticker</th><th>Contract</th><th>Balance</th><th>Issuer match</th><th>Powers summary</th><th>Slot / block</th></tr></thead><tbody>{assets_html}</tbody></table></div></section><p class=\"lookup-note statement-nonclaims\">Balances are on-chain facts at a height, not proof of ownership. QED does not prove backing, custody, reserves, solvency, safety, price, liquidity, or endorsement. Statement pages are public to anyone with their link.</p>",
        html_escape(&statement.observed_at),
        statement.assets.len()
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
        &title,
        "Wallet statement",
        "STATEMENT",
        STATEMENT_PURPOSE,
        "Signed point-in-time token balances and chain positions for selected wallets.",
        &canonical_path,
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
        html_escape(&previous.observed_at),
        html_escape(&current.observed_at),
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
        "Statement verification",
        "Verify this statement.",
        "STATEMENT",
        summary,
        "Verify the signed statement hash, signature, trusted signer, and environment.",
        &format!("/statements/{id}/verify"),
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
        format!("{} (re-run)", previous.label.trim())
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
        "Certificate verification",
        "Verify this record.",
        "CERTIFICATE",
        summary,
        "Check a QED certificate signature, trusted signer, environment, and freshness.",
        &format!("/v/{id}/verify"),
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
        .map(Html)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::pool::PoolError;

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
            let mut facts = crate::domain::powers::PowerFacts::default();
            facts.transient_failure = true;
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
        let body =
            axum::body::to_bytes(response.into_body(), 1024 * 1024).await.expect("token page body");
        assert!(String::from_utf8_lossy(&body).contains("NVDA tokenized"));
        assert!(String::from_utf8_lossy(&body).contains("Signals unavailable (transient)"));

        tokio::time::timeout(std::time::Duration::from_secs(1), started.notified())
            .await
            .expect("cache miss schedules background power read");
        release.notify_one();
        let key = (
            Chain::Solana,
            address.to_owned(),
            state.app.registry_version.load(std::sync::atomic::Ordering::Acquire),
        );
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
    async fn complete_power_facts_use_the_full_ttl_cache() {
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
        let key = (
            Chain::Solana,
            address.to_owned(),
            state.app.registry_version.load(std::sync::atomic::Ordering::Acquire),
        );

        let record = crate::app::powers::inspect(&state.app, Chain::Solana, address)
            .await
            .expect("complete observation");
        assert!(record.unavailable.is_empty());
        assert_eq!(record.source_verified, crate::domain::powers::SourceVerified::Unavailable);
        assert!(state.app.powers_cache.get(&key).await.is_some());
        assert!(state.app.powers_retry_cache.get(&key).await.is_none());
        assert_eq!(crate::app::warm::POWERS_CACHE_TTL, Duration::from_secs(30 * 60));

        let cached = crate::app::powers::inspect(&state.app, Chain::Solana, address)
            .await
            .expect("cached complete observation");
        assert_eq!(cached.observed_at, record.observed_at);
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
        let key = (
            Chain::Solana,
            address.to_owned(),
            state.app.registry_version.load(std::sync::atomic::Ordering::Acquire),
        );
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

        let version = state.app.registry_version.load(std::sync::atomic::Ordering::Acquire);
        let mut cached = powers_record(crate::domain::powers::SourceVerified::Match);
        cached.chain = Chain::Solana;
        cached.contract = nvda.to_owned();
        cached.observed_at = chrono::Utc::now().to_rfc3339();
        state.app.powers_cache.insert((Chain::Solana, nvda.to_owned(), version), cached).await;
        let mut stale_cached = powers_record(crate::domain::powers::SourceVerified::Match);
        stale_cached.chain = Chain::Solana;
        stale_cached.contract = tsla.to_owned();
        stale_cached.observed_at = (chrono::Utc::now() - chrono::Duration::minutes(6)).to_rfc3339();
        state
            .app
            .powers_cache
            .insert((Chain::Solana, tsla.to_owned(), version), stale_cached)
            .await;

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
            .get(&(Chain::Solana, tsla.to_owned(), version))
            .await
            .expect("stale cached record was re-inspected");
        let observed_at = chrono::DateTime::parse_from_rfc3339(&refreshed.observed_at)
            .expect("refreshed observation timestamp")
            .with_timezone(&chrono::Utc);
        assert!(
            chrono::Utc::now().signed_duration_since(observed_at) < chrono::Duration::minutes(1)
        );
        assert!(
            state.app.powers_cache.get(&(Chain::Solana, gme.to_owned(), version)).await.is_none()
        );
    }

    #[tokio::test]
    async fn transient_warm_refresh_keeps_cached_facts_visible_on_token_page() {
        use axum::{body::Body, http::Request};
        use tower::ServiceExt;

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

        let version = state.app.registry_version.load(std::sync::atomic::Ordering::Acquire);
        let mut cached = powers_record(crate::domain::powers::SourceVerified::Match);
        cached.chain = Chain::Solana;
        cached.contract = address.to_owned();
        cached.source_verified_subject = crate::domain::powers::SourceVerifiedSubject::TokenProgram;
        cached.can_seize.push(crate::domain::powers::Reason::new(
            "permanent_delegate",
            "Permanent delegate can transfer or burn tokens.",
        ));
        cached.observed_at = (chrono::Utc::now() - chrono::Duration::minutes(6)).to_rfc3339();
        let cached_time = cached.observed_at.clone();
        let key = (Chain::Solana, address.to_owned(), version);
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

        let response = crate::adapters::web::router(state)
            .oneshot(Request::get("/tokens/NVDA").body(Body::empty()).unwrap())
            .await
            .expect("token page response");
        assert_eq!(response.status(), StatusCode::OK);
        let body =
            axum::body::to_bytes(response.into_body(), 1024 * 1024).await.expect("token page body");
        let body = String::from_utf8_lossy(&body);
        assert!(body.contains("Can seize"));
        assert!(body.contains("Permanent delegate can transfer or burn tokens."));
        assert!(!body.contains("Signals unavailable (transient)"));
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
    fn wallet_holdings_template_renders_genuine_pool_and_unknown_rows() {
        let template = WalletHoldingsTemplate {
            asset_version: 1,
            public_url: "https://qed.example".to_owned(),
            address: "0x0000000000000000000000000000000000000001".to_owned(),
            holdings: vec![
                WalletHoldingView {
                    symbol: "NVDA".to_owned(),
                    amount: "1.5".to_owned(),
                    verdict: "Contract matches issuer registry".to_owned(),
                    verdict_class: "is-verified".to_owned(),
                    chain: "Base".to_owned(),
                    pool_url: None,
                    trade_links: Vec::new(),
                    reason: None,
                },
                WalletHoldingView {
                    symbol: "NVDA".to_owned(),
                    amount: "2".to_owned(),
                    verdict: "Verified · issuer NVDA".to_owned(),
                    verdict_class: "is-verified".to_owned(),
                    chain: "Ethereum".to_owned(),
                    pool_url: Some("/validated/ethereum/pool".to_owned()),
                    trade_links: vec![WalletTradeLinkView {
                        label: "Market pool".to_owned(),
                        url: "https://dex.example/pool".to_owned(),
                    }],
                    reason: None,
                },
                WalletHoldingView {
                    symbol: "Unknown".to_owned(),
                    amount: "3".to_owned(),
                    verdict: "Unknown".to_owned(),
                    verdict_class: "is-unknown".to_owned(),
                    chain: "BNB Chain".to_owned(),
                    pool_url: None,
                    trade_links: Vec::new(),
                    reason: Some("No issuer registry or known pool match.".to_owned()),
                },
            ],
        };
        let rendered = template.render().expect("wallet template renders");
        assert!(rendered.contains("Contract matches issuer registry"));
        assert!(rendered.contains("Verified · issuer NVDA"));
        assert!(rendered.contains("Unknown"));
        assert!(rendered.contains("https://dex.example/pool"));
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

    #[test]
    fn token_directory_contract_cards_render_icons_actions_and_no_decimals() {
        let rendered = TokenTemplate {
            asset_version: 1,
            public_url: "https://qed.example".to_owned(),
            ticker: "NVDA".to_owned(),
            title: "NVDA tokenized".to_owned(),
            contracts: vec![DirectoryContractView {
                issuer: "Issuer".to_owned(),
                ticker: "NVDA".to_owned(),
                chain: "Base".to_owned(),
                chain_kind: Chain::Base,
                chain_icon: "ethereum",
                contract: "0x0000000000000000000000000000000000000002".to_owned(),
                explorer_url: "https://explorer.example/token".to_owned(),
                source_url: "https://issuer.example/nvda".to_owned(),
                powers: powers_view(crate::domain::powers::PowersRecord {
                    chain: Chain::Base,
                    contract: "0x0000000000000000000000000000000000000002".to_owned(),
                    can_seize: Vec::new(),
                    can_block: vec![crate::domain::powers::Reason::new(
                        "pausable",
                        "Paused status could not be read.",
                    )],
                    can_change_rules: vec![crate::domain::powers::Reason::new(
                        "eip1967_implementation",
                        "Implementation source is verified.",
                    )],
                    token_paused: None,
                    sanctions_list: None,
                    unavailable: vec![crate::domain::powers::Reason::new(
                        "rpc_unavailable",
                        "A supported read failed temporarily.",
                    )],
                    source_verified_subject:
                        crate::domain::powers::SourceVerifiedSubject::Implementation,
                    source_verified: crate::domain::powers::SourceVerified::ExactMatch,
                    source_verified_proxy: Some(crate::domain::powers::SourceVerified::Match),
                    observed_at: "2026-10-01T00:00:00Z".to_owned(),
                    block: Some(123),
                    slot: None,
                    reads: Vec::new(),
                }),
            }],
            pools: Vec::new(),
            json_ld: "{}".to_owned(),
        }
        .render()
        .expect("token template renders");
        assert!(rendered.contains(r#"class="contract-card""#));
        assert!(rendered.contains(r#"#ethereum"#));
        assert!(rendered.contains("Explorer"));
        assert!(rendered.contains("unavailable (transient)"));
        assert!(rendered.contains("rpc_unavailable: A supported read failed temporarily."));
        assert!(rendered.contains("Exact match"));
        assert!(rendered.contains("Implementation source"));
        assert!(rendered.contains("proxy: Match"));
        assert!(!rendered.contains("No evidence observed"));
        assert!(rendered.contains("Absence of observed signals is not proof"));
        assert!(rendered.contains("Issuer source"));
        assert!(!rendered.contains("decimals"));
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

    fn render_token_powers(powers: DirectoryPowersView) -> String {
        TokenTemplate {
            asset_version: 1,
            public_url: "https://qed.example".to_owned(),
            ticker: "NVDA".to_owned(),
            title: "NVDA tokenized".to_owned(),
            contracts: vec![DirectoryContractView {
                issuer: "Issuer".to_owned(),
                ticker: "NVDA".to_owned(),
                chain: "Base".to_owned(),
                chain_kind: Chain::Base,
                chain_icon: "ethereum",
                contract: "0x0000000000000000000000000000000000000002".to_owned(),
                explorer_url: "https://explorer.example/token".to_owned(),
                source_url: "https://issuer.example/nvda".to_owned(),
                powers,
            }],
            pools: Vec::new(),
            json_ld: "{}".to_owned(),
        }
        .render()
        .expect("token template renders")
    }

    #[test]
    fn token_power_summary_renders_fact_availability_and_source_badges() {
        for (field, expected_badge, expected_sentence) in [
            ("seize", "Can seize", "the token can be seized"),
            ("block", "Can block", "transfers can be blocked"),
            ("rules", "Can change rules", "token rules can be changed"),
        ] {
            let mut record = powers_record(crate::domain::powers::SourceVerified::None);
            let reason = crate::domain::powers::Reason::new("signal", "observed");
            match field {
                "seize" => record.can_seize.push(reason),
                "block" => record.can_block.push(reason),
                _ => record.can_change_rules.push(reason),
            }
            let rendered = render_token_powers(powers_view(record));
            assert!(rendered.contains(expected_badge));
            assert!(rendered.contains(&format!("Observed controls: {expected_sentence}.")));
            assert!(!rendered.contains("No control signals observed"));
            assert!(rendered.contains("not a safety rating"));
        }

        let no_facts = render_token_powers(powers_view(powers_record(
            crate::domain::powers::SourceVerified::None,
        )));
        assert!(
            no_facts.contains(
                r#"class="powers-summary" role="group" aria-label="Token control signals""#
            )
        );
        assert!(no_facts.contains("No control signals observed"));
        assert!(no_facts.contains(
            "On Base: No control signals were observed; this is not proof that no authority exists."
        ));
        assert!(no_facts.contains("Which contracts did each issuer publish for NVDA?"));

        let mut failed_read = powers_record(crate::domain::powers::SourceVerified::None);
        failed_read
            .unavailable
            .push(crate::domain::powers::Reason::new("rpc", "temporarily unavailable"));
        let unavailable = render_token_powers(powers_view(failed_read));
        assert!(unavailable.contains("Signals unavailable (transient)"));
        assert!(
            unavailable
                .contains("Some control checks were unavailable; retry for a complete reading.")
        );

        for (source, expected_badge) in [
            (crate::domain::powers::SourceVerified::ExactMatch, "Source verified (exact match)"),
            (crate::domain::powers::SourceVerified::Match, "Source verified (match)"),
            (crate::domain::powers::SourceVerified::None, "Source unverified"),
            (crate::domain::powers::SourceVerified::Unavailable, "Source unavailable"),
        ] {
            let rendered = render_token_powers(powers_view(powers_record(source)));
            assert!(rendered.contains(expected_badge), "{expected_badge}");
        }

        let missing_record = render_token_powers(DirectoryPowersView::unavailable());
        assert!(missing_record.contains("Signals unavailable (transient)"));
        assert!(missing_record.contains("Source unavailable"));
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

    #[test]
    fn homepage_links_powers_and_preserves_protected_statements() {
        let rendered = IndexTemplate {
            asset_version: 1,
            public_url: "https://qed.example".to_owned(),
            stats_line: "0 pools checked · 0 issuer matches · 0 mismatches · 0 not read yet"
                .to_owned(),
            leaderboard: super::super::views::LeaderboardPageView::from_value(serde_json::json!({
                "entries": []
            })),
            whats_new: super::super::views::WhatsNewView::default(),
        }
        .render()
        .expect("homepage template renders");

        assert!(rendered.contains(
            "A ticker is not a contract. Compare the pool with the issuer's published stock-token contract."
        ));
        assert!(rendered.contains(
            "QED checks whether a pool uses the stock-token contract published by its issuer."
        ));
        assert!(rendered.contains(
            r#"<a href="/tokens/NVDA">QED also shows what the issuer can do to each token.</a>"#
        ));
        assert!(rendered.contains(r#"<a href="/stats">Stats</a>"#));
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
        for heading in [
            "Chain",
            "Ticker",
            "Contract",
            "Balance",
            "Issuer match",
            "Powers summary",
            "Slot / block",
        ] {
            assert!(html.contains(&format!("<th>{heading}</th>")));
        }
        assert!(html.contains("<strong>Label:</strong> Q3 &lt;investor&gt; report"));
        assert!(html.contains("href=\"/statements/statement-id/download.csv\" download"));
        assert!(html.contains("href=\"/statements/statement-id/verify\""));
        assert!(html.contains(
            "Verify this record: <a href=\"/statements/statement-id/verify\">/statements/statement-id/verify</a>"
        ));
        assert!(html.contains("action=\"/statements/statement-id/recheck\""));
        assert!(html.contains("data-print-statement"));
        assert!(html.contains("Observed: 2026-10-04T00:00:00Z"));
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
