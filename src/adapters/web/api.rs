use super::pages;
use crate::{
    adapters::{
        discovery::{self, FeaturedPool, LeaderboardEntry},
        state::{AppState, RegistryApiCache, StatsSnapshotCache},
    },
    app::{attestation as attest, check},
    domain::{
        attestation::Attestation,
        chain::Chain,
        check::{CheckResult, valid_public_input},
        registry::{self, Registry},
        statement::Statement,
    },
};
use axum::{
    Json,
    body::Body,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use bytes::{Bytes, BytesMut};
use chrono::{DateTime, Datelike, Utc};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use futures_util::StreamExt;
use serde::de::{IgnoredAny, MapAccess, Visitor};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::collections::HashSet;
use std::fmt;
use std::sync::{Arc, atomic::Ordering as AtomicOrdering};
pub(crate) async fn healthz() -> impl IntoResponse {
    Json(serde_json::json!({ "status": "ok" }))
}

pub(crate) async fn admin_stats(State(state): State<AppState>) -> Response {
    ([(header::CACHE_CONTROL, "no-store")], Json(state.usage_stats.snapshot())).into_response()
}
#[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeaderboardCounts {
    pub(crate) pools_checked: usize,
    pub(crate) issuer_matches: usize,
    pub(crate) mismatches: usize,
    pub(crate) unsupported_venue: usize,
    pub(crate) not_read_yet: ReadStatusCounts,
}

#[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReadStatusCounts {
    pub(crate) count: usize,
    pub(crate) rpc_limit: usize,
    pub(crate) transient: usize,
    pub(crate) unsupported: usize,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChainLeaderboardCounts {
    pub(crate) chain: String,
    pub(crate) chain_label: String,
    pub(crate) counts: LeaderboardCounts,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct IssuerRegistrySize {
    pub(crate) issuer: String,
    pub(crate) entries: usize,
    pub(crate) chains: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct RegistrySizes {
    pub(crate) active_entries: usize,
    pub(crate) by_issuer: Vec<IssuerRegistrySize>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct PublisherCatalogStats {
    pub(crate) first_flagged_this_week: usize,
    pub(crate) currently_flagged_last_7_days: usize,
    pub(crate) unsupported_chain_candidates_seen: usize,
    pub(crate) official_on_unsupported_chain: usize,
    pub(crate) evicted_entries: usize,
    pub(crate) evicted_unsupported_candidates: usize,
    pub(crate) rejected_oversize_entries: usize,
    pub(crate) rejected_oversize_unsupported_candidates: usize,
    pub(crate) last_scanned_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) source_unavailable_since: Option<String>,
    pub(crate) method: String,
    pub(crate) catalog_absent_tokens_truncated: bool,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeaderboardStats {
    pub(crate) generated_at: String,
    pub(crate) headline: String,
    pub(crate) listed_pools: usize,
    pub(crate) pools_checked: usize,
    pub(crate) issuer_matches: usize,
    pub(crate) mismatches: usize,
    pub(crate) unsupported_venue: usize,
    pub(crate) not_read_yet: ReadStatusCounts,
    pub(crate) by_chain: Vec<ChainLeaderboardCounts>,
    pub(crate) registry: RegistrySizes,
    pub(crate) publisher_catalog_watch: PublisherCatalogStats,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct StatsInputHashes {
    pub(crate) registry_source: String,
    pub(crate) leaderboard: String,
    pub(crate) impostor_watch: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct StatsReducerInputs {
    pub(crate) leaderboard: Vec<LeaderboardEntry>,
    pub(crate) impostor_watch: discovery::ImpostorSnapshot,
    pub(crate) active_registry: Registry,
    pub(crate) hashes: StatsInputHashes,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct StatsDocumentPayload<'a> {
    kind: &'a str,
    stats: &'a LeaderboardStats,
    reducer_inputs: &'a StatsReducerInputs,
    observed_at: &'a str,
    public_key: &'a str,
    dev: bool,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct StatsDocument {
    pub(crate) id: String,
    pub(crate) kind: String,
    pub(crate) stats: LeaderboardStats,
    pub(crate) reducer_inputs: StatsReducerInputs,
    pub(crate) observed_at: String,
    pub(crate) public_key: String,
    pub(crate) signature: String,
    pub(crate) dev: bool,
}

impl StatsDocument {
    fn payload(&self) -> StatsDocumentPayload<'_> {
        StatsDocumentPayload {
            kind: &self.kind,
            stats: &self.stats,
            reducer_inputs: &self.reducer_inputs,
            observed_at: &self.observed_at,
            public_key: &self.public_key,
            dev: self.dev,
        }
    }

    pub(crate) fn verify(&self) -> Result<(), String> {
        let payload = crate::domain::attestation::canonical_json(&self.payload())
            .map_err(|error| error.to_string())?;
        if payload.len() > MAX_SIGNED_STATS_DOCUMENT_BYTES {
            return Err("stats snapshot exceeds the maximum signed document size".to_owned());
        }
        let expected_id = crate::domain::attestation::hex_lower(&Sha256::digest(&payload));
        if self.kind != "stats"
            || self.id.len() != 64
            || !self.id.bytes().all(|byte| byte.is_ascii_hexdigit())
            || !self.id.eq_ignore_ascii_case(&expected_id)
        {
            return Err("stats snapshot id or kind is invalid".to_owned());
        }
        if self.public_key.len() > Chain::MAX_SOLANA_ADDRESS_CHARS {
            return Err("stats snapshot public key is too long".to_owned());
        }
        let mut public_key_bytes = [0; 32];
        let public_key_length = bs58::decode(&self.public_key)
            .onto(&mut public_key_bytes)
            .map_err(|error| error.to_string())?;
        if public_key_length != public_key_bytes.len() {
            return Err("stats snapshot public key is not 32 bytes".to_owned());
        }
        let verifying_key =
            VerifyingKey::from_bytes(&public_key_bytes).map_err(|error| error.to_string())?;
        if self.signature.len() != 88 {
            return Err("stats snapshot signature length is invalid".to_owned());
        }
        let mut signature_bytes = [0; 64];
        let signature_length = BASE64
            .decode_slice(&self.signature, &mut signature_bytes)
            .map_err(|error| error.to_string())?;
        if signature_length != signature_bytes.len() {
            return Err("stats snapshot signature length is invalid".to_owned());
        }
        let signature =
            Signature::from_slice(&signature_bytes).map_err(|error| error.to_string())?;
        verifying_key.verify(&payload, &signature).map_err(|error| error.to_string())
    }
}

#[derive(Serialize)]
struct StatsApiPage<'a> {
    stats: &'a LeaderboardStats,
    page: usize,
    page_size: usize,
    pages: usize,
    leaderboard_total: usize,
    impostor_candidates_total: usize,
    unsupported_candidates_total: usize,
    active_registry_total: usize,
    hashes: &'a StatsInputHashes,
    leaderboard: &'a [LeaderboardEntry],
    impostor_candidates: &'a [discovery::ImpostorEntry],
    unsupported_candidates: &'a [discovery::UnsupportedImpostorCandidate],
    active_registry: &'a [registry::Entry],
}

const STATS_API_PAGE_SIZE: usize = 50;
pub(crate) const MAX_SIGNED_STATS_DOCUMENT_BYTES: usize = 16 * 1024 * 1024;

const IMPOSTOR_STATS_TABLE_LIMIT: usize = 20;
const IMPOSTOR_STATS_METHOD: &str = concat!(
    "DexScreener is queried first; a cached USDC search returning no pairs marks it unavailable for five minutes. ",
    "On DexScreener errors or canary unavailability, GeckoTerminal searches registry product names across Robinhood, Solana, Ethereum, Base, BNB Chain, Arc, and TON for the top 10 active tickers, with a 10-call-per-minute budget. ",
    "The watch keeps exact ticker or ticker+x symbol filtering, counts unsupported-chain candidates without judging them, and records the source for each observation. ",
    "At most 70 GeckoTerminal network searches and 50 sequential QED Guard evaluations run per refresh, including rechecks. ",
    "Exact on-chain symbol-and-name matches absent from the publisher catalog are observations about catalog coverage, not conclusions about intent. ",
    "Reported volume is not independently verified; first_seen and last_seen are scanner observation times."
);
fn unix_timestamp(timestamp: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(timestamp).ok().map(|value| value.timestamp())
}

fn is_in_current_utc_week(timestamp: &str, current_week: chrono::naive::IsoWeek) -> bool {
    DateTime::parse_from_rfc3339(timestamp)
        .ok()
        .is_some_and(|value| value.with_timezone(&Utc).iso_week() == current_week)
}

fn is_within_last_7_days(timestamp: &str, cutoff: i64, now: i64) -> bool {
    unix_timestamp(timestamp).is_some_and(|seen_at| seen_at >= cutoff && seen_at <= now)
}
pub(crate) fn recent_catalog_entries(
    entries: &[discovery::ImpostorEntry],
    now: i64,
) -> Vec<&discovery::ImpostorEntry> {
    let cutoff = now.saturating_sub(chrono::Duration::days(7).num_seconds());
    let mut recent = entries
        .iter()
        .filter(|entry| is_within_last_7_days(&entry.last_seen_at, cutoff, now))
        .collect::<Vec<_>>();
    recent.sort_by(|left, right| {
        unix_timestamp(&right.last_seen_at).cmp(&unix_timestamp(&left.last_seen_at))
    });
    recent
}

fn leaderboard_counts<'a>(
    entries: impl Iterator<Item = &'a LeaderboardEntry>,
) -> LeaderboardCounts {
    let mut counts = LeaderboardCounts::default();
    for entry in entries {
        if entry.read_status == "unsupported_venue" {
            counts.unsupported_venue += 1;
            continue;
        }
        if entry.read_status == "not_read_yet" {
            counts.not_read_yet.count += 1;
            match entry.read_reason.as_deref().unwrap_or("unsupported") {
                "rpc_limit" => counts.not_read_yet.rpc_limit += 1,
                "transient" => counts.not_read_yet.transient += 1,
                _ => counts.not_read_yet.unsupported += 1,
            }
            continue;
        }
        counts.pools_checked += 1;
        match entry.verdict.as_str() {
            "verified" => counts.issuer_matches += 1,
            "mismatch" => counts.mismatches += 1,
            _ => {}
        }
    }
    counts
}

pub(crate) fn leaderboard_stats(
    board: &discovery::Leaderboard,
    registry_entries: &Registry,
) -> LeaderboardStats {
    let counts = leaderboard_counts(board.entries.iter());
    let by_chain = [Chain::Solana, Chain::RobinhoodChain, Chain::Base, Chain::Ethereum, Chain::Bnb]
        .into_iter()
        .map(|chain| ChainLeaderboardCounts {
            chain: discovery::chain_slug(chain).to_owned(),
            chain_label: discovery::chain_label(chain).to_owned(),
            counts: leaderboard_counts(
                board
                    .entries
                    .iter()
                    .filter(|entry| discovery::chain_from_dex_id(&entry.chain) == Some(chain)),
            ),
        })
        .collect::<Vec<_>>();
    let issuer_names = ["Backed xStocks", "Robinhood", "Ondo"];
    let by_issuer = issuer_names
        .into_iter()
        .map(|issuer| {
            let entries = registry_entries
                .iter()
                .filter(|entry| entry.issuer == issuer && registry::matchable(entry))
                .collect::<Vec<_>>();
            let mut chains = entries
                .iter()
                .map(|entry| discovery::chain_label(entry.chain).to_owned())
                .collect::<Vec<_>>();
            chains.sort();
            chains.dedup();
            IssuerRegistrySize { issuer: issuer.to_owned(), entries: entries.len(), chains }
        })
        .collect::<Vec<_>>();
    let now = Utc::now();
    let current_week = now.iso_week();
    let now_timestamp = now.timestamp();
    let last_7_days_cutoff = (now - chrono::Duration::days(7)).timestamp();
    let first_flagged_this_week = board
        .impostors
        .entries
        .iter()
        .filter(|entry| is_in_current_utc_week(&entry.first_seen_at, current_week))
        .count();
    let currently_flagged_last_7_days = board
        .impostors
        .entries
        .iter()
        .filter(|entry| {
            is_within_last_7_days(&entry.last_seen_at, last_7_days_cutoff, now_timestamp)
        })
        .count();
    let catalog_absent_tokens_truncated =
        currently_flagged_last_7_days > IMPOSTOR_STATS_TABLE_LIMIT;
    let listed_pools = board.entries.len();
    let catalog_summary = match board.impostors.source_unavailable_since.as_deref() {
        Some(since) if board.impostors.scanned_at.is_empty() => {
            format!("Impostor search source unavailable since {since}; no successful search yet.")
        }
        Some(since) => format!(
            "Impostor search source unavailable since {since}; from the last successful search at {}, {currently_flagged_last_7_days} catalog observations are currently flagged (seen in the last 7 days); {first_flagged_this_week} were first seen this UTC week.",
            board.impostors.scanned_at
        ),
        None => format!(
            "{currently_flagged_last_7_days} catalog observations are currently flagged (seen in the last 7 days); {first_flagged_this_week} were first seen this UTC week."
        ),
    };
    let headline = format!(
        "QED checked {} of {listed_pools} listed pools: {} issuer matches, {} mismatches, {} unsupported venues, and {} not read yet. {catalog_summary} {} tokens on unsupported chains were seen but not judged. {} supported observations and {} unsupported candidates were evicted; {} invalid supported and {} invalid unsupported candidates were rejected.",
        counts.pools_checked,
        counts.issuer_matches,
        counts.mismatches,
        counts.unsupported_venue,
        counts.not_read_yet.count,
        board.impostors.unsupported_seen,
        board.impostors.evicted_entries,
        board.impostors.evicted_unsupported_candidates,
        board.impostors.rejected_oversize_entries,
        board.impostors.rejected_oversize_unsupported_candidates
    );
    LeaderboardStats {
        generated_at: board.updated_at.clone(),
        headline,
        listed_pools,
        pools_checked: counts.pools_checked,
        issuer_matches: counts.issuer_matches,
        mismatches: counts.mismatches,
        unsupported_venue: counts.unsupported_venue,
        not_read_yet: counts.not_read_yet,
        by_chain,
        registry: RegistrySizes {
            active_entries: registry_entries
                .iter()
                .filter(|entry| registry::matchable(entry))
                .count(),
            by_issuer,
        },
        publisher_catalog_watch: PublisherCatalogStats {
            currently_flagged_last_7_days,
            first_flagged_this_week,
            unsupported_chain_candidates_seen: board.impostors.unsupported_seen,
            official_on_unsupported_chain: board.impostors.official_on_unsupported_chain,
            evicted_entries: board.impostors.evicted_entries,
            evicted_unsupported_candidates: board.impostors.evicted_unsupported_candidates,
            rejected_oversize_entries: board.impostors.rejected_oversize_entries,
            rejected_oversize_unsupported_candidates: board
                .impostors
                .rejected_oversize_unsupported_candidates,
            last_scanned_at: board.impostors.scanned_at.clone(),
            source_unavailable_since: board.impostors.source_unavailable_since.clone(),
            method: IMPOSTOR_STATS_METHOD.to_owned(),
            catalog_absent_tokens_truncated,
        },
    }
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct StatsPageQuery {
    pub(crate) page: Option<usize>,
}

pub(crate) async fn api_stats(
    State(state): State<AppState>,
    Query(query): Query<StatsPageQuery>,
) -> Response {
    let page = query.page.unwrap_or(1);
    if page == 0 {
        return StatusCode::BAD_REQUEST.into_response();
    }
    match get_stats_snapshot(&state).await {
        Ok(snapshot) => snapshot
            .api_pages
            .get(page - 1)
            .cloned()
            .map(|bytes| stats_bytes_response(bytes, "application/json; charset=utf-8", None))
            .unwrap_or_else(|| StatusCode::NOT_FOUND.into_response()),
        Err(error) => {
            tracing::error!(%error, "could not prepare stats snapshot");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

async fn build_stats_cache(state: &AppState) -> Result<StatsSnapshotCache, String> {
    let board = discovery::normalize_leaderboard(state.leaderboard.read().await.clone());
    let registry = state.registry.read().await.clone();
    let stats = leaderboard_stats(&board, &registry);
    let recent_catalog_entries =
        recent_catalog_entries(&board.impostors.entries, Utc::now().timestamp());
    let visible_catalog_entries =
        &recent_catalog_entries[..recent_catalog_entries.len().min(IMPOSTOR_STATS_TABLE_LIMIT)];
    let html = super::pages::render_stats_page(state, &stats, visible_catalog_entries)
        .map_err(|status| format!("rendering stats page failed with {status}"))?;
    let document = build_stats_document(state, stats, &board, &registry)?;
    let json = serde_json::to_vec(&document).map_err(|error| error.to_string())?;
    let csv = stats_csv(&document).map_err(|error| error.to_string())?;
    let inputs = &document.reducer_inputs;
    let pages = inputs
        .leaderboard
        .len()
        .max(inputs.impostor_watch.entries.len())
        .max(inputs.impostor_watch.unsupported_candidates.len())
        .max(inputs.active_registry.len())
        .div_ceil(STATS_API_PAGE_SIZE)
        .max(1);
    let mut api_pages = Vec::with_capacity(pages);
    for index in 0..pages {
        let start = index * STATS_API_PAGE_SIZE;
        let end = start.saturating_add(STATS_API_PAGE_SIZE);
        let page = StatsApiPage {
            stats: &document.stats,
            page: index + 1,
            page_size: STATS_API_PAGE_SIZE,
            pages,
            leaderboard_total: inputs.leaderboard.len(),
            impostor_candidates_total: inputs.impostor_watch.entries.len(),
            unsupported_candidates_total: inputs.impostor_watch.unsupported_candidates.len(),
            active_registry_total: inputs.active_registry.len(),
            hashes: &inputs.hashes,
            leaderboard: inputs
                .leaderboard
                .get(start..end.min(inputs.leaderboard.len()))
                .unwrap_or(&[]),
            impostor_candidates: inputs
                .impostor_watch
                .entries
                .get(start..end.min(inputs.impostor_watch.entries.len()))
                .unwrap_or(&[]),
            unsupported_candidates: inputs
                .impostor_watch
                .unsupported_candidates
                .get(start..end.min(inputs.impostor_watch.unsupported_candidates.len()))
                .unwrap_or(&[]),
            active_registry: inputs
                .active_registry
                .get(start..end.min(inputs.active_registry.len()))
                .unwrap_or(&[]),
        };
        api_pages.push(Bytes::from(serde_json::to_vec(&page).map_err(|error| error.to_string())?));
    }
    Ok(StatsSnapshotCache { html, json: Bytes::from(json), csv: Bytes::from(csv), api_pages })
}

pub(crate) async fn refresh_stats_snapshot(state: &AppState) -> Result<(), String> {
    let _refresh = state.stats_snapshot_refresh.lock().await;
    let snapshot = Arc::new(build_stats_cache(state).await?);
    *state.stats_snapshot.write().await = Some(snapshot);
    Ok(())
}

async fn get_stats_snapshot(state: &AppState) -> Result<Arc<StatsSnapshotCache>, String> {
    if let Some(snapshot) = state.stats_snapshot.read().await.clone() {
        return Ok(snapshot);
    }
    let _refresh = state.stats_snapshot_refresh.lock().await;
    if let Some(snapshot) = state.stats_snapshot.read().await.clone() {
        return Ok(snapshot);
    }
    let snapshot = Arc::new(build_stats_cache(state).await?);
    *state.stats_snapshot.write().await = Some(Arc::clone(&snapshot));
    Ok(snapshot)
}

fn stats_bytes_response(
    bytes: Bytes,
    content_type: &'static str,
    filename: Option<&'static str>,
) -> Response {
    let mut response = Response::new(Body::from(bytes));
    response.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if let Some(filename) = filename {
        let value = if filename == "qed-stats.json" {
            "attachment; filename=\"qed-stats.json\""
        } else {
            "attachment; filename=\"qed-stats.csv\""
        };
        response.headers_mut().insert(header::CONTENT_DISPOSITION, HeaderValue::from_static(value));
    }
    response
}

fn stats_input_hash<T: Serialize>(value: &T) -> Result<String, String> {
    let bytes =
        crate::domain::attestation::canonical_json(value).map_err(|error| error.to_string())?;
    Ok(crate::domain::attestation::hex_lower(&Sha256::digest(bytes)))
}

fn build_stats_document(
    state: &AppState,
    stats: LeaderboardStats,
    board: &discovery::Leaderboard,
    registry: &Registry,
) -> Result<StatsDocument, String> {
    let active_registry =
        registry.iter().filter(|entry| registry::matchable(entry)).cloned().collect::<Vec<_>>();
    let reducer_inputs = StatsReducerInputs {
        leaderboard: board.entries.clone(),
        impostor_watch: board.impostors.clone(),
        hashes: StatsInputHashes {
            registry_source: state
                .app
                .registry_hash
                .read()
                .map_err(|error| error.to_string())?
                .clone(),
            leaderboard: stats_input_hash(&board.entries)?,
            impostor_watch: stats_input_hash(&board.impostors)?,
        },
        active_registry,
    };
    let kind = "stats";
    let observed_at = Utc::now().to_rfc3339();
    let public_key = attest::public_key_b58(&state.app);
    let dev = state.app.dev_signer;
    let payload = StatsDocumentPayload {
        kind,
        stats: &stats,
        reducer_inputs: &reducer_inputs,
        observed_at: &observed_at,
        public_key: &public_key,
        dev,
    };
    let payload_bytes =
        crate::domain::attestation::canonical_json(&payload).map_err(|error| error.to_string())?;
    if payload_bytes.len() > MAX_SIGNED_STATS_DOCUMENT_BYTES {
        return Err("stats snapshot exceeds the maximum signed document size".to_owned());
    }
    let (id, signature) = attest::sign_document_payload(&state.app, &payload_bytes)?;
    drop(payload);
    Ok(StatsDocument {
        id,
        kind: kind.to_owned(),
        stats,
        reducer_inputs,
        observed_at,
        public_key,
        signature,
        dev,
    })
}

pub(crate) async fn stats_snapshot_json(State(state): State<AppState>) -> Response {
    match get_stats_snapshot(&state).await {
        Ok(snapshot) => stats_bytes_response(
            snapshot.json.clone(),
            "application/json; charset=utf-8",
            Some("qed-stats.json"),
        ),
        Err(error) => {
            tracing::error!(%error, "could not prepare signed stats snapshot");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

const STATS_CSV_COLUMNS: usize = 14;

fn push_csv_record(output: &mut String, fields: &[&str]) {
    debug_assert!(fields.len() <= STATS_CSV_COLUMNS);
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
    for _ in fields.len()..STATS_CSV_COLUMNS {
        output.push(',');
        output.push_str("\"\"");
    }
    output.push('\n');
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

fn stats_csv(document: &StatsDocument) -> Result<String, serde_json::Error> {
    let document_json = serde_json::to_string(document)?;
    let stats = &document.stats;
    let mut csv = String::new();
    push_csv_record(&mut csv, &["snapshot", "signed_stats_document_json", &document_json]);
    push_csv_record(
        &mut csv,
        &[
            "section",
            "metric",
            "value",
            "chain",
            "ticker",
            "publisher",
            "symbol",
            "name",
            "address",
            "reported_24h_volume_usd",
            "first_seen_at",
            "last_seen_at",
            "guard_url",
            "reason",
        ],
    );
    for (metric, value) in [
        ("generated_at", stats.generated_at.as_str()),
        ("headline", stats.headline.as_str()),
        (
            "publisher_catalog_last_scanned_at",
            stats.publisher_catalog_watch.last_scanned_at.as_str(),
        ),
        ("publisher_catalog_method", stats.publisher_catalog_watch.method.as_str()),
    ] {
        push_csv_record(&mut csv, &["summary", metric, value]);
    }
    for (metric, value) in [
        ("listed_pools", stats.listed_pools),
        ("pools_checked", stats.pools_checked),
        ("issuer_matches", stats.issuer_matches),
        ("mismatches", stats.mismatches),
        ("unsupported_venue", stats.unsupported_venue),
        ("not_read_yet", stats.not_read_yet.count),
        ("registry_active_entries", stats.registry.active_entries),
        (
            "publisher_catalog_first_flagged_this_week",
            stats.publisher_catalog_watch.first_flagged_this_week,
        ),
        (
            "publisher_catalog_currently_flagged_last_7_days",
            stats.publisher_catalog_watch.currently_flagged_last_7_days,
        ),
        (
            "unsupported_chain_candidates_seen",
            stats.publisher_catalog_watch.unsupported_chain_candidates_seen,
        ),
        (
            "official_on_unsupported_chain",
            stats.publisher_catalog_watch.official_on_unsupported_chain,
        ),
        ("evicted_entries", stats.publisher_catalog_watch.evicted_entries),
        (
            "evicted_unsupported_candidates",
            stats.publisher_catalog_watch.evicted_unsupported_candidates,
        ),
        ("rejected_oversize_entries", stats.publisher_catalog_watch.rejected_oversize_entries),
        (
            "rejected_oversize_unsupported_candidates",
            stats.publisher_catalog_watch.rejected_oversize_unsupported_candidates,
        ),
    ] {
        push_csv_record(&mut csv, &["summary", metric, &value.to_string()]);
    }
    for chain in &stats.by_chain {
        push_csv_record(
            &mut csv,
            &[
                "chain",
                &chain.chain_label,
                &format!(
                    "checked={},matches={},mismatches={},unsupported_venue={},not_read_yet={}",
                    chain.counts.pools_checked,
                    chain.counts.issuer_matches,
                    chain.counts.mismatches,
                    chain.counts.unsupported_venue,
                    chain.counts.not_read_yet.count
                ),
            ],
        );
    }
    for issuer in &stats.registry.by_issuer {
        push_csv_record(
            &mut csv,
            &[
                "issuer",
                &issuer.issuer,
                &format!("active_entries={},chains={}", issuer.entries, issuer.chains.join("|")),
            ],
        );
    }
    let recent_catalog_entries = recent_catalog_entries(
        &document.reducer_inputs.impostor_watch.entries,
        unix_timestamp(&document.observed_at).unwrap_or(i64::MIN),
    );
    for entry in recent_catalog_entries.into_iter().take(IMPOSTOR_STATS_TABLE_LIMIT) {
        let volume = entry.volume_24h_usd.map(|value| value.to_string()).unwrap_or_default();
        push_csv_record(
            &mut csv,
            &[
                "publisher_catalog_absent_token",
                "",
                "",
                &entry.chain_label,
                &entry.ticker,
                &entry.publisher,
                &entry.symbol,
                &entry.name,
                &entry.address,
                &volume,
                &entry.first_seen_at,
                &entry.last_seen_at,
                &entry.guard_url,
                &entry.reason,
            ],
        );
    }
    Ok(csv)
}

pub(crate) async fn stats_snapshot_csv(State(state): State<AppState>) -> Response {
    match get_stats_snapshot(&state).await {
        Ok(snapshot) => stats_bytes_response(
            snapshot.csv.clone(),
            "text/csv; charset=utf-8",
            Some("qed-stats.csv"),
        ),
        Err(error) => {
            tracing::error!(%error, "could not prepare signed stats CSV");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}
pub(crate) async fn api_registry(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let registry_hash = state.app.registry_hash.read().map(|hash| hash.clone()).unwrap_or_default();
    let registry_hash =
        format!("{registry_hash}:{}", state.app.registry_version.load(AtomicOrdering::Acquire));
    let body = {
        let mut cache = state.registry_api_cache.write().await;
        if let Some(cached) = cache.as_ref().filter(|cached| cached.registry_hash == registry_hash)
        {
            Arc::clone(&cached.body)
        } else {
            let body = Arc::new(Bytes::from(
                serde_json::to_vec(&*state.registry.read().await)
                    .expect("registry serialization must remain valid"),
            ));
            *cache = Some(RegistryApiCache {
                registry_hash: registry_hash.clone(),
                body: Arc::clone(&body),
            });
            body
        }
    };
    let etag = format!("\"qed-registry-{registry_hash}\"");
    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == etag)
    {
        return StatusCode::NOT_MODIFIED.into_response();
    }
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CACHE_CONTROL, "public, max-age=60")
        .header(header::ETAG, etag)
        .body(Body::from(body.as_ref().clone()))
        .expect("registry response headers are valid")
}
pub(crate) async fn api_featured(State(state): State<AppState>) -> Json<Vec<FeaturedPool>> {
    Json(state.featured.read().await.clone())
}
#[derive(Debug, Deserialize, Default)]
pub(crate) struct LeaderboardQuery {
    page: Option<usize>,
    per: Option<usize>,
    sort: Option<String>,
    dir: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct PowersQuery {
    chain: Option<String>,
}

fn metric(entry: &LeaderboardEntry, sort: &str) -> Option<f64> {
    match sort {
        "change" => entry.change_24h_pct,
        "liquidity" => entry.liquidity_usd,
        "price" => entry.price_usd,
        _ => entry.volume_24h_usd,
    }
}

fn sort_entries(entries: &mut [LeaderboardEntry], sort: &str, descending: bool) {
    entries.sort_by(|left, right| {
        let ordering = match (metric(left, sort), metric(right, sort)) {
            (Some(left), Some(right)) => {
                let ordering = left.total_cmp(&right);
                if descending { ordering.reverse() } else { ordering }
            }
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        };
        ordering
            .then_with(|| left.pool.to_ascii_lowercase().cmp(&right.pool.to_ascii_lowercase()))
            .then_with(|| left.rank.cmp(&right.rank))
    });
}

fn page_entries(entries: Vec<LeaderboardEntry>, page: usize, per: usize) -> Vec<LeaderboardEntry> {
    let start = page.saturating_sub(1).saturating_mul(per);
    entries
        .into_iter()
        .skip(start)
        .take(per)
        .enumerate()
        .map(|(offset, mut entry)| {
            entry.rank = start + offset + 1;
            entry
        })
        .collect()
}

pub(crate) async fn api_leaderboard(
    State(state): State<AppState>,
    Query(query): Query<LeaderboardQuery>,
) -> Json<serde_json::Value> {
    let page = query.page.unwrap_or(1).max(1);
    let per = query.per.unwrap_or(discovery::LEADERBOARD_PAGE_SIZE).clamp(1, 50);
    let sort = query.sort.as_deref().unwrap_or("volume");
    let descending = !query.dir.as_deref().is_some_and(|dir| dir.eq_ignore_ascii_case("asc"));
    let board = discovery::normalize_leaderboard(state.leaderboard.read().await.clone());
    let prices_updated_at = state.prices.read().await.updated_at.clone();
    let total = board.total.max(board.entries.len());
    let mut entries = board.entries;
    sort_entries(&mut entries, sort, descending);
    let entries = page_entries(entries, page, per);
    Json(serde_json::json!({
        "page": page,
        "per": per,
        "total": total,
        "updated_at": board.updated_at,
        "next_refresh_at": board.next_refresh_at,
        "restored": board.restored,
        "refreshing": board.refreshing,
        "empty_successful": board.empty_successful,
        "prices_updated_at": prices_updated_at,
        "source": board.source,
        "registry": board.registry,
        "entries": entries,
    }))
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct PricesQuery {
    ids: Option<String>,
}
pub(crate) async fn api_prices(
    State(state): State<AppState>,
    Query(query): Query<PricesQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let ids = query.ids.as_deref().unwrap_or("");
    if ids.len() > 2048 {
        return Err(StatusCode::BAD_REQUEST);
    }
    let snapshot = state.prices.read().await.clone();
    let mut prices = Vec::new();
    let mut seen = HashSet::new();
    for id in ids.split(',') {
        if id.len() > 128 || !seen.insert(id) {
            return Err(StatusCode::BAD_REQUEST);
        }
        let Some((chain_id, pool)) = id.split_once(':') else { continue };
        let Some(chain) = discovery::chain_from_dex_id(chain_id) else { continue };
        if pool.is_empty() {
            continue;
        }
        let chain = discovery::chain_slug(chain).to_owned();
        if let Some(point) = snapshot
            .prices
            .iter()
            .find(|point| {
                point.chain.eq_ignore_ascii_case(&chain) && point.pool.eq_ignore_ascii_case(pool)
            })
            .cloned()
        {
            prices.push(point);
        }
        if prices.len() > 100 {
            return Err(StatusCode::BAD_REQUEST);
        }
    }
    Ok(Json(serde_json::json!({ "updated_at": snapshot.updated_at, "prices": prices })))
}

pub(crate) async fn api_status(State(state): State<AppState>) -> Json<serde_json::Value> {
    let registry = state.registry_status.read().await.clone();
    let leaderboard = state.leaderboard.read().await;
    let featured = state.featured_status.read().await.clone();
    let prices = state.prices.read().await.clone();
    Json(serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "registry": registry,
        "leaderboard": {
            "updated_at": leaderboard.updated_at,
            "next_refresh_at": leaderboard.next_refresh_at,
            "restored": leaderboard.restored,
            "refreshing": leaderboard.refreshing,
            "empty_successful": leaderboard.empty_successful,
        },
        "featured": featured,
        "prices": {
            "updated_at": prices.updated_at,
        },
    }))
}

pub(crate) async fn api_check(
    State(state): State<AppState>,
    Path(address): Path<String>,
) -> Result<Json<CheckResult>, StatusCode> {
    let address = address.trim();
    if !valid_public_input(address) {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(Json(check::check(&state.app, address).await))
}
pub(crate) async fn api_statement(
    State(state): State<AppState>,
    Json(request): Json<crate::app::statement::StatementRequest>,
) -> Result<Json<crate::domain::statement::Statement>, (StatusCode, Json<serde_json::Value>)> {
    crate::app::statement::create(&state.app, request)
        .await
        .map(Json)
        .map_err(statement_error_response)
}

fn statement_error_response(
    error: crate::app::statement::StatementError,
) -> (StatusCode, Json<serde_json::Value>) {
    use crate::app::statement::StatementError;

    let (status, message) = match error {
        StatementError::InvalidRequest => (
            StatusCode::BAD_REQUEST,
            "Provide 1–32 valid wallet addresses and select at least one supported chain."
                .to_owned(),
        ),
        StatementError::UnknownWallet => (
            StatusCode::BAD_REQUEST,
            "A wallet address does not match the selected chain. Check the address and chain selection."
                .to_owned(),
        ),
        StatementError::ReaderUnavailable(chain) => (
            StatusCode::BAD_GATEWAY,
            format!("QED has no balance reader configured for {chain}."),
        ),
        StatementError::ReadFailed(chain) => (
            StatusCode::BAD_GATEWAY,
            format!("The {chain} balance read failed. No statement was created."),
        ),
        StatementError::DeadlineExceeded => (
            StatusCode::GATEWAY_TIMEOUT,
            "QED's 15-second balance-read deadline elapsed. No statement was created; retry the request."
                .to_owned(),
        ),
        StatementError::Signing => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "QED could not sign the statement. No statement was saved.".to_owned(),
        ),
    };
    (status, Json(serde_json::json!({ "error": message })))
}

pub(crate) async fn api_statement_get(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Statement>, StatusCode> {
    crate::app::statement::get(&state.app, &id).await.map(Json).ok_or(StatusCode::NOT_FOUND)
}

pub(crate) async fn api_powers(
    State(state): State<AppState>,
    Path(address): Path<String>,
    Query(query): Query<PowersQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let chain = match query.chain.as_deref() {
        Some(chain) => {
            Some(crate::domain::chain::Chain::parse(chain).ok_or(StatusCode::BAD_REQUEST)?)
        }
        None => None,
    };
    let records = crate::app::powers::for_any(&state.app, &address, chain).await;
    match records {
        Ok(mut records) => {
            let value = if records.len() == 1 {
                serde_json::to_value(records.pop().expect("one powers record"))
            } else {
                serde_json::to_value(records)
            }
            .expect("powers records serialize");
            Ok(Json(value))
        }
        Err(crate::app::powers::LookupError::InvalidAddress) => Err(StatusCode::BAD_REQUEST),
        Err(crate::app::powers::LookupError::NotFound) => Err(StatusCode::NOT_FOUND),
        Err(crate::app::powers::LookupError::ReadFailed) => Err(StatusCode::BAD_GATEWAY),
        Err(crate::app::powers::LookupError::DeadlineExceeded) => Err(StatusCode::GATEWAY_TIMEOUT),
    }
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct GuardQuery {
    chain: Option<String>,
    wallet: Option<String>,
}

pub(crate) async fn api_guard(
    State(state): State<AppState>,
    Path(address): Path<String>,
    Query(query): Query<GuardQuery>,
) -> Result<Json<crate::domain::guard::GuardDocument>, StatusCode> {
    let chain = query
        .chain
        .as_deref()
        .and_then(crate::domain::chain::Chain::parse)
        .ok_or(StatusCode::BAD_REQUEST)?;
    crate::app::guard::create(&state.app, &address, chain, query.wallet.as_deref())
        .await
        .map(Json)
        .map_err(|error| super::guard_error_status(&error))
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GuardPostRequest {
    address: String,
    chain: String,
    wallet: Option<String>,
}

pub(crate) async fn api_guard_post(
    State(state): State<AppState>,
    Json(request): Json<GuardPostRequest>,
) -> Result<Json<crate::domain::guard::GuardDocument>, StatusCode> {
    let chain =
        crate::domain::chain::Chain::parse(&request.chain).ok_or(StatusCode::BAD_REQUEST)?;
    crate::app::guard::create(&state.app, &request.address, chain, request.wallet.as_deref())
        .await
        .map(Json)
        .map_err(|error| super::guard_error_status(&error))
}

pub(crate) async fn api_wallet(
    State(state): State<AppState>,
    Json(request): Json<pages::WalletRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let holdings = crate::app::wallet::wallet_holdings(&state.app, &request.address)
        .await
        .map_err(super::wallet_error_status)?;
    let holdings = super::views::wallet_holding_views(holdings);
    Ok(Json(serde_json::json!({
        "address": request.address,
        "holdings": holdings,
    })))
}
pub(crate) async fn api_attestation(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, StatusCode> {
    let attestation =
        attest::get_certificate_async(&state.app, &id).await.ok_or(StatusCode::NOT_FOUND)?;
    let current_payload = crate::domain::attestation::canonical_payload_json(&attestation)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let current_id =
        crate::domain::attestation::hex_lower(&Sha256::digest(current_payload.as_bytes()));
    let mut document =
        serde_json::to_value(&attestation).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if !attestation.id.eq_ignore_ascii_case(&current_id) {
        crate::domain::attestation::restore_legacy_chain_names(&mut document);
    }
    Ok(([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(document)).into_response())
}
pub(crate) async fn well_known(State(state): State<AppState>) -> Response {
    (
        [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")],
        Json(serde_json::json!({
            "name": "QED",
            "version": 1,
            "algorithm": "Ed25519",
            "public_key": attest::public_key_b58(&state.app),
            "key": attest::public_key_b58(&state.app),
            "dev": state.app.dev_signer,
        })),
    )
        .into_response()
}
const MAX_NON_STATS_VERIFY_BODY_BYTES: usize = 2 * 1024 * 1024 - 1;
pub(crate) const MAX_STATS_VERIFY_BODY_BYTES: usize = MAX_SIGNED_STATS_DOCUMENT_BYTES + 1024;
const VERIFY_KIND_PREFIX_BYTES: usize = 256 * 1024;

fn capture_document_kind<'de, D>(deserializer: D, kind: &mut Option<String>) -> Result<(), D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct KindVisitor<'a>(&'a mut Option<String>);

    impl<'de, 'a> Visitor<'de> for KindVisitor<'a> {
        type Value = ();

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a JSON object with an optional top-level kind")
        }

        fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
        where
            A: MapAccess<'de>,
        {
            while let Some(key) = map.next_key::<String>()? {
                if key == "kind" {
                    *self.0 = map.next_value::<Option<String>>()?;
                } else {
                    map.next_value::<IgnoredAny>()?;
                }
            }
            Ok(())
        }
    }

    deserializer.deserialize_map(KindVisitor(kind))
}

fn peek_document_kind(prefix: &[u8]) -> Option<String> {
    let mut kind = None;
    let mut deserializer = serde_json::Deserializer::from_slice(prefix);
    let _ = capture_document_kind(&mut deserializer, &mut kind);
    kind
}

async fn read_verify_document_body(body: Body) -> Result<Bytes, StatusCode> {
    let mut stream = body.into_data_stream();
    let mut bytes = BytesMut::new();
    let mut limit = MAX_NON_STATS_VERIFY_BODY_BYTES;
    let mut kind_checked = false;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| StatusCode::BAD_REQUEST)?;
        let mut chunk_offset = 0;
        if !kind_checked {
            let prefix_len = chunk.len().min(VERIFY_KIND_PREFIX_BYTES.saturating_sub(bytes.len()));
            bytes.extend_from_slice(&chunk[..prefix_len]);
            chunk_offset = prefix_len;
            if let Some(kind) = peek_document_kind(&bytes) {
                limit = if kind == "stats" {
                    MAX_STATS_VERIFY_BODY_BYTES
                } else {
                    MAX_NON_STATS_VERIFY_BODY_BYTES
                };
                kind_checked = true;
            } else if bytes.len() >= VERIFY_KIND_PREFIX_BYTES {
                kind_checked = true;
            }
        }
        if kind_checked {
            if bytes.len().saturating_add(chunk.len() - chunk_offset) > limit {
                return Err(StatusCode::PAYLOAD_TOO_LARGE);
            }
            bytes.extend_from_slice(&chunk[chunk_offset..]);
        } else if bytes.len() > limit {
            return Err(StatusCode::PAYLOAD_TOO_LARGE);
        }
    }
    if bytes.len() > limit {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }
    Ok(bytes.freeze())
}

pub(crate) async fn verify_body_size_limit(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let (parts, body) = request.into_parts();
    let body = match read_verify_document_body(body).await {
        Ok(body) => Body::from(body),
        Err(status) => {
            let message = if status == StatusCode::PAYLOAD_TOO_LARGE {
                "Request body is too large."
            } else {
                "Could not read request body."
            };
            return (status, Json(serde_json::json!({ "error": message }))).into_response();
        }
    };
    next.run(axum::extract::Request::from_parts(parts, body)).await
}

pub(crate) async fn verify_attestation(
    State(state): State<AppState>,
    Json(document): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let kind = document.get("kind").and_then(serde_json::Value::as_str);
    if document.get("kind").is_some() && kind.is_none() {
        return Err(verify_bad_request("QED document kind must be a string."));
    }
    let has_statement_fields =
        ["wallets", "assets", "positions"].iter().any(|field| document.get(*field).is_some());
    let has_attestation_fields = ["pool", "quote_share_of_supply", "registry_hash"]
        .iter()
        .any(|field| document.get(*field).is_some());
    let has_guard_fields = ["identity", "powers", "source", "pools", "reasons", "wallet"]
        .iter()
        .any(|field| document.get(*field).is_some());
    let has_stats_fields = document.get("stats").is_some();
    if (has_statement_fields && (has_attestation_fields || has_guard_fields || has_stats_fields))
        || (has_attestation_fields && (has_guard_fields || has_stats_fields))
        || (has_guard_fields && has_stats_fields)
    {
        return Err(verify_bad_request("Hybrid QED document payloads are not accepted."));
    }

    match kind {
        Some("statement") => {
            if has_attestation_fields || has_guard_fields {
                return Err(verify_bad_request(
                    "Payload kind is statement but the payload contains fields from another QED document kind.",
                ));
            }
            let Ok(statement) = serde_json::from_value::<Statement>(document) else {
                return Err(verify_bad_request(
                    "Payload kind is statement but the payload does not match a QED statement.",
                ));
            };
            Ok(Json(verify_statement_document(&state, statement)))
        }
        Some("attestation") => {
            if has_statement_fields || has_guard_fields {
                return Err(verify_bad_request(
                    "Payload kind is attestation but the payload contains fields from another QED document kind.",
                ));
            }
            let Ok(attestation) = serde_json::from_value::<Attestation>(document) else {
                return Err(verify_bad_request(
                    "Payload kind is attestation but the payload does not match a QED attestation.",
                ));
            };
            Ok(Json(verify_attestation_document(&state, attestation)))
        }
        Some("guard") => {
            if has_statement_fields || has_attestation_fields {
                return Err(verify_bad_request(
                    "Payload kind is guard but the payload contains fields from another QED document kind.",
                ));
            }
            let Ok(guard) = serde_json::from_value::<crate::domain::guard::GuardDocument>(document)
            else {
                return Err(verify_bad_request(
                    "Payload kind is guard but the payload does not match a QED Guard document.",
                ));
            };
            Ok(Json(verify_guard_document(&state, guard)))
        }
        Some("stats") => {
            if has_statement_fields || has_attestation_fields || has_guard_fields {
                return Err(verify_bad_request(
                    "Payload kind is stats but the payload contains fields from another QED document kind.",
                ));
            }
            if stats_document_has_legacy_guard(&document) {
                return Err(verify_bad_request(
                    "Signed stats snapshots must not contain migration-only Guard documents.",
                ));
            }
            let Ok(stats) = serde_json::from_value::<StatsDocument>(document) else {
                return Err(verify_bad_request(
                    "Payload kind is stats but the payload does not match a signed QED stats snapshot.",
                ));
            };
            Ok(Json(verify_stats_document(&state, stats)))
        }
        Some(_) => Err(verify_bad_request("Unsupported QED document kind.")),
        None if has_statement_fields => {
            Err(verify_bad_request("Signed statements must include kind=\"statement\"."))
        }
        None if has_guard_fields => {
            Err(verify_bad_request("Signed Guards must include kind=\"guard\"."))
        }
        None if has_stats_fields => {
            Err(verify_bad_request("Signed stats snapshots must include kind=\"stats\"."))
        }
        None => {
            let Ok(attestation) = serde_json::from_value::<Attestation>(document) else {
                return Err(verify_bad_request(
                    "Payload must match a legacy QED attestation or declare its document kind.",
                ));
            };
            Ok(Json(verify_attestation_document(&state, attestation)))
        }
    }
}

fn verify_bad_request(message: &str) -> (StatusCode, Json<serde_json::Value>) {
    (StatusCode::BAD_REQUEST, Json(serde_json::json!({ "error": message })))
}

fn stats_document_has_legacy_guard(document: &serde_json::Value) -> bool {
    document
        .get("reducer_inputs")
        .and_then(|inputs| inputs.get("impostor_watch"))
        .and_then(|watch| watch.get("entries"))
        .and_then(|entries| entries.as_array())
        .is_some_and(|entries| entries.iter().any(|entry| entry.get("guard_document").is_some()))
}
fn verify_stats_document(state: &AppState, document: StatsDocument) -> serde_json::Value {
    let cryptographic = document.verify().is_ok();
    let trusted_signer = cryptographic
        && (document.public_key == attest::public_key_b58(&state.app)
            || state.app.trusted_signers.contains(&document.public_key));
    let environment_match = document.dev == state.app.dev_signer;
    serde_json::json!({
        "ok": cryptographic && trusted_signer && environment_match,
        "kind": "stats",
        "id": document.id,
        "observed_at": document.observed_at,
        "cryptographic": cryptographic,
        "trusted_signer": trusted_signer,
        "environment_match": environment_match,
        "fresh": null,
    })
}

pub(crate) fn verify_attestation_document(
    state: &AppState,
    attestation: Attestation,
) -> serde_json::Value {
    let cryptographic = crate::domain::attestation::verify(&attestation).is_ok();
    let trusted_signer = cryptographic && attest::signer_trusted(&state.app, &attestation);
    let environment_match = attestation.dev == state.app.dev_signer;
    let fresh = cryptographic && attest::fresh(&attestation);
    serde_json::json!({
        "ok": cryptographic && trusted_signer && environment_match && fresh,
        "kind": "attestation",
        "id": attestation.id,
        "cryptographic": cryptographic,
        "trusted_signer": trusted_signer,
        "environment_match": environment_match,
        "fresh": fresh,
    })
}

pub(crate) fn verify_statement_document(
    state: &AppState,
    statement: Statement,
) -> serde_json::Value {
    let cryptographic = crate::domain::statement::verify(&statement).is_ok();
    let trusted_signer = cryptographic
        && (state.app.trusted_signers.contains(&statement.signer)
            || statement.signer == state.app.signer.public_key());
    let environment_match = statement.dev == state.app.dev_signer;
    serde_json::json!({
        "ok": cryptographic && trusted_signer && environment_match,
        "kind": "statement",
        "id": statement.id,
        "cryptographic": cryptographic,
        "trusted_signer": trusted_signer,
        "environment_match": environment_match,
        "fresh": null,
    })
}

fn verify_guard_document(
    state: &AppState,
    guard: crate::domain::guard::GuardDocument,
) -> serde_json::Value {
    let cryptographic = crate::domain::guard::verify(&guard).is_ok();
    let trusted_signer = cryptographic
        && (state.app.trusted_signers.contains(&guard.public_key)
            || guard.public_key == state.app.signer.public_key());
    let environment_match = guard.dev == state.app.dev_signer;
    serde_json::json!({
        "ok": cryptographic && trusted_signer && environment_match,
        "kind": "guard",
        "id": guard.id,
        "cryptographic": cryptographic,
        "trusted_signer": trusted_signer,
        "environment_match": environment_match,
        "fresh": null,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use base64::engine::general_purpose::STANDARD;
    use tower::ServiceExt;
    fn test_state(dev_signer: bool) -> AppState {
        AppState::for_tests(Vec::new(), Vec::new(), dev_signer)
    }

    #[tokio::test]
    async fn historical_attestation_api_preserves_legacy_chain_names() {
        let attestation: crate::domain::attestation::Attestation = serde_json::from_str(
            include_str!(
                "../../../tests/fixtures/attest/fc6147d4cd42374b72246cc6340d23e26f5c411e63f613322a18413c3da243bb.json"
            ),
        )
        .expect("release-7 attestation fixture");
        let id = attestation.id.clone();
        let trusted_signer = attestation.signer.clone();
        let mut state = test_state(true);
        Arc::get_mut(&mut state.app).expect("test context is uniquely owned").trusted_signers =
            Arc::new(std::collections::HashSet::from([trusted_signer]));
        state.app.attestations.write().unwrap().insert(id.clone(), attestation);
        let app = crate::adapters::web::router(state);
        let response = app
            .oneshot(Request::get(format!("/api/attest/{id}")).body(Body::empty()).unwrap())
            .await
            .expect("historical attestation response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 1024 * 1024).await.expect("attestation body");
        let document: serde_json::Value =
            serde_json::from_slice(&body).expect("historical attestation JSON");
        assert_eq!(document["chain"], "Solana");
        assert_eq!(document["pool"]["chain"], "Solana");
        let restored: crate::domain::attestation::Attestation =
            serde_json::from_value(document).expect("restored attestation");
        crate::domain::attestation::verify(&restored)
            .expect("legacy chain name signature remains valid");
    }

    struct GuardReviewReader;

    #[async_trait::async_trait]
    impl crate::ports::ChainReader for GuardReviewReader {
        fn chain(&self) -> crate::domain::chain::Chain {
            crate::domain::chain::Chain::Base
        }

        async fn read_pool(
            &self,
            _address: &str,
        ) -> Result<crate::domain::pool::PoolInfo, crate::domain::pool::PoolError> {
            Err(crate::domain::pool::PoolError::Unknown("not a pool".to_owned()))
        }

        async fn code_at(&self, _address: &str) -> Result<Vec<u8>, crate::domain::pool::PoolError> {
            Ok(vec![0x60])
        }
    }

    #[tokio::test]
    async fn guard_get_and_json_post_documents_verify_over_the_rest_routes() {
        let address = "0x0000000000000000000000000000000000000001";
        let wallet = "0x0000000000000000000000000000000000000004";
        let app = crate::adapters::web::router(AppState::for_tests(
            Vec::new(),
            vec![Box::new(GuardReviewReader)],
            false,
        ));
        let requests = [
            Request::get(format!("/api/guard/{address}?chain=base")).body(Body::empty()).unwrap(),
            Request::post("/api/guard")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({ "address": address, "chain": "base", "wallet": wallet })
                        .to_string(),
                ))
                .unwrap(),
        ];
        for request in requests {
            let response = app.clone().oneshot(request).await.expect("Guard route response");
            assert_eq!(response.status(), StatusCode::OK);
            let body = to_bytes(response.into_body(), 1024 * 1024).await.expect("Guard response");
            let guard: serde_json::Value = serde_json::from_slice(&body).expect("Guard JSON");
            assert_eq!(guard["subject_type"], "token");
            assert_eq!(guard["subject_address"], address);
            if guard["wallet"].is_null() {
                assert!(guard["wallet_check"].is_null());
            } else {
                assert_eq!(guard["wallet"], wallet);
                assert_eq!(guard["wallet_check"]["status"], "unavailable");
            }
            let verify = app
                .clone()
                .oneshot(
                    Request::post("/verify")
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(guard.to_string()))
                        .unwrap(),
                )
                .await
                .expect("verification response");
            assert_eq!(verify.status(), StatusCode::OK);
            let body =
                to_bytes(verify.into_body(), 64 * 1024).await.expect("verification JSON body");
            let verified: serde_json::Value =
                serde_json::from_slice(&body).expect("verification JSON");
            assert_eq!(verified["kind"], "guard");
            assert_eq!(verified["ok"], true);
        }
    }

    #[tokio::test]
    async fn guard_api_rejects_missing_chain_invalid_token_and_invalid_wallet() {
        let app = crate::adapters::web::router(test_state(false));
        let address = "0x0000000000000000000000000000000000000001";
        for path in [
            format!("/api/guard/{address}"),
            "/api/guard/not-a-contract?chain=base".to_owned(),
            format!("/api/guard/{address}?chain=base&wallet=not-a-wallet"),
        ] {
            let response = app
                .clone()
                .oneshot(Request::get(path).body(Body::empty()).unwrap())
                .await
                .expect("Guard validation response");
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        }
    }

    #[tokio::test]

    async fn powers_api_accepts_an_unregistered_evm_contract_without_any_filter() {
        let address = "0x0000000000000000000000000000000000000011";
        let state = AppState::for_tests(Vec::new(), vec![Box::new(GuardReviewReader)], false);
        let version = state.app.registry_version.load(AtomicOrdering::Acquire);
        state
            .app
            .powers_cache
            .insert(
                (crate::domain::chain::Chain::Base, address.to_owned(), version),
                crate::domain::powers::PowersRecord {
                    chain: crate::domain::chain::Chain::Base,
                    contract: address.to_owned(),
                    can_seize: Vec::new(),
                    can_block: Vec::new(),
                    can_change_rules: Vec::new(),
                    token_paused: Some(false),
                    sanctions_list: None,
                    unavailable: Vec::new(),
                    source_verified_subject: crate::domain::powers::SourceVerifiedSubject::Contract,
                    source_verified: crate::domain::powers::SourceVerified::None,
                    source_verified_proxy: None,
                    observed_at: "2026-10-01T00:00:00Z".to_owned(),
                    block: Some(42),
                    slot: None,
                    reads: Vec::new(),
                },
            )
            .await;
        let response = crate::adapters::web::router(state)
            .oneshot(Request::get(format!("/api/powers/{address}")).body(Body::empty()).unwrap())
            .await
            .expect("any-contract powers response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 4096).await.expect("powers response");
        let value: serde_json::Value = serde_json::from_slice(&body).expect("powers JSON");
        assert_eq!(value["chain"], "base");
        assert_eq!(value["contract"], address);
        assert_eq!(value["token_paused"], false);
    }
    #[tokio::test]
    async fn powers_api_returns_all_registered_chain_records_or_a_filtered_record() {
        let address = "0x0000000000000000000000000000000000000011";
        let state = test_state(false);
        *state.registry.write().await = std::sync::Arc::new(
            [crate::domain::chain::Chain::RobinhoodChain, crate::domain::chain::Chain::Base]
                .into_iter()
                .map(|chain| crate::adapters::registry::Entry {
                    issuer: "Issuer".to_owned(),
                    ticker: "NVDA".to_owned(),
                    name: "Issuer NVDA".to_owned(),
                    chain,
                    contract: address.to_owned(),
                    decimals: Some(18),
                    source: "test".to_owned(),
                    source_url: "https://issuer.example".to_owned(),
                    last_checked: crate::adapters::registry::now_rfc3339(),
                    removed_at: None,
                    stale_since: None,
                    official_deployments: Vec::new(),
                })
                .collect(),
        );
        let version = state.app.registry_version.load(std::sync::atomic::Ordering::Acquire);
        for chain in
            [crate::domain::chain::Chain::RobinhoodChain, crate::domain::chain::Chain::Base]
        {
            state
                .app
                .powers_cache
                .insert(
                    (chain, address.to_owned(), version),
                    crate::domain::powers::PowersRecord {
                        chain,
                        contract: address.to_owned(),
                        can_seize: Vec::new(),
                        can_block: Vec::new(),
                        can_change_rules: Vec::new(),
                        unavailable: Vec::new(),
                        token_paused: None,
                        sanctions_list: None,
                        source_verified_subject:
                            crate::domain::powers::SourceVerifiedSubject::Contract,
                        source_verified: crate::domain::powers::SourceVerified::None,
                        source_verified_proxy: None,
                        observed_at: "2026-01-01T00:00:00Z".to_owned(),
                        block: Some(42),
                        slot: None,
                        reads: Vec::new(),
                    },
                )
                .await;
        }

        let app = crate::adapters::web::router(state);
        let response = app
            .clone()
            .oneshot(Request::get(format!("/api/powers/{address}")).body(Body::empty()).unwrap())
            .await
            .expect("multi-chain powers response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 4096).await.unwrap();
        let all: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            all.as_array()
                .unwrap()
                .iter()
                .map(|record| record["chain"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["robinhood", "base"]
        );

        let response = app
            .oneshot(
                Request::get(format!("/api/powers/{address}?chain=base"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("filtered powers response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 4096).await.unwrap();
        let filtered: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(filtered["chain"], "base");
    }
    fn entry(pool: &str, rank: usize, volume: Option<f64>, price: Option<f64>) -> LeaderboardEntry {
        LeaderboardEntry {
            rank,
            chain: "solana".to_owned(),
            chain_label: "Solana".to_owned(),
            dex: "raydium".to_owned(),
            pool: pool.to_owned(),
            base_symbol: "NVDAx".to_owned(),
            quote_symbol: "USDC".to_owned(),
            source: discovery::MarketSource::Dexscreener,
            issuer: Some("Issuer".to_owned()),
            ticker: Some("NVDA".to_owned()),
            issuer_on_base: None,
            verdict: "verified".to_owned(),
            read_status: "checked".to_owned(),
            read_reason: None,
            price_usd: price,
            change_24h_pct: Some(1.0),
            volume_24h_usd: volume,
            liquidity_usd: Some(5.0),
            txns_24h: Some(1),
            detail_url: "/validated/solana/pool".to_owned(),
            trade_url: "https://dexscreener.com/solana/pool".to_owned(),
            explorer_url: "https://solscan.io/account/pool".to_owned(),
            attestation_id: None,
            checked_at: None,
        }
    }
    fn recompute_leaderboard_counts<'a>(
        entries: impl Iterator<Item = &'a LeaderboardEntry>,
    ) -> LeaderboardCounts {
        let mut counts = LeaderboardCounts::default();
        for entry in entries {
            if entry.read_status == "unsupported_venue" {
                counts.unsupported_venue += 1;
            } else if entry.read_status == "not_read_yet" {
                counts.not_read_yet.count += 1;
                match entry.read_reason.as_deref().unwrap_or("unsupported") {
                    "rpc_limit" => counts.not_read_yet.rpc_limit += 1,
                    "transient" => counts.not_read_yet.transient += 1,
                    _ => counts.not_read_yet.unsupported += 1,
                }
            } else {
                counts.pools_checked += 1;
                match entry.verdict.as_str() {
                    "verified" => counts.issuer_matches += 1,
                    "mismatch" => counts.mismatches += 1,
                    _ => {}
                }
            }
        }
        counts
    }

    fn assert_signed_stats_recompute_from_inputs(document: &StatsDocument) {
        let inputs = &document.reducer_inputs;
        let stats = &document.stats;
        let counts = recompute_leaderboard_counts(inputs.leaderboard.iter());
        assert_eq!(stats.listed_pools, inputs.leaderboard.len());
        assert_eq!(stats.pools_checked, counts.pools_checked);
        assert_eq!(stats.issuer_matches, counts.issuer_matches);
        assert_eq!(stats.mismatches, counts.mismatches);
        assert_eq!(stats.unsupported_venue, counts.unsupported_venue);
        assert_eq!(stats.not_read_yet, counts.not_read_yet);

        for chain in &stats.by_chain {
            let counts = recompute_leaderboard_counts(inputs.leaderboard.iter().filter(|entry| {
                discovery::chain_from_dex_id(&entry.chain)
                    .is_some_and(|entry_chain| discovery::chain_slug(entry_chain) == chain.chain)
            }));
            assert_eq!(chain.counts, counts);
        }

        assert_eq!(stats.registry.active_entries, inputs.active_registry.len());
        for issuer in &stats.registry.by_issuer {
            let entries = inputs
                .active_registry
                .iter()
                .filter(|entry| entry.issuer == issuer.issuer)
                .collect::<Vec<_>>();
            assert_eq!(issuer.entries, entries.len());
            let mut chains = entries
                .iter()
                .map(|entry| discovery::chain_label(entry.chain).to_owned())
                .collect::<Vec<_>>();
            chains.sort();
            chains.dedup();
            assert_eq!(issuer.chains, chains);
        }

        let observed_at =
            DateTime::parse_from_rfc3339(&document.observed_at).expect("snapshot observation time");
        let observed_week = observed_at.with_timezone(&Utc).iso_week();
        let weekly_count = inputs
            .impostor_watch
            .entries
            .iter()
            .filter(|entry| {
                DateTime::parse_from_rfc3339(&entry.first_seen_at)
                    .ok()
                    .is_some_and(|seen_at| seen_at.with_timezone(&Utc).iso_week() == observed_week)
            })
            .count();
        let observed_timestamp = observed_at.timestamp();
        let cutoff = observed_timestamp - chrono::Duration::days(7).num_seconds();
        let recent_count = inputs
            .impostor_watch
            .entries
            .iter()
            .filter(|entry| {
                DateTime::parse_from_rfc3339(&entry.last_seen_at).ok().is_some_and(|seen_at| {
                    (cutoff..=observed_timestamp).contains(&seen_at.timestamp())
                })
            })
            .count();
        let watch = &stats.publisher_catalog_watch;
        assert_eq!(watch.first_flagged_this_week, weekly_count);
        assert_eq!(watch.evicted_entries, inputs.impostor_watch.evicted_entries);
        assert_eq!(
            watch.evicted_unsupported_candidates,
            inputs.impostor_watch.evicted_unsupported_candidates
        );
        assert_eq!(
            watch.rejected_oversize_entries,
            inputs.impostor_watch.rejected_oversize_entries
        );
        assert_eq!(
            watch.rejected_oversize_unsupported_candidates,
            inputs.impostor_watch.rejected_oversize_unsupported_candidates
        );
        assert_eq!(watch.source_unavailable_since, inputs.impostor_watch.source_unavailable_since);
        assert_eq!(
            watch.catalog_absent_tokens_truncated,
            recent_count > IMPOSTOR_STATS_TABLE_LIMIT
        );

        let leaderboard_hash = crate::domain::attestation::canonical_json(&inputs.leaderboard)
            .expect("leaderboard input canonical JSON");
        assert_eq!(
            inputs.hashes.leaderboard,
            crate::domain::attestation::hex_lower(&Sha256::digest(leaderboard_hash))
        );
        let watch_hash = crate::domain::attestation::canonical_json(&inputs.impostor_watch)
            .expect("watch input canonical JSON");
        assert_eq!(
            inputs.hashes.impostor_watch,
            crate::domain::attestation::hex_lower(&Sha256::digest(watch_hash))
        );
    }
    #[test]
    fn catalog_watch_recency_includes_the_seven_day_boundary_only() {
        let now = Utc::now().timestamp();
        let cutoff = now - chrono::Duration::days(7).num_seconds();
        let timestamp = |seconds| {
            DateTime::<Utc>::from_timestamp(seconds, 0).expect("valid Unix timestamp").to_rfc3339()
        };

        assert!(is_within_last_7_days(&timestamp(cutoff), cutoff, now));
        assert!(!is_within_last_7_days(&timestamp(cutoff - 1), cutoff, now));
        assert!(!is_within_last_7_days(&timestamp(now + 1), cutoff, now));
        assert!(!is_within_last_7_days("not-a-timestamp", cutoff, now));
    }
    #[tokio::test]
    async fn public_stats_summarize_leaderboard_reads_and_active_issuer_registry() {
        let state = test_state(false);
        let at_chain = |mut row: LeaderboardEntry, chain: &str, label: &str| {
            row.chain = chain.to_owned();
            row.chain_label = label.to_owned();
            row
        };
        let unread = |pool: &str, chain: &str, label: &str, reason: &str| {
            let mut row = at_chain(entry(pool, 1, None, None), chain, label);
            row.verdict = "unknown".to_owned();
            row.read_status = "not_read_yet".to_owned();
            row.read_reason = Some(reason.to_owned());
            row
        };
        let mut mismatch = at_chain(entry("mismatch", 2, None, None), "base", "Base");
        mismatch.verdict = "mismatch".to_owned();
        let mut completed_unknown =
            at_chain(entry("checked-unknown", 3, None, None), "base", "Base");
        completed_unknown.verdict = "unknown".to_owned();
        let mut unsupported_venue =
            at_chain(entry("unsupported-venue", 4, None, None), "base", "Base");
        unsupported_venue.read_status = "unsupported_venue".to_owned();
        unsupported_venue.read_reason = Some("unsupported_venue".to_owned());
        let rows = vec![
            at_chain(entry("verified", 1, None, None), "base", "Base"),
            mismatch,
            completed_unknown,
            unsupported_venue,
            unread("rpc-limit", "ethereum", "Ethereum", "rpc_limit"),
            unread("transient", "solana", "Solana", "transient"),
            unread("unsupported", "robinhoodchain", "Robinhood Chain", "unsupported"),
        ];
        let scanned_at = Utc::now().to_rfc3339();
        let catalog_absent = discovery::ImpostorEntry {
            chain: "base".to_owned(),
            chain_label: "Base".to_owned(),
            ticker: "NVDA".to_owned(),
            publisher: "Robinhood".to_owned(),
            symbol: "NVDAx".to_owned(),
            name: "NVIDIA xStock".to_owned(),
            address: "0x0000000000000000000000000000000000000002".to_owned(),
            first_seen_at: scanned_at.clone(),
            last_seen_at: scanned_at.clone(),
            volume_24h_usd: Some(1234.5),
            source: discovery::MarketSource::Dexscreener,
            guard_url: "/guard/base/0x0000000000000000000000000000000000000002".to_owned(),
            reason: "The address is absent from the publisher's published deployment catalog."
                .to_owned(),
            reads: vec![crate::domain::attestation::Read {
                method: "eth_call".to_owned(),
                params: serde_json::json!([
                    "0x0000000000000000000000000000000000000002",
                    "symbol()"
                ]),
                result_hash: "a".repeat(64),
                raw_result: Some(serde_json::json!("Some(\"NVDAx\")")),
                block: Some(42),
                slot: None,
            }],
            on_chain_symbol: None,
            on_chain_name: None,
            publisher_catalog_snapshot_hash: Some("b".repeat(64)),
            evidence_truncated: false,
            guard_document: None,
        };
        let mut catalog_entries = Vec::with_capacity(IMPOSTOR_STATS_TABLE_LIMIT + 1);
        catalog_entries.push(catalog_absent);
        for index in 1..=IMPOSTOR_STATS_TABLE_LIMIT {
            let mut entry = catalog_entries[0].clone();
            entry.ticker = format!("NVDA{index}");
            entry.address = format!("0x{index:040x}");
            catalog_entries.push(entry);
        }
        let mut stale_entry = catalog_entries[0].clone();
        stale_entry.ticker = "STALE".to_owned();
        stale_entry.address = format!("0x{:040x}", IMPOSTOR_STATS_TABLE_LIMIT + 1);
        stale_entry.last_seen_at = (Utc::now() - chrono::Duration::days(8)).to_rfc3339();
        catalog_entries.push(stale_entry);
        let mut recently_seen_entry = catalog_entries[0].clone();
        recently_seen_entry.ticker = "RECENT".to_owned();
        recently_seen_entry.address = format!("0x{:040x}", IMPOSTOR_STATS_TABLE_LIMIT + 2);
        recently_seen_entry.first_seen_at = (Utc::now() - chrono::Duration::days(8)).to_rfc3339();
        catalog_entries.push(recently_seen_entry);
        *state.leaderboard.write().await = discovery::Leaderboard {
            updated_at: "2026-10-06T12:00:00Z".to_owned(),
            total: rows.len(),
            entries: rows,
            impostors: discovery::ImpostorSnapshot {
                scanned_at: scanned_at.clone(),
                unsupported_seen: 2,
                official_on_unsupported_chain: 1,
                entries: catalog_entries,
                unsupported_candidates: (0..2)
                    .map(|index| discovery::UnsupportedImpostorCandidate {
                        dex_chain_id: format!("unsupported-{index}"),
                        ticker: "NVDA".to_owned(),
                        publisher: "Robinhood".to_owned(),
                        symbol: "NVDAx".to_owned(),
                        name: "NVIDIA xStock".to_owned(),
                        address: format!("token-{index}"),
                        first_seen_at: scanned_at.clone(),
                        last_seen_at: scanned_at.clone(),
                        volume_24h_usd: None,
                        source: discovery::MarketSource::Dexscreener,
                        evidence_truncated: false,
                    })
                    .collect(),
                ..discovery::ImpostorSnapshot::default()
            },
            ..discovery::Leaderboard::default()
        };
        let registry_entry =
            |issuer: &str, chain: Chain, ticker: &str, removed: bool| registry::Entry {
                issuer: issuer.to_owned(),
                ticker: ticker.to_owned(),
                name: ticker.to_owned(),
                chain,
                contract: format!("0x{ticker}"),
                decimals: Some(18),
                source: "test".to_owned(),
                source_url: "https://example.invalid".to_owned(),
                last_checked: "2026-10-06T00:00:00Z".to_owned(),
                removed_at: removed.then(|| "2026-10-06T00:00:00Z".to_owned()),
                stale_since: None,
                official_deployments: Vec::new(),
            };
        *state.registry.write().await = Arc::new(vec![
            registry_entry("Backed xStocks", Chain::Base, "NVDA", false),
            registry_entry("Backed xStocks", Chain::RobinhoodChain, "TSLA", false),
            registry_entry("Backed xStocks", Chain::Ethereum, "OLD", true),
            registry_entry("Robinhood", Chain::RobinhoodChain, "NVDA", false),
            registry_entry("Ondo", Chain::Bnb, "AAPL", false),
        ]);
        let response = crate::adapters::web::router(state.clone())
            .oneshot(Request::get("/api/stats").body(Body::empty()).unwrap())
            .await
            .expect("public stats response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 1_048_576).await.expect("stats response body");
        let value: serde_json::Value = serde_json::from_slice(&body).expect("stats JSON");
        let stats = &value["stats"];
        let watch = &stats["publisher_catalog_watch"];
        assert_eq!(watch["first_flagged_this_week"], 22);
        assert_eq!(watch["currently_flagged_last_7_days"], 22);
        assert!(
            stats["headline"].as_str().unwrap().contains(
                "22 catalog observations are currently flagged (seen in the last 7 days)"
            )
        );
        assert_eq!(watch["unsupported_chain_candidates_seen"], 2);
        assert_eq!(watch["official_on_unsupported_chain"], 1);
        assert!(
            !stats["headline"]
                .as_str()
                .unwrap()
                .contains("official deployments on unsupported chains")
        );
        assert_eq!(watch["last_scanned_at"], scanned_at);
        assert!(
            watch.get("catalog_absent_tokens").is_none(),
            "the signed summary must not duplicate complete catalog rows"
        );

        assert_eq!(stats["generated_at"], "2026-10-06T12:00:00Z");
        assert_eq!(stats["listed_pools"], 7);
        assert_eq!(stats["pools_checked"], 3);
        assert_eq!(stats["issuer_matches"], 1);
        assert_eq!(stats["mismatches"], 1);
        assert_eq!(stats["unsupported_venue"], 1);
        assert_eq!(stats["not_read_yet"]["count"], 3);
        assert_eq!(stats["not_read_yet"]["rpc_limit"], 1);
        assert_eq!(stats["not_read_yet"]["transient"], 1);
        assert_eq!(stats["not_read_yet"]["unsupported"], 1);
        assert_eq!(stats["by_chain"].as_array().unwrap().len(), 5);
        let base = stats["by_chain"]
            .as_array()
            .unwrap()
            .iter()
            .find(|chain| chain["chain"] == "base")
            .expect("Base counts");
        assert_eq!(base["counts"]["pools_checked"], 3);
        assert_eq!(base["counts"]["mismatches"], 1);
        assert_eq!(base["counts"]["unsupported_venue"], 1);
        let robinhood = stats["by_chain"]
            .as_array()
            .unwrap()
            .iter()
            .find(|chain| chain["chain"] == "robinhood")
            .expect("canonical Robinhood counts");
        assert_eq!(robinhood["counts"]["not_read_yet"]["count"], 1);
        assert_eq!(robinhood["counts"]["not_read_yet"]["unsupported"], 1);
        assert_eq!(stats["registry"]["active_entries"], 4);
        let backed = stats["registry"]["by_issuer"]
            .as_array()
            .unwrap()
            .iter()
            .find(|issuer| issuer["issuer"] == "Backed xStocks")
            .expect("Backed xStocks registry counts");
        assert_eq!(backed["entries"], 2);
        assert_eq!(backed["chains"].as_array().unwrap().len(), 2);
        assert_eq!(value["page"], 1);
        assert_eq!(value["pages"], 1);
        assert_eq!(value["leaderboard_total"], 7);
        assert_eq!(value["leaderboard"].as_array().unwrap().len(), 7);
        assert_eq!(value["impostor_candidates_total"], 23);
        assert_eq!(value["impostor_candidates"].as_array().unwrap().len(), 23);
        assert!(
            value["impostor_candidates"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["ticker"] == "STALE")
        );
        assert!(
            value["impostor_candidates"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["ticker"] == "RECENT")
        );
        assert_eq!(value["unsupported_candidates_total"], 2);
        assert_eq!(value["unsupported_candidates"].as_array().unwrap().len(), 2);
        assert_eq!(value["active_registry_total"], 4);
        assert_eq!(value["active_registry"].as_array().unwrap().len(), 4);
        assert_eq!(value["hashes"]["leaderboard"].as_str().unwrap().len(), 64);
        assert_eq!(value["hashes"]["impostor_watch"].as_str().unwrap().len(), 64);
        let response = crate::adapters::web::router(state.clone())
            .oneshot(Request::get("/stats.json").body(Body::empty()).unwrap())
            .await
            .expect("signed stats snapshot response");
        assert_eq!(response.status(), StatusCode::OK);
        let body =
            to_bytes(response.into_body(), 1_048_576).await.expect("signed stats snapshot body");
        let document: StatsDocument = serde_json::from_slice(&body).expect("strict stats document");
        document.verify().expect("signed stats document verifies");
        assert_signed_stats_recompute_from_inputs(&document);
        assert!(
            document
                .reducer_inputs
                .impostor_watch
                .entries
                .iter()
                .any(|entry| !entry.reads.is_empty())
        );

        let response = crate::adapters::web::router(state.clone())
            .oneshot(Request::get("/stats").body(Body::empty()).unwrap())
            .await
            .expect("stats HTML response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 1_048_576).await.unwrap();
        let html = String::from_utf8(body.to_vec()).expect("stats HTML");
        assert_eq!(html.matches("<tr>").count(), IMPOSTOR_STATS_TABLE_LIMIT + 1);
    }

    #[tokio::test]
    async fn signed_stats_support_large_registry_inputs_and_recompute_totals() {
        let state = test_state(true);
        let registry = (0..4016)
            .map(|index| {
                let (issuer, chain) = match index % 3 {
                    0 => ("Backed xStocks", Chain::Base),
                    1 => ("Robinhood", Chain::RobinhoodChain),
                    _ => ("Ondo", Chain::Ethereum),
                };
                registry::Entry {
                    issuer: issuer.to_owned(),
                    ticker: format!("T{index}"),
                    name: format!("Token {index}"),
                    chain,
                    contract: format!("0x{index:040x}"),
                    decimals: Some(18),
                    source: "fixture".to_owned(),
                    source_url: "https://example.invalid".to_owned(),
                    last_checked: "2026-10-06T00:00:00Z".to_owned(),
                    removed_at: None,
                    stale_since: None,
                    official_deployments: (0..3)
                        .map(|deployment| registry::OfficialDeployment {
                            network: format!("network-{deployment}"),
                            address: format!("0x{index:040x}"),
                            wrapper_address: Some(format!("0x{:040x}", index + deployment)),
                            wrapper_address_v2: Some(format!(
                                "0x{:040x}",
                                index + deployment + 10_000
                            )),
                        })
                        .collect(),
                }
            })
            .collect::<Vec<_>>();
        let registry_size = serde_json::to_vec(&registry).unwrap().len();
        assert!(registry_size > 2 * 1_048_576);

        *state.registry.write().await = Arc::new(registry);
        refresh_stats_snapshot(&state).await.expect("prepare large stats snapshot");
        let response = crate::adapters::web::router(state.clone())
            .oneshot(Request::get("/stats.json").body(Body::empty()).unwrap())
            .await
            .expect("signed stats snapshot route");
        assert_eq!(response.status(), StatusCode::OK);
        let body =
            to_bytes(response.into_body(), MAX_SIGNED_STATS_DOCUMENT_BYTES + 1024).await.unwrap();
        let document: StatsDocument = serde_json::from_slice(&body).expect("strict stats document");
        document.verify().expect("signed stats snapshot verifies");
        assert_signed_stats_recompute_from_inputs(&document);
        assert_eq!(document.reducer_inputs.active_registry.len(), 4016);
        assert!(document.reducer_inputs.impostor_watch.entries.is_empty());
        let payload_size =
            crate::domain::attestation::canonical_json(&document.payload()).unwrap().len();
        assert!(payload_size > crate::domain::attestation::MAX_ATTESTATION_BYTES);
        assert!(payload_size <= MAX_SIGNED_STATS_DOCUMENT_BYTES);
        assert!(body.len() > 2 * 1_048_576);

        let verify_response = crate::adapters::web::router(state)
            .oneshot(
                Request::post("/verify")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .expect("stats verification route");
        let verify_status = verify_response.status();
        let verify_body =
            to_bytes(verify_response.into_body(), 32_768).await.expect("verify response body");
        assert_eq!(
            verify_status,
            StatusCode::OK,
            "unexpected stats verification response: {}",
            String::from_utf8_lossy(&verify_body)
        );
        let verified: serde_json::Value =
            serde_json::from_slice(&verify_body).expect("stats verification result");
        assert_eq!(verified["ok"], true);
    }

    #[tokio::test]
    async fn signed_stats_keep_maximum_watch_snapshot_bounded_after_restart() {
        let state = test_state(true);
        let directory = tempfile::tempdir().expect("temporary watch directory");
        let scanned_at = "2026-10-06T12:00:00Z";
        // Every free-text field is over its cap and JSON-escapes to six bytes per byte, so
        // the clipped retained set is the largest one the watch can store and sign.
        let over_cap = |bytes: usize| "\u{1}".repeat(bytes * 2);
        let mut persisted = discovery::Leaderboard::default();
        persisted.updated_at = scanned_at.to_owned();
        persisted.entries.push(entry("fixture-pool", 1, None, None));
        persisted.impostors.entries = (0..129)
            .map(|index| {
                let address = format!("0x{index:040x}");
                let reads = ["symbol()", "name()"]
                    .into_iter()
                    .map(|selector| crate::domain::attestation::Read {
                        method: "eth_call".to_owned(),
                        params: serde_json::json!([address, selector, "p".repeat(2048)]),
                        result_hash: format!("{index:064x}"),
                        raw_result: Some(serde_json::json!("x".repeat(2048))),
                        block: Some(42),
                        slot: None,
                    })
                    .collect();
                discovery::ImpostorEntry {
                    chain: "base".to_owned(),
                    chain_label: "Base".to_owned(),
                    ticker: over_cap(discovery::MAX_IMPOSTOR_LABEL_BYTES),
                    publisher: over_cap(discovery::MAX_IMPOSTOR_LABEL_BYTES),
                    symbol: over_cap(discovery::MAX_IMPOSTOR_LABEL_BYTES),
                    name: over_cap(discovery::MAX_IMPOSTOR_LABEL_BYTES),
                    address: address.clone(),
                    first_seen_at: scanned_at.to_owned(),
                    last_seen_at: scanned_at.to_owned(),
                    volume_24h_usd: Some(1.0),
                    source: discovery::MarketSource::Dexscreener,
                    guard_url: format!("/guard/base/{address}"),
                    reason: over_cap(discovery::MAX_IMPOSTOR_REASON_BYTES),
                    reads,
                    on_chain_symbol: Some(over_cap(discovery::MAX_IMPOSTOR_LABEL_BYTES)),
                    on_chain_name: Some(over_cap(discovery::MAX_IMPOSTOR_LABEL_BYTES)),
                    publisher_catalog_snapshot_hash: Some("b".repeat(64)),
                    evidence_truncated: false,
                    guard_document: None,
                }
            })
            .collect();
        let mut invalid_supported = persisted.impostors.entries[0].clone();
        invalid_supported.address = "0xnot-a-token-address".to_owned();
        persisted.impostors.entries.push(invalid_supported);
        persisted.impostors.unsupported_candidates = (0..257)
            .map(|index| discovery::UnsupportedImpostorCandidate {
                dex_chain_id: "u".repeat(32),
                ticker: over_cap(discovery::MAX_IMPOSTOR_LABEL_BYTES),
                publisher: over_cap(discovery::MAX_IMPOSTOR_LABEL_BYTES),
                symbol: over_cap(discovery::MAX_IMPOSTOR_LABEL_BYTES),
                name: over_cap(discovery::MAX_IMPOSTOR_LABEL_BYTES),
                address: format!("{index:\"<128}"),
                first_seen_at: scanned_at.to_owned(),
                last_seen_at: scanned_at.to_owned(),
                volume_24h_usd: Some(1.0),
                source: discovery::MarketSource::Dexscreener,
                evidence_truncated: false,
            })
            .collect();
        let mut invalid_unsupported = persisted.impostors.unsupported_candidates[0].clone();
        invalid_unsupported.address = "token address with spaces".to_owned();
        persisted.impostors.unsupported_candidates.push(invalid_unsupported);

        discovery::save_leaderboard(directory.path(), &persisted);
        let restored =
            discovery::load_leaderboard(directory.path()).expect("persisted board restored");
        assert_eq!(restored.impostors.entries.len(), 128);
        assert_eq!(restored.impostors.unsupported_candidates.len(), 256);
        assert_eq!(restored.impostors.evicted_entries, 1);
        assert_eq!(restored.impostors.evicted_unsupported_candidates, 1);
        assert_eq!(restored.impostors.rejected_oversize_entries, 1);
        assert_eq!(restored.impostors.rejected_oversize_unsupported_candidates, 1);
        assert!(restored.impostors.entries.iter().all(|entry| {
            entry.reads.len() == 2
                && entry.evidence_truncated
                && entry.reason.len() == discovery::MAX_IMPOSTOR_REASON_BYTES
                && [&entry.ticker, &entry.publisher, &entry.symbol, &entry.name]
                    .into_iter()
                    .chain(entry.on_chain_symbol.as_ref())
                    .chain(entry.on_chain_name.as_ref())
                    .all(|text| text.len() == discovery::MAX_IMPOSTOR_LABEL_BYTES)
                && entry
                    .publisher_catalog_snapshot_hash
                    .as_ref()
                    .is_some_and(|hash| hash.len() == 64)
                && entry.reads.iter().all(|read| {
                    read.params[0] == entry.address
                        && matches!(read.params[1].as_str(), Some("symbol()" | "name()"))
                        && serde_json::to_vec(&read.params).unwrap().len()
                            <= discovery::MAX_IMPOSTOR_READ_PARAMS_BYTES
                        && read.raw_result.as_ref().is_some_and(|result| {
                            serde_json::to_vec(result).unwrap().len()
                                <= discovery::MAX_IMPOSTOR_READ_RESULT_BYTES
                        })
                })
                && entry.guard_document.is_none()
        }));
        assert!(restored.impostors.unsupported_candidates.iter().all(|candidate| {
            candidate.evidence_truncated
                && [&candidate.ticker, &candidate.publisher, &candidate.symbol, &candidate.name]
                    .into_iter()
                    .all(|text| text.len() == discovery::MAX_IMPOSTOR_LABEL_BYTES)
        }));
        *state.leaderboard.write().await = restored;
        refresh_stats_snapshot(&state).await.expect("prepare bounded signed snapshot");

        let response = crate::adapters::web::router(state)
            .oneshot(Request::get("/stats.json").body(Body::empty()).unwrap())
            .await
            .expect("maximum stats response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), MAX_SIGNED_STATS_DOCUMENT_BYTES + 1024)
            .await
            .expect("bounded maximum stats body");
        let document: StatsDocument =
            serde_json::from_slice(&body).expect("strict bounded stats document");
        document.verify().expect("bounded stats signature");
        assert_signed_stats_recompute_from_inputs(&document);
        assert_eq!(document.reducer_inputs.impostor_watch.entries.len(), 128);
        assert_eq!(document.reducer_inputs.impostor_watch.unsupported_candidates.len(), 256);
        assert_eq!(document.reducer_inputs.impostor_watch.evicted_entries, 1);
        assert_eq!(document.reducer_inputs.impostor_watch.evicted_unsupported_candidates, 1);
        assert_eq!(document.reducer_inputs.impostor_watch.rejected_oversize_entries, 1);
        assert_eq!(
            document.reducer_inputs.impostor_watch.rejected_oversize_unsupported_candidates,
            1
        );
        assert!(
            document
                .reducer_inputs
                .impostor_watch
                .entries
                .iter()
                .all(|entry| entry.reads.len() == 2 && entry.evidence_truncated)
        );
        assert!(
            document
                .reducer_inputs
                .impostor_watch
                .unsupported_candidates
                .iter()
                .all(|candidate| candidate.evidence_truncated)
        );
        let payload_size =
            crate::domain::attestation::canonical_json(&document.payload()).unwrap().len();
        assert!(payload_size <= MAX_SIGNED_STATS_DOCUMENT_BYTES);
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(
            json["reducer_inputs"]["impostor_watch"]["entries"][0].get("guard_document").is_none()
        );
        assert!(
            serde_json::to_vec(
                &json["reducer_inputs"]["impostor_watch"]["entries"][0]["reads"][0]["raw_result"]
            )
            .unwrap()
            .len()
                <= discovery::MAX_IMPOSTOR_READ_RESULT_BYTES
        );
    }

    #[tokio::test]
    async fn verify_rejects_two_mib_non_stats_payload() {
        let state = test_state(false);
        let prefix = r#"{"kind":"attestation","padding":""#;
        let suffix = r#""}"#;
        let total_len = 2 * 1024 * 1024;
        let padding_len = total_len - prefix.len() - suffix.len();
        let mut body = String::with_capacity(total_len);
        body.push_str(prefix);
        body.push_str(&"x".repeat(padding_len));
        body.push_str(suffix);
        assert_eq!(body.len(), total_len);

        let response = crate::adapters::web::router(state)
            .oneshot(
                Request::post("/verify")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .expect("non-stats verification response");
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }
    #[tokio::test]
    async fn stats_downloads_are_signed_and_verify_strictly_by_kind() {
        let state = test_state(false);
        let first_seen_at = Utc::now().to_rfc3339();
        let mut leaderboard_entry =
            entry("0x0000000000000000000000000000000000000001", 1, Some(5.0), Some(1.0));
        leaderboard_entry.chain = "base".to_owned();
        leaderboard_entry.chain_label = "Base".to_owned();
        leaderboard_entry.dex = "uniswap".to_owned();
        leaderboard_entry.source = discovery::MarketSource::Geckoterminal;
        leaderboard_entry.detail_url =
            "/validated/base/0x0000000000000000000000000000000000000001".to_owned();
        leaderboard_entry.trade_url =
            "https://www.geckoterminal.com/base/pools/0x0000000000000000000000000000000000000001"
                .to_owned();
        leaderboard_entry.explorer_url =
            "https://basescan.org/address/0x0000000000000000000000000000000000000001".to_owned();
        state.leaderboard.write().await.entries.push(leaderboard_entry);
        state.leaderboard.write().await.impostors.entries.push(discovery::ImpostorEntry {
            chain: "base".to_owned(),
            chain_label: "Base".to_owned(),
            ticker: "NVDA".to_owned(),
            publisher: "Backed xStocks".to_owned(),
            symbol: "NVDAx".to_owned(),
            name: " \t=IMPORTXML".to_owned(),
            address: "0x0000000000000000000000000000000000000002".to_owned(),
            first_seen_at: first_seen_at.clone(),
            last_seen_at: first_seen_at,
            volume_24h_usd: Some(1234.56789),
            source: discovery::MarketSource::Dexscreener,
            guard_url: "/guard/base/0x0000000000000000000000000000000000000002".to_owned(),
            reason: "The address is absent from the publisher's published deployment catalog."
                .to_owned(),
            reads: Vec::new(),
            on_chain_symbol: None,
            on_chain_name: None,
            publisher_catalog_snapshot_hash: Some("b".repeat(64)),
            evidence_truncated: false,
            guard_document: None,
        });
        state.leaderboard.write().await.impostors.official_on_unsupported_chain = 1;
        let page_response = crate::adapters::web::router(state.clone())
            .oneshot(Request::get("/stats").body(Body::empty()).unwrap())
            .await
            .expect("publisher catalog stats page");
        assert_eq!(page_response.status(), StatusCode::OK);
        let page_body = to_bytes(page_response.into_body(), 32_768).await.unwrap();
        let page_html = String::from_utf8(page_body.to_vec()).expect("stats page HTML");
        assert!(page_html.contains("Publisher deployment catalog watch"));
        assert!(page_html.contains("Last searched at"));
        assert!(page_html.contains("Market data: <a href=\"https://dexscreener.com\""));
        assert!(page_html.contains("href=\"https://www.geckoterminal.com\""));
        assert!(page_html.contains("href=\"https://www.coingecko.com/en/api_terms\""));
        assert!(page_html.contains("class=\"market-data-attribution\""));
        assert!(page_html.contains("<dl class=\"statement-summary-grid\">"));
        assert!(page_html.contains("Download signed JSON"));
        assert!(
            page_html
                .contains("catalog observations are currently flagged (seen in the last 7 days)")
        );
        assert!(page_html.contains("All retained observations seen in the last 7 days are shown."));
        assert!(
            page_html
                .contains("1 official deployments on unsupported chains were counted separately")
        );
        let home_response = crate::adapters::web::router(state.clone())
            .oneshot(Request::get("/").body(Body::empty()).unwrap())
            .await
            .expect("home page response");
        assert_eq!(home_response.status(), StatusCode::OK);
        let home_body = to_bytes(home_response.into_body(), 1_048_576).await.unwrap();
        let home_html = String::from_utf8(home_body.to_vec()).expect("home page HTML");
        assert!(home_html.contains("1 currently flagged (seen in the last 7 days)"));
        assert!(home_html.contains("1 new this UTC week"));
        let json_response = crate::adapters::web::router(state.clone())
            .oneshot(Request::get("/stats.json").body(Body::empty()).unwrap())
            .await
            .expect("signed stats JSON response");
        assert_eq!(json_response.status(), StatusCode::OK);
        assert_eq!(
            json_response.headers()[header::CONTENT_DISPOSITION],
            "attachment; filename=\"qed-stats.json\""
        );
        let json_body =
            to_bytes(json_response.into_body(), 1_048_576).await.expect("signed stats JSON body");
        let document: StatsDocument =
            serde_json::from_slice(&json_body).expect("strict signed stats snapshot");
        let snapshot_value: serde_json::Value =
            serde_json::from_slice(&json_body).expect("raw signed snapshot JSON");
        assert_eq!(snapshot_value["reducer_inputs"]["leaderboard"][0]["source"], "geckoterminal");
        assert_eq!(
            snapshot_value["reducer_inputs"]["impostor_watch"]["entries"][0]["source"],
            "dexscreener"
        );
        let catalog_entry = &document.reducer_inputs.impostor_watch.entries[0];
        assert_eq!(catalog_entry.volume_24h_usd, Some(1234.56789));
        assert_eq!(catalog_entry.name, " \t=IMPORTXML");
        assert!(!document.stats.publisher_catalog_watch.catalog_absent_tokens_truncated);
        assert_eq!(document.stats.publisher_catalog_watch.currently_flagged_last_7_days, 1);
        assert_eq!(document.stats.publisher_catalog_watch.first_flagged_this_week, 1);
        assert_eq!(document.reducer_inputs.impostor_watch.official_on_unsupported_chain, 1);
        assert_eq!(document.stats.publisher_catalog_watch.official_on_unsupported_chain, 1);
        document.verify().expect("stats snapshot signature");
        assert_eq!(document.reducer_inputs.impostor_watch.entries.len(), 1);
        assert_eq!(document.reducer_inputs.impostor_watch.entries[0].on_chain_symbol, None);
        assert_eq!(document.reducer_inputs.leaderboard.len(), 1);
        let repeated = crate::adapters::web::router(state.clone())
            .oneshot(Request::get("/stats.json").body(Body::empty()).unwrap())
            .await
            .expect("cached signed stats response");
        let repeated_body = to_bytes(repeated.into_body(), 1_048_576).await.unwrap();
        assert_eq!(repeated_body, json_body);

        let verify_response = crate::adapters::web::router(state.clone())
            .oneshot(
                Request::post("/verify")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&document).unwrap()))
                    .unwrap(),
            )
            .await
            .expect("stats verify response");
        assert_eq!(verify_response.status(), StatusCode::OK);
        let verify_body = to_bytes(verify_response.into_body(), 4096).await.unwrap();
        let verification: serde_json::Value = serde_json::from_slice(&verify_body).unwrap();
        assert_eq!(verification["kind"], "stats");
        assert_eq!(verification["cryptographic"], true);
        assert_eq!(verification["trusted_signer"], true);
        assert_eq!(verification["environment_match"], true);
        assert_eq!(verification["ok"], true);
        assert!(verification["fresh"].is_null());
        let mut with_guard_document = serde_json::to_value(&document).unwrap();
        with_guard_document["reducer_inputs"]["impostor_watch"]["entries"][0]["guard_document"] =
            serde_json::to_value(crate::domain::guard::signed_test_guard([9; 32])).unwrap();
        let with_guard_response = crate::adapters::web::router(state.clone())
            .oneshot(
                Request::post("/verify")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&with_guard_document).unwrap()))
                    .unwrap(),
            )
            .await
            .expect("migration-only Guard verification response");
        assert_eq!(with_guard_response.status(), StatusCode::BAD_REQUEST);

        let mut with_unknown_read = serde_json::to_value(&document).unwrap();
        with_unknown_read["reducer_inputs"]["impostor_watch"]["entries"][0]["reads"] = serde_json::json!([{
            "method": "eth_call",
            "params": [],
            "result_hash": "0".repeat(64),
            "block": null,
            "slot": null,
            "untrusted_extra": true
        }]);
        let with_unknown_read_response = crate::adapters::web::router(state.clone())
            .oneshot(
                Request::post("/verify")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&with_unknown_read).unwrap()))
                    .unwrap(),
            )
            .await
            .expect("nested Read verification response");
        assert_eq!(with_unknown_read_response.status(), StatusCode::BAD_REQUEST);

        let mut tampered = serde_json::to_value(&document).unwrap();
        tampered["stats"]["listed_pools"] = serde_json::json!(document.stats.listed_pools + 1);
        let tampered_response = crate::adapters::web::router(state.clone())
            .oneshot(
                Request::post("/verify")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&tampered).unwrap()))
                    .unwrap(),
            )
            .await
            .expect("tampered stats verification response");
        assert_eq!(tampered_response.status(), StatusCode::OK);
        let tampered_body = to_bytes(tampered_response.into_body(), 4096).await.unwrap();
        let tampered_result: serde_json::Value = serde_json::from_slice(&tampered_body).unwrap();
        assert_eq!(tampered_result["cryptographic"], false);
        assert_eq!(tampered_result["ok"], false);

        let mut unknown = serde_json::to_value(&document).unwrap();
        unknown["stats"]["publisher_catalog_watch"]["unknown"] = serde_json::json!(true);
        let unknown_response = crate::adapters::web::router(state.clone())
            .oneshot(
                Request::post("/verify")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&unknown).unwrap()))
                    .unwrap(),
            )
            .await
            .expect("strict stats schema response");
        assert_eq!(unknown_response.status(), StatusCode::BAD_REQUEST);
        let mut unknown_leaderboard_row = serde_json::to_value(&document).unwrap();
        unknown_leaderboard_row["reducer_inputs"]["leaderboard"][0]["unknown"] =
            serde_json::json!(true);
        let unknown_leaderboard_response = crate::adapters::web::router(state.clone())
            .oneshot(
                Request::post("/verify")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&unknown_leaderboard_row).unwrap()))
                    .unwrap(),
            )
            .await
            .expect("strict nested leaderboard response");
        assert_eq!(unknown_leaderboard_response.status(), StatusCode::BAD_REQUEST);

        let csv_response = crate::adapters::web::router(state)
            .oneshot(Request::get("/stats.csv").body(Body::empty()).unwrap())
            .await
            .expect("signed stats CSV response");
        assert_eq!(csv_response.status(), StatusCode::OK);
        assert_eq!(
            csv_response.headers()[header::CONTENT_DISPOSITION],
            "attachment; filename=\"qed-stats.csv\""
        );
        let csv_body = to_bytes(csv_response.into_body(), 1_048_576).await.unwrap();
        let csv = String::from_utf8(csv_body.to_vec()).expect("CSV UTF-8");
        assert!(csv.starts_with("\"snapshot\",\"signed_stats_document_json\",\"{"));
        assert!(csv.contains("\"section\",\"metric\",\"value\""));
        assert!(csv.contains("publisher_catalog_method"));
        assert!(csv.contains("publisher_catalog_currently_flagged_last_7_days"));
        assert!(csv.contains("1234.56789"));
        assert!(csv.contains("\"' \t=IMPORTXML\""));
    }

    #[tokio::test]
    async fn public_stats_api_pages_cover_all_watch_history_and_reject_invalid_pages() {
        let state = test_state(false);
        let now = Utc::now().to_rfc3339();
        state.leaderboard.write().await.impostors.entries = (0..55)
            .map(|index| discovery::ImpostorEntry {
                chain: "base".to_owned(),
                chain_label: "Base".to_owned(),
                ticker: format!("T{index}"),
                publisher: "Backed xStocks".to_owned(),
                symbol: format!("T{index}x"),
                name: format!("Issuer token {index}"),
                address: format!("0x{index:040x}"),
                first_seen_at: now.clone(),
                last_seen_at: now.clone(),
                volume_24h_usd: Some(index as f64),
                source: discovery::MarketSource::Dexscreener,
                guard_url: format!("/guard/base/0x{index:040x}"),
                reason: "On-chain metadata matches a publisher product; deployment is absent."
                    .to_owned(),
                reads: Vec::new(),
                on_chain_symbol: Some(format!("T{index}x")),
                on_chain_name: Some(format!("Issuer token {index}")),
                publisher_catalog_snapshot_hash: Some("b".repeat(64)),
                evidence_truncated: false,
                guard_document: None,
            })
            .collect();
        let app = crate::adapters::web::router(state);
        let first = app
            .clone()
            .oneshot(Request::get("/api/stats?page=1").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        let first_body = to_bytes(first.into_body(), 1_048_576).await.unwrap();
        let first: serde_json::Value = serde_json::from_slice(&first_body).unwrap();
        assert_eq!(first["pages"], 2);
        assert_eq!(first["impostor_candidates_total"], 55);
        assert_eq!(first["impostor_candidates"].as_array().unwrap().len(), 50);
        assert_eq!(first["impostor_candidates"][0]["ticker"], "T0");
        let second = app
            .clone()
            .oneshot(Request::get("/api/stats?page=2").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(second.status(), StatusCode::OK);
        let second_body = to_bytes(second.into_body(), 1_048_576).await.unwrap();
        let second: serde_json::Value = serde_json::from_slice(&second_body).unwrap();
        assert_eq!(second["page"], 2);
        assert_eq!(second["impostor_candidates"].as_array().unwrap().len(), 5);
        assert_eq!(second["impostor_candidates"][0]["ticker"], "T50");
        assert_eq!(second["impostor_candidates"][4]["ticker"], "T54");
        let zero = app
            .clone()
            .oneshot(Request::get("/api/stats?page=0").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(zero.status(), StatusCode::BAD_REQUEST);
        let beyond = app
            .oneshot(Request::get("/api/stats?page=3").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(beyond.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn admin_stats_reports_instance_schema_and_counters() {
        let state = test_state(false);
        state.usage_stats.record_request(true, true, false, false, false, false);
        state.usage_stats.record_html_page_view();
        state.usage_stats.record_response(200);

        let response = admin_stats(State(state)).await;
        assert_eq!(response.headers().get(header::CACHE_CONTROL).unwrap(), "no-store");
        let body = to_bytes(response.into_body(), 4096).await.expect("stats body");
        let value: serde_json::Value = serde_json::from_slice(&body).expect("stats serialize");
        assert_eq!(value["scope"], "instance");
        assert!(value["started_at"].as_str().is_some_and(|value| !value.is_empty()));
        assert!(value["uptime_seconds"].is_u64());
        assert_eq!(value["total_requests"], 1);
        assert_eq!(value["html_page_views"], 1);
        assert_eq!(value["api_requests"], 1);
        assert_eq!(value["checks"], 1);
        assert_eq!(value["health_requests"], 0);
        assert_eq!(value["static_asset_requests"], 0);
        assert_eq!(value["responses"]["2xx"], 1);
    }
    #[tokio::test]
    async fn admin_route_rejects_without_auth_and_accepts_basic_auth() {
        let state = test_state(false);
        let app = crate::adapters::web::router(state.clone());

        let rejected = app
            .clone()
            .oneshot(Request::get("/admin/stats").body(Body::empty()).unwrap())
            .await
            .expect("unauthorized response");
        assert_eq!(rejected.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(rejected.headers().get(header::CACHE_CONTROL).unwrap(), "no-store");
        let rejected_body = to_bytes(rejected.into_body(), 1024).await.expect("body");
        assert_eq!(rejected_body.as_ref(), b"Unauthorized\n");
        let after_rejection = state.usage_stats.snapshot();
        assert_eq!(after_rejection.total_requests, 1);
        assert_eq!(after_rejection.admin_requests, 1);
        assert_eq!(after_rejection.responses.class_4xx, 1);

        let encoded = STANDARD.encode(b"test-admin:test-password");
        let accepted = app
            .oneshot(
                Request::get("/admin/stats")
                    .header("authorization", format!("Basic {encoded}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("authorized response");
        assert_eq!(accepted.status(), StatusCode::OK);
        assert_eq!(accepted.headers().get(header::CACHE_CONTROL).unwrap(), "no-store");
        let body = to_bytes(accepted.into_body(), 4096).await.expect("body");
        let value: serde_json::Value = serde_json::from_slice(&body).expect("stats JSON");
        assert_eq!(value["scope"], "instance");
        assert_eq!(value["admin_requests"], 2);
        assert_eq!(state.usage_stats.snapshot().responses.class_2xx, 1);
        assert_eq!(state.usage_stats.snapshot().html_page_views, 0);
        let mut unavailable_state = test_state(false);
        unavailable_state.admin_auth =
            std::sync::Arc::new(crate::adapters::state::AdminAuth::new(None, Some("ignored")));
        let unavailable = crate::adapters::web::router(unavailable_state)
            .oneshot(
                Request::get("/admin/stats")
                    .header("authorization", format!("Basic {encoded}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("fail-closed response");
        assert_eq!(unavailable.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(unavailable.headers().get(header::CACHE_CONTROL).unwrap(), "no-store");

        let public_status = crate::adapters::web::router(test_state(false))
            .oneshot(Request::get("/api/status").body(Body::empty()).unwrap())
            .await
            .expect("public status response");
        assert_eq!(public_status.status(), StatusCode::OK);
        let body = axum::body::to_bytes(public_status.into_body(), 1024 * 1024)
            .await
            .expect("status body");
        let status: serde_json::Value = serde_json::from_slice(&body).expect("status JSON");
        assert_eq!(status["version"], env!("CARGO_PKG_VERSION"));
    }
    #[tokio::test]
    async fn admin_auth_attempts_are_rate_limited_before_authentication() {
        let app = crate::adapters::web::router(test_state(false));
        for _ in 0..60 {
            let response = app
                .clone()
                .oneshot(Request::get("/admin/stats").body(Body::empty()).unwrap())
                .await
                .expect("auth response");
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }
        let response = app
            .oneshot(Request::get("/admin/stats").body(Body::empty()).unwrap())
            .await
            .expect("rate-limited response");
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[test]
    fn sorting_supports_metric_direction_and_nulls_last() {
        let mut entries = vec![
            entry("low", 1, Some(10.0), Some(3.0)),
            entry("high", 2, Some(30.0), Some(1.0)),
            entry("missing", 3, None, Some(2.0)),
        ];
        sort_entries(&mut entries, "volume", true);
        assert_eq!(
            entries.iter().map(|entry| entry.pool.as_str()).collect::<Vec<_>>(),
            vec!["high", "low", "missing"]
        );
        sort_entries(&mut entries, "price", false);
        assert_eq!(
            entries.iter().map(|entry| entry.pool.as_str()).collect::<Vec<_>>(),
            vec!["high", "missing", "low"]
        );
    }

    #[test]
    fn pagination_returns_the_requested_ranked_window() {
        let entries = (0..125)
            .map(|index| entry(&format!("pool-{index}"), index + 1, Some(index as f64), None))
            .collect();
        let page = page_entries(entries, 2, 50);
        assert_eq!(page.len(), 50);
        assert_eq!(page.first().map(|entry| entry.rank), Some(51));
        assert_eq!(page.last().map(|entry| entry.rank), Some(100));
    }
    #[tokio::test]
    async fn price_api_omits_unknown_pools_instead_of_zeroing_them() {
        let state = test_state(false);
        state.prices.write().await.prices.push(crate::adapters::discovery::PricePoint {
            chain: "solana".to_owned(),
            pool: "known".to_owned(),
            price_usd: Some(12.5),
            change_24h_pct: Some(1.5),
            volume_24h_usd: Some(100.0),
            liquidity_usd: Some(200.0),
            source: discovery::MarketSource::Dexscreener,
        });

        let body = api_prices(
            State(state),
            Query(PricesQuery { ids: Some("solana:known,solana:missing".to_owned()) }),
        )
        .await
        .expect("valid price query")
        .0;
        let prices = body["prices"].as_array().expect("price array");
        assert_eq!(prices.len(), 1);
        assert_eq!(prices[0]["pool"], "known");
        assert_eq!(prices[0]["price_usd"], 12.5);
    }
    #[tokio::test]
    async fn verify_handler_reports_all_validation_dimensions() {
        let state = test_state(false);
        let attestation = crate::app::attestation::signed_test_attestation([7; 32], false);
        let body = verify_attestation(
            State(state),
            Json(serde_json::to_value(attestation).expect("serialize attestation")),
        )
        .await
        .expect("attestation verifies")
        .0;
        assert_eq!(body["kind"], "attestation");
        assert_eq!(body["ok"], true);
        assert_eq!(body["cryptographic"], true);
        assert_eq!(body["trusted_signer"], true);
        assert_eq!(body["environment_match"], true);
        assert_eq!(body["fresh"], true);
    }

    #[tokio::test]
    async fn verify_accepts_signed_statements_and_guards_and_rejects_cross_kind_tampering() {
        let state = test_state(false);
        let statement = crate::domain::statement::signed_test_statement([7; 32], false);
        let body = verify_attestation(
            State(state.clone()),
            Json(serde_json::to_value(statement.clone()).unwrap()),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(body["kind"], "statement");
        assert_eq!(body["id"], statement.id);
        assert_eq!(body["cryptographic"], true);
        assert_eq!(body["trusted_signer"], true);
        assert_eq!(body["environment_match"], true);
        assert!(body["fresh"].is_null());
        assert_eq!(body["ok"], true);

        let mut cross_kind = statement.clone();
        cross_kind.kind = "attestation".to_owned();
        let error = verify_attestation(
            State(state.clone()),
            Json(serde_json::to_value(cross_kind).unwrap()),
        )
        .await
        .unwrap_err();
        assert_eq!(error.0, StatusCode::BAD_REQUEST);
        assert!(error.1.0["error"].is_string());

        let mut hybrid = serde_json::to_value(statement.clone()).unwrap();
        hybrid["pool"] = serde_json::json!({});
        let error = verify_attestation(State(state.clone()), Json(hybrid)).await.unwrap_err();
        assert_eq!(error.0, StatusCode::BAD_REQUEST);
        assert!(error.1.0["error"].is_string());
        let guard = crate::domain::guard::signed_test_guard([7; 32]);
        let body =
            verify_attestation(State(state.clone()), Json(serde_json::to_value(guard).unwrap()))
                .await
                .unwrap()
                .0;
        assert_eq!(body["kind"], "guard");
        assert_eq!(body["cryptographic"], true);
        assert_eq!(body["trusted_signer"], true);
        assert_eq!(body["environment_match"], true);
        assert!(body["fresh"].is_null());
        assert_eq!(body["ok"], true);

        let mut tampered = statement;
        tampered.observed_at.push_str(" altered");
        let body = verify_attestation(State(state), Json(serde_json::to_value(tampered).unwrap()))
            .await
            .unwrap()
            .0;
        assert_eq!(body["cryptographic"], false);
        assert_eq!(body["ok"], false);
    }

    #[tokio::test]
    async fn verify_rejects_unknown_guard_fields_at_every_depth() {
        let state = test_state(false);
        let guard = crate::domain::guard::signed_test_guard([7; 32]);
        let valid = serde_json::to_value(guard).expect("Guard JSON");
        for path in ["", "/identity"] {
            let mut value = valid.clone();
            let target =
                if path.is_empty() { &mut value } else { value.pointer_mut(path).unwrap() };
            target
                .as_object_mut()
                .unwrap()
                .insert("unexpected".to_owned(), serde_json::json!(true));
            let error = verify_attestation(State(state.clone()), Json(value)).await.unwrap_err();
            assert_eq!(error.0, StatusCode::BAD_REQUEST, "unknown field at {path}");
        }
    }
    #[tokio::test]
    async fn verify_rejects_unknown_attestation_and_statement_fields_recursively() {
        let state = test_state(false);

        let attestation = crate::app::attestation::signed_test_attestation([7; 32], false);
        let mut value = serde_json::to_value(attestation).expect("attestation JSON");
        value["pool"]["base"]["unexpected"] = serde_json::json!(true);
        let error = verify_attestation(State(state.clone()), Json(value)).await.unwrap_err();
        assert_eq!(error.0, StatusCode::BAD_REQUEST);
        let attestation = crate::app::attestation::signed_test_attestation([7; 32], false);
        let mut value = serde_json::to_value(attestation).expect("attestation JSON");
        value["verdict"] = serde_json::json!({
            "Mismatch": {
                "claimed": "NVDA",
                "actual": "NVDA",
                "unexpected": true
            }
        });
        let error = verify_attestation(State(state.clone()), Json(value)).await.unwrap_err();
        assert_eq!(error.0, StatusCode::BAD_REQUEST);
        let attestation = crate::app::attestation::signed_test_attestation([7; 32], false);
        let mut value = serde_json::to_value(attestation).expect("attestation JSON");
        value["registry_entry"] = serde_json::json!({
            "issuer": "Example Publisher",
            "ticker": "NVDA",
            "name": "NVIDIA",
            "chain": "Base",
            "contract": "0x0000000000000000000000000000000000000001",
            "decimals": 18,
            "source": "fixture",
            "source_url": "https://example.invalid/registry",
            "last_checked": "2026-10-01T00:00:00Z",
            "removed_at": null,
            "stale_since": null,
            "unexpected": true
        });
        let error = verify_attestation(State(state.clone()), Json(value)).await.unwrap_err();
        assert_eq!(error.0, StatusCode::BAD_REQUEST);

        let statement = crate::domain::statement::signed_test_statement([7; 32], false);
        let mut value = serde_json::to_value(statement).expect("statement JSON");
        value["assets"] = serde_json::json!([{
            "wallet": "0x0000000000000000000000000000000000000001",
            "chain": "Base",
            "contract": "0x0000000000000000000000000000000000000002",
            "ticker": "NVDA",
            "issuer": "Example Publisher",
            "issuer_match": true,
            "balance": "1",
            "decimals": 18,
            "powers_observed_at": null,
            "powers_block": null,
            "powers_slot": null,
            "slot": null,
            "powers_summary": {
                "can_seize": [],
                "can_block": [{ "code": "wallet_blocked", "detail": "Observed", "unexpected": true }],
                "can_change_rules": [],
                "unavailable": []
            }
        }]);
        let error = verify_attestation(State(state), Json(value)).await.unwrap_err();
        assert_eq!(error.0, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn verify_route_accepts_awkward_float_round_trips() {
        for quote_share in [1.1129609814871755e-8, 0.1 + 0.2] {
            let attestation = crate::app::attestation::signed_test_attestation_with_quote_share(
                [7; 32],
                false,
                quote_share,
            );
            let id = attestation.id.clone();
            let serialized = serde_json::to_vec(&attestation).expect("serialize attestation");
            let response = crate::adapters::web::router(test_state(false))
                .oneshot(
                    Request::post("/verify")
                        .header(axum::http::header::CONTENT_TYPE, "application/json")
                        .body(Body::from(serialized))
                        .expect("verify request"),
                )
                .await
                .expect("verify response");

            assert_eq!(response.status(), axum::http::StatusCode::OK);
            let body =
                to_bytes(response.into_body(), 64 * 1024).await.expect("verify response body");
            let value: serde_json::Value =
                serde_json::from_slice(&body).expect("verify response JSON");
            assert_eq!(value["id"], id);
            assert_eq!(value["cryptographic"], true, "quote share: {quote_share:?}");
            assert_eq!(value["trusted_signer"], true);
            assert_eq!(value["fresh"], true);
            assert_eq!(value["ok"], true);
        }
    }

    fn assert_negative_dimensions(
        body: &serde_json::Value,
        cryptographic: bool,
        trusted_signer: bool,
        environment_match: bool,
        fresh: bool,
    ) {
        assert_eq!(body["ok"], false);
        assert_eq!(body["cryptographic"], cryptographic);
        assert_eq!(body["trusted_signer"], trusted_signer);
        assert_eq!(body["environment_match"], environment_match);
        assert_eq!(body["fresh"], fresh);
    }

    #[tokio::test]
    async fn verify_handler_reports_each_negative_validation_dimension() {
        let mut bad_signature = crate::app::attestation::signed_test_attestation([7; 32], false);
        bad_signature.signature.push('x');
        let body = verify_attestation(
            State(test_state(false)),
            Json(serde_json::to_value(bad_signature).unwrap()),
        )
        .await
        .unwrap()
        .0;
        assert_negative_dimensions(&body, false, false, true, false);

        let untrusted = crate::app::attestation::signed_test_attestation([9; 32], false);
        let body = verify_attestation(
            State(test_state(false)),
            Json(serde_json::to_value(untrusted).unwrap()),
        )
        .await
        .unwrap()
        .0;
        assert_negative_dimensions(&body, true, false, true, true);

        let environment_mismatch = crate::app::attestation::signed_test_attestation([7; 32], true);
        let body = verify_attestation(
            State(test_state(false)),
            Json(serde_json::to_value(environment_mismatch).unwrap()),
        )
        .await
        .unwrap()
        .0;
        assert_negative_dimensions(&body, true, true, false, true);

        let expired = crate::app::attestation::expired_signed_test_attestation([7; 32], false);
        let body = verify_attestation(
            State(test_state(false)),
            Json(serde_json::to_value(expired).unwrap()),
        )
        .await
        .unwrap()
        .0;
        assert_negative_dimensions(&body, true, true, true, false);
    }

    #[tokio::test]
    async fn check_api_rejects_malformed_public_input_before_reader_access() {
        let result = api_check(State(test_state(false)), Path("not-an-address".to_owned())).await;
        assert!(matches!(result, Err(StatusCode::BAD_REQUEST)));
    }
    #[tokio::test]
    async fn statement_api_rejects_wallets_that_do_not_match_selected_chains() {
        let response = crate::adapters::web::router(test_state(false))
            .oneshot(
                Request::post("/api/statement")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "wallets": ["not-a-wallet"],
                            "chains": ["solana"]
                        })
                        .to_string(),
                    ))
                    .expect("statement request"),
            )
            .await
            .expect("statement response");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(response.into_body(), 4096).await.expect("statement error body");
        let value: serde_json::Value = serde_json::from_slice(&body).expect("statement error JSON");
        assert_eq!(value["error"], "Bad request");
        assert_eq!(
            value["detail"],
            "A wallet address does not match the selected chain. Check the address and chain selection."
        );
    }

    #[tokio::test]
    async fn uncached_statement_page_returns_not_found() {
        let response = crate::adapters::web::router(test_state(false))
            .oneshot(
                Request::get("/statements/not-cached")
                    .body(Body::empty())
                    .expect("statement page request"),
            )
            .await
            .expect("statement page response");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn cached_statement_json_route_returns_the_signed_payload() {
        let state = test_state(false);
        let statement = crate::domain::statement::signed_test_statement([7; 32], false);
        let id = statement.id.clone();
        state.app.statement_cache.insert(id.clone(), statement).await;
        let response = crate::adapters::web::router(state)
            .oneshot(
                Request::get(format!("/api/statement/{id}"))
                    .body(Body::empty())
                    .expect("statement JSON request"),
            )
            .await
            .expect("statement JSON response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["id"], id);
        assert_eq!(value["kind"], "statement");
    }

    #[tokio::test]
    async fn powers_api_routes_invalid_and_unregistered_contracts_without_chain_reads() {
        let app = crate::adapters::web::router(test_state(false));
        for (address, expected) in [
            ("not-an-address", StatusCode::BAD_REQUEST),
            ("0x0000000000000000000000000000000000000001", StatusCode::NOT_FOUND),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::get(format!("/api/powers/{address}"))
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("powers API response");
            assert_eq!(response.status(), expected);
        }
    }
    #[tokio::test]
    async fn mcp_legacy_tools_are_counted_from_the_bounded_json_body() {
        let state = test_state(false);
        let app = crate::adapters::web::router(state.clone());
        for name in ["qed_check", "qed_wallet"] {
            let body = serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": { "name": name, "arguments": { "address": "not-an-address" } }
            })
            .to_string();
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/mcp")
                        .header(header::CONTENT_TYPE, "application/json")
                        .header(header::CONTENT_LENGTH, body.len().to_string())
                        .body(Body::from(body))
                        .expect("MCP request"),
                )
                .await
                .expect("MCP response");
            assert_eq!(response.status(), StatusCode::OK);
            let body = to_bytes(response.into_body(), 64 * 1024).await.expect("MCP tool body");
            let value: serde_json::Value =
                serde_json::from_slice(&body).expect("JSON-RPC response");
            assert_eq!(value["result"]["isError"], true);
        }
        let metrics = state.usage_stats.snapshot();
        assert_eq!(metrics.checks, 1);
        assert_eq!(metrics.wallet_requests, 1);
    }

    #[tokio::test]
    async fn registry_api_reuses_etag_for_unchanged_snapshot() {
        let state = test_state(false);
        let first = api_registry(State(state.clone()), HeaderMap::new()).await;
        assert_eq!(first.status(), StatusCode::OK);
        let etag = first.headers().get(header::ETAG).cloned().expect("registry response has ETag");

        let mut headers = HeaderMap::new();
        headers.insert(header::IF_NONE_MATCH, etag);
        let second = api_registry(State(state), headers).await;
        assert_eq!(second.status(), StatusCode::NOT_MODIFIED);
    }
}
