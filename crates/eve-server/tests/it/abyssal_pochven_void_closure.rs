//! Integration tests for Void Space and Pochven Travel mechanics.
//!
//! Covers:
//! - `feat:navigation.filament-void`: `voidSpaceMgr.VoidSpacePlayerJump enters void space content.`
//! - `feat:navigation.pochven-travel`: `Pochven conduits/filaments and internal gates follow rules.`

#![forbid(unsafe_code)]

use crate::common;
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

const POCHVEN_SYS_ID: u32 = 30_000_157; // Otela (Pochven)
const KINO_SYS_ID: u32 = 30_001_372;    // Kino (Pochven Home System)
const JITA_SYS_ID: u32 = 30_000_142;    // Jita (Empire K-space)
const VOID_SPACE_SYS_ID: u32 = 32_000_002; // Void Space pocket
const TRIGLAVIAN_FACTION_ID: i64 = 500_026;

#[tokio::test]
async fn test_void_space_player_jump_and_gate_activation() {
    // parity: feat:navigation.filament-void
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_601_i64;
    let filament_item_id = 200_000_601_i64;

    let mut sess = SessionState::new(886_001);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
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
    ctx.advance_secs(11);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = JITA_SYS_ID;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            eve_db::InventoryItemRecord {
                item_id: ship_id,
                type_id: 621, // Cerberus cruiser
                owner_id: char_id,
                location_id: JITA_SYS_ID as i64,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    sess.station_id = None;
    sess.structure_id = None;
    sess.solar_system_id = Some(JITA_SYS_ID);
    sess.ship_id = Some(ship_id);

    // 1. Missing arguments rejected
    let missing_err = ctx
        .dispatch_rpc(&mut sess, "voidSpaceMgr", None, "VoidSpacePlayerJump", &[], &kw, None)
        .await
        .expect_err("VoidSpacePlayerJump with no args must fail");
    assert_eq!(missing_err.code, "MissingRequiredArgument");

    // 2. Docked pilot rejected
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(60_003_760);
        }
    });
    sess.station_id = Some(60_003_760);
    let docked_err = ctx
        .dispatch_rpc(
            &mut sess,
            "voidSpaceMgr",
            None,
            "VoidSpacePlayerJump",
            &[PyRep::Integer(filament_item_id)],
            &kw,
            None,
        )
        .await
        .expect_err("VoidSpacePlayerJump while docked must fail");
    assert_eq!(docked_err.code, "ShipDocked");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
        }
    });
    sess.station_id = None;

    // 3. Undocked pilot setup
    let jita_handle = ctx.systems.get_or_spawn(JITA_SYS_ID);
    let _ = jita_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 621,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            radius: 50.0,
            mass: 1_000_000.0,
            mode: MovementMode::Stop,
            speed_fraction: 0.0,
            max_velocity: 250.0,
            inertia: 0.5,
        })
        .await;

    // 4. Invalid target / own ship rejected
    let self_err = ctx
        .dispatch_rpc(
            &mut sess,
            "voidSpaceMgr",
            None,
            "VoidSpacePlayerJump",
            &[PyRep::Integer(ship_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Targeting own ship must fail");
    assert_eq!(self_err.code, "InvalidTarget");

    // 5. Jump via cargo filament: seed filament in active ship cargo
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            filament_item_id,
            eve_db::InventoryItemRecord {
                item_id: filament_item_id,
                type_id: 47_763, // Calm Firestorm Filament (Abyssal/Void filament)
                owner_id: char_id,
                location_id: ship_id,
                flag: eve_db::InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    let jump_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "voidSpaceMgr",
            None,
            "VoidSpacePlayerJump",
            &[PyRep::Integer(filament_item_id)],
            &kw,
            None,
        )
        .await
        .expect("VoidSpacePlayerJump via cargo filament");

    assert_eq!(
        jump_rep.dict_get("solarSystemID"),
        Some(&PyRep::Integer(VOID_SPACE_SYS_ID as i64))
    );
    assert_eq!(
        jump_rep.dict_get("originSolarSystemID"),
        Some(&PyRep::Integer(JITA_SYS_ID as i64))
    );
    assert_eq!(
        jump_rep.dict_get("voidSpaceJump"),
        Some(&PyRep::Bool(true))
    );

    // Verify filament consumed from cargo
    assert!(
        ctx.db.with_snapshot(|snap| !snap.items.contains_key(&filament_item_id)),
        "Filament must be consumed on jump"
    );

    // Verify session updated to Void Space
    assert_eq!(sess.solar_system_id, Some(VOID_SPACE_SYS_ID));

    // Verify ship ball and exit gates spawned in Void Space
    let void_handle = ctx.systems.get_or_spawn(VOID_SPACE_SYS_ID);
    assert!(
        void_handle.get_ball_snapshot(ship_id).await.unwrap().is_some(),
        "Ship ball must exist in Void Space"
    );
    assert!(
        void_handle.get_ball_snapshot(888_001).await.unwrap().is_some(),
        "Exit gate 888001 must exist in Void Space"
    );

    // 6. ClientIsReady RPC
    let ready_rep = ctx
        .dispatch_rpc(&mut sess, "voidSpaceMgr", None, "ClientIsReady", &[], &kw, None)
        .await
        .expect("ClientIsReady");
    assert_eq!(ready_rep.dict_get("ready"), Some(&PyRep::Bool(true)));

    // 7. Exit gate activation returns to origin solar system
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
    assert_eq!(
        exit_rep.dict_get("solarSystemID"),
        Some(&PyRep::Integer(JITA_SYS_ID as i64))
    );
    assert_eq!(sess.solar_system_id, Some(JITA_SYS_ID));
}

#[tokio::test]
async fn test_pochven_travel_conduits_filaments_and_gates() {
    // parity: feat:navigation.pochven-travel
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_602_i64;

    let mut sess = SessionState::new(886_002);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
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
    ctx.advance_secs(11);

    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.station_id = None;
        ch.structure_id = None;
        ch.solar_system_id = POCHVEN_SYS_ID;
        ch.active_ship_id = ship_id;
        ch.standings.insert(TRIGLAVIAN_FACTION_ID, 0.5); // Low standing < 1.0
        snap.items.insert(
            ship_id,
            eve_db::InventoryItemRecord {
                item_id: ship_id,
                type_id: 621,
                owner_id: char_id,
                location_id: POCHVEN_SYS_ID as i64,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    sess.station_id = None;
    sess.structure_id = None;
    sess.solar_system_id = Some(POCHVEN_SYS_ID);
    sess.ship_id = Some(ship_id);

    let poch_handle = ctx.systems.get_or_spawn(POCHVEN_SYS_ID);
    let _ = poch_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 621,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            radius: 50.0,
            mass: 1_000_000.0,
            mode: MovementMode::Stop,
            speed_fraction: 0.0,
            max_velocity: 250.0,
            inertia: 0.5,
        })
        .await;

    // 1. Capital ship cannot jump Pochven stargate
    let dread_ship_id = 100_000_603_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            dread_ship_id,
            eve_db::InventoryItemRecord {
                item_id: dread_ship_id,
                type_id: 19720,
                owner_id: char_id,
                location_id: POCHVEN_SYS_ID as i64,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = dread_ship_id;
        }
    });
    sess.ship_id = Some(dread_ship_id);
    let _ = poch_handle
        .spawn_ball(Ball {
            id: dread_ship_id,
            type_id: 19720, // Revelation Dreadnought (group 485 capital)
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            radius: 500.0,
            mass: 1_200_000_000.0,
            mode: MovementMode::Stop,
            speed_fraction: 0.0,
            max_velocity: 100.0,
            inertia: 0.1,
        })
        .await;
    let poch_beyonce = ctx.bind_object(&mut sess, "beyonce", POCHVEN_SYS_ID as i64).unwrap();
    let cap_err = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(poch_beyonce),
            "CmdStargateJump",
            &[PyRep::Integer(50_016_520)],
            &kw,
            None,
        )
        .await
        .expect_err("Capital ship jumping Pochven gate must fail");
    assert_eq!(cap_err.code, "CapitalShipCannotJumpStargate");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = ship_id;
        }
    });
    sess.ship_id = Some(ship_id);

    // 2. Pochven Border / Internal conduit jump allowed with low standing
    let gate_nalvula = 50_016_518_i64; // Otela -> Nalvula (30001445)
    let gpos = ctx.sde.jumps.get(&gate_nalvula).map(|j| j.position).unwrap_or(DVec3::ZERO);
    let _ = poch_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 621,
            position: gpos + DVec3::new(100.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            radius: 50.0,
            mass: 1_000_000.0,
            mode: MovementMode::Stop,
            speed_fraction: 0.0,
            max_velocity: 250.0,
            inertia: 0.5,
        })
        .await;
    ctx.advance_secs(15);
    let jump_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(poch_beyonce),
            "CmdStargateJump",
            &[PyRep::Integer(gate_nalvula)],
            &kw,
            None,
        )
        .await
        .expect("Internal Pochven conduit allows standing < 6.0");
    assert_eq!(
        jump_rep.dict_get("solarSystemID").and_then(|v| v.as_i64()),
        Some(30_001_445)
    );

    // 3. Clade Home gate jump requires standing >= 6.0
    // Move ship back near Kino gate in Otela
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = POCHVEN_SYS_ID;
        }
    });
    sess.solar_system_id = Some(POCHVEN_SYS_ID);
    let home_gate = 50_016_520_i64; // Otela -> Kino (30001372)
    let hpos = ctx.sde.jumps.get(&home_gate).map(|j| j.position).unwrap_or(DVec3::ZERO);
    let _ = poch_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 621,
            position: hpos + DVec3::new(100.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            radius: 50.0,
            mass: 1_000_000.0,
            mode: MovementMode::Stop,
            speed_fraction: 0.0,
            max_velocity: 250.0,
            inertia: 0.5,
        })
        .await;
    ctx.advance_secs(15);
    let low_err = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(poch_beyonce),
            "CmdStargateJump",
            &[PyRep::Integer(home_gate)],
            &kw,
            None,
        )
        .await
        .expect_err("Pochven home gate jump with standing < 6.0 must fail");
    assert_eq!(low_err.code, "SystemCheck_JumpFailed_StandingsTooLow");

    // Grant standing >= 6.0 and verify jump succeeds into Kino
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().standings.insert(TRIGLAVIAN_FACTION_ID, 7.0);
    });
    ctx.advance_secs(15);
    let home_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(poch_beyonce),
            "CmdStargateJump",
            &[PyRep::Integer(home_gate)],
            &kw,
            None,
        )
        .await
        .expect("Pochven home gate jump with standing >= 6.0");
    assert_eq!(
        home_rep.dict_get("solarSystemID").and_then(|v| v.as_i64()),
        Some(KINO_SYS_ID as i64)
    );

    // 4. Triglavian acceleration gate standing check
    let trig_gate_id = 700_000_701_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut g = eve_db::InventoryItemRecord {
            item_id: trig_gate_id,
            type_id: 47_685, // Triglavian Space Portal / Acceleration Gate
            owner_id: 1,
            location_id: POCHVEN_SYS_ID as i64,
            flag: eve_db::InventoryFlag::None,
            quantity: 1,
            singleton: true,
            ..Default::default()
        };
        g.dynamic_attributes.insert(1004, 3.0); // min standing 3.0
        g.dynamic_attributes.insert(1005, TRIGLAVIAN_FACTION_ID as f64);
        snap.items.insert(trig_gate_id, g);
        snap.characters.get_mut(&char_id).unwrap().standings.insert(TRIGLAVIAN_FACTION_ID, 1.5); // 1.5 < 3.0
    });
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = POCHVEN_SYS_ID;
        }
    });
    sess.solar_system_id = Some(POCHVEN_SYS_ID);
    let _ = poch_handle
        .spawn_ball(Ball {
            id: trig_gate_id,
            type_id: 47_685,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            radius: 100.0,
            mass: 10_000_000.0,
            mode: MovementMode::Rigid,
            speed_fraction: 0.0,
            max_velocity: 0.0,
            inertia: 1.0,
        })
        .await;
    let _ = poch_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 621,
            position: DVec3::new(100.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            radius: 50.0,
            mass: 1_000_000.0,
            mode: MovementMode::Stop,
            speed_fraction: 0.0,
            max_velocity: 250.0,
            inertia: 0.5,
        })
        .await;

    let gate_low_err = ctx
        .dispatch_rpc(
            &mut sess,
            "keeper",
            None,
            "ActivateAccelerationGate",
            &[PyRep::Integer(trig_gate_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Triglavian gate with standing < 3.0 must fail");
    assert_eq!(gate_low_err.code, "SystemCheck_JumpFailed_StandingsTooLow");

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().standings.insert(TRIGLAVIAN_FACTION_ID, 4.0); // 4.0 >= 3.0
    });
    let gate_pass = ctx
        .dispatch_rpc(
            &mut sess,
            "keeper",
            None,
            "ActivateAccelerationGate",
            &[PyRep::Integer(trig_gate_id)],
            &kw,
            None,
        )
        .await
        .expect("Triglavian gate with standing >= 3.0 must succeed");
    assert_eq!(gate_pass.dict_get("success"), Some(&PyRep::Bool(true)));

    // 5. Pochven filament rules: exit filament from Pochven to Empire
    let exit_fil_id = 200_000_602_i64;
    let fleet_id = 888_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            fleet_id,
            eve_db::FleetRecord {
                fleet_id,
                boss_id: char_id,
                members: vec![char_id],
                ..Default::default()
            },
        );
        snap.items.insert(
            exit_fil_id,
            eve_db::InventoryItemRecord {
                item_id: exit_fil_id,
                type_id: 56_065, // Devana Pochven K-space Exit Filament
                owner_id: char_id,
                location_id: ship_id,
                flag: eve_db::InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    let _ = poch_handle.set_movement_mode(ship_id, MovementMode::Stop).await;
    let _ = poch_handle
        .set_ball_position(ship_id, DVec3::new(50_000_000.0, 50_000_000.0, 50_000_000.0))
        .await;
    ctx.advance_secs(11);
    let fil_res = ctx
        .dispatch_rpc(
            &mut sess,
            "randomJumpMgr",
            None,
            "ActivateRandomJumpFilament",
            &[PyRep::Integer(exit_fil_id)],
            &kw,
            None,
        )
        .await
        .expect("Exit filament from Pochven");
    let exit_dest = fil_res.dict_get("solarSystemID").and_then(|v| v.as_i64()).unwrap();
    let exit_reg = ctx.sde.solar_systems.get(&(exit_dest as u32)).map(|s| s.region_id).unwrap();
    assert_ne!(exit_reg, 10_000_070, "Exit filament must take pilot out of Pochven");
}
