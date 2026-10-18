use super::{
    ASSET_VERSION, render_page,
    views::{DirectoryPowersView, TokenTemplate},
};
use crate::{
    adapters::state::AppState,
    app::context::Context,
    domain::{
        asset_history::{AssetKey, AssetState, Facet, Observation, Outcome, SubjectKind},
        chain::Chain,
    },
};
use axum::{
    extract::{Path, Query, State},
    http::{StatusCode, header},
    response::{Html, IntoResponse, Response},
};
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
pub(crate) struct HistoryQuery {
    days: Option<u32>,
    before_time: Option<DateTime<Utc>>,
    before_id: Option<String>,
    limit: Option<usize>,
}
#[derive(Debug, Default)]
pub(crate) struct HistoryView {
    pub path: String,
    pub days: u32,
    pub notice: String,
    pub assets: Vec<AssetView>,
    pub rows: Vec<PointView>,
    pub lanes: Vec<LaneView>,
    pub height: usize,
    pub next: String,
    pub exact: bool,
    pub points: Vec<PointView>,
    pub ticks: Vec<TickView>,
    pub legend: String,
}
#[derive(Debug)]
pub(crate) struct AssetView {
    pub path: String,
    pub address: String,
    pub chain: String,
    pub kind: &'static str,
    pub summary: String,
    pub guard_url: String,
    pub proof_url: String,
    pub short_address: String,
    pub label: String,
    pub tone: &'static str,
    pub confirmed: String,
    pub next_check: String,
    pub misses: u32,
    pub facts: Vec<FactView>,
    pub controls: Vec<String>,
}
#[derive(Debug)]
pub(crate) struct FactView {
    pub label: &'static str,
    pub value: String,
    pub address: String,
    pub matched: bool,
}
#[derive(Debug)]
pub(crate) struct TickView {
    pub x: usize,
    pub mobile_x: usize,
    pub label: String,
}
#[derive(Debug, Clone)]
pub(crate) struct PointView {
    pub id: String,
    pub time: String,
    pub human_time: String,
    observed_at: DateTime<Utc>,
    pub asset: String,
    pub facet: &'static str,
    pub outcome: &'static str,
    pub reason: String,
    pub position: String,
    pub href: String,
    pub title: String,
    pub x: usize,
    pub mobile_x: usize,
    pub y: usize,
    pub shape: &'static str,
    pub class: &'static str,
    pub count: usize,
}
#[derive(Debug)]
pub(crate) struct LaneView {
    pub label: String,
    pub y: usize,
    pub label_y: usize,
}
fn facet_label(facet: Facet) -> &'static str {
    match facet {
        Facet::ContractMatch => "Contract match",
        Facet::Powers => "Controls",
        Facet::Guard => "Guard",
    }
}
fn summary(state: Option<&AssetState>) -> String {
    state.map_or_else(|| "Not read yet".into(), AssetState::summary)
}
fn label_slug(label: &str) -> &'static str {
    match label {
        "Verified" => "verified",
        "Mismatch" => "mismatch",
        "No match" => "nomatch",
        _ => "unknown",
    }
}
fn fallback_label(verdict: &str, read_status: &str) -> &'static str {
    if read_status == "not_read_yet" {
        return "Not read yet";
    }
    if read_status == "unsupported_venue" {
        return "Unsupported";
    }
    match verdict {
        "verified" => "Verified",
        "mismatch" => "Mismatch",
        "nomatch" => "No match",
        _ => "Unknown",
    }
}
pub(super) fn fallback_summary(entry: &super::super::discovery::LeaderboardEntry) -> String {
    let label = fallback_label(&entry.verdict, &entry.read_status);
    let mut text = label.to_owned();
    if matches!(label, "Verified" | "Mismatch" | "No match")
        && let Some(time) =
            entry.checked_at.as_deref().and_then(|time| DateTime::parse_from_rfc3339(time).ok())
    {
        text.push_str(&format!(" · last confirmed {}", time.format("%-d %b %Y, %H:%M UTC")));
    }
    text
}
pub(super) fn fill_pool_fallbacks(
    context: &Context,
    entries: &[super::super::discovery::LeaderboardEntry],
    history: &mut HistoryView,
) {
    for entry in entries {
        let Some(key) = Chain::parse(&entry.chain)
            .and_then(|chain| AssetKey::new(chain, SubjectKind::Pool, &entry.pool))
        else {
            continue;
        };
        if context.history.state(&key, Facet::ContractMatch).is_some() {
            continue;
        }
        let path = key.path();
        if let Some(asset) = history.assets.iter_mut().find(|asset| asset.path == path) {
            asset.summary = fallback_summary(entry);
            asset.label = fallback_label(&entry.verdict, &entry.read_status).into();
            asset.confirmed = entry
                .checked_at
                .as_deref()
                .and_then(|time| DateTime::parse_from_rfc3339(time).ok())
                .map(|time| human_time(time.with_timezone(&Utc)))
                .unwrap_or_else(|| "No completed read".into());
            if let Some(guard) = context.history.state(&key, Facet::Guard) {
                asset.summary.push_str(&format!(" · public Guard: {}", guard.summary()));
            }
            asset.proof_url = entry
                .attestation_id
                .as_ref()
                .map(|id| format!("/v/{id}/verify"))
                .unwrap_or_default();
        }
    }
}
/// Add an unsigned display projection; raw latest-attempt fields stay unchanged.
pub(crate) fn decorate_entries(context: &Context, value: &mut serde_json::Value) {
    let Some(entries) = value.get_mut("entries").and_then(serde_json::Value::as_array_mut) else {
        return;
    };
    for entry in entries {
        let key =
            entry.get("chain").and_then(serde_json::Value::as_str).and_then(Chain::parse).and_then(
                |chain| AssetKey::new(chain, SubjectKind::Pool, entry.get("pool")?.as_str()?),
            );
        let state = key.as_ref().and_then(|key| context.history.state(key, Facet::ContractMatch));
        let state = state.as_ref();
        let fallback = fallback_label(
            entry.get("verdict").and_then(serde_json::Value::as_str).unwrap_or(""),
            entry.get("read_status").and_then(serde_json::Value::as_str).unwrap_or("not_read_yet"),
        );
        let label = state.map_or(fallback, AssetState::label);
        let text = state.map_or_else(|| label.to_owned(), AssetState::summary);
        entry["display_label"] = label.into();
        entry["display_note"] = state
            .filter(|state| state.consecutive_misses > 0)
            .map(|state| {
                state.summary().trim_start_matches(label).trim_start_matches(" · ").to_owned()
            })
            .unwrap_or_default()
            .into();
        entry["display_summary"] = text.into();
        entry["display_verdict"] = label_slug(label).into();
        entry["display_confirmed_at"] = state.map_or_else(
            || {
                if matches!(label, "Verified" | "Mismatch" | "No match") {
                    entry.get("checked_at").cloned().unwrap_or(serde_json::Value::Null)
                } else {
                    serde_json::Value::Null
                }
            },
            |state| {
                state.last_confirmed.as_ref().map(|event| event.observed_at.to_rfc3339()).into()
            },
        );
        if let Some(key) = key {
            entry["detail_url"] = key.path().into();
        }
    }
}
pub(crate) async fn prepare(
    context: &Context,
    assets: Vec<AssetKey>,
    path: String,
    query: &HistoryQuery,
) -> HistoryView {
    let now = context.clock.now();
    let mut inventories = std::collections::HashMap::new();
    for key in &assets {
        if let Ok(inventory) = context.history.inventory(key).await {
            inventories.insert(key.clone(), inventory);
        }
    }
    let oldest = inventories.values().filter_map(|(first, _)| *first).min().unwrap_or(now);
    let days = query.days.filter(|days| [1, 7, 30, 90].contains(days)).unwrap_or_else(|| {
        [1, 7, 30, 90]
            .into_iter()
            .find(|days| oldest >= now - Duration::days(i64::from(*days)))
            .unwrap_or(90)
    });
    let since = now - Duration::days(i64::from(days));
    let limit = query.limit.unwrap_or(200).clamp(1, 200);
    let cursor = query.before_time.zip(query.before_id.clone());
    let mut events =
        context.history.history(&assets, since, cursor, limit + 1).await.unwrap_or_default();
    let has_more = events.len() > limit;
    events.truncate(limit);
    let exact = path.starts_with("/assets/");
    let mut view = HistoryView { path, days, exact, height: 58, ..HistoryView::default() };
    view.notice = context.history.degraded_since().map(|since| format!("History recording degraded since {}. Local outbox retained; exports may be incomplete.", human_time(since))).unwrap_or_else(|| if context.history.durable() { String::new() } else { "In-memory history · non-durable; resets on restart.".into() });
    for key in &assets {
        let facet =
            if key.kind == SubjectKind::Pool { Facet::ContractMatch } else { Facet::Powers };
        let state = context.history.state(key, facet);
        let label = state.as_ref().map_or("Not read yet", AssetState::label).to_owned();
        let misses = state.as_ref().map_or(0, |state| state.consecutive_misses);
        let confirmed_at = state
            .as_ref()
            .and_then(|state| state.last_confirmed.as_ref())
            .map(|event| event.observed_at);
        let next = state.as_ref().and_then(|state| state.next_retry_at);
        let (first, count) = inventories.get(key).copied().unwrap_or_default();
        let facts = vec![
            fact("Chain", key.chain.to_string()),
            fact("Kind", if key.kind == SubjectKind::Pool { "Pool" } else { "Token" }),
            fact("First seen", first.map(human_time).unwrap_or_else(|| "Not recorded".into())),
            fact("Observations", count.to_string()),
            fact(
                "Last attempt",
                state
                    .as_ref()
                    .and_then(|state| state.latest.as_ref())
                    .map(|event| human_time(event.observed_at))
                    .unwrap_or_else(|| "Not recorded".into()),
            ),
        ];
        view.assets.push(AssetView {
            path: key.path(),
            address: key.address.clone(),
            short_address: short_address(&key.address),
            chain: key.chain.to_string(),
            kind: if key.kind == SubjectKind::Pool { "pool" } else { "token" },
            summary: summary(state.as_ref()),
            label: if (1..3).contains(&misses) {
                format!("Retrying {misses}/3")
            } else {
                label.clone()
            },
            tone: if matches!(label.as_str(), "Mismatch" | "Deny") { "deny" } else { "unknown" },
            confirmed: confirmed_at.map(human_time).unwrap_or_else(|| "No completed read".into()),
            next_check: next
                .map(|time| if time <= now { "due now".into() } else { human_time(time) })
                .unwrap_or_else(|| {
                    confirmed_at
                        .map(|time| {
                            format!(
                                "{} – {} (6–24 h cadence)",
                                human_time(time + Duration::hours(6)),
                                human_time(time + Duration::hours(24))
                            )
                        })
                        .unwrap_or_else(|| "on next read".into())
                }),
            misses,
            facts,
            controls: Vec::new(),
            guard_url: format!(
                "/guard/{}/{}",
                super::super::discovery::chain_slug(key.chain),
                key.address
            ),
            proof_url: state
                .as_ref()
                .and_then(|state| {
                    state
                        .latest
                        .as_ref()
                        .filter(|event| event.attestation_id.is_some())
                        .or(state.last_confirmed.as_ref())
                })
                .and_then(|event| event.attestation_id.as_ref())
                .map(|id| format!("/v/{id}/verify"))
                .unwrap_or_default(),
        });
        let facets: &[Facet] =
            if exact { &[Facet::ContractMatch, Facet::Powers, Facet::Guard] } else { &[facet] };
        for lane_facet in facets {
            let mut lane: Vec<_> = events
                .iter()
                .filter(|event| event.asset == *key && (!exact || event.facet == *lane_facet))
                .collect();
            if lane.is_empty() {
                continue;
            }
            lane.reverse();
            let y = view.height;
            let label = if exact {
                facet_label(*lane_facet).into()
            } else {
                format!(
                    "{} · {} · {}",
                    key.chain,
                    if key.kind == SubjectKind::Pool { "pool" } else { "token" },
                    short_address(&key.address)
                )
            };
            view.lanes.push(LaneView { label, y, label_y: y - 16 });
            for event in lane {
                let x = 280
                    + ((event.observed_at - since).num_seconds().clamp(0, i64::from(days) * 86400)
                        as u64
                        * 590
                        / (u64::from(days) * 86400)) as usize;
                let row = point(event, x, y);
                // Merge crowded points on the same horizontal lane, never invent vertical time.
                if let Some(previous) =
                    view.points.last_mut().filter(|point| point.y == y && x < point.x + 14)
                {
                    previous.count += 1;
                    previous.title.push_str(&format!("; {}", row.title));
                    if previous.shape != row.shape {
                        previous.shape = "◇";
                        previous.class = "history-result";
                    }
                } else {
                    view.points.push(row.clone());
                }
                view.rows.push(row);
            }
            view.height += 44;
        }
    }
    for index in 0..=4 {
        let time = since + Duration::seconds(i64::from(days) * 86400 * index / 4);
        view.ticks.push(TickView {
            x: 280 + index as usize * 590 / 4,
            mobile_x: 32 + index as usize * 326 / 4,
            label: if days == 1 {
                time.format("%H:%M").to_string()
            } else {
                time.format("%-d %b").to_string()
            },
        });
    }
    view.height += 16;
    let mut legend = Vec::new();
    for (shape, label) in [
        ("●", "Match"),
        ("×", "Contradiction"),
        ("■", "No match"),
        ("○", "Miss / incomplete"),
        ("◆", "Controls"),
        ("▲", "Public Guard"),
        ("!", "Unavailable"),
        ("?", "Unknown"),
        ("◇", "Grouped observations"),
    ] {
        if view.points.iter().any(|point| point.shape == shape) {
            legend.push(format!("{shape} {label}"));
        }
    }
    view.legend = legend.join(" · ");
    view.rows.sort_by(|a, b| b.observed_at.cmp(&a.observed_at).then_with(|| b.id.cmp(&a.id)));
    if has_more && let Some(last) = events.last() {
        view.next = format!(
            "?days={days}&before_time={}&before_id={}",
            last.observed_at.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true),
            last.event_id
        );
    }
    view
}
fn fact(label: &'static str, value: impl Into<String>) -> FactView {
    FactView { label, value: value.into(), address: String::new(), matched: false }
}
fn human_time(time: DateTime<Utc>) -> String {
    time.format("%-d %b %Y, %H:%M UTC").to_string()
}
fn short_address(address: &str) -> String {
    if address.len() > 16 {
        format!("{}…{}", &address[..6], &address[address.len() - 4..])
    } else {
        address.to_owned()
    }
}

pub(crate) fn fill_facts(
    context: &Context,
    registry: &crate::domain::registry::Registry,
    contracts: &[super::views::DirectoryContractView],
    entries: &[super::super::discovery::LeaderboardEntry],
    history: &mut HistoryView,
) {
    for asset in &mut history.assets {
        let Some(key) = Chain::parse(&asset.chain).and_then(|chain| {
            AssetKey::new(
                chain,
                if asset.kind == "pool" { SubjectKind::Pool } else { SubjectKind::Token },
                &asset.address,
            )
        }) else {
            continue;
        };
        if let Some(contract) = contracts.iter().find(|contract| {
            contract.chain_kind == key.chain
                && crate::domain::check::same_contract(&contract.contract, &key.address, key.chain)
        }) {
            asset.controls = contract.powers.summary_badges.clone();
            asset.controls.push(contract.powers.source_badge.clone());
        }
        if key.kind == SubjectKind::Token {
            let entry = crate::domain::registry::lookup(registry, key.chain, &key.address);
            asset.facts.push(FactView {
                label: "Registry match",
                value: entry
                    .map(|entry| entry.issuer.clone())
                    .unwrap_or_else(|| "No publisher known".into()),
                address: String::new(),
                matched: entry.is_some(),
            });
            continue;
        }
        let projection = context.history.state(&key, Facet::ContractMatch);
        let pool = projection
            .as_ref()
            .and_then(|state| state.last_confirmed.as_ref())
            .and_then(|event| event.attestation_id.as_ref())
            .and_then(|id| {
                context
                    .attestations
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get(id)
                    .map(|attestation| attestation.pool.clone())
            });
        let listing = entries.iter().find(|entry| {
            Chain::parse(&entry.chain) == Some(key.chain)
                && crate::domain::check::same_contract(&entry.pool, &key.address, key.chain)
        });
        let base_symbol = pool
            .as_ref()
            .and_then(|pool| pool.base.symbol.as_deref())
            .filter(|symbol| !symbol.is_empty())
            .or_else(|| listing.map(|entry| entry.base_symbol.as_str()).filter(|s| !s.is_empty()));
        let quote_symbol = pool
            .as_ref()
            .and_then(|pool| pool.quote.symbol.as_deref())
            .filter(|symbol| !symbol.is_empty())
            .or_else(|| listing.map(|entry| entry.quote_symbol.as_str()).filter(|s| !s.is_empty()));
        asset.facts.insert(
            2,
            fact(
                "Venue",
                pool.as_ref()
                    .map(|pool| pool.dex.clone())
                    .or_else(|| listing.map(|entry| entry.dex.clone()))
                    .unwrap_or_else(|| "Not recorded".into()),
            ),
        );
        asset.facts.insert(
            3,
            fact(
                "Pair",
                if base_symbol.is_some() || quote_symbol.is_some() {
                    format!(
                        "{} / {}",
                        base_symbol.unwrap_or("Not recorded"),
                        quote_symbol.unwrap_or("Not recorded")
                    )
                } else {
                    "Not recorded".into()
                },
            ),
        );
        for (label, side, symbol) in [
            ("Base token", pool.as_ref().map(|pool| &pool.base), base_symbol),
            ("Quote token", pool.as_ref().map(|pool| &pool.quote), quote_symbol),
        ] {
            asset.facts.push(match side {
                Some(side) => FactView {
                    label,
                    value: format!(
                        "{} · {}",
                        symbol.unwrap_or("Token"),
                        short_address(&side.address)
                    ),
                    address: side.address.clone(),
                    matched: crate::domain::registry::lookup(registry, key.chain, &side.address)
                        .is_some(),
                },
                None => fact(label, "Not recorded"),
            });
        }
    }
}
fn point(event: &Observation, x: usize, y: usize) -> PointView {
    let reason = event.miss_reason.clone().unwrap_or_default();
    let position = event
        .block
        .map(|block| format!("block {block}"))
        .or_else(|| event.slot.map(|slot| format!("slot {slot}")))
        .unwrap_or_else(|| "position unavailable".into());
    let href = event
        .attestation_id
        .as_ref()
        .map(|id| format!("/v/{id}"))
        .unwrap_or_else(|| format!("{}/observations/{}", event.asset.path(), event.event_id));
    let time = event.observed_at.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true);
    let title = format!(
        "{time} · {} · {} · {reason} · {position} · {}",
        facet_label(event.facet),
        event.outcome.label(),
        event.attestation_id.as_deref().unwrap_or(&event.event_id)
    );
    let (shape, class) = match event.outcome {
        Outcome::Miss | Outcome::ControlsPartial => ("○", "history-miss"),
        Outcome::Mismatch | Outcome::Deny => ("×", "history-contradiction"),
        Outcome::NoMatch => ("■", "history-result"),
        Outcome::RegistryStale | Outcome::RegistryRemoved | Outcome::Unsupported => {
            ("!", "history-unavailable")
        }
        Outcome::Unknown | Outcome::Rejected => ("?", "history-unavailable"),
        _ if event.facet == Facet::Powers => ("◆", "history-result"),
        _ if event.facet == Facet::Guard => ("▲", "history-result"),
        _ => ("●", "history-result"),
    };
    PointView {
        id: event.event_id.clone(),
        time,
        human_time: human_time(event.observed_at),
        observed_at: event.observed_at,
        asset: format!("{} {}", event.asset.chain, short_address(&event.asset.address)),
        facet: facet_label(event.facet),
        outcome: event.outcome.label(),
        reason,
        position,
        href,
        title,
        x,
        mobile_x: 32 + x.saturating_sub(280) * 326 / 590,
        y,
        shape,
        class,
        count: 1,
    }
}
fn parse_key(chain: &str, kind: &str, address: &str) -> Result<AssetKey, StatusCode> {
    let chain = Chain::parse(chain).ok_or(StatusCode::NOT_FOUND)?;
    let kind = match kind {
        "pool" => SubjectKind::Pool,
        "token" => SubjectKind::Token,
        _ => return Err(StatusCode::NOT_FOUND),
    };
    AssetKey::new(chain, kind, address).ok_or(StatusCode::NOT_FOUND)
}
pub(crate) async fn page(
    State(state): State<AppState>,
    Path((chain, kind, address)): Path<(String, String, String)>,
    Query(query): Query<HistoryQuery>,
) -> Result<Html<String>, StatusCode> {
    let key = parse_key(&chain, &kind, &address)?;
    let registry = state.registry.read().await.clone();
    let mut contracts: Vec<_> = registry
        .iter()
        .filter(|entry| {
            key.kind == SubjectKind::Token
                && entry.chain == key.chain
                && crate::domain::check::same_contract(&entry.contract, &key.address, key.chain)
        })
        .map(super::pages::contract_view)
        .collect();
    if key.kind == SubjectKind::Token && contracts.is_empty() {
        contracts.push(super::views::DirectoryContractView {
            issuer: "No publisher known".into(),
            ticker: String::new(),
            chain: key.chain.to_string(),
            chain_kind: key.chain,
            chain_icon: super::views::chain_icon(key.chain),
            contract: key.address.clone(),
            explorer_url: super::views::explorer_link(key.chain, &key.address),
            source_url: String::new(),
            powers: DirectoryPowersView::unavailable(),
        });
    }
    for contract in &mut contracts {
        let cached = crate::app::powers::cached_record(&state.app, key.chain, &key.address).await;
        let record = cached;
        contract.powers = match record {
            Some(record) => super::pages::display_powers(&state.app, record).await,
            None => DirectoryPowersView::unavailable(),
        };
        for value in [&mut contract.powers.observed_at, &mut contract.powers.last_confirmed_at] {
            if let Ok(time) = DateTime::parse_from_rfc3339(value) {
                *value = human_time(time.with_timezone(&Utc));
            }
        }
    }
    let board = state.leaderboard.read().await;
    let path = key.path();
    let ticker = contracts
        .first()
        .map(|entry| entry.ticker.clone())
        .filter(|ticker| !ticker.is_empty())
        .or_else(|| {
            board
                .entries
                .iter()
                .find(|entry| {
                    Chain::parse(&entry.chain) == Some(key.chain)
                        && crate::domain::check::same_contract(&entry.pool, &key.address, key.chain)
                })
                .and_then(|entry| entry.ticker.clone())
        })
        .unwrap_or_default();
    let title = if ticker.is_empty() {
        format!("{} {kind}", key.chain)
    } else {
        format!("{ticker} · {} {kind}", key.chain)
    };
    let name = registry
        .iter()
        .find(|entry| entry.ticker == ticker)
        .map(|entry| entry.name.clone())
        .unwrap_or_default();
    let issuers = contracts
        .iter()
        .map(|contract| contract.issuer.as_str())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(" · ");
    let mut history = prepare(&state.app, vec![key], path, &query).await;
    fill_pool_fallbacks(&state.app, &board.entries, &mut history);
    fill_facts(&state.app, &registry, &contracts, &board.entries, &mut history);
    render_page(
        TokenTemplate {
            asset_version: ASSET_VERSION,
            public_url: state.public_url.to_string(),
            title,
            name,
            issuers,
            contracts,
            history,
        },
        false,
    )
}
pub(crate) async fn export_json(
    State(state): State<AppState>,
    Path(key): Path<(String, String, String)>,
    Query(query): Query<HistoryQuery>,
) -> Result<Response, StatusCode> {
    export(state, key, query, "json").await
}
pub(crate) async fn export_csv(
    State(state): State<AppState>,
    Path(key): Path<(String, String, String)>,
    Query(query): Query<HistoryQuery>,
) -> Result<Response, StatusCode> {
    export(state, key, query, "csv").await
}
async fn export(
    state: AppState,
    (chain, kind, address): (String, String, String),
    query: HistoryQuery,
    format: &str,
) -> Result<Response, StatusCode> {
    let key = parse_key(&chain, &kind, &address)?;
    let days = query.days.filter(|days| [1, 7, 30, 90].contains(days)).unwrap_or(90);
    let limit = query.limit.unwrap_or(200).clamp(1, 200);
    let mut events = state
        .app
        .history
        .history(
            std::slice::from_ref(&key),
            state.app.clock.now() - Duration::days(i64::from(days)),
            query.before_time.zip(query.before_id),
            limit + 1,
        )
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let more = events.len() > limit;
    events.truncate(limit);
    let next = if more {
        events.last().map(|event| {
            format!(
                "{}/history.{format}?days={days}&limit={limit}&before_time={}&before_id={}",
                key.path(),
                event.observed_at.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true),
                event.event_id
            )
        })
    } else {
        None
    };
    let mut response = match format {
        "json" => axum::Json(serde_json::json!({"label":"Observation history — not a signed attestation", "durable":state.app.history.durable(), "recording_degraded_since":state.app.history.degraded_since(), "observations":events, "next":next})).into_response(),
        "csv" => {
            let mut csv = String::from("not_a_signed_attestation,event_id,chain,kind,address,facet,observed_at,outcome,miss_reason,attestation_id,block,slot\r\n");
            for event in events {
                let time = event.observed_at.to_rfc3339();
                let block = event.block.map(|v| v.to_string()).unwrap_or_default();
                let slot = event.slot.map(|v| v.to_string()).unwrap_or_default();
                let values = ["not a signed attestation",event.event_id.as_str(),super::super::discovery::chain_slug(event.asset.chain),if event.asset.kind == SubjectKind::Pool { "pool" } else { "token" },event.asset.address.as_str(),facet_label(event.facet),time.as_str(),event.outcome.label(),event.miss_reason.as_deref().unwrap_or_default(),event.attestation_id.as_deref().unwrap_or_default(),block.as_str(),slot.as_str()];
                for (index,value) in values.into_iter().enumerate() {
                    if index > 0 { csv.push(','); }
                    csv.push('"');
                    for character in value.chars() { if character == '"' { csv.push('"'); } csv.push(character); }
                    csv.push('"');
                }
                csv.push_str("\r\n");
            }
            ([(header::CONTENT_TYPE,"text/csv; charset=utf-8")],csv).into_response()
        },
        _ => return Err(StatusCode::NOT_FOUND),
    };
    if let Some(next) = next {
        response.headers_mut().insert(
            header::LINK,
            format!("<{next}>; rel=\"next\"")
                .parse()
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        );
    }
    Ok(response)
}

pub(crate) fn stats_summary(context: &Context) -> String {
    let states = context.history.states();
    let pools: Vec<_> =
        states.iter().filter(|(_, facet, _)| *facet == Facet::ContractMatch).collect();
    let verified = pools.iter().filter(|(_, _, state)| state.label() == "Verified").count();
    let retrying = pools
        .iter()
        .filter(|(_, _, state)| state.consecutive_misses > 0 && state.consecutive_misses < 3)
        .count();
    let unverified = pools.iter().filter(|(_, _, state)| state.consecutive_misses >= 3).count();
    format!(
        "<section class=\"history-asset\"><h2>Last-confirmed status · unsigned display summary</h2><p>{verified} Verified · {retrying} retrying · {unverified} Unverified. Signed statistics below remain latest-attempt counts, not this retained display projection.</p></section>"
    )
}

pub(crate) async fn observation(
    State(state): State<AppState>,
    Path((chain, kind, address, id)): Path<(String, String, String, String)>,
) -> Result<axum::Json<serde_json::Value>, StatusCode> {
    let asset = parse_key(&chain, &kind, &address)?;
    let event = state
        .app
        .history
        .observation(&asset, &id)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(axum::Json(
        serde_json::json!({"label":"Operational observation — not a signed attestation","observation":event}),
    ))
}

#[cfg(test)]
#[path = "asset_tests.rs"]
mod tests;
