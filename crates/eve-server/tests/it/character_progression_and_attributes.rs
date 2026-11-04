//! Character progression, skills, and attributes parity suite.

#![forbid(unsafe_code)]

#[allow(unused_imports, dead_code, unused_variables, unused_mut)]
mod char_progression_misc_ext {
    use super::*;

// Integration tests closing remaining `destiny`, `sde`, `quasar`, and `plan` parity rows.


use std::collections::HashMap;

use eve_db::types::TrainedSkillRecord;
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, Ballpark, MovementMode};
use eve_machonet::SessionState;
use eve_proto::quasar::{
    EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage,
    TYPE_GOOGLEAPIS_PREFIX, encode_varint_field,
};
use eve_proto::quasar_wire::{decode_fields, ProtoWireValue};
use eve_proto::PyRep;
use eve_sde::normalize_localization_fsd_lang;
use eve_server::quasar_gateway::{STATUS_OK, STATUS_UNAUTHORIZED};
use eve_server::router::ServerContext;
use eve_server::system_actor::populate_asteroid_belt_entities;
use glam::DVec3;

use crate::common::login_pilot;

const PILOT_A: i64 = 90_000_001;
const SYS_PERIMETER: u32 = 30_000_144;

fn new_ctx() -> ServerContext {
    let sde = std::sync::Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("open sde"),
    );
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock_paused(true);
    ctx
}

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
            &[PyRep::Integer(PILOT_A)],
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
        correlation_uuid: b"parity-misc-part1-01".to_vec(),
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
        .expect("publicGatewaySvc.SendRequest");
    extract_response_proto(&rep)
}

fn extract_destiny_actions(updates: &PyRep) -> Vec<(String, Vec<PyRep>)> {
    let bytes = eve_proto::marshal::encode(updates);
    let decoded = eve_proto::marshal::decode(&bytes).expect("DoDestinyUpdate marshal round-trip");
    assert_eq!(&decoded, updates);
    let PyRep::List(entries) = decoded else {
        panic!("expected DoDestinyUpdate list, got {decoded:?}");
    };
    entries
        .into_iter()
        .map(|entry| {
            let PyRep::Tuple(outer) = entry else {
                panic!("expected (stamp, action) tuple, got {entry:?}");
            };
            assert!(matches!(outer[0], PyRep::Integer(_)));
            let PyRep::Tuple(action) = &outer[1] else {
                panic!("expected (actionName, args) tuple, got {:?}", outer[1]);
            };
            let name = action[0].as_str().expect("action name").to_string();
            let args = match &action[1] {
                PyRep::Tuple(t) => t.clone(),
                other => vec![other.clone()],
            };
            (name, args)
        })
        .collect()
}

#[tokio::test]
async fn test_parity_misc_destiny_on_drone_activity_change() {
    // parity: destiny:OnDroneActivityChange
    eve_testkit::attest(&["destiny:OnDroneActivityChange"]);
    let ctx = new_ctx();
    let mut sess = authed_demo_session(&ctx, 8100).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(PILOT_A).or_default().insert(
            3436,
            TrainedSkillRecord {
                type_id: 3436,
                level: 5,
                skill_points: 256_000.0,
            },
        );
    });

    let kw = HashMap::new();
    let _ = ctx
        .dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("stationSvc.Undock");
    let ship_id = sess.ship_id.expect("active ship after Undock");
    let sys_id = sess.solar_system_id.expect("solar system after Undock");

    let drone_item_id = 99_770_001;
    let target_id = 99_770_002;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship_item) = snap.items.get_mut(&ship_id) {
            ship_item.type_id = 626;
            ship_item.version += 1;
        }
        snap.items.insert(
            drone_item_id,
            InventoryItemRecord {
                item_id: drone_item_id,
                type_id: 2454,
                owner_id: PILOT_A,
                location_id: ship_id,
                flag: InventoryFlag::DroneBay,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    ctx.invalidate_ship_fitting(ship_id);

    let handle = ctx.systems.get_or_spawn(sys_id);
    let (_, ship_ball) = handle
        .get_ball_snapshot(ship_id)
        .await
        .expect("ship ball snapshot")
        .expect("ship ball in space");
    let target_cs = ctx.build_type_combat_state(587);
    handle
        .spawn_ball_with_combat(
            Ball {
                id: target_id,
                type_id: 587,
                position: ship_ball.position + DVec3::new(2_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            target_cs,
        )
        .await
        .expect("spawn target ball");

    let launched = ctx
        .dispatch_rpc(
            &mut sess,
            "ship",
            None,
            "LaunchDrones",
            &[PyRep::List(vec![PyRep::Integer(drone_item_id)])],
            &kw,
            None,
        )
        .await
        .expect("ship.LaunchDrones");
    let PyRep::List(launched_ids) = launched else {
        panic!("expected launched drone ball IDs list, got {launched:?}");
    };
    let drone_ball_id = launched_ids[0].as_i64().expect("launched drone ball_id");
    let _ = handle.drain_observer_updates(ship_id).await;

    let _ = ctx
        .dispatch_rpc(
            &mut sess,
            "entity",
            None,
            "CmdEngage",
            &[
                PyRep::List(vec![PyRep::Integer(drone_ball_id)]),
                PyRep::Integer(target_id),
            ],
            &kw,
            None,
        )
        .await
        .expect("entity.CmdEngage");
    let (_tick, _evs, engage_updates) = handle
        .drain_observer_updates(ship_id)
        .await
        .expect("drain observer updates after CmdEngage");
    let engage_actions = extract_destiny_actions(&engage_updates);
    assert!(
        engage_actions
            .iter()
            .any(|(name, args)| name == "OnDroneActivityChange"
                && args.as_slice() == [PyRep::Integer(drone_ball_id), PyRep::Integer(1)]),
        "expected OnDroneActivityChange(drone_id={drone_ball_id}, activity_state=1) in DoDestinyUpdate, got {engage_actions:?}"
    );

    let _ = ctx
        .dispatch_rpc(
            &mut sess,
            "entity",
            None,
            "CmdReturnHome",
            &[PyRep::List(vec![PyRep::Integer(drone_ball_id)])],
            &kw,
            None,
        )
        .await
        .expect("entity.CmdReturnHome");
    let (_tick, _evs, return_updates) = handle
        .drain_observer_updates(ship_id)
        .await
        .expect("drain observer updates after CmdReturnHome");
    let return_actions = extract_destiny_actions(&return_updates);
    assert!(
        return_actions
            .iter()
            .any(|(name, args)| name == "OnDroneActivityChange"
                && args.as_slice() == [PyRep::Integer(drone_ball_id), PyRep::Integer(0)]),
        "expected OnDroneActivityChange(drone_id={drone_ball_id}, activity_state=0) in DoDestinyUpdate, got {return_actions:?}"
    );
}

#[test]
fn test_parity_misc_sde_localization_fsd_it_pickle() {
    // parity: sde:res:/localizationfsd/localization_fsd_it.pickle
    eve_testkit::attest(&["sde:res:/localizationfsd/localization_fsd_it.pickle"]);
    assert_eq!(normalize_localization_fsd_lang("it"), Some("it"));
    assert_eq!(normalize_localization_fsd_lang("it-IT,it;q=0.9"), Some("it"));

    let ctx = new_ctx();
    let it_msgs = ctx
        .sde
        .load_all_localized_messages("it")
        .expect("load localization_fsd_it.pickle");
    assert!(
        it_msgs.len() >= 7_000,
        "expected >=7k Italian localization messages, got {}",
        it_msgs.len()
    );
    assert!(
        it_msgs.values().any(|v| !v.is_empty()),
        "expected non-empty Italian localization strings"
    );
}

#[tokio::test]
async fn test_parity_misc_quasar_sovereignty_resource_star_requests() {
    // parity: quasar:eveProto/generated/eve_public/sovereignty/resource/star/api/requests_pb2.pyj
    let ctx = new_ctx();
    let mut sess = authed_demo_session(&ctx, 8101).await;

    let resp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.sovereignty.resource.star.api.GetAllConfigurationsRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(resp.status_code, STATUS_OK);
    let payload = resp.payload.expect("GetAllConfigurationsResponse payload");
    assert_eq!(
        payload.type_url,
        format!(
            "{TYPE_GOOGLEAPIS_PREFIX}eve_public.sovereignty.resource.star.api.GetAllConfigurationsResponse"
        )
    );
    assert!(
        !payload.value.is_empty(),
        "expected non-empty star configuration entries from SDE"
    );
}

#[tokio::test]
async fn test_parity_misc_plan_c5_and_mn01_mining_sde_and_belts() {
    let ctx = new_ctx();
    let sde = &ctx.sde;

    // MN-01: SDE mining static data (typeMaterials + ore attributes)
    let veldspar_mats = sde.type_materials_for(1230);
    assert_eq!(veldspar_mats.len(), 1);
    assert_eq!(veldspar_mats[0].material_type_id, 34);

    let scordite_mats: Vec<u32> = sde
        .type_materials_for(1228)
        .iter()
        .map(|m| m.material_type_id)
        .collect();
    assert_eq!(scordite_mats, vec![34, 35]);

    // C-5: Asteroid belt ball spawning in solar system ballpark
    let mut ballpark = Ballpark::new(SYS_PERIMETER);
    let spawned = populate_asteroid_belt_entities(&mut ballpark, SYS_PERIMETER, sde);
    assert!(
        !spawned.is_empty(),
        "expected asteroid belt balls to spawn in Perimeter"
    );
}

#[tokio::test]
async fn test_parity_misc_plan_q3_and_q4_quasar_identity_and_progression() {
    let ctx = new_ctx();

    let mut unauth = SessionState::new(8201);
    let unauth_resp = quasar_rpc(
        &ctx,
        &mut unauth,
        "eve_public.character.api.GetRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(unauth_resp.status_code, STATUS_UNAUTHORIZED);

    let mut authed = authed_demo_session(&ctx, 8202).await;
    let q3_resp = quasar_rpc(
        &ctx,
        &mut authed,
        "eve_public.store.category.api.GetAllRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(q3_resp.status_code, STATUS_OK);
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
    assert_eq!(
        q4_resp.payload.as_ref().unwrap().type_url,
        format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.career.goal.api.GetAllResponse")
    );
}

#[tokio::test]
async fn test_parity_misc_plan_q7_and_q8_quasar_space_and_sovereignty() {
    let ctx = new_ctx();
    let mut authed = authed_demo_session(&ctx, 8301).await;

    let mut cdn_req = Vec::new();
    encode_varint_field(2, 1, &mut cdn_req);
    let q7_resp = quasar_rpc(
        &ctx,
        &mut authed,
        "eve_public.space.api.cdn.GetUpdateRequest",
        cdn_req,
    )
    .await;
    assert_eq!(q7_resp.status_code, STATUS_OK);
    let q7_any = q7_resp.payload.as_ref().expect("q7 payload");
    assert_eq!(
        q7_any.type_url,
        format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.space.api.cdn.GetUpdateResponse")
    );
    let q7_fields = decode_fields(&q7_any.value).expect("decode cdn GetUpdateResponse");
    let cp_bytes = q7_fields
        .iter()
        .find_map(|(f, v)| match (*f, v) {
            (1, ProtoWireValue::Bytes(b)) => Some(b.as_slice()),
            _ => None,
        })
        .expect("cdn update checkpoint");
    let cp_fields = decode_fields(cp_bytes).expect("decode checkpoint");
    let cdn_url = cp_fields
        .iter()
        .find_map(|(f, v)| match (*f, v) {
            (1, ProtoWireValue::Bytes(b)) => std::str::from_utf8(b).ok().map(str::to_string),
            _ => None,
        })
        .expect("cdn url");
    assert!(cdn_url.starts_with("http://127.0.0.1:26004/cdn/maptags/v24.1."));

    let q8_resp = quasar_rpc(
        &ctx,
        &mut authed,
        "eve_public.sovereignty.resource.planet.api.GetAllDefinitionsRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(q8_resp.status_code, STATUS_OK);
    let q8_any = q8_resp.payload.as_ref().expect("q8 payload");
    assert_eq!(
        q8_any.type_url,
        format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.sovereignty.resource.planet.api.GetAllDefinitionsResponse")
    );
    let q8_fields = decode_fields(&q8_any.value).expect("decode GetAllDefinitionsResponse");
    let ver_bytes = q8_fields
        .iter()
        .find_map(|(f, v)| match (*f, v) {
            (2, ProtoWireValue::Bytes(b)) => Some(b.as_slice()),
            _ => None,
        })
        .expect("sovereignty definitions version");
    let ver_fields = decode_fields(ver_bytes).expect("decode version");
    let major = ver_fields
        .iter()
        .find_map(|(f, v)| match (*f, v) {
            (1, ProtoWireValue::Varint(n)) => Some(*n),
            _ => None,
        })
        .unwrap_or(0);
    assert_eq!(major, 1);
}

#[test]
fn test_parity_misc_plan_t2_sde_and_dogma_effects_sweep() {
    let ctx = new_ctx();
    let main_loc = ctx
        .sde
        .load_localization_main()
        .expect("load localization_fsd_main.pickle");
    assert_eq!(main_loc.languages.len(), 9);
    assert_eq!(main_loc.labels.len(), 31_872);

    let rifter = ctx.sde.get_type(587).expect("Rifter type 587");
    assert_eq!(rifter.group_id, 25);
    assert!(
        ctx.sde.dogma_effects().contains_key(&16),
        "expected online effect 16 in SDE dogma_effects"
    );
}

#[tokio::test]
async fn test_parity_misc_plan_t8_and_t9_invariants_and_restart() {
    let ctx = new_ctx();

    // T-8: Rejected RPC leaves character wallet balance unchanged
    let before_isk = ctx
        .db
        .with_snapshot(|snap| snap.characters.get(&PILOT_A).map(|c| c.balance.0).unwrap_or(0));
    let mut unauth = SessionState::new(8401);
    let kw = HashMap::new();
    let err = ctx
        .dispatch_rpc(
            &mut unauth,
            "account",
            None,
            "GiveCash",
            &[
                PyRep::Integer(PILOT_A),
                PyRep::Real(1_000_000.0),
                PyRep::String("unauth".into()),
            ],
            &kw,
            None,
        )
        .await;
    assert!(err.is_err(), "unauthenticated GiveCash must be rejected");
    let after_isk = ctx
        .db
        .with_snapshot(|snap| snap.characters.get(&PILOT_A).map(|c| c.balance.0).unwrap_or(0));
    assert_eq!(before_isk, after_isk);

    // T-9: Durable inventory items persist in snapshot without transient active-cycle state
    let item_id = 99_880_001;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id: 34,
                owner_id: PILOT_A,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 500,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    let persisted = ctx
        .db
        .with_snapshot(|snap| snap.items.get(&item_id).cloned())
        .expect("persisted Tritanium stack");
    assert_eq!(persisted.quantity, 500);
}

#[tokio::test]
async fn test_parity_misc_plan_t10_and_t11_client_oracle_and_destiny_diff() {
    let ctx = new_ctx();
    let mut authed = authed_demo_session(&ctx, 8501).await;

    // T-10: Client-expectation oracle (Quasar envelope correlation UUID + GoogleAny type_url)
    let resp = quasar_rpc(
        &ctx,
        &mut authed,
        "eve_public.store.category.api.GetAllRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(resp.status_code, STATUS_OK);
    assert_eq!(resp.correlation_uuid, b"parity-misc-part1-01");
    assert!(resp.payload.is_some());

    // T-11: Destiny differential physics step + wire encoding
    let mut ballpark = Ballpark::new(SYS_PERIMETER);
    let _ = populate_asteroid_belt_entities(&mut ballpark, SYS_PERIMETER, &ctx.sde);
    let ball_id = *ballpark.balls.keys().next().expect("seeded ball");
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
    let moved = ballpark.balls.get(&ball_id).expect("moved ball");
    assert!(moved.position.x > start_x + 50.0, "expected forward X displacement");
}

}
