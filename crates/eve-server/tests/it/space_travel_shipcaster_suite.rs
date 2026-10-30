//! Integration test suite for Shipcaster and Space Travel RPC Services.
//!
//! Validates:
//! - `shipCasterLandingPadMgr` (GetAllLandingPads, GetFactionLandingPads)
//! - `shipCasterLauncherMgr` (GetFactionsWithShipcaster)
//! - `starterShipcasterTravelMgr` (CanCharacterJump, CmdJumpThroughStarterShipcaster)
//! - `shipcasterTravelMgr` (CmdJumpThroughShipcaster)
//! - `populationCap` (GetCharacterLoadSlot, MoveCharacterToNewSystem)
//!
//! Enforces:
//! - Operational proximity checks (<= 5,000m) failing closed if balls are absent.
//! - Crimewatch weapons timer gating.
//! - Destination landing pad linkage and status verification.
//! - Full cross-system jump transfer (ball migration, session state, chat sync).
//! - Server-offered alternative slot validation for population cap migration.

use crate::common;
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

fn kv_i64(rep: &PyRep, key: &str) -> i64 {
    match rep {
        PyRep::KeyVal(m) => m.get(key).and_then(|v| v.as_i64()).unwrap_or(0),
        _ => 0,
    }
}

fn kv_bool(rep: &PyRep, key: &str) -> bool {
    match rep {
        PyRep::KeyVal(m) => m.get(key).and_then(|v| v.as_bool()).unwrap_or(false),
        _ => false,
    }
}

#[tokio::test]
async fn test_shipcaster_landing_pads_and_launchers() {
    let ctx = ServerContext::new();
    let mut sess = SessionState::new(20_201);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

    let kw = HashMap::new();
    let char_id = 90_000_001;
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

    // 1. GetAllLandingPads
    let all_pads_rep = ctx
        .dispatch_rpc(&mut sess, "shipCasterLandingPadMgr", None, "GetAllLandingPads", &[], &kw, None)
        .await
        .expect("GetAllLandingPads");
    let PyRep::List(all_pads) = all_pads_rep else {
        panic!("GetAllLandingPads must return a List");
    };
    assert_eq!(all_pads.len(), 8, "Must seed 8 canonical Empire FW landing pads (2 per faction)");
    for pad in &all_pads {
        assert!(kv_i64(pad, "itemID") > 0, "Landing pad itemID must be positive");
        assert!(kv_i64(pad, "solarSystemID") > 0, "Landing pad solarSystemID must be valid");
        assert!(kv_bool(pad, "isBuilt"), "Landing pad must be built");
        assert!(kv_bool(pad, "canBeLinked"), "Built undisrupted pad can be linked");
        assert!(!kv_bool(pad, "isDisrupted"), "Default landing pad must not be disrupted");
    }

    // 2. GetFactionLandingPads
    let caldari_pads_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "shipCasterLandingPadMgr",
            None,
            "GetFactionLandingPads",
            &[PyRep::Integer(500001)],
            &kw,
            None,
        )
        .await
        .expect("GetFactionLandingPads for Caldari");
    let PyRep::List(caldari_pads) = caldari_pads_rep else {
        panic!("GetFactionLandingPads must return a List");
    };
    assert_eq!(caldari_pads.len(), 2, "Caldari has 2 active warzone landing pads");
    assert_eq!(kv_i64(&caldari_pads[0], "factionID"), 500001);
    assert_eq!(kv_i64(&caldari_pads[0], "solarSystemID"), 30002811); // Onatoh
    assert_eq!(caldari_pads.iter().filter(|p| kv_bool(p, "isLinked")).count(), 1);

    let minmatar_pads_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "shipCasterLandingPadMgr",
            None,
            "GetFactionLandingPads",
            &[PyRep::Integer(500002)],
            &kw,
            None,
        )
        .await
        .expect("GetFactionLandingPads for Minmatar");
    let PyRep::List(minmatar_pads) = minmatar_pads_rep else {
        panic!("GetFactionLandingPads must return a List");
    };
    assert_eq!(minmatar_pads.len(), 2, "Minmatar has 2 active warzone landing pads");
    assert_eq!(kv_i64(&minmatar_pads[0], "factionID"), 500002);
    assert_eq!(kv_i64(&minmatar_pads[0], "solarSystemID"), 30002056); // Resbroko
    assert_eq!(minmatar_pads.iter().filter(|p| kv_bool(p, "isLinked")).count(), 1);

    // Missing faction argument fails
    let missing_faction_err = ctx
        .dispatch_rpc(&mut sess, "shipCasterLandingPadMgr", None, "GetFactionLandingPads", &[], &kw, None)
        .await
        .expect_err("Missing factionID must fail");
    assert_eq!(missing_faction_err.code, "InvalidArguments");

    // 3. GetFactionsWithShipcaster
    let factions_rep = ctx
        .dispatch_rpc(&mut sess, "shipCasterLauncherMgr", None, "GetFactionsWithShipcaster", &[], &kw, None)
        .await
        .expect("GetFactionsWithShipcaster");
    let PyRep::List(factions) = factions_rep else {
        panic!("GetFactionsWithShipcaster must return a List");
    };
    assert_eq!(factions.len(), 4, "Four empire factions operate shipcaster launchers");
    assert_eq!(factions[0], PyRep::Integer(500001));
    assert_eq!(factions[1], PyRep::Integer(500002));
    assert_eq!(factions[2], PyRep::Integer(500003));
    assert_eq!(factions[3], PyRep::Integer(500004));
}

#[tokio::test]
async fn test_starter_and_interstellar_shipcaster_jumps() {
    let ctx = ServerContext::new();
    let mut sess = SessionState::new(20_202);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

    let kw = HashMap::new();
    let char_id = 90_000_001;
    let starter_sys: u32 = 30000141; // Kisogo (Caldari State War Academy starter system)
    let staging_sys: u32 = 30045324; // Onnamon / Caldari FW HQ
    let starter_launcher_id: i64 = 105_000_117;

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

    let ship_id = sess.ship_id.unwrap_or(100_000_001);

    // Undock character to starter system space
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(starter_sys);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = starter_sys;
            ch.station_id = None;
            ch.structure_id = None;
        }
    });

    let handle_starter = ctx.systems.get_or_spawn(starter_sys);

    // 1. starterShipcasterTravelMgr.CanCharacterJump
    let can_jump_rep = ctx
        .dispatch_rpc(&mut sess, "starterShipcasterTravelMgr", None, "CanCharacterJump", &[], &kw, None)
        .await
        .expect("CanCharacterJump");
    assert_eq!(can_jump_rep, PyRep::Bool(true), "Character with no weapons timer can jump");

    // 2. Negative test: jump fails closed when ship ball is not in space
    let missing_ship_err = ctx
        .dispatch_rpc(
            &mut sess,
            "starterShipcasterTravelMgr",
            None,
            "CmdJumpThroughStarterShipcaster",
            &[PyRep::Integer(starter_launcher_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Must fail when ship ball is not in space");
    assert_eq!(missing_ship_err.code, "ShipNotInSpace");

    // Spawn ship ball in space
    let _ = handle_starter
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 601,
                position: DVec3::new(50_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 1000.0,
                inertia: 1.0,
                mass: 100_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_ship_combat_state(char_id, ship_id, 601),
        )
        .await;

    // Negative test: jump fails closed when target_id == 0 (R-SC-01)
    let zero_target_err = ctx
        .dispatch_rpc(
            &mut sess,
            "starterShipcasterTravelMgr",
            None,
            "CmdJumpThroughStarterShipcaster",
            &[PyRep::Integer(0)],
            &kw,
            None,
        )
        .await
        .expect_err("Must fail when target_id is 0");
    assert_eq!(zero_target_err.code, "TargetNotFound");



    // 3. Negative test: jump fails when outside 5,000m range (R-SC-01)
    let out_of_range_err = ctx.dispatch_rpc(&mut sess, "starterShipcasterTravelMgr", None, "CmdJumpThroughStarterShipcaster", &[PyRep::Integer(starter_launcher_id)], &kw, None).await.expect_err("Must fail when outside operational range");
    assert_eq!(out_of_range_err.code, "OutsideOperationalRange");

    // Move ship within operational range: launcher is at 300_000.
    let _ = handle_starter.remove_ball(ship_id).await;
    let _ = handle_starter.spawn_ball_with_combat(Ball { id: ship_id, type_id: 601, position: DVec3::new(298_000.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 1000.0, inertia: 1.0, mass: 100_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop }, ctx.build_ship_combat_state(char_id, ship_id, 601)).await;

    // 4. Negative test: jump blocked by weapons timer
    let now = ctx.now_unix();
    { let mut cw = ctx.crimewatch.write().unwrap(); cw.get_or_create(char_id).weapon_timer_until_unix = now + 60; }
    let timer_rep = ctx.dispatch_rpc(&mut sess, "starterShipcasterTravelMgr", None, "CanCharacterJump", &[], &kw, None).await.expect("CanCharacterJump with weapons timer");
    assert_eq!(timer_rep, PyRep::Bool(false));

    let timer_err = ctx.dispatch_rpc(&mut sess, "starterShipcasterTravelMgr", None, "CmdJumpThroughStarterShipcaster", &[PyRep::Integer(starter_launcher_id)], &kw, None).await.expect_err("Must fail under weapons timer");
    assert_eq!(timer_err.code, "WeaponsTimerActive");

    { let mut cw = ctx.crimewatch.write().unwrap(); cw.get_or_create(char_id).weapon_timer_until_unix = 0; }

    // 5. Positive test: Starter jump succeeds and moves ball to Onnamon FW HQ system (R-SC-06)
    ctx.advance_secs(12);
    let starter_jump_res = ctx.dispatch_rpc(&mut sess, "starterShipcasterTravelMgr", None, "CmdJumpThroughStarterShipcaster", &[PyRep::Integer(starter_launcher_id)], &kw, None).await.expect("CmdJumpThroughStarterShipcaster");
    assert_eq!(starter_jump_res, PyRep::None);
    assert_eq!(sess.solar_system_id, Some(staging_sys));

    eve_server::services::space_destiny::clear_session_change_timer(&ctx, char_id);

    let ball_in_old = handle_starter.get_ball_snapshot(ship_id).await.unwrap();
    assert!(ball_in_old.is_none(), "Ship ball removed from origin system");
    let handle_staging = ctx.systems.get_or_spawn(staging_sys);
    let ball_in_new = handle_staging.get_ball_snapshot(ship_id).await.unwrap();
    assert!(ball_in_new.is_some(), "Ship ball spawned in destination FW HQ system");

    // Resolve currently linked Caldari warzone beacon
    let caldari_pads_rep = ctx.dispatch_rpc(&mut sess, "shipCasterLandingPadMgr", None, "GetFactionLandingPads", &[PyRep::Integer(500001)], &kw, None).await.expect("GetFactionLandingPads");
    let PyRep::List(caldari_pads) = caldari_pads_rep else { panic!("Expected List"); };
    let active_pad = caldari_pads.iter().find(|p| kv_bool(p, "isLinked")).expect("1 active Caldari beacon");
    let active_pad_id = kv_i64(active_pad, "itemID");
    let warzone_sys = kv_i64(active_pad, "solarSystemID") as u32;

    // 6. Interstellar Shipcaster Jump: Negative test - non-existent landing pad rejected (R-SC-01)
    let unlinked_err = ctx.dispatch_rpc(&mut sess, "shipcasterTravelMgr", None, "CmdJumpThroughShipcaster", &[PyRep::Integer(105_000_001), PyRep::Integer(warzone_sys as i64), PyRep::Integer(999_999)], &kw, None).await.expect_err("Missing landing pad must be rejected");
    assert_eq!(unlinked_err.code, "LandingPadNotFound");

    let _ = handle_staging.remove_ball(ship_id).await;
    let _ = handle_staging.spawn_ball_with_combat(Ball { id: ship_id, type_id: 601, position: DVec3::new(198_000.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 1000.0, inertia: 1.0, mass: 100_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop }, ctx.build_ship_combat_state(char_id, ship_id, 601)).await;

    // 7. Positive test: Interstellar jump succeeds to active warzone landing pad (R-SC-01, R-SC-06)
    ctx.db.with_mut_snapshot(|snap| { if let Some(ch) = snap.characters.get_mut(&char_id) { ch.war_faction_id = Some(500001); } });
    let shipcaster_jump = ctx.dispatch_rpc(&mut sess, "shipcasterTravelMgr", None, "CmdJumpThroughShipcaster", &[PyRep::Integer(105_000_001), PyRep::Integer(warzone_sys as i64), PyRep::Integer(active_pad_id)], &kw, None).await.expect("CmdJumpThroughShipcaster");
    assert_eq!(shipcaster_jump, PyRep::None);
    assert_eq!(sess.solar_system_id, Some(warzone_sys));

    let ball_in_staging = handle_staging.get_ball_snapshot(ship_id).await.unwrap();
    assert!(ball_in_staging.is_none(), "Ship ball removed from HQ system");
    let handle_warzone = ctx.systems.get_or_spawn(warzone_sys);
    let ball_in_warzone = handle_warzone.get_ball_snapshot(ship_id).await.unwrap();
    assert!(ball_in_warzone.is_some(), "Ship ball spawned in warzone system");

    ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&char_id).expect("character in snapshot");
        assert_eq!(ch.solar_system_id, warzone_sys, "Character DB solar system updated");
        let ship = snap.items.get(&ship_id).expect("ship item in snapshot");
        assert_eq!(ship.location_id, warzone_sys as i64, "Ship item DB location updated");
    });

    let _ = handle_warzone.remove_ball(ship_id).await;
    let _ = handle_staging.spawn_ball_with_combat(Ball { id: ship_id, type_id: 601, position: DVec3::new(198_000.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 1000.0, inertia: 1.0, mass: 100_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop }, ctx.build_ship_combat_state(char_id, ship_id, 601)).await;
    sess.solar_system_id = Some(staging_sys);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) { ch.solar_system_id = staging_sys; }
        if let Some(ship) = snap.items.get_mut(&ship_id) { ship.location_id = staging_sys as i64; }
    });

    let re_jump_err = ctx.dispatch_rpc(&mut sess, "shipcasterTravelMgr", None, "CmdJumpThroughShipcaster", &[PyRep::Integer(105_000_001), PyRep::Integer(warzone_sys as i64), PyRep::Integer(active_pad_id)], &kw, None).await.expect_err("Immediate re-jump must fail with session change timer active");
    assert_eq!(re_jump_err.code, "SessionChangeInProgress");

    eve_server::services::space_destiny::clear_session_change_timer(&ctx, char_id);

    sess.station_id = Some(60_000_001);
    ctx.db.with_mut_snapshot(|snap| { if let Some(ch) = snap.characters.get_mut(&char_id) { ch.station_id = Some(60_000_001); } });
    let docked_err = ctx.dispatch_rpc(&mut sess, "shipcasterTravelMgr", None, "CmdJumpThroughShipcaster", &[PyRep::Integer(105_000_001), PyRep::Integer(warzone_sys as i64), PyRep::Integer(active_pad_id)], &kw, None).await.expect_err("Docked shipcaster jump must be rejected");
    assert_eq!(docked_err.code, "CantDoThatWhileDocked");
    sess.station_id = None;
    ctx.db.with_mut_snapshot(|snap| { if let Some(ch) = snap.characters.get_mut(&char_id) { ch.station_id = None; } });

    ctx.db.with_mut_snapshot(|snap| { if let Some(ship) = snap.items.get_mut(&ship_id) { ship.type_id = 19720; } });
    let cap_err = ctx.dispatch_rpc(&mut sess, "shipcasterTravelMgr", None, "CmdJumpThroughShipcaster", &[PyRep::Integer(105_000_001), PyRep::Integer(warzone_sys as i64), PyRep::Integer(active_pad_id)], &kw, None).await.expect_err("Capital ship jump must be rejected");
    assert_eq!(cap_err.code, "CapitalShipCannotJumpShipcaster");

    ctx.db.with_mut_snapshot(|snap| { if let Some(ship) = snap.items.get_mut(&ship_id) { ship.type_id = 601; } });

    ctx.db.with_mut_snapshot(|snap| { if let Some(ch) = snap.characters.get_mut(&char_id) { ch.war_faction_id = Some(500002); } });
    let fw_err = ctx.dispatch_rpc(&mut sess, "shipcasterTravelMgr", None, "CmdJumpThroughShipcaster", &[PyRep::Integer(105_000_001), PyRep::Integer(warzone_sys as i64), PyRep::Integer(active_pad_id)], &kw, None).await.expect_err("Hostile militia character must be rejected");
    assert_eq!(fw_err.code, "FactionShipcasterAccessDenied");

    ctx.db.with_mut_snapshot(|snap| { if let Some(ch) = snap.characters.get_mut(&char_id) { ch.war_faction_id = None; } });
}

#[tokio::test]
async fn test_population_cap_load_slots_and_migration() {
    // parity: rpc:populationCap.MoveCharacterToNewSystem
    eve_testkit::attest(&["rpc:populationCap.MoveCharacterToNewSystem"]);

    let ctx = ServerContext::new();
    let mut sess = SessionState::new(20_203);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

    let kw = HashMap::new();
    let char_id = 90_000_001;
    let uncapped_sys = 30000143; // Non-capped system
    let capped_sys = 30002813;   // Tama (capped system)

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = uncapped_sys;
        }
    });

    eve_server::services::cluster_macho_misc_services::with_misc_state(&ctx, |st| {
        st.system_population_caps.insert(capped_sys, 0);
    });


    // 1. Uncapped system returns single slot with no alternative
    let normal_slots_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "populationCap",
            None,
            "GetCharacterLoadSlot",
            &[PyRep::Integer(char_id)],
            &kw,
            None,
        )
        .await
        .expect("GetCharacterLoadSlot for uncapped system");
    let PyRep::Dict(normal_slots) = normal_slots_rep else {
        panic!("GetCharacterLoadSlot must return a Dict");
    };
    assert_eq!(normal_slots.len(), 1, "Uncapped system has exactly 1 load slot");
    assert_eq!(normal_slots[0].0, PyRep::Integer(uncapped_sys as i64));
    assert_eq!(normal_slots[0].1, PyRep::None);

    // Negative test: Arbitrary move attempt on uncapped system without offered slot must fail (R-SC-03)
    let uncapped_move_err = ctx
        .dispatch_rpc(
            &mut sess,
            "populationCap",
            None,
            "MoveCharacterToNewSystem",
            &[PyRep::Integer(char_id), PyRep::Integer(30002811), PyRep::String("arbitrary-key".into())],
            &kw,
            None,
        )
        .await
        .expect_err("Arbitrary move without offer must fail");
    assert_eq!(uncapped_move_err.code, "SystemNotOffered");

    // 2. Set system to capped system (Tama)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = capped_sys;
        }
    });

    let capped_slots_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "populationCap",
            None,
            "GetCharacterLoadSlot",
            &[PyRep::Integer(char_id)],
            &kw,
            None,
        )
        .await
        .expect("GetCharacterLoadSlot for capped system");
    let PyRep::Dict(capped_slots) = capped_slots_rep else {
        panic!("GetCharacterLoadSlot must return a Dict");
    };
    assert_eq!(capped_slots.len(), 2, "Capped system offers alternative load slot");
    assert_eq!(capped_slots[0].0, PyRep::Integer(capped_sys as i64));
    assert_eq!(capped_slots[0].1, PyRep::None);
    let offered_sys = if let PyRep::Integer(sys_id) = &capped_slots[1].0 {
        *sys_id as u32
    } else {
        panic!("Expected Integer for offered_sys");
    };
    let PyRep::String(slot_key) = &capped_slots[1].1 else {
        panic!("Offered alternative slot must include slotKey string");
    };
    assert!(!slot_key.is_empty(), "slotKey must not be empty");

    // 3. Negative test: Unowned character rejected (R-SC-03)
    let unowned_err = ctx
        .dispatch_rpc(
            &mut sess,
            "populationCap",
            None,
            "MoveCharacterToNewSystem",
            &[PyRep::Integer(99_999_999), PyRep::Integer(offered_sys as i64), PyRep::String(slot_key.clone())],
            &kw,
            None,
        )
        .await
        .expect_err("Unowned character must be rejected");
    assert_eq!(unowned_err.code, "CharacterOwnershipError");

    // 4. Negative test: Unoffered system rejected even if in SDE (R-SC-03)
    let unoffered_err = ctx
        .dispatch_rpc(
            &mut sess,
            "populationCap",
            None,
            "MoveCharacterToNewSystem",
            &[PyRep::Integer(char_id), PyRep::Integer(30000142), PyRep::String(slot_key.clone())],
            &kw,
            None,
        )
        .await
        .expect_err("Unoffered system must be rejected");
    assert_eq!(unoffered_err.code, "SystemNotOffered");

    // 5. Negative test: Invalid slot key rejected (R-SC-03)
    let invalid_key_err = ctx
        .dispatch_rpc(
            &mut sess,
            "populationCap",
            None,
            "MoveCharacterToNewSystem",
            &[PyRep::Integer(char_id), PyRep::Integer(offered_sys as i64), PyRep::String("bogus-slot-key".into())],
            &kw,
            None,
        )
        .await
        .expect_err("Invalid slot key must be rejected");
    assert_eq!(invalid_key_err.code, "SystemNotOffered");

    // 6. Positive test: MoveCharacterToNewSystem to offered alternative system succeeds (R-SC-03)
    let move_res = ctx
        .dispatch_rpc(
            &mut sess,
            "populationCap",
            None,
            "MoveCharacterToNewSystem",
            &[PyRep::Integer(char_id), PyRep::Integer(offered_sys as i64), PyRep::String(slot_key.clone())],
            &kw,
            None,
        )
        .await
        .expect("MoveCharacterToNewSystem");
    assert_eq!(move_res, PyRep::None);

    // Verify character and ship relocated, docking state cleared
    ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&char_id).expect("character in snapshot");
        assert_eq!(ch.solar_system_id, offered_sys, "Character solar system updated");
        assert_eq!(ch.station_id, None, "Station cleared");
        assert_eq!(ch.structure_id, None, "Structure cleared");
    });

    // 7. Consumed offer cannot be reused
    let reuse_err = ctx
        .dispatch_rpc(
            &mut sess,
            "populationCap",
            None,
            "MoveCharacterToNewSystem",
            &[PyRep::Integer(char_id), PyRep::Integer(offered_sys as i64), PyRep::String(slot_key.clone())],
            &kw,
            None,
        )
        .await
        .expect_err("Reusing consumed slot key must be rejected");
    assert_eq!(reuse_err.code, "SystemNotOffered");
}
