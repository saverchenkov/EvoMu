//! Integration test suite (Part 1) for server-pushed client notifications (`notify:*`) across
//! Hacking, Time Dilation, System-Wide Interference, Planetary Orbitals,
//! Scanning & Moon Survey, AIR Operations, Surveys, Market, and ESI UI Windows.

use crate::common;

use eve_db::{
    ContractRecord, InventoryFlag, InventoryItemRecord, IskCents, SystemDynamicState,
};
use eve_destiny::{Ball, DVec3, MovementMode, TargetLock};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::esi::{EsiHttpRequest, EsiRouter};
use eve_server::router::ServerContext;
use std::collections::HashMap;
use std::sync::Arc;

const P1: i64 = 90_000_001;
const JITA: u32 = 30_000_142;
const JITA_44: i64 = 60_003_760;

fn new_ctx() -> Arc<ServerContext> {
    let sde = Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("open sde"),
    );
    let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = Arc::new(ServerContext::with_sde_and_repository(sde, db));
    ctx.set_clock_paused(true);
    ctx
}

async fn login_pilot_char(
    ctx: &ServerContext,
    sid: u64,
    user: &str,
    pass: &str,
    char_id: i64,
) -> SessionState {
    let mut s = SessionState::new(sid);
    common::login_pilot(ctx, &mut s, user, pass).await;
    ctx.dispatch_rpc(
        &mut s,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");
    let _ = s.drain_notifications();
    let _ = ctx.drain_notifications_for_char(char_id);
    s
}

async fn call(
    ctx: &ServerContext,
    s: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> PyRep {
    ctx.dispatch_rpc(s, svc, None, method, args, &HashMap::new(), None)
        .await
        .unwrap_or_else(|e| panic!("{svc}.{method} failed: {} {}", e.code, e.message))
}

fn collect_notifs(
    ctx: &ServerContext,
    s: &mut SessionState,
    char_id: i64,
    sink: &mut Vec<(String, Vec<PyRep>)>,
) {
    for (ev, args) in s.drain_notifications() {
        sink.push((ev, args));
    }
    for n in ctx.drain_notifications_for_char(char_id) {
        sink.push((n.event, n.args));
    }
}

fn take_notif(
    ctx: &ServerContext,
    s: &mut SessionState,
    char_id: i64,
    sink: &mut Vec<(String, Vec<PyRep>)>,
    event: &str,
) -> Vec<PyRep> {
    collect_notifs(ctx, s, char_id, sink);
    let idx = sink.iter().position(|(e, _)| e == event).unwrap_or_else(|| {
        let names: Vec<&str> = sink.iter().map(|(e, _)| e.as_str()).collect();
        panic!("Expected notification {event} in collected stream, got: {names:?}")
    });
    sink.remove(idx).1
}

#[tokio::test]
async fn test_notify_sweep_sov_tidi_interference_and_planet() {
    // parity: notify:OnHackingStart, notify:OnHackingVirusChanged, notify:OnHackingTurnComplete
    eve_testkit::attest(&[
        "notify:OnHackingStart",
        "notify:OnHackingVirusChanged",
        "notify:OnHackingTurnComplete",
    ]);

    let ctx = new_ctx();
    let mut sess = login_pilot_char(&ctx, 9103, "demo", "demo-secret", P1).await;
    sess.role = u64::MAX;
    let null_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status <= 0.0 && s.solar_system_id < 31_000_000)
        .expect("nullsec system")
        .solar_system_id;
    let ship_id = sess.ship_id.expect("ship_id");
    let container_id = 880_002_i64;
    sess.solar_system_id = Some(null_sys);
    sess.station_id = None;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&P1) {
            ch.solar_system_id = null_sys;
            ch.station_id = None;
        }
        snap.system_states.entry(null_sys).or_insert(SystemDynamicState {
            solar_system_id: null_sys,
            ..Default::default()
        });
        snap.items.insert(
            container_id,
            InventoryItemRecord {
                item_id: container_id,
                type_id: 21831,
                owner_id: 90_000_099,
                location_id: null_sys as i64,
                flag: InventoryFlag::Locked,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            910_421,
            InventoryItemRecord {
                item_id: 910_421,
                type_id: 22175,
                owner_id: P1,
                location_id: ship_id,
                flag: InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    ctx.invalidate_ship_fitting(ship_id);

    let sys = ctx.systems.get_or_spawn(null_sys);
    for (bid, tid, x) in [(ship_id, 626_u32, 0.0), (container_id, 21831, 500.0)] {
        let _ = sys
            .spawn_ball(Ball {
                id: bid,
                type_id: tid,
                position: DVec3::new(x, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                mass: 1_000_000.0,
                inertia: 2.5,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
    }
    let _ = sys
        .add_target_lock(
            ship_id,
            TargetLock {
                target_id: container_id,
                lock_time_ms: 1000.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await;

    let mut seen = Vec::new();

    call(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(910_421), PyRep::None, PyRep::Integer(container_id)]).await;
    let hack_start = take_notif(&ctx, &mut sess, P1, &mut seen, "OnHackingStart");
    assert_eq!(hack_start, vec![PyRep::Integer(container_id), PyRep::Integer(910_421)]);
    let hack_virus = take_notif(&ctx, &mut sess, P1, &mut seen, "OnHackingVirusChanged");
    assert_eq!(hack_virus.first(), Some(&PyRep::Integer(container_id)));

    let hacking_can_id = 1_870_000_000 + (null_sys as i64 % 100_000) * 100;
    call(&ctx, &mut sess, "hackingMgr", "StartNewGameInstance", &[PyRep::Integer(hacking_can_id)]).await;
    call(&ctx, &mut sess, "hackingMgr", "ClickedOnTile", &[PyRep::Integer(hacking_can_id), PyRep::Integer(1)]).await;
    let turn_args = take_notif(&ctx, &mut sess, P1, &mut seen, "OnHackingTurnComplete");
    assert_eq!(turn_args.len(), 4);
}


#[tokio::test]
async fn test_notify_sweep_operations_survey_and_market() {
    // parity: notify:OnOperationCategoryChanged, notify:OnOperationTaskCompleted, notify:OnOperationCategoryCompleted, notify:OnSurveyRewardsAvailable, notify:OnShowMarketDetails
    // parity: notify:OnShowContract, notify:OnShowOwnerDetailsWindow, notify:OnCreateNewMailWindow, notify:OnAutopilotUpdated
    eve_testkit::attest(&[
        "notify:OnOperationCategoryChanged",
        "notify:OnOperationTaskCompleted",
        "notify:OnOperationCategoryCompleted",
        "notify:OnSurveyRewardsAvailable",
        "notify:OnShowMarketDetails",
        "notify:OnShowContract",
        "notify:OnShowOwnerDetailsWindow",
        "notify:OnCreateNewMailWindow",
        "notify:OnAutopilotUpdated",
    ]);

    let ctx = new_ctx();
    let mut sess = login_pilot_char(&ctx, 9105, "demo", "demo-secret", P1).await;
    let mut seen = Vec::new();

    call(&ctx, &mut sess, "operationsManager", "activate_operation_in_solar_system", &[PyRep::Integer(1)]).await;
    assert_eq!(take_notif(&ctx, &mut sess, P1, &mut seen, "OnOperationCategoryChanged").first(), Some(&PyRep::Integer(1)));
    call(&ctx, &mut sess, "operationsManager", "skip_task", &[PyRep::Integer(10)]).await;
    let task_notif = take_notif(&ctx, &mut sess, P1, &mut seen, "OnOperationTaskCompleted");
    assert_eq!(task_notif.get(2), Some(&PyRep::Integer(10)));
    call(&ctx, &mut sess, "operationsManager", "complete_category_for_character", &[PyRep::Integer(1)]).await;
    assert_eq!(take_notif(&ctx, &mut sess, P1, &mut seen, "OnOperationCategoryCompleted").first(), Some(&PyRep::Integer(1)));

    call(&ctx, &mut sess, "survey", "PerformSurveyChecks", &[]).await;
    call(&ctx, &mut sess, "survey", "ClaimSurveyRewards", &[PyRep::Integer(1)]).await;
    assert_eq!(take_notif(&ctx, &mut sess, P1, &mut seen, "OnSurveyRewardsAvailable").len(), 1);

    call(&ctx, &mut sess, "marketProxy", "StartupCheck", &[]).await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.contracts.insert(
            5001,
            ContractRecord {
                contract_id: 5001, issuer_id: P1, issuer_corp_id: 98_000_001, assignee_id: 0,
                acceptor_id: None, start_station_id: JITA_44, end_station_id: JITA_44,
                contract_type: "item_exchange".into(), status: "outstanding".into(),
                title: "Test Contract".into(), for_corp: false, availability: "public".into(),
                price: IskCents::from_isk(1_000), reward: IskCents(0), collateral: IskCents(0),
                buyout: IskCents(0), item_ids: Vec::new(), requested_items: Vec::new(),
                date_issued_unix: ctx.now_unix(), expiration_unix: ctx.now_unix() + 86_400,
                current_bid: IskCents(0), current_bidder_id: None, bid_history: Vec::new(), num_days: 1,
            },
        );
    });
    let router = EsiRouter::new(Arc::clone(&ctx));
    let now = ctx.now_unix();
    let scopes = vec!["esi-ui.open_window.v1".into(), "esi-ui.write_waypoint.v1".into()];
    let code = ctx.db.authorize_pkce("demo", "demo-secret", P1, "evomu-notify-sweep", "https://localhost/callback", scopes, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM", "S256", now).expect("authorize_pkce");
    let token = ctx.db.exchange_pkce_code(&code, "evomu-notify-sweep", "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk", now).expect("exchange_pkce_code").access_token;

    assert_eq!(router.handle_request(&EsiHttpRequest::new("POST", "/v1/ui/openwindow/contract/?contract_id=5001").with_bearer_token(&token)).status_code, 204);
    assert_eq!(take_notif(&ctx, &mut sess, P1, &mut seen, "OnShowContract"), vec![PyRep::Integer(5001)]);
    assert_eq!(router.handle_request(&EsiHttpRequest::new("POST", "/v1/ui/openwindow/marketdetails/?type_id=34").with_bearer_token(&token)).status_code, 204);
    assert_eq!(take_notif(&ctx, &mut sess, P1, &mut seen, "OnShowMarketDetails"), vec![PyRep::Integer(34)]);
    assert_eq!(router.handle_request(&EsiHttpRequest::new("POST", &format!("/v1/ui/openwindow/information/?target_id={P1}")).with_bearer_token(&token)).status_code, 204);
    assert_eq!(take_notif(&ctx, &mut sess, P1, &mut seen, "OnShowOwnerDetailsWindow"), vec![PyRep::Integer(P1)]);
    let mail_body = br#"{"subject":"Hi","body":"Hello","recipients":[90000002]}"#.to_vec();
    assert_eq!(router.handle_request(&EsiHttpRequest::new("POST", "/v1/ui/openwindow/newmail/").with_bearer_token(&token).with_body(mail_body)).status_code, 204);
    assert_eq!(take_notif(&ctx, &mut sess, P1, &mut seen, "OnCreateNewMailWindow").len(), 5);
    assert_eq!(router.handle_request(&EsiHttpRequest::new("POST", "/v2/ui/autopilot/waypoint/?destination_id=30000142&add_to_beginning=false&clear_other_waypoints=true").with_bearer_token(&token)).status_code, 204);
    assert_eq!(take_notif(&ctx, &mut sess, P1, &mut seen, "OnAutopilotUpdated"), Vec::<PyRep>::new());
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&P1].autopilot_waypoints.clone()), vec![JITA as i64]);
}
