use super::*;
use crate::{
    adapters::{
        discovery::{FeaturedPool, LeaderboardEntry, MarketSource},
        state::RateLimiter,
    },
    domain::{
        pool::{PoolError, PoolInfo, TokenMeta, TokenSide},
        registry::Entry,
    },
    ports::{ChainReader, Clock, RpcRequestContext, with_rpc_request_context},
};
use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tower::ServiceExt;
const POOL: &str = "0x0000000000000000000000000000000000000011";
const TOKEN: &str = "0x0000000000000000000000000000000000000022";
const USD: &str = "0x0000000000000000000000000000000000000033";
struct Reader {
    chain: Chain,
    failing: Arc<AtomicBool>,
    calls: Arc<AtomicUsize>,
}
#[async_trait::async_trait]
impl ChainReader for Reader {
    fn chain(&self) -> Chain {
        self.chain
    }
    async fn read_pool(&self, _: &str) -> Result<PoolInfo, PoolError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.failing.load(Ordering::SeqCst) {
            return Err(PoolError::Reader("synthetic outage".into()));
        }
        Ok(PoolInfo {
            chain: self.chain,
            pool: POOL.into(),
            dex: "uniswap-v2".into(),
            base: side(USD, "USDC"),
            quote: side(TOKEN, "NVDA"),
        })
    }
    async fn code_at(&self, _: &str) -> Result<Vec<u8>, PoolError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.failing.load(Ordering::SeqCst) {
            Err(PoolError::Reader("synthetic probe outage".into()))
        } else {
            Ok(vec![1])
        }
    }
    async fn token_meta(&self, address: &str) -> Result<TokenMeta, PoolError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(TokenMeta {
            address: address.into(),
            symbol: Some(if address == TOKEN { "NVDA" } else { "USDC" }.into()),
            name: Some(if address == TOKEN { "NVIDIA" } else { "USD Coin" }.into()),
            decimals: Some(6),
            total_supply: Some("1000000".into()),
        })
    }
    async fn power_facts(&self, _: &str) -> Result<crate::domain::powers::PowerFacts, PoolError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Default::default())
    }
}
fn side(address: &str, symbol: &str) -> TokenSide {
    TokenSide {
        address: address.into(),
        symbol: Some(symbol.into()),
        decimals: Some(6),
        balance: Some("100".into()),
    }
}
fn entry() -> Entry {
    Entry {
        issuer: "Synthetic issuer".into(),
        ticker: "NVDA".into(),
        name: "NVIDIA".into(),
        chain: Chain::Base,
        contract: TOKEN.into(),
        decimals: Some(6),
        source: "test".into(),
        source_url: "https://issuer.example".into(),
        last_checked: Utc::now().to_rfc3339(),
        removed_at: None,
        stale_since: None,
        official_deployments: Vec::new(),
    }
}
fn state() -> (AppState, Arc<AtomicBool>, Arc<AtomicUsize>) {
    let failing = Arc::new(AtomicBool::new(false));
    let calls = Arc::new(AtomicUsize::new(0));
    (
        AppState::for_tests(
            vec![entry()],
            vec![Box::new(Reader {
                chain: Chain::Base,
                failing: failing.clone(),
                calls: calls.clone(),
            })],
            false,
        ),
        failing,
        calls,
    )
}
async fn body(state: &AppState, path: &str) -> String {
    let response = super::super::router(state.clone())
        .oneshot(Request::get(path).header("user-agent", "Googlebot").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "{path}");
    String::from_utf8(to_bytes(response.into_body(), usize::MAX).await.unwrap().to_vec()).unwrap()
}
async fn seed_listings(state: &AppState) {
    state.leaderboard.write().await.entries.push(LeaderboardEntry {
        rank: 1,
        chain: "base".into(),
        chain_label: "Base".into(),
        dex: "uniswap-v2".into(),
        pool: POOL.into(),
        base_symbol: "USDC".into(),
        quote_symbol: "NVDA".into(),
        source: MarketSource::Dexscreener,
        issuer: Some("Synthetic issuer".into()),
        ticker: Some("NVDA".into()),
        issuer_on_base: Some(false),
        verdict: "verified".into(),
        read_status: "checked".into(),
        read_reason: None,
        price_usd: None,
        change_24h_pct: None,
        volume_24h_usd: None,
        liquidity_usd: None,
        txns_24h: None,
        detail_url: format!("/assets/base/pool/{POOL}"),
        trade_url: String::new(),
        explorer_url: String::new(),
        attestation_id: None,
        checked_at: Some(Utc::now().to_rfc3339()),
    });
    state.featured.write().await.push(FeaturedPool {
        chain: Chain::Base,
        dex: "uniswap-v2".into(),
        pool: POOL.into(),
        base_symbol: "USDC".into(),
        base_address: USD.into(),
        quote_symbol: "NVDA".into(),
        quote_address: TOKEN.into(),
        issuer: Some("Synthetic issuer".into()),
        ticker: Some("NVDA".into()),
        verdict: "verified".into(),
        quote_balance: None,
        quote_share_of_supply: None,
        volume_24h_usd: None,
        liquidity_usd: None,
        curated: false,
        note: None,
        updated_at: Utc::now().to_rfc3339(),
    });
}
#[tokio::test]
async fn executed_three_misses_and_recovery_agree_on_all_public_surfaces() {
    let (mut state, failing, _) = state();
    seed_listings(&state).await;
    let key = AssetKey::new(Chain::Base, SubjectKind::Pool, POOL).unwrap();
    let start = Utc::now() - Duration::hours(4);
    for step in 0..=4 {
        Arc::make_mut(&mut state.app).clock =
            Arc::new(FixedClock(start + Duration::hours(i64::from(step))));
        failing.store((1..=3).contains(&step), Ordering::SeqCst);
        state.app.check_cache.invalidate(&format!("Base:{POOL}")).await;
        let (result, _) = with_rpc_request_context(
            RpcRequestContext::new(false, false),
            crate::app::check::check_on_chain(&state.app, Chain::Base, POOL),
        )
        .await;
        assert_eq!(result.read_issue.is_some(), (1..=3).contains(&step));
        let expected = if step == 3 { "Unverified" } else { "Verified" };
        let projection = state.app.history.state(&key, Facet::ContractMatch).unwrap();
        assert_eq!(projection.label(), expected);
        assert_eq!(projection.consecutive_misses, if step == 4 { 0 } else { step });
        let api: serde_json::Value =
            serde_json::from_str(&body(&state, "/api/leaderboard").await).unwrap();
        assert_eq!(api["entries"][0]["display_label"], expected, "leaderboard.js input");
        let ssr = body(&state, "/").await;
        assert!(ssr.contains(&format!(">{expected}</span>")), "SSR leaderboard status");
        let featured = body(&state, "/pools/featured").await;
        assert!(featured.contains(expected), "featured projection");
        let asset = body(&state, &key.path()).await;
        let badge =
            if (1..3).contains(&step) { format!("Retrying {step}/3") } else { expected.into() };
        assert!(
            asset.contains(&format!("class=\"guard-badge\">{badge}</span>")),
            "exact asset badge"
        );
        let ticker = body(&state, "/tokens/NVDA").await;
        assert!(
            ticker.contains(&format!("data-label=\"Status\">{badge}</td>")),
            "ticker row status"
        );
    }
}
struct FixedClock(DateTime<Utc>);
impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        self.0
    }
}
#[tokio::test]
async fn scheduled_budget_blocked_slots_reach_unverified_once_per_hour_without_provider_calls() {
    let (mut state, _, calls) = state();
    let start = Utc::now() - Duration::hours(4);
    let key = AssetKey::new(Chain::Base, SubjectKind::Pool, POOL).unwrap();
    for (facet, outcome) in
        [(Facet::ContractMatch, Outcome::Verified), (Facet::Guard, Outcome::Allow)]
    {
        let event = crate::app::asset_history::event(
            &state.app,
            key.clone(),
            facet,
            outcome,
            start,
            serde_json::json!({}),
        );
        state.app.history.append(event).await.unwrap();
    }
    for hour in 1..=3 {
        Arc::make_mut(&mut state.app).clock = Arc::new(FixedClock(start + Duration::hours(hour)));
        for _ in 0..3 {
            for facet in [Facet::ContractMatch, Facet::Guard] {
                with_rpc_request_context(
                    RpcRequestContext::new(false, false),
                    crate::app::asset_history::retry(&state.app, key.clone(), facet, true),
                )
                .await;
            }
        }
        for facet in [Facet::ContractMatch, Facet::Guard] {
            let projection = state.app.history.state(&key, facet).unwrap();
            assert_eq!(projection.consecutive_misses, hour as u32);
            assert_eq!(
                projection.label(),
                if hour == 3 {
                    "Unverified"
                } else if facet == Facet::Guard {
                    "Allow"
                } else {
                    "Verified"
                }
            );
        }
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let events = state
        .app
        .history
        .history(std::slice::from_ref(&key), DateTime::<Utc>::MIN_UTC, None, 20)
        .await
        .unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.outcome == Outcome::Miss
                && event.miss_reason.as_deref() == Some("budget_rejection"))
            .count(),
        6
    );
}
#[tokio::test]
async fn failed_unknown_chain_probes_never_create_phantom_assets() {
    let failing = Arc::new(AtomicBool::new(true));
    let calls = Arc::new(AtomicUsize::new(0));
    let state = AppState::for_tests(
        Vec::new(),
        [Chain::Base, Chain::Ethereum]
            .into_iter()
            .map(|chain| {
                Box::new(Reader { chain, failing: failing.clone(), calls: calls.clone() })
                    as Box<dyn ChainReader>
            })
            .collect(),
        false,
    );
    crate::app::check::check(&state.app, POOL).await;
    let states = state.app.history.states();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(states.is_empty());
    for chain in [Chain::Base, Chain::Ethereum] {
        let key = AssetKey::new(chain, SubjectKind::Pool, POOL).unwrap();
        assert_eq!(state.app.history.inventory(&key).await.unwrap().1, 0);
    }
}
#[tokio::test]
async fn wallet_guard_never_enters_public_asset_history() {
    let (state, _, calls) = state();
    let wallet = "0x0000000000000000000000000000000000000044";
    let result = crate::app::guard::create(&state.app, POOL, Chain::Base, Some(wallet)).await;
    assert!(result.is_ok(), "wallet Guard executes a real signed review");
    assert!(calls.load(Ordering::SeqCst) > 0);
    // Independently executed wallet-free controls may be journalled, never the wallet review.
    assert!(state.app.history.states().iter().all(|(_, facet, _)| *facet == Facet::Powers));
    let controls = body(&state, &format!("/assets/base/token/{TOKEN}/history.json")).await;
    assert!(!controls.contains(wallet));
    assert!(!controls.contains("\"facet\":\"guard\""));
    let json: serde_json::Value = serde_json::from_str(
        &body(&state, &format!("/assets/base/pool/{POOL}/history.json")).await,
    )
    .unwrap();
    assert_eq!(json["observations"], serde_json::json!([]));
}
#[tokio::test]
async fn timeline_preserves_records_without_same_time_vertical_stacks() {
    let (state, _, _) = state();
    let key = AssetKey::new(Chain::Base, SubjectKind::Pool, POOL).unwrap();
    let time = state.app.clock.now() - Duration::hours(2);
    for facet in [Facet::ContractMatch, Facet::Guard] {
        for index in 0..4 {
            let mut event = crate::app::asset_history::event(
                &state.app,
                key.clone(),
                facet,
                Outcome::Miss,
                time,
                serde_json::json!({}),
            );
            event.event_id = format!("{facet:?}-{index}");
            event.miss_reason = Some("timeout".into());
            state.app.history.append(event).await.unwrap();
        }
    }
    let view = prepare(&state.app, vec![key.clone()], key.path(), &HistoryQuery::default()).await;
    assert_eq!(view.days, 1);
    assert_eq!(view.rows.len(), 8);
    assert_eq!(view.points.iter().map(|point| point.count).collect::<Vec<_>>(), [4, 4]);
    assert_eq!(
        view.points.iter().map(|point| point.y).collect::<Vec<_>>(),
        view.lanes.iter().map(|lane| lane.y).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn asset_views_are_cache_only_for_people_and_crawlers_and_share_read_limits() {
    let (mut state, _, calls) = state();
    state.rate_limiter = Arc::new(RateLimiter::with_rpc_limits(4, 8, 1000, 1000, 1));
    let app = super::super::router(state.clone());
    for (index, user_agent) in ["Mozilla/5.0", "Googlebot", "Mozilla/5.0", "Googlebot", "Googlebot"]
        .into_iter()
        .enumerate()
    {
        let address = format!("0x{:040x}", index + 100);
        let response = app
            .clone()
            .oneshot(
                Request::get(format!("/assets/base/token/{address}"))
                    .header("user-agent", user_agent)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(state.app.history.states().is_empty());
    crate::app::powers::inspect(&state.app, Chain::Base, TOKEN).await.unwrap();
    let token = AssetKey::new(Chain::Base, SubjectKind::Token, TOKEN).unwrap();
    let before = state.app.history.inventory(&token).await.unwrap();
    calls.store(0, Ordering::SeqCst);
    for user_agent in ["Mozilla/5.0", "Googlebot"] {
        let response = app
            .clone()
            .oneshot(
                Request::get(token.path())
                    .header("user-agent", user_agent)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "cached controls rendering never refreshes the chain"
    );
    assert_eq!(state.app.history.inventory(&token).await.unwrap(), before);
    let ip = "0.0.0.0".parse().unwrap();
    while state.rate_limiter.allow(ip) {}
    for suffix in ["", "/history.json", "/history.csv", "/observations/missing"] {
        let response = app
            .clone()
            .oneshot(
                Request::get(format!("/assets/base/token/{TOKEN}{suffix}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

struct NoContractReader;
#[async_trait::async_trait]
impl ChainReader for NoContractReader {
    fn chain(&self) -> Chain {
        Chain::Base
    }
    async fn read_pool(&self, _: &str) -> Result<PoolInfo, PoolError> {
        Err(PoolError::Unknown("not a pool".into()))
    }
    async fn code_at(&self, _: &str) -> Result<Vec<u8>, PoolError> {
        Ok(Vec::new())
    }
}
#[tokio::test]
async fn five_unknown_eoa_guards_are_journal_only_but_allow_deny_track() {
    let mut state = AppState::for_tests(Vec::new(), vec![Box::new(NoContractReader)], false);
    let directory = tempfile::tempdir().unwrap();
    let mut database = None;
    if let Ok(url) = std::env::var("QED_TEST_DATABASE_URL") {
        let (client, connection) =
            tokio_postgres::connect(&url, tokio_postgres::NoTls).await.unwrap();
        tokio::spawn(async move {
            let _ = connection.await;
        });
        let schema = format!("qed_guard_history_{:x}", rand::random::<u64>());
        client.batch_execute(&format!("CREATE SCHEMA {schema}")).await.unwrap();
        let mut scoped = reqwest::Url::parse(&url).unwrap();
        scoped.query_pairs_mut().append_pair("options", &format!("-csearch_path={schema}"));
        Arc::make_mut(&mut state.app).history = crate::adapters::history::HistoryStore::open(
            Some(scoped.as_str()),
            directory.path(),
            None,
        )
        .await
        .unwrap();
        database = Some((client, schema));
    }
    let app = super::super::router(state.clone());
    let mut assets = Vec::new();
    for index in 0..5 {
        let address = format!("0x{:040x}", 0xfeed00 + index);
        let response = app
            .clone()
            .oneshot(
                Request::get(format!("/api/guard/{address}?chain=base"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let document: crate::domain::guard::GuardDocument =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(document.verdict, crate::domain::guard::GuardVerdict::Unknown);
        assert!(document.reasons.iter().any(|reason| reason.code == "no_contract_at_address"));
        crate::domain::guard::verify(&document).unwrap();
        assets.push(AssetKey::new(Chain::Base, SubjectKind::Token, &address).unwrap());
    }
    state.app.history.flush().await.unwrap();
    assert!(
        state.app.history.states().is_empty(),
        "untracked EOA answers cannot create public state"
    );
    let journal =
        state.app.history.history(&assets, DateTime::<Utc>::MIN_UTC, None, 10).await.unwrap();
    assert_eq!(journal.len(), 5);
    assert!(
        journal
            .iter()
            .all(|event| event.facet == Facet::Guard && event.outcome == Outcome::Unknown)
    );
    if let Some((client, schema)) = &database {
        let row=client.query_one(&format!("SELECT (SELECT count(*) FROM {schema}.asset_state),(SELECT count(*) FROM {schema}.observations)"),&[]).await.unwrap();
        assert_eq!(row.get::<_, i64>(0), 0);
        assert_eq!(row.get::<_, i64>(1), 5);
    }
    for outcome in [Outcome::Allow, Outcome::Deny] {
        let event = crate::app::asset_history::event(
            &state.app,
            assets[0].clone(),
            Facet::Guard,
            outcome,
            state.app.clock.now(),
            serde_json::json!({}),
        );
        state.app.history.append(event).await.unwrap();
        state.app.history.flush().await.unwrap();
        assert_eq!(
            state
                .app
                .history
                .state(&assets[0], Facet::Guard)
                .unwrap()
                .last_confirmed
                .as_ref()
                .unwrap()
                .outcome,
            outcome
        );
    }
    if let Some((client, schema)) = database {
        client.batch_execute(&format!("DROP SCHEMA {schema} CASCADE")).await.unwrap();
    }
}

struct BudgetRejectedReader;
#[async_trait::async_trait]
impl ChainReader for BudgetRejectedReader {
    fn chain(&self) -> Chain {
        Chain::Base
    }
    async fn code_at(&self, _: &str) -> Result<Vec<u8>, PoolError> {
        Ok(vec![1])
    }
    async fn read_pool(&self, _: &str) -> Result<PoolInfo, PoolError> {
        crate::ports::current_rpc_request_context()
            .expect("admitted request")
            .mark(crate::ports::RPC_BUDGET_REJECTED);
        Err(PoolError::DailyBudgetExceeded)
    }
}
#[tokio::test]
async fn three_budget_503s_keep_verified_and_journal_rejections_not_misses() {
    let state = AppState::for_tests(vec![entry()], vec![Box::new(BudgetRejectedReader)], false);
    seed_listings(&state).await;
    let key = AssetKey::new(Chain::Base, SubjectKind::Pool, POOL).unwrap();
    let verified = crate::app::asset_history::event(
        &state.app,
        key.clone(),
        Facet::ContractMatch,
        Outcome::Verified,
        state.app.clock.now() - Duration::minutes(1),
        serde_json::json!({}),
    );
    state.app.history.append(verified).await.unwrap();
    let app = super::super::router(state.clone());
    for _ in 0..3 {
        let response = app
            .clone()
            .oneshot(
                Request::get(format!("/api/check/{POOL}?chain=base")).body(Body::empty()).unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
    let projection = state.app.history.state(&key, Facet::ContractMatch).unwrap();
    assert_eq!(projection.label(), "Verified");
    assert_eq!(projection.consecutive_misses, 0);
    assert!(projection.next_retry_at.is_none());
    let events = state
        .app
        .history
        .history(std::slice::from_ref(&key), DateTime::<Utc>::MIN_UTC, None, 10)
        .await
        .unwrap();
    assert_eq!(events.iter().filter(|event| event.outcome == Outcome::Rejected).count(), 3);
    assert!(!events.iter().any(|event| event.outcome == Outcome::Miss));
    let board: serde_json::Value =
        serde_json::from_str(&body(&state, "/api/leaderboard").await).unwrap();
    assert_eq!(board["entries"][0]["display_label"], "Verified");
}

#[tokio::test]
async fn history_exports_cap_large_requested_pages_at_200_with_keyset_next() {
    let (state, _, _) = state();
    let key = AssetKey::new(Chain::Base, SubjectKind::Token, TOKEN).unwrap();
    for index in 0..250 {
        let mut event = crate::app::asset_history::event(
            &state.app,
            key.clone(),
            Facet::Powers,
            Outcome::ControlsComplete,
            state.app.clock.now() - Duration::seconds(index),
            serde_json::json!({}),
        );
        event.event_id = format!("export-{index:04}");
        state.app.history.append(event).await.unwrap();
    }
    let response = super::super::router(state)
        .oneshot(
            Request::get(format!("{}/history.json?limit=999", key.path()))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().contains_key(header::LINK));
    let json: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    let rows = json["observations"].as_array().unwrap();
    assert_eq!(rows.len(), 200);
    assert_eq!(rows[0]["event_id"], "export-0000");
    assert_eq!(rows[199]["event_id"], "export-0199");
    assert!(json["next"].as_str().unwrap().contains("before_id=export-0199"));
}
