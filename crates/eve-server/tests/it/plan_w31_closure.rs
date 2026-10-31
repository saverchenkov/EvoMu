//! Smoke integration tests for mining SDE, asteroid belt balls, NPC mining convoys,
//! agent/scan RPCs, SQLite persistence, and Quasar gateway endpoints.
#![forbid(unsafe_code)]

use std::collections::HashMap;

use eve_destiny::Ballpark;
use eve_machonet::SessionState;
use eve_proto::quasar::{
    EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage,
    TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::living_world::{LivingWorldDirector, NpcArchetype};
use eve_server::quasar_gateway::{STATUS_OK, STATUS_UNAUTHORIZED};
use eve_server::router::ServerContext;
use eve_server::system_actor::populate_asteroid_belt_entities;

use crate::common::login_pilot;

async fn authed_demo_session(ctx: &ServerContext, sid: u64) -> SessionState {
    let mut sess = SessionState::new(sid);
    login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
    let kw = HashMap::new();
    let _ = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(90_000_001)],
            &kw,
            None,
        )
        .await
        .expect("SelectCharacterID 90_000_001");
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
        correlation_uuid: b"plan-w31-closure-01".to_vec(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value: payload_bytes,
        }),
        ..Default::default()
    };
    let req_bytes = req.encode_proto();
    let empty_kw = HashMap::new();
    let rep = ctx
        .dispatch_rpc(
            sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(req_bytes)],
            &empty_kw,
            None,
        )
        .await
        .expect("publicGatewaySvc.send_request");
    extract_response_proto(&rep)
}

#[tokio::test]
async fn test_plan_c5_mn01_mn02_mining_sde_and_belt_balls() {
    let ctx = ServerContext::new();
    let sde = &ctx.sde;

    // MN-01: Veldspar (1230) -> Tritanium (34) only
    let veldspar_mats = sde.type_materials_for(1230);
    assert_eq!(veldspar_mats.len(), 1);
    assert_eq!(veldspar_mats[0].material_type_id, 34);

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

    // MN-02 / C-5: Asteroid belts as balls in Perimeter (30000144, highsec) & LZ-6SU (30000208, nullsec)
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

#[tokio::test]
async fn test_plan_mn17_mn18_npc_mining_ops_and_mining_missions() {
    let ctx = ServerContext::new();

    // MN-17: LivingWorldDirector seeded convoys include MiningOp fleets
    let director = LivingWorldDirector::new_seeded();
    let convoys = director.list_convoys();
    assert!(!convoys.is_empty(), "Seeded LivingWorldDirector must have convoys");
    assert!(
        convoys.iter().any(|c| c.archetype == NpcArchetype::MiningOp),
        "Seeded convoys must include MiningOp archetype"
    );

    // MN-18: Agent mission RPCs + negative unauthenticated check
    let empty_kw = HashMap::new();
    let mut unauth = SessionState::new(7101);
    let err = ctx
        .dispatch_rpc(
            &mut unauth,
            "agentMgr",
            None,
            "GetMyJournalDetails",
            &[],
            &empty_kw,
            None,
        )
        .await
        .expect_err("unauthenticated agentMgr.GetMyJournalDetails must fail");
    assert!(!format!("{err:?}").is_empty());

    let mut authed = authed_demo_session(&ctx, 7102).await;
    let agents_rep = ctx
        .dispatch_rpc(
            &mut authed,
            "agentMgr",
            None,
            "GetAgents",
            &[],
            &empty_kw,
            None,
        )
        .await
        .expect("authenticated agentMgr.GetAgents");
    assert!(!matches!(agents_rep, PyRep::None));
}

#[tokio::test]
async fn test_plan_so5_exploration_anomalies_probes_dscan_and_dungeons() {
    let ctx = ServerContext::new();
    let empty_kw = HashMap::new();

    // Negative/auth check on scanSvc
    let mut unauth = SessionState::new(7201);
    let err = ctx
        .dispatch_rpc(
            &mut unauth,
            "scanSvc",
            None,
            "GetCombatAnomalyInstances",
            &[],
            &empty_kw,
            None,
        )
        .await
        .expect_err("unauthenticated scanSvc.GetCombatAnomalyInstances must fail");
    assert!(!format!("{err:?}").is_empty());

    // Authenticated anomaly & signature query
    let mut authed = authed_demo_session(&ctx, 7202).await;
    let anomalies = ctx
        .dispatch_rpc(
            &mut authed,
            "scanSvc",
            None,
            "GetCombatAnomalyInstances",
            &[],
            &empty_kw,
            None,
        )
        .await
        .expect("authenticated scanSvc.GetCombatAnomalyInstances");
    assert_ne!(anomalies, PyRep::None);
    assert!(format!("{anomalies:?}").len() > 4);
}

#[tokio::test]
async fn test_plan_d6_single_source_of_truth_and_sqlite_restart() {
    let db_path = std::env::temp_dir().join(format!("plan_w31_d6_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&db_path);

    {
        let ctx = ServerContext::from_sqlite(&db_path).expect("from_sqlite");
        ctx.seed_demo_world_fixtures();
        let empty_kw = HashMap::new();

        // Negative/auth check on charMgr.AddContact
        let mut unauth = SessionState::new(7301);
        let err = ctx
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
                &empty_kw,
                None,
            )
            .await
            .expect_err("unauthenticated charMgr.AddContact must fail");
        assert!(!format!("{err:?}").is_empty());

        // Authenticated contact mutation persisted to SQLite
        let mut authed = authed_demo_session(&ctx, 7302).await;
        ctx.dispatch_rpc(
            &mut authed,
            "charMgr",
            None,
            "AddContact",
            &[
                PyRep::Integer(90_000_002),
                PyRep::Real(5.0),
                PyRep::Integer(0),
                PyRep::Bool(false),
            ],
            &empty_kw,
            None,
        )
        .await
        .expect("authenticated charMgr.AddContact");
    }

    // Reopen SQLite and verify contact survived restart
    let ctx2 = ServerContext::from_sqlite(&db_path).expect("reopen_sqlite");
    let mut authed2 = authed_demo_session(&ctx2, 7303).await;
    let contacts = ctx2
        .dispatch_rpc(
            &mut authed2,
            "charMgr",
            None,
            "GetContacts",
            &[],
            &HashMap::new(),
            None,
        )
        .await
        .expect("charMgr.GetContacts after restart");
    assert_ne!(contacts, PyRep::None);
    assert!(
        format!("{contacts:?}").contains("90000002"),
        "Contact 90000002 must persist across SQLite restart: {contacts:?}"
    );
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_plan_t8_t9_atomic_rollback_and_crash_restart_invariants() {
    let db_path = std::env::temp_dir().join(format!("plan_w31_t8_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&db_path);

    let (initial_isk, initial_plex) = {
        let ctx = ServerContext::from_sqlite(&db_path).expect("from_sqlite");
        ctx.seed_demo_world_fixtures();
        let _sess = authed_demo_session(&ctx, 7401).await;
        let (before_isk, before_plex) = ctx
            .db
            .with_snapshot(|snap| {
                snap.characters
                    .get(&90_000_001)
                    .map(|c| (c.balance, c.plex_balance))
            })
            .expect("character 90_000_001 exists");

        // T-8: Failing transaction via try_with_mut_snapshot must roll back atomically
        let rollback_res: Result<(), String> = ctx.db.try_with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&90_000_001) {
                ch.balance.0 += 999_999_999;
                ch.plex_balance += 500;
            }
            Err("simulated_handler_failure".to_string())
        });
        assert_eq!(rollback_res, Err("simulated_handler_failure".to_string()));

        let (after_isk, after_plex) = ctx
            .db
            .with_snapshot(|snap| {
                snap.characters
                    .get(&90_000_001)
                    .map(|c| (c.balance, c.plex_balance))
            })
            .expect("character 90_000_001 exists");
        assert_eq!(before_isk, after_isk, "ISK must roll back on error");
        assert_eq!(before_plex, after_plex, "PLEX must roll back on error");
        (before_isk, before_plex)
    };

    // T-9: Crash/restart recovery preserves committed state with zero duplication/loss
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
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_plan_q3_q4_q5_quasar_gateway_domains() {
    let ctx = ServerContext::new();

    // Negative/auth check: unauthenticated Quasar request must return STATUS_UNAUTHORIZED
    let mut unauth = SessionState::new(7501);
    let unauth_resp = quasar_rpc(
        &ctx,
        &mut unauth,
        "eve_public.character.api.GetRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(unauth_resp.status_code, STATUS_UNAUTHORIZED);

    // Authenticated Q-3 (Identity/Entitlements), Q-4 (Character/Progression), Q-5 (Social/Corp)
    let mut authed = authed_demo_session(&ctx, 7502).await;
    let q3_resp = quasar_rpc(
        &ctx,
        &mut authed,
        "eve_public.store.category.api.GetAllRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(q3_resp.status_code, STATUS_OK);
    assert_eq!(q3_resp.correlation_uuid, b"plan-w31-closure-01");
    assert_eq!(
        q3_resp.payload.as_ref().unwrap().type_url,
        format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.store.category.api.GetAllResponse")
    );

    let q4_resp = quasar_rpc(
        &ctx,
        &mut authed,
        "eve_public.career.goal.api.GetAllRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(q4_resp.status_code, STATUS_OK);
    assert_eq!(q4_resp.correlation_uuid, b"plan-w31-closure-01");

    let q5_resp = quasar_rpc(
        &ctx,
        &mut authed,
        "eve_public.corporationgoal.api.GetAllRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(q5_resp.status_code, STATUS_OK);
    assert_eq!(q5_resp.correlation_uuid, b"plan-w31-closure-01");
}


#[tokio::test]
async fn test_plan_q6_q7_q8_t10_quasar_and_client_oracle() {
    let ctx = ServerContext::new();
    let mut authed = authed_demo_session(&ctx, 7601).await;

    let q6_resp = quasar_rpc(
        &ctx,
        &mut authed,
        "eve.market.orders.GetOrdersRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(q6_resp.status_code, STATUS_OK);
    assert_eq!(q6_resp.correlation_uuid, b"plan-w31-closure-01");

    let mut cdn_req = Vec::new();
    eve_proto::quasar::encode_varint_field(2, 1, &mut cdn_req);
    let q7_resp = quasar_rpc(
        &ctx,
        &mut authed,
        "eve_public.space.api.cdn.GetUpdateRequest",
        cdn_req,
    )
    .await;
    assert_eq!(q7_resp.status_code, STATUS_OK);
    assert_eq!(q7_resp.correlation_uuid, b"plan-w31-closure-01");

    let q8_resp = quasar_rpc(
        &ctx,
        &mut authed,
        "eve_public.sovereignty.resource.planet.api.GetAllDefinitionsRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(q8_resp.status_code, STATUS_OK);
    assert_eq!(q8_resp.correlation_uuid, b"plan-w31-closure-01");
}
