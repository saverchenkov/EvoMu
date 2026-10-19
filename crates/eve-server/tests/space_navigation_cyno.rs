//! `crates/eve-server/tests/space_navigation_cyno.rs`
//!
//! Track 6: Space & Navigation Polish (SP-2, SP-3, SP-5, S-2, S-3)
//!
//! Comprehensive integration and specification tests for:
//! - `SP-2`: Spatial grid bubbles: interdiction spheres (bubbles), mobile warp disruptors, bubble transition and trapping warp exits within bubble radius.
//! - `SP-3`: Beyonce command set: velocity, agility, approach, orbit, keep at range, align, warp to bookmark / celestial.
//! - `SP-5`: Dock and undock: approach station docking perimeter radius, dock delay, undock along station undock vector.
//! - `S-2`: Fighter manager (`fighterMgr`): load tubes, launch, recall, movement, fighter abilities.
//! - `S-3`: Pharolux cyno beacon: `structureCynoBeaconMgr.CmdJumpToStructureBeacon/CmdBridgeToStructureBeacon/CmdGroupJumpToStructureBeacon` jumping to a beacon structure with range and fuel checks.
//!
//! // parity: plan:SP-2, plan:SP-3, plan:SP-5, plan:S-2, plan:S-3
//! // parity: feat:modules.interdiction-sphere, feat:modules.mobile-warp-disruptor, feat:modules.interdiction-nullifier, feat:dogma.velocity-agility, feat:modules.afterburner, feat:modules.microwarpdrive, feat:modules.micro-jump-drive, feat:navigation.approach, feat:ships.board-ship, feat:navigation.undock, feat:navigation.dock-station, feat:navigation.dock-structure, feat:drones.fighter-load-tubes, feat:drones.fighter-launch-recall, feat:drones.fighter-movement, feat:drones.fighter-abilities, feat:drones.fighter-abandon, feat:navigation.pharolux-beacon

#![forbid(unsafe_code)]

mod common;

use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_destiny::{Ball, MovementMode};
use eve_dogma::{eval_propulsion_module, DogmaItemState};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

#[tokio::test]
async fn test_sp2_spatial_bubbles_interdiction_sphere_mobile_warp_disruptor_and_nullifier() {
    // parity: plan:SP-2
    // parity: feat:modules.interdiction-sphere, feat:modules.mobile-warp-disruptor, feat:modules.interdiction-nullifier
    eve_testkit::attest(&[
        "plan:SP-2",
        "feat:modules.interdiction-sphere",
        "feat:modules.mobile-warp-disruptor",
        "feat:modules.interdiction-nullifier",
    ]);

    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(2001);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");

    let sys_id = 30_000_142;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let ship_id = 100_000_001;

    // 1. Deploy Mobile Warp Disruptor (feat:modules.mobile-warp-disruptor)
    let mwd_bubble_id = 8001;
    let mwd_bubble_pos = DVec3::new(12_000_000.0, 0.0, 0.0);
    let mwd_bubble_radius = 20_000.0;
    let ok_mwd = sys_handle
        .add_mobile_warp_disruptor(mwd_bubble_id, mwd_bubble_pos, mwd_bubble_radius)
        .await
        .expect("add_mobile_warp_disruptor");
    assert!(ok_mwd, "Mobile warp disruptor must be spawned on grid");

    // 2. Deploy Interdiction Sphere (dictor bubble) (feat:modules.interdiction-sphere)
    let sphere_bubble_id = 8002;
    let sphere_bubble_pos = DVec3::new(25_000_000.0, 0.0, 0.0);
    let sphere_bubble_radius = 20_000.0;
    let ok_sphere = sys_handle
        .add_interdiction_sphere(sphere_bubble_id, sphere_bubble_pos, sphere_bubble_radius)
        .await
        .expect("add_interdiction_sphere");
    assert!(ok_sphere, "Interdiction sphere must be spawned on grid");

    // Verify bubble points are scrambled
    assert!(
        sys_handle.is_ball_scrambled(mwd_bubble_id).await.unwrap_or(false),
        "Position at mobile warp disruptor must be scrambled"
    );

    // 3. Move ship inside bubble -> verify ship becomes scrambled and cannot warp
    sys_handle.set_ball_position(ship_id, mwd_bubble_pos).await;
    assert!(
        sys_handle.is_ball_scrambled(ship_id).await.expect("is_ball_scrambled"),
        "Ship inside bubble must be scrambled"
    );

    let warp_attempt = sys_handle
        .set_movement_mode(
            ship_id,
            MovementMode::new_warp(mwd_bubble_pos, DVec3::new(50_000_000.0, 0.0, 0.0), 3.0),
        )
        .await;
    assert!(
        warp_attempt.is_err(),
        "Normal ship inside bubble must NOT be able to initiate warp"
    );
    assert!(
        warp_attempt.unwrap_err().contains("WarpScrambled"),
        "Warp error must indicate warp drive disabled by disruption"
    );

    // 4. Interdiction Nullifier (feat:modules.interdiction-nullifier)
    // Enable nullifier on ship -> verify ship is no longer scrambled by bubbles and can warp directly out!
    let null_ok = sys_handle
        .set_interdiction_nullified(ship_id, true)
        .await
        .expect("set_interdiction_nullified");
    assert!(null_ok, "Must successfully set interdiction nullification");
    assert!(
        !sys_handle.is_ball_scrambled(ship_id).await.expect("is_ball_scrambled"),
        "Interdiction nullified ship must NOT be scrambled by bubbles"
    );

    let null_warp = sys_handle
        .set_movement_mode(
            ship_id,
            MovementMode::new_warp(mwd_bubble_pos, DVec3::new(50_000_000.0, 0.0, 0.0), 3.0),
        )
        .await;
    assert!(
        null_warp.is_ok(),
        "Interdiction nullified ship CAN warp directly out of bubble"
    );
}

#[tokio::test]
async fn test_sp3_beyonce_command_set_navigation_agility_propulsion_and_mjd() {
    // parity: plan:SP-3
    // parity: feat:dogma.velocity-agility, feat:modules.afterburner, feat:modules.microwarpdrive, feat:modules.micro-jump-drive, feat:navigation.approach
    eve_testkit::attest(&[
        "plan:SP-3",
        "feat:dogma.velocity-agility",
        "feat:modules.afterburner",
        "feat:modules.microwarpdrive",
        "feat:modules.micro-jump-drive",
        "feat:navigation.approach",
    ]);

    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(2002);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    let undock_res = ctx
        .dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let PyRep::KeyVal(ukv) = undock_res else {
        panic!("Expected KeyVal from Undock");
    };
    let cur_b_id = ukv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;

    let sys_id = 30_000_142;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let ship_id = 100_000_001;

    // 1. Dogma Velocity & Agility calculations (feat:dogma.velocity-agility)
    let rifter = DogmaItemState::from_sde(ship_id, 587, &ctx.sde);
    let base_vel = rifter.get_attribute(eve_sde::attr_ids::MAX_VELOCITY, &ctx.sde);
    let base_mass = rifter.get_attribute(eve_sde::attr_ids::MASS, &ctx.sde);
    let base_inertia = rifter.get_attribute(eve_sde::attr_ids::AGILITY, &ctx.sde);
    assert_eq!(base_vel, 365.0);
    assert_eq!(base_mass, 1_067_000.0);
    assert!((base_inertia - 3.2).abs() < 1e-4);

    let tau = (base_inertia * base_mass) / 1_000_000.0;
    let t_align = (-0.25_f64.ln()) * tau;
    assert!((tau - 3.414).abs() < 1e-2, "Align time constant tau must be ~3.41 s: {tau}");
    assert!((t_align - 4.733).abs() < 1e-2, "Align time must be ~4.73 s: {t_align}");

    // 2. Afterburner (feat:modules.afterburner)
    let ab2 = DogmaItemState::from_sde(2001, 438, &ctx.sde); // 1MN Afterburner II
    let ab_active = eval_propulsion_module(&rifter, &ab2, true, false, false, &ctx.sde);
    assert!((ab_active.max_velocity - base_vel * 2.35).abs() < 1e-4, "AB boosts speed by 135%");
    assert_eq!(ab_active.mass, base_mass + 500_000.0, "AB adds 500,000 kg mass");
    assert_eq!(ab_active.signature_radius, rifter.get_attribute(eve_sde::attr_ids::SIGNATURE_RADIUS, &ctx.sde), "AB does NOT bloom signature");
    // AB operates under warp scramble
    let ab_scram = eval_propulsion_module(&rifter, &ab2, true, false, true, &ctx.sde);
    assert!(ab_scram.can_activate, "AB must be able to activate under warp scramble");

    // 3. Microwarpdrive (feat:modules.microwarpdrive)
    let mwd2 = DogmaItemState::from_sde(2002, 440, &ctx.sde); // 5MN Microwarpdrive II
    let mwd_active = eval_propulsion_module(&rifter, &mwd2, true, false, false, &ctx.sde);
    assert!((mwd_active.max_velocity - base_vel * 6.10).abs() < 1e-4, "MWD boosts speed by 510%");
    assert_eq!(mwd_active.mass, base_mass + 500_000.0, "MWD adds 500,000 kg mass");
    assert!((mwd_active.signature_radius - 35.0 * 5.75).abs() < 1e-4, "MWD blooms signature by 475%");
    // MWD is disabled under warp scramble
    let mwd_scram = eval_propulsion_module(&rifter, &mwd2, true, false, true, &ctx.sde);
    assert!(!mwd_scram.can_activate, "MWD CANNOT activate under warp scramble");

    // 4. Beyonce Command Set: SetSpeedFraction, Approach, KeepAtRange, Orbit, Align (feat:navigation.approach)
    let spd_res = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(cur_b_id),
            "CmdSetSpeedFraction",
            &[PyRep::Real(0.75)],
            &kw,
            None,
        )
        .await
        .expect("CmdSetSpeedFraction");
    assert!(matches!(spd_res, PyRep::Integer(_)));

    let approach_res = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(cur_b_id),
            "CmdApproach",
            &[PyRep::Integer(60003760)],
            &kw,
            None,
        )
        .await
        .expect("CmdApproach");
    assert!(matches!(approach_res, PyRep::Integer(_)));

    let keep_res = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(cur_b_id),
            "CmdKeepAtRange",
            &[PyRep::Integer(60003760), PyRep::Real(15_000.0)],
            &kw,
            None,
        )
        .await
        .expect("CmdKeepAtRange");
    assert!(matches!(keep_res, PyRep::Integer(_)));

    let orbit_res = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(cur_b_id),
            "CmdOrbit",
            &[PyRep::Integer(60003760), PyRep::Real(2_500.0)],
            &kw,
            None,
        )
        .await
        .expect("CmdOrbit");
    assert!(matches!(orbit_res, PyRep::Integer(_)));

    let align_res = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(cur_b_id),
            "CmdAlign",
            &[PyRep::Integer(60003760)],
            &kw,
            None,
        )
        .await
        .expect("CmdAlign");
    assert!(matches!(align_res, PyRep::Integer(_)));

    // 5. Micro Jump Drive (feat:modules.micro-jump-drive)
    let pos_before = sys_handle
        .get_ball_snapshot(ship_id)
        .await
        .unwrap()
        .unwrap()
        .1
        .position;
    let mjd_res = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(cur_b_id),
            "CmdMicroJump",
            &[PyRep::Real(100_000.0)],
            &kw,
            None,
        )
        .await
        .expect("CmdMicroJump");
    let PyRep::KeyVal(mjd_kv) = mjd_res else {
        panic!("Expected KeyVal from CmdMicroJump");
    };
    let x_new = mjd_kv.get("x").unwrap().as_f64().unwrap();
    let y_new = mjd_kv.get("y").unwrap().as_f64().unwrap();
    let z_new = mjd_kv.get("z").unwrap().as_f64().unwrap();
    let pos_after = DVec3::new(x_new, y_new, z_new);
    let jump_dist = pos_before.distance(pos_after);
    assert!(
        (jump_dist - 100_000.0).abs() < 1.0,
        "Micro Jump Drive must teleport exactly 100 km forward: got {jump_dist}"
    );
}

#[tokio::test]
async fn test_sp5_docking_and_undocking_station_structure_and_boarding() {
    // parity: plan:SP-5
    // parity: feat:ships.board-ship, feat:navigation.undock, feat:navigation.dock-station, feat:navigation.dock-structure
    eve_testkit::attest(&[
        "plan:SP-5",
        "feat:ships.board-ship",
        "feat:navigation.undock",
        "feat:navigation.dock-station",
        "feat:navigation.dock-structure",
    ]);

    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(2003);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    let station_id = 60003760;
    assert_eq!(sess.station_id, Some(station_id));

    // 1. Board ship in station hangar (feat:ships.board-ship)
    let board_res = ctx
        .dispatch_rpc(
            &mut sess,
            "ship",
            None,
            "Board",
            &[PyRep::Integer(100_000_001)],
            &kw,
            None,
        )
        .await
        .expect("Board in station");
    assert!(matches!(board_res, PyRep::KeyVal(_)));

    // 2. Undock from station along undock vector (feat:navigation.undock)
    let undock_res = ctx
        .dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let PyRep::KeyVal(ukv) = undock_res else {
        panic!("Expected KeyVal from Undock");
    };
    assert_eq!(ukv.get("shipID").unwrap().as_i64().unwrap(), 100_000_001);
    assert_eq!(sess.station_id, None, "Pilot must be undocked in space");

    let sys_id = 30_000_142;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let (_, ball) = sys_handle
        .get_ball_snapshot(100_000_001)
        .await
        .unwrap()
        .unwrap();

    let station_pos = ctx.sde.stations[&station_id].position;
    let expected_spawn = station_pos + DVec3::new(1_500.0, 0.0, 0.0);
    assert!(
        ball.position.distance(expected_spawn) < 1.0,
        "Undock spawn position must be along station undock vector (+1500m)"
    );
    assert_eq!(
        ball.mode,
        MovementMode::GotoDirection { direction: DVec3::X },
        "Undocking ship must head along station undock vector"
    );

    // 3. Docking perimeter check: ship at spawn (+1500m) is within 2,500m docking perimeter
    // Move ship far away (50 km) -> docking must reject with OutsideDockingPerimeter
    sys_handle
        .set_ball_position(100_000_001, station_pos + DVec3::new(50_000.0, 0.0, 0.0))
        .await;
    let far_dock = ctx
        .dispatch_rpc(
            &mut sess,
            "stationSvc",
            None,
            "Dock",
            &[PyRep::Integer(station_id)],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        far_dock.unwrap_err().code,
        "OutsideDockingPerimeter",
        "Docking from 50 km must fail outside docking perimeter"
    );

    // Approach station to within docking perimeter (1,000 m <= 2,500 m) -> Dock succeeds (feat:navigation.dock-station)
    sys_handle
        .set_ball_position(100_000_001, station_pos + DVec3::new(1_000.0, 0.0, 0.0))
        .await;
    let dock_ok = ctx
        .dispatch_rpc(
            &mut sess,
            "stationSvc",
            None,
            "Dock",
            &[PyRep::Integer(station_id)],
            &kw,
            None,
        )
        .await
        .expect("Dock at station within docking perimeter");
    assert!(matches!(dock_ok, PyRep::KeyVal(_)));
    assert_eq!(sess.station_id, Some(station_id));

    // 4. Upwell structure docking (feat:navigation.dock-structure)
    // Undock again, create an Upwell structure in system, and dock there
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock again");

    let struct_id = 1_040_000_000_100_i64;
    let struct_pos = DVec3::new(200_000.0, 0.0, 0.0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35832, // Astrahus
                solar_system_id: sys_id,
                owner_corp_id: 1000001,
                name: "Astrahus Staging".into(),
                position: struct_pos.to_array(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 100_000.0,
                armor_hp: 100_000.0,
                hull_hp: 100_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
            },
        );
    });

    // Move within structure docking perimeter (1,500 m)
    sys_handle
        .set_ball_position(100_000_001, struct_pos + DVec3::new(1_500.0, 0.0, 0.0))
        .await;
    let struct_dock = ctx
        .dispatch_rpc(
            &mut sess,
            "structureDocking",
            None,
            "Dock",
            &[PyRep::Integer(struct_id)],
            &kw,
            None,
        )
        .await
        .expect("Dock at Upwell structure");
    assert!(matches!(struct_dock, PyRep::KeyVal(_)));
    assert_eq!(sess.structure_id, Some(struct_id));
}

#[tokio::test]
async fn test_s2_fighter_manager_load_launch_recall_movement_abilities_abandon() {
    // parity: plan:S-2
    // parity: feat:drones.fighter-load-tubes, feat:drones.fighter-launch-recall, feat:drones.fighter-movement, feat:drones.fighter-abilities, feat:drones.fighter-abandon
    eve_testkit::attest(&[
        "plan:S-2",
        "feat:drones.fighter-load-tubes",
        "feat:drones.fighter-launch-recall",
        "feat:drones.fighter-movement",
        "feat:drones.fighter-abilities",
        "feat:drones.fighter-abandon",
    ]);

    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(2004);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    let ship_id = 100_000_001;

    // Seed 2 fighter squadrons into ship's FighterBay (flag 158)
    let fighter1_item_id = 70_000_001;
    let fighter2_item_id = 70_000_002;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            fighter1_item_id,
            InventoryItemRecord {
                item_id: fighter1_item_id,
                type_id: 40358, // Dragonfly I (Light Fighter)
                owner_id: 90_000_001,
                location_id: ship_id,
                flag: InventoryFlag::FighterBay,
                quantity: 9,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.items.insert(
            fighter2_item_id,
            InventoryItemRecord {
                item_id: fighter2_item_id,
                type_id: 40359, // Firbolg I (Light Fighter)
                owner_id: 90_000_001,
                location_id: ship_id,
                flag: InventoryFlag::FighterBay,
                quantity: 9,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    // 1. GetFightersForShip: verify fighters in bay
    let list_res = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "GetFightersForShip",
            &[PyRep::Integer(ship_id)],
            &kw,
            None,
        )
        .await
        .expect("GetFightersForShip");
    let PyRep::Rowset { columns, .. } = list_res else {
        panic!("Expected rowset from GetFightersForShip");
    };
    assert_eq!(columns, vec!["itemID", "typeID", "squadronSize"]);

    // 2. Load fighter tubes (feat:drones.fighter-load-tubes)
    let load_res1 = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "LoadFightersToTube",
            &[PyRep::Integer(0), PyRep::Integer(fighter1_item_id)],
            &kw,
            None,
        )
        .await
        .expect("Load tube 0");
    assert!(matches!(load_res1, PyRep::KeyVal(_)));

    let load_res2 = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "LoadFightersToTube",
            &[PyRep::Integer(1), PyRep::Integer(fighter2_item_id)],
            &kw,
            None,
        )
        .await
        .expect("Load tube 1");
    assert!(matches!(load_res2, PyRep::KeyVal(_)));

    // 3. Undock and launch fighters into space (feat:drones.fighter-launch-recall)
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");

    let launch_res = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "LaunchFightersFromTubes",
            &[PyRep::List(vec![PyRep::Integer(0), PyRep::Integer(1)])],
            &kw,
            None,
        )
        .await
        .expect("Launch fighters");
    let PyRep::List(launched_balls) = launch_res else {
        panic!("Expected List from LaunchFightersFromTubes");
    };
    assert_eq!(launched_balls.len(), 2, "Both loaded tubes must launch");
    let f1_ball_id = launched_balls[0].as_i64().unwrap();
    let f2_ball_id = launched_balls[1].as_i64().unwrap();

    let sys_id = 30_000_142;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    assert!(
        sys_handle.get_ball_snapshot(f1_ball_id).await.unwrap().is_some(),
        "Fighter 1 must exist as ball in ballpark"
    );

    // 4. Fighter Movement (feat:drones.fighter-movement)
    let move_res = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "ExecuteMovementCommandOnFighters",
            &[
                PyRep::List(vec![PyRep::Integer(f1_ball_id)]),
                PyRep::String("GOTO_POINT".into()),
                PyRep::Real(10_000.0),
                PyRep::Real(0.0),
                PyRep::Real(0.0),
            ],
            &kw,
            None,
        )
        .await
        .expect("ExecuteMovementCommandOnFighters GOTO_POINT");
    assert!(matches!(move_res, PyRep::Bool(true)));

    let orbit_res = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "ExecuteMovementCommandOnFighters",
            &[
                PyRep::List(vec![PyRep::Integer(f2_ball_id)]),
                PyRep::String("ORBIT".into()),
                PyRep::Integer(ship_id),
                PyRep::Real(2_000.0),
            ],
            &kw,
            None,
        )
        .await
        .expect("ExecuteMovementCommandOnFighters ORBIT");
    assert!(matches!(orbit_res, PyRep::Bool(true)));

    // 5. Fighter Abilities (feat:drones.fighter-abilities)
    // Spawn a target dummy drone near fighter 1 and lock it from the controlling ship
    let (_, f1_ball) = sys_handle
        .get_ball_snapshot(f1_ball_id)
        .await
        .unwrap()
        .unwrap();
    let dummy_target_id = 99_001;
    let dummy_ball = Ball {
        id: dummy_target_id,
        type_id: 282,
        position: f1_ball.position + DVec3::new(1_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 1.0,
        mass: 1_000.0,
        radius: 10.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    sys_handle
        .spawn_ball_with_combat(dummy_ball, ctx.build_type_combat_state(282))
        .await
        .expect("spawn dummy");
    sys_handle
        .add_target_lock(
            ship_id,
            eve_destiny::TargetLock {
                target_id: dummy_target_id,
                lock_time_ms: 1_000.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(100_000.0),
            },
        )
        .await
        .expect("lock dummy target");

    // Slot 0: Primary Attack deals damage
    let cs_before = sys_handle.get_combat_state(dummy_target_id).await.unwrap().unwrap();
    let ability_res = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "CmdActivateAbilitySlots",
            &[
                PyRep::List(vec![PyRep::Integer(f1_ball_id)]),
                PyRep::Integer(0),
                PyRep::Integer(dummy_target_id),
            ],
            &kw,
            None,
        )
        .await
        .expect("CmdActivateAbilitySlots Slot 0");
    assert!(matches!(ability_res, PyRep::KeyVal(_)));
    let cs_after = sys_handle.get_combat_state(dummy_target_id).await.unwrap().unwrap();
    assert!(
        cs_after.shield_hp < cs_before.shield_hp,
        "Fighter slot 0 attack must deal damage to target"
    );

    // Slot 1: MWD / Boost
    let mwd_res = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "CmdActivateAbilitySlots",
            &[PyRep::List(vec![PyRep::Integer(f1_ball_id)]), PyRep::Integer(1)],
            &kw,
            None,
        )
        .await
        .expect("CmdActivateAbilitySlots Slot 1 MWD");
    assert!(matches!(mwd_res, PyRep::KeyVal(_)));

    // 6. Abandon fighters (feat:drones.fighter-abandon)
    let abandon_res = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "CmdAbandonFighter",
            &[PyRep::Integer(f1_ball_id)],
            &kw,
            None,
        )
        .await
        .expect("CmdAbandonFighter");
    let PyRep::List(ab_list) = abandon_res else {
        panic!("Expected List from CmdAbandonFighter");
    };
    assert!(!ab_list.is_empty(), "Fighter must be abandoned in space");
    assert!(
        ctx.active_fighters.read().unwrap()[&f1_ball_id].is_abandoned,
        "Fighter 1 must be marked abandoned"
    );

    // 7. Recall fighters (feat:drones.fighter-launch-recall)
    // Recall all active fighters back to ship
    let recall_res = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "RecallFightersToTubes",
            &[PyRep::List(vec![PyRep::Integer(f1_ball_id), PyRep::Integer(f2_ball_id)])],
            &kw,
            None,
        )
        .await
        .expect("RecallFightersToTubes");
    let PyRep::List(rec_list) = recall_res else {
        panic!("Expected List from RecallFightersToTubes");
    };
    assert_eq!(rec_list.len(), 2, "Both fighters must be recalled from space");
    assert!(
        sys_handle.get_ball_snapshot(f1_ball_id).await.unwrap().is_none(),
        "Recalled fighter ball must be removed from ballpark"
    );
}

#[tokio::test]
async fn test_s3_pharolux_cyno_beacon_jump_bridge_and_group_jump() {
    // parity: plan:S-3
    // parity: feat:navigation.pharolux-beacon
    eve_testkit::attest(&[
        "plan:S-3",
        "feat:navigation.pharolux-beacon",
    ]);

    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(2005);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    let ship_id = 100_000_001;
    let pilot_corp_id = sess.corporation_id.unwrap();

    // Seed Pharolux Cyno Beacon (type 35840) in low-sec Ohkunen (30_000_204, security 0.38, 4.80 LY from Jita)
    let dest_sys_id = 30_000_204;
    let beacon_struct_id = 1_040_000_000_200_i64;
    let beacon_pos = DVec3::new(450_000.0, 0.0, 0.0);

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            beacon_struct_id,
            UpwellStructureRecord {
                structure_id: beacon_struct_id,
                type_id: 35840, // Pharolux Cyno Beacon
                solar_system_id: dest_sys_id,
                owner_corp_id: pilot_corp_id,
                name: "Ohkunen Pharolux Beacon".into(),
                position: beacon_pos.to_array(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 50,
                shield_hp: 50_000.0,
                armor_hp: 50_000.0,
                hull_hp: 50_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
            },
        );

        // Put capital jump fuel (Helium Isotopes) in ship's SpecializedFuelBay
        snap.items.insert(
            88_000_001,
            InventoryItemRecord {
                item_id: 88_000_001,
                type_id: 16274, // Helium Isotopes
                owner_id: 90_000_001,
                location_id: ship_id,
                flag: InventoryFlag::SpecializedFuelBay,
                quantity: 10_000,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    // 1. structureCynoBeaconMgr.GetBeaconFleet: lists online beacons for character's corp
    let fleet_beacons = ctx
        .dispatch_rpc(
            &mut sess,
            "structureCynoBeaconMgr",
            None,
            "GetBeaconFleet",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetBeaconFleet");
    let PyRep::List(blist) = fleet_beacons else {
        panic!("Expected List from GetBeaconFleet");
    };
    assert!(
        blist.iter().any(|b| {
            if let PyRep::KeyVal(d) = b {
                d.get("structureID").and_then(|v| v.as_i64()) == Some(beacon_struct_id)
            } else {
                false
            }
        }),
        "GetBeaconFleet must contain the seeded Pharolux beacon"
    );

    // 2. Undock into space in origin system
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let _origin_sys = sess.solar_system_id.unwrap();

    // 3. Jump to Pharolux Cyno Beacon (feat:navigation.pharolux-beacon)
    let jump_res = ctx
        .dispatch_rpc(
            &mut sess,
            "structureCynoBeaconMgr",
            None,
            "CmdJumpToStructureBeacon",
            &[
                PyRep::Integer(dest_sys_id as i64),
                PyRep::Integer(beacon_struct_id),
            ],
            &kw,
            None,
        )
        .await
        .expect("CmdJumpToStructureBeacon");
    let PyRep::KeyVal(jkv) = jump_res else {
        panic!("Expected KeyVal from CmdJumpToStructureBeacon");
    };
    assert_eq!(
        jkv.get("solarSystemID").unwrap().as_i64().unwrap(),
        dest_sys_id as i64,
        "Target solar system must match Ohkunen"
    );
    assert!(jkv.get("jumpFatigueMinutes").unwrap().as_f64().unwrap() > 0.0);
    assert!(jkv.get("jumpCooldownMinutes").unwrap().as_f64().unwrap() > 0.0);
    assert_eq!(sess.solar_system_id, Some(dest_sys_id));

    // Verify ship spawned in Ohkunen near beacon
    let dest_handle = ctx.systems.get_or_spawn(dest_sys_id);
    let (_, dest_ball) = dest_handle
        .get_ball_snapshot(ship_id)
        .await
        .unwrap()
        .expect("Ship spawned in destination system");
    let dist_to_beacon = dest_ball.position.distance(beacon_pos);
    assert!(
        dist_to_beacon <= 10_000.0,
        "Ship must land within 10 km of beacon: got {dist_to_beacon}"
    );

    // Verify fuel was consumed from ship
    let rem_fuel = ctx.db.with_snapshot(|snap| {
        snap.items.get(&88_000_001).map(|i| i.quantity).unwrap_or(0)
    });
    assert!(rem_fuel < 10_000, "Jump must consume isotope fuel");

    // 4. Bridge to structure beacon
    let bridge_res = ctx
        .dispatch_rpc(
            &mut sess,
            "structureCynoBeaconMgr",
            None,
            "CmdBridgeToStructureBeacon",
            &[
                PyRep::Integer(dest_sys_id as i64),
                PyRep::Integer(beacon_struct_id),
            ],
            &kw,
            None,
        )
        .await
        .expect("CmdBridgeToStructureBeacon");
    let PyRep::KeyVal(b_kv) = bridge_res else {
        panic!("Expected KeyVal from CmdBridgeToStructureBeacon");
    };
    assert_eq!(b_kv.get("bridgeActive").unwrap(), &PyRep::Bool(true));

    // 5. Group jump to structure beacon
    let group_res = ctx
        .dispatch_rpc(
            &mut sess,
            "structureCynoBeaconMgr",
            None,
            "CmdGroupJumpToStructureBeacon",
            &[
                PyRep::Integer(dest_sys_id as i64),
                PyRep::Integer(beacon_struct_id),
            ],
            &kw,
            None,
        )
        .await
        .expect("CmdGroupJumpToStructureBeacon");
    let PyRep::KeyVal(g_kv) = group_res else {
        panic!("Expected KeyVal from CmdGroupJumpToStructureBeacon");
    };
    assert_eq!(g_kv.get("groupJumpActive").unwrap(), &PyRep::Bool(true));

    // 6. Error validation: High-sec beacon jump rejected
    let highsec_beacon_id = 1_040_000_000_201_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            highsec_beacon_id,
            UpwellStructureRecord {
                structure_id: highsec_beacon_id,
                type_id: 35840,
                solar_system_id: 30000142, // Jita 0.95 high-sec
                owner_corp_id: pilot_corp_id,
                name: "Jita HighSec Beacon".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 50,
                shield_hp: 50_000.0,
                armor_hp: 50_000.0,
                hull_hp: 50_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
            },
        );
    });

    let hs_err = ctx
        .dispatch_rpc(
            &mut sess,
            "structureCynoBeaconMgr",
            None,
            "CmdJumpToStructureBeacon",
            &[
                PyRep::Integer(30000142),
                PyRep::Integer(highsec_beacon_id),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        hs_err.unwrap_err().code,
        "CannotJumpToHighSecBeacon",
        "Jumping to high-sec cyno beacon must be rejected"
    );
}
