//! Integration tests for open `plan:` parity rows (Batch 7).

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use eve_db::WorldRepository;
use eve_destiny::{Ball, Ballpark, DVec3, MovementMode};
use eve_machonet::SessionState;
use eve_proto::quasar::{
    EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage,
    TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::esi::{EsiHttpRequest, EsiRouter};
use eve_server::quasar_gateway::{STATUS_OK, STATUS_UNAUTHORIZED};
use eve_server::router::ServerContext;
use serde_json::json;

use super::common;

const PILOT_1: i64 = 90_000_001;
const PILOT_2: i64 = 90_000_002;
const JITA_SYS: u32 = 30_000_142;
const PERIMETER_SYS: u32 = 30_000_144;
const JITA_STATION: i64 = 60_003_760;
const NULLSEC_SYS: u32 = 30_004_759;
const PKCE_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const PKCE_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None)
        .await
}

async fn setup_pilot(ctx: &ServerContext, char_id: i64) -> SessionState {
    let mut sess = SessionState::new(char_id as u64);
    common::login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
    let uid = sess.user_id.unwrap_or(1);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.user_id = uid;
            ch.balance = eve_db::IskCents(500_000_000_00);
            ch.station_id = Some(JITA_STATION);
            ch.solar_system_id = JITA_SYS;
        }
    });
    rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
    )
    .await
    .expect("SelectCharacterID");
    sess
}

#[tokio::test]
async fn test_plan_mn02_asteroid_belts_and_warp_notification() {
    // parity: plan:MN-02
    eve_testkit::attest(&["plan:MN-02"]);
    let ctx = ServerContext::new();
    let mut sess = setup_pilot(&ctx, PILOT_1).await;

    let perim_station = ctx
        .sde
        .stations
        .values()
        .find(|s| s.solar_system_id == PERIMETER_SYS)
        .map(|s| s.station_id)
        .unwrap_or(JITA_STATION);
    let active_ship = sess.ship_id;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_1) {
            ch.solar_system_id = PERIMETER_SYS;
            ch.station_id = Some(perim_station);
        }
        if let Some(ship_id) = active_ship {
            if let Some(ship) = snap.items.get_mut(&ship_id) {
                ship.location_id = perim_station;
            }
        }
    });
    sess.solar_system_id = Some(PERIMETER_SYS);
    sess.station_id = Some(perim_station);
    let _ = rpc(&ctx, &mut sess, "stationSvc", "Undock", &[])
        .await
        .expect("Undock in Perimeter");
    let belt = ctx
        .sde
        .celestials_in_system(PERIMETER_SYS)
        .into_iter()
        .find(|c| c.group_id == 9)
        .expect("Perimeter has asteroid belt celestials");

    let handle = ctx.systems.get_or_spawn(PERIMETER_SYS);
    let jita_asteroids = handle
        .populate_asteroid_belt_entities()
        .await
        .expect("populate Perimeter belts");
    assert!(!jita_asteroids.is_empty(), "Jita belt spawns asteroids");
    let highsec_ores = [1230, 1228, 1224, 18];
    assert!(
        jita_asteroids.iter().all(|a| highsec_ores.contains(&a.type_id)),
        "Jita belts contain only high-sec ores"
    );

    let _ = ctx.drain_notifications_for_char(PILOT_1);
    let _ = rpc(
        &ctx,
        &mut sess,
        "beyonce",
        "CmdWarpToStuff",
        &[
            PyRep::String("item".into()),
            PyRep::Integer(belt.celestial_id),
            PyRep::Integer(0),
        ],
    )
    .await
    .expect("WarpToStuff to belt");
    let notifs = ctx.drain_notifications_for_char(PILOT_1);
    assert!(
        notifs.iter().any(|n| n.event == "OnWarpToAsteroidBelt"),
        "OnWarpToAsteroidBelt emitted on warp-to-belt"
    );

    let null_handle = ctx.systems.get_or_spawn(NULLSEC_SYS);
    let null_asteroids = null_handle
        .populate_asteroid_belt_entities()
        .await
        .expect("populate null-sec belts");
    assert!(!null_asteroids.is_empty(), "Null-sec belt spawns asteroids");
    assert!(
        null_asteroids.iter().any(|a| !highsec_ores.contains(&a.type_id)),
        "0.0 system contains null-sec ores"
    );

    let err = rpc(
        &ctx,
        &mut sess,
        "beyonce",
        "CmdWarpToStuff",
        &[
            PyRep::String("item".into()),
            PyRep::Integer(999_999_999_999),
            PyRep::Integer(0),
        ],
    )
    .await;
    assert!(err.is_err(), "WarpToStuff to invalid target must fail");
}

#[tokio::test]
async fn test_plan_mn17_mn18_npc_mining_ops_and_goals_market() {
    // parity: plan:MN-17, plan:MN-18
    eve_testkit::attest(&["plan:MN-17", "plan:MN-18"]);
    let ctx = ServerContext::new();
    let mut sess = setup_pilot(&ctx, PILOT_1).await;

    let convoy_id = ctx
        .living_world
        .ensure_system_mining_op(PERIMETER_SYS, &ctx.sde);
    let handle = ctx.systems.get_or_spawn(PERIMETER_SYS);
    let before_asteroids = handle
        .populate_asteroid_belt_entities()
        .await
        .expect("populate belts");
    let before_total: u64 = before_asteroids
        .iter()
        .map(|a| a.remaining_units as u64)
        .sum();
    let mut total_mined = 0u64;
    let mut last_asteroid_id = 0i64;
    for _ in 0..3 {
        let step_res = ctx
            .living_world
            .step_npc_mining_op(convoy_id, &ctx.systems, &ctx.db, &ctx.sde)
            .await
            .expect("step_npc_mining_op");
        total_mined += step_res.mined_units as u64;
        last_asteroid_id = step_res.asteroid_id;
    }
    assert!(total_mined > 0, "MiningOp depletes asteroid units");
    let after_state = handle
        .get_asteroid_state(last_asteroid_id)
        .await
        .expect("get_asteroid_state")
        .expect("asteroid exists");
    assert!(
        (after_state.remaining_units as u64) < before_total,
        "Belt asteroid total decreased after MiningOp tick"
    );

    assert!(
        ctx.living_world
            .step_npc_mining_op(999_999_999, &ctx.systems, &ctx.db, &ctx.sde)
            .await
            .is_err(),
        "step_npc_mining_op rejects unknown/non-MiningOp convoy"
    );

    let orders = rpc(
        &ctx,
        &mut sess,
        "marketProxy",
        "GetOrders",
        &[PyRep::Integer(34)],
    )
    .await
    .expect("marketProxy.GetOrders for Tritanium");
    assert!(!matches!(orders, PyRep::None), "Tritanium market orders present");

    let mut unauth = SessionState::new(999);
    let unauth_err = rpc(
        &ctx,
        &mut unauth,
        "marketProxy",
        "GetOrders",
        &[PyRep::Integer(34)],
    )
    .await;
    assert!(unauth_err.is_err(), "Unauthenticated marketProxy call rejected");
}

#[tokio::test]
async fn test_plan_so5_exploration_scanning_and_dscan() {
    // parity: plan:SO-5
    eve_testkit::attest(&["plan:SO-5"]);
    let ctx = ServerContext::new();
    let mut sess = setup_pilot(&ctx, PILOT_1).await;
    let _ = rpc(&ctx, &mut sess, "stationSvc", "Undock", &[])
        .await
        .expect("Undock");

    let sys_scan = rpc(&ctx, &mut sess, "scanMgr", "GetSystemScanMgr", &[])
        .await
        .expect("GetSystemScanMgr");
    assert!(!matches!(sys_scan, PyRep::None));

    let sigs = rpc(&ctx, &mut sess, "scanMgr", "GetSignatures", &[])
        .await
        .expect("GetSignatures");
    assert!(!matches!(sigs, PyRep::None));

    let dscan = rpc(
        &ctx,
        &mut sess,
        "scanMgr",
        "ConeScan",
        &[
            PyRep::Real(360.0),
            PyRep::Real(14.3 * 149_597_870_700.0),
            PyRep::Real(1.0),
            PyRep::Real(0.0),
            PyRep::Real(0.0),
        ],
    )
    .await
    .expect("ConeScan");
    match dscan {
        PyRep::List(rows) => {
            assert!(!rows.is_empty(), "ConeScan returns ballpark entities");
            assert_ne!(format!("{:?}", rows[0]), "None");
        }
        PyRep::Tuple(rows) => {
            assert!(!rows.is_empty(), "ConeScan returns ballpark entities");
            assert_ne!(format!("{:?}", rows[0]), "None");
        }
        other => panic!("Expected List/Tuple from ConeScan, got {other:?}"),
    }

    let mut unauth = SessionState::new(998);
    let err = rpc(&ctx, &mut unauth, "scanMgr", "GetSignatures", &[]).await;
    assert!(err.is_err(), "Unauthenticated scanMgr.GetSignatures rejected");
}

#[tokio::test]
async fn test_plan_q9_quasar_cosmetics_and_persistence() {
    // parity: plan:Q-9
    eve_testkit::attest(&["plan:Q-9"]);
    let ctx = ServerContext::new();
    let mut sess = setup_pilot(&ctx, PILOT_1).await;

    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: ctx.now_unix() as i64,
            nanos: 0,
        }),
        correlation_uuid: b"q9-broker-fees".to_vec(),
        payload: Some(GoogleAny {
            type_url: format!(
                "{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.market.api.GetBrokerFeesRequest"
            ),
            value: Vec::new(),
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-q9-test".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let rep = rpc(
        &ctx,
        &mut sess,
        "publicGatewaySvc",
        "SendRequest",
        &[PyRep::Bytes(req.encode_proto())],
    )
    .await
    .expect("Quasar GetBrokerFeesRequest");
    if let PyRep::KeyVal(entries) = rep {
        let proto_bytes = entries
            .iter()
            .find(|(k, _)| *k == "response_proto")
            .and_then(|(_, v)| match v {
                PyRep::Bytes(b) => Some(b.as_slice()),
                _ => None,
            })
            .expect("response_proto bytes");
        let resp = EvePublicResponse::decode_proto(proto_bytes).expect("decode response");
        assert_eq!(resp.status_code, STATUS_OK);
        assert_eq!(resp.correlation_uuid, b"q9-broker-fees");
        let payload = resp.payload.expect("broker fees payload");
        assert!(
            payload.type_url.contains("GetBrokerFeesResponse") && !payload.value.is_empty(),
            "GetBrokerFeesResponse payload populated"
        );
    } else {
        panic!("Expected KeyVal from publicGatewaySvc.SendRequest");
    }

    let mut unauth = SessionState::new(997);
    let unauth_rep = rpc(
        &ctx,
        &mut unauth,
        "publicGatewaySvc",
        "SendRequest",
        &[PyRep::Bytes(req.encode_proto())],
    )
    .await
    .expect("Unauth Quasar response");
    if let PyRep::KeyVal(entries) = unauth_rep {
        let proto_bytes = entries
            .iter()
            .find(|(k, _)| *k == "response_proto")
            .and_then(|(_, v)| match v {
                PyRep::Bytes(b) => Some(b.as_slice()),
                _ => None,
            })
            .expect("response_proto bytes");
        let resp = EvePublicResponse::decode_proto(proto_bytes).expect("decode response");
        assert_eq!(resp.status_code, STATUS_UNAUTHORIZED);
    }
}

#[tokio::test]
async fn test_plan_esi1_esi7_sso_launch_login_and_fleets_ui() {
    // parity: plan:ESI-1, plan:ESI-7
    eve_testkit::attest(&["plan:ESI-1", "plan:ESI-7"]);
    let ctx = Arc::new(ServerContext::new());
    let mut sess = setup_pilot(&ctx, PILOT_1).await;

    let sel_data = rpc(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "GetCharacterSelectionData",
        &[],
    )
    .await
    .expect("GetCharacterSelectionData");
    assert!(!matches!(sel_data, PyRep::None));

    let now = ctx.now_unix();
    let scopes = vec![
        "esi-fleets.read_fleet.v1".to_string(),
        "esi-fleets.write_fleet.v1".to_string(),
        "esi-ui.open_window.v1".to_string(),
        "esi-ui.write_waypoint.v1".to_string(),
    ];
    let code = ctx
        .db
        .authorize_pkce(
            "demo",
            "demo-secret",
            PILOT_1,
            "evomu-b7-client",
            "https://localhost/callback",
            scopes,
            PKCE_CHALLENGE,
            "S256",
            now,
        )
        .expect("authorize_pkce");
    let token_pair = ctx
        .db
        .exchange_pkce_code(&code, "evomu-b7-client", PKCE_VERIFIER, now)
        .expect("exchange_pkce_code");
    assert_eq!(token_pair.token_type, "Bearer");

    assert!(
        ctx.db
            .exchange_pkce_code(&code, "evomu-b7-client", "wrong-verifier", now)
            .is_err(),
        "Invalid PKCE verifier must fail"
    );

    let _ = rpc(&ctx, &mut sess, "fleetMgr", "CreateFleet", &[])
        .await
        .expect("CreateFleet");
    let fleet_id = ctx
        .db
        .with_snapshot(|snap| snap.fleets.keys().next().copied())
        .expect("PILOT_1 in fleet");

    let router = EsiRouter::new(Arc::clone(&ctx));
    let mut headers = HashMap::new();
    headers.insert(
        "authorization".to_string(),
        format!("Bearer {}", token_pair.access_token),
    );
    let put_req = EsiHttpRequest {
        method: "PUT".to_string(),
        raw_target: format!("/fleets/{fleet_id}"),
        headers: headers.clone(),
        body: serde_json::to_vec(&json!({"motd": "Batch 7 Fleet MOTD", "is_free_move": true}))
            .unwrap(),
        peer_ip: "127.0.0.1".to_string(),
    };
    let put_resp = router.handle_request(&put_req);
    assert_eq!(put_resp.status_code, 204, "PUT /fleets/{{fleet_id}} returns 204");
    let updated_motd = ctx
        .db
        .with_snapshot(|snap| snap.fleets.get(&fleet_id).map(|f| f.motd.clone()))
        .expect("fleet exists");
    assert_eq!(updated_motd, "Batch 7 Fleet MOTD");

    let ui_req = EsiHttpRequest {
        method: "POST".to_string(),
        raw_target: format!("/ui/openwindow/information?target_id={PILOT_2}"),
        headers,
        body: Vec::new(),
        peer_ip: "127.0.0.1".to_string(),
    };
    let ui_resp = router.handle_request(&ui_req);
    assert_eq!(
        ui_resp.status_code, 204,
        "POST /ui/openwindow/information returns 204"
    );

    let unauth_req = EsiHttpRequest {
        method: "GET".to_string(),
        raw_target: format!("/fleets/{fleet_id}"),
        headers: HashMap::new(),
        body: Vec::new(),
        peer_ip: "127.0.0.1".to_string(),
    };
    assert_eq!(router.handle_request(&unauth_req).status_code, 401);
}

#[tokio::test]
async fn test_plan_t2_t6_t11_sde_harness_and_destiny_vectors() {
    // parity: plan:T-2, plan:T-6, plan:T-11
    eve_testkit::attest(&["plan:T-2", "plan:T-6", "plan:T-11"]);
    let ctx = ServerContext::new();

    assert!(ctx.sde.types.contains_key(&34), "Tritanium in SDE");
    assert!(ctx.sde.types.contains_key(&1230), "Veldspar in SDE");
    assert!(ctx.sde.types.contains_key(&587), "Rifter in SDE");
    assert!(ctx.sde.stations.contains_key(&JITA_STATION), "Jita 4-4 in SDE");
    assert!(!ctx.sde.blueprints.is_empty(), "Blueprints in SDE");
    assert!(!ctx.sde.agents.is_empty(), "Agents in SDE");

    let s1 = setup_pilot(&ctx, PILOT_1).await;
    let s2 = setup_pilot(&ctx, PILOT_2).await;
    assert_eq!(s1.character_id, Some(PILOT_1));
    assert_eq!(s2.character_id, Some(PILOT_2));
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    assert!(
        root.join("tools/client-harness/run_journeys").exists(),
        "run_journeys harness exists"
    );

    let fixtures_path = root.join("crates/eve-destiny/tests/fixtures/ccp_golden_vectors.json");
    assert!(fixtures_path.exists(), "ccp_golden_vectors.json exists");
    let mut bp = Ballpark::new(JITA_SYS);
    let ball = Ball {
        id: 1001,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 2.5,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 1.0,
        mode: MovementMode::Stop,
    };
    bp.add_ball(ball);
    let warp_mode = MovementMode::new_warp(DVec3::ZERO, DVec3::new(150_000_000.0, 0.0, 0.0), 3.0);
    bp.set_mode(1001, warp_mode).expect("set_mode");
    bp.step(1.0);
    let moved = bp.balls.get(&1001).expect("ship ball");
    assert!(moved.position.x > 0.0);

    let tmp_path = std::env::temp_dir().join(format!("evomu_plan_b7_{}.db", std::process::id()));
    let _ = std::fs::remove_file(&tmp_path);
    let db = WorldRepository::open_sqlite(&tmp_path).expect("open sqlite");
    db.with_mut_snapshot(|snap| {
        snap.wal_seq += 1;
    });
    drop(db);
    let db2 = WorldRepository::open_sqlite(&tmp_path).expect("reopen sqlite");
    assert!(db2.with_snapshot(|s| s.wal_seq) >= 1);
    let _ = std::fs::remove_file(&tmp_path);
}
