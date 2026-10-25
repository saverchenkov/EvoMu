//! Integration test suite for Abyssal Deadspace & Void Space RPC endpoints.
//!
//! Validates full live-server parity for:
//! - `abyssalMgr.AbyssalEntranceGateActivation`
//! - `abyssalMgr.AbyssalGateActivation`
//! - `abyssalMgr.AbyssalEndGateActivation`
//! - `abyssalMgr.ClientIsReady`
//! - `voidSpaceMgr.ClientIsReady`
//! - `voidSpaceMgr.VoidSpacePlayerJump`
//! - `voidSpaceMgr.VoidSpaceEndGateActivation`

use crate::common;
use eve_db::InventoryFlag;
use eve_destiny::MovementMode;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

const PILOT_ID: i64 = 90_000_001;
const SYS_JITA: u32 = 30_000_142;
const SHIP_ID: i64 = 100_000_001;
const FILAMENT_TYPE: u32 = 47763; // Calm Firestorm Filament (group 1979)

async fn setup_pilot_in_jita(
    ctx: &ServerContext,
    session_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
    filament_item_id: i64,
) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
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

    // Configure pilot in space in Jita
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = SYS_JITA;
            ch.station_id = None;
            ch.structure_id = None;
        }
        // Seed Cruiser ship (type 621 = Caracal, Cruiser group 26)
        snap.items.insert(
            SHIP_ID,
            eve_db::InventoryItemRecord {
                item_id: SHIP_ID,
                type_id: 621,
                owner_id: char_id,
                location_id: SYS_JITA as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        // Seed filament into cargo
        snap.items.insert(
            filament_item_id,
            eve_db::InventoryItemRecord {
                item_id: filament_item_id,
                type_id: FILAMENT_TYPE,
                owner_id: char_id,
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

    // Spawn ship ball in Jita destiny engine
    let sys_handle = ctx.systems.get_or_spawn(SYS_JITA);
    let ball = eve_destiny::Ball {
        id: SHIP_ID,
        type_id: 621,
        position: glam::DVec3::new(10_000.0, 0.0, 0.0),
        velocity: glam::DVec3::ZERO,
        max_velocity: 250.0,
        inertia: 0.5,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let cs = ctx.build_ship_combat_state(char_id, SHIP_ID, 621);
    let _ = sys_handle.spawn_ball_with_combat(ball, cs).await;

    sess
}

#[tokio::test]
async fn test_abyssal_deadspace_lifecycle_and_gate_progression() {
    // parity: rpc:abyssalMgr.AbyssalEntranceGateActivation, rpc:abyssalMgr.AbyssalGateActivation, rpc:abyssalMgr.AbyssalEndGateActivation, rpc:abyssalMgr.ClientIsReady
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let fil_item_id = 1_300_000_101;
    let mut sess = setup_pilot_in_jita(&ctx, 9_501, "demo", "demo-secret", PILOT_ID, fil_item_id).await;

    // Verify negative checks outside of abyssal space
    let ready_outside = ctx
        .dispatch_rpc(&mut sess, "abyssalMgr", None, "ClientIsReady", &[], &kw, None)
        .await
        .expect_err("ClientIsReady outside abyss");
    assert_eq!(ready_outside.code, "UnimplementedRPC");

    let gate_outside = ctx
        .dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalGateActivation", &[], &kw, None)
        .await
        .expect_err("AbyssalGateActivation outside abyss");
    assert_eq!(gate_outside.code, "UnimplementedRPC");

    let end_outside = ctx
        .dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEndGateActivation", &[], &kw, None)
        .await
        .expect_err("AbyssalEndGateActivation outside abyss");
    assert_eq!(end_outside.code, "UnimplementedRPC");

    // 1. Deploy filament in Jita -> spawns entrance trace ball
    let deploy_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "abyssalMgr",
            None,
            "AbyssalEntranceDeployment",
            &[PyRep::Integer(fil_item_id)],
            &kw,
            None,
        )
        .await
        .expect("AbyssalEntranceDeployment");
    let trace_ball_id = deploy_rep
        .dict_get("traceBallID")
        .and_then(|v| v.as_i64())
        .expect("traceBallID in deploy result");

    // Verify filament stack reduced from 3 to 2
    let fil_qty = ctx
        .db
        .with_snapshot(|snap| snap.items.get(&fil_item_id).map(|i| i.quantity).unwrap_or(0));
    assert_eq!(fil_qty, 2, "One filament deducted on deployment");

    // 2. Activate entrance gate into Abyssal Deadspace (Room 1)
    let enter_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "abyssalMgr",
            None,
            "AbyssalEntranceGateActivation",
            &[PyRep::Integer(trace_ball_id)],
            &kw,
            None,
        )
        .await
        .expect("AbyssalEntranceGateActivation");

    let abyssal_sys = enter_rep
        .dict_get("solarSystemID")
        .and_then(|v| v.as_i64())
        .expect("solarSystemID in enter result") as u32;
    assert_eq!(abyssal_sys, 32_000_001);
    assert_eq!(sess.solar_system_id, Some(32_000_001));

    let room1 = enter_rep
        .dict_get("currentRoom")
        .and_then(|v| v.as_i64())
        .expect("currentRoom");
    assert_eq!(room1, 1);

    // Verify ship ball was spawned in Abyssal system destiny engine
    let abyss_ball = ctx
        .systems
        .get_or_spawn(32_000_001)
        .get_ball_snapshot(SHIP_ID)
        .await
        .ok()
        .flatten();
    assert!(abyss_ball.is_some(), "Ship ball present in Abyssal Deadspace system");

    // 3. ClientIsReady acknowledgment when scene loads in Abyssal Deadspace
    let ready_rep = ctx
        .dispatch_rpc(&mut sess, "abyssalMgr", None, "ClientIsReady", &[], &kw, None)
        .await
        .expect("ClientIsReady");
    assert_eq!(
        ready_rep.dict_get("ready").and_then(|v| v.as_bool()),
        Some(true)
    );

    // 4. Activate intermediate acceleration gate (Room 1 -> Room 2)
    let gate1_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "abyssalMgr",
            None,
            "AbyssalGateActivation",
            &[PyRep::Integer(1_895_000_001)],
            &kw,
            None,
        )
        .await
        .expect("AbyssalGateActivation R1->R2");
    let room2 = gate1_rep
        .dict_get("room")
        .and_then(|v| v.as_i64())
        .expect("room");
    assert_eq!(room2, 2);
    assert_eq!(
        gate1_rep.dict_get("gateActivated").and_then(|v| v.as_bool()),
        Some(true)
    );

    // 5. Activate intermediate acceleration gate (Room 2 -> Room 3)
    let gate2_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "abyssalMgr",
            None,
            "AbyssalGateActivation",
            &[PyRep::Integer(1_895_000_002)],
            &kw,
            None,
        )
        .await
        .expect("AbyssalGateActivation R2->R3");
    let room3 = gate2_rep
        .dict_get("room")
        .and_then(|v| v.as_i64())
        .expect("room");
    assert_eq!(room3, 3);
    assert_eq!(
        gate2_rep.dict_get("gateActivated").and_then(|v| v.as_bool()),
        Some(true)
    );

    // 6. Calling intermediate gate in Room 3 is blocked (must use exit conduit)
    let extra_gate = ctx
        .dispatch_rpc(
            &mut sess,
            "abyssalMgr",
            None,
            "AbyssalGateActivation",
            &[PyRep::Integer(1_895_000_003)],
            &kw,
            None,
        )
        .await
        .expect_err("Gate activation in room 3 returns error");
    assert_eq!(extra_gate.code, "AlreadyInFinalRoom");

    // 7. Activate exit gate conduit from Room 3 back to Jita normal space
    let exit_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "abyssalMgr",
            None,
            "AbyssalEndGateActivation",
            &[PyRep::Integer(1_895_000_003)],
            &kw,
            None,
        )
        .await
        .expect("AbyssalEndGateActivation");
    let returned_sys = exit_rep
        .dict_get("solarSystemID")
        .and_then(|v| v.as_i64())
        .expect("solarSystemID");
    assert_eq!(returned_sys, SYS_JITA as i64);
    assert_eq!(
        exit_rep.dict_get("exitedAbyssalSpace").and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(sess.solar_system_id, Some(SYS_JITA));

    // Verify ship ball returned to Jita destiny engine
    let jita_ball = ctx
        .systems
        .get_or_spawn(SYS_JITA)
        .get_ball_snapshot(SHIP_ID)
        .await
        .ok()
        .flatten();
    assert!(jita_ball.is_some(), "Ship ball restored to Jita destiny engine");
}

#[tokio::test]
async fn test_void_space_jump_lifecycle_and_gate_activation() {
    // parity: rpc:voidSpaceMgr.ClientIsReady, rpc:voidSpaceMgr.VoidSpacePlayerJump, rpc:voidSpaceMgr.VoidSpaceEndGateActivation
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let fil_item_id = 1_300_000_201;
    let mut sess = setup_pilot_in_jita(&ctx, 9_502, "demo", "demo-secret", PILOT_ID, fil_item_id).await;

    // 1. Negative check: VoidSpacePlayerJump with empty arguments returns UnimplementedRPC
    let empty_err = ctx
        .dispatch_rpc(&mut sess, "voidSpaceMgr", None, "VoidSpacePlayerJump", &[], &kw, None)
        .await
        .expect_err("VoidSpacePlayerJump with empty args");
    assert_eq!(empty_err.code, "UnimplementedRPC");

    // Negative check: VoidSpaceEndGateActivation outside void space returns error
    let exit_outside = ctx
        .dispatch_rpc(&mut sess, "voidSpaceMgr", None, "VoidSpaceEndGateActivation", &[], &kw, None)
        .await
        .expect_err("VoidSpaceEndGateActivation outside void space");
    assert_eq!(exit_outside.code, "NotInVoidSpace");

    // 2. Perform Void Space jump
    let jump_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "voidSpaceMgr",
            None,
            "VoidSpacePlayerJump",
            &[PyRep::Integer(700_001), PyRep::Integer(9_001)],
            &kw,
            None,
        )
        .await
        .expect("VoidSpacePlayerJump");
    let void_sys = jump_rep
        .dict_get("solarSystemID")
        .and_then(|v| v.as_i64())
        .expect("solarSystemID");
    assert_eq!(void_sys, 32_000_002);
    assert_eq!(
        jump_rep.dict_get("voidSpaceJump").and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(sess.solar_system_id, Some(32_000_002));

    // Verify ship ball spawned in Void Space destiny engine
    let void_ball = ctx
        .systems
        .get_or_spawn(32_000_002)
        .get_ball_snapshot(SHIP_ID)
        .await
        .ok()
        .flatten();
    assert!(void_ball.is_some(), "Ship ball present in Void Space destiny engine");

    // 3. ClientIsReady acknowledgment when scene loads in Void Space
    let ready_rep = ctx
        .dispatch_rpc(&mut sess, "voidSpaceMgr", None, "ClientIsReady", &[], &kw, None)
        .await
        .expect("ClientIsReady in Void Space");
    assert_eq!(
        ready_rep.dict_get("ready").and_then(|v| v.as_bool()),
        Some(true)
    );

    // 4. Activate Void Space exit gate to return to origin system (Jita)
    let exit_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "voidSpaceMgr",
            None,
            "VoidSpaceEndGateActivation",
            &[PyRep::Integer(888_001)],
            &kw,
            None,
        )
        .await
        .expect("VoidSpaceEndGateActivation");
    let returned_sys = exit_rep
        .dict_get("solarSystemID")
        .and_then(|v| v.as_i64())
        .expect("solarSystemID");
    assert_eq!(returned_sys, SYS_JITA as i64);
    assert_eq!(
        exit_rep.dict_get("exitedVoidSpace").and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(sess.solar_system_id, Some(SYS_JITA));

    // Verify ship ball restored to Jita destiny engine
    let jita_ball = ctx
        .systems
        .get_or_spawn(SYS_JITA)
        .get_ball_snapshot(SHIP_ID)
        .await
        .ok()
        .flatten();
    assert!(jita_ball.is_some(), "Ship ball restored to Jita destiny engine");
}
