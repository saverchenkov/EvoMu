//! Integration test verifying Abyssal Deadspace entry, `ClientIsReady` ballpark sync,
//! session constellation/region propagation, and exit restoration to Jita.

#[path = "common.rs"]
mod common;

use eve_db::InventoryFlag;
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

const PILOT1: i64 = 90_000_001;
const SYS_JITA: u32 = 30_000_142;
const SYS_ABYSS: u32 = 32_000_001;
const SHIP_ID: i64 = 100_000_001;
const FILAMENT_TYPE: u32 = 56134; // Tranquil Firestorm Filament (T0)
const FILAMENT_ITEM_ID: i64 = 1_300_000_777;

fn keyval_i64(rep: &PyRep, key: &str) -> Option<i64> {
    rep.dict_get(key).and_then(|v| v.as_i64())
}

fn keyval_i64_list(rep: &PyRep, key: &str) -> Vec<i64> {
    match rep.dict_get(key) {
        Some(PyRep::List(items)) | Some(PyRep::Tuple(items)) => {
            items.iter().filter_map(|v| v.as_i64()).collect()
        }
        _ => Vec::new(),
    }
}

async fn move_ship_near_gate(ctx: &ServerContext, sys_id: u32, ship_id: i64, gate_id: i64) {
    let handle = ctx.systems.get_or_spawn(sys_id);
    let gate_pos = handle
        .get_ball_snapshot(gate_id)
        .await
        .ok()
        .flatten()
        .map(|(_, b)| b.position)
        .unwrap_or(DVec3::ZERO);
    let _ = handle
        .set_ball_position(ship_id, gate_pos + DVec3::new(100.0, 0.0, 0.0))
        .await;
}

#[tokio::test]
async fn test_abyssal_entry_client_is_ready_and_exit_sync() {
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(9_801);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    sess.role = u64::MAX;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(PILOT1)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT1) {
            ch.solar_system_id = SYS_JITA;
            ch.station_id = None;
            ch.structure_id = None;
        }
        if let Some(it) = snap.items.get_mut(&SHIP_ID) {
            it.type_id = 621;
            it.location_id = SYS_JITA as i64;
        }
        snap.items.insert(
            FILAMENT_ITEM_ID,
            eve_db::InventoryItemRecord {
                item_id: FILAMENT_ITEM_ID,
                type_id: FILAMENT_TYPE,
                owner_id: PILOT1,
                location_id: SHIP_ID,
                flag: InventoryFlag::Cargo,
                quantity: 3,
                singleton: false,
                ..Default::default()
            },
        );
    });
    sess.solar_system_id = Some(SYS_JITA);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(SHIP_ID);

    let jita_handle = ctx.systems.get_or_spawn(SYS_JITA);
    let ship_ball = Ball {
        id: SHIP_ID,
        type_id: 621,
        position: DVec3::new(25_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 250.0,
        inertia: 0.5,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let cs = ctx.build_ship_combat_state(PILOT1, SHIP_ID, 621);
    let slim = ctx.build_ship_slim_meta(PILOT1, 621);
    jita_handle
        .spawn_ball_with_meta(ship_ball, cs, slim)
        .await
        .expect("spawn Jita ship ball");

    let deploy = ctx
        .dispatch_rpc(
            &mut sess,
            "abyssalMgr",
            None,
            "AbyssalEntranceDeployment",
            &[PyRep::Integer(FILAMENT_ITEM_ID)],
            &kw,
            None,
        )
        .await
        .expect("AbyssalEntranceDeployment");
    let trace_id = keyval_i64(&deploy, "traceBallID").expect("traceBallID");

    let enter = ctx
        .dispatch_rpc(
            &mut sess,
            "abyssalMgr",
            None,
            "AbyssalEntranceGateActivation",
            &[PyRep::Integer(trace_id)],
            &kw,
            None,
        )
        .await
        .expect("AbyssalEntranceGateActivation");
    let pocket_id = keyval_i64(&enter, "pocketID").expect("pocketID");
    let mut room_npcs = keyval_i64_list(&enter, "npcBallIDs");
    assert_eq!(sess.solar_system_id, Some(SYS_ABYSS));
    let expected_abyss_sde = ctx
        .sde
        .solar_systems
        .get(&SYS_ABYSS)
        .expect("32000001 in SDE");
    assert_eq!(sess.constellation_id, Some(expected_abyss_sde.constellation_id));
    assert_eq!(sess.region_id, Some(expected_abyss_sde.region_id));

    ctx.dispatch_rpc(
        &mut sess,
        "abyssalMgr",
        None,
        "ClientIsReady",
        &[],
        &kw,
        None,
    )
    .await
    .expect("ClientIsReady");

    let abyss_handle = ctx.systems.get_or_spawn(SYS_ABYSS);
    assert!(
        abyss_handle.get_ball_snapshot(SHIP_ID).await.ok().flatten().is_some(),
        "Ship ball must be spawned in Abyssal system 32000001"
    );

    for rm in 1..=3_i64 {
        for npc_id in room_npcs.drain(..) {
            let _ = ctx
                .dispatch_rpc(
                    &mut sess,
                    "slash",
                    None,
                    "SlashCmd",
                    &[PyRep::String(format!("/despawn {npc_id}"))],
                    &kw,
                    None,
                )
                .await;
        }
        let _ = ctx
            .dispatch_rpc(
                &mut sess,
                "slash",
                None,
                "SlashCmd",
                &[PyRep::String("/heal".into())],
                &kw,
                None,
            )
            .await;
        let gate_id = 1_895_000_000 + pocket_id * 10 + rm;
        move_ship_near_gate(&ctx, SYS_ABYSS, SHIP_ID, gate_id).await;
        if rm < 3 {
            let gate_rep = ctx
                .dispatch_rpc(
                    &mut sess,
                    "abyssalMgr",
                    None,
                    "AbyssalGateActivation",
                    &[PyRep::Integer(gate_id)],
                    &kw,
                    None,
                )
                .await
                .expect("AbyssalGateActivation");
            room_npcs = keyval_i64_list(&gate_rep, "npcBallIDs");
        } else {
            ctx.dispatch_rpc(
                &mut sess,
                "abyssalMgr",
                None,
                "AbyssalEndGateActivation",
                &[PyRep::Integer(gate_id)],
                &kw,
                None,
            )
            .await
            .expect("AbyssalEndGateActivation");
        }
    }

    assert_eq!(sess.solar_system_id, Some(SYS_JITA));
    assert_eq!(sess.constellation_id, Some(20_000_020));
    assert_eq!(sess.region_id, Some(10_000_002));
    assert!(
        jita_handle.get_ball_snapshot(SHIP_ID).await.ok().flatten().is_some(),
        "Ship ball must be restored in Jita system 30000142 after Abyssal exit"
    );
}

#[tokio::test]
async fn capsule_inventory_is_accessible_after_ship_loss() {
    let ctx = ServerContext::new();
    let mut sess = SessionState::new(9_302);
    let kw = HashMap::new();
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    sess.role = u64::MAX;

    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(PILOT1)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    if sess.station_id.is_some() {
        ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
            .await
            .expect("Undock");
    }

    let sys_id = sess.solar_system_id.expect("solar_system_id") as i64;
    let bound = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "MachoBindObject",
            &[PyRep::Integer(sys_id)],
            &kw,
            None,
        )
        .await
        .expect("MachoBindObject invbroker");
    let PyRep::Tuple(parts) = bound else {
        panic!("MachoBindObject returns tuple, got {bound:?}");
    };
    let inv_mgr_id = parts[0].as_i64().expect("bound invbroker id") as u64;

    let old_ship_id = sess.ship_id.expect("initial ship_id");
    ctx.dispatch_rpc(&mut sess, "ship", None, "SelfDestruct", &[], &kw, None)
        .await
        .expect("SelfDestruct");
    ctx.dispatch_rpc(
        &mut sess,
        "timeControl",
        None,
        "AdvanceClock",
        &[PyRep::Integer(125)],
        &kw,
        None,
    )
    .await
    .expect("AdvanceClock");
    ctx.sync_session(&mut sess);

    let cap_id = sess.ship_id.expect("capsule ship_id after self-destruct");
    let cap_type = ctx.db.get_item(cap_id).expect("capsule item").type_id;
    assert_eq!(cap_type, 670, "expected Capsule typeID 670");

    let inv_res = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            Some(inv_mgr_id),
            "GetInventoryFromId",
            &[PyRep::Integer(cap_id), PyRep::Integer(1)],
            &kw,
            None,
        )
        .await;
    assert!(
        inv_res.is_ok(),
        "Capsule inventory must be accessible for godma priming after ship loss, got: {inv_res:?}"
    );

    let destroyed_inv_err = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            Some(inv_mgr_id),
            "GetInventoryFromId",
            &[PyRep::Integer(old_ship_id), PyRep::Integer(0)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(
        destroyed_inv_err.code, "FakeItemNotFound",
        "Destroyed ship GetInventoryFromId must raise FakeItemNotFound so V24.01 HoldTracker handles ship loss cleanly"
    );
}

#[tokio::test]
async fn reentrant_db_read_snapshot_and_container_list_never_deadlock() {
    let ctx = ServerContext::new();
    let nested = ctx.db.with_snapshot(|outer| {
        ctx.db.with_snapshot(|inner| {
            assert_eq!(outer.wal_seq, inner.wal_seq);
            ctx.db.try_with_snapshot(|third| third.wal_seq)
        })
    });
    assert!(nested.is_some());

    let mut sess = SessionState::default();
    let kw = HashMap::new();
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = ctx.db.with_snapshot(|s| {
        s.characters
            .values()
            .find(|c| Some(c.user_id) == sess.user_id)
            .map(|c| c.character_id)
            .expect("demo char")
    });
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");
    let PyRep::Tuple(bound) = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "MachoBindObject",
            &[PyRep::Integer(60_003_760), PyRep::None],
            &kw,
            None,
        )
        .await
        .expect("MachoBindObject")
    else {
        panic!("expected tuple");
    };
    let inv_id = bound[0].as_i64().unwrap() as u64;
    let listed = ctx
        .dispatch_rpc(&mut sess, "invbroker", Some(inv_id), "List", &[], &kw, None)
        .await
        .expect("invbroker.List");
    assert!(matches!(listed, PyRep::Object { .. } | PyRep::ObjectEx { .. } | PyRep::List(_) | PyRep::Tuple(_)));
}

