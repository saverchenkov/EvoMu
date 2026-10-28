//! Space dynamic bubble transitions (`SP-3`), collision physics & asteroid belt respawn (`SP-4`),
//! stargate jumping & gate cloak (`SP-5`), and mining/harvesting cycles & ledger (`MN-01`..`MN-03`) integration tests.

#![forbid(unsafe_code)]

use eve_db::types::InventoryFlag;
use eve_destiny::{
    Ball, DamageVector, DestinyEvent, MovementMode,
    BUBBLE_TRANSITION_THRESHOLD_METERS,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::system_actor::AsteroidState;
use glam::DVec3;
use std::collections::HashMap;

#[tokio::test]
async fn test_sp3_spatial_grid_bubbles_dynamic_transitions_and_warp_curves() {
    // parity: plan:SP-3, feat:navigation.bubbles-grid, feat:navigation.warp-speed-formula, destiny:AddBalls2, destiny:RemoveBalls
    eve_testkit::attest(&["plan:SP-3", "feat:navigation.bubbles-grid", "feat:navigation.warp-speed-formula", "destiny:AddBalls2", "destiny:RemoveBalls"]);
    let ctx = ServerContext::new();
    let jita = ctx.systems.get_or_spawn(30_000_142);

    let ship_1_id = 100_600_001;
    let ship_2_id = 100_600_002;

    // Spawn Ship 1 at (0, 0, 0) and Ship 2 at (0, 0, 500) -> both belong to Grid 1
    jita.spawn_ball(Ball {
        id: ship_1_id,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 365.0,
        inertia: 3.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    })
    .await
    .unwrap();

    jita.spawn_ball(Ball {
        id: ship_2_id,
        type_id: 587,
        position: DVec3::new(0.0, 0.0, 500.0),
        velocity: DVec3::ZERO,
        max_velocity: 365.0,
        inertia: 3.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    })
    .await
    .unwrap();

    let b1_grid = jita.get_bubble_id_for_ball(ship_1_id).await.unwrap().unwrap();
    let b2_grid = jita.get_bubble_id_for_ball(ship_2_id).await.unwrap().unwrap();
    assert_eq!(b1_grid, b2_grid, "Ships within 500m must share the same grid bubble");

    // Dynamic balls in same bubble can see each other
    let visible_to_2 = jita.list_balls_in_bubble(ship_2_id).await.unwrap();
    assert!(visible_to_2.iter().any(|b| b.id == ship_1_id));

    let _ = jita.drain_observer_updates(ship_2_id).await;

    // Move Ship 1 across the 10,000 km threshold (> 10,000,000 m) to (12,000 km, 0, 0)
    let dest_x = BUBBLE_TRANSITION_THRESHOLD_METERS + 2_000_000.0;
    let warp_mode = MovementMode::new_warp(
        DVec3::ZERO,
        DVec3::new(dest_x, 0.0, 0.0),
        3.0,
    );
    jita.set_movement_mode(ship_1_id, warp_mode).await.unwrap();

    // Step physics until warp completes and bubble migration triggers
    let mut saw_remove = false;
    for _ in 0..60 {
        let tb = jita.step_manual(1.0).await.unwrap();
        if let Some(evs) = tb.observer_events.get(&ship_2_id) {
            if evs.iter().any(|e| match e {
                DestinyEvent::RemoveBalls { ball_ids } => ball_ids.contains(&ship_1_id),
                DestinyEvent::RemoveBall { ball_id } => *ball_id == ship_1_id,
                _ => false,
            }) {
                saw_remove = true;
            }
        }
        let (_, snap) = jita.get_ball_snapshot(ship_1_id).await.unwrap().unwrap();
        if matches!(snap.mode, MovementMode::Stop) {
            break;
        }
    }

    let b1_new_grid = jita.get_bubble_id_for_ball(ship_1_id).await.unwrap().unwrap();
    assert_ne!(
        b1_new_grid, b2_grid,
        "Ship 1 after crossing 10,000 km threshold must migrate to an adjacent grid bubble"
    );

    // Inter-grid visibility filtering: dynamic ball in Grid 2 CANNOT see dynamic ball in Grid 1
    let dynamic_in_grid_2 = jita.list_dynamic_balls_in_bubble(ship_2_id).await.unwrap();
    assert!(
        !dynamic_in_grid_2.iter().any(|b| b.id == ship_1_id),
        "Dynamic ship in adjacent grid bubble must be invisible across grid boundaries"
    );

    // Static celestials (stars, stations, gates) remain visible in list_balls_in_bubble
    let all_visible_to_2 = jita.list_balls_in_bubble(ship_2_id).await.unwrap();
    assert!(
        all_visible_to_2.iter().any(|b| b.id == 40_009_076),
        "Static sun celestial must be visible to all observers across grids"
    );

    // Observer in Grid 2 received RemoveBalls for Ship 1 upon migration
    assert!(
        saw_remove,
        "Observer in Grid 2 must receive RemoveBalls when Ship 1 migrates across grid bubble threshold"
    );

    // Clean up
    let _ = jita.remove_ball(ship_1_id).await;
    let _ = jita.remove_ball(ship_2_id).await;
}

#[tokio::test]
async fn test_sp4_asteroid_belt_entities_collision_bounce_and_respawn_cycle() {
    // parity: feat:mining.depletion, feat:mining.belt-respawn
    eve_testkit::attest(&["feat:mining.depletion", "feat:mining.belt-respawn"]);
    let ctx = ServerContext::new();
    let perim = ctx.systems.get_or_spawn(30_000_144);

    // 1. Populate asteroid belt entities in Perimeter (has 3 SDE asteroid belts)
    let belt_asteroids = perim.populate_asteroid_belt_entities().await.unwrap();
    assert!(
        !belt_asteroids.is_empty(),
        "Populating asteroid belts must create harvestable asteroids"
    );
    let sample_asteroid = &belt_asteroids[0];
    let sample_id = sample_asteroid.asteroid_id;
    assert_eq!(sample_asteroid.type_id, 1230); // Veldspar
    assert_eq!(sample_asteroid.initial_units, 20_000);
    assert_eq!(sample_asteroid.remaining_units, 20_000);

    // Verify asteroid ball exists in live Ballpark
    let (_, ast_ball) = perim.get_ball_snapshot(sample_id).await.unwrap().unwrap();
    assert_eq!(ast_ball.id, sample_id);

    // 2. Physical elastic collision bounce test:
    // Spawn ship 1 near sample asteroid positioned with overlap (radii 40m + 60m = 100m, distance 70m -> 30m overlap)
    let ship_col_id = 100_700_001;
    let ship_pos = ast_ball.position - DVec3::new(70.0, 0.0, 0.0);
    perim.spawn_ball(Ball {
        id: ship_col_id,
        type_id: 587,
        position: ship_pos,
        velocity: DVec3::new(20.0, 0.0, 0.0), // heading directly toward asteroid center
        max_velocity: 365.0,
        inertia: 3.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.1,
        mode: MovementMode::GotoDirection { direction: DVec3::X },
    })
    .await
    .unwrap();

    let tick_broadcast = perim.step_manual(1.0).await.unwrap();
    let had_collision = tick_broadcast.events.iter().any(|e| {
        matches!(
            e,
            DestinyEvent::Collision { ball_a, ball_b }
                if (*ball_a == ship_col_id && *ball_b == sample_id)
                    || (*ball_a == sample_id && *ball_b == ship_col_id)
        )
    });
    assert!(
        had_collision,
        "Stepping overlapping ship and asteroid must trigger DestinyEvent::Collision"
    );

    // Verify ship velocity rebounded backwards
    let (_, ship_after_bounce) = perim.get_ball_snapshot(ship_col_id).await.unwrap().unwrap();
    assert!(
        ship_after_bounce.velocity.x < 0.0,
        "Ship velocity must bounce backwards after colliding with asteroid"
    );

    // 3. Asteroid depletion and downtime respawn cycle:
    // Extract remaining units to deplete asteroid
    let ext1 = perim.mine_asteroid(sample_id, 20_000, 0.0).await.unwrap();
    assert_eq!(ext1.mined_units, 20_000);
    assert_eq!(ext1.remaining_units, 0);
    assert!(ext1.depleted, "Asteroid must be marked depleted when units reach 0");

    // Asteroid ball removed from ballpark on depletion
    let snap_after_depletion = perim.get_ball_snapshot(sample_id).await.unwrap();
    assert!(
        snap_after_depletion.is_none(),
        "Depleted asteroid ball must be removed from the Ballpark"
    );

    // Deplete remaining asteroids in the belt and advance virtual clock across downtime
    for ast in belt_asteroids
        .iter()
        .filter(|a| a.belt_id == sample_asteroid.belt_id && a.asteroid_id != sample_id)
    {
        let _ = perim
            .mine_asteroid(ast.asteroid_id, ast.initial_units, 0.0)
            .await
            .unwrap();
    }
    ctx.set_clock(std::sync::Arc::new(eve_db::VirtualServerClock::new()));
    ctx.advance_secs(eve_server::system_actor::DEFAULT_BELT_RESPAWN_INTERVAL_SECS * 2);
    let _ = perim.step_manual(1.0).await.unwrap();

    // Verify asteroid ball restored in Ballpark with initial units
    let restored_state = perim.get_asteroid_state(sample_id).await.unwrap().unwrap();
    assert_eq!(restored_state.remaining_units, 20_000);
    assert!(!restored_state.depleted);
    let restored_snap = perim.get_ball_snapshot(sample_id).await.unwrap();
    assert!(
        restored_snap.is_some(),
        "Respawned asteroid ball must reappear in Ballpark"
    );

    // Clean up
    let _ = perim.remove_ball(ship_col_id).await;
}

#[tokio::test]
async fn test_sp5_stargate_jumping_session_handoff_and_gate_cloak_invulnerability() {
    // parity: plan:SP-5, feat:navigation.stargate-jump, feat:navigation.gate-cloak, rpc:authentication.Login, rpc:charUnboundMgr.SelectCharacterID
    eve_testkit::attest(&["plan:SP-5", "feat:navigation.stargate-jump", "feat:navigation.gate-cloak"]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(955);

    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[
            PyRep::String("demo".into()),
            PyRep::String("demo-secret".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Login");
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

    // Undock in Jita (30000142)
    let undock = ctx
        .dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let PyRep::KeyVal(undock_kv) = &undock else { panic!("Expected KeyVal from Undock") };
    let beyonce_id = undock_kv["beyonceBoundID"].as_i64().unwrap() as u64;
    let ship_id = undock_kv["shipID"].as_i64().unwrap();

    let jita_handle = ctx.systems.get_or_spawn(30_000_142);
    let perim_gate_id = 50_001_249i64; // Perimeter Stargate in Jita
    let (_, gate_ball) = jita_handle
        .get_ball_snapshot(perim_gate_id)
        .await
        .unwrap()
        .unwrap();

    // Position ship within 2,000m of stargate to jump
    let _ = jita_handle.set_movement_mode(ship_id, MovementMode::Stop).await;
    // Spawn ship directly within jump range of the stargate
    jita_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: gate_ball.position + DVec3::new(1_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 365.0,
            inertia: 3.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await
        .unwrap();

    // Jump from Jita to Perimeter via CmdStargateJump
    ctx.advance_secs(12);
    let jump_res = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(beyonce_id),
            "CmdStargateJump",
            &[PyRep::Integer(perim_gate_id)],
            &kw,
            None,
        )
        .await
        .expect("CmdStargateJump");
    let PyRep::KeyVal(jump_kv) = &jump_res else { panic!("Expected KeyVal") };
    assert_eq!(jump_kv["solarSystemID"].as_i64(), Some(30_000_144));
    assert_eq!(sess.solar_system_id, Some(30_000_144));

    let perim_handle = ctx.systems.get_or_spawn(30_000_144);
    let perim_snap = perim_handle.get_ball_snapshot(ship_id).await.unwrap();
    assert!(perim_snap.is_some(), "Ship must be spawned in target system Perimeter");

    // Verify 60s gate cloak invulnerability upon arrival:
    // Damage applied while gate cloak is active deals 0.0 damage!
    let dmg_res = perim_handle
        .apply_damage(
            ship_id,
            DamageVector {
                em: 500.0,
                thermal: 500.0,
                kinetic: 500.0,
                explosive: 500.0,
            },
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(dmg_res.0, 0.0, "Ship with gate cloak must be invulnerable (0.0 damage)");
    assert!(!dmg_res.1, "Ship must not be destroyed while under gate cloak");

    // Stepping physics advances tick and gate cloak decays
    let _ = perim_handle.step_manual(1.0).await.unwrap();

    // Active movement command (CmdGotoDirection) breaks the gate cloak
    let new_beyonce_id = jump_kv["beyonceBoundID"].as_i64().unwrap() as u64;
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(new_beyonce_id),
        "CmdGotoDirection",
        &[PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)],
        &kw,
        None,
    )
    .await
    .expect("CmdGotoDirection");

    // Now gate cloak is broken, damage can be applied normally
    let dmg_post_cloak = perim_handle
        .apply_damage(
            ship_id,
            DamageVector {
                em: 10.0,
                thermal: 10.0,
                kinetic: 10.0,
                explosive: 10.0,
            },
        )
        .await
        .unwrap()
        .unwrap();
    assert!(
        dmg_post_cloak.0 > 0.0,
        "Damage must be applied after gate cloak breaks on active movement"
    );
}

#[tokio::test]
async fn test_mn01_mining_laser_cycle_yield_residue_and_ore_hold_deposit() {
    // parity: rpc:authentication.Login, rpc:charUnboundMgr.SelectCharacterID
    eve_testkit::attest(&[]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(961);

    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[
            PyRep::String("demo".into()),
            PyRep::String("demo-secret".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Login");
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

    let undock = ctx
        .dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let PyRep::KeyVal(undock_kv) = &undock else { panic!("Expected KeyVal") };
    let beyonce_id = undock_kv["beyonceBoundID"].as_i64().unwrap() as u64;
    let ship_id = undock_kv["shipID"].as_i64().unwrap();

    let jita = ctx.systems.get_or_spawn(30_000_142);
    let (_, ship_ball) = jita.get_ball_snapshot(ship_id).await.unwrap().unwrap();

    // Spawn a Veldspar asteroid (type 1230, unit_volume 0.1 m3) 1,500m from ship
    let test_asteroid_id = 750_000_123i64;
    let ast_pos = ship_ball.position + DVec3::new(1_500.0, 0.0, 0.0);
    jita.spawn_asteroid(
        AsteroidState {
            asteroid_id: test_asteroid_id,
            belt_id: 40_000_001,
            type_id: 1230,
            initial_units: 5_000,
            remaining_units: 5_000,
            unit_volume: 0.1,
            depleted: false,
            respawn_at_tick: None,
        },
        Ball {
            id: test_asteroid_id,
            type_id: 1230,
            position: ast_pos,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 0.0,
            mass: 1.0e10,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
    )
    .await
    .unwrap();

    // Execute mining cycle via beyonce.CmdMine
    let mine_res = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(beyonce_id),
            "CmdMineRepeatedly",
            &[PyRep::Integer(test_asteroid_id)],
            &kw,
            None,
        )
        .await
        .expect("CmdMine");
    let PyRep::KeyVal(mine_kv) = &mine_res else { panic!("Expected KeyVal from CmdMine") };

    let mined = mine_kv["minedUnits"].as_i64().unwrap();
    let remaining = mine_kv["remainingUnits"].as_i64().unwrap();
    let flag_id = mine_kv["flagID"].as_i64().unwrap();
    assert!(flag_id == 5 || flag_id == 133, "Ore must be in Cargo or Ore Hold");
    assert!(mined > 0, "Mining cycle must yield positive ore units");
    assert_eq!(5_000 - mined, remaining);

    // Verify ore was deposited into ship's inventory (flag 5 Cargo or 133 SpecializedOreHold)
    ctx.db.with_snapshot(|snap| {
        let has_ore = snap.items.values().any(|item| {
            item.location_id == ship_id
                && item.type_id == 1230
                && i64::from(item.quantity) >= mined
                && (item.flag == InventoryFlag::Cargo || item.flag == InventoryFlag::SpecializedOreHold)
        });
        assert!(has_ore, "Mined Veldspar must be present in ship inventory");
    });
}

#[tokio::test]
async fn test_mn02_ice_harvesting_and_gas_harvesting_cycles() {
    // parity: rpc:authentication.Login, rpc:charUnboundMgr.SelectCharacterID
    eve_testkit::attest(&[]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(962);

    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[
            PyRep::String("demo".into()),
            PyRep::String("demo-secret".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Login");
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

    let undock = ctx
        .dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let PyRep::KeyVal(undock_kv) = &undock else { panic!("Expected KeyVal") };
    let beyonce_id = undock_kv["beyonceBoundID"].as_i64().unwrap() as u64;
    let ship_id = undock_kv["shipID"].as_i64().unwrap();

    let jita = ctx.systems.get_or_spawn(30_000_142);
    let (_, ship_ball) = jita.get_ball_snapshot(ship_id).await.unwrap().unwrap();

    // 1. Ice harvesting cycle: Clear Icicle (type 16262, group 465, unit volume 1000.0 m3)
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(90_000_001).or_default();
        for sid in [3386u32, 16281, 25544] {
            sk.insert(sid, eve_db::TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 });
        }
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.type_id = 22548; // Mackinaw (Exhumer group 543, 31,500 m3 Mining Hold)
            sh.dynamic_attributes.extend([(14, 2.0), (102, 2.0), (11, 500.0), (48, 500.0)]);
        }
        snap.items.retain(|_, it| {
            !(it.location_id == ship_id
                && matches!(it.flag, InventoryFlag::HiSlot0 | InventoryFlag::HiSlot1))
        });
        for (iid, tid, flag) in [
            (750_000_901i64, 16278u32, InventoryFlag::HiSlot0),
            (750_000_902i64, 25266u32, InventoryFlag::HiSlot1),
        ] {
            snap.items.insert(
                iid,
                eve_db::InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: 90_000_001,
                    location_id: ship_id,
                    flag,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
    let ice_ast_id = 750_000_465i64;
    jita.spawn_asteroid(
        AsteroidState {
            asteroid_id: ice_ast_id,
            belt_id: 40_000_001,
            type_id: 16262,
            initial_units: 50,
            remaining_units: 50,
            unit_volume: 1000.0,
            depleted: false,
            respawn_at_tick: None,
        },
        Ball {
            id: ice_ast_id,
            type_id: 16262,
            position: ship_ball.position + DVec3::new(1_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 0.0,
            mass: 1.0e10,
            radius: 80.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
    )
    .await
    .unwrap();

    let ice_res = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(beyonce_id),
            "CmdMineRepeatedly",
            &[PyRep::Integer(ice_ast_id)],
            &kw,
            None,
        )
        .await
        .expect("CmdMine on Ice");
    let PyRep::KeyVal(ice_kv) = &ice_res else { panic!("Expected KeyVal") };
    assert_eq!(ice_kv["typeID"].as_i64(), Some(16262));
    assert!(ice_kv["minedUnits"].as_i64().unwrap() >= 1);

    // 2. Gas harvesting cycle: Fullerite-C50 (type 30370, group 711, unit volume 1.0 m3)
    let gas_ast_id = 750_000_711i64;
    jita.spawn_asteroid(
        AsteroidState {
            asteroid_id: gas_ast_id,
            belt_id: 40_000_001,
            type_id: 30370,
            initial_units: 200,
            remaining_units: 200,
            unit_volume: 1.0,
            depleted: false,
            respawn_at_tick: None,
        },
        Ball {
            id: gas_ast_id,
            type_id: 30370,
            position: ship_ball.position + DVec3::new(2_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 0.0,
            mass: 1.0e9,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
    )
    .await
    .unwrap();

    let gas_res = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(beyonce_id),
            "CmdMineRepeatedly",
            &[PyRep::Integer(gas_ast_id)],
            &kw,
            None,
        )
        .await
        .expect("CmdMine on Gas");
    let PyRep::KeyVal(gas_kv) = &gas_res else { panic!("Expected KeyVal") };
    assert_eq!(gas_kv["typeID"].as_i64(), Some(30370));
    assert!(gas_kv["minedUnits"].as_i64().unwrap() >= 1);
}
