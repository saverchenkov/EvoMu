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
const SYS_JITA: u32 = 30_000_002; // Lashesih (0.8 high-sec, allows T1 filaments cleanly)
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
    // parity: feat:pve.abyssal-pocket, rpc:abyssalMgr.AbyssalEntranceGateActivation, rpc:abyssalMgr.AbyssalGateActivation, rpc:abyssalMgr.AbyssalEndGateActivation, rpc:abyssalMgr.ClientIsReady
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let fil_item_id = 1_300_000_101;
    let mut sess = setup_pilot_in_jita(&ctx, 9_501, "demo", "demo-secret", PILOT_ID, fil_item_id).await;

    // Verify negative checks outside of abyssal space
    let ready_outside = ctx
        .dispatch_rpc(&mut sess, "abyssalMgr", None, "ClientIsReady", &[], &kw, None)
        .await
        .expect_err("ClientIsReady outside abyss");
    assert_eq!(ready_outside.code, "NotInAbyssalSpace");

    let gate_outside = ctx
        .dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalGateActivation", &[], &kw, None)
        .await
        .expect_err("AbyssalGateActivation outside abyss");
    assert_eq!(gate_outside.code, "NotInAbyssalSpace");

    let end_outside = ctx
        .dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEndGateActivation", &[], &kw, None)
        .await
        .expect_err("AbyssalEndGateActivation outside abyss");
    assert_eq!(end_outside.code, "NotInAbyssalSpace");

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

    // BF-02: Verify that activating entrance while docked fails and does NOT delete trace
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_ID) {
            ch.station_id = Some(60_003_760);
        }
    });
    sess.station_id = Some(60_003_760);
    let docked_err = ctx
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
        .expect_err("Docked activation rejected");
    assert_eq!(docked_err.code, "ShipDocked");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_ID) {
            ch.station_id = None;
        }
    });
    sess.station_id = None;

    // Verify trace still exists in registry after rejected activation
    let trace_persists = eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
        reg.abyssal_traces.contains_key(&trace_ball_id)
    });
    assert!(trace_persists, "Trace preserved after rejected validation");

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
    let abyss_handle = ctx.systems.get_or_spawn(32_000_001);
    let abyss_ball = abyss_handle
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

    // BF-03: Verify intermediate gate and end gate are locked while Room 1 NPCs are alive
    let r1_npcs: Vec<i64> = enter_rep
        .dict_get("npcBallIDs")
        .and_then(|v| match v {
            PyRep::List(l) => Some(l.iter().filter_map(|x| x.as_i64()).collect()),
            _ => None,
        })
        .unwrap_or_default();
    assert!(!r1_npcs.is_empty(), "Room 1 has spawned NPC balls");

    let locked_r1 = ctx
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
        .expect_err("Gate activation blocked while Room 1 NPCs alive");
    assert_eq!(locked_r1.code, "GateLocked");

    let early_exit_r1 = ctx
        .dispatch_rpc(
            &mut sess,
            "abyssalMgr",
            None,
            "AbyssalEndGateActivation",
            &[PyRep::Integer(1_895_000_001)],
            &kw,
            None,
        )
        .await
        .expect_err("End gate rejected in Room 1");
    assert_eq!(early_exit_r1.code, "NotInFinalRoom");

    // Room 1 Bioadaptive Cache loot; clear Room 1 NPCs through real combat damage
    // (Calm filament = tier 1 -> 2 Survey Databases + 1 filament)
    crate::abyssal_room_combat::assert_room_cache_loot(&ctx, PILOT_ID, 0, 2, FILAMENT_TYPE).await;
    assert_eq!(r1_npcs.len(), 2);
    crate::abyssal_room_combat::destroy_room_npcs_via_damage(&ctx, PILOT_ID, 0).await;

    // 4. Activate intermediate acceleration gate (Room 1 -> Room 2) now that Room 1 is clear
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

    // BF-03: Verify end gate rejected in Room 2, and gate locked while Room 2 NPCs alive
    let early_exit_r2 = ctx
        .dispatch_rpc(
            &mut sess,
            "abyssalMgr",
            None,
            "AbyssalEndGateActivation",
            &[PyRep::Integer(1_895_000_002)],
            &kw,
            None,
        )
        .await
        .expect_err("End gate rejected in Room 2");
    assert_eq!(early_exit_r2.code, "NotInFinalRoom");

    let locked_r2 = ctx
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
        .expect_err("Gate activation blocked while Room 2 NPCs alive");
    assert_eq!(locked_r2.code, "GateLocked");

    // Room 2 NPCs (real SDE types + CombatState) and Bioadaptive Cache; clear via combat damage
    crate::abyssal_room_combat::assert_room_cache_loot(&ctx, PILOT_ID, 1, 2, FILAMENT_TYPE).await;
    crate::abyssal_room_combat::destroy_room_npcs_via_damage(&ctx, PILOT_ID, 1).await;

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

    // BF-03: End gate in Room 3 is locked while Room 3 NPCs are alive
    let locked_r3 = ctx
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
        .expect_err("End gate locked while Room 3 NPCs alive");
    assert_eq!(locked_r3.code, "GateLocked");

    // Room 3 NPCs and Bioadaptive Cache; clear via combat damage
    crate::abyssal_room_combat::assert_room_cache_loot(&ctx, PILOT_ID, 2, 2, FILAMENT_TYPE).await;
    crate::abyssal_room_combat::destroy_room_npcs_via_damage(&ctx, PILOT_ID, 2).await;

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
    // P1-02: verify live ballpark position was recorded and used on exit (+1000.0 offset)
    assert_eq!(jita_ball.unwrap().1.position, glam::DVec3::new(11_000.0, 1_000.0, 1_000.0));
    let pid = gate1_rep.dict_get("pocketID").and_then(|v| v.as_i64()).expect("pocketID");
    assert!(abyss_handle.get_ball_snapshot(1_895_000_000 + pid * 10 + 3).await.unwrap().is_none(), "Room 3 gate marker removed on exit");

    // 7. Enter a second Abyssal pocket and exceed the 20-minute (1,200s) hard collapse timer:
    //    both ship (621) and capsule (670) are destroyed, Bioadaptive Cache items and NPCs are cleaned up,
    //    and pilot respawns at home station.
    let deploy2 = ctx.dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(fil_item_id)], &kw, None).await.expect("deploy 2");
    let trace2 = deploy2.dict_get("traceBallID").and_then(|v| v.as_i64()).expect("trace2");
    let enter2 = ctx.dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(trace2)], &kw, None).await.expect("enter 2");
    let pid2 = enter2.dict_get("pocketID").and_then(|v| v.as_i64()).expect("pocketID 2");
    let cache2_r1 = 1_896_000_000 + pid2 * 10 + 1;
    let npc2_r1 = pid2 * 100 + 11;
    assert!(ctx.db.with_snapshot(|s| s.items.contains_key(&cache2_r1)), "Room 1 cache exists before collapse");
    assert!(eve_server::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| rt.npc_entities.contains_key(&npc2_r1)));
    ctx.advance_secs(1_205);
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&SHIP_ID), "Ship destroyed on 20-minute pocket collapse");
        assert!(!snap.items.contains_key(&cache2_r1), "Bioadaptive Cache item removed on collapse");
        let ch = snap.characters.get(&PILOT_ID).expect("pilot");
        assert_eq!(ch.station_id, Some(60_003_760), "Pilot respawned at home station after collapse");
        assert!(snap.killmails.values().any(|k| k.victim_character_id == PILOT_ID && k.ship_type_id == 621), "Ship killmail recorded on collapse");
        assert!(snap.killmails.values().any(|k| k.victim_character_id == PILOT_ID && k.ship_type_id == 670), "Capsule killmail recorded on collapse");
    });
    assert!(!eve_server::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| rt.npc_entities.contains_key(&npc2_r1)), "Pocket NPCs removed on collapse");
}

#[tokio::test]
async fn test_void_space_jump_lifecycle_and_gate_activation() {
    // parity: rpc:voidSpaceMgr.ClientIsReady, rpc:voidSpaceMgr.VoidSpacePlayerJump, rpc:voidSpaceMgr.VoidSpaceEndGateActivation
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let fil_item_id = 1_300_000_201;
    let mut sess = setup_pilot_in_jita(&ctx, 9_502, "demo", "demo-secret", PILOT_ID, fil_item_id).await;

    // 1. Negative checks:
    let empty_err = ctx
        .dispatch_rpc(&mut sess, "voidSpaceMgr", None, "VoidSpacePlayerJump", &[], &kw, None)
        .await
        .expect_err("VoidSpacePlayerJump with empty args");
    assert_eq!(empty_err.code, "MissingRequiredArgument");

    let invalid_target = ctx
        .dispatch_rpc(
            &mut sess,
            "voidSpaceMgr",
            None,
            "VoidSpacePlayerJump",
            &[PyRep::Integer(999_999)],
            &kw,
            None,
        )
        .await
        .expect_err("VoidSpacePlayerJump with invalid target");
    assert_eq!(invalid_target.code, "InvalidTarget");

    // P1-01 checks: (a) reject self, (b) reject non-gate ball, (c) reject non-filament & non-cargo items
    let r_self = ctx.dispatch_rpc(&mut sess, "voidSpaceMgr", None, "VoidSpacePlayerJump", &[PyRep::Integer(SHIP_ID)], &kw, None).await.unwrap_err();
    assert_eq!(r_self.code, "InvalidTarget");
    let non_gate = eve_destiny::Ball { id: 700_002, type_id: 621, position: glam::DVec3::new(10_100.0, 0.0, 0.0), velocity: glam::DVec3::ZERO, max_velocity: 0.0, inertia: 1.0, mass: 1e6, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop };
    let _ = ctx.systems.get_or_spawn(SYS_JITA).spawn_ball(non_gate).await;
    let r_ball = ctx.dispatch_rpc(&mut sess, "voidSpaceMgr", None, "VoidSpacePlayerJump", &[PyRep::Integer(700_002)], &kw, None).await.unwrap_err();
    assert_eq!(r_ball.code, "InvalidTarget");
    ctx.systems.get_or_spawn(SYS_JITA).try_remove_ball(700_002);
    // R-VOID-P1-03: Regular stargate (type 16) within 5km is rejected
    let norm_gate = eve_destiny::Ball { id: 700_003, type_id: 16, position: glam::DVec3::new(10_200.0, 0.0, 0.0), velocity: glam::DVec3::ZERO, max_velocity: 0.0, inertia: 1.0, mass: 1e7, radius: 100.0, speed_fraction: 0.0, mode: MovementMode::Stop };
    let _ = ctx.systems.get_or_spawn(SYS_JITA).spawn_ball(norm_gate).await;
    let r_gate = ctx.dispatch_rpc(&mut sess, "voidSpaceMgr", None, "VoidSpacePlayerJump", &[PyRep::Integer(700_003)], &kw, None).await.unwrap_err();
    assert_eq!(r_gate.code, "InvalidTarget");
    ctx.systems.get_or_spawn(SYS_JITA).try_remove_ball(700_003);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(1_300_000_999, eve_db::InventoryItemRecord { item_id: 1_300_000_999, type_id: 621, owner_id: PILOT_ID, location_id: SHIP_ID, flag: InventoryFlag::Cargo, quantity: 1, ..Default::default() });
        snap.items.insert(1_300_000_998, eve_db::InventoryItemRecord { item_id: 1_300_000_998, type_id: FILAMENT_TYPE, owner_id: PILOT_ID, location_id: SHIP_ID, flag: InventoryFlag::Hangar, quantity: 1, ..Default::default() });
    });
    let r_fil = ctx.dispatch_rpc(&mut sess, "voidSpaceMgr", None, "VoidSpacePlayerJump", &[PyRep::Integer(1_300_000_999)], &kw, None).await.unwrap_err();
    assert_eq!(r_fil.code, "InvalidTarget");
    let r_flg = ctx.dispatch_rpc(&mut sess, "voidSpaceMgr", None, "VoidSpacePlayerJump", &[PyRep::Integer(1_300_000_998)], &kw, None).await.unwrap_err();
    assert_eq!(r_flg.code, "InvalidTarget");

    let exit_outside = ctx
        .dispatch_rpc(&mut sess, "voidSpaceMgr", None, "VoidSpaceEndGateActivation", &[], &kw, None)
        .await
        .expect_err("VoidSpaceEndGateActivation outside void space");
    assert_eq!(exit_outside.code, "NotInVoidSpace");

    // Spawn Void Space gate ball in Jita within activation distance of ship (ship is at (10_000, 0, 0))
    let jita_handle = ctx.systems.get_or_spawn(SYS_JITA);
    let gate_ball = eve_destiny::Ball {
        id: 700_001,
        type_id: 47_956,
        position: glam::DVec3::new(10_500.0, 0.0, 0.0),
        velocity: glam::DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 1.0,
        mass: 10_000_000.0,
        radius: 100.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = jita_handle.spawn_ball(gate_ball).await;

    // 2. Perform Void Space jump via gate
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

    // BF-01: Verify docked pilot cannot activate Void Space exit gate
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_ID) {
            ch.station_id = Some(60_003_760);
        }
    });
    sess.station_id = Some(60_003_760);
    let docked_exit = ctx
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
        .expect_err("Docked exit rejected");
    assert_eq!(docked_exit.code, "ShipDocked");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_ID) {
            ch.station_id = None;
        }
    });
    sess.station_id = None;

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

    // Verify ship ball restored to Jita destiny engine near original position
    let jita_ball = ctx
        .systems
        .get_or_spawn(SYS_JITA)
        .get_ball_snapshot(SHIP_ID)
        .await
        .ok()
        .flatten();
    assert!(jita_ball.is_some(), "Ship ball restored to Jita destiny engine");
    let pos = jita_ball.unwrap().1.position;
    assert!((pos.x - 11_000.0).abs() < 1.0, "Position restored near entry coordinates");

    // 5. Also verify jumping to Void Space directly using cargo filament consumption
    let cargo_jump = ctx
        .dispatch_rpc(
            &mut sess,
            "voidSpaceMgr",
            None,
            "VoidSpacePlayerJump",
            &[PyRep::Integer(fil_item_id)],
            &kw,
            None,
        )
        .await
        .expect("VoidSpacePlayerJump via cargo filament");
    assert_eq!(
        cargo_jump.dict_get("solarSystemID").and_then(|v| v.as_i64()),
        Some(32_000_002)
    );
    // Filament quantity reduced from 3 to 2
    let rem_fil = ctx
        .db
        .with_snapshot(|snap| snap.items.get(&fil_item_id).map(|i| i.quantity).unwrap_or(0));
    assert_eq!(rem_fil, 2, "Cargo filament consumed on jump");

    // Exit back to Jita
    let exit2 = ctx
        .dispatch_rpc(
            &mut sess,
            "voidSpaceMgr",
            None,
            "VoidSpaceEndGateActivation",
            &[PyRep::Integer(888_002)],
            &kw,
            None,
        )
        .await
        .expect("VoidSpaceEndGateActivation after cargo jump");
    assert_eq!(
        exit2.dict_get("solarSystemID").and_then(|v| v.as_i64()),
        Some(SYS_JITA as i64)
    );
}
