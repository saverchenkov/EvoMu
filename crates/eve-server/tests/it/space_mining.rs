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
    let ship_id = ctx.db.with_snapshot(|snap| snap.characters[&90_000_001].active_ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, i| !(i.location_id == ship_id && i.flag == InventoryFlag::HiSlot0));
        snap.items.insert(
            750_000_901,
            eve_db::InventoryItemRecord {
                item_id: 750_000_901,
                type_id: 483, // Miner I
                owner_id: 90_000_001,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(750_000_901, true);
        snap.skills.entry(90_000_001).or_default().insert(
            3386,
            eve_db::TrainedSkillRecord {
                type_id: 3386,
                level: 5,
                skill_points: 256_000.0,
            },
        );
    });
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
    assert_eq!(ship_id, undock_kv["shipID"].as_i64().unwrap());

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
    let ship_id = ctx.db.with_snapshot(|snap| snap.characters[&90_000_001].active_ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, i| !(i.location_id == ship_id && i.flag == InventoryFlag::HiSlot0));
        snap.items.insert(
            750_000_901,
            eve_db::InventoryItemRecord {
                item_id: 750_000_901,
                type_id: 483, // Miner I
                owner_id: 90_000_001,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(750_000_901, true);
        snap.skills.entry(90_000_001).or_default().insert(
            3386,
            eve_db::TrainedSkillRecord {
                type_id: 3386,
                level: 5,
                skill_points: 256_000.0,
            },
        );
    });
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
    assert_eq!(ship_id, undock_kv["shipID"].as_i64().unwrap());

    let jita = ctx.systems.get_or_spawn(30_000_142);
    let (_, ship_ball) = jita.get_ball_snapshot(ship_id).await.unwrap().unwrap();

    // 1. Ice harvesting cycle: Clear Icicle (type 16262, group 465, unit volume 1000.0 m3)
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(90_000_001).or_default();
        for sid in [3386u32, 3410, 17940, 16281, 25544] {
            sk.insert(sid, eve_db::TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 });
        }
        if let Some(sh) = snap.items.get_mut(&ship_id) { sh.type_id = 17478; }
        snap.items.retain(|_, item| item.location_id != ship_id);
        snap.items.insert(750_000_460, eve_db::InventoryItemRecord {
            item_id: 750_000_460, type_id: 16278, owner_id: 90_000_001, location_id: ship_id,
            flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, ..Default::default()
        });
        snap.module_online_states.insert(750_000_460, true);
        snap.items.insert(750_000_462, eve_db::InventoryItemRecord {
            item_id: 750_000_462, type_id: 1230, owner_id: 90_000_001, location_id: ship_id,
            flag: InventoryFlag::SpecializedOreHold, quantity: 270_000, singleton: false, ..Default::default()
        });
    });
    ctx.invalidate_ship_fitting(ship_id);
    let ice_ast_id = 750_000_465i64;
    jita.spawn_asteroid(
        AsteroidState {
            asteroid_id: ice_ast_id, belt_id: 40_000_001, type_id: 16262,
            initial_units: 50, remaining_units: 50, unit_volume: 1000.0, depleted: false, respawn_at_tick: None,
        },
        Ball {
            id: ice_ast_id, type_id: 16262, position: ship_ball.position + DVec3::new(1_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO, max_velocity: 0.0, inertia: 0.0, mass: 1.0e10, radius: 80.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        },
    ).await.unwrap();

    // R-NAV-P1-01 negative test: mining into an insufficient hold returns CargoFull
    let full_err = ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_id), "CmdMineRepeatedly", &[PyRep::Integer(ice_ast_id)], &kw, None).await.expect_err("Mining into full hold must return CargoFull");
    assert_eq!(full_err.code, "CargoFull");
    ctx.db.with_mut_snapshot(|snap| { snap.items.remove(&750_000_462); });

    let ice_res = ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_id), "CmdMineRepeatedly", &[PyRep::Integer(ice_ast_id)], &kw, None).await.expect("CmdMine on Ice");
    let PyRep::KeyVal(ice_kv) = &ice_res else { panic!("Expected KeyVal") };
    assert_eq!(ice_kv["typeID"].as_i64(), Some(16262));
    assert!(ice_kv["minedUnits"].as_i64().unwrap() >= 1);

    // 2. Gas harvesting cycle: Fullerite-C50 (type 30370, group 711, unit volume 1.0 m3)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sh) = snap.items.get_mut(&ship_id) { sh.type_id = 32880; }
        if let Some(m) = snap.items.get_mut(&750_000_460) { m.type_id = 25542; }
    });
    ctx.invalidate_ship_fitting(ship_id);
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
            position: ship_ball.position + DVec3::new(1_000.0, 0.0, 0.0),
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

#[allow(dead_code, unused_imports)]
pub(crate) mod space_mining_residue {
    use super::*;

//  Mining ledger (`MN-03`, `MN-16`) and Track 1 regression tests (split from `space_mining.rs`).


use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::system_actor::AsteroidState;
use glam::DVec3;
use std::collections::HashMap;

#[tokio::test]
async fn test_mn03_mining_ledger_recording_and_char_mining_ledger_service() {
    // parity: feat:mining.character-ledger, rpc:authentication.Login, rpc:charUnboundMgr.SelectCharacterID, rpc:characterMiningLedger.GetCharacterLogs
    eve_testkit::attest(&["feat:mining.character-ledger"]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(963);

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
    let ship_id = ctx.db.with_snapshot(|snap| snap.characters[&90_000_001].active_ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, i| !(i.location_id == ship_id && i.flag == eve_db::types::InventoryFlag::HiSlot0));
        snap.items.insert(
            750_000_902,
            eve_db::InventoryItemRecord {
                item_id: 750_000_902,
                type_id: 483, // Miner I
                owner_id: 90_000_001,
                location_id: ship_id,
                flag: eve_db::types::InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(750_000_902, true);
        snap.skills.entry(90_000_001).or_default().insert(
            3386,
            eve_db::TrainedSkillRecord {
                type_id: 3386,
                level: 5,
                skill_points: 256_000.0,
            },
        );
    });
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
    assert_eq!(ship_id, undock_kv["shipID"].as_i64().unwrap());

    let jita = ctx.systems.get_or_spawn(30_000_142);
    let (_, ship_ball) = jita.get_ball_snapshot(ship_id).await.unwrap().unwrap();

    // Spawn and mine Scordite asteroid (type 1228)
    let scordite_ast_id = 750_000_228i64;
    jita.spawn_asteroid(
        AsteroidState {
            asteroid_id: scordite_ast_id,
            belt_id: 40_000_001,
            type_id: 1228,
            initial_units: 1_000,
            remaining_units: 1_000,
            unit_volume: 0.15,
            depleted: false,
            respawn_at_tick: None,
        },
        Ball {
            id: scordite_ast_id,
            type_id: 1228,
            position: ship_ball.position + DVec3::new(1_200.0, 0.0, 0.0),
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

    let mine_res = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(beyonce_id),
            "CmdMineRepeatedly",
            &[PyRep::Integer(scordite_ast_id)],
            &kw,
            None,
        )
        .await
        .expect("CmdMine");
    let PyRep::KeyVal(mine_kv) = &mine_res else { panic!("Expected KeyVal") };
    let mined_units = mine_kv["minedUnits"].as_i64().unwrap();

    // Execute a second mining cycle on the same asteroid on Day 1 so we can verify per-day/per-type/per-system aggregation
    let mine_res_2 = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(beyonce_id),
            "CmdMineRepeatedly",
            &[PyRep::Integer(scordite_ast_id)],
            &kw,
            None,
        )
        .await
        .expect("CmdMine #2");
    let PyRep::KeyVal(mine_kv_2) = &mine_res_2 else { panic!("Expected KeyVal") };
    let mined_units_2 = mine_kv_2["minedUnits"].as_i64().unwrap();
    let expected_day1_scordite = mined_units + mined_units_2;

    // Advance virtual clock by 1 day (Day 2) and mine Scordite ore (1228), Clear Icicle ice (16262), and Fullerite-C50 gas (30370)
    ctx.advance_time_seconds(86_400);
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(90_000_001).or_default();
        for sid in [3386u32, 16281, 25544] {
            sk.insert(sid, eve_db::TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 });
        }
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.type_id = 22548; // Mackinaw (Exhumer group 543, 31,500 m3 Mining Hold)
            sh.dynamic_attributes.extend([(14, 3.0), (102, 3.0), (11, 500.0), (48, 500.0)]);
        }
        snap.items.retain(|_, it| {
            !(it.location_id == ship_id
                && matches!(
                    it.flag,
                    eve_db::InventoryFlag::HiSlot0
                        | eve_db::InventoryFlag::HiSlot1
                        | eve_db::InventoryFlag::HiSlot2
                ))
        });
        for (iid, tid, flag) in [
            (750_000_911i64, 483u32, eve_db::InventoryFlag::HiSlot0),
            (750_000_912i64, 16278u32, eve_db::InventoryFlag::HiSlot1),
            (750_000_913i64, 25266u32, eve_db::InventoryFlag::HiSlot2),
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
    let ice_ast_id = 750_000_229i64;
    let gas_ast_id = 750_000_230i64;
    for (aid, tid, units, uvol) in [
        (ice_ast_id, 16262u32, 50u64, 1_000.0_f64),
        (gas_ast_id, 30370u32, 500u64, 1.0_f64),
    ] {
        jita.spawn_asteroid(
            AsteroidState {
                asteroid_id: aid,
                belt_id: 40_000_001,
                type_id: tid,
                initial_units: units,
                remaining_units: units,
                unit_volume: uvol,
                depleted: false,
                respawn_at_tick: None,
            },
            Ball {
                id: aid,
                type_id: tid,
                position: ship_ball.position + DVec3::new(1_100.0, 0.0, 0.0),
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
    }
    let day2_ore_res = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(beyonce_id),
            "CmdMineRepeatedly",
            &[PyRep::Integer(scordite_ast_id)],
            &kw,
            None,
        )
        .await
        .expect("Day 2 ore mine");
    let day2_scordite = day2_ore_res.dict_get("minedUnits").unwrap().as_i64().unwrap();
    let day2_ice_res = ctx
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
        .expect("Day 2 ice mine");
    let day2_ice = day2_ice_res.dict_get("minedUnits").unwrap().as_i64().unwrap();
    let day2_gas_res = ctx
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
        .expect("Day 2 gas mine");
    let day2_gas = day2_gas_res.dict_get("minedUnits").unwrap().as_i64().unwrap();

    // Query mining ledger via canonical V24 RPC service characterMiningLedger.GetCharacterLogs
    let ledger_res = ctx
        .dispatch_rpc(
            &mut sess,
            "characterMiningLedger",
            None,
            "GetCharacterLogs",
            &[],
            &kw,
            None,
        )
        .await
        .expect("characterMiningLedger.GetCharacterLogs");
    let PyRep::List(entries) = &ledger_res else { panic!("Expected List of ledger entries") };
    assert!(
        entries.len() >= 4,
        "Mining ledger must contain Day 1 Scordite + Day 2 Scordite + Day 2 Ice + Day 2 Gas"
    );

    let scordite_rows: Vec<&HashMap<String, PyRep>> = entries
        .iter()
        .filter_map(|e| {
            if let PyRep::KeyVal(kv) = e {
                if kv.get("typeID").and_then(|v| v.as_i64()) == Some(1228)
                    && kv.get("solarSystemID").and_then(|v| v.as_i64()) == Some(30_000_142)
                {
                    return Some(kv);
                }
            }
            None
        })
        .collect();
    assert_eq!(
        scordite_rows.len(),
        2,
        "Day 1 (2 cycles aggregated) and Day 2 Scordite must appear as 2 distinct daily rows"
    );
    assert_eq!(
        scordite_rows[0].get("quantity").and_then(|v| v.as_i64()),
        Some(expected_day1_scordite),
        "Day 1 aggregated ledger row must sum both mining cycles ({mined_units} + {mined_units_2} = {expected_day1_scordite})"
    );
    assert_eq!(
        scordite_rows[1].get("quantity").and_then(|v| v.as_i64()),
        Some(day2_scordite)
    );
    assert!(entries.iter().any(|e| {
        e.dict_get("typeID").and_then(|v| v.as_i64()) == Some(16262)
            && e.dict_get("groupID").and_then(|v| v.as_i64()) == Some(465)
            && e.dict_get("quantity").and_then(|v| v.as_i64()) == Some(day2_ice)
    }), "Character mining ledger must record harvested Ice (16262)");
    assert!(entries.iter().any(|e| {
        e.dict_get("typeID").and_then(|v| v.as_i64()) == Some(30370)
            && e.dict_get("groupID").and_then(|v| v.as_i64()) == Some(711)
            && e.dict_get("quantity").and_then(|v| v.as_i64()) == Some(day2_gas)
    }), "Character mining ledger must record harvested Gas (30370)");

    // Advance virtual clock by another 90 days (so Day 1 is now 91 days old and outside the 90-day rolling window, while Day 2 is 90 days old)
    ctx.advance_time_seconds(90 * 86_400 + 60);
    let ledger_after_91d = ctx
        .dispatch_rpc(
            &mut sess,
            "characterMiningLedger",
            None,
            "GetCharacterLogs",
            &[],
            &kw,
            None,
        )
        .await
        .expect("characterMiningLedger.GetCharacterLogs after 91d");
    let PyRep::List(entries_after) = &ledger_after_91d else { panic!("Expected List") };
    let has_day1_entry = entries_after.iter().any(|e| {
        e.dict_get("typeID").and_then(|v| v.as_i64()) == Some(1228)
            && e.dict_get("quantity").and_then(|v| v.as_i64()) == Some(expected_day1_scordite)
    });
    assert!(
        !has_day1_entry,
        "Mining ledger entries older than 90 days must age out of characterMiningLedger.GetCharacterLogs"
    );
}

#[tokio::test]
async fn test_r3_track1_mining_pve_fw_regressions() {
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(964);

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
    let char_id = 90_000_001_i64;
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

    let undock = ctx
        .dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let PyRep::KeyVal(undock_kv) = &undock else { panic!("Expected KeyVal") };
    let beyonce_id = undock_kv["beyonceBoundID"].as_i64().unwrap() as u64;
    let ship_id = undock_kv["shipID"].as_i64().unwrap();

    // #111: Cross-system DB asteroid must not be cloned into caller's system
    let foreign_ast_id = 755_000_999_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.asteroids.insert(
            foreign_ast_id,
            eve_db::AsteroidRecord {
                asteroid_id: foreign_ast_id,
                belt_id: 40_000_999,
                solar_system_id: 30_002_187, // Amarr
                type_id: 1230,
                quantity: 5_000,
                initial_quantity: 5_000,
                position: (0.0, 0.0, 0.0),
                radius: 50.0,
            },
        );
    });
    let cross_sys_err = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(beyonce_id),
            "CmdMineRepeatedly",
            &[PyRep::Integer(foreign_ast_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Cross-system asteroid must be rejected");
    assert_eq!(cross_sys_err.code, "TargetNotInSystem");

    // #112: warpVectorMgr.UseVector rejects self-targeting and caps displacement
    let self_vec_err = ctx
        .dispatch_rpc(
            &mut sess,
            "warpVectorMgr",
            None,
            "UseVector",
            &[PyRep::Integer(ship_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Self-targeting UseVector must fail");
    assert_eq!(self_vec_err.code, "InvalidArgument");

    // #113: Data/Relic/Ghost sites cannot be completed via EnterCombatAnomaly while containers are unhacked
    eve_server::services::pve_exploration::ensure_system_pve_sites_seeded(&ctx, 30_000_142).await;
    let dat_pos = eve_server::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system
            .get(&30_000_142)
            .and_then(|l| l.iter().find(|s| s.site_id == "DAT-30000142"))
            .map(|s| eve_destiny::DVec3::from_array(s.position))
            .unwrap()
    });
    assert!(ctx.systems.get_or_spawn(30_000_142).set_ball_position(ship_id, dat_pos).await);
    let unhacked_err = ctx
        .dispatch_rpc(
            &mut sess,
            "scanSvc",
            None,
            "EnterCombatAnomaly",
            &[PyRep::String("DAT-30000142".into()), PyRep::Bool(true)],
            &kw,
            None,
        )
        .await
        .expect_err("Data site with unhacked cans must reject wave completion");
    assert_eq!(unhacked_err.code, "SiteContainersUnhacked");

    // #115: Capsules cannot join Proving Grounds queue
    let fil_id = 755_001_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 670; // Capsule
        }
        snap.items.insert(
            fil_id,
            eve_db::InventoryItemRecord {
                item_id: fil_id,
                type_id: 56_303,
                owner_id: char_id,
                location_id: ship_id,
                flag: eve_db::InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                ..Default::default()
            },
        );
    });
    let pod_pvp_err = ctx
        .dispatch_rpc(
            &mut sess,
            "pvpFilamentMgr",
            None,
            "JoinPVPQueue",
            &[PyRep::Integer(fil_id), PyRep::Integer(56_303)],
            &kw,
            None,
        )
        .await
        .expect_err("Capsule must be rejected from JoinPVPQueue");
    assert_eq!(pod_pvp_err.code, "InvalidShipForFilament");

    // #116: GetExtractionsForCorp rejects foreign corporation IDs
    let foreign_corp_err = ctx
        .dispatch_rpc(
            &mut sess,
            "moonExtractions",
            None,
            "GetExtractionsForCorp",
            &[PyRep::Integer(98_999_999)],
            &kw,
            None,
        )
        .await
        .expect_err("Foreign corp moon extraction query must be denied");
    assert_eq!(foreign_corp_err.code, "AccessDenied");

    // #117: award_fw_kill_lp does not double-credit LP or credit unenlisted pilots as Caldari
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.war_faction_id = None;
            ch.loyalty_points.clear();
        }
    });
    let _ = eve_server::services::fw_services::award_fw_kill_lp(&ctx, char_id, 30_002_537, 100_000_000.0);
    let unenlisted_lp_empty = ctx
        .db
        .with_snapshot(|s| s.characters[&char_id].loyalty_points.is_empty());
    assert!(unenlisted_lp_empty, "Unenlisted character must not receive Caldari LP");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.war_faction_id = Some(500_002); // Minmatar Republic
        }
    });
    let awarded = eve_server::services::fw_services::award_fw_kill_lp(&ctx, char_id, 30_002_537, 100_000_000.0);
    let total_lp: i64 = ctx
        .db
        .with_snapshot(|s| s.characters[&char_id].loyalty_points.values().copied().sum());
    assert_eq!(total_lp, awarded, "Enlisted pilot must receive single militia corp LP without double-crediting");
}

}

#[allow(unused_imports, dead_code)]
mod mining_crystals_reprocessing_and_foreman_bursts {
    use super::*;

// Phase 5M mining crystal tiers, scrapmetal exclusions, Prismaticite/Unrefined Mineral
// reactions, Mining Foreman Burst charges, and phased asteroid fields (`MN-07`..`MN-08`).


use eve_db::{
    InventoryFlag, InventoryItemRecord, IskCents, MiningLedgerRecord, TrainedSkillRecord,
    UpwellStructureRecord,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::{BTreeSet, HashMap};

async fn setup_session(ctx: &ServerContext, session_id: u64, user: &str, secret: &str, char_id: i64) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(session_id);
    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[PyRep::String(user.into()), PyRep::String(secret.into())],
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
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");
    sess
}

fn make_item(id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, qty: i32, singleton: bool) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id: id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity: qty,
        singleton,
        version: 1,
        ..Default::default()
    }
}

fn get_dict_val<'a>(dict: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match dict {
        PyRep::KeyVal(entries) => entries.iter().find(|(k, _)| k.as_str() == key).map(|(_, v)| v),
        PyRep::Dict(entries) => entries
            .iter()
            .find(|(k, _)| matches!(k, PyRep::String(s) if s == key))
            .map(|(_, v)| v),
        _ => None,
    }
}

#[tokio::test]
async fn test_mining_crystal_tier_ore_compatibility_and_loading_rules() {
    // parity: feat:mining.crystal-tier-ore-compatibility
    eve_testkit::attest(&["feat:mining.crystal-tier-ore-compatibility"]);
    use crate::mining_cycle_engine::{activate, advance, lock, pilot, rock, rpc, CHAR_ID, JITA, VELDSPAR};
    const MINER_I: u32 = 483;
    const STRIP_I: u32 = 17_482;
    const MOD_STRIP_II: u32 = 17_912;
    const MOD_DEEP_STRIP_II: u32 = 24_305;
    const ORE_DEEP_CORE: u32 = 28_748;
    const BANIDINE: u32 = 28_617;

    let (mod_id, crystal_id, cargo_crystal_id) = (100_000_865_i64, 100_000_866_i64, 100_000_867_i64);
    let skills = [
        (3_386, 5), (3_410, 5), (3_402, 5), (3_380, 5), (3_327, 5), (11_395, 5),
        (17_940, 5), (32_918, 5), (3_429, 5), (60_377, 5), (60_378, 5), (60_379, 5),
        (60_380, 5), (60_381, 5), (12_189, 5), (46_152, 5), (46_153, 5), (46_154, 5),
        (46_155, 5), (46_156, 5),
    ];
    let mut p = pilot(17_478, &skills, &[(mod_id, MOD_STRIP_II, InventoryFlag::HiSlot0)]).await;
    let bid = p.beyonce_id;
    rpc(&mut p, "beyonce", Some(bid), "CmdStop", &[]).await.unwrap();
    p.ctx.set_mechanic_rng_seed(Some(7));

    let tiers: &[(u32, u32, u32, u32)] = &[
        (MOD_STRIP_II, 60_276, VELDSPAR, 20),
        (MOD_STRIP_II, 60_285, 20, VELDSPAR),
        (MOD_STRIP_II, 60_291, 1_225, VELDSPAR),
        (MOD_STRIP_II, 60_297, 1_223, VELDSPAR),
        (MOD_STRIP_II, 60_303, 52_306, VELDSPAR),
        (MOD_DEEP_STRIP_II, 18_054, 11_396, VELDSPAR),
        (MOD_STRIP_II, 46_355, 45_490, 45_494),
        (MOD_STRIP_II, 46_365, 45_494, 45_490),
        (MOD_STRIP_II, 46_367, 45_498, 45_490),
        (MOD_STRIP_II, 46_369, 45_502, 45_490),
        (MOD_STRIP_II, 46_371, 45_510, 45_490),
    ];

    let base_m3 = 120.0 * 1.25 * 1.25 * 1.15 * 1.10;
    let sys = p.ctx.systems.get_or_spawn(JITA);
    let max_cap = sys.get_combat_state(p.ship_id).await.unwrap().unwrap().max_capacitor;

    for (idx, &(miner_tid, crystal_tid, match_ore, mismatch_ore)) in tiers.iter().enumerate() {
        sys.set_ball_position(p.ship_id, p.ship_pos).await;
        let match_rock = 1_000_000_200_i64 + (idx as i64) * 2;
        let mismatch_rock = match_rock + 1;
        rock(&p, match_rock, match_ore, 100_000, 2_000.0).await;
        rock(&p, mismatch_rock, mismatch_ore, 100_000, 2_000.0).await;
        lock(&mut p, match_rock).await;
        lock(&mut p, mismatch_rock).await;
        p.ctx.db.with_mut_snapshot(|snap| {
            snap.items.get_mut(&mod_id).unwrap().type_id = miner_tid;
            snap.items.insert(crystal_id, make_item(crystal_id, crystal_tid, CHAR_ID, p.ship_id, InventoryFlag::HiSlot0, 1, true));
        });
        p.ctx.invalidate_ship_fitting(p.ship_id);

        let expected_base = if miner_tid == MOD_DEEP_STRIP_II { 80.0 * 1.25 * 1.25 * 1.15 * 1.10 } else { base_m3 };
        sys.set_ball_position(p.ship_id, p.ship_pos).await;
        sys.set_capacitor(p.ship_id, max_cap).await.unwrap();
        activate(&mut p, mod_id, match_rock).await.expect("activate on matching ore");
        let out_match = advance(&p, 45).await;
        assert_eq!(out_match[0].crystal.map(|c| (c.type_id, c.matched)), Some((crystal_tid, true)));
        assert!((out_match[0].mining_amount_m3 - expected_base * 1.5).abs() < 1e-9, "tier {idx} matched yield");
        rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(mod_id), PyRep::String("miningLaser".into())]).await.unwrap();

        sys.set_ball_position(p.ship_id, p.ship_pos).await;
        sys.set_capacitor(p.ship_id, max_cap).await.unwrap();
        activate(&mut p, mod_id, mismatch_rock).await.expect("activate on mismatched ore");
        let out_mis = advance(&p, 45).await;
        assert_eq!(out_mis[0].crystal.map(|c| (c.type_id, c.matched)), Some((crystal_tid, false)));
        assert!((out_mis[0].mining_amount_m3 - expected_base).abs() < 1e-9, "tier {idx} mismatched yield");
        rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(mod_id), PyRep::String("miningLaser".into())]).await.unwrap();

        rpc(&mut p, "dogmaIM", None, "RemoveTarget", &[PyRep::Integer(match_rock)]).await.unwrap();
        rpc(&mut p, "dogmaIM", None, "RemoveTarget", &[PyRep::Integer(mismatch_rock)]).await.unwrap();
    }

    sys.set_ball_position(p.ship_id, p.ship_pos).await;
    let banidine_rock = 1_000_000_250_i64;
    rock(&p, banidine_rock, BANIDINE, 100_000, 2_000.0).await;
    lock(&mut p, banidine_rock).await;
    p.ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&mod_id).unwrap().type_id = MOD_STRIP_II;
        snap.items.get_mut(&crystal_id).unwrap().type_id = 60_276;
    });
    p.ctx.invalidate_ship_fitting(p.ship_id);
    sys.set_capacitor(p.ship_id, max_cap).await.unwrap();
    activate(&mut p, mod_id, banidine_rock).await.expect("activate on mission-unique Banidine");
    let out_mission = advance(&p, 45).await;
    assert_eq!(out_mission[0].crystal.map(|c| c.matched), Some(false));
    assert!((out_mission[0].mining_amount_m3 - base_m3).abs() < 1e-9);
    rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(mod_id), PyRep::String("miningLaser".into())]).await.unwrap();

    p.ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&crystal_id);
        snap.items.insert(cargo_crystal_id, make_item(cargo_crystal_id, 60_276, CHAR_ID, p.ship_id, InventoryFlag::Cargo, 5, false));
    });
    let ship_id = p.ship_id;
    for unmod_tid in [MINER_I, STRIP_I] {
        p.ctx.db.with_mut_snapshot(|snap| snap.items.get_mut(&mod_id).unwrap().type_id = unmod_tid);
        p.ctx.invalidate_ship_fitting(ship_id);
        let err = rpc(&mut p, "dogmaIM", None, "LoadAmmo", &[PyRep::Integer(ship_id), PyRep::List(vec![PyRep::Integer(mod_id)]), PyRep::List(vec![PyRep::Integer(cargo_crystal_id)])]).await.unwrap_err();
        assert_eq!(err, "IncompatibleAmmo");
    }

    p.ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&mod_id).unwrap().type_id = MOD_STRIP_II;
        snap.items.get_mut(&cargo_crystal_id).unwrap().type_id = 18_054;
    });
    p.ctx.invalidate_ship_fitting(ship_id);
    let err = rpc(&mut p, "dogmaIM", None, "LoadAmmo", &[PyRep::Integer(ship_id), PyRep::List(vec![PyRep::Integer(mod_id)]), PyRep::List(vec![PyRep::Integer(cargo_crystal_id)])]).await.unwrap_err();
    assert_eq!(err, "IncompatibleAmmo");

    for (miner_tid, cryst_tid, req_skill_id) in [
        (MOD_STRIP_II, 60_276_u32, 60_377_u32),
        (MOD_DEEP_STRIP_II, 18_054_u32, 12_189_u32),
        (ORE_DEEP_CORE, 18_054_u32, 12_189_u32),
    ] {
        p.ctx.db.with_mut_snapshot(|snap| {
            snap.items.retain(|&id, it| id == mod_id || id == cargo_crystal_id || it.flag != InventoryFlag::HiSlot0);
            snap.items.get_mut(&mod_id).unwrap().type_id = miner_tid;
            let ch = snap.items.get_mut(&cargo_crystal_id).unwrap();
            ch.type_id = cryst_tid;
            ch.location_id = ship_id;
            ch.flag = InventoryFlag::Cargo;
            ch.quantity = 5;
            snap.skills.get_mut(&CHAR_ID).unwrap().remove(&req_skill_id);
        });
        p.ctx.invalidate_ship_fitting(ship_id);
        let sk_err = rpc(&mut p, "dogmaIM", None, "LoadAmmo", &[PyRep::Integer(ship_id), PyRep::List(vec![PyRep::Integer(mod_id)]), PyRep::List(vec![PyRep::Integer(cargo_crystal_id)])]).await.unwrap_err();
        assert_eq!(sk_err, "SkillPrerequisitesNotMet");

        p.ctx.db.with_mut_snapshot(|snap| {
            snap.skills.entry(CHAR_ID).or_default().insert(req_skill_id, TrainedSkillRecord { type_id: req_skill_id, level: 5, skill_points: 256_000.0 });
        });
        p.ctx.invalidate_ship_fitting(ship_id);
        rpc(&mut p, "dogmaIM", None, "LoadAmmo", &[PyRep::Integer(ship_id), PyRep::List(vec![PyRep::Integer(mod_id)]), PyRep::List(vec![PyRep::Integer(cargo_crystal_id)])]).await.unwrap();
    }
}

#[tokio::test]
async fn test_mining_reprocessing_curves_scrapmetal_and_prismaticite() {
    // parity: feat:mining.npc-reprocessing-equipment-and-tax-curve, feat:mining.scrapmetal-yield-exclusions, feat:mining.prismaticite-reprocessing-and-unrefined-reactions
    eve_testkit::attest(&[
        "feat:mining.npc-reprocessing-equipment-and-tax-curve",
        "feat:mining.scrapmetal-yield-exclusions",
        "feat:mining.prismaticite-reprocessing-and-unrefined-reactions",
    ]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let char_id = 90_000_001_i64;
    let mut sess = setup_session(&ctx, 902, "demo", "demo-secret", char_id).await;

    // 1. NPC Reprocessing equipment base yield & standing tax curve (0.0 -> 5%, 6.67 -> 0%)
    let jita_station = 60_003_760_i64;
    sess.station_id = Some(jita_station);
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.station_id = Some(jita_station);
        ch.balance = IskCents::from_isk(500_000_000);
        ch.standings.insert(1_000_035, 0.0);
    });
    let quote_zero = ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "GetQuotes", &[], &kw, None).await.unwrap();
    assert!((get_dict_val(&quote_zero, "stationTaxRate").and_then(|v| v.as_f64()).unwrap() - 0.05).abs() < 1e-6);

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().standings.insert(1_000_035, 6.67);
    });
    let quote_high = ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "GetQuotes", &[], &kw, None).await.unwrap();
    assert!((get_dict_val(&quote_high, "stationTaxRate").and_then(|v| v.as_f64()).unwrap() - 0.0).abs() < 1e-6);

    // 2. Scrapmetal yield exclusions (`feat:mining.scrapmetal-yield-exclusions`):
    // Dock in a Null-Sec rigged Tatara (35836) with Reprocessing V, Reprocessing Efficiency V,
    // Simple Ore Processing V, Scrapmetal Processing V, and RX-804 (+4% implant 27170).
    let ns_sys = ctx.sde.solar_systems.values().find(|s| s.security_status < -0.2 && s.solar_system_id < 31_000_000).unwrap().solar_system_id;
    let rig_tid = ctx.sde.types.values().find(|t| matches!(t.group_id, 1941..=1945) && t.default_attributes.get(&1547) == Some(&3.0)).map(|t| t.type_id).unwrap_or(46_497);
    let tatara_id = 1_045_000_000_801_i64;
    let corp_id = ctx.db.with_snapshot(|s| s.characters[&char_id].corporation_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(tatara_id, UpwellStructureRecord {
            structure_id: tatara_id, type_id: 35_836, owner_corp_id: corp_id, solar_system_id: ns_sys,
            position: [0.0, 0.0, 0.0], name: "NS Rigged Tatara".into(), state: "ShieldVulnerable".into(),
            fuel_blocks: 5_000, quantum_core_installed: true, shield_hp: 1.0, armor_hp: 1.0, hull_hp: 1.0,
            reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
        });
        snap.items.insert(100_000_868, make_item(100_000_868, 35_899, corp_id, tatara_id, InventoryFlag::ServiceSlot0, 1, true));
        snap.items.insert(100_000_869, make_item(100_000_869, rig_tid, corp_id, tatara_id, InventoryFlag::RigSlot0, 1, true));
        snap.module_online_states.insert(100_000_868, true);
        let sk = snap.skills.entry(char_id).or_default();
        for (sid, lvl) in [(3_385, 5), (3_389, 5), (60_377, 5), (12_196, 5), (45_746, 5), (90_398, 5)] {
            sk.insert(sid, TrainedSkillRecord { type_id: sid, level: lvl, skill_points: 256_000.0 });
        }
        snap.active_implants.entry(char_id).or_default().push(27_170); // RX-804 +4% reprocessing yield
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.solar_system_id = ns_sys;
        ch.station_id = None;
        ch.structure_id = Some(tatara_id);
        snap.items.insert(100_000_870, make_item(100_000_870, 482, char_id, tatara_id, InventoryFlag::Hangar, 1, false));   // Miner II module
        snap.items.insert(100_000_871, make_item(100_000_871, 587, char_id, tatara_id, InventoryFlag::Hangar, 1, false));   // Packaged Rifter
        snap.items.insert(100_000_872, make_item(100_000_872, 587, char_id, tatara_id, InventoryFlag::Hangar, 1, true));    // Assembled Rifter (must fail)
        snap.items.insert(100_000_873, make_item(100_000_873, 1_230, char_id, tatara_id, InventoryFlag::Hangar, 1_000, false)); // Veldspar ore
    });
    sess.station_id = None;
    sess.structure_id = Some(tatara_id);
    sess.solar_system_id = Some(ns_sys);

    // Assembled ship is rejected
    let asm_err = ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(100_000_872)], &kw, None).await.unwrap_err();
    assert_eq!(asm_err.code, "ItemMustBeRepackaged");

    // Ore at rigged NS Tatara receives structure + rig + nullsec + skills + RX-804 (> 75%)
    let ore_rep = ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(100_000_873)], &kw, None).await.unwrap();
    let ore_yield = get_dict_val(&ore_rep, "yieldRate").and_then(|v| v.as_f64()).unwrap();
    assert!(ore_yield > 0.75, "Ore at rigged NS Tatara must receive structure/rig/nullsec/implant bonuses (got {ore_yield})");

    // V24 client Dict {itemID: invItem} input to GetQuotes preserves typeID (Miner I = 482) and non-empty outputs
    let miner_dict_quote = ctx
        .dispatch_rpc(
            &mut sess,
            "reprocessingSvc",
            None,
            "GetQuotes",
            &[
                PyRep::Dict(vec![(
                    PyRep::Integer(100_000_870),
                    PyRep::keyval([("itemID", PyRep::Integer(100_000_870))]),
                )]),
                PyRep::Integer(char_id),
            ],
            &kw,
            None,
        )
        .await
        .unwrap();
    let PyRep::Tuple(ref m_parts) = miner_dict_quote else {
        panic!("expected 3-tuple from GetQuotes Dict, got {miner_dict_quote:?}");
    };
    let PyRep::List(ref m_quotes) = m_parts[2] else {
        panic!("expected quote list");
    };
    assert_eq!(m_quotes.len(), 1);
    assert_eq!(get_dict_val(&m_quotes[0], "itemID").and_then(|v| v.as_i64()), Some(100_000_870));
    assert_eq!(get_dict_val(&m_quotes[0], "typeID").and_then(|v| v.as_i64()), Some(482));
    assert!(!matches!(get_dict_val(&m_quotes[0], "outputs"), Some(PyRep::List(o)) if o.is_empty()));

    // Module (482) and packaged ship (587) ignore Upwell structure, rig, security-band, Reprocessing, and RX-804 bonuses -> exact 55% at Scrapmetal V
    let mod_rep = ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(100_000_870)], &kw, None).await.unwrap();
    let ship_rep = ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(100_000_871)], &kw, None).await.unwrap();
    assert!((get_dict_val(&mod_rep, "yieldRate").and_then(|v| v.as_f64()).unwrap() - 0.55).abs() < 1e-6);
    assert!((get_dict_val(&ship_rep, "yieldRate").and_then(|v| v.as_f64()).unwrap() - 0.55).abs() < 1e-6);

    // Scrapmetal Processing III -> exact 0.50 * (1 + 0.02 * 3) = 0.53 even in rigged NS Tatara with RX-804
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.get_mut(&char_id).unwrap().get_mut(&12_196).unwrap().level = 3;
        snap.items.insert(100_000_874, make_item(100_000_874, 482, char_id, tatara_id, InventoryFlag::Hangar, 1, false));
    });
    let mod_lvl3 = ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(100_000_874)], &kw, None).await.unwrap();
    assert!((get_dict_val(&mod_lvl3, "yieldRate").and_then(|v| v.as_f64()).unwrap() - 0.53).abs() < 1e-6);

    // 3. Prismaticite per-batch random mineral & Unrefined Mineral reactions (`feat:mining.prismaticite-reprocessing-and-unrefined-reactions`):
    let mut single_batch_minerals = BTreeSet::new();
    let mut single_batch_qtys = BTreeSet::new();
    for (idx, seed) in [11_u64, 29, 73, 151, 313, 619].into_iter().enumerate() {
        let iid = 100_000_880 + idx as i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(iid, make_item(iid, 90_041, char_id, tatara_id, InventoryFlag::Hangar, 100, false));
        });
        ctx.set_mechanic_rng_seed(Some(seed));
        let rep = ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(iid)], &kw, None).await.unwrap();
        let PyRep::List(outs) = get_dict_val(&rep, "outputs").unwrap() else { panic!("expected outputs list") };
        assert_eq!(outs.len(), 1, "Single 100-unit Prismaticite batch must yield exactly 1 mineral type");
        let tid = get_dict_val(&outs[0], "typeID").and_then(|v| v.as_i64()).unwrap() as u32;
        let qty = get_dict_val(&outs[0], "quantity").and_then(|v| v.as_i64()).unwrap();
        assert!(ctx.sde.types.get(&tid).is_some_and(|t| t.group_id == 18));
        single_batch_minerals.insert(tid);
        single_batch_qtys.insert((tid, qty));
    }
    assert!(single_batch_minerals.len() >= 2, "Different RNG seeds must roll distinct mineral types across Prismaticite batches");

    // Multi-batch stack (1,200 Prismaticite = 12 batches) rolls independently per batch into multiple minerals:
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(100_000_890, make_item(100_000_890, 90_041, char_id, tatara_id, InventoryFlag::Hangar, 1_200, false));
    });
    ctx.set_mechanic_rng_seed(Some(42));
    let multi_rep = ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(100_000_890)], &kw, None).await.unwrap();
    let PyRep::List(multi_outs) = get_dict_val(&multi_rep, "outputs").unwrap() else { panic!() };
    assert!(multi_outs.len() >= 2, "12-batch Prismaticite stack must aggregate independent per-batch mineral rolls");

    // Unrefined Mineral reaction (90274 Unrefined Tritanium Formula, group 1888):
    // Negative: having only Standup Biochemical Reactor I (45539) online rejects InstallJob with FacilityServiceOffline!
    let (formula_id, biochem_svc_id, comp_svc_id) = (100_000_891_i64, 100_000_892_i64, 100_000_893_i64);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(formula_id, make_item(formula_id, 90_274, char_id, tatara_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(100_000_894, make_item(100_000_894, 90_307, char_id, tatara_id, InventoryFlag::Hangar, 200, false)); // Compressed Prismaticite
        snap.items.insert(100_000_895, make_item(100_000_895, 16_634, char_id, tatara_id, InventoryFlag::Hangar, 200, false)); // Atmospheric Gases
        let mut biochem = make_item(biochem_svc_id, 45_539, corp_id, tatara_id, InventoryFlag::ServiceSlot1, 1, true);
        biochem.dynamic_attributes.insert(2, 1.0);
        snap.items.insert(biochem_svc_id, biochem);
        snap.module_online_states.insert(biochem_svc_id, true);
    });
    let job_args = [PyRep::keyval([
        ("blueprintID", PyRep::Integer(formula_id)),
        ("facilityID", PyRep::Integer(tatara_id)),
        ("activityID", PyRep::Integer(9)),
        ("runs", PyRep::Integer(2)),
    ])];
    let wrong_reactor_err = ctx.dispatch_rpc(&mut sess, "industryManager", None, "InstallJob", &job_args, &kw, None).await.unwrap_err();
    assert_eq!(wrong_reactor_err.code, "CompositeReactorRequired", "Unrefined Mineral reaction must require Standup Composite Reactor I (45537)");

    // Online Standup Composite Reactor I (45537) -> InstallJob succeeds -> CompleteJob delivers 200x Unrefined Tritanium (90283)
    ctx.db.with_mut_snapshot(|snap| {
        let mut comp = make_item(comp_svc_id, 45_537, corp_id, tatara_id, InventoryFlag::ServiceSlot2, 1, true);
        comp.dynamic_attributes.insert(2, 1.0);
        snap.items.insert(comp_svc_id, comp);
        snap.module_online_states.insert(comp_svc_id, true);
    });
    let inst_rep = ctx.dispatch_rpc(&mut sess, "industryManager", None, "InstallJob", &job_args, &kw, None).await.expect("Install Unrefined Tritanium reaction");
    let job_id = get_dict_val(&inst_rep, "jobID").and_then(|v| v.as_i64()).unwrap();
    ctx.advance_secs(800);
    ctx.dispatch_rpc(&mut sess, "industryManager", None, "CompleteJob", &[PyRep::Integer(job_id)], &kw, None).await.expect("Complete Unrefined Tritanium reaction");

    let unrefined_ids: Vec<i64> = ctx.db.with_snapshot(|snap| {
        snap.items.values().filter(|i| i.owner_id == char_id && i.location_id == tatara_id && i.type_id == 90_283).map(|i| i.item_id).collect()
    });
    assert!(!unrefined_ids.is_empty(), "Reaction must deliver Unrefined Tritanium (90283)");

    // Split into two 100-unit batches and reprocess under distinct RNG seeds -> both yield Tritanium (34) in variable quantities!
    let (u1, u2) = (100_000_896_i64, 100_000_897_i64);
    ctx.db.with_mut_snapshot(|snap| {
        for id in &unrefined_ids { snap.items.remove(id); }
        snap.items.insert(u1, make_item(u1, 90_283, char_id, tatara_id, InventoryFlag::Hangar, 100, false));
        snap.items.insert(u2, make_item(u2, 90_283, char_id, tatara_id, InventoryFlag::Hangar, 100, false));
    });
    ctx.set_mechanic_rng_seed(Some(101));
    let u_rep1 = ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(u1)], &kw, None).await.unwrap();
    ctx.set_mechanic_rng_seed(Some(999_983));
    let u_rep2 = ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(u2)], &kw, None).await.unwrap();
    let PyRep::List(u_out1) = get_dict_val(&u_rep1, "outputs").unwrap() else { panic!() };
    let PyRep::List(u_out2) = get_dict_val(&u_rep2, "outputs").unwrap() else { panic!() };
    assert_eq!(get_dict_val(&u_out1[0], "typeID").and_then(|v| v.as_i64()), Some(34));
    assert_eq!(get_dict_val(&u_out2[0], "typeID").and_then(|v| v.as_i64()), Some(34));
    let q1 = get_dict_val(&u_out1[0], "quantity").and_then(|v| v.as_i64()).unwrap();
    let q2 = get_dict_val(&u_out2[0], "quantity").and_then(|v| v.as_i64()).unwrap();
    assert_ne!(q1, q2, "Unrefined Tritanium reprocessing must produce variable Tritanium output across batches ({q1} vs {q2})");
}

#[tokio::test]
async fn test_mining_foreman_burst_charges_orca_porpoise_rorqual_and_preservation() {
    // parity: feat:mining.foreman-burst-charges
    eve_testkit::attest(&["feat:mining.foreman-burst-charges"]);
    use crate::mining_cycle_engine::{activate, advance, lock, pilot, rock, rpc, CHAR_ID, JITA, VELDSPAR};
    const ORCA: u32 = 28_606;
    const PORPOISE: u32 = 42_244;
    const RORQUAL: u32 = 28_352;
    const BURST_II: u32 = 43_551;
    const CHG_FIELD: u32 = 42_829;
    const CHG_OPT: u32 = 42_830;
    const CHG_PRES: u32 = 42_831;
    const CHG_ARMOR_WRONG: u32 = 42_832;
    const MOD_STRIP_II: u32 = 17_912;
    const ICE_HARV_I: u32 = 16_278;
    const TYPE_A_I: u32 = 60_276;

    let (strip_id, ice_id, cryst_id) = (800_600_001_i64, 800_600_002_i64, 800_600_003_i64);
    let miner_skills = [
        (3_386, 5), (3_410, 5), (16_281, 5), (17_940, 5), (60_377, 5),
        (3_402, 5), (3_380, 5), (3_327, 5), (3_429, 5), (3_436, 5),
    ];
    let mut p = pilot(
        17_478,
        &miner_skills,
        &[
            (strip_id, MOD_STRIP_II, InventoryFlag::HiSlot0),
            (ice_id, ICE_HARV_I, InventoryFlag::HiSlot1),
        ],
    )
    .await;
    let kw = HashMap::new();
    let sys = p.ctx.systems.get_or_spawn(JITA);
    let (ore_rock, ice_rock) = (750_600_001_i64, 750_600_002_i64);
    rock(&p, ore_rock, VELDSPAR, 500_000, 3_000.0).await;
    rock(&p, ice_rock, 16_262, 500, 3_000.0).await;
    lock(&mut p, ore_rock).await;
    lock(&mut p, ice_rock).await;

    // Negative: fitting Mining Foreman Burst II (43551) onto a Retriever (17478) via invbroker.Add fails hull restriction
    p.ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(800_600_009, make_item(800_600_009, BURST_II, CHAR_ID, p.ship_id, InventoryFlag::Cargo, 1, true));
    });
    let p_ship = p.ship_id;
    let fit_err = rpc(&mut p, "invbroker", None, "Add", &[PyRep::Integer(800_600_009), PyRep::Integer(p_ship), PyRep::Integer(InventoryFlag::HiSlot2 as i64)]).await;
    assert!(fit_err.is_err(), "Mining Barge (Retriever) must not fit Mining Foreman Burst");

    // Set up second real pilot (90_000_002) in Orca (28606), form fleet via fleetObjectHandler.CreateFleet + Invite
    let booster_cid = 90_000_002_i64;
    let (b_ship, burst_mod, chg_item, bystander_ship) = (9_400_600_001_i64, 9_400_600_002_i64, 9_400_600_003_i64, 9_400_600_099_i64);
    let mut bsess = setup_session(&p.ctx, 903, "other", "other-secret", booster_cid).await;
    p.ctx.db.with_mut_snapshot(|snap| {
        let b_sk = snap.skills.entry(booster_cid).or_default();
        for (sid, lvl) in [(3_348, 5), (22_536, 5), (22_552, 5), (29_637, 5), (28_374, 5)] {
            b_sk.insert(sid, TrainedSkillRecord { type_id: sid, level: lvl, skill_points: 1_000_000.0 });
        }
        let ch = snap.characters.get_mut(&booster_cid).unwrap();
        ch.clone_grade = 2; // Omega clone required for Tech II Command Burst Specialist skill 22552
        ch.solar_system_id = JITA;
        ch.station_id = None;
        ch.active_ship_id = b_ship;
        snap.items.insert(b_ship, make_item(b_ship, ORCA, booster_cid, JITA as i64, InventoryFlag::None, 1, true));
        snap.items.insert(burst_mod, make_item(burst_mod, BURST_II, booster_cid, b_ship, InventoryFlag::HiSlot0, 1, true));
        snap.module_online_states.insert(burst_mod, true);
        // Non-fleet bystander ship in range (must NOT receive fleet bursts)
        snap.items.insert(bystander_ship, make_item(bystander_ship, 17_478, 90_000_099, JITA as i64, InventoryFlag::None, 1, true));
    });
    bsess.solar_system_id = Some(JITA);
    bsess.station_id = None;
    bsess.ship_id = Some(b_ship);
    sys.spawn_ball(Ball {
        id: b_ship, type_id: ORCA, position: p.ship_pos + DVec3::new(2_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO, max_velocity: 100.0, inertia: 1.0, mass: 250_000_000.0, radius: 250.0,
        speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await.unwrap();
    sys.spawn_ball(Ball {
        id: bystander_ship, type_id: 17_478, position: p.ship_pos + DVec3::new(1_500.0, 0.0, 0.0),
        velocity: DVec3::ZERO, max_velocity: 100.0, inertia: 1.0, mass: 20_000_000.0, radius: 100.0,
        speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await.unwrap();

    p.ctx.advance_secs(12);
    p.ctx.dispatch_rpc(&mut bsess, "fleetObjectHandler", None, "CreateFleet", &[], &kw, None).await.expect("CreateFleet");
    p.ctx.dispatch_rpc(&mut bsess, "fleetObjectHandler", None, "Invite", &[PyRep::Integer(CHAR_ID), PyRep::None, PyRep::None, PyRep::None], &kw, None).await.expect("Invite miner");
    p.ctx.dispatch_rpc(&mut p.sess, "fleetObjectHandler", None, "AcceptInvite", &[PyRep::Integer(bsess.fleet_id.unwrap())], &kw, None).await.expect("AcceptInvite");

    // Negative: loading Armor Command Burst charge (42832, group 1774) into Mining Foreman Burst II fails with InvalidChargeGroup
    p.ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(chg_item, make_item(chg_item, CHG_ARMOR_WRONG, booster_cid, b_ship, InventoryFlag::HiSlot0, 5, false));
    });
    p.ctx.invalidate_ship_fitting(b_ship);
    let wrong_chg = p.ctx.dispatch_rpc(&mut bsess, "dogmaIM", None, "Activate", &[PyRep::Integer(burst_mod), PyRep::String("moduleBonusWarfareLinkMining".into())], &kw, None).await.unwrap_err();
    assert_eq!(wrong_chg.code, "InvalidChargeGroup");

    // 1. Orca (28606, +3%/lvl Ind Cmd V = 1.15x strength, 2.0 * 1.25 = 37.5 km range) + Optimization (42830):
    // Consumes 1 charge (5 -> 4) and buffs both Strip Miner AND Ice Harvester duration, excluding non-fleet bystander.
    p.ctx.db.with_mut_snapshot(|snap| snap.items.get_mut(&chg_item).unwrap().type_id = CHG_OPT);
    p.ctx.invalidate_ship_fitting(b_ship);
    let orca_rep = p.ctx.dispatch_rpc(&mut bsess, "dogmaIM", None, "Activate", &[PyRep::Integer(burst_mod), PyRep::String("moduleBonusWarfareLinkMining".into())], &kw, None).await.expect("Activate Orca Optimization burst");
    assert_eq!(p.ctx.db.get_item(chg_item).unwrap().quantity, 4, "One burst charge must be consumed per activation");
    let PyRep::List(aff_ships) = get_dict_val(&orca_rep, "affectedShipIDs").unwrap() else { panic!() };
    assert!(aff_ships.contains(&PyRep::Integer(p.ship_id)));
    assert!(!aff_ships.contains(&PyRep::Integer(bystander_ship)), "Non-fleet ship in AoE must never receive bursts");
    assert!((get_dict_val(&orca_rep, "aoeRangeMeters").and_then(|v| v.as_f64()).unwrap() - 37_500.0).abs() < 1e-3);

    let max_cap = sys.get_combat_state(p.ship_id).await.unwrap().unwrap().max_capacitor;
    sys.set_capacitor(p.ship_id, max_cap).await.unwrap();
    let ice_act = activate(&mut p, ice_id, ice_rock).await.expect("Activate Ice Harvester under Orca Optimization");
    let orca_mult = 1.25 * 1.5 * 1.15; // T2 1.25 * Mining Director V 1.5 * Orca Ind Cmd V 1.15 = 2.15625
    let base_ice_ms = 240_000.0 * 0.75 * 0.90 * 0.875; // Retriever Ice Harvester I = 141,750 ms
    assert_eq!(ice_act.dict_get("cycleTimeMs").and_then(|v| v.as_i64()), Some((base_ice_ms * (1.0 - 0.15 * orca_mult)) as i64));
    rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(ice_id), PyRep::String("miningLaser".into())]).await.unwrap();

    // 2. Porpoise (42244, +2%/lvl = 1.10x strength) + Field Enhancement (42829) & Rorqual (28352, +5%/lvl = 1.25x strength) + Preservation (42831):
    let _ = p.ctx.dispatch_rpc(&mut bsess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(burst_mod)], &kw, None).await;
    p.ctx.advance_virtual_clock(95);
    p.ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&b_ship).unwrap().type_id = PORPOISE;
        snap.items.get_mut(&chg_item).unwrap().type_id = CHG_FIELD;
        snap.module_active_charges.remove(&burst_mod);
    });
    p.ctx.invalidate_ship_fitting(b_ship);
    p.ctx.dispatch_rpc(&mut bsess, "dogmaIM", None, "Activate", &[PyRep::Integer(burst_mod), PyRep::String("moduleBonusWarfareLinkMining".into())], &kw, None).await.unwrap();
    sys.set_capacitor(p.ship_id, max_cap).await.unwrap();
    let porp_act = activate(&mut p, strip_id, ore_rock).await.unwrap();
    let porp_mult = 1.25 * 1.5 * 1.10;
    assert!((porp_act.dict_get("maxRange").and_then(|v| v.as_f64()).unwrap() - 15_000.0 * (1.0 + 0.40 * porp_mult)).abs() < 1e-3);
    rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(strip_id), PyRep::String("miningLaser".into())]).await.unwrap();

    // 3. Rorqual (28352) + Mining Equipment Preservation Charge (42831, dbuff 25: -15% * 1.25 * 1.5 * 1.25 = -35.15625% crystal volatility!):
    // Base Type A I volatility = 0.025 -> under Rorqual Preservation = 0.025 * (1 - 0.3515625) = 0.0162109375.
    // Pick deterministic RNG seed whose 3rd draw (crystal volatility) lies in (0.017, 0.024):
    // -> WITHOUT Preservation (< 0.025) crystal takes 0.05 damage; WITH Preservation (> 0.01621) crystal takes 0.0 damage!
    let _ = p.ctx.dispatch_rpc(&mut bsess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(burst_mod)], &kw, None).await;
    p.ctx.advance_virtual_clock(95);
    let pres_seed = (1u64..)
        .find(|&s| {
            let mut st = s;
            let mut d = || eve_server::rng::u64_to_unit_f64(eve_server::rng::splitmix64_next(&mut st));
            d() >= 0.02 && d() >= 0.40 && (0.018..0.024).contains(&d())
        })
        .unwrap();
    p.ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(cryst_id, make_item(cryst_id, TYPE_A_I, CHAR_ID, p.ship_id, InventoryFlag::HiSlot0, 1, true));
    });
    p.ctx.invalidate_ship_fitting(p.ship_id);
    sys.set_capacitor(p.ship_id, max_cap).await.unwrap();
    activate(&mut p, strip_id, ore_rock).await.unwrap();
    p.ctx.set_mechanic_rng_seed(Some(pres_seed));
    let unbuffed_cyc = advance(&p, 45).await;
    assert_eq!(unbuffed_cyc[0].crystal_damage, 0.05, "Without Preservation burst, roll in (0.018..0.024) damages crystal");
    rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(strip_id), PyRep::String("miningLaser".into())]).await.unwrap();

    p.ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&b_ship).unwrap().type_id = RORQUAL;
        snap.items.get_mut(&chg_item).unwrap().type_id = CHG_PRES;
        snap.module_active_charges.remove(&burst_mod);
    });
    p.ctx.invalidate_ship_fitting(b_ship);
    p.ctx.dispatch_rpc(&mut bsess, "dogmaIM", None, "Activate", &[PyRep::Integer(burst_mod), PyRep::String("moduleBonusWarfareLinkMining".into())], &kw, None).await.expect("Activate Rorqual Preservation burst");
    sys.set_capacitor(p.ship_id, max_cap).await.unwrap();
    activate(&mut p, strip_id, ore_rock).await.unwrap();
    p.ctx.set_mechanic_rng_seed(Some(pres_seed));
    let buffed_cyc = advance(&p, 45).await;
    assert_eq!(buffed_cyc[0].crystal_damage, 0.0, "Rorqual Preservation burst lowers crystal volatility from 2.5% to 1.621% so roll misses");
    rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(strip_id), PyRep::String("miningLaser".into())]).await.unwrap();
}

#[tokio::test]
async fn test_mining_wormhole_sites_phased_fields_and_market_goals() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    use eve_server::services::mining_sites::{PhasedAsteroidFieldState, resolve_wormhole_ore_site, WORMHOLE_ORE_SITES};
    assert_eq!(WORMHOLE_ORE_SITES.len(), 11);
    for def in WORMHOLE_ORE_SITES {
        let ores = resolve_wormhole_ore_site(&ctx.sde, def).expect("W-space site ores must exist in SDE");
        assert!(!ores.is_empty());
        assert!(!def.guards.is_empty());
    }

    let mut field = PhasedAsteroidFieldState::new("SIG-001".to_string(), 30_000_142);
    assert!(!field.beacon_spawned);
    assert_eq!(field.mining_efficiency(), 0.10);
    field.advance_fracture(0.45);
    assert!(field.beacon_spawned);
    assert!(!field.connect_industrial_command_ship(1001, 50));
    assert!(!field.connect_industrial_command_ship(1002, 35));
    assert_eq!(field.mining_efficiency(), 0.10);
    assert!(field.connect_industrial_command_ship(1003, 35));
    assert_eq!(field.mining_efficiency(), 1.0);
    field.advance_fracture(0.50);
    assert!(field.collapsed);
    assert_eq!(field.mining_efficiency(), 0.0);

    for (tid, expected_vol) in [(1_230_u32, 0.10_f64), (34_u32, 0.01_f64), (28_432_u32, 0.15_f64)] {
        let t = ctx.sde.types.get(&tid).unwrap();
        assert!(t.published && (t.volume - expected_vol).abs() < 1e-4);
    }

    let char_id = 90_000_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.mining_ledger.push(MiningLedgerRecord {
            character_id: char_id,
            solar_system_id: 30_000_142,
            type_id: 1_230,
            quantity: 5_000,
            date: "2026-10-20".into(),
            timestamp_unix: 1_729_430_000,
        });
    });
    let ledger_total: i64 = ctx.db.with_snapshot(|snap| {
        snap.mining_ledger.iter().filter(|e| e.character_id == char_id).map(|e| e.quantity).sum()
    });
    assert_eq!(ledger_total, 5_000);
}

}

// Folded submodule: mining_mechanics_suite
#[allow(unused_imports, dead_code)]
mod mining_mechanics_suite {
    use super::*;

// Phase 5M (`MN-01`, `MN-02`, `MN-09`, `MN-17`, `MN-18`) and `OF-28` closure integration tests.

use std::collections::HashMap;

use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, Ballpark, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::living_world::{LivingWorldConvoy, LivingWorldDirector, NpcArchetype};
use eve_server::router::ServerContext;
use eve_server::services::mining_sites::ICE_BELT_SITES;
use eve_server::system_actor::{AsteroidState, populate_asteroid_belt_entities};
use glam::DVec3;

use crate::common::login_pilot;

// ============================================================================
// 1. MN-01: Mining static data in the SDE importer
// ============================================================================
#[tokio::test]
async fn test_mn01_sde_mining_static_data_and_distributions() {
    let ctx = ServerContext::new();
    let sde = &ctx.sde;

    // Veldspar (1230) -> Tritanium (34) only
    let veldspar_mats = sde.type_materials_for(1230);
    assert_eq!(veldspar_mats.len(), 1);
    assert_eq!(veldspar_mats[0].material_type_id, 34);

    // Scordite (1228) -> Tritanium (34) + Pyerite (35)
    let scordite_mats: Vec<u32> = sde.type_materials_for(1228).iter().map(|m| m.material_type_id).collect();
    assert_eq!(scordite_mats, vec![34, 35]);

    // Ice type (Clear Icicle 16262) yields its isotope and fuel materials (16272, 16273, 16274, 16275)
    let ice_mats: Vec<u32> = sde.type_materials_for(16262).iter().map(|m| m.material_type_id).collect();
    for expected in [16272u32, 16273, 16274, 16275] {
        assert!(ice_mats.contains(&expected), "Clear Icicle must yield {expected}");
    }

    // Ore variant yields differ from base ore (Concentrated Veldspar 17470 vs Veldspar 1230)
    let conc_veld_mats = sde.type_materials_for(17470);
    assert_eq!(conc_veld_mats.len(), 1);
    assert!(conc_veld_mats[0].quantity > veldspar_mats[0].quantity);

    // Moon ores (R4-R64: groups 1884, 1920..=1923), Gas (711), and Prismaticite (randomized_type_materials)
    assert!(sde.types.values().any(|t| (1920..=1923).contains(&t.group_id) && t.published));
    assert!(sde.types.values().any(|t| t.group_id == 711 && t.published));
    assert!(!sde.randomized_type_materials.is_empty(), "Prismaticite randomized materials must be loaded");

    // Compressed counterparts & portionSize (V24 Batch Compressed Veldspar = 62516)
    assert_eq!(sde.compressible_types.get(&1230).copied(), Some(62516));
    assert_eq!(sde.get_type(1230).unwrap().portion_size, 100);

    // Regional ice belts & belt celestials with positions (Perimeter 30000144)
    assert!(!ICE_BELT_SITES.is_empty());
    let perim_belts: Vec<_> = sde.celestials_in_system(30_000_144).into_iter().filter(|c| c.group_id == 9).collect();
    assert!(!perim_belts.is_empty());
    assert!(perim_belts.iter().all(|b| b.position.length() > 0.0));

    // Mining modules (Miner I 483), crystals, drones (Mining Drone I 10246), MLU I (22542), Venture (32880)
    let miner_i = sde.get_type(483).unwrap();
    assert!(miner_i.default_attributes.contains_key(&77)); // miningAmount
    assert!(miner_i.default_attributes.contains_key(&73)); // duration
    assert!(miner_i.default_attributes.contains_key(&54)); // maxRange
    let venture = sde.get_type(32880).unwrap();
    assert!(venture.default_attributes.contains_key(&1556)); // specialOreHoldCapacity
}

// ============================================================================
// 2. MN-02: Asteroid belts as balls & security/region ore distribution
// ============================================================================
#[tokio::test]
async fn test_mn02_asteroid_belts_as_balls_highsec_and_nullsec() {
    // parity: plan:MN-02
    eve_testkit::attest(&["plan:MN-02"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();

    // 1. Live high-sec system (Perimeter 30000144, Caldari 1.0) & null-sec system (30000208 LZ-6SU)
    //    spawn real asteroid belt balls and asteroids with authentic security/region ore sets.
    let mut pilot = crate::common::gm_pilot_in_space(&ctx, 9102, 95_091_002, 587, 30_000_144).await;
    let perim_handle = ctx.systems.get_or_spawn(30_000_144);
    let _ = perim_handle.populate_asteroid_belt_entities().await;
    let perim_belts: Vec<_> = ctx
        .sde
        .celestials_in_system(30_000_144)
        .into_iter()
        .filter(|c| c.group_id == 9)
        .collect();
    assert!(!perim_belts.is_empty(), "Perimeter must have SDE asteroid belts");
    let target_belt = &perim_belts[0];

    let (_, belt_ball) = perim_handle
        .get_ball_snapshot(target_belt.celestial_id)
        .await
        .unwrap()
        .expect("Perimeter belt ball must exist in live ballpark");
    assert_eq!(belt_ball.mode, MovementMode::Rigid);

    let perim_expected: std::collections::BTreeSet<u32> = ctx
        .sde
        .belt_ores_for_system(30_000_144)
        .into_iter()
        .map(|t| t.type_id)
        .collect();
    assert_eq!(perim_expected, std::collections::BTreeSet::from([1230, 1228]));

    let mut perim_spawned = std::collections::BTreeSet::new();
    for offset in 0..perim_expected.len() as i64 {
        let aid = 750_000_000i64 + offset;
        let (_, ast_ball) = perim_handle
            .get_ball_snapshot(aid)
            .await
            .unwrap()
            .expect("Perimeter asteroid ball in ballpark");
        assert_eq!(ast_ball.mode, MovementMode::Stop);
        assert!(ast_ball.radius >= 60.0);
        perim_spawned.insert(ast_ball.type_id);
    }
    assert_eq!(perim_spawned, perim_expected);

    let null_handle = ctx.systems.get_or_spawn(30_000_208);
    let _ = null_handle.populate_asteroid_belt_entities().await;
    let null_expected: std::collections::BTreeSet<u32> = ctx
        .sde
        .belt_ores_for_system(30_000_208)
        .into_iter()
        .map(|t| t.type_id)
        .collect();
    assert!(!null_expected.contains(&1230) && !null_expected.contains(&1228));
    assert!(null_expected.contains(&1229) && null_expected.contains(&11396));
    let mut null_spawned = std::collections::BTreeSet::new();
    for offset in 0..null_expected.len() as i64 {
        let aid = 750_208_000i64 + offset;
        let (_, ast_ball) = null_handle
            .get_ball_snapshot(aid)
            .await
            .unwrap()
            .expect("Nullsec asteroid ball in ballpark");
        assert!(ast_ball.radius >= 60.0);
        null_spawned.insert(ast_ball.type_id);
    }
    assert_eq!(null_spawned, null_expected);

    // 2. Warp live pilot to Perimeter belt via beyonce.CmdWarpToStuff and step server ticks
    let _ = ctx.drain_notifications_for_char(pilot.char_id);
    ctx.dispatch_rpc(
        &mut pilot.sess,
        "beyonce",
        Some(pilot.beyonce),
        "CmdWarpToStuff",
        &[
            PyRep::String("item".into()),
            PyRep::Integer(target_belt.celestial_id),
            PyRep::Real(0.0),
        ],
        &kw,
        None,
    )
    .await
    .expect("CmdWarpToStuff to Perimeter belt");
    for _ in 0..40 {
        let _ = perim_handle.tick().await;
        if let Ok(Some((_, b))) = perim_handle.get_ball_snapshot(pilot.ship_id).await {
            if b.position.distance(target_belt.position) < 150_000.0
                && !matches!(b.mode, MovementMode::Warp { .. })
            {
                break;
            }
        }
    }
    let (_, ship_after) = perim_handle.get_ball_snapshot(pilot.ship_id).await.unwrap().unwrap();
    assert!(ship_after.position.distance(target_belt.position) < 150_000.0);
    let notifs = ctx.drain_notifications_for_char(pilot.char_id);
    assert!(!notifs.iter().any(|n| n.event == "OnWarpToAsteroidBelt"));
}

// ============================================================================
// 3. OF-28: OnOreMined notification & characterMiningLedger RPC
// ============================================================================
#[tokio::test]
async fn test_of28_mining_laser_fired_local_and_canonical_ledger_rpc() {
    // parity: notify:OnOreMined, rpc:characterMiningLedger.GetCharacterLogs
    eve_testkit::attest(&["notify:OnOreMined", "rpc:characterMiningLedger.GetCharacterLogs"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001i64;
    let mut sess = SessionState::new(9109);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let kw = HashMap::new();
    let _ = ctx
        .dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None)
        .await
        .unwrap();

    // Non-catalog `charMiningLedger` service must not be dispatched
    let legacy_err = ctx
        .dispatch_rpc(&mut sess, "charMiningLedger", None, "GetMiningLedger", &[], &kw, None)
        .await;
    assert!(legacy_err.is_err(), "Non-catalog charMiningLedger must be rejected");

    let laser_id = ctx.db.with_mut_snapshot(|snap| {
        let active_ship = snap.characters.get(&char_id).unwrap().active_ship_id;
        snap.items.get_mut(&active_ship).unwrap().type_id = 32880; // Venture
        snap.items.retain(|_, i| i.location_id != active_ship);
        snap.skills.entry(char_id).or_default().insert(
            3386,
            eve_db::TrainedSkillRecord {
                type_id: 3386, // Mining
                level: 5,
                skill_points: 256_000.0,
            },
        );
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(
            id,
            InventoryItemRecord {
                item_id: id,
                type_id: 483, // Miner I
                owner_id: char_id,
                location_id: active_ship,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        id
    });

    let _ = ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None).await.unwrap();
    let ship_id = sess.ship_id.unwrap();
    ctx.invalidate_ship_fitting(ship_id);
    let jita = ctx.systems.get_or_spawn(30_000_142);
    let (_, ship_ball) = jita.get_ball_snapshot(ship_id).await.unwrap().unwrap();

    let ast_id = 750_999_001i64;
    let ast_pos = ship_ball.position + DVec3::new(2_000.0, 0.0, 0.0);
    ctx.db.spawn_asteroid(eve_db::AsteroidRecord {
        asteroid_id: ast_id,
        solar_system_id: 30_000_142,
        belt_id: 40_000_001,
        type_id: 1230,
        quantity: 10_000,
        initial_quantity: 10_000,
        position: (ast_pos.x, ast_pos.y, ast_pos.z),
        radius: 100.0,
    });
    jita.spawn_asteroid(
        AsteroidState {
            asteroid_id: ast_id,
            belt_id: 40_000_001,
            type_id: 1230,
            initial_units: 10_000,
            remaining_units: 10_000,
            unit_volume: 0.1,
            depleted: false,
            respawn_at_tick: None,
        },
        Ball {
            id: ast_id,
            type_id: 1230,
            position: ast_pos,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 0.0,
            mass: 1.0e8,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
    )
    .await
    .unwrap();

    let _ = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "AddTarget", &[PyRep::Integer(ast_id)], &kw, None).await.unwrap();
    for _ in 0..10 {
        jita.step_manual(1.0).await.unwrap();
    }
    let _ = ctx.drain_notifications_for_char(char_id);
    let _ = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(laser_id), PyRep::String("miningLaser".into()), PyRep::Integer(ast_id)],
            &kw,
            None,
        )
        .await
        .unwrap();
    ctx.advance_secs(65);
    let _ = eve_server::services::mining_cycle::process_mining_cycles(&ctx).await;

    let notifs = ctx.drain_notifications_for_char(char_id);
    assert!(notifs.iter().any(|n| n.event == "OnOreMined"), "OnOreMined must fire");

    let logs = ctx.dispatch_rpc(&mut sess, "characterMiningLedger", None, "GetCharacterLogs", &[], &kw, None).await.unwrap();
    let PyRep::List(rows) = logs else { panic!("Expected List from GetCharacterLogs") };
    assert!(!rows.is_empty());
    let kv_i64 = |rep: &PyRep, key: &str| -> Option<i64> {
        match rep {
            PyRep::KeyVal(m) => m.get(key).and_then(|v| v.as_i64()),
            PyRep::Dict(entries) => entries
                .iter()
                .find(|(k, _)| k.as_str() == Some(key))
                .and_then(|(_, v)| v.as_i64()),
            _ => None,
        }
    };
    let entry = rows
        .iter()
        .find(|r| kv_i64(r, "typeID") == Some(1230))
        .expect("GetCharacterLogs must contain Veldspar (1230) entry");
    assert_eq!(kv_i64(entry, "solarSystemID"), Some(30_000_142));
    assert!(kv_i64(entry, "quantity").unwrap_or(0) > 0, "GetCharacterLogs quantity must be > 0");
}

// ============================================================================
// 4. MN-09: Mining drones
// ============================================================================
#[tokio::test]
async fn test_mn09_mining_drones_ore_hold_delivery_and_range_check() {
    // parity: plan:MN-09, feat:drones.mining-drones, rpc:entity.CmdMineRepeatedly
    eve_testkit::attest(&["plan:MN-09", "feat:drones.mining-drones", "rpc:entity.CmdMineRepeatedly"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001i64;
    let mut sess = SessionState::new(9109);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let kw = HashMap::new();
    let _ = ctx
        .dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None)
        .await
        .unwrap();

    let (ship_id, drone_a, drone_b) = ctx.db.with_mut_snapshot(|snap| {
        let active_ship = snap.characters.get(&char_id).unwrap().active_ship_id;
        snap.items.get_mut(&active_ship).unwrap().type_id = 32880; // Venture (5,000 m3 SpecializedOreHold, 10 mbit drone bandwidth)
        snap.items.retain(|_, i| !(i.location_id == active_ship && (i.flag == InventoryFlag::SpecializedOreHold || i.flag == InventoryFlag::DroneBay)));
        snap.next_entity_id += 1;
        let prefill_id = snap.next_entity_id;
        snap.items.insert(
            prefill_id,
            InventoryItemRecord {
                item_id: prefill_id,
                type_id: 1230, // Veldspar (0.1 m3 -> 49,500 units = 4,950 m3, leaving 50 m3 = 500 units)
                owner_id: char_id,
                location_id: active_ship,
                flag: InventoryFlag::SpecializedOreHold,
                quantity: 49_500,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.next_entity_id += 1;
        let da = snap.next_entity_id;
        snap.items.insert(
            da,
            InventoryItemRecord {
                item_id: da,
                type_id: 10246, // Mining Drone I
                owner_id: char_id,
                location_id: active_ship,
                flag: InventoryFlag::DroneBay,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.next_entity_id += 1;
        let db_id = snap.next_entity_id;
        snap.items.insert(
            db_id,
            InventoryItemRecord {
                item_id: db_id,
                type_id: 10246, // Mining Drone I
                owner_id: char_id,
                location_id: active_ship,
                flag: InventoryFlag::DroneBay,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        (active_ship, da, db_id)
    });

    let _ = ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None).await.unwrap();
    ctx.invalidate_ship_fitting(ship_id);
    let jita = ctx.systems.get_or_spawn(30_000_142);
    let (_, ship_ball) = jita.get_ball_snapshot(ship_id).await.unwrap().unwrap();

    // Launch 2 Mining Drone Is from the ship's drone bay
    let drone_pair = PyRep::List(vec![PyRep::Integer(drone_a), PyRep::Integer(drone_b)]);
    ctx.dispatch_rpc(&mut sess, "ship", None, "LaunchDrones", std::slice::from_ref(&drone_pair), &kw, None)
        .await
        .expect("ship.LaunchDrones 2 Mining Drone Is");

    // Out-of-range asteroid at 25 km (> 20 km default drone control range, < 35 km legacy limit) must be rejected
    let far_ast_id = 750_999_002i64;
    jita.spawn_asteroid(
        AsteroidState {
            asteroid_id: far_ast_id,
            belt_id: 40_000_001,
            type_id: 1230,
            initial_units: 5_000,
            remaining_units: 5_000,
            unit_volume: 0.1,
            depleted: false,
            respawn_at_tick: None,
        },
        Ball {
            id: far_ast_id,
            type_id: 1230,
            position: ship_ball.position + DVec3::new(25_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 0.0,
            mass: 1.0e8,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
    )
    .await
    .unwrap();
    let far_err = ctx
        .dispatch_rpc(
            &mut sess,
            "entity",
            None,
            "CmdMineRepeatedly",
            &[drone_pair.clone(), PyRep::Integer(far_ast_id)],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        far_err.expect_err("Out-of-range asteroid must be rejected for mining drones").code,
        "OutOfRange"
    );

    // Spawn in-range asteroid and run entity.CmdMineRepeatedly
    let near_ast_id = 750_999_003i64;
    jita.spawn_asteroid(
        AsteroidState {
            asteroid_id: near_ast_id,
            belt_id: 40_000_001,
            type_id: 1230,
            initial_units: 10_000,
            remaining_units: 10_000,
            unit_volume: 0.1,
            depleted: false,
            respawn_at_tick: None,
        },
        Ball {
            id: near_ast_id,
            type_id: 1230,
            position: ship_ball.position + DVec3::new(2_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 0.0,
            mass: 1.0e8,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
    )
    .await
    .unwrap();

    ctx.dispatch_rpc(
        &mut sess,
        "entity",
        None,
        "CmdMineRepeatedly",
        &[drone_pair, PyRep::Integer(near_ast_id)],
        &kw,
        None,
    )
    .await
    .expect("entity.CmdMineRepeatedly 2 Mining Drone Is");

    let c1_dur = eve_server::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
        assert_eq!(rt.active_drone_miners.len(), 2, "Both Mining Drone Is must be actively mining");
        rt.active_drone_miners.values().next().unwrap().duration_secs
    });

    // Cycle 1: 2 Mining Drone Is deposit Veldspar into SpecializedOreHold
    ctx.advance_secs(c1_dur);
    eve_server::sp6_lifecycle::step_repeating_drone_mining(&ctx, ctx.now_unix()).await;
    let hold_after_c1: i32 = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| i.location_id == ship_id && i.flag == InventoryFlag::SpecializedOreHold && i.type_id == 1230)
            .map(|i| i.quantity)
            .sum()
    });
    assert!(hold_after_c1 > 49_500, "Ore hold quantity ({hold_after_c1}) must increase after drone mining cycle");

    // Cycle 2: Ore hold is now full for another full drone cycle -> drones stop mining
    ctx.advance_secs(c1_dur);
    eve_server::sp6_lifecycle::step_repeating_drone_mining(&ctx, ctx.now_unix()).await;
    let active_after_full = eve_server::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
        rt.active_drone_miners.len()
    });
    assert_eq!(active_after_full, 0, "Mining drones must stop mining when ore hold is full");
}

// ============================================================================
// 5. DERIVED NPC mining op hauler replenishment
// ============================================================================
#[tokio::test]
async fn test_npc_mining_op_derived_hauler_replenishment() {
    // parity: plan:MN-17
    let ctx = ServerContext::new();
    let director = LivingWorldDirector::new();

    let convoy_id = 800_000_199i64;
    let system_id = 30_000_144u32;
    let dest_station_id = 60_003_766i64;
    let ore_type_id = 1230u32;

    let handle = ctx.systems.get_or_spawn(system_id);
    let ast_id = 750_000_271i64;
    handle
        .spawn_asteroid(
            AsteroidState {
                asteroid_id: ast_id,
                belt_id: 40_000_001,
                type_id: ore_type_id,
                initial_units: 60_000,
                remaining_units: 60_000,
                unit_volume: 0.1,
                depleted: false,
                respawn_at_tick: None,
            },
            Ball {
                id: ast_id,
                type_id: ore_type_id,
                position: DVec3::new(5_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 0.0,
                mass: 1.0e8,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
        )
        .await
        .unwrap();

    director.register_convoy(LivingWorldConvoy {
        convoy_id,
        archetype: NpcArchetype::MiningOp,
        ship_type_id: 32880,
        origin_system_id: system_id,
        current_system_id: system_id,
        destination_system_id: system_id,
        destination_station_id: dest_station_id,
        cargo_type_id: ore_type_id,
        cargo_quantity: 48_800,
        lod_level: 1,
        destroyed: false,
    });

    let vol_before: i32 = ctx.db.with_snapshot(|snap| {
        snap.market_orders
            .values()
            .filter(|o| !o.is_bid && o.is_npc && o.station_id == dest_station_id && o.type_id == ore_type_id)
            .map(|o| o.vol_remaining)
            .sum()
    });
    let _ = director.step_npc_mining_op(convoy_id, &ctx.systems, &ctx.db, &ctx.sde).await.unwrap();
    ctx.advance_secs(60);
    let step = director.step_npc_mining_op(convoy_id, &ctx.systems, &ctx.db, &ctx.sde).await.unwrap();
    assert!(step.mined_units > 0, "NPC mining op must deplete asteroid quantity over ticks");
    assert!(step.remaining_asteroid_units < 60_000);
    assert!(!step.hauled_to_station);
    ctx.advance_secs(60);
    let deliver_step = director.step_npc_mining_op(convoy_id, &ctx.systems, &ctx.db, &ctx.sde).await.unwrap();
    assert!(deliver_step.hauled_to_station);
    let vol_after: i32 = ctx.db.with_snapshot(|snap| {
        snap.market_orders
            .values()
            .filter(|o| !o.is_bid && o.is_npc && o.station_id == dest_station_id && o.type_id == ore_type_id)
            .map(|o| o.vol_remaining)
            .sum()
    });
    assert!(
        vol_after > vol_before,
        "NPC hauler delivery must increase NPC sell-order vol_remaining ({vol_before} -> {vol_after})"
    );
}

}


#[allow(unused_imports, dead_code, unused_variables, unused_mut)]
mod space_mining_sites_ext {
    use super::*;

// PVE Agent Finder, Conversation, Mission Accept/Decline/Quit Integration Tests (Part 2).


use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

async fn create_auth_session(ctx: &ServerContext, session_id: u64) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(session_id);
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
    .expect("Login failed");
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
    .expect("SelectCharacterID failed");
    sess.solar_system_id = Some(30_000_142);
    sess.station_id = Some(60_003_760);
    sess.region_id = Some(10_000_002);
    sess
}

async fn bind_service_oid(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    bind_param: PyRep,
) -> u64 {
    let kw = HashMap::new();
    let res = ctx
        .dispatch_rpc(
            sess,
            service,
            None,
            "MachoBindObject",
            &[bind_param, PyRep::None],
            &kw,
            None,
        )
        .await
        .expect("MachoBindObject failed");
    let PyRep::Tuple(outer) = res else {
        panic!("Expected Tuple from MachoBindObject");
    };
    outer[0].as_i64().expect("bound_id integer") as u64
}

// ============================================================================
// E-5: PVE Agent Finder, Conversation, Mission Accept/Decline/Quit
// ============================================================================
#[tokio::test]
async fn test_e5_agent_finder_and_conversation_lifecycle() {
    // parity: feat:pve.agent-finder, feat:pve.agent-conversation, feat:pve.mission-accept, feat:pve.mission-decline, feat:pve.mission-quit, rpc:agentMgr.GetAgents, rpc:agentMgr.GetMyJournalDetails, rpc:agentMgr.GetSolarSystemOfAgent
    eve_testkit::attest(&["feat:pve.agent-finder", "feat:pve.agent-conversation", "feat:pve.mission-accept", "feat:pve.mission-decline", "feat:pve.mission-quit"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = create_auth_session(&ctx, 1002).await;
    let kw = HashMap::new();
    let char_id = 90_000_001_i64;

    // 1. Agent Finder (feat:pve.agent-finder)
    let agents_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            None,
            "GetAgents",
            &[PyRep::Integer(1), PyRep::Integer(24)],
            &kw,
            None,
        )
        .await
        .expect("agentMgr.GetAgents");

    let PyRep::Rowset {
        columns: agents_cols,
        rows: agents_rows,
    } = agents_rep
    else {
        panic!("Expected Rowset from GetAgents");
    };
    assert_eq!(
        agents_cols,
        vec![
            "agentID",
            "agentTypeID",
            "divisionID",
            "corporationID",
            "factionID",
            "solarsystemID",
            "stationID",
            "level",
            "quality",
            "hasAccess",
            "isLocatorAgent",
        ]
    );
    assert!(!agents_rows.is_empty(), "Level 1 Security agents must be found");

    for row in &agents_rows {
        assert_eq!(row[7].as_i64(), Some(1), "Level must be 1");
        assert_eq!(row[2].as_i64(), Some(24), "Division must be 24");
        assert_eq!(row[9], PyRep::Bool(true), "Level 1 agents must have hasAccess = true");
    }

    let l2_agents_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            None,
            "GetAgents",
            &[PyRep::Integer(2), PyRep::Integer(24)],
            &kw,
            None,
        )
        .await
        .expect("agentMgr.GetAgents L2");

    let PyRep::Rowset { rows: l2_rows, .. } = l2_agents_rep else {
        panic!("Expected Rowset");
    };
    assert!(!l2_rows.is_empty(), "Level 2 Security agents must be found");
    let test_l2_agent_id = l2_rows[0][0].as_i64().unwrap();
    let test_l2_corp_id = l2_rows[0][3].as_i64().unwrap();
    assert_eq!(l2_rows[0][9], PyRep::Bool(false), "L2 agent requires standing >= 1.0");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(test_l2_corp_id, 3.0);
        }
    });

    let l2_accessible_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            None,
            "GetAgents",
            &[PyRep::Integer(2), PyRep::Integer(24)],
            &kw,
            None,
        )
        .await
        .expect("agentMgr.GetAgents L2 with standing");

    let PyRep::Rowset { rows: l2_acc_rows, .. } = l2_accessible_rep else {
        panic!("Expected Rowset");
    };
    let accessible_row = l2_acc_rows
        .iter()
        .find(|r| r[0].as_i64() == Some(test_l2_agent_id))
        .unwrap();
    assert_eq!(accessible_row[9], PyRep::Bool(true));

    // 2. Agent Conversation via V24 bound agentMgr moniker (feat:pve.agent-conversation)
    let l1_agent = agents_rows
        .iter()
        .find(|r| r[5].as_i64() == Some(30_000_142))
        .unwrap_or(&agents_rows[0]);
    let l1_agent_id = l1_agent[0].as_i64().unwrap();
    let l1_agent_sys = l1_agent[5].as_i64().unwrap() as u32;
    let l1_agent_sta = l1_agent[6].as_i64();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = l1_agent_sys;
            ch.station_id = l1_agent_sta;
        }
    });
    sess.solar_system_id = Some(l1_agent_sys);
    sess.station_id = l1_agent_sta;

    let agent_oid = bind_service_oid(&ctx, &mut sess, "agentMgr", PyRep::Integer(l1_agent_id)).await;

    let info_res = ctx
        .dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "GetInfoServiceDetails", &[], &kw, None)
        .await
        .expect("agentMgr.GetInfoServiceDetails");
    assert_eq!(info_res.dict_get("agentID").and_then(|v| v.as_i64()), Some(l1_agent_id));

    let agent_sys_res = ctx
        .dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "GetSolarSystemOfAgent", &[], &kw, None)
        .await
        .expect("agentMgr.GetSolarSystemOfAgent");
    assert_eq!(agent_sys_res.as_i64(), Some(l1_agent_sys as i64));

    let conv_res = ctx
        .dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(2)], &kw, None)
        .await
        .expect("agentMgr.DoAction(RequestMission)");

    let PyRep::KeyVal(conv_kv) = conv_res else {
        panic!("Expected KeyVal from DoAction(RequestMission)");
    };
    let mission_id = conv_kv["missionID"].as_i64().expect("missionID") as u32;
    assert_eq!(conv_kv["agentID"].as_i64(), Some(l1_agent_id));
    assert_eq!(conv_kv["state"].as_str(), Some("Offered"));
    assert_eq!(conv_kv["missionState"].as_i64(), Some(1));
    assert!(conv_kv["briefing"].as_str().is_some());
    assert!(conv_kv["objectives"].as_str().is_some());
    assert!(conv_kv["iskReward"].as_i64().unwrap() > 0);
    assert!(conv_kv["lpReward"].as_i64().unwrap() > 0);
    assert!(conv_kv["expiryUnix"].as_i64().unwrap() > 0);

    // 3. Mission Accept via DoAction(3) (feat:pve.mission-accept)
    let accept_res = ctx
        .dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(3)], &kw, None)
        .await
        .expect("agentMgr.DoAction(Accept)");

    let PyRep::KeyVal(acc_kv) = accept_res else {
        panic!("Expected KeyVal from DoAction(Accept)");
    };
    assert_eq!(acc_kv["missionID"].as_i64(), Some(mission_id as i64));
    assert_eq!(acc_kv["state"].as_str(), Some("Accepted"));
    assert_eq!(acc_kv["missionState"].as_i64(), Some(2));
    assert!(acc_kv["bookmarkID"].as_i64().is_some());
    assert!(acc_kv["expiryUnix"].as_i64().is_some());
    assert!(acc_kv["bonusExpiryUnix"].as_i64().is_some());

    let bm_id = acc_kv["bookmarkID"].as_i64().unwrap();
    assert!(ctx.db.with_snapshot(|snap| snap.bookmarks.contains_key(&bm_id)));

    let journal_res = ctx
        .dispatch_rpc(&mut sess, "agentMgr", None, "GetMyJournalDetails", &[], &kw, None)
        .await
        .expect("agentMgr.GetMyJournalDetails");
    let PyRep::Tuple(j_tuple) = journal_res else {
        panic!("Expected Tuple from GetMyJournalDetails");
    };
    let PyRep::List(j_missions) = &j_tuple[0] else {
        panic!("Expected missions List");
    };
    assert!(
        j_missions.iter().any(|m| m.dict_get("missionID").and_then(|v| v.as_i64()) == Some(mission_id as i64)
            && m.dict_get("missionState").and_then(|v| v.as_i64()) == Some(2))
    );

    let briefing_info = ctx
        .dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "GetMissionBriefingInfo", &[], &kw, None)
        .await
        .expect("agentMgr.GetMissionBriefingInfo");
    assert_eq!(briefing_info.dict_get("missionID").and_then(|v| v.as_i64()), Some(mission_id as i64));

    // 4. Mission Quit via DoAction(11) (feat:pve.mission-quit)
    let (initial_agent_st, initial_corp_st) = ctx.db.with_snapshot(|snap| {
        let ch = &snap.characters[&char_id];
        (
            ch.standings.get(&l1_agent_id).copied().unwrap_or(0.0),
            ch.standings.get(&l1_agent[3].as_i64().unwrap()).copied().unwrap_or(0.0),
        )
    });

    let quit_res = ctx
        .dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(11)], &kw, None)
        .await
        .expect("agentMgr.DoAction(Quit)");

    let PyRep::KeyVal(quit_kv) = quit_res else {
        panic!("Expected KeyVal from DoAction(Quit)");
    };
    assert_eq!(quit_kv["state"].as_str(), Some("Quit"));
    assert_eq!(quit_kv["missionState"].as_i64(), Some(6));
    assert_eq!(quit_kv["dungeonCleaned"], PyRep::Bool(true));
    assert_eq!(quit_kv["standingPenaltyApplied"], PyRep::Bool(true));

    let (post_quit_agent_st, post_quit_corp_st) = ctx.db.with_snapshot(|snap| {
        let ch = &snap.characters[&char_id];
        (
            ch.standings.get(&l1_agent_id).copied().unwrap_or(0.0),
            ch.standings.get(&l1_agent[3].as_i64().unwrap()).copied().unwrap_or(0.0),
        )
    });
    assert!((post_quit_agent_st - (initial_agent_st - 0.1)).abs() < 1e-4);
    assert!((post_quit_corp_st - (initial_corp_st - 0.1)).abs() < 1e-4);
    assert!(!ctx.db.with_snapshot(|snap| snap.bookmarks.contains_key(&bm_id)));

    let journal_after_quit = ctx
        .dispatch_rpc(&mut sess, "agentMgr", None, "GetMyJournalDetails", &[], &kw, None)
        .await
        .expect("GetMyJournalDetails after quit");
    let PyRep::Tuple(jq_sections) = journal_after_quit else { panic!() };
    let PyRep::List(jq_missions) = &jq_sections[0] else { panic!() };
    assert!(jq_missions.is_empty());

    // 5. Mission Decline via DoAction(9) (feat:pve.mission-decline)
    let offer2_res = ctx
        .dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(2)], &kw, None)
        .await
        .expect("agentMgr.DoAction(RequestMission) 2");
    let PyRep::KeyVal(offer2_kv) = offer2_res else { panic!() };
    let _mid2 = offer2_kv["missionID"].as_i64().unwrap() as u32;

    let decline1_res = ctx
        .dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(9)], &kw, None)
        .await
        .expect("agentMgr.DoAction(Decline) 1");
    let PyRep::KeyVal(dec1_kv) = decline1_res else { panic!() };
    assert_eq!(dec1_kv["state"].as_str(), Some("Declined"));
    assert_eq!(dec1_kv["missionState"].as_i64(), Some(5));
    assert_eq!(dec1_kv["standingPenaltyApplied"], PyRep::Bool(false));
    let st_after_dec1 = ctx.db.with_snapshot(|snap| {
        snap.characters[&char_id].standings.get(&l1_agent[3].as_i64().unwrap()).copied().unwrap_or(0.0)
    });
    assert!((st_after_dec1 - post_quit_corp_st).abs() < 1e-6);

    let offer3_res = ctx
        .dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(2)], &kw, None)
        .await
        .expect("agentMgr.DoAction(RequestMission) 3");
    let PyRep::KeyVal(offer3_kv) = offer3_res else { panic!() };
    let _mid3 = offer3_kv["missionID"].as_i64().unwrap() as u32;

    let decline2_res = ctx
        .dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(9)], &kw, None)
        .await
        .expect("agentMgr.DoAction(Decline) 2");
    let PyRep::KeyVal(dec2_kv) = decline2_res else { panic!() };
    assert_eq!(dec2_kv["state"].as_str(), Some("Declined"));
    assert_eq!(dec2_kv["standingPenaltyApplied"], PyRep::Bool(true));
    let st_after_dec2 = ctx.db.with_snapshot(|snap| {
        snap.characters[&char_id].standings.get(&l1_agent[3].as_i64().unwrap()).copied().unwrap_or(0.0)
    });
    assert!(st_after_dec2 < st_after_dec1);
}

}


pub(crate) mod npc_mining_operations_suite {
    #[allow(unused_imports)]
    use super::*;

// E2E parity integration tests for NPC & Mining features.


use crate::features_market_orders_rpcs::{rpc, select_pilot, PILOT_1, PILOT_2};
use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::living_world::{sde_ball_and_combat, LivingWorldConvoy, NpcArchetype};
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{
    is_outlaw_ship_docking_refused, step_empire_navy_patrols, step_extended_npc_ai_sync,
    with_combat_runtime,
};
use eve_server::sp6_lifecycle::tick_background_cluster;
use eve_server::system_actor::AsteroidState;
use glam::DVec3;
use std::collections::HashSet;

const JITA_SYS: u32 = 30_000_142;
const PERIMETER_SYS: u32 = 30_000_144;

async fn spawn_pilot_ship_in_space(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    solar_system_id: u32,
    pos: DVec3,
) {
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.station_id = None;
            c.structure_id = None;
            c.solar_system_id = solar_system_id;
            c.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_type_id,
                owner_id: char_id,
                location_id: solar_system_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    sess.solar_system_id = Some(solar_system_id);
    sess.station_id = None;
    sess.ship_id = Some(ship_id);

    let handle = ctx.systems.get_or_spawn(solar_system_id);
    let (ball, combat) = sde_ball_and_combat(&ctx.sde, ship_id, ship_type_id, pos);
    handle
        .spawn_ball_with_combat(ball, combat)
        .await
        .expect("spawn pilot ship ball");
}

#[tokio::test]
async fn test_parity_navy_npc_and_faction_police() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let outlaw_ship = 850_001i64;
    let mut s_outlaw = select_pilot(&ctx, 50_001, "demo", "demo-secret", PILOT_1).await;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&PILOT_1) {
            c.security_status = -6.0;
        }
    });
    spawn_pilot_ship_in_space(
        &ctx,
        &mut s_outlaw,
        PILOT_1,
        outlaw_ship,
        624,
        JITA_SYS,
        DVec3::new(10_000.0, 0.0, 0.0),
    )
    .await;

    let pod_ship = 850_002i64;
    let mut s_pod = select_pilot(&ctx, 50_002, "other", "other-secret", PILOT_2).await;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&PILOT_2) {
            c.security_status = -6.0;
        }
    });
    spawn_pilot_ship_in_space(
        &ctx,
        &mut s_pod,
        PILOT_2,
        pod_ship,
        670,
        JITA_SYS,
        DVec3::new(20_000.0, 0.0, 0.0),
    )
    .await;

    assert!(
        is_outlaw_ship_docking_refused(&ctx, JITA_SYS, PILOT_1, outlaw_ship),
        "Outlaw ship in highsec must be refused docking"
    );
    assert!(
        !is_outlaw_ship_docking_refused(&ctx, JITA_SYS, PILOT_2, pod_ship),
        "Outlaw capsule in highsec must be allowed to dock"
    );

    ctx.advance_secs(5);
    tick_background_cluster(&ctx).await;
    let _ = step_empire_navy_patrols(&ctx, JITA_SYS).await;
    step_extended_npc_ai_sync(&ctx, JITA_SYS);

    let (police_targets, navy_targets) = with_combat_runtime(&ctx, |rt| {
        let pol: Vec<Option<i64>> = rt
            .npc_entities
            .values()
            .filter(|n| n.solar_system_id == JITA_SYS && n.archetype == "FactionPolice")
            .map(|n| n.target_id)
            .collect();
        let nav: Vec<Option<i64>> = rt
            .npc_entities
            .values()
            .filter(|n| n.solar_system_id == JITA_SYS && n.archetype == "Navy")
            .map(|n| n.target_id)
            .collect();
        (pol, nav)
    });

    assert!(
        police_targets.contains(&Some(outlaw_ship)),
        "Faction Police must spawn and target outlaw ship"
    );
    assert!(
        !police_targets.contains(&Some(pod_ship)),
        "Faction Police must never target capsules"
    );
    assert!(
        !navy_targets.is_empty(),
        "Empire Navy patrol fleet must spawn in empire-owned system"
    );
    assert!(
        navy_targets.contains(&Some(outlaw_ship)),
        "Empire Navy patrol must engage outlaw ship"
    );
    assert!(
        !navy_targets.contains(&Some(pod_ship)),
        "Empire Navy patrol must never engage capsules"
    );
}

#[tokio::test]
async fn test_parity_mining_op_npc_and_hauler_convoy() {
    // parity: feat:npc.npc-hauler
    eve_testkit::attest(&["feat:npc.npc-hauler"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let ship_id = 850_010i64;
    let mut sess = select_pilot(&ctx, 50_010, "demo", "demo-secret", PILOT_1).await;
    spawn_pilot_ship_in_space(
        &ctx,
        &mut sess,
        PILOT_1,
        ship_id,
        624,
        PERIMETER_SYS,
        DVec3::ZERO,
    )
    .await;

    let perim_handle = ctx.systems.get_or_spawn(PERIMETER_SYS);
    let initial_asts = perim_handle
        .populate_asteroid_belt_entities()
        .await
        .expect("populate Perimeter belts");
    let initial_total_units: u64 = initial_asts.iter().map(|a| a.remaining_units).sum();

    let mining_convoy_id = ctx
        .living_world
        .ensure_system_mining_op(PERIMETER_SYS, &ctx.sde);
    for _ in 0..8 {
        ctx.advance_secs(20);
        let _ = ctx
            .living_world
            .step_all_mining_ops(&ctx.systems, &ctx.db, &ctx.sde)
            .await;
    }

    let post_asts = perim_handle
        .populate_asteroid_belt_entities()
        .await
        .expect("post-mining belts");
    let post_total_units: u64 = post_asts.iter().map(|a| a.remaining_units).sum();
    assert!(
        post_total_units < initial_total_units,
        "NPC mining op must extract ore from belt asteroids"
    );
    assert!(
        perim_handle
            .get_ball_snapshot(mining_convoy_id)
            .await
            .ok()
            .flatten()
            .is_some(),
        "NPC miner ball must be materialized in Perimeter ballpark"
    );

    let orders_rep = rpc(
        &ctx,
        &mut sess,
        "marketProxy",
        "GetOrders",
        &[PyRep::Integer(1230)],
    )
    .await
    .expect("marketProxy.GetOrders");
    assert!(!matches!(orders_rep, PyRep::None));

    // Multi-system HaulerConvoy stargate traversal (Jita -> Perimeter) & cargo drop
    let hauler_cid = 880_001i64;
    ctx.living_world.register_convoy(LivingWorldConvoy {
        convoy_id: hauler_cid,
        archetype: NpcArchetype::HaulerConvoy,
        ship_type_id: 648,
        origin_system_id: JITA_SYS,
        current_system_id: JITA_SYS,
        destination_system_id: PERIMETER_SYS,
        destination_station_id: 60_003_760,
        cargo_type_id: 34,
        cargo_quantity: 50_000,
        lod_level: 2,
        destroyed: false,
    });
    ctx.living_world
        .set_convoy_route(hauler_cid, vec![JITA_SYS, PERIMETER_SYS]);

    let jita_handle = ctx.systems.get_or_spawn(JITA_SYS);
    let (h_ball, h_combat) = sde_ball_and_combat(&ctx.sde, hauler_cid, 648, DVec3::ZERO);
    jita_handle
        .spawn_ball_with_combat(h_ball, h_combat)
        .await
        .expect("spawn hauler in Jita");

    let _ = ctx
        .living_world
        .step_hauler_convoy(hauler_cid, &ctx.systems, &ctx.db, &ctx.sde)
        .await;
    let transit = eve_server::living_world::LivingWorldDirector::compute_hauler_transit_duration_secs(
        &ctx.sde,
        648,
        JITA_SYS,
        None,
        Some(PERIMETER_SYS),
        60_003_760,
    );
    ctx.advance_secs(transit);
    let _ = ctx
        .living_world
        .step_hauler_convoy(hauler_cid, &ctx.systems, &ctx.db, &ctx.sde)
        .await;
    assert!(
        jita_handle
            .get_ball_snapshot(hauler_cid)
            .await
            .ok()
            .flatten()
            .is_none(),
        "Hauler ball must leave Jita ballpark after stargate jump"
    );
    assert!(
        perim_handle
            .get_ball_snapshot(hauler_cid)
            .await
            .ok()
            .flatten()
            .is_some(),
        "Hauler ball must materialize in Perimeter ballpark after stargate jump"
    );

    let (wreck_id, _bounty, dropped_types) =
        eve_server::services::combat_crimewatch::destroy_specialized_npc_and_queue_bounty(
            &ctx,
            PERIMETER_SYS,
            hauler_cid,
            PILOT_1,
        )
        .await;
    assert!(
        wreck_id > 0 && dropped_types.contains(&34),
        "Destroying hauler convoy must spawn wreck and drop Tritanium cargo"
    );
}

#[tokio::test]
async fn test_parity_mining_belts_as_balls_and_ore_distribution() {
    // parity: feat:mining.belts-as-balls, feat:mining.belt-ore-distribution
    eve_testkit::attest(&[
        "feat:mining.belts-as-balls",
        "feat:mining.belt-ore-distribution",
    ]);

    let ctx = ServerContext::new();

    let perim_handle = ctx.systems.get_or_spawn(PERIMETER_SYS);
    let perim_asts = perim_handle
        .populate_asteroid_belt_entities()
        .await
        .expect("Perimeter belts");
    assert!(!perim_asts.is_empty(), "Perimeter must spawn belt asteroids");
    let perim_types: HashSet<u32> = perim_asts.iter().map(|a| a.type_id).collect();
    assert!(perim_types.contains(&1230), "Highsec 0.9 must contain Veldspar");
    assert!(perim_types.contains(&1228), "Highsec 0.9 must contain Scordite");

    let first_ast = &perim_asts[0];
    let (_, ast_ball) = perim_handle
        .get_ball_snapshot(first_ast.asteroid_id)
        .await
        .expect("get_ball_snapshot")
        .expect("Asteroid must exist as a Ball in ballpark");
    assert_eq!(ast_ball.type_id, first_ast.type_id);
    assert!(ast_ball.radius >= 60.0 && ast_ball.mass > 0.0);

    let tama_handle = ctx.systems.get_or_spawn(30_002_813);
    let tama_asts = tama_handle
        .populate_asteroid_belt_entities()
        .await
        .expect("Tama belts");
    let tama_types: HashSet<u32> = tama_asts.iter().map(|a| a.type_id).collect();
    assert!(!tama_types.contains(&1230), "Lowsec must not spawn Veldspar");
    assert!(!tama_types.contains(&1228), "Lowsec must not spawn Scordite");
    assert!(
        tama_types.contains(&1226) || tama_types.contains(&1231) || tama_types.contains(&21),
        "Lowsec must spawn lowsec ores"
    );

    let null_handle = ctx.systems.get_or_spawn(30_004_759);
    let null_asts = null_handle
        .populate_asteroid_belt_entities()
        .await
        .expect("Nullsec belts");
    let null_types: HashSet<u32> = null_asts.iter().map(|a| a.type_id).collect();
    assert!(
        null_types.contains(&1229) && null_types.contains(&1225) && null_types.contains(&11396),
        "Nullsec must spawn Gneiss, Crokite, and Mercoxit"
    );
    let deep_null_ores: HashSet<u32> = ctx
        .sde
        .belt_ores_for_security_and_region(-0.8, 10_000_060)
        .into_iter()
        .map(|t| t.type_id)
        .collect();
    assert!(
        deep_null_ores.contains(&1223) && deep_null_ores.contains(&22) && deep_null_ores.contains(&11396),
        "Deep nullsec (-0.8) must distribute Bistot, Arkonor, and Mercoxit"
    );

    let pochven_ores: HashSet<u32> = ctx
        .sde
        .belt_ores_for_system(30_000_021)
        .into_iter()
        .map(|t| t.type_id)
        .collect();
    assert!(
        pochven_ores.contains(&52306)
            && pochven_ores.contains(&52315)
            && pochven_ores.contains(&52316),
        "Pochven must distribute Talassonite, Rakovene, and Bezdnacine"
    );
}

#[tokio::test]
async fn test_parity_excavator_drones_ore_and_ice() {

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let rorq_id = 850_020i64;
    let core_id = 850_021i64;
    let fuel_id = 850_022i64;
    let exc_ore_id = 850_023i64;
    let exc_ice_id = 850_024i64;
    let ore_ast_id = 850_025i64;
    let ice_ast_id = 850_026i64;

    let mut sess = select_pilot(&ctx, 50_020, "demo", "demo-secret", PILOT_1).await;
    spawn_pilot_ship_in_space(
        &ctx,
        &mut sess,
        PILOT_1,
        rorq_id,
        28352,
        PERIMETER_SYS,
        DVec3::ZERO,
    )
    .await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            core_id,
            InventoryItemRecord {
                item_id: core_id,
                type_id: 28583,
                owner_id: PILOT_1,
                location_id: rorq_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(core_id, true);
        snap.items.insert(
            fuel_id,
            InventoryItemRecord {
                item_id: fuel_id,
                type_id: 16273,
                owner_id: PILOT_1,
                location_id: rorq_id,
                flag: InventoryFlag::Cargo,
                quantity: 5_000,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            exc_ore_id,
            InventoryItemRecord {
                item_id: exc_ore_id,
                type_id: 41030,
                owner_id: PILOT_1,
                location_id: rorq_id,
                flag: InventoryFlag::DroneBay,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            exc_ice_id,
            InventoryItemRecord {
                item_id: exc_ice_id,
                type_id: 43681,
                owner_id: PILOT_1,
                location_id: rorq_id,
                flag: InventoryFlag::DroneBay,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        for (sk_id, lvl) in [(3438, 5), (3436, 5), (22541, 5), (43702, 5), (43703, 5), (28374, 5)] {
            snap.skills.entry(PILOT_1).or_default().insert(
                sk_id,
                TrainedSkillRecord {
                    type_id: sk_id,
                    level: lvl,
                    skill_points: 256_000.0,
                },
            );
        }
    });
    ctx.invalidate_ship_fitting(rorq_id);

    let handle = ctx.systems.get_or_spawn(PERIMETER_SYS);
    let (ore_ball, _) = sde_ball_and_combat(&ctx.sde, ore_ast_id, 1230, DVec3::new(1_500.0, 0.0, 0.0));
    handle
        .spawn_asteroid(
            AsteroidState {
                asteroid_id: ore_ast_id,
                belt_id: 0,
                type_id: 1230,
                initial_units: 100_000,
                remaining_units: 100_000,
                unit_volume: 0.1,
                depleted: false,
                respawn_at_tick: None,
            },
            ore_ball,
        )
        .await
        .expect("spawn Veldspar");

    let (ice_ball, _) = sde_ball_and_combat(&ctx.sde, ice_ast_id, 16264, DVec3::new(1_800.0, 0.0, 0.0));
    handle
        .spawn_asteroid(
            AsteroidState {
                asteroid_id: ice_ast_id,
                belt_id: 0,
                type_id: 16264,
                initial_units: 1_000,
                remaining_units: 1_000,
                unit_volume: 1_000.0,
                depleted: false,
                respawn_at_tick: None,
            },
            ice_ball,
        )
        .await
        .expect("spawn Blue Ice");

    let _ = rpc(
        &ctx,
        &mut sess,
        "ship",
        "LaunchDrones",
        &[PyRep::List(vec![PyRep::Integer(exc_ore_id), PyRep::Integer(exc_ice_id)])],
    )
    .await
    .expect("LaunchDrones");

    let no_core_err = rpc(
        &ctx,
        &mut sess,
        "entity",
        "CmdMineRepeatedly",
        &[
            PyRep::List(vec![PyRep::Integer(exc_ore_id)]),
            PyRep::Integer(ore_ast_id),
        ],
    )
    .await
    .expect_err("Excavator without Industrial Core must fail");
    assert_eq!(no_core_err.code, "IndustrialCoreRequired");

    let _ = rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(core_id), PyRep::String("industrialCore".into())],
    )
    .await
    .expect("Activate Capital Industrial Core");

    let _ = rpc(
        &ctx,
        &mut sess,
        "entity",
        "CmdMineRepeatedly",
        &[
            PyRep::List(vec![PyRep::Integer(exc_ore_id)]),
            PyRep::Integer(ore_ast_id),
        ],
    )
    .await
    .expect("Excavator ore mining with Industrial Core");

    let _ = rpc(
        &ctx,
        &mut sess,
        "entity",
        "CmdMineRepeatedly",
        &[
            PyRep::List(vec![PyRep::Integer(exc_ice_id)]),
            PyRep::Integer(ice_ast_id),
        ],
    )
    .await
    .expect("Excavator ice harvesting with Industrial Core");

    let next_tick = with_combat_runtime(&ctx, |rt| {
        rt.active_drone_miners
            .values()
            .map(|m| m.next_cycle_unix)
            .max()
            .unwrap()
    });
    eve_server::sp6_lifecycle::step_repeating_drone_mining(&ctx, next_tick).await;

    let (ore_mined, ice_mined) = ctx.db.with_snapshot(|snap| {
        let ore: i32 = snap
            .items
            .values()
            .filter(|i| i.location_id == rorq_id && i.type_id == 1230)
            .map(|i| i.quantity)
            .sum();
        let ice: i32 = snap
            .items
            .values()
            .filter(|i| i.location_id == rorq_id && i.type_id == 16264)
            .map(|i| i.quantity)
            .sum();
        (ore, ice)
    });
    assert!(ore_mined > 0, "Excavator mining drone must yield ore into Rorqual");
    assert!(ice_mined > 0, "Excavator ice drone must yield ice into Rorqual");
}
}


pub(crate) mod mining_goals_npc_standings_lookup_and_downtime {
    #[allow(unused_imports)]
    use super::*;

// E2E parity integration tests for Mining Market/Goals, Standings, Lookup, and Downtime.


use crate::features_market_orders_rpcs::{rpc, select_pilot, PILOT_1, PILOT_2};
use eve_db::{CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::living_world::sde_ball_and_combat;
use eve_server::router::ServerContext;
use eve_server::services::agents_missions::agents_standings_arcs::apply_faction_standing_with_derived;
use eve_server::sp6_lifecycle::tick_background_cluster;
use eve_server::system_actor::AsteroidState;
use glam::DVec3;

const JITA_SYS: u32 = 30_000_142;
const JITA_STATION: i64 = 60_003_760;

#[tokio::test]
async fn test_parity_mining_market_and_mining_goals() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let mut s1 = select_pilot(&ctx, 50_030, "demo", "demo-secret", PILOT_1).await;
    let mut s2 = select_pilot(&ctx, 50_031, "other", "other-secret", PILOT_2).await;

    // 1. Negative for feat:mining.mining-goals: client-supplied event must not populate mining_ledger
    let _ = rpc(
        &ctx,
        &mut s1,
        "seasonManager",
        "process_client_event",
        &[PyRep::String("totalMined".into()), PyRep::Integer(99_999)],
    )
    .await;
    assert!(
        ctx.db.get_mining_ledger_with_waste(PILOT_1).is_empty(),
        "Client-supplied event must never populate mining_ledger"
    );

    // 2. Positive for feat:mining.mining-goals: real mining in space advances ledger & season challenges
    let ship_id = 850_030i64;
    let miner_id = 850_031i64;
    let ast_id = 850_032i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&PILOT_1) {
            c.station_id = None;
            c.structure_id = None;
            c.solar_system_id = JITA_SYS;
            c.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 32880,
                owner_id: PILOT_1,
                location_id: JITA_SYS as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            miner_id,
            InventoryItemRecord {
                item_id: miner_id,
                type_id: 483,
                owner_id: PILOT_1,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(miner_id, true);
        snap.skills.entry(PILOT_1).or_default().insert(
            3386,
            eve_db::TrainedSkillRecord {
                type_id: 3386,
                level: 4,
                skill_points: 45_255.0,
            },
        );
    });
    s1.solar_system_id = Some(JITA_SYS);
    s1.station_id = None;
    s1.ship_id = Some(ship_id);
    ctx.invalidate_ship_fitting(ship_id);

    let jita_handle = ctx.systems.get_or_spawn(JITA_SYS);
    let (ship_ball, ship_combat) = sde_ball_and_combat(&ctx.sde, ship_id, 32880, DVec3::ZERO);
    jita_handle
        .spawn_ball_with_combat(ship_ball, ship_combat)
        .await
        .expect("spawn Venture");
    let (ast_ball, _) = sde_ball_and_combat(&ctx.sde, ast_id, 1230, DVec3::new(1_000.0, 0.0, 0.0));
    jita_handle
        .spawn_asteroid(
            AsteroidState {
                asteroid_id: ast_id,
                belt_id: 0,
                type_id: 1230,
                initial_units: 20_000,
                remaining_units: 20_000,
                unit_volume: 0.1,
                depleted: false,
                respawn_at_tick: None,
            },
            ast_ball,
        )
        .await
        .expect("spawn asteroid");

    let _ = rpc(&ctx, &mut s1, "dogmaIM", "ShipOnlineModules", &[PyRep::Integer(ship_id)])
        .await
        .expect("ShipOnlineModules");
    let _ = rpc(&ctx, &mut s1, "dogmaIM", "AddTarget", &[PyRep::Integer(ast_id)])
        .await
        .expect("AddTarget");
    ctx.advance_secs(5);
    jita_handle.step_manual(5.0).await.expect("step lock");
    let _ = rpc(
        &ctx,
        &mut s1,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(miner_id), PyRep::Integer(ast_id)],
    )
    .await
    .expect("Activate Miner I");
    ctx.advance_secs(60);
    tick_background_cluster(&ctx).await;

    assert!(
        !ctx.db.get_mining_ledger_with_waste(PILOT_1).is_empty(),
        "Real mining must record to mining_ledger"
    );
    let progress = rpc(
        &ctx,
        &mut s1,
        "seasonManager",
        "get_character_progress_by_character_id",
        &[],
    )
    .await
    .expect("get_character_progress_by_character_id");
    assert!(!matches!(progress, PyRep::None));

    // 3. feat:mining.mining-market: verify SDE volumes & trade raw ore (1230), compressed ore (62516), and Tritanium (34)
    let veld_vol = ctx.sde.types.get(&1230).map(|t| t.volume).unwrap_or(0.0);
    let comp_vol = ctx.sde.types.get(&62516).map(|t| t.volume).unwrap_or(0.0);
    let trit_vol = ctx.sde.types.get(&34).map(|t| t.volume).unwrap_or(0.0);
    assert!((veld_vol - 0.1).abs() < 1e-6, "Veldspar volume must be 0.1 m3");
    assert!(comp_vol < veld_vol, "Compressed Veldspar volume must be smaller than raw Veldspar");
    assert!((trit_vol - 0.01).abs() < 1e-6, "Tritanium volume must be 0.01 m3");

    let raw_ore_item = 850_033i64;
    let comp_ore_item = 850_034i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&PILOT_1) {
            c.station_id = Some(JITA_STATION);
        }
        if let Some(c2) = snap.characters.get_mut(&PILOT_2) {
            c2.station_id = Some(JITA_STATION);
            c2.balance = IskCents::from_isk(10_000_000);
        }
        snap.items.insert(
            raw_ore_item,
            InventoryItemRecord {
                item_id: raw_ore_item,
                type_id: 1230,
                owner_id: PILOT_1,
                location_id: JITA_STATION,
                flag: InventoryFlag::Hangar,
                quantity: 1_000,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            comp_ore_item,
            InventoryItemRecord {
                item_id: comp_ore_item,
                type_id: 62516,
                owner_id: PILOT_1,
                location_id: JITA_STATION,
                flag: InventoryFlag::Hangar,
                quantity: 200,
                singleton: false,
                ..Default::default()
            },
        );
    });
    s1.station_id = Some(JITA_STATION);
    s2.station_id = Some(JITA_STATION);

    let _ = rpc(
        &ctx,
        &mut s1,
        "reprocessingSvc",
        "Reprocess",
        &[
            PyRep::List(vec![PyRep::Integer(raw_ore_item)]),
            PyRep::Integer(JITA_STATION),
            PyRep::Integer(InventoryFlag::Hangar as i64),
            PyRep::Bool(false),
            PyRep::List(vec![]),
        ],
    )
    .await
    .expect("Reprocess Veldspar");

    let trit_item = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .find(|i| i.owner_id == PILOT_1 && i.location_id == JITA_STATION && i.type_id == 34 && i.quantity > 0)
            .map(|i| i.item_id)
            .expect("Tritanium produced")
    });

    let _ = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![
                PyRep::keyval([
                    ("stationID", PyRep::Integer(JITA_STATION)),
                    ("typeID", PyRep::Integer(62516)),
                    ("itemID", PyRep::Integer(comp_ore_item)),
                    ("price", PyRep::Real(20.0)),
                    ("quantity", PyRep::Integer(50)),
                ]),
                PyRep::keyval([
                    ("stationID", PyRep::Integer(JITA_STATION)),
                    ("typeID", PyRep::Integer(34)),
                    ("itemID", PyRep::Integer(trit_item)),
                    ("price", PyRep::Real(5.0)),
                    ("quantity", PyRep::Integer(100)),
                ]),
            ]),
            PyRep::Bool(false),
            PyRep::Integer(90),
        ],
    )
    .await
    .expect("PlaceMultiSellOrder");

    for tid in [62516i64, 34i64] {
        let orders = rpc(
            &ctx,
            &mut s2,
            "marketProxy",
            "GetOrders",
            &[PyRep::Integer(tid)],
        )
        .await
        .expect("marketProxy.GetOrders");
        assert!(
            !matches!(orders, PyRep::None),
            "Market orders for type {tid} must be listed"
        );
    }
}

#[tokio::test]
async fn test_parity_standings_npc_npc_standings() {
    let ctx = ServerContext::new();
    let mut unauth = SessionState::new(99_999);
    assert!(
        rpc(&ctx, &mut unauth, "standingMgr", "GetNPCNPCStandings", &[])
            .await
            .is_err(),
        "Unauthenticated GetNPCNPCStandings must fail"
    );

    let mut sess = select_pilot(&ctx, 50_040, "demo", "demo-secret", PILOT_1).await;

    let rep = rpc(&ctx, &mut sess, "standingMgr", "GetNPCNPCStandings", &[])
        .await
        .expect("GetNPCNPCStandings");
    let PyRep::Rowset { rows, .. } = rep else {
        panic!("Expected Rowset from GetNPCNPCStandings");
    };
    assert!(!rows.is_empty(), "GetNPCNPCStandings must return inter-faction pairs");

    let mut found_amarr = false;
    let mut found_gallente = false;
    for r in &rows {
        if let (Some(from_id), Some(to_id), Some(val)) = (r[0].as_i64(), r[1].as_i64(), r[2].as_f64()) {
            if from_id == 500_001 && to_id == 500_003 {
                assert!((val - 5.0).abs() < 1e-6);
                found_amarr = true;
            }
            if from_id == 500_001 && to_id == 500_004 {
                assert!((val - (-5.0)).abs() < 1e-6);
                found_gallente = true;
            }
        }
    }
    assert!(found_amarr && found_gallente, "Must include Caldari-Amarr and Caldari-Gallente relationships");

    let mut changes = Vec::new();
    ctx.db.with_mut_snapshot(|snap| {
        apply_faction_standing_with_derived(&ctx, snap, PILOT_1, 500_001, 20.0, true, &mut changes);
    });

    let (amarr_eff, gallente_eff) = ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&PILOT_1).unwrap();
        (
            ch.standings.get(&500_003).copied().unwrap_or(0.0),
            ch.standings.get(&500_004).copied().unwrap_or(0.0),
        )
    });
    assert!(amarr_eff > 0.0, "Amarr standing must derive positively from Caldari gain");
    assert!(gallente_eff < 0.0, "Gallente standing must derive negatively from Caldari gain");
}

#[tokio::test]
async fn test_parity_ui_lookup_all_methods() {
    // parity: feat:ui.lookup
    eve_testkit::attest(&["feat:ui.lookup"]);

    let ctx = ServerContext::new();
    let corp_id = 98_050_001i64;
    let mut sess = select_pilot(&ctx, 50_050, "demo", "demo-secret", PILOT_1).await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Astraeus Mining Guild".to_string(),
                ticker: "AMG".to_string(),
                ceo_id: PILOT_1,
                ..Default::default()
            },
        );
    });

    let kv_get = |rep: &PyRep, key: &str| -> Option<PyRep> {
        if let PyRep::KeyVal(entries) = rep {
            entries.iter().find(|(k, _)| *k == key).map(|(_, v)| v.clone())
        } else {
            None
        }
    };

    let chars = rpc(&ctx, &mut sess, "lookupSvc", "LookupCharacters", &[PyRep::String("".into()), PyRep::Bool(false)])
        .await
        .expect("LookupCharacters");
    let PyRep::List(char_list) = chars else { panic!("Expected List") };
    assert!(char_list.iter().any(|item| kv_get(item, "characterID").and_then(|v| v.as_i64()) == Some(PILOT_1)));

    let corps = rpc(&ctx, &mut sess, "lookupSvc", "LookupCorporations", &[PyRep::String("Astraeus".into()), PyRep::Bool(false)])
        .await
        .expect("LookupCorporations");
    let PyRep::List(corp_list) = corps else { panic!("Expected List") };
    assert_eq!(corp_list.len(), 1);
    assert_eq!(kv_get(&corp_list[0], "corporationID").and_then(|v| v.as_i64()), Some(corp_id));
    assert_eq!(kv_get(&corp_list[0], "corporationName").as_ref().and_then(|v| v.as_str()), Some("Astraeus Mining Guild"));

    let owners = rpc(&ctx, &mut sess, "lookupSvc", "LookupOwners", &[PyRep::String("Astraeus".into()), PyRep::Bool(false)])
        .await
        .expect("LookupOwners");
    let PyRep::List(owner_list) = owners else { panic!("Expected List") };
    assert!(owner_list.iter().any(|item| kv_get(item, "ownerID").and_then(|v| v.as_i64()) == Some(corp_id)));

    let locs = rpc(&ctx, &mut sess, "lookupSvc", "LookupLocationsByGroup", &[PyRep::Integer(5), PyRep::String("Jita".into())])
        .await
        .expect("LookupLocationsByGroup");
    let PyRep::List(loc_list) = locs else { panic!("Expected List") };
    assert!(loc_list.iter().any(|item| kv_get(item, "itemID").and_then(|v| v.as_i64()) == Some(JITA_SYS as i64)));

    let warable = rpc(&ctx, &mut sess, "lookupSvc", "LookupWarableCorporationsOrAlliances", &[PyRep::String("Astraeus".into())])
        .await
        .expect("LookupWarableCorporationsOrAlliances");
    let PyRep::List(warable_list) = warable else { panic!("Expected List") };
    assert!(warable_list.is_empty(), "Corp without structures must not be war-eligible");
}

#[tokio::test]
async fn test_parity_serverops_daily_downtime() {
    let ctx = ServerContext::new();
    let base_1055 = 1_767_264_900u64;
    ctx.set_clock(std::sync::Arc::new(eve_db::VirtualServerClock::with_fixed_base(
        base_1055,
    )));

    let _sess = select_pilot(&ctx, 50_060, "demo", "demo-secret", PILOT_1).await;
    assert_eq!(ctx.db.online_session_count(), 1);

    tick_background_cluster(&ctx).await;
    assert!(!ctx.is_shutting_down());
    assert_eq!(ctx.db.online_session_count(), 1);

    ctx.advance_secs(300);
    tick_background_cluster(&ctx).await;
    assert!(ctx.is_shutting_down(), "Cluster must be in downtime at 11:00 UTC");
    assert_eq!(ctx.db.online_session_count(), 0, "Non-GM sessions must be disconnected at 11:00 UTC");

    ctx.advance_secs(900);
    tick_background_cluster(&ctx).await;
    assert!(!ctx.is_shutting_down(), "Cluster must complete restart by 11:15 UTC");
}
}
