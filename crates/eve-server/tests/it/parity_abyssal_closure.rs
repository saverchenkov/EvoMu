//! End-to-end integration tests closing Abyssal Deadspace parity features:
//! - `feat:abyssal.automated-pylons`
//! - `feat:abyssal.spatial-hazard-clouds`
//! - `feat:abyssal.weather-effects-and-penalties`

#![forbid(unsafe_code)]

use crate::abyssal_deadspace_closure::{
    login_pilot_in_space, move_pilot_to_system, seed_cargo_item, FIL_T3_ELEC, SYS_08_LASHESIH,
    SYS_LOW_TAMA,
};
use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, DamageVector, MovementMode};
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::pve_special_sites::with_pve_special_reg;
use eve_server::services::pve_special_sites_abyssal::{
    evacuate_abyssal_pockets_for_downtime, evaluate_abyssal_spatial_clouds_for_ship,
    roll_abyssal_weather_penalty_pct, spatial_cloud_radius_m, suppressor_pulse_damage,
    suppressor_radius_m, tracking_pylon_radius_and_mult, AbyssalWeatherKind,
    AUTOMATA_SUPPRESSOR_LARGE_TYPE_ID, AUTOMATA_SUPPRESSOR_MEDIUM_TYPE_ID,
    AUTOMATA_SUPPRESSOR_SMALL_TYPE_ID, BIOLUMINESCENT_CLOUD_LARGE_TYPE_ID,
    BIOLUMINESCENT_CLOUD_SMALL_TYPE_ID, BIOLUMINESCENT_CLOUD_TYPE_ID,
    FILAMENT_CLOUD_LARGE_TYPE_ID, FILAMENT_CLOUD_SMALL_TYPE_ID, FILAMENT_CLOUD_TYPE_ID,
    MULTIBODY_TRACKING_PYLON_MEDIUM_TYPE_ID, MULTIBODY_TRACKING_PYLON_TYPE_ID,
    PROVING_TACHYON_CLOUD_TYPE_ID, SPATIAL_CLOUD_RADIUS_M, TACHYON_CLOUD_TYPE_ID,
};
use glam::DVec3;
use std::collections::HashMap;

// parity: feat:abyssal.automated-pylons
#[tokio::test]
async fn test_parity_abyssal_automated_pylons_tick_and_conduit() {
    eve_testkit::attest(&["feat:abyssal.automated-pylons"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let kw = HashMap::new();
    let abyss_sys = 32_000_011_u32;
    let sys_h = ctx.systems.get_or_spawn(abyss_sys);

    // 1. Verify V24 SDE radii and damage profiles
    assert_eq!(suppressor_radius_m(Some(&ctx.sde), AUTOMATA_SUPPRESSOR_SMALL_TYPE_ID, 0.0), 15_000.0);
    assert_eq!(suppressor_radius_m(Some(&ctx.sde), AUTOMATA_SUPPRESSOR_MEDIUM_TYPE_ID, 0.0), 40_000.0);
    assert_eq!(suppressor_radius_m(Some(&ctx.sde), AUTOMATA_SUPPRESSOR_LARGE_TYPE_ID, 0.0), 40_000.0);
    assert_eq!(
        suppressor_pulse_damage(Some(&ctx.sde), AUTOMATA_SUPPRESSOR_LARGE_TYPE_ID),
        DamageVector { em: 0.0, thermal: 0.0, kinetic: 0.0, explosive: 90.0 }
    );
    assert_eq!(tracking_pylon_radius_and_mult(Some(&ctx.sde), MULTIBODY_TRACKING_PYLON_TYPE_ID, 0.0), (15_000.0, 1.8));
    assert_eq!(tracking_pylon_radius_and_mult(Some(&ctx.sde), MULTIBODY_TRACKING_PYLON_MEDIUM_TYPE_ID, 0.0), (40_000.0, 1.6));

    // 2. Spawn ships, drones, sentries, missiles, fighters, and NPCs
    let ship_id = 751_000_001_i64;
    let sentry_id = 751_000_002_i64;
    let drone_id = 751_000_003_i64;
    let missile_id = 751_000_004_i64;
    let fighter_id = 751_000_005_i64;
    let rogue_npc_id = 751_000_006_i64;
    let trig_npc_id = 751_000_007_i64;
    let tessera_id = 751_000_008_i64;

    for (bid, tid, pos, max_vel) in [
        (ship_id, 621_u32, DVec3::new(10_000.0, 0.0, 0.0), 250.0),
        (sentry_id, 23_559, DVec3::new(10_500.0, 0.0, 0.0), 0.0),
        (drone_id, 2456, DVec3::new(35_000.0, 0.0, 0.0), 400.0),
        (missile_id, 204, DVec3::new(35_000.0, 100.0, 0.0), 2_000.0),
        (fighter_id, 23_057, DVec3::new(35_000.0, 200.0, 0.0), 800.0),
        (rogue_npc_id, 47_847, DVec3::new(35_000.0, 300.0, 0.0), 500.0),
        (trig_npc_id, 48_092, DVec3::new(12_000.0, 0.0, 0.0), 400.0),
        (tessera_id, 48_262, DVec3::new(10_000.0, 200.0, 0.0), 300.0),
    ] {
        let mut cs = ctx.build_type_combat_state(tid);
        cs.shield_hp = 100.0;
        cs.max_shield_hp = 100.0;
        cs.shield_recharge_time_ms = 0.0;
        cs.structure_hp = 5_000.0;
        cs.max_structure_hp = 5_000.0;
        let _ = sys_h
            .spawn_ball_with_combat(
                Ball {
                    id: bid,
                    type_id: tid,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: max_vel,
                    inertia: 1.0,
                    mass: 1_000.0,
                    radius: 25.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                cs,
            )
            .await;
    }

    // Small Suppressor (15 km) spares 35 km targets and spares ships/sentries/Triglavians/Tesseras
    let _ = sys_h
        .spawn_ball(Ball {
            id: 751_010_001,
            type_id: AUTOMATA_SUPPRESSOR_SMALL_TYPE_ID,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1.0,
            radius: 500.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    let _ = sys_h.try_step_full_tick(1.0);
    assert_eq!(sys_h.get_combat_state(drone_id).await.unwrap().unwrap().shield_hp, 100.0);
    assert_eq!(sys_h.get_combat_state(sentry_id).await.unwrap().unwrap().shield_hp, 100.0);
    assert_eq!(sys_h.get_combat_state(tessera_id).await.unwrap().unwrap().shield_hp, 100.0);
    assert_eq!(sys_h.get_combat_state(trig_npc_id).await.unwrap().unwrap().shield_hp, 100.0);
    assert_eq!(sys_h.get_combat_state(ship_id).await.unwrap().unwrap().shield_hp, 100.0);
    sys_h.try_remove_ball(751_010_001);

    // Medium Suppressor (40 km) damages drone, missile, fighter, and rogue drone NPC at 35 km
    let _ = sys_h
        .spawn_ball(Ball {
            id: 751_010_002,
            type_id: AUTOMATA_SUPPRESSOR_MEDIUM_TYPE_ID,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1.0,
            radius: 500.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    let _ = sys_h.try_step_full_tick(1.0);
    assert!(sys_h.get_combat_state(drone_id).await.unwrap().unwrap().shield_hp < 100.0);
    assert!(sys_h.get_combat_state(missile_id).await.unwrap().unwrap().shield_hp < 100.0);
    assert!(sys_h.get_combat_state(fighter_id).await.unwrap().unwrap().shield_hp < 100.0);
    assert!(sys_h.get_combat_state(rogue_npc_id).await.unwrap().unwrap().shield_hp < 100.0);
    assert_eq!(sys_h.get_combat_state(sentry_id).await.unwrap().unwrap().shield_hp, 100.0);
    sys_h.try_remove_ball(751_010_002);

    // Multibody Tracking Pylons (40 km +60% = 1.6x, 15 km +80% = 1.8x) & restoration on exit
    let _ = sys_h
        .spawn_ball(Ball {
            id: 751_010_003,
            type_id: MULTIBODY_TRACKING_PYLON_MEDIUM_TYPE_ID,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1.0,
            radius: 500.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    let _ = sys_h.try_step_full_tick(1.0);
    assert!((sys_h.get_combat_state(ship_id).await.unwrap().unwrap().tracking_speed_multiplier - 1.6).abs() < 1e-6);
    assert!((sys_h.get_combat_state(sentry_id).await.unwrap().unwrap().tracking_speed_multiplier - 1.6).abs() < 1e-6);

    let _ = sys_h
        .spawn_ball(Ball {
            id: 751_010_004,
            type_id: MULTIBODY_TRACKING_PYLON_TYPE_ID,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1.0,
            radius: 500.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    let _ = sys_h.try_step_full_tick(1.0);
    assert!((sys_h.get_combat_state(ship_id).await.unwrap().unwrap().tracking_speed_multiplier - 1.8).abs() < 1e-6);

    let _ = sys_h.set_ball_position(ship_id, DVec3::new(45_000.0, 0.0, 0.0)).await;
    let _ = sys_h.try_step_full_tick(1.0);
    assert!((sys_h.get_combat_state(ship_id).await.unwrap().unwrap().tracking_speed_multiplier - 1.0).abs() < 1e-6);

    // 3. End-to-end Abyssal pocket: Suppressor kill of last room NPC unlocks conduit
    let char1 = 90_000_001_i64;
    let c_ship = 651_000_020_i64;
    let mut sess1 = login_pilot_in_space(&ctx, 888_301, "demo", "demo-secret", char1, c_ship, 621, SYS_08_LASHESIH, DVec3::new(5_000_000.0, 2.0e9, 0.0)).await;
    seed_cargo_item(&ctx, 751_100_011, FIL_T3_ELEC, char1, c_ship, 1);
    let d3 = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(751_100_011)], &kw, None).await.expect("dep T3");
    let t3 = d3.dict_get("traceBallID").and_then(|v| v.as_i64()).unwrap();
    ctx.set_mechanic_rng_seed(Some(4));
    let e3 = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(t3)], &kw, None).await.expect("ent T3");
    let pid3 = e3.dict_get("pocketID").and_then(|v| v.as_i64()).unwrap();
    let p3_sys = e3.dict_get("solarSystemID").and_then(|v| v.as_i64()).unwrap() as u32;
    let p3_h = ctx.systems.get_or_spawn(p3_sys);

    let r1_npcs = with_pve_special_reg(&ctx, |reg| reg.abyssal_pockets[&char1].rooms[0].npc_ball_ids.clone());
    for &nid in &r1_npcs[1..] {
        let _ = p3_h
            .apply_damage_with_attacker(
                nid,
                DamageVector { em: 1e7, thermal: 1e7, kinetic: 1e7, explosive: 1e7 },
                Some(char1),
            )
            .await;
    }
    let last_npc = r1_npcs[0];
    let sup_pos = p3_h.get_ball_snapshot(1_899_000_000 + pid3 * 100 + 11).await.unwrap().unwrap().1.position;
    {
        let mut bp = p3_h.ballpark.write().unwrap();
        if let Some(b) = bp.balls.get_mut(&last_npc) {
            b.type_id = 47_847;
            b.position = sup_pos;
        }
        if let Some(cs) = bp.combat_states.get_mut(&last_npc) {
            cs.shield_hp = 0.0;
            cs.armor_hp = 0.0;
            cs.structure_hp = 10.0;
        }
    }
    let _ = p3_h.try_step_full_tick(1.0);
    assert!(with_pve_special_reg(&ctx, |reg| reg.abyssal_pockets[&char1].rooms[0].cleared));
    let _ = evacuate_abyssal_pockets_for_downtime(&ctx);
}

// parity: feat:abyssal.spatial-hazard-clouds
#[tokio::test]
async fn test_parity_abyssal_spatial_hazard_clouds_tick_and_rpc() {
    eve_testkit::attest(&["feat:abyssal.spatial-hazard-clouds"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let abyss_sys = 32_000_012_u32;
    let sys_h = ctx.systems.get_or_spawn(abyss_sys);

    assert_eq!(spatial_cloud_radius_m(Some(&ctx.sde), BIOLUMINESCENT_CLOUD_SMALL_TYPE_ID, 0.0), 5_000.0);
    assert_eq!(spatial_cloud_radius_m(Some(&ctx.sde), BIOLUMINESCENT_CLOUD_TYPE_ID, 0.0), 15_000.0);
    assert_eq!(spatial_cloud_radius_m(Some(&ctx.sde), BIOLUMINESCENT_CLOUD_LARGE_TYPE_ID, 0.0), 40_000.0);
    assert_eq!(spatial_cloud_radius_m(Some(&ctx.sde), FILAMENT_CLOUD_SMALL_TYPE_ID, 0.0), 5_000.0);
    assert_eq!(spatial_cloud_radius_m(Some(&ctx.sde), FILAMENT_CLOUD_TYPE_ID, 0.0), 15_000.0);
    assert_eq!(spatial_cloud_radius_m(Some(&ctx.sde), FILAMENT_CLOUD_LARGE_TYPE_ID, 0.0), 40_000.0);
    assert_eq!(spatial_cloud_radius_m(Some(&ctx.sde), PROVING_TACHYON_CLOUD_TYPE_ID, 0.0), 40_000.0);

    let ship_id = 752_000_001_i64;
    let base_cs = ctx.build_type_combat_state(621);
    let base_ball = eve_server::services::pve_abyssal_pylons::build_fitted_ship_space_ball(
        &ctx,
        0,
        ship_id,
        621,
        DVec3::ZERO,
    );
    let default_vel = base_ball.max_velocity;
    let default_in = base_ball.inertia;
    let _ = sys_h.spawn_ball_with_combat(base_ball, base_cs.clone()).await;

    for (cid, ctid, cx) in [
        (752_020_001_i64, BIOLUMINESCENT_CLOUD_TYPE_ID, 0.0),
        (752_020_002, FILAMENT_CLOUD_TYPE_ID, 50_000.0),
        (752_020_003, TACHYON_CLOUD_TYPE_ID, -50_000.0),
    ] {
        let _ = sys_h
            .spawn_ball(Ball {
                id: cid,
                type_id: ctid,
                position: DVec3::new(cx, 50_000.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 1.0,
                radius: SPATIAL_CLOUD_RADIUS_M,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
    }

    // 1. Verify via SystemActor simulation tick (`try_step_full_tick`)
    let _ = sys_h.set_ball_position(ship_id, DVec3::new(0.0, 50_000.0, 0.0)).await;
    let _ = sys_h.try_step_full_tick(1.0);
    assert!((sys_h.get_combat_state(ship_id).await.unwrap().unwrap().signature_radius - base_cs.signature_radius * 4.0).abs() < 1e-3);

    let _ = sys_h.set_ball_position(ship_id, DVec3::new(50_000.0, 50_000.0, 0.0)).await;
    let _ = sys_h.try_step_full_tick(1.0);
    let fil_cs = sys_h.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!((fil_cs.shield_boost_multiplier - 0.6).abs() < 1e-6);
    assert!((fil_cs.signature_radius - base_cs.signature_radius).abs() < 1e-3);

    let _ = sys_h.set_ball_position(ship_id, DVec3::new(-50_000.0, 50_000.0, 0.0)).await;
    let _ = sys_h.try_step_full_tick(1.0);
    let (_, tach_ball) = sys_h.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!((tach_ball.max_velocity - default_vel * 4.0).abs() < 1e-3);
    assert!((tach_ball.inertia - default_in * 0.5).abs() < 1e-3);
    assert!((sys_h.get_combat_state(ship_id).await.unwrap().unwrap().signature_radius - base_cs.signature_radius * 4.0).abs() < 1e-3);

    // Leaving cloud restores all attributes immediately on tick
    let _ = sys_h.set_ball_position(ship_id, DVec3::ZERO).await;
    let _ = sys_h.try_step_full_tick(1.0);
    let (_, exit_ball) = sys_h.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let exit_cs = sys_h.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!((exit_ball.max_velocity - default_vel).abs() < 1e-3);
    assert!((exit_ball.inertia - default_in).abs() < 1e-3);
    assert!((exit_cs.signature_radius - base_cs.signature_radius).abs() < 1e-3);
    assert!((exit_cs.shield_boost_multiplier - 1.0).abs() < 1e-6);

    // 2. Verify via `evaluate_abyssal_spatial_clouds_for_ship`
    let _ = sys_h.set_ball_position(ship_id, DVec3::new(-50_000.0, 50_000.0, 0.0)).await;
    assert_eq!(evaluate_abyssal_spatial_clouds_for_ship(&ctx, abyss_sys, ship_id).await, Some("TachyonCloud"));
    let _ = sys_h.set_ball_position(ship_id, DVec3::ZERO).await;
    assert!(evaluate_abyssal_spatial_clouds_for_ship(&ctx, abyss_sys, ship_id).await.is_none());
}

// parity: feat:abyssal.weather-effects-and-penalties
#[tokio::test]
async fn test_parity_abyssal_weather_effects_and_penalties_all_five() {
    eve_testkit::attest(&["feat:abyssal.weather-effects-and-penalties"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let kw = HashMap::new();
    let char1 = 90_000_001_i64;
    let c_ship = 653_000_020_i64;
    let mut sess1 = login_pilot_in_space(&ctx, 889_301, "demo", "demo-secret", char1, c_ship, 621, SYS_LOW_TAMA, DVec3::new(0.0, 2.0e9, 0.0)).await;

    let mut low_seed = None;
    let mut high_seed = None;
    for s in 1_u64..100 {
        ctx.set_mechanic_rng_seed(Some(s));
        if ctx.mechanic_rng.roll(0.5) {
            if high_seed.is_none() {
                high_seed = Some(s);
            }
        } else if low_seed.is_none() {
            low_seed = Some(s);
        }
        if low_seed.is_some() && high_seed.is_some() {
            break;
        }
    }
    let (low_seed, high_seed) = (low_seed.unwrap(), high_seed.unwrap());
    ctx.set_mechanic_rng_seed(Some(low_seed));
    assert_eq!(roll_abyssal_weather_penalty_pct(&ctx, 1), 30.0);
    ctx.set_mechanic_rng_seed(Some(high_seed));
    assert_eq!(roll_abyssal_weather_penalty_pct(&ctx, 4), 70.0);

    let base_ship_cs = ctx.build_ship_combat_state(char1, c_ship, 621);
    let fitted_base_vel = eve_server::services::pve_abyssal_pylons::build_fitted_ship_space_ball(&ctx, char1, c_ship, 621, DVec3::ZERO).max_velocity;
    let base_opt = ctx.sde.types.get(&621).and_then(|t| t.default_attributes.get(&54).copied()).unwrap_or(15_000.0);
    let base_scan = ctx.sde.types.get(&621).and_then(|t| t.default_attributes.get(&564).copied()).unwrap_or(250.0);

    let weather_cases = [
        (47_765_u32, AbyssalWeatherKind::Electrical, low_seed, 30.0_f64),
        (47_762, AbyssalWeatherKind::Dark, high_seed, 50.0),
        (47_761, AbyssalWeatherKind::Exotic, low_seed, 30.0),
        (47_763, AbyssalWeatherKind::Firestorm, high_seed, 50.0),
        (47_764, AbyssalWeatherKind::Gamma, low_seed, 30.0),
        (47_906, AbyssalWeatherKind::Electrical, high_seed, 70.0),
        (47_894, AbyssalWeatherKind::Dark, high_seed, 70.0),
        (47_890, AbyssalWeatherKind::Exotic, high_seed, 70.0),
        (47_898, AbyssalWeatherKind::Firestorm, high_seed, 70.0),
        (47_902, AbyssalWeatherKind::Gamma, high_seed, 70.0),
    ];

    for (idx, (fil_tid, weather, seed, exp_pen)) in weather_cases.into_iter().enumerate() {
        move_pilot_to_system(&ctx, &mut sess1, char1, c_ship, 621, SYS_LOW_TAMA, DVec3::new((idx as f64 + 1.0) * 6_000_000.0, 2.0e9, 0.0)).await;
        ctx.set_mechanic_rng_seed(Some(seed));
        let fiid = 753_200_000 + idx as i64;
        seed_cargo_item(&ctx, fiid, fil_tid, char1, c_ship, 1);
        let d_w = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(fiid)], &kw, None).await.expect("dep w");
        let t_w = d_w.dict_get("traceBallID").and_then(|v| v.as_i64()).unwrap();
        let e_w = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(t_w)], &kw, None).await.expect("ent w");
        let w_sys = e_w.dict_get("solarSystemID").and_then(|v| v.as_i64()).unwrap() as u32;
        let w_h = ctx.systems.get_or_spawn(w_sys);
        let npc_id = with_pve_special_reg(&ctx, |reg| {
            let p = &reg.abyssal_pockets[&char1];
            assert_eq!(p.weather, weather);
            assert_eq!(p.weather_penalty_pct, exp_pen);
            p.rooms[0].npc_ball_ids[0]
        });
        let (_, s_ball) = w_h.get_ball_snapshot(c_ship).await.unwrap().unwrap();
        let s_cs = w_h.get_combat_state(c_ship).await.unwrap().unwrap();
        let (_, n_ball) = w_h.get_ball_snapshot(npc_id).await.unwrap().unwrap();
        let n_cs = w_h.get_combat_state(npc_id).await.unwrap().unwrap();
        let n_base_cs = ctx.build_type_combat_state(n_ball.type_id);
        let n_base_vel = ctx.sde.types.get(&n_ball.type_id).and_then(|t| t.default_attributes.get(&37).copied()).filter(|&v| v > 0.0).unwrap_or(380.0);
        let pen = exp_pen / 100.0;
        let dyn_attrs = ctx.db.with_snapshot(|s| s.items[&c_ship].dynamic_attributes.clone());
        let pen_res = |r: f64| (r * (1.0 + pen)).clamp(0.0, 1.0);
        match weather {
            AbyssalWeatherKind::Electrical => {
                assert!((s_cs.shield_resonance[0] - pen_res(base_ship_cs.shield_resonance[0])).abs() < 1e-4);
                assert!((s_cs.armor_resonance[0] - pen_res(base_ship_cs.armor_resonance[0])).abs() < 1e-4);
                assert!((s_cs.structure_resonance[0] - pen_res(base_ship_cs.structure_resonance[0])).abs() < 1e-4);
                assert!((s_cs.capacitor_recharge_time_ms - (base_ship_cs.capacitor_recharge_time_ms * 0.5).max(1_000.0)).abs() < 1.0);
                assert!((n_cs.armor_resonance[0] - pen_res(n_base_cs.armor_resonance[0])).abs() < 1e-4);
            }
            AbyssalWeatherKind::Dark => {
                assert!((s_ball.max_velocity - fitted_base_vel * 1.5).abs() < 1e-3);
                assert!((dyn_attrs[&54] - base_opt * (1.0 - pen)).abs() < 1.0);
                assert!((n_ball.max_velocity - n_base_vel * 1.5).abs() < 1e-3);
            }
            AbyssalWeatherKind::Exotic => {
                assert!((s_cs.shield_resonance[2] - pen_res(base_ship_cs.shield_resonance[2])).abs() < 1e-4);
                assert!((s_cs.armor_resonance[2] - pen_res(base_ship_cs.armor_resonance[2])).abs() < 1e-4);
                assert!((s_cs.structure_resonance[2] - pen_res(base_ship_cs.structure_resonance[2])).abs() < 1e-4);
                assert!((dyn_attrs[&564] - base_scan * 1.5).abs() < 1e-3);
                assert!((n_cs.armor_resonance[2] - pen_res(n_base_cs.armor_resonance[2])).abs() < 1e-4);
            }
            AbyssalWeatherKind::Firestorm => {
                assert!((s_cs.max_armor_hp - base_ship_cs.max_armor_hp * 1.5).abs() < 1.0);
                assert!((s_cs.armor_resonance[1] - pen_res(base_ship_cs.armor_resonance[1])).abs() < 1e-4);
                assert!((s_cs.structure_resonance[1] - pen_res(base_ship_cs.structure_resonance[1])).abs() < 1e-4);
                assert!((n_cs.armor_resonance[1] - pen_res(n_base_cs.armor_resonance[1])).abs() < 1e-4);
            }
            AbyssalWeatherKind::Gamma => {
                assert!((s_cs.max_shield_hp - base_ship_cs.max_shield_hp * 1.5).abs() < 1.0);
                assert!((s_cs.shield_resonance[3] - pen_res(base_ship_cs.shield_resonance[3])).abs() < 1e-4);
                assert!((s_cs.structure_resonance[3] - pen_res(base_ship_cs.structure_resonance[3])).abs() < 1e-4);
                assert!((n_cs.shield_resonance[3] - pen_res(n_base_cs.shield_resonance[3])).abs() < 1e-4);
            }
        }
        let _ = evacuate_abyssal_pockets_for_downtime(&ctx);
        assert!(ctx.db.with_snapshot(|s| s.items[&c_ship].dynamic_attributes.is_empty()));
    }

    // Live Gunnery turret optimal & falloff penalty in Dark weather (-50%) and restoration on exit
    move_pilot_to_system(&ctx, &mut sess1, char1, c_ship, 624, SYS_LOW_TAMA, DVec3::new(90_000_000.0, 2.0e9, 0.0)).await;
    let (turret_id, ammo_id) = (753_300_001_i64, 753_300_002_i64);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(turret_id, InventoryItemRecord { item_id: turret_id, type_id: 487, owner_id: char1, location_id: c_ship, flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, version: 1, ..Default::default() });
        snap.items.insert(ammo_id, InventoryItemRecord { item_id: ammo_id, type_id: 182, owner_id: char1, location_id: c_ship, flag: InventoryFlag::HiSlot0, quantity: 40, singleton: false, version: 1, ..Default::default() });
        snap.module_online_states.insert(turret_id, true);
    });
    ctx.invalidate_ship_fitting(c_ship);
    ctx.set_mechanic_rng_seed(Some(low_seed));
    seed_cargo_item(&ctx, 753_300_003, 47_894, char1, c_ship, 1);
    let d_dk = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(753_300_003)], &kw, None).await.expect("dep dark");
    let e_dk = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(d_dk.dict_get("traceBallID").unwrap().as_i64().unwrap())], &kw, None).await.expect("ent dark");
    let dk_sys = e_dk.dict_get("solarSystemID").unwrap().as_i64().unwrap() as u32;
    let dk_npc = e_dk.dict_get("npcBallIDs").and_then(|v| if let PyRep::List(l) = v { l[0].as_i64() } else { None }).unwrap();
    let dk_h = ctx.systems.get_or_spawn(dk_sys);
    let _ = dk_h.set_ball_position(c_ship, DVec3::ZERO).await;
    let _ = dk_h.set_ball_position(dk_npc, DVec3::new(30_000.0, 0.0, 0.0)).await;
    let _ = dk_h.add_target_lock(c_ship, eve_destiny::TargetLock { target_id: dk_npc, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: None }).await;
    let oor = eve_server::services::combat_crimewatch::dispatch_combat_rpc(&ctx, &mut sess1, "dogmaIM", "Activate", &[PyRep::Integer(turret_id), PyRep::String("projectileFired".into()), PyRep::Integer(dk_npc)], &kw).await.unwrap_err();
    assert_eq!(oor.code, "TargetOutOfRange");
    let _ = evacuate_abyssal_pockets_for_downtime(&ctx);
    assert!(ctx.db.with_snapshot(|s| s.items[&turret_id].dynamic_attributes.is_empty()));
}
