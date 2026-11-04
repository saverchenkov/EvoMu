//! Remediation integration tests for navigation features (part 2):
//! `feat:navigation.shipcaster`, `feat:navigation.starter-shipcaster`, `feat:navigation.warp-vector`.

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use eve_server::router::ServerContext;
use eve_server::services::cluster_macho_misc_services::{with_misc_state, LandingPadRecord};
use glam::DVec3;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

fn place_pilot_in_space(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
) {
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(sys_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.location_id = sys_id as i64;
        }
    });
}

#[tokio::test]
async fn test_navigation_shipcaster_and_starter() {
    // parity: feat:navigation.shipcaster, feat:navigation.starter-shipcaster
    eve_testkit::attest(&[
        "feat:navigation.shipcaster",
        "feat:navigation.starter-shipcaster",
    ]);

    let sde = match SdeRegistry::open(&SdeRegistry::default_db_path()) {
        Ok(reg) => Arc::new(reg),
        Err(_) => Arc::new(SdeRegistry::with_canonical_seed()),
    };
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = Arc::new(ServerContext::with_sde_and_repository(sde, db));
    common::pin_clock(&ctx);

    let char_id = 90_000_001_i64;
    let ship_id = 850_001_i64;
    let starter_sys = 30_030_141_u32;
    let hq_sys = 30_045_324_u32; // Onnamon War HQ

    let mut sess = SessionState::new(20_201);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");

    let now_unix = ctx.now_unix();
    let (career_launcher_id, career_launcher_pos) = with_misc_state(&ctx, |st| {
        let l = st
            .starter_launchers
            .iter()
            .find(|l| l.solar_system_id == starter_sys)
            .expect("Starter launcher in system");
        (l.item_id, l.position)
    });

    // Setup ship in space near career launcher
    sess.solar_system_id = Some(starter_sys);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = starter_sys;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
            ch.create_date_unix = now_unix - 40 * 86_400; // 40 days old (> 30 days)
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 601, // Ibis (Rookie frigate)
                owner_id: char_id,
                location_id: starter_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    let starter_handle = ctx.systems.get_or_spawn(starter_sys);
    let _ = starter_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 601,
            position: career_launcher_pos + DVec3::new(1_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 400.0,
            inertia: 1.0,
            mass: 100_000.0,
            radius: 30.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    // --- Part A: Starter Shipcaster ---
    // 1. Veteran rejection (age > 30 days)
    let vet_err = ctx
        .dispatch_rpc(
            &mut sess,
            "starterShipcasterTravelMgr",
            None,
            "CmdJumpThroughStarterShipcaster",
            &[PyRep::Integer(career_launcher_id)],
            &HashMap::new(),
            None,
        )
        .await
        .expect_err("Veteran rejected");
    assert_eq!(vet_err.code, "CharacterTooOldForStarterShipcaster");

    // 2. Set age <= 30 days (5 days old)
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().create_date_unix = now_unix - 5 * 86_400;
    });
    let can_jump = ctx
        .dispatch_rpc(
            &mut sess,
            "starterShipcasterTravelMgr",
            None,
            "CanCharacterJump",
            &[],
            &HashMap::new(),
            None,
        )
        .await
        .expect("CanCharacterJump");
    assert_eq!(can_jump, PyRep::Bool(true));

    // 3. Weapons timer denial
    ctx.crimewatch
        .write()
        .unwrap()
        .get_or_create(char_id)
        .weapon_timer_until_unix = now_unix + 60;
    assert_eq!(
        ctx.dispatch_rpc(
            &mut sess,
            "starterShipcasterTravelMgr",
            None,
            "CanCharacterJump",
            &[],
            &HashMap::new(),
            None
        )
        .await
        .unwrap(),
        PyRep::Bool(false)
    );
    ctx.crimewatch
        .write()
        .unwrap()
        .get_or_create(char_id)
        .weapon_timer_until_unix = 0;

    // 4. Capital ship denial
    ctx.db
        .with_mut_snapshot(|s| s.items.get_mut(&ship_id).unwrap().type_id = 19720); // Revelation (Dreadnought)
    let cap_err = ctx
        .dispatch_rpc(
            &mut sess,
            "starterShipcasterTravelMgr",
            None,
            "CmdJumpThroughStarterShipcaster",
            &[PyRep::Integer(career_launcher_id)],
            &HashMap::new(),
            None,
        )
        .await
        .expect_err("Capital ship denied");
    assert_eq!(cap_err.code, "CapitalShipCannotJumpShipcaster");
    ctx.db
        .with_mut_snapshot(|s| s.items.get_mut(&ship_id).unwrap().type_id = 601);

    // 5. Successful rookie launch
    ctx.advance_secs(15);
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, char_id);
    let jump_res = ctx
        .dispatch_rpc(
            &mut sess,
            "starterShipcasterTravelMgr",
            None,
            "CmdJumpThroughStarterShipcaster",
            &[PyRep::Integer(career_launcher_id)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("Starter jump succeeds");
    assert_eq!(jump_res, PyRep::None);
    assert_eq!(sess.solar_system_id, Some(hq_sys));

    // 6. Arrived ball has launch trajectory velocity
    let hq_handle = ctx.systems.get_or_spawn(hq_sys);
    let (_, arrived_ball) = hq_handle
        .get_ball_snapshot(ship_id)
        .await
        .unwrap()
        .expect("Ball in HQ");
    assert!(
        arrived_ball.velocity.length() > 50.0,
        "Ball should have launch velocity"
    );

    // 7. Starter shipcaster one-time-use per character
    place_pilot_in_space(&ctx, &mut sess, char_id, ship_id, starter_sys);
    let _ = starter_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 601,
            position: career_launcher_pos + DVec3::new(1_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 400.0,
            inertia: 1.0,
            mass: 100_000.0,
            radius: 30.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, char_id);

    let reuse_err = ctx
        .dispatch_rpc(
            &mut sess,
            "starterShipcasterTravelMgr",
            None,
            "CmdJumpThroughStarterShipcaster",
            &[PyRep::Integer(career_launcher_id)],
            &HashMap::new(),
            None,
        )
        .await
        .expect_err("Second jump denied");
    assert_eq!(reuse_err.code, "StarterShipcasterAlreadyUsed");

    // --- Part B: Interstellar Shipcaster ---
    // Place pilot at Onnamon FW Launcher
    let (launcher_id, launcher_pos) = with_misc_state(&ctx, |st| {
        let l = st
            .launchers
            .iter()
            .find(|l| l.faction_id == 500_001)
            .unwrap();
        (l.item_id, l.position)
    });
    let pad_sys = 30_002_811_u32; // Onatoh
    let pad_id = 104_000_001_i64;
    with_misc_state(&ctx, |st| {
        st.landing_pads.push(LandingPadRecord {
            item_id: pad_id,
            type_id: 75929,
            built_type_id: 75929,
            solar_system_id: pad_sys,
            faction_id: 500_001,
            is_built: true,
            link_timestamp: now_unix as i64,
            is_disrupted: false,
            position: DVec3::new(100_000.0, 0.0, 0.0),
            required_materials: BTreeMap::new(),
            contributed_materials: BTreeMap::new(),
        });
        if let Some(l) = st.launchers.iter_mut().find(|l| l.item_id == launcher_id) {
            if !l.linked_pad_ids.contains(&pad_id) {
                l.linked_pad_ids.push(pad_id);
            }
        }
    });

    place_pilot_in_space(&ctx, &mut sess, char_id, ship_id, hq_sys);
    let _ = hq_handle.remove_ball(ship_id).await;
    let _ = hq_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 601,
            position: launcher_pos + DVec3::new(1_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 400.0,
            inertia: 1.0,
            mass: 100_000.0,
            radius: 30.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, char_id);

    // 1. Hostile militia faction rejected
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&char_id).unwrap().war_faction_id = Some(500_002)
    });
    let hostile_err = ctx
        .dispatch_rpc(
            &mut sess,
            "shipcasterTravelMgr",
            None,
            "CmdJumpThroughShipcaster",
            &[
                PyRep::Integer(launcher_id),
                PyRep::Integer(pad_sys as i64),
                PyRep::Integer(pad_id),
            ],
            &HashMap::new(),
            None,
        )
        .await
        .expect_err("Hostile militia denied");
    assert_eq!(hostile_err.code, "FactionShipcasterAccessDenied");

    // 2. Capital ship rejected
    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&char_id).unwrap();
        ch.war_faction_id = Some(500_001);
        s.items.get_mut(&ship_id).unwrap().type_id = 19720; // Capital
    });
    let cap_fw_err = ctx
        .dispatch_rpc(
            &mut sess,
            "shipcasterTravelMgr",
            None,
            "CmdJumpThroughShipcaster",
            &[
                PyRep::Integer(launcher_id),
                PyRep::Integer(pad_sys as i64),
                PyRep::Integer(pad_id),
            ],
            &HashMap::new(),
            None,
        )
        .await
        .expect_err("Capital ship denied on FW shipcaster");
    assert_eq!(cap_fw_err.code, "CapitalShipCannotJumpShipcaster");
    ctx.db
        .with_mut_snapshot(|s| s.items.get_mut(&ship_id).unwrap().type_id = 601);

    // 3. Successful FW shipcaster jump to linked landing pad
    let fw_jump = ctx
        .dispatch_rpc(
            &mut sess,
            "shipcasterTravelMgr",
            None,
            "CmdJumpThroughShipcaster",
            &[
                PyRep::Integer(launcher_id),
                PyRep::Integer(pad_sys as i64),
                PyRep::Integer(pad_id),
            ],
            &HashMap::new(),
            None,
        )
        .await
        .expect("FW shipcaster jump succeeds");
    assert_eq!(fw_jump, PyRep::None);
    assert_eq!(sess.solar_system_id, Some(pad_sys));

    let pad_handle = ctx.systems.get_or_spawn(pad_sys);
    let (_, pad_ball) = pad_handle
        .get_ball_snapshot(ship_id)
        .await
        .unwrap()
        .expect("Ball at pad");
    assert!(
        pad_ball.velocity.length() > 50.0,
        "Ball should have launch velocity upon arrival"
    );
}

#[tokio::test]
async fn test_navigation_warp_vector() {
    // parity: feat:navigation.warp-vector
    eve_testkit::attest(&["feat:navigation.warp-vector"]);

    let sde = match SdeRegistry::open(&SdeRegistry::default_db_path()) {
        Ok(reg) => Arc::new(reg),
        Err(_) => Arc::new(SdeRegistry::with_canonical_seed()),
    };
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = Arc::new(ServerContext::with_sde_and_repository(sde, db));
    common::pin_clock(&ctx);

    let char_id = 90_000_001_i64;
    let ship_id = 860_001_i64;
    let sys_id = 30_000_142_u32; // Jita

    let mut sess = SessionState::new(20_301);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");

    let star_pos = ctx
        .sde
        .celestials_in_system(sys_id)
        .into_iter()
        .find(|c| c.group_id == 6)
        .map(|c| c.position)
        .unwrap_or(DVec3::ZERO);

    // 1. Seed Warp Vector Data item (type 77825, group 4599) in ship cargo
    let vec_item_id = 870_001_i64;
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    sess.station_id = None;
    sess.structure_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587, // Rifter (Frigate)
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            vec_item_id,
            InventoryItemRecord {
                item_id: vec_item_id,
                type_id: 77_825, // Warp Vector Data (group 4599)
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 2,
                singleton: false,
                ..Default::default()
            },
        );
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let near_star = star_pos + DVec3::new(500_000_000.0, 250_000_000.0, -100_000_000.0);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: near_star,
            velocity: DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    // 2. Activate Warp Vector (`warpVectorMgr.UseVector`)
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "warpVectorMgr",
            None,
            "UseVector",
            &[PyRep::Integer(vec_item_id)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("UseVector succeeds");

    assert_eq!(res.dict_get("consumed"), Some(&PyRep::Bool(true)));
    assert_eq!(res.dict_get("remainingQuantity"), Some(&PyRep::Integer(1)));

    // 3. Verify returned entrySpeed = 0.75 * max_velocity
    let entry_speed = res
        .dict_get("entrySpeed")
        .and_then(|v| v.as_f64())
        .expect("entrySpeed");
    assert!((entry_speed - 350.0 * 0.75).abs() < 1e-4);

    // 4. Verify returned alignmentVector is a valid normalized direction vector
    let align_vec = res.dict_get("alignmentVector").expect("alignmentVector");
    let PyRep::Tuple(vec_comps) = align_vec else {
        panic!("Expected Tuple")
    };
    let vx = vec_comps[0].as_f64().unwrap();
    let vy = vec_comps[1].as_f64().unwrap();
    let vz = vec_comps[2].as_f64().unwrap();
    let v_len = (vx * vx + vy * vy + vz * vz).sqrt();
    assert!(
        (v_len - 1.0).abs() < 1e-4,
        "Alignment vector must be normalized"
    );

    // 5. Verify distance >= 150 km
    let dist = res
        .dict_get("distanceMeters")
        .and_then(|v| v.as_f64())
        .expect("distanceMeters");
    assert!(dist >= 150_000.0, "Warp distance must be >= 150 km");

    // 6. Verify ship ball entered MovementMode::Warp
    let (_, ball) = sys_handle
        .get_ball_snapshot(ship_id)
        .await
        .unwrap()
        .unwrap();
    let MovementMode::Warp {
        destination,
        warp_speed_au_s,
        ..
    } = ball.mode
    else {
        panic!("Ship must be in Warp movement mode");
    };
    assert!(destination.distance(near_star) >= 150_000.0);
    assert!(warp_speed_au_s > 0.0);

    // 7. Verify 300s cooldown prevents immediate second activation
    let cd_err = ctx
        .dispatch_rpc(
            &mut sess,
            "warpVectorMgr",
            None,
            "UseVector",
            &[PyRep::Integer(vec_item_id)],
            &HashMap::new(),
            None,
        )
        .await
        .expect_err("Cooldown should prevent second activation");
    assert_eq!(cd_err.code, "WarpVectorCooldown");

    // 8. Verify star proximity check (if ship > 10 AU from star, rejected with WarpVectorTooFarFromStar)
    ctx.advance_secs(305); // clear cooldown
    let far_from_star = star_pos + DVec3::new(2.0e12, 0.0, 0.0); // > 13 AU
    let _ = sys_handle
        .set_movement_mode(ship_id, MovementMode::Stop)
        .await;
    let _ = sys_handle
        .set_ball_position(ship_id, far_from_star)
        .await;

    let far_err = ctx
        .dispatch_rpc(
            &mut sess,
            "warpVectorMgr",
            None,
            "UseVector",
            &[PyRep::Integer(vec_item_id)],
            &HashMap::new(),
            None,
        )
        .await
        .expect_err("Far from star should fail");
    assert_eq!(far_err.code, "WarpVectorTooFarFromStar");
}
