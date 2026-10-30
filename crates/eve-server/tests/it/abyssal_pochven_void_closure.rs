//! Integration tests for Void Space and Pochven Travel mechanics.
//!
//! Note: Void space content (pockets, nebulas, transfer gates) is server-side live event content.
//! Corresponding parity row `feat:navigation.filament-void` is verified below.

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

// parity: feat:navigation.filament-void
#[tokio::test]
async fn test_void_space_player_jump_and_gate_activation() {
    eve_testkit::attest(&["feat:navigation.filament-void"]);
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
                type_id: 621, // Caracal cruiser
                owner_id: char_id,
                location_id: JITA_SYS_ID as i64,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            100_000_690,
            eve_db::InventoryItemRecord {
                item_id: 100_000_690,
                type_id: 2_605, // Nanofiber Internal Structure II (+maxVelocity, -agility)
                owner_id: char_id,
                location_id: ship_id,
                flag: eve_db::InventoryFlag::LoSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    ctx.invalidate_ship_fitting(ship_id);
    let _ = ctx.get_or_rebuild_ship_fitting(char_id, ship_id);
    ctx.sync_character_local_chat(char_id, None, JITA_SYS_ID);
    assert!(
        ctx.db.with_snapshot(|s| s.chat_channels["solarsystemid2:30000142"].members.contains(&char_id)),
        "Pilot starts in Jita Local chat before Void Space jump"
    );
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

    // 4b. BF-05: Non-Void Space filaments (Needlejack 50057/53977, Pochven 56068/56065, Proving 56725/53965, Abyssal 47763) and Abyssal Trace (47458) rejected
    for (idx, non_void_tid) in [50_057_u32, 53_977, 56_068, 56_065, 56_725, 53_965, 47_763].into_iter().enumerate() {
        let bad_iid = 200_000_700 + idx as i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(bad_iid, eve_db::InventoryItemRecord {
                item_id: bad_iid,
                type_id: non_void_tid,
                owner_id: char_id,
                location_id: ship_id,
                flag: eve_db::InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                ..Default::default()
            });
        });
        let err = ctx
            .dispatch_rpc(&mut sess, "voidSpaceMgr", None, "VoidSpacePlayerJump", &[PyRep::Integer(bad_iid)], &kw, None)
            .await
            .expect_err("Non-Void filament must be rejected by VoidSpacePlayerJump");
        assert_eq!(err.code, "InvalidTarget", "type {non_void_tid} rejected");
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.remove(&bad_iid);
        });
    }
    let _ = jita_handle
        .spawn_ball(Ball {
            id: 200_000_750,
            type_id: 47_458, // Abyssal Trace
            position: DVec3::new(500.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            radius: 50.0,
            mass: 1_000.0,
            mode: MovementMode::Stop,
            speed_fraction: 0.0,
            max_velocity: 0.0,
            inertia: 1.0,
        })
        .await;
    let trace_err = ctx
        .dispatch_rpc(&mut sess, "voidSpaceMgr", None, "VoidSpacePlayerJump", &[PyRep::Integer(200_000_750)], &kw, None)
        .await
        .expect_err("Abyssal Trace must be rejected by VoidSpacePlayerJump");
    assert_eq!(trace_err.code, "InvalidTarget");
    jita_handle.try_remove_ball(200_000_750);

    // 5. Seed Warp Matrix Filament (62_448, SDE group 4145) and enforce < 1,000 km station & stargate ProximityRestriction
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            filament_item_id,
            eve_db::InventoryItemRecord {
                item_id: filament_item_id,
                type_id: 62_448, // QA Void Filament (SDE group 4145 Warp Matrix Filaments)
                owner_id: char_id,
                location_id: ship_id,
                flag: eve_db::InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    let stn_pos = ctx.sde.stations[&60_003_760].position;
    let _ = jita_handle.set_ball_position(ship_id, stn_pos + DVec3::new(500_000.0, 0.0, 0.0)).await;
    let stn_prox = ctx
        .dispatch_rpc(&mut sess, "voidSpaceMgr", None, "VoidSpacePlayerJump", &[PyRep::Integer(filament_item_id)], &kw, None)
        .await
        .expect_err("VoidSpacePlayerJump <1000km from station must fail");
    assert_eq!(stn_prox.code, "ProximityRestriction");
    let gate_pos = ctx.sde.jumps.values().find(|j| j.from_solar_system_id == JITA_SYS_ID).unwrap().position;
    let _ = jita_handle.set_ball_position(ship_id, gate_pos + DVec3::new(500_000.0, 0.0, 0.0)).await;
    let gate_prox = ctx
        .dispatch_rpc(&mut sess, "voidSpaceMgr", None, "VoidSpacePlayerJump", &[PyRep::Integer(filament_item_id)], &kw, None)
        .await
        .expect_err("VoidSpacePlayerJump <1000km from stargate must fail");
    assert_eq!(gate_prox.code, "ProximityRestriction");
    assert!(ctx.db.with_snapshot(|s| s.items.contains_key(&filament_item_id)), "Proximity refusal must not consume filament");

    // Move >1,000 km from Jita stations/stargates and jump
    let _ = jita_handle.set_ball_position(ship_id, DVec3::new(0.0, 2.0e9, 0.0)).await;

    let _ = ctx.drain_notifications_for_char(char_id);
    let jump_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "voidSpaceMgr",
            None,
            "VoidSpacePlayerJump",
            &[PyRep::Integer(filament_item_id), PyRep::Integer(5001)],
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
    assert_eq!(
        jump_rep.dict_get("contentID"),
        Some(&PyRep::Integer(filament_item_id))
    );

    // Verify OnVoidSpaceEntered notification
    let notifs = ctx.drain_notifications_for_char(char_id);
    let entered_notif = notifs
        .iter()
        .find(|n| n.event == "OnVoidSpaceEntered")
        .expect("OnVoidSpaceEntered notification must be emitted");
    assert_eq!(entered_notif.args.len(), 1);
    assert_eq!(
        entered_notif.args[0].dict_get("content_id"),
        Some(&PyRep::Integer(filament_item_id))
    );
    assert_eq!(
        entered_notif.args[0].dict_get("nebula_graphic_id"),
        Some(&PyRep::Integer(200_001))
    );

    // Verify filament consumed from cargo
    assert!(
        ctx.db.with_snapshot(|snap| !snap.items.contains_key(&filament_item_id)),
        "Filament must be consumed on jump"
    );

    // Verify session updated, Jita Local chat membership removed, and fitted Dogma kinematics preserved on Void Space ball
    assert_eq!(sess.solar_system_id, Some(VOID_SPACE_SYS_ID));
    ctx.db.with_snapshot(|s| {
        assert!(!s.chat_channels.get("solarsystemid2:30000142").is_some_and(|c| c.members.contains(&char_id)), "Removed from Jita Local on Void Space entry");
        assert!(!s.chat_channels.contains_key("solarsystemid2:32000002"), "Void Space has no Local chat channel");
    });
    let raw_vel = ctx.sde.types[&621].default_attributes[&37];
    let raw_in = ctx.sde.types[&621].default_attributes[&70];
    let fit = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).expect("fitted Caracal");
    let (fit_vel, fit_in) = (fit.ship.get_attribute(37, &ctx.sde), fit.ship.get_attribute(70, &ctx.sde));
    assert!((fit_vel - raw_vel).abs() > 1.0 && (fit_in - raw_in).abs() > 0.01, "Nanofiber II modifies maxVelocity & agility from raw hull");

    let void_handle = ctx.systems.get_or_spawn(VOID_SPACE_SYS_ID);
    let (_, vb) = void_handle.get_ball_snapshot(ship_id).await.unwrap().expect("Ship ball must exist in Void Space");
    assert!((vb.max_velocity - fit_vel).abs() < 1e-3 && (vb.inertia - fit_in).abs() < 1e-4, "Void Space ball preserves fitted Dogma kinematics");
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

    // 7. Exit gate activation returns to origin solar system, restores Local chat, and preserves fitted Dogma kinematics
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
    assert!(ctx.db.with_snapshot(|s| s.chat_channels["solarsystemid2:30000142"].members.contains(&char_id)), "Restored to Jita Local chat on Void Space exit");
    let (_, jb) = jita_handle.get_ball_snapshot(ship_id).await.unwrap().expect("Ship ball restored in Jita");
    assert!((jb.max_velocity - fit_vel).abs() < 1e-3 && (jb.inertia - fit_in).abs() < 1e-4, "Returned Jita ball preserves fitted Dogma kinematics");

    // 8. Also verify V24 Warp Matrix / Void filament type 62_448 ("QA Void Filament", SDE group_id = 4145)
    //    jumps into Void Space (32_000_002) and distinguish it from Needlejack Jump Filaments (53_977, group_id = 4041)
    assert_eq!(ctx.sde.types.get(&62_448).map(|t| t.group_id), Some(4145), "QA Void Filament 62448 is SDE group 4145 (Warp Matrix Filaments)");
    assert_eq!(ctx.sde.types.get(&53_977).map(|t| t.group_id), Some(4041), "Noise-5 53977 is SDE group 4041 (Jump Filaments), not Void Space");
    let void_fil_iid = 200_000_602_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            void_fil_iid,
            eve_db::InventoryItemRecord {
                item_id: void_fil_iid,
                type_id: 62_448, // QA Void Filament (SDE group 4145)
                owner_id: char_id,
                location_id: ship_id,
                flag: eve_db::InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    let jump_rep2 = ctx
        .dispatch_rpc(
            &mut sess,
            "voidSpaceMgr",
            None,
            "VoidSpacePlayerJump",
            &[PyRep::Integer(void_fil_iid), PyRep::Integer(5002)],
            &kw,
            None,
        )
        .await
        .expect("VoidSpacePlayerJump via QA Void Filament 62448");
    assert_eq!(jump_rep2.dict_get("solarSystemID"), Some(&PyRep::Integer(VOID_SPACE_SYS_ID as i64)));
    assert_eq!(sess.solar_system_id, Some(VOID_SPACE_SYS_ID));
    let exit_rep2 = ctx
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
        .expect("VoidSpaceEndGateActivation second exit");
    assert_eq!(exit_rep2.dict_get("solarSystemID"), Some(&PyRep::Integer(JITA_SYS_ID as i64)));
    assert_eq!(sess.solar_system_id, Some(JITA_SYS_ID));
}

#[tokio::test]
async fn test_pochven_travel_conduits_filaments_and_gates() {
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
    let poch_beyonce = ctx
        .bind_object(&sess, "beyonce", POCHVEN_SYS_ID as i64)
        .expect("rebind beyonce in Otela after manual return");
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

    // 4. Pochven filament rules: exit filament from Pochven to Empire
    let exit_fil_id = 200_000_602_i64;
    let fleet_id = 888_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = POCHVEN_SYS_ID;
        }
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.location_id = POCHVEN_SYS_ID as i64;
        }
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
    sess.solar_system_id = Some(POCHVEN_SYS_ID);
    let _ = poch_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 621,
            position: DVec3::new(50_000_000.0, 50_000_000.0, 50_000_000.0),
            velocity: DVec3::ZERO,
            radius: 50.0,
            mass: 1_000_000.0,
            mode: MovementMode::Stop,
            speed_fraction: 0.0,
            max_velocity: 250.0,
            inertia: 0.5,
        })
        .await;
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
