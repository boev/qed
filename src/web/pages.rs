use super::views::{
    CertificateResultTemplate, CertificateTemplate, CertificateView, ChainTemplate,
    CheckResultPageTemplate, CheckTemplate, DirectoryContractView, DirectoryPoolView,
    FeaturedTemplate, FeaturedView, GlossaryTemplate, GuideVerifyTemplate, IndexTemplate,
    RegistryTableQuery, RegistryTableTemplate, RegistryTemplate, ResultTemplate, TokenTemplate,
    ValidatedCardView, ValidatedTemplate, WalletHoldingView, WalletHoldingsTemplate,
    WalletTemplate, WalletTradeLinkView,
};
use super::{ASSET_VERSION, render_page};
use crate::{
    attest,
    chain::Chain,
    check::{self, Verdict},
    pool::{PoolError, WalletHolding},
    registry::{self, Entry},
    state::AppState,
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
pub(crate) async fn index(
    State(state): State<AppState>,
    _headers: HeaderMap,
) -> Result<Html<String>, StatusCode> {
    let leaderboard = state.leaderboard.read().await;
    let mut value = serde_json::to_value(&*leaderboard)
        .unwrap_or_else(|_| serde_json::json!({ "entries": [] }));
    if let Some(object) = value.as_object_mut() {
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
        },
        false,
    )
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct TokenLookupQuery {
    pub(crate) ticker: Option<String>,
}

fn canonical_ticker(registry: &registry::Registry, query: &str) -> Option<String> {
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
    let holdings = wallet_holdings(&state, &address).await?;
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

const WALLET_DEADLINE: Duration = Duration::from_secs(20);

pub(crate) async fn wallet_holdings(
    state: &AppState,
    address: &str,
) -> Result<Vec<WalletHoldingView>, StatusCode> {
    tokio::time::timeout(WALLET_DEADLINE, wallet_holdings_inner(state, address))
        .await
        .unwrap_or(Err(StatusCode::GATEWAY_TIMEOUT))
}

async fn wallet_holdings_inner(
    state: &AppState,
    address: &str,
) -> Result<Vec<WalletHoldingView>, StatusCode> {
    if address.is_empty() || address.len() > 128 {
        return Err(StatusCode::NOT_FOUND);
    }
    let Some(chains) = wallet_chains(address) else {
        return Err(StatusCode::NOT_FOUND);
    };
    let registry = state.registry.read().await.clone();
    let leaderboard = state.leaderboard.read().await.clone();
    let attestations =
        state.attestations.read().ok().map(|value| value.clone()).unwrap_or_default();
    let mut known_pools = Vec::new();
    for entry in &leaderboard.entries {
        let Some(chain) = super::views::parse_chain(&entry.chain) else { continue };
        let attestation =
            entry.attestation_id.as_ref().and_then(|id| attestations.get(id)).or_else(|| {
                attestations.values().find(|value| {
                    value.chain == chain && value.pool.pool.eq_ignore_ascii_case(&entry.pool)
                })
            });
        let Some(attestation) = attestation else { continue };
        let trade_url =
            crate::discovery::canonical_market_url(chain, &entry.pool, Some(&entry.trade_url));
        for token in [&attestation.pool.base, &attestation.pool.quote] {
            known_pools.push(KnownWalletPool {
                chain,
                token_address: token.address.clone(),
                symbol: token.symbol.clone(),
                pool: entry.pool.clone(),
                pool_url: entry.detail_url.clone(),
                trade_url: trade_url.clone(),
            });
        }
    }
    let (solana, robinhood, base, ethereum, bnb) = tokio::join!(
        wallet_chain_holdings(
            state,
            address,
            Chain::Solana,
            chains.contains(&Chain::Solana),
            &registry,
            &known_pools,
        ),
        wallet_chain_holdings(
            state,
            address,
            Chain::RobinhoodChain,
            chains.contains(&Chain::RobinhoodChain),
            &registry,
            &known_pools,
        ),
        wallet_chain_holdings(
            state,
            address,
            Chain::Base,
            chains.contains(&Chain::Base),
            &registry,
            &known_pools,
        ),
        wallet_chain_holdings(
            state,
            address,
            Chain::Ethereum,
            chains.contains(&Chain::Ethereum),
            &registry,
            &known_pools,
        ),
        wallet_chain_holdings(
            state,
            address,
            Chain::Bnb,
            chains.contains(&Chain::Bnb),
            &registry,
            &known_pools,
        ),
    );
    let mut result = Vec::new();
    for holdings in [solana, robinhood, base, ethereum, bnb] {
        result.extend(holdings?);
    }
    result.sort_by(|left, right| {
        left.chain.cmp(&right.chain).then_with(|| left.symbol.cmp(&right.symbol))
    });
    Ok(result)
}

async fn wallet_chain_holdings(
    state: &AppState,
    address: &str,
    chain: Chain,
    enabled: bool,
    registry: &registry::Registry,
    known_pools: &[KnownWalletPool],
) -> Result<Vec<WalletHoldingView>, StatusCode> {
    if !enabled {
        return Ok(Vec::new());
    }
    let Some(reader) = state.readers.iter().find(|reader| reader.chain() == chain) else {
        return Ok(Vec::new());
    };
    let mut known_tokens: Vec<String> = registry
        .iter()
        .filter(|entry| entry.chain == chain && registry::matchable(entry))
        .map(|entry| entry.contract.clone())
        .collect();
    known_tokens.extend(
        known_pools
            .iter()
            .filter(|pool| pool.chain == chain)
            .map(|pool| pool.token_address.clone()),
    );
    known_tokens.sort();
    known_tokens.dedup();
    let holdings = reader.wallet_holdings(address, &known_tokens).await.map_err(|error| {
        if matches!(error, PoolError::BudgetExceeded(_)) {
            StatusCode::SERVICE_UNAVAILABLE
        } else {
            StatusCode::BAD_GATEWAY
        }
    })?;
    let mut views = Vec::with_capacity(holdings.len());
    for holding in holdings {
        views.push(wallet_holding_view(state, registry, known_pools, holding).await);
    }
    Ok(views)
}

fn wallet_chains(address: &str) -> Option<Vec<Chain>> {
    if address.is_empty() || address.len() > 128 {
        return None;
    }
    if Chain::detect(address) == Some(Chain::Solana) {
        Some(vec![Chain::Solana])
    } else if Chain::is_evm_address(address) {
        Some(vec![Chain::RobinhoodChain, Chain::Base, Chain::Ethereum, Chain::Bnb])
    } else {
        None
    }
}

#[derive(Debug, Clone)]
struct KnownWalletPool {
    chain: Chain,
    token_address: String,
    symbol: Option<String>,
    pool: String,
    pool_url: String,
    trade_url: String,
}

async fn wallet_holding_view(
    state: &AppState,
    registry: &registry::Registry,
    known_pools: &[KnownWalletPool],
    holding: WalletHolding,
) -> WalletHoldingView {
    let registry_entry = registry::lookup(registry, holding.chain, &holding.token_address);
    let pools: Vec<_> = known_pools
        .iter()
        .filter(|pool| {
            pool.chain == holding.chain
                && same_wallet_token(&pool.token_address, &holding.token_address, holding.chain)
        })
        .collect();
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
    if let Some(entry) = registry_entry {
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
    if let Some(pool) = pools.first() {
        let checked = check::check(state, &pool.pool).await;
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

fn same_wallet_token(left: &str, right: &str, chain: Chain) -> bool {
    if chain == Chain::Solana { left == right } else { left.eq_ignore_ascii_case(right) }
}

fn format_wallet_amount(raw: &str, decimals: Option<u8>) -> String {
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

pub(crate) async fn check_page(
    State(state): State<AppState>,
    _headers: HeaderMap,
) -> Result<Html<String>, StatusCode> {
    render_page(
        CheckTemplate { asset_version: ASSET_VERSION, public_url: state.public_url.to_string() },
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
        GlossaryTemplate { asset_version: ASSET_VERSION, public_url: state.public_url.to_string() },
        false,
    )
}

pub(crate) async fn guide_verify_page(
    State(state): State<AppState>,
    _headers: HeaderMap,
) -> Result<Html<String>, StatusCode> {
    render_page(
        GuideVerifyTemplate {
            asset_version: ASSET_VERSION,
            public_url: state.public_url.to_string(),
        },
        false,
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
        chain_icon: super::views::chain_icon(entry.chain),
        contract: entry.contract.clone(),
        explorer_url: super::views::explorer_link(entry.chain, &entry.contract),
        source_url: entry.source_url.clone(),
    }
}

fn pool_view(entry: &crate::discovery::LeaderboardEntry) -> DirectoryPoolView {
    let verdict = entry.verdict.to_ascii_lowercase();
    let (verdict, verdict_class) = match verdict.as_str() {
        "verified" => ("Verified".to_owned(), "is-verified".to_owned()),
        "mismatch" => ("Mismatch".to_owned(), "is-mismatch".to_owned()),
        "nomatch" => ("No match".to_owned(), "is-mismatch".to_owned()),
        _ => ("Unknown".to_owned(), "is-unknown".to_owned()),
    };
    let trade_url = super::views::parse_chain(&entry.chain)
        .map(|chain| {
            crate::discovery::canonical_market_url(chain, &entry.pool, Some(&entry.trade_url))
        })
        .unwrap_or_default();
    DirectoryPoolView {
        chain: entry.chain_label.clone(),
        dex: entry.dex.clone(),
        pair: format!("{}/{}", entry.base_symbol, entry.quote_symbol),
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
    let mut cards = super::views::verified_attestations(&state)
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
    State(state): State<AppState>,
    Path((chain_name, subject)): Path<(String, String)>,
    _headers: HeaderMap,
) -> Result<Html<String>, StatusCode> {
    let chain = super::views::parse_chain(&chain_name).ok_or(StatusCode::NOT_FOUND)?;
    let attestation = attest::latest_for_pool_async(&state, chain, &subject).await;
    if let Some(attestation) = attestation {
        return render_page(
            CertificateTemplate {
                asset_version: ASSET_VERSION,
                public_url: state.public_url.to_string(),
                certificate: CertificateView::from_live_attestation(attestation),
            },
            false,
        );
    }
    let result = check::check(&state, &subject).await;
    let attestation = match result.attestation_id.as_deref() {
        Some(id) => attest::get_async(&state, id).await,
        None => None,
    };
    let result = ResultTemplate::from_check(result, attestation)
        .render()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    render_page(
        CheckResultPageTemplate {
            asset_version: ASSET_VERSION,
            public_url: state.public_url.to_string(),
            canonical_path: format!("/validated/{}/{}", super::views::chain_slug(chain), subject),
            result,
        },
        false,
    )
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

#[derive(Debug, Deserialize)]
pub(crate) struct CheckForm {
    address: String,
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

pub(crate) async fn check_form(
    State(state): State<AppState>,
    Form(form): Form<CheckForm>,
) -> Result<Html<String>, StatusCode> {
    let address = form.address.trim();
    let template = if address.is_empty()
        || (Chain::detect(address).is_none()
            && !Chain::is_evm_address(address)
            && !Chain::is_v4_pool_id(address))
    {
        ResultTemplate::invalid()
    } else {
        let result = check::check(&state, address).await;
        let attestation = match result.attestation_id.as_deref() {
            Some(id) => attest::get_async(&state, id).await,
            None => None,
        };
        ResultTemplate::from_check(result, attestation)
    };
    template.render().map(Html).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

pub(crate) async fn featured(State(state): State<AppState>) -> Result<Html<String>, StatusCode> {
    let featured = state.featured.read().await;
    FeaturedTemplate { pools: featured.iter().take(4).map(FeaturedView::from_pool).collect() }
        .render()
        .map(Html)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

pub(crate) async fn certificate(
    State(state): State<AppState>,
    Path(id): Path<String>,
    _headers: HeaderMap,
) -> Result<Html<String>, StatusCode> {
    let Some(attestation) = attest::get_certificate_async(&state, &id).await else {
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

pub(crate) async fn recheck_certificate(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<RecheckQuery>,
) -> Result<Html<String>, StatusCode> {
    let recheck = attest::recheck(&state, &id).await;
    let selected = if recheck.fresh_id.is_empty() { id } else { recheck.fresh_id.clone() };
    let Some(attestation) = attest::get_async(&state, &selected).await else {
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
        };
        assert_eq!(canonical_ticker(&vec![entry.clone()], " nvda "), Some("NVDA".to_owned()));
        let response = token_redirect(&vec![entry.clone()], Some(" nvda ")).unwrap().into_response();
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
    fn check_page_renders_distinct_ticker_lookup_form() {
        let rendered = CheckTemplate { asset_version: 1, public_url: "https://qed.example".to_owned() }
            .render()
            .expect("check template renders");
        assert!(rendered.contains(r#"id="ticker-lookup""#));
        assert!(rendered.contains(r#"method="get" action="/tokens""#));
        assert!(rendered.contains(r#"name="ticker""#));
        assert!(rendered.contains(r#"hx-post="/check""#));
    }

    #[test]
    fn token_directory_contracts_have_intentional_chain_order() {
        let view = |chain: &str, issuer: &str| DirectoryContractView {
            issuer: issuer.to_owned(),
            ticker: "NVDA".to_owned(),
            chain: chain.to_owned(),
            chain_icon: "ethereum",
            contract: format!("0x{issuer}"),
            explorer_url: "https://explorer.example".to_owned(),
            source_url: "https://issuer.example".to_owned(),
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
                chain: "Solana".to_owned(),
                chain_icon: "solana",
                contract: "So11111111111111111111111111111111111111112".to_owned(),
                explorer_url: "https://explorer.example/token".to_owned(),
                source_url: "https://issuer.example/nvda".to_owned(),
            }],
            pools: Vec::new(),
            json_ld: "{}".to_owned(),
        }
        .render()
        .expect("token template renders");
        assert!(rendered.contains(r#"class="contract-card""#));
        assert!(rendered.contains(r#"#solana"#));
        assert!(rendered.contains("Explorer"));
        assert!(rendered.contains("Issuer source"));
        assert!(!rendered.contains("decimals"));
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
}
