//! Integration tests for Faction Warfare Interstellar Shipcasters and Rookie Starter Shipcasters.
//!
//! Attests parity for:
//! - `rpc:shipCasterLandingPadMgr.GetAllLandingPads`
//! - `rpc:shipCasterLandingPadMgr.GetFactionLandingPads`
//! - `rpc:shipCasterLauncherMgr.GetFactionsWithShipcaster`
//! - `rpc:starterShipcasterTravelMgr.CanCharacterJump`
//! - `rpc:starterShipcasterTravelMgr.CmdJumpThroughStarterShipcaster`
//! - `feat:navigation.starter-shipcaster`
//! - `rpc:shipcasterTravelMgr.CmdJumpThroughShipcaster`
//! - `feat:navigation.shipcaster`

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

async fn spawn_test_ship(
    ctx: &ServerContext,
    handle: &eve_server::system_actor::SystemHandle,
    char_id: i64,
    ship_id: i64,
    type_id: u32,
    pos: DVec3,
) {
    let _ = handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 1000.0,
                inertia: 1.0,
                mass: 100_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_ship_combat_state(char_id, ship_id, type_id),
        )
        .await;
}

#[tokio::test]
async fn test_shipcaster_landing_pads_and_factions_complete() {
    // parity: rpc:shipCasterLandingPadMgr.GetAllLandingPads
    // parity: rpc:shipCasterLandingPadMgr.GetFactionLandingPads
    // parity: rpc:shipCasterLauncherMgr.GetFactionsWithShipcaster
    let ctx = ServerContext::new();
    let mut sess = SessionState::new(30_101);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

    let kw = HashMap::new();
    let char_id = 90_000_001;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None).await.expect("SelectCharacterID");

    // 1. GetAllLandingPads
    let all_pads_rep = ctx.dispatch_rpc(&mut sess, "shipCasterLandingPadMgr", None, "GetAllLandingPads", &[], &kw, None).await.expect("GetAllLandingPads");
    let PyRep::List(all_pads) = all_pads_rep else { panic!("GetAllLandingPads must return a List"); };
    assert_eq!(all_pads.len(), 4, "Must seed 4 canonical Empire FW landing pads");

    let expected_factions = [500001, 500002, 500003, 500004];
    for pad in &all_pads {
        assert!(kv_i64(pad, "itemID") > 0, "Landing pad itemID must be positive");
        assert!(kv_i64(pad, "solarSystemID") > 0, "Landing pad solarSystemID must be positive");
        assert!(expected_factions.contains(&kv_i64(pad, "factionID")));
        assert!(kv_bool(pad, "isBuilt") && kv_bool(pad, "isLinked") && kv_bool(pad, "canBeLinked") && !kv_bool(pad, "isDisrupted"));
    }

    // 2. GetFactionLandingPads for each faction
    for &(fid, sys) in &[(500001, 30002811), (500002, 30002056), (500003, 30002185), (500004, 30003855)] {
        let rep = ctx.dispatch_rpc(&mut sess, "shipCasterLandingPadMgr", None, "GetFactionLandingPads", &[PyRep::Integer(fid)], &kw, None).await.expect("GetFactionLandingPads");
        let PyRep::List(list) = rep else { panic!("Expected list"); };
        assert_eq!(list.len(), 1);
        assert_eq!(kv_i64(&list[0], "factionID"), fid);
        assert_eq!(kv_i64(&list[0], "solarSystemID"), sys);
        assert!(kv_bool(&list[0], "canBeLinked") && kv_bool(&list[0], "isBuilt") && kv_bool(&list[0], "isLinked") && !kv_bool(&list[0], "isDisrupted"));
    }

    // Negative tests for GetFactionLandingPads
    let empty = ctx.dispatch_rpc(&mut sess, "shipCasterLandingPadMgr", None, "GetFactionLandingPads", &[PyRep::Integer(999_999)], &kw, None).await.expect("Unknown faction");
    assert_eq!(empty, PyRep::List(vec![]));

    let no_arg_err = ctx.dispatch_rpc(&mut sess, "shipCasterLandingPadMgr", None, "GetFactionLandingPads", &[], &kw, None).await.expect_err("Missing factionID");
    assert_eq!(no_arg_err.code, "InvalidArguments");

    // 3. GetFactionsWithShipcaster
    let factions_rep = ctx.dispatch_rpc(&mut sess, "shipCasterLauncherMgr", None, "GetFactionsWithShipcaster", &[], &kw, None).await.expect("GetFactionsWithShipcaster");
    let PyRep::List(factions) = factions_rep else { panic!("GetFactionsWithShipcaster must return List"); };
    assert_eq!(factions, vec![PyRep::Integer(500001), PyRep::Integer(500002), PyRep::Integer(500003), PyRep::Integer(500004)]);
}

#[tokio::test]
async fn test_starter_shipcaster_travel_complete() {
    // parity: rpc:starterShipcasterTravelMgr.CanCharacterJump
    // parity: rpc:starterShipcasterTravelMgr.CmdJumpThroughStarterShipcaster
    // parity: feat:navigation.starter-shipcaster
    let ctx = ServerContext::new();
    let mut sess = SessionState::new(30_102);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

    let kw = HashMap::new();
    let char_id = 90_000_001;
    let starter_sys: u32 = 30002800; // Caldari starter system
    let staging_sys: u32 = 30002813; // Tama staging
    let starter_launcher_id: i64 = 105_000_101;

    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None).await.expect("SelectCharacterID");
    let ship_id = sess.ship_id.unwrap_or(100_000_001);

    // 1. CanCharacterJump without weapons timer
    let can_jump = ctx.dispatch_rpc(&mut sess, "starterShipcasterTravelMgr", None, "CanCharacterJump", &[], &kw, None).await.expect("CanCharacterJump");
    assert_eq!(can_jump, PyRep::Bool(true));

    // 2. Docked jump rejected
    sess.station_id = Some(60000001);
    let docked_err = ctx.dispatch_rpc(&mut sess, "starterShipcasterTravelMgr", None, "CmdJumpThroughStarterShipcaster", &[PyRep::Integer(starter_launcher_id)], &kw, None).await.expect_err("Docked jump fail");
    assert_eq!(docked_err.code, "CantDoThatWhileDocked");
    sess.station_id = None;

    // Undock to starter system
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(starter_sys);
    ctx.db.with_mut_snapshot(|s| if let Some(ch) = s.characters.get_mut(&char_id) { ch.solar_system_id = starter_sys; ch.station_id = None; ch.structure_id = None; });
    let handle_starter = ctx.systems.get_or_spawn(starter_sys);

    // 3. ShipNotInSpace error
    let missing_err = ctx.dispatch_rpc(&mut sess, "starterShipcasterTravelMgr", None, "CmdJumpThroughStarterShipcaster", &[PyRep::Integer(starter_launcher_id)], &kw, None).await.expect_err("ShipNotInSpace");
    assert_eq!(missing_err.code, "ShipNotInSpace");

    // Spawn ball far away (50,000m; launcher is at 300,000m)
    spawn_test_ship(&ctx, &handle_starter, char_id, ship_id, 601, DVec3::new(50_000.0, 0.0, 0.0)).await;

    // 4. Invalid target ID
    let zero_err = ctx.dispatch_rpc(&mut sess, "starterShipcasterTravelMgr", None, "CmdJumpThroughStarterShipcaster", &[PyRep::Integer(0)], &kw, None).await.expect_err("TargetNotFound");
    assert_eq!(zero_err.code, "TargetNotFound");

    // 5. Outside operational range
    let range_err = ctx.dispatch_rpc(&mut sess, "starterShipcasterTravelMgr", None, "CmdJumpThroughStarterShipcaster", &[PyRep::Integer(starter_launcher_id)], &kw, None).await.expect_err("OutsideOperationalRange");
    assert_eq!(range_err.code, "OutsideOperationalRange");

    // Move ship within range (298,500m => 1,500m away from 300,000m launcher)
    let _ = handle_starter.remove_ball(ship_id).await;
    spawn_test_ship(&ctx, &handle_starter, char_id, ship_id, 601, DVec3::new(298_500.0, 0.0, 0.0)).await;

    // 6. Weapons timer blocks jump
    let now = ctx.now_unix();
    { let mut cw = ctx.crimewatch.write().unwrap(); cw.get_or_create(char_id).weapon_timer_until_unix = now + 60; }
    let timer_rep = ctx.dispatch_rpc(&mut sess, "starterShipcasterTravelMgr", None, "CanCharacterJump", &[], &kw, None).await.expect("CanCharacterJump with timer");
    assert_eq!(timer_rep, PyRep::Bool(false));

    let timer_err = ctx.dispatch_rpc(&mut sess, "starterShipcasterTravelMgr", None, "CmdJumpThroughStarterShipcaster", &[PyRep::Integer(starter_launcher_id)], &kw, None).await.expect_err("WeaponsTimerActive");
    assert_eq!(timer_err.code, "WeaponsTimerActive");
    { let mut cw = ctx.crimewatch.write().unwrap(); cw.get_or_create(char_id).weapon_timer_until_unix = 0; }

    // 7. Capital ship rejected
    ctx.db.with_mut_snapshot(|s| if let Some(ship) = s.items.get_mut(&ship_id) { ship.type_id = 19720; });
    let cap_err = ctx.dispatch_rpc(&mut sess, "starterShipcasterTravelMgr", None, "CmdJumpThroughStarterShipcaster", &[PyRep::Integer(starter_launcher_id)], &kw, None).await.expect_err("CapitalShipCannotJumpShipcaster");
    assert_eq!(cap_err.code, "CapitalShipCannotJumpShipcaster");
    ctx.db.with_mut_snapshot(|s| if let Some(ship) = s.items.get_mut(&ship_id) { ship.type_id = 601; });

    // 8. Negative test: Arbitrary destinationSystemID rejected (BF-01)
    let invalid_dest_err = ctx.dispatch_rpc(&mut sess, "starterShipcasterTravelMgr", None, "CmdJumpThroughStarterShipcaster", &[PyRep::Integer(30000142)], &kw, None).await.expect_err("Arbitrary destination system");
    assert_eq!(invalid_dest_err.code, "InvalidDestinationSystem");

    // 9. Positive jump with destinationSystemID
    ctx.advance_secs(15);
    let jump_res = ctx.dispatch_rpc(&mut sess, "starterShipcasterTravelMgr", None, "CmdJumpThroughStarterShipcaster", &[PyRep::Integer(staging_sys as i64)], &kw, None).await.expect("Starter jump success");
    assert_eq!(jump_res, PyRep::None);
    assert_eq!(sess.solar_system_id, Some(staging_sys));

    // Verify ball handoff
    assert!(handle_starter.get_ball_snapshot(ship_id).await.unwrap().is_none());
    let handle_staging = ctx.systems.get_or_spawn(staging_sys);
    assert!(handle_staging.get_ball_snapshot(ship_id).await.unwrap().is_some());

    ctx.db.with_snapshot(|s| {
        assert_eq!(s.characters.get(&char_id).unwrap().solar_system_id, staging_sys);
        assert_eq!(s.items.get(&ship_id).unwrap().location_id, staging_sys as i64);
    });
}

#[tokio::test]
async fn test_interstellar_shipcaster_travel_complete() {
    // parity: rpc:shipcasterTravelMgr.CmdJumpThroughShipcaster
    // parity: feat:navigation.shipcaster
    let ctx = ServerContext::new();
    let mut sess = SessionState::new(30_103);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

    let kw = HashMap::new();
    let char_id = 90_000_001;
    let staging_sys: u32 = 30002813; // Tama
    let warzone_sys: u32 = 30002811; // Onatoh
    let origin_launcher_id: i64 = 105_000_001;
    let dest_pad_id: i64 = 104_000_001;

    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None).await.expect("SelectCharacterID");
    let ship_id = sess.ship_id.unwrap_or(100_000_001);

    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(staging_sys);
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&char_id) { ch.solar_system_id = staging_sys; ch.station_id = None; ch.structure_id = None; }
        if let Some(ship) = s.items.get_mut(&ship_id) { ship.location_id = staging_sys as i64; }
    });

    let handle_staging = ctx.systems.get_or_spawn(staging_sys);
    spawn_test_ship(&ctx, &handle_staging, char_id, ship_id, 601, DVec3::new(198_000.0, 0.0, 0.0)).await;

    // 1. Negative test: Unlinked / non-existent landing pad
    let unlinked_err = ctx.dispatch_rpc(&mut sess, "shipcasterTravelMgr", None, "CmdJumpThroughShipcaster", &[PyRep::Integer(origin_launcher_id), PyRep::Integer(warzone_sys as i64), PyRep::Integer(999_999)], &kw, None).await.expect_err("Unlinked pad");
    assert_eq!(unlinked_err.code, "LandingPadNotFound");

    // 2. Negative test: Hostile militia character rejected
    ctx.db.with_mut_snapshot(|s| if let Some(ch) = s.characters.get_mut(&char_id) { ch.war_faction_id = Some(500002); });
    let hostile_err = ctx.dispatch_rpc(&mut sess, "shipcasterTravelMgr", None, "CmdJumpThroughShipcaster", &[PyRep::Integer(dest_pad_id), PyRep::Integer(dest_pad_id), PyRep::Integer(origin_launcher_id)], &kw, None).await.expect_err("Hostile militia");
    assert_eq!(hostile_err.code, "FactionShipcasterAccessDenied");

    // Negative test: Unenlisted neutral character rejected (BF-02)
    ctx.db.with_mut_snapshot(|s| if let Some(ch) = s.characters.get_mut(&char_id) { ch.war_faction_id = None; });
    let neutral_err = ctx.dispatch_rpc(&mut sess, "shipcasterTravelMgr", None, "CmdJumpThroughShipcaster", &[PyRep::Integer(dest_pad_id), PyRep::Integer(dest_pad_id), PyRep::Integer(origin_launcher_id)], &kw, None).await.expect_err("Neutral militia");
    assert_eq!(neutral_err.code, "FactionShipcasterAccessDenied");

    // Set enrolled Caldari militia faction for pilot
    ctx.db.with_mut_snapshot(|s| if let Some(ch) = s.characters.get_mut(&char_id) { ch.war_faction_id = Some(500001); });

    // 3. Negative test: Capital ship rejected
    ctx.db.with_mut_snapshot(|s| if let Some(ship) = s.items.get_mut(&ship_id) { ship.type_id = 19720; });
    let cap_err = ctx.dispatch_rpc(&mut sess, "shipcasterTravelMgr", None, "CmdJumpThroughShipcaster", &[PyRep::Integer(dest_pad_id), PyRep::Integer(dest_pad_id), PyRep::Integer(origin_launcher_id)], &kw, None).await.expect_err("Capital ship");
    assert_eq!(cap_err.code, "CapitalShipCannotJumpShipcaster");
    ctx.db.with_mut_snapshot(|s| if let Some(ship) = s.items.get_mut(&ship_id) { ship.type_id = 601; });

    // 4. Positive test: Launch with (beaconID, landingPadID, originBeaconID)
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, char_id);
    let jump_res = ctx.dispatch_rpc(&mut sess, "shipcasterTravelMgr", None, "CmdJumpThroughShipcaster", &[PyRep::Integer(dest_pad_id), PyRep::Integer(dest_pad_id), PyRep::Integer(origin_launcher_id)], &kw, None).await.expect("CmdJumpThroughShipcaster");
    assert_eq!(jump_res, PyRep::None);
    assert_eq!(sess.solar_system_id, Some(warzone_sys));

    // Verify ball handoff
    assert!(handle_staging.get_ball_snapshot(ship_id).await.unwrap().is_none());
    let handle_warzone = ctx.systems.get_or_spawn(warzone_sys);
    assert!(handle_warzone.get_ball_snapshot(ship_id).await.unwrap().is_some());

    ctx.db.with_snapshot(|s| {
        assert_eq!(s.characters.get(&char_id).unwrap().solar_system_id, warzone_sys);
        assert_eq!(s.items.get(&ship_id).unwrap().location_id, warzone_sys as i64);
    });

    // 5. Negative test: Immediate re-jump blocked by session change timer
    let _ = handle_warzone.remove_ball(ship_id).await;
    spawn_test_ship(&ctx, &handle_staging, char_id, ship_id, 601, DVec3::new(198_000.0, 0.0, 0.0)).await;
    sess.solar_system_id = Some(staging_sys);
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&char_id) { ch.solar_system_id = staging_sys; }
        if let Some(ship) = s.items.get_mut(&ship_id) { ship.location_id = staging_sys as i64; }
    });

    let rejump_err = ctx.dispatch_rpc(&mut sess, "shipcasterTravelMgr", None, "CmdJumpThroughShipcaster", &[PyRep::Integer(dest_pad_id), PyRep::Integer(dest_pad_id), PyRep::Integer(origin_launcher_id)], &kw, None).await.expect_err("SessionChangeInProgress");
    assert_eq!(rejump_err.code, "SessionChangeInProgress");
}
