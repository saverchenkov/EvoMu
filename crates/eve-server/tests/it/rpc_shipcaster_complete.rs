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
use eve_server::services::cluster_macho_misc_services::{
    LandingPadRecord, SHIPCASTER_ROTATION_INTERVAL_SECS, starter_launchers_from_sde,
    with_misc_state,
};
use glam::DVec3;
use std::collections::{HashMap, HashSet};

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
    eve_testkit::attest(&[
        "rpc:shipCasterLandingPadMgr.GetAllLandingPads",
        "rpc:shipCasterLandingPadMgr.GetFactionLandingPads",
        "rpc:shipCasterLauncherMgr.GetFactionsWithShipcaster",
    ]);
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = SessionState::new(30_101);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

    let kw = HashMap::new();
    let char_id = 90_000_001;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None).await.expect("SelectCharacterID");

    // 1. GetAllLandingPads returns 8 empire FW beacons (2 per faction), with at most 1 linked per faction
    let all_pads_rep = ctx.dispatch_rpc(&mut sess, "shipCasterLandingPadMgr", None, "GetAllLandingPads", &[], &kw, None).await.expect("GetAllLandingPads");
    let PyRep::List(all_pads) = all_pads_rep else { panic!("GetAllLandingPads must return a List"); };
    assert_eq!(all_pads.len(), 8, "Must seed 8 canonical Empire FW landing pads (2 per faction)");

    let expected_factions = [500001, 500002, 500003, 500004];
    let mut linked_per_faction: HashMap<i64, usize> = HashMap::new();
    for pad in &all_pads {
        let fid = kv_i64(pad, "factionID");
        assert!(kv_i64(pad, "itemID") > 0, "Landing pad itemID must be positive");
        assert!(kv_i64(pad, "solarSystemID") > 0, "Landing pad solarSystemID must be valid");
        assert!(expected_factions.contains(&fid));
        assert!(kv_bool(pad, "isBuilt") && kv_bool(pad, "canBeLinked") && !kv_bool(pad, "isDisrupted"));
        if kv_bool(pad, "isLinked") {
            *linked_per_faction.entry(fid).or_insert(0) += 1;
        }
    }
    for fid in expected_factions {
        assert_eq!(linked_per_faction.get(&fid).copied(), Some(1), "Each faction must have exactly 1 active linked beacon at a time");
    }

    // 2. GetFactionLandingPads & 6-minute (360s) rotation across 0..=3 active beacons
    let expected_systems_by_faction: [(i64, [i64; 2]); 4] = [
        (500001, [30002811, 30002813]), // Caldari: Onatoh & Tama
        (500002, [30002056, 30002537]), // Minmatar: Resbroko & Amamake
        (500003, [30002185, 30003067]), // Amarr: NI-J0B & Huola
        (500004, [30003855, 30003838]), // Gallente: Mantenault & Oicx
    ];
    for &(fid, expected_sys) in &expected_systems_by_faction {
        let rep = ctx.dispatch_rpc(&mut sess, "shipCasterLandingPadMgr", None, "GetFactionLandingPads", &[PyRep::Integer(fid)], &kw, None).await.expect("GetFactionLandingPads");
        let PyRep::List(list) = rep else { panic!("Expected list"); };
        assert_eq!(list.len(), 2);
        let systems: Vec<i64> = list.iter().map(|p| kv_i64(p, "solarSystemID")).collect();
        assert_eq!(systems, expected_sys);
        let linked_before: Vec<i64> = list.iter().filter(|p| kv_bool(p, "isLinked")).map(|p| kv_i64(p, "itemID")).collect();
        assert_eq!(linked_before.len(), 1);

        // Advance clock by 360 seconds (6 minutes): linked beacon must cycle to the other beacon
        ctx.advance_secs(SHIPCASTER_ROTATION_INTERVAL_SECS);
        let rep_after = ctx.dispatch_rpc(&mut sess, "shipCasterLandingPadMgr", None, "GetFactionLandingPads", &[PyRep::Integer(fid)], &kw, None).await.expect("GetFactionLandingPads after 360s");
        let PyRep::List(list_after) = rep_after else { panic!("Expected list"); };
        let linked_after: Vec<i64> = list_after.iter().filter(|p| kv_bool(p, "isLinked")).map(|p| kv_i64(p, "itemID")).collect();
        assert_eq!(linked_after.len(), 1);
        assert_ne!(linked_before, linked_after, "Active beacon must rotate every 6 minutes (360s)");
    }

    // Add a 3rd active beacon for Caldari (Nennamaila 30045344) and verify 3-beacon rotation
    with_misc_state(&ctx, |st| {
        st.landing_pads.push(LandingPadRecord {
            item_id: 104_000_009,
            type_id: 75929,
            solar_system_id: 30045344, // Nennamaila
            faction_id: 500001,
            is_built: true,
            link_timestamp: 1_700_000_000,
            is_disrupted: false,
            position: DVec3::new(100_000.0, 0.0, 0.0),
        });
    });
    let mut seen_linked_ids = HashSet::new();
    for _ in 0..3 {
        let rep = ctx.dispatch_rpc(&mut sess, "shipCasterLandingPadMgr", None, "GetFactionLandingPads", &[PyRep::Integer(500001)], &kw, None).await.expect("GetFactionLandingPads 3 beacons");
        let PyRep::List(list) = rep else { panic!("Expected list"); };
        assert_eq!(list.len(), 3);
        let linked: Vec<i64> = list.iter().filter(|p| kv_bool(p, "isLinked")).map(|p| kv_i64(p, "itemID")).collect();
        assert_eq!(linked.len(), 1);
        seen_linked_ids.insert(linked[0]);
        ctx.advance_secs(SHIPCASTER_ROTATION_INTERVAL_SECS);
    }
    assert_eq!(seen_linked_ids.len(), 3, "All 3 active beacons must cycle across three 6-minute windows");

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
    eve_testkit::attest(&[
        "rpc:starterShipcasterTravelMgr.CanCharacterJump",
        "rpc:starterShipcasterTravelMgr.CmdJumpThroughStarterShipcaster",
        "feat:navigation.starter-shipcaster",
    ]);
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = SessionState::new(30_102);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

    let kw = HashMap::new();
    let char_id = 90_000_001;
    let starter_sys: u32 = 30000141; // Kisogo (State War Academy, school_id 17)
    let hq_sys: u32 = 30045324;      // Onnamon (Caldari FW HQ)
    let starter_launcher_id: i64 = 105_000_117;

    // Verify all 12 SDE starter schools (school_id 11..=22) map to canonical empire FW HQs
    let sde_starters = starter_launchers_from_sde(&ctx.sde);
    assert_eq!(sde_starters.len(), 12, "All 12 empire starter school systems must have Rookie Shipcasters");
    let expected_hq_by_school: [(i64, u32, u32); 12] = [
        (105_000_111, 30003489, 30002974), // Chaven -> Mehatoor
        (105_000_112, 30005042, 30002974), // Sehmy -> Mehatoor
        (105_000_113, 30001672, 30002974), // Emrayur -> Mehatoor
        (105_000_114, 30002547, 30002055), // Ammold -> Amo
        (105_000_115, 30002505, 30002055), // Hulm -> Amo
        (105_000_116, 30003410, 30002055), // Ryddinjorn -> Amo
        (105_000_117, 30000141, 30045324), // Kisogo -> Onnamon
        (105_000_118, 30001392, 30045324), // Amsen -> Onnamon
        (105_000_119, 30001407, 30045324), // Todaki -> Onnamon
        (105_000_120, 30004971, 30003788), // Duripant -> Intaki
        (105_000_121, 30002715, 30003788), // Bourynes -> Intaki
        (105_000_122, 30005305, 30003788), // Cistuvaert -> Intaki
    ];
    for (item_id, sys_id, dest_hq) in expected_hq_by_school {
        let rec = sde_starters.iter().find(|s| s.item_id == item_id).expect("starter school record");
        assert_eq!(rec.solar_system_id, sys_id);
        assert_eq!(rec.staging_system_id, dest_hq);
    }

    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None).await.expect("SelectCharacterID");
    let ship_id = sess.ship_id.unwrap_or(100_000_001);

    // 1. CanCharacterJump without weapons/criminal timer in starter hull
    let can_jump = ctx.dispatch_rpc(&mut sess, "starterShipcasterTravelMgr", None, "CanCharacterJump", &[], &kw, None).await.expect("CanCharacterJump");
    assert_eq!(can_jump, PyRep::Bool(true));

    // 2. Docked jump rejected
    sess.station_id = Some(60000001);
    let docked_err = ctx.dispatch_rpc(&mut sess, "starterShipcasterTravelMgr", None, "CmdJumpThroughStarterShipcaster", &[PyRep::Integer(starter_launcher_id)], &kw, None).await.expect_err("Docked jump fail");
    assert_eq!(docked_err.code, "CantDoThatWhileDocked");
    sess.station_id = None;

    // Undock to Kisogo starter system
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(starter_sys);
    ctx.db.with_mut_snapshot(|s| if let Some(ch) = s.characters.get_mut(&char_id) { ch.solar_system_id = starter_sys; ch.station_id = None; ch.structure_id = None; });
    let handle_starter = ctx.systems.get_or_spawn(starter_sys);

    // 3. ShipNotInSpace error
    let missing_err = ctx.dispatch_rpc(&mut sess, "starterShipcasterTravelMgr", None, "CmdJumpThroughStarterShipcaster", &[PyRep::Integer(starter_launcher_id)], &kw, None).await.expect_err("ShipNotInSpace");
    assert_eq!(missing_err.code, "ShipNotInSpace");

    // Spawn ball far away (50,000m; launcher is at 300,000m)
    spawn_test_ship(&ctx, &handle_starter, char_id, ship_id, 601, DVec3::new(50_000.0, 0.0, 0.0)).await;

    // 4. Invalid target ID & Outside operational range
    let zero_err = ctx.dispatch_rpc(&mut sess, "starterShipcasterTravelMgr", None, "CmdJumpThroughStarterShipcaster", &[PyRep::Integer(0)], &kw, None).await.expect_err("TargetNotFound");
    assert_eq!(zero_err.code, "TargetNotFound");

    let range_err = ctx.dispatch_rpc(&mut sess, "starterShipcasterTravelMgr", None, "CmdJumpThroughStarterShipcaster", &[PyRep::Integer(starter_launcher_id)], &kw, None).await.expect_err("OutsideOperationalRange");
    assert_eq!(range_err.code, "OutsideOperationalRange");

    // Move ship within range (298,500m => 1,500m away from 300,000m launcher)
    let _ = handle_starter.remove_ball(ship_id).await;
    spawn_test_ship(&ctx, &handle_starter, char_id, ship_id, 601, DVec3::new(298_500.0, 0.0, 0.0)).await;

    // 5. Weapons & criminal timers block jump
    let now = ctx.now_unix();
    { let mut cw = ctx.crimewatch.write().unwrap(); cw.get_or_create(char_id).weapon_timer_until_unix = now + 60; }
    assert_eq!(ctx.dispatch_rpc(&mut sess, "starterShipcasterTravelMgr", None, "CanCharacterJump", &[], &kw, None).await.unwrap(), PyRep::Bool(false));
    let timer_err = ctx.dispatch_rpc(&mut sess, "starterShipcasterTravelMgr", None, "CmdJumpThroughStarterShipcaster", &[PyRep::Integer(starter_launcher_id)], &kw, None).await.expect_err("WeaponsTimerActive");
    assert_eq!(timer_err.code, "WeaponsTimerActive");
    { let mut cw = ctx.crimewatch.write().unwrap(); cw.get_or_create(char_id).weapon_timer_until_unix = 0; }

    { let mut cw = ctx.crimewatch.write().unwrap(); cw.get_or_create(char_id).criminal_flag_until_unix = now + 60; }
    assert_eq!(ctx.dispatch_rpc(&mut sess, "starterShipcasterTravelMgr", None, "CanCharacterJump", &[], &kw, None).await.unwrap(), PyRep::Bool(false));
    let crim_err = ctx.dispatch_rpc(&mut sess, "starterShipcasterTravelMgr", None, "CmdJumpThroughStarterShipcaster", &[PyRep::Integer(starter_launcher_id)], &kw, None).await.expect_err("CriminalTimerActive");
    assert_eq!(crim_err.code, "CriminalTimerActive");
    { let mut cw = ctx.crimewatch.write().unwrap(); cw.get_or_create(char_id).criminal_flag_until_unix = 0; }

    // 6. Cruiser & Capital ship rejected on Rookie Shipcaster
    ctx.db.with_mut_snapshot(|s| if let Some(ship) = s.items.get_mut(&ship_id) { ship.type_id = 621; }); // Caracal Cruiser
    assert_eq!(ctx.dispatch_rpc(&mut sess, "starterShipcasterTravelMgr", None, "CanCharacterJump", &[], &kw, None).await.unwrap(), PyRep::Bool(false));
    let cruiser_err = ctx.dispatch_rpc(&mut sess, "starterShipcasterTravelMgr", None, "CmdJumpThroughStarterShipcaster", &[PyRep::Integer(starter_launcher_id)], &kw, None).await.expect_err("Cruiser rejected");
    assert_eq!(cruiser_err.code, "ShipClassNotEligibleForStarterShipcaster");

    ctx.db.with_mut_snapshot(|s| if let Some(ship) = s.items.get_mut(&ship_id) { ship.type_id = 19720; }); // Revelation Dreadnought
    let cap_err = ctx.dispatch_rpc(&mut sess, "starterShipcasterTravelMgr", None, "CmdJumpThroughStarterShipcaster", &[PyRep::Integer(starter_launcher_id)], &kw, None).await.expect_err("CapitalShipCannotJumpShipcaster");
    assert_eq!(cap_err.code, "CapitalShipCannotJumpShipcaster");
    ctx.db.with_mut_snapshot(|s| if let Some(ship) = s.items.get_mut(&ship_id) { ship.type_id = 601; });

    // 7. Arbitrary destinationSystemID rejected; positive jump to Onnamon (30045324) succeeds
    let invalid_dest_err = ctx.dispatch_rpc(&mut sess, "starterShipcasterTravelMgr", None, "CmdJumpThroughStarterShipcaster", &[PyRep::Integer(30000142)], &kw, None).await.expect_err("Arbitrary destination system");
    assert_eq!(invalid_dest_err.code, "InvalidDestinationSystem");

    ctx.advance_secs(15);
    let jump_res = ctx.dispatch_rpc(&mut sess, "starterShipcasterTravelMgr", None, "CmdJumpThroughStarterShipcaster", &[PyRep::Integer(hq_sys as i64)], &kw, None).await.expect("Starter jump success to Onnamon");
    assert_eq!(jump_res, PyRep::None);
    assert_eq!(sess.solar_system_id, Some(hq_sys));

    assert!(handle_starter.get_ball_snapshot(ship_id).await.unwrap().is_none());
    let handle_hq = ctx.systems.get_or_spawn(hq_sys);
    assert!(handle_hq.get_ball_snapshot(ship_id).await.unwrap().is_some());

    ctx.db.with_snapshot(|s| {
        assert_eq!(s.characters.get(&char_id).unwrap().solar_system_id, hq_sys);
        assert_eq!(s.items.get(&ship_id).unwrap().location_id, hq_sys as i64);
    });
}

#[tokio::test]
async fn test_interstellar_shipcaster_travel_complete() {
    // parity: rpc:shipcasterTravelMgr.CmdJumpThroughShipcaster
    // parity: feat:navigation.shipcaster
    eve_testkit::attest(&[
        "rpc:shipcasterTravelMgr.CmdJumpThroughShipcaster",
        "feat:navigation.shipcaster",
    ]);
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = SessionState::new(30_103);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

    let kw = HashMap::new();
    let char_id = 90_000_001;
    let hq_sys: u32 = 30045324; // Onnamon (Caldari FW HQ)
    let origin_launcher_id: i64 = 105_000_001;

    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None).await.expect("SelectCharacterID");
    let ship_id = sess.ship_id.unwrap_or(100_000_001);

    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(hq_sys);
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&char_id) { ch.solar_system_id = hq_sys; ch.station_id = None; ch.structure_id = None; }
        if let Some(ship) = s.items.get_mut(&ship_id) { ship.location_id = hq_sys as i64; }
    });

    let handle_hq = ctx.systems.get_or_spawn(hq_sys);
    spawn_test_ship(&ctx, &handle_hq, char_id, ship_id, 601, DVec3::new(198_000.0, 0.0, 0.0)).await;

    // Determine currently linked and currently unlinked Caldari warzone beacons
    let pads_rep = ctx.dispatch_rpc(&mut sess, "shipCasterLandingPadMgr", None, "GetFactionLandingPads", &[PyRep::Integer(500001)], &kw, None).await.expect("GetFactionLandingPads");
    let PyRep::List(caldari_pads) = pads_rep else { panic!("Expected List"); };
    let linked_pad = caldari_pads.iter().find(|p| kv_bool(p, "isLinked")).expect("1 linked Caldari pad");
    let unlinked_pad = caldari_pads.iter().find(|p| !kv_bool(p, "isLinked")).expect("1 unlinked Caldari pad");
    let linked_pad_id = kv_i64(linked_pad, "itemID");
    let linked_pad_sys = kv_i64(linked_pad, "solarSystemID") as u32;
    let unlinked_pad_id = kv_i64(unlinked_pad, "itemID");
    let unlinked_pad_sys = kv_i64(unlinked_pad, "solarSystemID") as u32;

    // 1. Non-existent landing pad rejected
    let missing_err = ctx.dispatch_rpc(&mut sess, "shipcasterTravelMgr", None, "CmdJumpThroughShipcaster", &[PyRep::Integer(origin_launcher_id), PyRep::Integer(linked_pad_sys as i64), PyRep::Integer(999_999)], &kw, None).await.expect_err("Non-existent pad");
    assert_eq!(missing_err.code, "LandingPadNotFound");

    // 2. Hostile militia & neutral character rejected
    ctx.db.with_mut_snapshot(|s| if let Some(ch) = s.characters.get_mut(&char_id) { ch.war_faction_id = Some(500002); });
    let hostile_err = ctx.dispatch_rpc(&mut sess, "shipcasterTravelMgr", None, "CmdJumpThroughShipcaster", &[PyRep::Integer(linked_pad_id), PyRep::Integer(linked_pad_id), PyRep::Integer(origin_launcher_id)], &kw, None).await.expect_err("Hostile militia");
    assert_eq!(hostile_err.code, "FactionShipcasterAccessDenied");

    ctx.db.with_mut_snapshot(|s| if let Some(ch) = s.characters.get_mut(&char_id) { ch.war_faction_id = None; });
    let neutral_err = ctx.dispatch_rpc(&mut sess, "shipcasterTravelMgr", None, "CmdJumpThroughShipcaster", &[PyRep::Integer(linked_pad_id), PyRep::Integer(linked_pad_id), PyRep::Integer(origin_launcher_id)], &kw, None).await.expect_err("Neutral militia");
    assert_eq!(neutral_err.code, "FactionShipcasterAccessDenied");

    // Enroll in Caldari militia (500001)
    ctx.db.with_mut_snapshot(|s| if let Some(ch) = s.characters.get_mut(&char_id) { ch.war_faction_id = Some(500001); });

    // 3. Capital ship rejected
    ctx.db.with_mut_snapshot(|s| if let Some(ship) = s.items.get_mut(&ship_id) { ship.type_id = 19720; });
    let cap_err = ctx.dispatch_rpc(&mut sess, "shipcasterTravelMgr", None, "CmdJumpThroughShipcaster", &[PyRep::Integer(linked_pad_id), PyRep::Integer(linked_pad_id), PyRep::Integer(origin_launcher_id)], &kw, None).await.expect_err("Capital ship");
    assert_eq!(cap_err.code, "CapitalShipCannotJumpShipcaster");
    ctx.db.with_mut_snapshot(|s| if let Some(ship) = s.items.get_mut(&ship_id) { ship.type_id = 601; });

    // 4. Attempting to jump to the beacon NOT linked in this 6-minute window is rejected
    let unlinked_window_err = ctx.dispatch_rpc(&mut sess, "shipcasterTravelMgr", None, "CmdJumpThroughShipcaster", &[PyRep::Integer(origin_launcher_id), PyRep::Integer(unlinked_pad_sys as i64), PyRep::Integer(unlinked_pad_id)], &kw, None).await.expect_err("Unlinked beacon in current 6m window");
    assert_eq!(unlinked_window_err.code, "LandingPadNotLinked");

    // 5. Positive jump to currently linked warzone beacon succeeds
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, char_id);
    let jump_res = ctx.dispatch_rpc(&mut sess, "shipcasterTravelMgr", None, "CmdJumpThroughShipcaster", &[PyRep::Integer(linked_pad_id), PyRep::Integer(linked_pad_id), PyRep::Integer(origin_launcher_id)], &kw, None).await.expect("CmdJumpThroughShipcaster");
    assert_eq!(jump_res, PyRep::None);
    assert_eq!(sess.solar_system_id, Some(linked_pad_sys));

    assert!(handle_hq.get_ball_snapshot(ship_id).await.unwrap().is_none());
    let handle_warzone = ctx.systems.get_or_spawn(linked_pad_sys);
    assert!(handle_warzone.get_ball_snapshot(ship_id).await.unwrap().is_some());

    // 6. Return pilot to Onnamon HQ and advance clock by 360 seconds (6 minutes):
    // Previously unlinked beacon is now the active linked target!
    let _ = handle_warzone.remove_ball(ship_id).await;
    spawn_test_ship(&ctx, &handle_hq, char_id, ship_id, 601, DVec3::new(198_000.0, 0.0, 0.0)).await;
    sess.solar_system_id = Some(hq_sys);
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&char_id) { ch.solar_system_id = hq_sys; }
        if let Some(ship) = s.items.get_mut(&ship_id) { ship.location_id = hq_sys as i64; }
    });

    // Immediate re-jump before session timer expires fails
    let rejump_err = ctx.dispatch_rpc(&mut sess, "shipcasterTravelMgr", None, "CmdJumpThroughShipcaster", &[PyRep::Integer(linked_pad_id), PyRep::Integer(linked_pad_id), PyRep::Integer(origin_launcher_id)], &kw, None).await.expect_err("SessionChangeInProgress");
    assert_eq!(rejump_err.code, "SessionChangeInProgress");

    // Advance 360 seconds (clears 10s session timer AND rotates Shipcaster to the second beacon)
    ctx.advance_secs(SHIPCASTER_ROTATION_INTERVAL_SECS);
    let rotated_jump = ctx.dispatch_rpc(&mut sess, "shipcasterTravelMgr", None, "CmdJumpThroughShipcaster", &[PyRep::Integer(origin_launcher_id), PyRep::Integer(unlinked_pad_sys as i64), PyRep::Integer(unlinked_pad_id)], &kw, None).await.expect("Rotated CmdJumpThroughShipcaster after 360s");
    assert_eq!(rotated_jump, PyRep::None);
    assert_eq!(sess.solar_system_id, Some(unlinked_pad_sys));
}
