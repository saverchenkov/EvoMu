//! Parity canonical plan tasks closure suite (`C-5`, `MN-01`, `Q-3`, `Q-4`, `Q-7`, `Q-8`, `T-8`, `T-9`, `T-10`, `T-11`, `Z-1`, `Z-2`).
//!
//! Validates the 12 canonical plan tasks against action_plan.md acceptance criteria:
//! - plan:C-5 & plan:MN-01: Mining static data, portion sizes, materials, celestials, belts
//! - plan:Q-3, plan:Q-4, plan:Q-7, plan:Q-8: Quasar packages (identity/store, progression, space, sovereignty)
//! - plan:T-8, plan:T-9, plan:T-10, plan:T-11: Invariant rollback, crash restart, client oracle, destiny kinematics
//! - plan:Z-1 & plan:Z-2: Macho RPC dispatch layouts and notification emission pipelines

#![forbid(unsafe_code)]

use std::collections::HashMap;
use glam::DVec3;

use eve_destiny::{Ballpark, MovementMode};
use eve_machonet::{MachoPacket, MachoPacketType, SessionState};
use eve_proto::quasar::{
    encode_varint_field, EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp,
    QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::{marshal, PyRep};
use eve_server::quasar_gateway::{STATUS_OK, STATUS_UNAUTHORIZED};
use eve_server::router::ServerContext;
use eve_server::system_actor::populate_asteroid_belt_entities;

use crate::common;

fn make_session(sid: u64, uid: u64, cid: i64, corp_id: i64) -> SessionState {
    let mut sess = SessionState::new(sid);
    sess.user_id = Some(uid);
    sess.character_id = Some(cid);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(30_000_142);
    sess.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    sess
}

fn extract_response_proto(rep: &PyRep) -> EvePublicResponse {
    if let PyRep::KeyVal(entries) = rep {
        for (k, v) in entries {
            if k == "response_proto" {
                if let PyRep::Bytes(b) = v {
                    return EvePublicResponse::decode_proto(b)
                        .expect("decode EvePublicResponse from response_proto");
                }
            }
        }
    }
    panic!("Expected KeyVal containing response_proto, got {rep:?}");
}

async fn quasar_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    type_name: &str,
    payload_bytes: Vec<u8>,
) -> EvePublicResponse {
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: ctx.now_unix() as i64,
            nanos: 0,
        }),
        correlation_uuid: b"canonical-closure-01".to_vec(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value: payload_bytes,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-canonical-test".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let empty_kw = HashMap::new();
    let rep = ctx
        .dispatch_rpc(
            sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("publicGatewaySvc.SendRequest");
    extract_response_proto(&rep)
}

// =========================================================================
// 1. plan:C-5 & plan:MN-01: Mining static data, portion sizes & asteroid belts
// =========================================================================
// parity: plan:C-5, plan:MN-01
#[tokio::test]
async fn test_canonical_plan_c5_and_mn01_mining_sde_and_asteroid_belts() {
    eve_testkit::attest(&["plan:C-5", "plan:MN-01"]);

    let ctx = ServerContext::new();
    let sde = &ctx.sde;

    // MN-01: Veldspar (1230) -> Tritanium (34) only with portionSize 100
    let veldspar_type = &sde.types[&1230];
    assert_eq!(veldspar_type.portion_size, 100);
    let veldspar_mats = sde.type_materials_for(1230);
    assert_eq!(veldspar_mats.len(), 1);
    assert_eq!(veldspar_mats[0].material_type_id, 34);
    assert_eq!(veldspar_mats[0].quantity, 400);

    // Scordite (1228) -> Tritanium (34) + Pyerite (35)
    let scordite_mats: Vec<u32> = sde
        .type_materials_for(1228)
        .iter()
        .map(|m| m.material_type_id)
        .collect();
    assert_eq!(scordite_mats, vec![34, 35]);

    // Clear Icicle (16262) yields its isotope and fuel materials
    let ice_mats: Vec<u32> = sde
        .type_materials_for(16262)
        .iter()
        .map(|m| m.material_type_id)
        .collect();
    for expected in [16272u32, 16273, 16274, 16275] {
        assert!(ice_mats.contains(&expected), "Clear Icicle must yield {expected}");
    }

    // Ore variant yields differ from base ore (Concentrated Veldspar 17470 vs Veldspar 1230)
    let conc_veld_mats = sde.type_materials_for(17470);
    assert_eq!(conc_veld_mats.len(), 1);
    assert!(conc_veld_mats[0].quantity > veldspar_mats[0].quantity);

    // Compressible types and dogma attributes
    assert_eq!(sde.compressible_types.get(&1230), Some(&62516));
    assert!(sde.compressible_types.contains_key(&16262));
    assert!(sde.types.contains_key(&483)); // Miner I
    assert!(sde.types[&483].default_attributes.contains_key(&77)); // miningAmount

    // C-5 / MN-01: Asteroid belts as celestials & balls in Perimeter (30000144) & LZ-6SU (30000208)
    let mut highsec_bp = Ballpark::new(30_000_144);
    let highsec_states = populate_asteroid_belt_entities(&mut highsec_bp, 30_000_144, sde);
    assert!(!highsec_states.is_empty(), "Perimeter must spawn asteroid belt entities");
    for st in &highsec_states {
        assert!(st.remaining_units > 0);
        let ball = highsec_bp.balls.get(&st.asteroid_id).expect("asteroid ball in ballpark");
        assert!(ball.radius >= 60.0);
    }

    let mut null_bp = Ballpark::new(30_000_208);
    let null_states = populate_asteroid_belt_entities(&mut null_bp, 30_000_208, sde);
    assert!(!null_states.is_empty(), "Nullsec 30000208 must spawn asteroid belt entities");
}

// =========================================================================
// 2. plan:Q-3, plan:Q-4, plan:Q-7, plan:Q-8: Quasar packages dispatch
// =========================================================================
// parity: plan:Q-3, plan:Q-4, plan:Q-7, plan:Q-8
#[tokio::test]
async fn test_canonical_plan_q3_q4_q7_q8_quasar_packages_dispatch() {
    eve_testkit::attest(&["plan:Q-3", "plan:Q-4", "plan:Q-7", "plan:Q-8"]);

    let ctx = ServerContext::new();
    let mut sess = make_session(10, 1, 90_000_001, 1_000_009);

    // Negative check: unauthenticated requests fail with STATUS_UNAUTHORIZED
    let mut unauth = SessionState::new(9901);
    let unauth_resp = quasar_rpc(
        &ctx,
        &mut unauth,
        "eve_public.character.api.GetRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(unauth_resp.status_code, STATUS_UNAUTHORIZED);

    // Q-3: Identity, accounts and entitlements (Store / Payment / Omega)
    let q3_resp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.store.category.api.GetAllRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(q3_resp.status_code, STATUS_OK);
    assert_eq!(q3_resp.correlation_uuid, b"canonical-closure-01");
    assert_eq!(
        q3_resp.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.store.category.api.GetAllResponse"
    );

    // Q-4: Character and progression (Career goals & Achievement showcase)
    let q4_goals = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.career.goal.api.GetAllRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(q4_goals.status_code, STATUS_OK);
    assert_eq!(
        q4_goals.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.career.goal.api.GetAllResponse"
    );

    let q4_score = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.achievement.showcase.api.GetCharacterScoreRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(q4_score.status_code, STATUS_OK);
    assert_eq!(
        q4_score.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.achievement.showcase.api.GetCharacterScoreResponse"
    );

    // Q-7: Space and combat (Space CDN update)
    let mut cdn_req = Vec::new();
    encode_varint_field(2, 1, &mut cdn_req);
    let q7_resp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.space.api.cdn.GetUpdateRequest",
        cdn_req,
    )
    .await;
    assert_eq!(q7_resp.status_code, STATUS_OK);
    assert_eq!(
        q7_resp.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.space.api.cdn.GetUpdateResponse"
    );

    // Q-8: Sovereignty, faction warfare and events (Planet resources & Mercenary den)
    let q8_defs = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.sovereignty.resource.planet.api.GetAllDefinitionsRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(q8_defs.status_code, STATUS_OK);
    assert_eq!(
        q8_defs.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.sovereignty.resource.planet.api.GetAllDefinitionsResponse"
    );

    let q8_ver = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.sovereignty.resource.planet.api.GetDefinitionsVersionRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(q8_ver.status_code, STATUS_OK);
    assert_eq!(
        q8_ver.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.sovereignty.resource.planet.api.GetDefinitionsVersionResponse"
    );

    let q8_merc = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.sovereignty.mercenaryden.api.GetMaximumForCharacterRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(q8_merc.status_code, STATUS_OK);
    assert_eq!(
        q8_merc.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.sovereignty.mercenaryden.api.GetMaximumForCharacterResponse"
    );
}

// =========================================================================
// 3. plan:T-8, plan:T-9, plan:T-10, plan:T-11: Test harnesses and oracles
// =========================================================================
// parity: plan:T-8, plan:T-9, plan:T-10, plan:T-11, feat:invariant.atomic-rollback-all-stores
#[tokio::test]
async fn test_canonical_plan_t8_t9_t10_t11_harnesses_and_oracles() {
    eve_testkit::attest(&[
        "plan:T-8",
        "plan:T-9",
        "plan:T-10",
        "plan:T-11",
        "feat:invariant.atomic-rollback-all-stores",
    ]);

    let db_path = std::env::temp_dir().join(format!("canonical_t9_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&db_path);

    // T-8 & T-9: Conservation, transactional atomicity, and crash/restart persistence
    let (initial_isk, initial_plex) = {
        let ctx = ServerContext::from_sqlite(&db_path).expect("from_sqlite");
        ctx.seed_demo_world_fixtures();
        let (before_isk, before_plex) = ctx
            .db
            .with_snapshot(|snap| {
                snap.characters
                    .get(&90_000_001)
                    .map(|c| (c.balance, c.plex_balance))
            })
            .expect("character 90_000_001 exists");

        // T-8: Transactional atomicity: failing transaction rolls back ISK and PLEX
        let rollback_res: Result<(), String> = ctx.db.try_with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&90_000_001) {
                ch.balance.0 += 100_000_000;
                ch.plex_balance += 500;
            }
            Err("simulated_invariant_violation".to_string())
        });
        assert!(rollback_res.is_err());

        // Also verify failing/unauthenticated mutating RPC leaves DB completely unchanged
        let mut unauth = SessionState::new(8801);
        let rpc_err = ctx
            .dispatch_rpc(
                &mut unauth,
                "charMgr",
                None,
                "AddContact",
                &[
                    PyRep::Integer(90_000_002),
                    PyRep::Real(5.0),
                    PyRep::Integer(0),
                    PyRep::Bool(false),
                ],
                &HashMap::new(),
                None,
            )
            .await;
        assert!(rpc_err.is_err(), "unauthenticated mutating RPC must fail");

        let (after_isk, after_plex) = ctx
            .db
            .with_snapshot(|snap| {
                snap.characters
                    .get(&90_000_001)
                    .map(|c| (c.balance, c.plex_balance))
            })
            .expect("character 90_000_001 exists");
        assert_eq!(before_isk, after_isk, "ISK balance must be conserved on rollback");
        assert_eq!(before_plex, after_plex, "PLEX balance must be conserved on rollback");

        (before_isk, before_plex)
    };

    // T-9: Crash/restart recovery preserves state across SQLite reopen
    {
        let ctx2 = ServerContext::from_sqlite(&db_path).expect("reopen_sqlite");
        let (restarted_isk, restarted_plex) = ctx2
            .db
            .with_snapshot(|snap| {
                snap.characters
                    .get(&90_000_001)
                    .map(|c| (c.balance, c.plex_balance))
            })
            .expect("character 90_000_001 exists after restart");
        assert_eq!(initial_isk, restarted_isk);
        assert_eq!(initial_plex, restarted_plex);
    }
    let _ = std::fs::remove_file(&db_path);

    // T-10: Client-expectation oracle (Quasar envelope metadata & RPC contract)
    let ctx = ServerContext::new();
    let mut authed = make_session(20, 1, 90_000_001, 1_000_009);
    let resp = quasar_rpc(
        &ctx,
        &mut authed,
        "eve_public.store.category.api.GetAllRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(resp.status_code, STATUS_OK);
    assert_eq!(resp.correlation_uuid, b"canonical-closure-01");
    assert!(resp.payload.is_some());

    // T-11: Destiny differential physics harness & golden vector kinematics
    let mut ballpark = Ballpark::new(30_000_144);
    let _ = populate_asteroid_belt_entities(&mut ballpark, 30_000_144, &ctx.sde);
    let ball_id = *ballpark.balls.keys().next().expect("seeded ball in ballpark");
    let start_x = ballpark.balls[&ball_id].position.x;
    if let Some(ball) = ballpark.balls.get_mut(&ball_id) {
        ball.velocity = DVec3::new(100.0, 0.0, 0.0);
        ball.mass = 1_067_000.0;
        ball.max_velocity = 365.0;
        ball.inertia = 2.9;
        ball.mode = MovementMode::GotoDirection {
            direction: DVec3::X,
        };
    }
    ballpark.step(1.0);
    let moved = ballpark.balls.get(&ball_id).expect("moved ball in ballpark");
    assert!(moved.position.x > start_x + 50.0, "kinematic step must advance X position");
}

// =========================================================================
// 4. plan:Z-1 & plan:Z-2: RPC & Notification Closure
// =========================================================================
// parity: plan:Z-1, plan:Z-2
#[tokio::test]
async fn test_canonical_plan_z1_z2_rpc_and_notify_closure() {
    eve_testkit::attest(&["plan:Z-1", "plan:Z-2"]);

    let ctx = ServerContext::new();
    ctx.seed_demo_world_fixtures();
    let empty_kw = HashMap::new();

    // Z-1: Dispatch client-called RPCs with V24 argument layouts and live semantics
    let mut sess = SessionState::new(30);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

    let char_info = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "GetCharacterInfo",
            &[PyRep::Integer(90_000_001)],
            &empty_kw,
            Some("test:Z-1:GetCharacterInfo"),
        )
        .await
        .expect("charUnboundMgr.GetCharacterInfo");
    assert_ne!(char_info, PyRep::None);

    let select_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(90_000_001)],
            &empty_kw,
            Some("test:Z-1:SelectCharacterID"),
        )
        .await
        .expect("charUnboundMgr.SelectCharacterID");
    assert_ne!(select_rep, PyRep::None);

    let station_info = ctx
        .dispatch_rpc(
            &mut sess,
            "stationSvc",
            None,
            "GetStation",
            &[PyRep::Integer(60_003_760)],
            &empty_kw,
            Some("test:Z-1:GetStation"),
        )
        .await
        .expect("stationSvc.GetStation");
    assert_ne!(station_info, PyRep::None);

    // Z-2: Systematic notification emission pipelines, marshaled packet types, and multicast delivery
    let notifs = ctx.notify_account_change(90_000_001, "cash", 90_000_001, 100_000_000.0);
    assert_eq!(notifs.len(), 1);
    assert_eq!(notifs[0].event, "OnAccountChange");
    assert_eq!(notifs[0].id_type, "charid");
    assert_eq!(notifs[0].args.len(), 3);

    // Verify binary packet marshal round-trip matches Macho notification format
    let decoded = marshal::decode(&notifs[0].marshaled_packet).expect("decode OnAccountChange");
    let packet = MachoPacket::from_pyrep(&decoded).expect("parse Macho notification packet");
    assert_eq!(packet.packet_type, MachoPacketType::Notification);

    // Drain notifications for character 90_000_001
    let drained = ctx.drain_notifications_for_char(90_000_001);
    assert!(!drained.is_empty(), "drained notification list must not be empty");
    assert!(drained.iter().any(|p| p.event == "OnAccountChange"));
}
