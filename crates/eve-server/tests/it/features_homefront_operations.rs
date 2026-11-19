//! Integration tests for batch 42 open `feat:` parity rows.
//!
//! Covers:
//! - `feat:abyssal.bioadaptive-cache-rewards`
//! - `feat:abyssal.spatial-hazard-clouds`
//! - `feat:mining.foreman-burst-charges`
//! - `feat:modules.stasis-grappler-mechanics`
//! - `feat:navigation.micro-jump-drive-spool`
//! - `feat:navigation.warp-scramble-propulsion-shutoff`

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{
    FleetRecord, InventoryFlag, InventoryItemRecord, IskCents, JumpCloneRecord, TrainedSkillRecord,
};
use eve_destiny::{Ball, MovementMode, TargetLock, WarpBubble};
use eve_dogma::{ModuleState, ShipFittingEngine};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::combat::death_pipeline::execute_death_pipeline_sync;
use eve_server::router::ServerContext;
use eve_server::services::pve_special_sites::{
    evaluate_abyssal_spatial_clouds_for_ship, BIOLUMINESCENT_CLOUD_TYPE_ID,
    FILAMENT_CLOUD_TYPE_ID, TACHYON_CLOUD_TYPE_ID,
};
use glam::DVec3;
use std::collections::HashMap;

async fn setup_pilot_in_space(ctx: &ServerContext, conn_id: u64, char_id: i64, ship_id: i64, ship_type_id: u32, sys_id: u32, pos: DVec3) -> SessionState {
    let mut sess = SessionState::new(conn_id);
    let uid = ctx.db.with_mut_snapshot(|s| {
        if !s.characters.contains_key(&char_id) {
            let mut base = s.characters[&90_000_001].clone();
            base.character_id = char_id;
            base.name = format!("Pilot_{char_id}");
            base.user_id = if char_id > 90_000_000 {
                (char_id - 90_000_000) as u64
            } else {
                100_000_u64 + char_id.unsigned_abs()
            };
            s.characters.insert(char_id, base);
        }
        let uid = s.characters[&char_id].user_id;
        if let Some(ch) = s.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.clone_grade = 1;
            ch.balance = IskCents::from_isk(500_000_000);
        }
        if !s.accounts.values().any(|a| a.user_id == uid) {
            let uname = format!("hf_it_user_{uid}");
            s.accounts.insert(
                uname.clone(),
                eve_db::AccountRecord {
                    user_id: uid,
                    username: uname,
                    password_token: String::new(),
                    role: 0,
                    banned_until_unix: None,
                    ban_reason: None,
                },
            );
        }
        s.items.insert(
            ship_id,
            InventoryItemRecord { item_id: ship_id, type_id: ship_type_id, owner_id: char_id, location_id: sys_id as i64, flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default() },
        );
        uid
    });
    let _ = ctx.db.acquire_online_lock(uid, conn_id, Some(char_id), true, ctx.now_unix());
    sess.user_id = Some(uid);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    let h = ctx.systems.get_or_spawn(sys_id);
    let _ = h.spawn_ball(Ball { id: ship_id, type_id: ship_type_id, position: pos, velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 0.5, mass: 10_000_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop }).await;
    let _ = h.set_combat_state(ship_id, ctx.build_type_combat_state(ship_type_id)).await;
    sess
}

fn seed_item(ctx: &ServerContext, item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32, singleton: bool) {
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(item_id, InventoryItemRecord { item_id, type_id, owner_id, location_id, flag, quantity, singleton, ..Default::default() });
        if matches!(flag as u16, 11..=34 | 92..=94 | 125..=132) {
            s.module_online_states.insert(item_id, true);
        }
    });
}

fn grant_skills(ctx: &ServerContext, char_id: i64, skills: &[(u32, u8)]) {
    ctx.db.with_mut_snapshot(|s| {
        let entry = s.skills.entry(char_id).or_default();
        for &(skill_type_id, level) in skills {
            entry.insert(skill_type_id, TrainedSkillRecord { type_id: skill_type_id, level, skill_points: 256_000.0 });
        }
    });
}

#[tokio::test]
async fn test_b42_abyssal_cache_and_spatial_clouds() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let kw = HashMap::new();
    let char_id = 90_000_001_i64;
    let ship_id = 742_000_001_i64;
    let mut sess = setup_pilot_in_space(&ctx, 892_001, char_id, ship_id, 621, 30_000_142, DVec3::new(0.0, 2.0e9, 0.0)).await;

    // 1. Bioadaptive Cache rewards & tier-scaled HP (feat:abyssal.bioadaptive-cache-rewards)
    seed_item(&ctx, 742_100_001, 56_131, char_id, ship_id, InventoryFlag::Cargo, 1, false);
    let d0 = ctx.dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(742_100_001)], &kw, None).await.expect("deploy T0");
    let t0 = d0.dict_get("traceBallID").and_then(|v| v.as_i64()).unwrap();
    let e0 = ctx.dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(t0)], &kw, None).await.expect("enter T0");
    let pid0 = e0.dict_get("pocketID").and_then(|v| v.as_i64()).unwrap();
    let sys0 = e0.dict_get("solarSystemID").and_then(|v| v.as_i64()).unwrap() as u32;
    let cache0_id = 1_896_000_000 + pid0 * 10 + 1;
    let hp_t0 = ctx.systems.get_or_spawn(sys0).get_combat_state(cache0_id).await.unwrap().expect("T0 cache").max_structure_hp;
    crate::abyssal_room_combat::assert_room_cache_loot(&ctx, char_id, 0, 1, 56_131).await;

    let char2 = 90_000_002_i64;
    let ship2 = 742_000_002_i64;
    let mut sess2 = setup_pilot_in_space(&ctx, 892_002, char2, ship2, 621, 30_002_086, DVec3::new(1.0e6, 2.0e9, 0.0)).await;
    seed_item(&ctx, 742_100_002, 47_907, char2, ship2, InventoryFlag::Cargo, 1, false);
    let d5 = ctx.dispatch_rpc(&mut sess2, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(742_100_002)], &kw, None).await.expect("deploy T5");
    let t5 = d5.dict_get("traceBallID").and_then(|v| v.as_i64()).unwrap();
    let e5 = ctx.dispatch_rpc(&mut sess2, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(t5)], &kw, None).await.expect("enter T5");
    let pid5 = e5.dict_get("pocketID").and_then(|v| v.as_i64()).unwrap();
    let sys5 = e5.dict_get("solarSystemID").and_then(|v| v.as_i64()).unwrap() as u32;
    let cache5_id = 1_896_000_000 + pid5 * 10 + 1;
    let hp_t5 = ctx.systems.get_or_spawn(sys5).get_combat_state(cache5_id).await.unwrap().expect("T5 cache").max_structure_hp;
    assert!(hp_t5 > hp_t0, "T5 cache HP ({hp_t5}) > T0 cache HP ({hp_t0})");

    let h0 = ctx.systems.get_or_spawn(sys0);
    let base_vel = h0.get_ball_snapshot(ship_id).await.unwrap().unwrap().1.max_velocity;
    let cloud_id = 742_900_001_i64;
    for (idx, tid, pos) in [
        (0, TACHYON_CLOUD_TYPE_ID, DVec3::new(100_000.0, 0.0, 0.0)),
        (1, BIOLUMINESCENT_CLOUD_TYPE_ID, DVec3::new(300_000.0, 0.0, 0.0)),
        (2, FILAMENT_CLOUD_TYPE_ID, DVec3::new(400_000.0, 0.0, 0.0)),
    ] {
        let _ = h0
            .spawn_ball(Ball {
                id: cloud_id + idx,
                type_id: tid,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 1.0,
                radius: 15_000.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
    }
    let _ = h0.set_ball_position(ship_id, DVec3::new(101_000.0, 0.0, 0.0)).await;
    assert_eq!(evaluate_abyssal_spatial_clouds_for_ship(&ctx, sys0, ship_id).await, Some("TachyonCloud"));
    let in_tachyon_vel = h0.get_ball_snapshot(ship_id).await.unwrap().unwrap().1.max_velocity;
    assert!(in_tachyon_vel > base_vel * 2.0, "Tachyon cloud boosts velocity");

    let _ = h0.set_ball_position(ship_id, DVec3::new(200_000.0, 0.0, 0.0)).await;
    assert!(evaluate_abyssal_spatial_clouds_for_ship(&ctx, sys0, ship_id).await.is_none());
    let restored_vel = h0.get_ball_snapshot(ship_id).await.unwrap().unwrap().1.max_velocity;
    assert!((restored_vel - base_vel).abs() < 1e-3, "Leaving cloud restores velocity");

    let base_cs = h0.get_combat_state(ship_id).await.unwrap().unwrap();
    let _ = h0.set_ball_position(ship_id, DVec3::new(301_000.0, 0.0, 0.0)).await;
    assert_eq!(evaluate_abyssal_spatial_clouds_for_ship(&ctx, sys0, ship_id).await, Some("BioluminescentCloud"));
    let bio_cs = h0.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(bio_cs.signature_radius > base_cs.signature_radius * 2.0, "Bioluminescent cloud inflates signature radius");

    let _ = h0.set_ball_position(ship_id, DVec3::new(401_000.0, 0.0, 0.0)).await;
    assert_eq!(evaluate_abyssal_spatial_clouds_for_ship(&ctx, sys0, ship_id).await, Some("FilamentCloud"));
    let fil_cs = h0.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!((fil_cs.shield_boost_multiplier - 0.6).abs() < 1e-3, "Filament cloud applies shield booster penalty");

    let _ = h0.set_ball_position(ship_id, DVec3::new(200_000.0, 0.0, 0.0)).await;
    assert!(evaluate_abyssal_spatial_clouds_for_ship(&ctx, sys0, ship_id).await.is_none());
    let restored_cs = h0.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!((restored_cs.signature_radius - base_cs.signature_radius).abs() < 1e-3, "Leaving cloud restores signature radius");
    assert!((restored_cs.shield_boost_multiplier - 1.0).abs() < 1e-3, "Leaving cloud restores shield boost multiplier");
}
#[tokio::test]
async fn test_b42_mining_foreman_burst_and_mutadaptive_rar() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let kw = HashMap::new();
    let sys_id = 30_000_142_u32;
    let c1 = 90_000_001_i64;
    let c2 = 90_000_002_i64;
    let c3 = 90_000_003_i64;
    let c4 = 90_000_004_i64;
    let orca_id = 742_010_001_i64;
    let hulk_near_id = 742_010_002_i64;
    let hulk_far_id = 742_010_003_i64;
    let hulk_noball_id = 742_010_004_i64;

    let mut sess1 = setup_pilot_in_space(&ctx, 892_010, c1, orca_id, 28_606, sys_id, DVec3::ZERO).await;
    let _sess2 = setup_pilot_in_space(&ctx, 892_011, c2, hulk_near_id, 22_544, sys_id, DVec3::new(5_000.0, 0.0, 0.0)).await;
    let _sess3 = setup_pilot_in_space(&ctx, 892_012, c3, hulk_far_id, 22_544, sys_id, DVec3::new(500_000.0, 0.0, 0.0)).await;
    ctx.db.with_mut_snapshot(|s| {
        let mut base = s.characters[&90_000_001].clone();
        base.character_id = c4;
        base.solar_system_id = sys_id;
        base.active_ship_id = hulk_noball_id;
        base.station_id = None;
        s.characters.insert(c4, base);
        s.fleets.insert(
            99_001,
            FleetRecord { fleet_id: 99_001, boss_id: c1, motd: String::new(), members: vec![c1, c2, c3, c4], ..Default::default() },
        );
    });

    grant_skills(&ctx, c1, &[(22536, 5), (22552, 5), (28609, 5), (11574, 5), (24764, 5), (3434, 5), (16069, 5)]);

    // 1. Mining Foreman Burst module (42528) + Mining Laser Optimization Charge (42830) via dogmaIM.Activate RPC
    let burst_mod_id = 742_010_101_i64;
    let charge_id = 742_010_102_i64;
    seed_item(&ctx, burst_mod_id, 42_528, c1, orca_id, InventoryFlag::HiSlot0, 1, true);
    let h = ctx.systems.get_or_spawn(sys_id);
    let no_charge_err = ctx
        .dispatch_rpc(&mut sess1, "dogmaIM", None, "Activate", &[PyRep::Integer(burst_mod_id), PyRep::String("commandBurst".into())], &kw, None)
        .await
        .expect_err("Unloaded Mining Foreman Burst must fail");
    assert_eq!(no_charge_err.code, "NoChargesLoaded");

    seed_item(&ctx, charge_id, 42_830, c1, orca_id, InventoryFlag::Cargo, 10, false);
    ctx.dispatch_rpc(&mut sess1, "dogmaIM", None, "LoadAmmo", &[PyRep::Integer(orca_id), PyRep::Integer(burst_mod_id), PyRep::Integer(charge_id)], &kw, None)
        .await
        .expect("Load Mining Laser Optimization Charge via LoadAmmo RPC");
    ctx.advance_secs(61);
    let res = ctx
        .dispatch_rpc(&mut sess1, "dogmaIM", None, "Activate", &[PyRep::Integer(burst_mod_id), PyRep::String("commandBurst".into())], &kw, None)
        .await
        .expect("Mining Foreman Burst activates with charge via RPC");
    let qty_after: i32 = ctx.db.with_snapshot(|s| s.items.values().filter(|i| i.location_id == orca_id && i.type_id == 42_830).map(|i| i.quantity).sum());
    assert_eq!(qty_after, 9, "Command burst consumes 1 charge per cycle");
    let buff_strength = res.dict_get("buffStrength").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert!(buff_strength != 0.0, "Mining Foreman Burst applies non-zero mining buff: {buff_strength}");
    let affected = res
        .dict_get("affectedShipIDs")
        .and_then(|v| match v {
            PyRep::List(l) => Some(l.iter().filter_map(|x| x.as_i64()).collect::<Vec<_>>()),
            _ => None,
        })
        .unwrap_or_default();
    assert!(affected.contains(&orca_id) && affected.contains(&hulk_near_id), "In-range fleet ships affected: {affected:?}");
    assert!(!affected.contains(&hulk_far_id), "Out-of-range fleet ship excluded: {affected:?}");
    assert!(!affected.contains(&hulk_noball_id), "Fleet ship without ball excluded: {affected:?}");

    // 2. Triglavian Mutadaptive Remote Armor Repairer (49770) spool-up (feat:modules.mutadaptive-remote-armor-repairer)
    let mut_rar_id = 742_010_201_i64;
    seed_item(&ctx, mut_rar_id, 49_770, c1, orca_id, InventoryFlag::HiSlot1, 1, true);
    let _ = h
        .add_target_lock(
            orca_id,
            TargetLock { target_id: hulk_near_id, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) },
        )
        .await;
    if let Ok(Some(mut cs)) = h.get_combat_state(hulk_near_id).await {
        cs.armor_hp = 100.0;
        cs.max_armor_hp = 50_000.0;
        let _ = h.set_combat_state(hulk_near_id, cs).await;
    }
    ctx.dispatch_rpc(
        &mut sess1,
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(mut_rar_id), PyRep::String("shipModuleRemoteArmorMutadaptiveRepairer".into()), PyRep::Integer(hulk_near_id), PyRep::Integer(1)],
        &kw,
        None,
    )
    .await
    .expect("Activate Mutadaptive RAR");
    for _ in 0..7 {
        h.step_manual(1.0).await.unwrap();
    }
    let armor_after_1 = h.get_combat_state(hulk_near_id).await.unwrap().unwrap().armor_hp;
    for _ in 0..7 {
        h.step_manual(1.0).await.unwrap();
    }
    let armor_after_2 = h.get_combat_state(hulk_near_id).await.unwrap().unwrap().armor_hp;
    let heal_1 = armor_after_1 - 100.0;
    let heal_2 = armor_after_2 - armor_after_1;
    assert!(heal_1 > 0.0 && heal_2 > heal_1, "Mutadaptive RAR spools up across cycles: c1={heal_1}, c2={heal_2}");
}

#[tokio::test]
async fn test_b42_overheat_rack_heat_and_damage_propagation() {
    let ctx = ServerContext::new();
    let ship_id = 742_020_001_i64;
    let mut fit_no_skill = ShipFittingEngine::from_sde(ship_id, 638, &ctx.sde);
    let mut fit_thermo5 = ShipFittingEngine::from_sde(ship_id, 638, &ctx.sde);
    for &(sid, lvl) in &[(3302, 5), (3300, 5), (11084, 5)] {
        fit_no_skill.character_skills.insert(sid, lvl);
        fit_thermo5.character_skills.insert(sid, lvl);
    }
    fit_thermo5.character_skills.insert(28_164, 5);

    for (idx, flag) in [(0, 27u16), (1, 28), (2, 29), (3, 33)] {
        let mid = 742_020_100 + idx;
        fit_no_skill.fit_module_with_state(mid, 2_889, flag, true, &ctx.sde).unwrap();
        fit_thermo5.fit_module_with_state(mid, 2_889, flag, true, &ctx.sde).unwrap();
    }

    fit_no_skill.activate_module(742_020_100, true, &ctx.sde).unwrap();
    fit_thermo5.activate_module(742_020_100, true, &ctx.sde).unwrap();
    assert_eq!(fit_no_skill.modules[&742_020_100].state, ModuleState::Overloaded);

    let r0 = fit_no_skill.apply_heat_cycle(742_020_100, 0.12, 0.01, 4.0, 0.05, &ctx.sde).unwrap();
    let r5 = fit_thermo5.apply_heat_cycle(742_020_100, 0.12, 0.01, 4.0, 0.05, &ctx.sde).unwrap();

    let expected_heat = eve_dogma::thermodynamics::step_rack_heat(0.0, 0.12, 0.01, 4.0);
    assert!((r0.rack_heat - expected_heat).abs() < 1e-6 && r0.rack_heat > 0.0 && r0.rack_heat < 1.0);

    let d_self = fit_no_skill.modules[&742_020_100].damage_hp;
    let d_adj1 = fit_no_skill.modules[&742_020_101].damage_hp;
    let d_adj2 = fit_no_skill.modules[&742_020_102].damage_hp;
    let d_edge = fit_no_skill.modules[&742_020_103].damage_hp;
    assert!(d_self > d_adj1 && d_adj1 > 0.0 && d_adj2 > 0.0, "Heat conducts to adjacent slots");
    assert!(eve_dogma::thermodynamics::heat_damage_probability(r0.rack_heat, 1, 0.5) > eve_dogma::thermodynamics::heat_damage_probability(r0.rack_heat, 2, 0.5));
    assert_eq!(d_edge, 0.0, "Heat conduction must not wrap around rack edges");
    assert!(r5.module_damage < r0.module_damage, "Thermodynamics V reduces heat damage: {} < {}", r5.module_damage, r0.module_damage);
}
#[tokio::test]
async fn test_b42_stasis_grappler_and_mjd_spool() {
    // parity: feat:modules.stasis-grappler-mechanics
    eve_testkit::attest(&[
        "feat:modules.stasis-grappler-mechanics",
    ]);
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let kw = HashMap::new();
    let sys_id = 30_000_142_u32;
    let char_id = 90_000_001_i64;
    let raven_id = 742_030_001_i64;
    let vexor_id = 742_030_002_i64;
    let target_id = 742_030_003_i64;
    let station_id = 60_003_760_i64;

    let mut sess = setup_pilot_in_space(&ctx, 892_030, char_id, raven_id, 638, sys_id, DVec3::ZERO).await;
    let mut sess_t = setup_pilot_in_space(&ctx, 892_031, 90_000_002, target_id, 626, sys_id, DVec3::new(1_000.0, 0.0, 0.0)).await;
    seed_item(&ctx, vexor_id, 626, char_id, station_id, InventoryFlag::Hangar, 1, true);
    let g1 = 742_030_101_i64;
    let g2 = 742_030_102_i64;
    seed_item(&ctx, g1, 41_057, char_id, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, g2, 41_057, char_id, station_id, InventoryFlag::Hangar, 1, true);
    grant_skills(&ctx, char_id, &[(3434, 5), (4385, 5), (3449, 5)]);
    grant_skills(&ctx, 90_000_002, &[(3434, 5)]);

    // 1. Stasis Grappler hull & max-1 restrictions, velocity reduction, and falloff (feat:modules.stasis-grappler-mechanics)
    let mut cruiser_fit = ShipFittingEngine::from_sde(vexor_id, 626, &ctx.sde);
    assert!(cruiser_fit.fit_module(g1, 41_057, 19, &ctx.sde).is_err(), "Cruiser cannot fit Heavy Stasis Grappler");
    let mut bs_fit = ShipFittingEngine::from_sde(raven_id, 638, &ctx.sde);
    bs_fit.character_skills.insert(3434, 5);
    bs_fit.character_skills.insert(3453, 5);
    bs_fit.fit_module(g1, 41_057, 19, &ctx.sde).expect("Battleship fits 1 Heavy Stasis Grappler");
    assert!(bs_fit.fit_module(g2, 41_057, 20, &ctx.sde).is_err(), "Max 1 Stasis Grappler per ship");

    let h = ctx.systems.get_or_spawn(sys_id);
    seed_item(&ctx, g1, 41_057, char_id, raven_id, InventoryFlag::MedSlot0, 1, true);
    let _ = h
        .add_target_lock(
            raven_id,
            TargetLock { target_id, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) },
        )
        .await;
    let base_target_vel = h.get_ball_snapshot(target_id).await.unwrap().unwrap().1.max_velocity;
    let _ = h.set_ball_position(target_id, DVec3::new(500.0, 0.0, 0.0)).await;
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(g1), PyRep::String("decreaseTargetSpeed".into()), PyRep::Integer(target_id)], &kw, None)
        .await
        .expect("Activate Heavy Stasis Grappler at close range");
    let vel_close = h.get_ball_snapshot(target_id).await.unwrap().unwrap().1.max_velocity;
    assert!(vel_close < base_target_vel * 0.25, "Stasis Grappler reduces target velocity by ~85% at optimal: {vel_close} vs {base_target_vel}");

    let _ = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(g1), PyRep::String("decreaseTargetSpeed".into())], &kw, None).await;
    let _ = h.set_ball_position(target_id, DVec3::new(1_000.0, 0.0, 0.0)).await;
    let _ = h.set_ball_position(target_id, DVec3::new(15_000.0, 0.0, 0.0)).await;
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(g1), PyRep::String("decreaseTargetSpeed".into()), PyRep::Integer(target_id)], &kw, None)
        .await
        .expect("Activate Heavy Stasis Grappler in falloff");
    let vel_falloff = h.get_ball_snapshot(target_id).await.unwrap().unwrap().1.max_velocity;
    assert!(vel_falloff > vel_close && vel_falloff < base_target_vel, "Stasis Grappler decays in falloff: close={vel_close}, falloff={vel_falloff}, base={base_target_vel}");
    let _ = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(g1), PyRep::String("decreaseTargetSpeed".into())], &kw, None).await;
    let _ = h.set_ball_position(target_id, DVec3::new(1_000.0, 0.0, 0.0)).await;

    // 2. Micro Jump Drive 12s spool, scram cancellation, and 100 km vector jump (feat:navigation.micro-jump-drive-spool)
    let mjd_id = 742_030_201_i64;
    let scram_id = 742_030_202_i64;
    seed_item(&ctx, mjd_id, 4_383, char_id, raven_id, InventoryFlag::MedSlot2, 1, true);
    seed_item(&ctx, scram_id, 448, 90_000_002, target_id, InventoryFlag::MedSlot0, 1, true);
    let _ = h
        .add_target_lock(
            target_id,
            TargetLock { target_id: raven_id, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) },
        )
        .await;
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(mjd_id), PyRep::String("microJumpDrive".into())], &kw, None)
        .await
        .expect("Activate MJD");
    ctx.dispatch_rpc(&mut sess_t, "dogmaIM", None, "Activate", &[PyRep::Integer(scram_id), PyRep::String("warpScramble".into()), PyRep::Integer(raven_id)], &kw, None)
        .await
        .expect("Activate Warp Scrambler");
    for _ in 0..13 {
        h.step_manual(1.0).await.unwrap();
    }
    let pos_after_scram = h.get_ball_snapshot(raven_id).await.unwrap().unwrap().1.position;
    assert!(pos_after_scram.length() < 1_000.0, "Scram during spool cancels MJD jump even past 12s: {pos_after_scram:?}");

    let _ = ctx.dispatch_rpc(&mut sess_t, "dogmaIM", None, "Deactivate", &[PyRep::Integer(scram_id), PyRep::String("warpScramble".into())], &kw, None).await;
    ctx.advance_secs(200);
    h.step_manual(1.0).await.unwrap();
    let start_pos = h.get_ball_snapshot(raven_id).await.unwrap().unwrap().1.position;
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(mjd_id), PyRep::String("microJumpDrive".into())], &kw, None)
        .await
        .expect("Activate MJD unscrambled");
    for _ in 0..11 {
        h.step_manual(1.0).await.unwrap();
    }
    let mid_pos = h.get_ball_snapshot(raven_id).await.unwrap().unwrap().1.position;
    assert!((mid_pos - start_pos).length() < 5_000.0, "MJD must not jump before 12s spool completes");
    for _ in 0..2 {
        h.step_manual(1.0).await.unwrap();
    }
    let end_pos = h.get_ball_snapshot(raven_id).await.unwrap().unwrap().1.position;
    assert!((end_pos - start_pos).length() >= 95_000.0, "MJD jumps ~100 km forward at 12s: start={start_pos:?}, end={end_pos:?}");
}
#[tokio::test]
async fn test_b42_warp_landing_bubbles_and_scram_shutoff() {
    // parity: feat:navigation.warp-scramble-propulsion-shutoff
    eve_testkit::attest(&[
        "feat:navigation.warp-scramble-propulsion-shutoff",
    ]);
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let kw = HashMap::new();
    let sys_id = 30_000_142_u32;
    let char_id = 90_000_001_i64;
    let ship_id = 742_040_001_i64;
    let attacker_id = 742_040_002_i64;

    let mut sess = setup_pilot_in_space(&ctx, 892_040, char_id, ship_id, 621, sys_id, DVec3::ZERO).await;
    let mut sess_att = setup_pilot_in_space(&ctx, 892_041, 90_000_002, attacker_id, 621, sys_id, DVec3::new(5_000.0, 0.0, 0.0)).await;
    grant_skills(&ctx, char_id, &[(3450, 5), (3449, 5)]);
    grant_skills(&ctx, 90_000_002, &[(3434, 5)]);
    let h = ctx.systems.get_or_spawn(sys_id);

    // 1. Minimum 150 km warp distance & landing offset (feat:navigation.warp-landing-points)
    let st_pos = ctx.sde.stations.get(&60_003_760).map(|s| s.position).unwrap_or(DVec3::new(1.0e7, 0.0, 0.0));
    let _ = h.set_ball_position(ship_id, st_pos + DVec3::new(50_000.0, 0.0, 0.0)).await;
    let beyonce = ctx.bind_object(&mut sess, "beyonce", sys_id as i64).unwrap();
    let short_err = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(beyonce),
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(60_003_760)],
            &kw,
            None,
        )
        .await
        .expect_err("Warp < 150 km must be refused");
    assert_eq!(short_err.code, "WarpDistanceTooShort");

    let _ = h.set_ball_position(ship_id, st_pos + DVec3::new(1_000_000.0, 0.0, 0.0)).await;
    let mut warp_kw = HashMap::new();
    warp_kw.insert("minRange".to_string(), PyRep::Real(50_000.0));
    let _ = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(beyonce),
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(60_003_760)],
            &warp_kw,
            None,
        )
        .await
        .expect("Warp >= 150 km with 50 km offset succeeds");

    // 2. Drag bubble (within 500 km behind dest) & Catch bubble (in front) (feat:navigation.warp-bubble-drag-and-catch)
    let origin = DVec3::ZERO;
    let dest = DVec3::new(1_000_000.0, 0.0, 0.0);
    let catch_bubble = WarpBubble { id: 1, position: DVec3::new(600_000.0, 0.0, 0.0), radius: 20_000.0 };
    let catch_pt = catch_bubble.intercept_warp(origin, dest).expect("Catch bubble intercepts warp in front");
    assert!((catch_pt.x - 580_000.0).abs() < 1e-3);

    let drag_bubble = WarpBubble { id: 2, position: DVec3::new(1_300_000.0, 0.0, 0.0), radius: 20_000.0 };
    let drag_pt = drag_bubble.intercept_warp(origin, dest).expect("Drag bubble within 500 km behind dest drags warp");
    assert!((drag_pt.x - 1_280_000.0).abs() < 1e-3);

    let far_bubble = WarpBubble { id: 3, position: DVec3::new(1_600_000.0, 0.0, 0.0), radius: 20_000.0 };
    assert!(far_bubble.intercept_warp(origin, dest).is_none(), "Bubble > 500 km behind dest does not drag");

    // 3. Warp Scrambler shuts off MWD vs Warp Disruptor leaves MWD running (feat:navigation.warp-scramble-propulsion-shutoff)
    let _ = h.set_ball_position(ship_id, DVec3::ZERO).await;
    let mwd_id = 742_040_101_i64;
    let point_id = 742_040_102_i64;
    let scram_id = 742_040_103_i64;
    seed_item(&ctx, mwd_id, 434, char_id, ship_id, InventoryFlag::MedSlot0, 1, true);
    seed_item(&ctx, point_id, 3_242, 90_000_002, attacker_id, InventoryFlag::MedSlot0, 1, true);
    seed_item(&ctx, scram_id, 448, 90_000_002, attacker_id, InventoryFlag::MedSlot1, 1, true);
    let _ = h
        .add_target_lock(
            attacker_id,
            TargetLock { target_id: ship_id, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) },
        )
        .await;
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(mwd_id), PyRep::String("moduleBonusMicrowarpdrive".into())], &kw, None)
        .await
        .expect("Activate MWD");
    ctx.dispatch_rpc(&mut sess_att, "dogmaIM", None, "Activate", &[PyRep::Integer(point_id), PyRep::String("warpDisrupt".into()), PyRep::Integer(ship_id)], &kw, None)
        .await
        .expect("Activate Warp Disruptor");
    h.step_manual(1.0).await.unwrap();
    let cs_disrupted = h.get_combat_state(ship_id).await.unwrap().unwrap();
    assert_eq!(cs_disrupted.warp_scramble_strength, 1);
    assert!(!cs_disrupted.is_mwd_mjd_disabled, "Long point must NOT disable MWD");

    ctx.dispatch_rpc(&mut sess_att, "dogmaIM", None, "Activate", &[PyRep::Integer(scram_id), PyRep::String("warpScramble".into()), PyRep::Integer(ship_id)], &kw, None)
        .await
        .expect("Activate Warp Scrambler");
    h.step_manual(1.0).await.unwrap();
    let cs_scrambled = h.get_combat_state(ship_id).await.unwrap().unwrap();
    assert_eq!(cs_scrambled.warp_scramble_strength, 3);
    assert!(cs_scrambled.is_mwd_mjd_disabled, "Warp Scrambler must disable MWD");
    let post_scram_vel = h.get_ball_snapshot(ship_id).await.unwrap().unwrap().1.max_velocity;
    assert!(post_scram_vel < 350.0, "Warp Scrambler shuts off active MWD cycle and restores base velocity: {post_scram_vel}");
    let reactivate_err = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(mwd_id), PyRep::String("moduleBonusMicrowarpdrive".into())], &kw, None)
        .await;
    assert!(reactivate_err.is_err(), "MWD reactivation must be refused while scrambled");
}

#[tokio::test]
async fn test_b42_zarzakh_and_ship_mechanics() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let kw = HashMap::new();
    let char_id = 90_000_001_i64;
    let ship_id = 742_050_001_i64;
    let turnur_sys = 30_002_086_u32;
    let zarzakh_sys = 30_100_000_u32;
    let turnur_gate = 50_016_562_i64;
    let alsa_exit_gate = 50_016_567_i64;

    let turnur_pos = ctx.sde.jumps.get(&turnur_gate).map(|j| j.position).unwrap_or(DVec3::ZERO);
    let mut sess = setup_pilot_in_space(&ctx, 892_050, char_id, ship_id, 621, turnur_sys, turnur_pos + DVec3::new(100.0, 0.0, 0.0)).await;

    // 1. Zarzakh gate toll & emanation lock (feat:navigation.zarzakh-gate-toll, feat:navigation.zarzakh-emanation-lock)
    let beyonce_t = ctx.bind_object(&mut sess, "beyonce", turnur_sys as i64).unwrap();
    let mut decline_kw = HashMap::new();
    decline_kw.insert("confirm_toll".to_string(), PyRep::Bool(false));
    let _ = ctx
        .dispatch_rpc(&mut sess, "beyonce", Some(beyonce_t), "CmdStargateJump", &[PyRep::Integer(turnur_gate)], &decline_kw, None)
        .await
        .expect_err("Declining Zarzakh toll must abort jump");

    let bal_before = ctx.db.with_snapshot(|s| s.characters[&char_id].balance);
    let mut confirm_kw = HashMap::new();
    confirm_kw.insert("confirm_toll".to_string(), PyRep::Bool(true));
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_t), "CmdStargateJump", &[PyRep::Integer(turnur_gate)], &confirm_kw, None)
        .await
        .expect("Confirmed Zarzakh jump succeeds");
    let (bal_after, has_journal, lock_entry) = ctx.db.with_snapshot(|s| {
        (
            s.characters[&char_id].balance,
            s.wallet_journal.iter().any(|j| j.character_id == char_id && j.entry_type == "zarzakh_gate_toll"),
            s.zarzakh_gate_locks.get(&char_id).copied(),
        )
    });
    assert!(bal_after < bal_before && has_journal, "Zarzakh toll debited and journaled");
    assert!(lock_entry.is_some(), "Zarzakh emanation lock recorded on entry");

    let alsa_pos = ctx.sde.jumps.get(&alsa_exit_gate).map(|j| j.position).unwrap_or(DVec3::ZERO);
    let _ = ctx.systems.get_or_spawn(zarzakh_sys).set_ball_position(ship_id, alsa_pos + DVec3::new(100.0, 0.0, 0.0)).await;
    let beyonce_z = ctx.bind_object(&mut sess, "beyonce", zarzakh_sys as i64).unwrap();
    let _ = ctx
        .dispatch_rpc(&mut sess, "beyonce", Some(beyonce_z), "CmdStargateJump", &[PyRep::Integer(alsa_exit_gate)], &kw, None)
        .await
        .expect_err("Different Zarzakh exit gate refused while locked");

    let dest_station = 60_003_760_i64;
    ctx.db.with_mut_snapshot(|s| {
        s.jump_clones.entry(char_id).or_default().push(JumpCloneRecord {
            jump_clone_id: 742_999,
            character_id: char_id,
            location_id: dest_station,
            implants: Vec::new(),
        });
    });
    grant_skills(&ctx, char_id, &[(24_242, 3)]);
    let pod_id = 742_050_099_i64;
    seed_item(&ctx, pod_id, 670, char_id, dest_station, InventoryFlag::Hangar, 1, true);
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&char_id) {
            ch.station_id = Some(dest_station);
            ch.active_ship_id = pod_id;
        }
    });
    sess.station_id = Some(dest_station);
    sess.ship_id = Some(pod_id);
    ctx.advance_secs(15);
    let _ = ctx
        .dispatch_rpc(&mut sess, "jumpCloneSvc", None, "CloneJump", &[PyRep::Integer(742_999)], &kw, None)
        .await
        .expect("CloneJump");
    assert!(ctx.db.with_snapshot(|s| !s.zarzakh_gate_locks.contains_key(&char_id)), "CloneJump clears Zarzakh emanation lock");
    ctx.db.with_mut_snapshot(|s| {
        s.zarzakh_gate_locks.insert(char_id, (50_016_566, ctx.now_unix_secs() + 21_600));
    });
    let _ = execute_death_pipeline_sync(&ctx, zarzakh_sys, char_id, pod_id, 90_000_002, DVec3::ZERO, 1);
    assert!(ctx.db.with_snapshot(|s| !s.zarzakh_gate_locks.contains_key(&char_id)), "Pod death clears Zarzakh emanation lock");

    // 2. Blockade Runner cargo scan immunity (feat:ships.blockade-runner-cargo-scan-immunity)
    let scanner_ship = 742_050_010_i64;
    let br_ship = 742_050_011_i64;
    let scanner_mod = 742_050_012_i64;
    let mut sess_scan = setup_pilot_in_space(&ctx, 892_051, char_id, scanner_ship, 621, 30_000_142, DVec3::ZERO).await;
    let _ = setup_pilot_in_space(&ctx, 892_052, 90_000_002, br_ship, 12_733, 30_000_142, DVec3::new(1_000.0, 0.0, 0.0)).await;
    seed_item(&ctx, scanner_mod, 442, char_id, scanner_ship, InventoryFlag::MedSlot0, 1, true);
    seed_item(&ctx, 742_050_013, 34, 90_000_002, br_ship, InventoryFlag::Cargo, 500, false);
    let jita_h = ctx.systems.get_or_spawn(30_000_142);
    let _ = jita_h
        .add_target_lock(
            scanner_ship,
            TargetLock { target_id: br_ship, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) },
        )
        .await;
    let scan_res = ctx
        .dispatch_rpc(&mut sess_scan, "dogmaIM", None, "Activate", &[PyRep::Integer(scanner_mod), PyRep::String("cargoScan".into()), PyRep::Integer(br_ship)], &kw, None)
        .await
        .expect("Cargo scanner activates on Blockade Runner");
    assert_eq!(scan_res.dict_get("cargoScanImmune").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(scan_res.dict_get("totalScannedQuantity").and_then(|v| v.as_i64()), Some(0));

    // 3. Covert Cyno & Normal Cyno beacon spawning (feat:ships.covert-cyno-beacon, feat:ships.cynosural-beacon-spawning)
    let recon_ship = 742_050_020_i64;
    let covert_cyno_mod = 742_050_021_i64;
    let mut sess_cov = setup_pilot_in_space(&ctx, 892_053, char_id, recon_ship, 11_957, turnur_sys, DVec3::new(50_000.0, 0.0, 0.0)).await;
    seed_item(&ctx, covert_cyno_mod, 28_646, char_id, recon_ship, InventoryFlag::HiSlot0, 1, true);
    seed_item(&ctx, 742_050_023, 16_273, char_id, recon_ship, InventoryFlag::Cargo, 500, false);
    grant_skills(&ctx, char_id, &[(21_603, 5), (22_761, 5), (24_242, 3)]);
    ctx.db.with_mut_snapshot(|s| {
        s.fleets.insert(99_002, FleetRecord { fleet_id: 99_002, boss_id: char_id, members: vec![char_id], ..Default::default() });
    });
    let _ = ctx
        .dispatch_rpc(&mut sess_cov, "dogmaIM", None, "Activate", &[PyRep::Integer(covert_cyno_mod), PyRep::String("cynosuralGeneration".into())], &kw, None)
        .await
        .expect("Covert cyno activates without consuming ozone");
    let ozone_after_covert: i32 = ctx.db.with_snapshot(|s| {
        s.items.values().filter(|i| i.location_id == recon_ship && i.type_id == 16_273).map(|i| i.quantity).sum()
    });
    assert_eq!(ozone_after_covert, 495, "Covert cyno on Force Recon consumes 5 Liquid Ozone (50 base * 0.2 role * 0.5 CFT V)");

    let recon_ship2 = 742_050_025_i64;
    let normal_cyno_mod = 742_050_022_i64;
    let mut sess_norm = setup_pilot_in_space(&ctx, 892_054, 90_000_003, recon_ship2, 11_957, turnur_sys, DVec3::new(60_000.0, 0.0, 0.0)).await;
    seed_item(&ctx, normal_cyno_mod, 21_096, 90_000_003, recon_ship2, InventoryFlag::HiSlot0, 1, true);
    seed_item(&ctx, 742_050_026, 16_273, 90_000_003, recon_ship2, InventoryFlag::Cargo, 500, false);
    grant_skills(&ctx, 90_000_003, &[(21_603, 5), (22_761, 5)]);
    ctx.db.with_mut_snapshot(|s| {
        s.fleets.insert(99_003, FleetRecord { fleet_id: 99_003, boss_id: 90_000_003, members: vec![90_000_003], ..Default::default() });
    });
    let _ = ctx
        .dispatch_rpc(&mut sess_norm, "dogmaIM", None, "Activate", &[PyRep::Integer(normal_cyno_mod), PyRep::String("cynosuralGeneration".into())], &kw, None)
        .await
        .expect("Normal cyno activates");
    let ozone_after_normal: i32 = ctx.db.with_snapshot(|s| {
        s.items.values().filter(|i| i.location_id == recon_ship2 && i.type_id == 16_273).map(|i| i.quantity).sum()
    });
    assert!(ozone_after_normal < 500, "Normal cyno consumes Liquid Ozone: {ozone_after_normal} < 500");

    // 4. Frigate Escape Bay on Battleship destruction (feat:ships.frigate-escape-bay)
    let bs_id = 742_050_030_i64;
    let esc_frig_id = 742_050_031_i64;
    let _ = setup_pilot_in_space(&ctx, 892_055, 90_000_004, bs_id, 638, turnur_sys, DVec3::new(80_000.0, 0.0, 0.0)).await;
    seed_item(&ctx, esc_frig_id, 587, 90_000_004, bs_id, InventoryFlag::from_u16(179), 1, true);
    let death_out = execute_death_pipeline_sync(&ctx, turnur_sys, 90_000_004, bs_id, char_id, DVec3::new(80_000.0, 0.0, 0.0), 1)
        .expect("BS death pipeline");
    assert_eq!(death_out.ejected_capsule_id, Some(esc_frig_id), "Pilot placed into stored escape frigate instead of capsule");
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.characters[&90_000_004].active_ship_id, esc_frig_id, "Active ship is now the escape frigate");
        assert_eq!(s.items[&esc_frig_id].location_id, turnur_sys as i64, "Escape frigate is in space, not in wreck");
    });
}

mod homefront_operations_it {
    use super::*;
    use eve_db::service_domains::pve_sites::HomefrontProgressRecord;
    use eve_db::IskCents;
    use eve_server::combat::drones_salvage_tractor::execute_wreck_salvage_cycle;
    use eve_server::services::pve_homefront_sites::{
        abyssal_clear_wave, activate_homefront_acceleration_gate,
        complete_homefront_operation_and_pay, dread_assault_advance_siege,
        dread_assault_transfer_capacitor, emergency_aid_advance_defense,
        get_homefront_runtime_state, metaliminal_mine_mutanite, raid_deliver_cargo_to_freighter,
        raid_tackle_and_destroy_hauler, record_homefront_combat_and_logistics,
        salvage_research_process_wreck, spawn_homefront_site_for_dungeon,
        stabilize_rift_neutralize_arrays, suspicious_signal_attack_relay,
        suspicious_signal_hack_array, traffic_stop_scan_and_intercept, HomefrontArchetypeProgress,
    };
    use eve_server::services::pve_special_sites_ops::{
        is_homefront_exordium_system, resolve_homefront_archetype_blueprint, HomefrontArchetype,
    };

    fn clone_demo_char_with_id(ctx: &ServerContext, char_id: i64, user_id: u64, sys_id: u32) {
        ctx.db.with_mut_snapshot(|s| {
            let mut template = s.characters[&90_000_001].clone();
            template.character_id = char_id;
            template.user_id = user_id;
            template.name = format!("HF_IT_{char_id}");
            template.solar_system_id = sys_id;
            template.balance = IskCents::from_isk(1_000_000);
            s.wallet_journal.retain(|j| j.character_id != char_id);
            s.characters.insert(char_id, template);
        });
    }

    async fn position_ship_at(ctx: &ServerContext, sys_id: u32, ship_id: i64, pos: DVec3) {
        let h = ctx.systems.get_or_spawn(sys_id);
        let _ = h.set_ball_position(ship_id, pos).await;
    }

    // parity: feat:pve.homefront-ops
    // parity: rpc:dungeonInstanceCacheMgr.GetHomefrontSiteInstances
    #[tokio::test]
    async fn test_homefront_operations_multi_role_archetypes_and_gate_restrictions() {
        let ctx = ServerContext::new();
        common::pin_clock(&ctx);
        let kw = HashMap::new();
        let jita: u32 = 30_000_142;

        // Seed 6 distinct pilots in Jita representing each canonical Homefront fleet role with real online modules
        let mut sessions = Vec::new();
        let roster: &[(i64, i64, u32, &[(InventoryFlag, u32)])] = &[
            (
                940_001_i64,
                743_000_001_i64,
                621_u32,
                &[(InventoryFlag::MedSlot0, 22_175)], // Caracal + Data Analyzer I (no mining laser)
            ),
            (
                940_002_i64,
                743_000_002_i64,
                16_236_u32,
                &[
                    (InventoryFlag::HiSlot0, 25_861),  // Salvager I
                    (InventoryFlag::HiSlot1, 533),     // Small Energy Neutralizer I
                    (InventoryFlag::MedSlot0, 22_175), // Data Analyzer I
                    (InventoryFlag::MedSlot1, 442),    // Cargo Scanner I
                ],
            ),
            (
                940_003_i64,
                743_000_003_i64,
                597_u32,
                &[
                    (InventoryFlag::HiSlot0, 3586), // Small Remote Shield Booster I
                    (InventoryFlag::HiSlot1, 529),  // Small Remote Capacitor Transmitter I
                ],
            ),
            (
                940_004_i64,
                743_000_004_i64,
                32_880_u32,
                &[
                    (InventoryFlag::HiSlot0, 483),     // Miner I
                    (InventoryFlag::MedSlot0, 22_175), // Data Analyzer I
                ],
            ),
            (940_005_i64, 743_000_005_i64, 649_u32, &[]), // Tayra (7,300 m3 >= 3,500 m3)
            (
                940_006_i64,
                743_000_006_i64,
                621_u32,
                &[(InventoryFlag::HiSlot0, 483)], // Caracal + fitted Miner I
            ),
        ];
        for (idx, &(cid, sid, stid, mods)) in roster.iter().enumerate() {
            clone_demo_char_with_id(&ctx, cid, 94_000 + idx as u64, jita);
            let sess = setup_pilot_in_space(
                &ctx,
                893_000 + idx as u64,
                cid,
                sid,
                stid,
                jita,
                DVec3::new((idx as f64) * 5_000.0, 1.0e9, 0.0),
            )
            .await;
            for (slot_idx, &(flag, mod_tid)) in mods.iter().enumerate() {
                seed_item(
                    &ctx,
                    743_100_000 + (idx as i64) * 10 + slot_idx as i64,
                    mod_tid,
                    cid,
                    sid,
                    flag,
                    1,
                    true,
                );
            }
            sessions.push(sess);
        }
        ctx.db.with_mut_snapshot(|s| {
            if let Some(ch) = s.characters.get_mut(&940_002) {
                ch.clone_grade = 2; // Omega clone required for Salvaging V (Alpha caps at III)
            }
        });
        grant_skills(&ctx, 940_002, &[(25_863, 5)]); // Salvaging V (>= 20% bonus with Salvager I)

        // Verify GetHomefrontSiteInstances RPC exposes Jita Homefront sites with canonical payouts
        let rpc_rep = ctx
            .dispatch_rpc(
                &mut sessions[0],
                "dungeonInstanceCacheMgr",
                None,
                "GetHomefrontSiteInstances",
                &[PyRep::Integer(i64::from(jita))],
                &kw,
                None,
            )
            .await
            .expect("GetHomefrontSiteInstances RPC");
        assert!(matches!(rpc_rep, PyRep::Dict(ref d) if !d.is_empty()));

        // Gate distance enforcement (>2,500m rejected) + ship restrictions (Cerberus, Drake, Raven, Vedmak rejected)
        let raid = spawn_homefront_site_for_dungeon(&ctx, jita, 10_347).expect("spawn Raid");
        let raid_rt = get_homefront_runtime_state(&ctx, raid.instance_id).expect("Raid runtime");
        assert!(
            !raid_rt.pocket_spawned,
            "Raid pocket entities must stay unspawned behind outer acceleration gate before entry"
        );
        position_ship_at(
            &ctx,
            jita,
            743_000_001,
            raid.site_position + DVec3::new(10_000.0, 0.0, 0.0),
        )
        .await;
        assert_eq!(
            activate_homefront_acceleration_gate(&ctx, &sessions[0], raid.instance_id)
                .await
                .expect_err("ship >2,500m from outer gate must fail range check")
                .code,
            "OutsideGateActivationRange"
        );

        for (idx, bad_tid) in [11_993_u32, 24_698, 638, 47_270].iter().enumerate() {
            let cid = 940_100 + idx as i64;
            let sid = 743_200_000 + idx as i64;
            clone_demo_char_with_id(&ctx, cid, 94_100 + idx as u64, jita);
            let bad_sess = setup_pilot_in_space(
                &ctx,
                893_100 + idx as u64,
                cid,
                sid,
                *bad_tid,
                jita,
                raid.site_position + DVec3::new(350.0, 0.0, 0.0),
            )
            .await;
            assert_eq!(
                activate_homefront_acceleration_gate(&ctx, &bad_sess, raid.instance_id)
                    .await
                    .expect_err("restricted ship must fail gate")
                    .code,
                "ShipRestrictedByGate"
            );
        }

        // 1. Complete Raid (10347) via multi-role actions (tackle hauler, Caracal hold check, Tayra delivery)
        for (idx, s) in sessions[..5].iter().enumerate() {
            let sid = 743_000_001 + idx as i64;
            position_ship_at(&ctx, jita, sid, raid.site_position + DVec3::new(350.0, 0.0, 0.0)).await;
            activate_homefront_acceleration_gate(&ctx, s, raid.instance_id)
                .await
                .expect("5-pilot eligible ships pass Raid gate");
        }
        assert_eq!(
            raid_tackle_and_destroy_hauler(&ctx, &sessions[0], raid.instance_id, false, 2_500.0)
                .expect_err("untackled Raid hauler escapes")
                .code,
            "TargetEscaped"
        );
        for _ in 0..4 {
            raid_tackle_and_destroy_hauler(&ctx, &sessions[0], raid.instance_id, true, 2_500.0)
                .expect("destroy tackled Raid hauler");
        }
        assert_eq!(
            raid_deliver_cargo_to_freighter(&ctx, &sessions[0], raid.instance_id)
                .expect_err("Caracal hold < 3500 m3 rejected")
                .code,
            "CargoCapacityTooSmall"
        );
        for _ in 0..4 {
            raid_deliver_cargo_to_freighter(&ctx, &sessions[4], raid.instance_id)
                .expect("Tayra >= 3500 m3 delivers Raid cargo");
        }
        record_homefront_combat_and_logistics(&ctx, &sessions[1], raid.instance_id, 2_000.0, 0.0).unwrap();
        record_homefront_combat_and_logistics(&ctx, &sessions[2], raid.instance_id, 0.0, 2_000.0).unwrap();
        record_homefront_combat_and_logistics(&ctx, &sessions[3], raid.instance_id, 1_500.0, 0.0).unwrap();
        let raid_pay = complete_homefront_operation_and_pay(&ctx, &sessions[0], raid.instance_id)
            .await
            .expect("settle Raid");
        assert_eq!(raid_pay.eligible_pilot_count, 5);
        assert_eq!(raid_pay.total_fleet_isk, 75_000_000);

        // 2. Emergency Aid (10238): sustain protectee for 600s -> solo opt=5 payout (4/5)^4 * 15M = 6,144,000
        let aid = spawn_homefront_site_for_dungeon(&ctx, jita, 10_238).unwrap();
        position_ship_at(&ctx, jita, 743_000_003, aid.site_position + DVec3::new(350.0, 0.0, 0.0)).await;
        activate_homefront_acceleration_gate(&ctx, &sessions[2], aid.instance_id).await.unwrap();
        emergency_aid_advance_defense(&ctx, &sessions[2], aid.instance_id, 5_000.0, 4_000.0, 600).unwrap();
        assert_eq!(
            complete_homefront_operation_and_pay(&ctx, &sessions[2], aid.instance_id)
                .await
                .unwrap()
                .total_fleet_isk,
            6_144_000
        );

        // 3. Metaliminal Meteoroid (10312): unfitted Caracal rejected, Venture + Miner-fitted Caracal clear 5,000 units -> 2 * (4/5)^3 * 12M = 12,288,000
        let met = spawn_homefront_site_for_dungeon(&ctx, jita, 10_312).unwrap();
        for (sid, sess) in [
            (743_000_001_i64, &sessions[0]),
            (743_000_004_i64, &sessions[3]),
            (743_000_006_i64, &sessions[5]),
        ] {
            position_ship_at(&ctx, jita, sid, met.site_position + DVec3::new(350.0, 0.0, 0.0)).await;
            activate_homefront_acceleration_gate(&ctx, sess, met.instance_id).await.unwrap();
        }
        assert_eq!(
            metaliminal_mine_mutanite(&ctx, &sessions[0], met.instance_id, 500, 0)
                .expect_err("unfitted Caracal cannot mine Mutanite")
                .code,
            "MiningModuleRequired"
        );
        metaliminal_mine_mutanite(&ctx, &sessions[3], met.instance_id, 3_000, 0).unwrap();
        metaliminal_mine_mutanite(&ctx, &sessions[5], met.instance_id, 2_000, 0).unwrap();
        let met_pay = complete_homefront_operation_and_pay(&ctx, &sessions[3], met.instance_id).await.unwrap();
        assert_eq!(met_pay.total_fleet_isk, 12_288_000);

        // 4. Dread Assault (10320): 3 cycles x 30k GJ cap -> 90s Siege volleys -> HQ destroyed (solo opt=5 = 6,144,000)
        let dread = spawn_homefront_site_for_dungeon(&ctx, jita, 10_320).unwrap();
        position_ship_at(&ctx, jita, 743_000_003, dread.site_position + DVec3::new(350.0, 0.0, 0.0)).await;
        activate_homefront_acceleration_gate(&ctx, &sessions[2], dread.instance_id).await.unwrap();
        for _ in 0..3 {
            dread_assault_transfer_capacitor(&ctx, &sessions[2], dread.instance_id, 30_000.0, 0.0).unwrap();
            dread_assault_advance_siege(&ctx, &sessions[2], dread.instance_id, 90).unwrap();
        }
        assert_eq!(
            complete_homefront_operation_and_pay(&ctx, &sessions[2], dread.instance_id)
                .await
                .unwrap()
                .total_fleet_isk,
            6_144_000
        );

        // 5. Abyssal Artifact Recovery (10346): 9 waves + warp bubble control + solo (4/5)^4 * 67.5M = 27,648,000
        let aar = spawn_homefront_site_for_dungeon(&ctx, jita, 10_346).unwrap();
        position_ship_at(&ctx, jita, 743_000_004, aar.site_position + DVec3::new(350.0, 0.0, 0.0)).await;
        activate_homefront_acceleration_gate(&ctx, &sessions[3], aar.instance_id).await.unwrap();
        let aar_cid = sessions[3].character_id.unwrap();
        let (aar_bal_before, aar_j_before) = ctx.db.with_snapshot(|snap| {
            let j_cnt = snap
                .wallet_journal
                .iter()
                .filter(|j| j.character_id == aar_cid && j.entry_type == "CorporateRewardPayout")
                .count();
            (snap.characters[&aar_cid].balance, j_cnt)
        });
        let _ = ctx.drain_notifications_for_char(aar_cid);
        for _ in 1..=9 {
            abyssal_clear_wave(&ctx, &sessions[3], aar.instance_id, 324, 1_500.0, true).await.unwrap();
        }
        let (aar_bal_pre_settle, aar_j_pre_settle) = ctx.db.with_snapshot(|snap| {
            let new_rows: Vec<_> = snap
                .wallet_journal
                .iter()
                .filter(|j| j.character_id == aar_cid && j.entry_type == "CorporateRewardPayout")
                .skip(aar_j_before)
                .collect();
            assert_eq!(new_rows.len(), 9, "9 per-wave AAR payouts must appear before settlement");
            assert!(new_rows.iter().all(|j| j.amount > IskCents(0)));
            (snap.characters[&aar_cid].balance, new_rows.len())
        });
        assert_eq!(
            aar_bal_pre_settle,
            aar_bal_before.checked_add(IskCents::from_isk(27_648_000)).unwrap()
        );
        assert!(ctx.drain_notifications_for_char(aar_cid).iter().any(|n| n.event == "OnAccountChange"));
        assert_eq!(
            complete_homefront_operation_and_pay(&ctx, &sessions[3], aar.instance_id)
                .await
                .unwrap()
                .total_fleet_isk,
            27_648_000
        );
        ctx.db.with_snapshot(|snap| {
            assert_eq!(snap.characters[&aar_cid].balance, aar_bal_pre_settle);
            let j_after = snap
                .wallet_journal
                .iter()
                .filter(|j| j.character_id == aar_cid && j.entry_type == "CorporateRewardPayout")
                .count();
            assert_eq!(j_after, aar_j_before + aar_j_pre_settle);
        });
        assert!(!ctx.drain_notifications_for_char(aar_cid).iter().any(|n| n.event == "OnAccountChange"));

        // 6. Suspicious Signal (10309): premature relay DPS blocked until 3 arrays hacked (solo opt=5 = 6,144,000)
        let sig = spawn_homefront_site_for_dungeon(&ctx, jita, 10_309).unwrap();
        position_ship_at(&ctx, jita, 743_000_001, sig.site_position + DVec3::new(350.0, 0.0, 0.0)).await;
        activate_homefront_acceleration_gate(&ctx, &sessions[0], sig.instance_id).await.unwrap();
        assert_eq!(
            suspicious_signal_attack_relay(&ctx, &sessions[0], sig.instance_id, 350_000.0, 10)
                .expect_err("relay invulnerable until 3 arrays hacked")
                .code,
            "StructureInvulnerable"
        );
        for idx in 0..3 {
            suspicious_signal_hack_array(&ctx, &sessions[0], sig.instance_id, idx).unwrap();
        }
        let sig_done = suspicious_signal_attack_relay(&ctx, &sessions[0], sig.instance_id, 350_000.0, 30).unwrap();
        assert!(matches!(sig_done.progress, HomefrontArchetypeProgress::SuspiciousSignal { relay_hp, .. } if relay_hp <= 0.0));
        assert_eq!(
            complete_homefront_operation_and_pay(&ctx, &sessions[0], sig.instance_id)
                .await
                .unwrap()
                .total_fleet_isk,
            6_144_000
        );

        // 7. Three-pilot gate restriction (rejects Caracal cruiser 621, Coercer Navy Issue 73789; accepts Coercer 16236) -> (4/5)^2 * 12M = 7,680,000
        let sal = spawn_homefront_site_for_dungeon(&ctx, jita, 10_713).unwrap();
        clone_demo_char_with_id(&ctx, 940_200, 94_200, jita);
        let navy_dessy = setup_pilot_in_space(
            &ctx,
            893_200,
            940_200,
            743_300_001,
            73_789,
            jita,
            sal.site_position + DVec3::new(350.0, 0.0, 0.0),
        )
        .await;
        position_ship_at(&ctx, jita, 743_000_001, sal.site_position + DVec3::new(350.0, 0.0, 0.0)).await;
        assert_eq!(
            activate_homefront_acceleration_gate(&ctx, &sessions[0], sal.instance_id).await.unwrap_err().code,
            "ShipRestrictedByGate"
        );
        assert_eq!(
            activate_homefront_acceleration_gate(&ctx, &navy_dessy, sal.instance_id).await.unwrap_err().code,
            "ShipRestrictedByGate"
        );
        position_ship_at(&ctx, jita, 743_000_002, sal.site_position + DVec3::new(350.0, 0.0, 0.0)).await;
        activate_homefront_acceleration_gate(&ctx, &sessions[1], sal.instance_id).await.unwrap();
        salvage_research_process_wreck(&ctx, &sessions[1], sal.instance_id, 30, true).unwrap();
        assert_eq!(
            complete_homefront_operation_and_pay(&ctx, &sessions[1], sal.instance_id)
                .await
                .unwrap()
                .total_fleet_isk,
            7_680_000
        );

        let rift = spawn_homefront_site_for_dungeon(&ctx, jita, 10_714).unwrap();
        position_ship_at(&ctx, jita, 743_000_002, rift.site_position + DVec3::new(350.0, 0.0, 0.0)).await;
        activate_homefront_acceleration_gate(&ctx, &sessions[1], rift.instance_id).await.unwrap();
        stabilize_rift_neutralize_arrays(&ctx, &sessions[1], rift.instance_id, [0.95; 5], 60).unwrap();
        assert_eq!(
            complete_homefront_operation_and_pay(&ctx, &sessions[1], rift.instance_id)
                .await
                .unwrap()
                .total_fleet_isk,
            7_680_000
        );

        let traf = spawn_homefront_site_for_dungeon(&ctx, jita, 10_705).unwrap();
        position_ship_at(&ctx, jita, 743_000_002, traf.site_position + DVec3::new(350.0, 0.0, 0.0)).await;
        activate_homefront_acceleration_gate(&ctx, &sessions[1], traf.instance_id).await.unwrap();
        traffic_stop_scan_and_intercept(&ctx, &sessions[1], traf.instance_id, 6, 0, 6_000.0).unwrap();
        assert_eq!(
            complete_homefront_operation_and_pay(&ctx, &sessions[1], traf.instance_id)
                .await
                .unwrap()
                .total_fleet_isk,
            7_680_000
        );

        // 8. Live RPC & space subsystem integration (beyonce gate, dogmaIM Mine, hackingMgr, Cargo Scanner, Salvager I >= 20%)
        let met_live = spawn_homefront_site_for_dungeon(&ctx, jita, 10_312).unwrap();
        let met_outer_gate = get_homefront_runtime_state(&ctx, met_live.instance_id).unwrap().outer_gate_ball_id;
        position_ship_at(&ctx, jita, 743_000_004, met_live.site_position + DVec3::new(300.0, 0.0, 0.0)).await;
        ctx.dispatch_rpc(
            &mut sessions[3],
            "beyonce",
            None,
            "ActivateAccelerationGate",
            &[PyRep::Integer(met_outer_gate)],
            &kw,
            None,
        )
        .await
        .expect("live beyonce.ActivateAccelerationGate enters Metaliminal pocket");
        let rock_id = match get_homefront_runtime_state(&ctx, met_live.instance_id).unwrap().progress {
            HomefrontArchetypeProgress::MetaliminalMeteoroid { meteoroid_ball_id, .. } => meteoroid_ball_id,
            other => panic!("expected MetaliminalMeteoroid, got {other:?}"),
        };
        ctx.dispatch_rpc(
            &mut sessions[3],
            "dogmaIM",
            None,
            "Mine",
            &[PyRep::Integer(rock_id)],
            &kw,
            None,
        )
        .await
        .expect("live dogmaIM.Mine mines Mutanite");
        assert!(matches!(
            get_homefront_runtime_state(&ctx, met_live.instance_id).unwrap().progress,
            HomefrontArchetypeProgress::MetaliminalMeteoroid {
                ore_mined_by_fleet,
                remaining_units,
                ..
            } if ore_mined_by_fleet > 0 && remaining_units < 5_000
        ));

        let jita_h = ctx.systems.get_or_spawn(jita);
        let sal_live = spawn_homefront_site_for_dungeon(&ctx, jita, 10_713).unwrap();
        let sal_gate = get_homefront_runtime_state(&ctx, sal_live.instance_id).unwrap().outer_gate_ball_id;
        position_ship_at(&ctx, jita, 743_000_002, sal_live.site_position + DVec3::new(300.0, 0.0, 0.0)).await;
        ctx.dispatch_rpc(
            &mut sessions[1],
            "beyonce",
            None,
            "ActivateAccelerationGate",
            &[PyRep::Integer(sal_gate)],
            &kw,
            None,
        )
        .await
        .expect("enter Salvage Research via beyonce gate");
        let (werpost_id, first_wreck_id) = ctx.db.with_snapshot(|snap| {
            match &snap.dungeon_runtime.homefront_runtimes[&sal_live.instance_id].progress {
                HomefrontProgressRecord::SalvageResearch {
                    werpost_ball_id,
                    wreck_ball_ids,
                    ..
                } => (*werpost_ball_id, wreck_ball_ids[0]),
                other => panic!("expected SalvageResearch record, got {other:?}"),
            }
        });
        let (_, wp_ball) = jita_h.get_ball_snapshot(werpost_id).await.unwrap().expect("Werpost ball");
        position_ship_at(&ctx, jita, 743_000_002, wp_ball.position + DVec3::new(600.0, 0.0, 0.0)).await;
        let hack_rep = ctx
            .dispatch_rpc(
                &mut sessions[1],
                "hackingMgr",
                None,
                "StartNewGameInstance",
                &[
                    PyRep::Integer(werpost_id),
                    PyRep::Integer(0),
                    PyRep::Integer(1),
                    PyRep::Integer(1),
                ],
                &kw,
                None,
            )
            .await
            .expect("hackingMgr.StartNewGameInstance succeeds on Homefront Werpost");
        assert_eq!(hack_rep.dict_get("status"), Some(&PyRep::String("InProgress".into())));

        let (_, wr_ball) = jita_h.get_ball_snapshot(first_wreck_id).await.unwrap().expect("Wreck ball");
        position_ship_at(&ctx, jita, 743_000_002, wr_ball.position + DVec3::new(600.0, 0.0, 0.0)).await;
        let _ = jita_h
            .add_target_lock(
                743_000_002,
                TargetLock {
                    target_id: first_wreck_id,
                    lock_time_ms: 0.0,
                    remaining_lock_ms: 0.0,
                    locked: true,
                    max_range_m: Some(50_000.0),
                },
            )
            .await;
        let mut salvaged_ok = false;
        let mut last_cyc = None;
        for _ in 0..50 {
            let cyc = execute_wreck_salvage_cycle(
                &ctx,
                jita,
                940_002,
                743_000_002,
                first_wreck_id,
                Some(25_861),
            )
            .await
            .expect("execute_wreck_salvage_cycle");
            if cyc.dict_get("salvaged").and_then(|v| v.as_bool()) == Some(true) {
                salvaged_ok = true;
                break;
            }
            last_cyc = Some(cyc);
        }
        assert!(
            salvaged_ok,
            "Salvager I + Salvaging V must salvage Homefront wreck (last={last_cyc:?})"
        );
        assert!(matches!(
            get_homefront_runtime_state(&ctx, sal_live.instance_id).unwrap().progress,
            HomefrontArchetypeProgress::SalvageResearch { wrecks_salvaged, .. } if wrecks_salvaged >= 1
        ));

        let traf_live = spawn_homefront_site_for_dungeon(&ctx, jita, 10_705).unwrap();
        let traf_gate = get_homefront_runtime_state(&ctx, traf_live.instance_id).unwrap().outer_gate_ball_id;
        position_ship_at(&ctx, jita, 743_000_002, traf_live.site_position + DVec3::new(300.0, 0.0, 0.0)).await;
        ctx.dispatch_rpc(
            &mut sessions[1],
            "beyonce",
            None,
            "ActivateAccelerationGate",
            &[PyRep::Integer(traf_gate)],
            &kw,
            None,
        )
        .await
        .expect("enter Traffic Stop via beyonce gate");
        let smuggler_id = ctx.db.with_snapshot(|snap| {
            match &snap.dungeon_runtime.homefront_runtimes[&traf_live.instance_id].progress {
                HomefrontProgressRecord::TrafficStop { smuggler_ball_ids, .. } => smuggler_ball_ids[0],
                other => panic!("expected TrafficStop record, got {other:?}"),
            }
        });
        let (_, sm_ball) = jita_h.get_ball_snapshot(smuggler_id).await.unwrap().expect("Smuggler ball");
        position_ship_at(&ctx, jita, 743_000_002, sm_ball.position + DVec3::new(1_000.0, 0.0, 0.0)).await;
        let _ = jita_h
            .add_target_lock(
                743_000_002,
                TargetLock {
                    target_id: smuggler_id,
                    lock_time_ms: 0.0,
                    remaining_lock_ms: 0.0,
                    locked: true,
                    max_range_m: Some(50_000.0),
                },
            )
            .await;
        let scanner_mod_id = 743_100_013_i64; // Coercer slot_idx=3 -> Cargo Scanner I (442)
        let scan_res = ctx
            .dispatch_rpc(
                &mut sessions[1],
                "dogmaIM",
                None,
                "Activate",
                &[
                    PyRep::Integer(scanner_mod_id),
                    PyRep::String("cargoScan".into()),
                    PyRep::Integer(smuggler_id),
                ],
                &kw,
                None,
            )
            .await
            .expect("live dogmaIM.Activate cargoScan on Traffic Stop smuggler");
        assert!(scan_res.dict_get("totalScannedQuantity").and_then(|v| v.as_i64()).unwrap_or(0) >= 1);
        assert!(ctx.db.with_snapshot(|snap| {
            matches!(
                &snap.dungeon_runtime.homefront_runtimes[&traf_live.instance_id].progress,
                HomefrontProgressRecord::TrafficStop { scanned_hauler_ball_ids, .. }
                    if scanned_hauler_ball_ids.contains(&smuggler_id)
            )
        }));

        // Exordium halving sanity check against real SDE schools
        assert!(!is_homefront_exordium_system(&ctx.sde, jita));
        if let Some(ex_sys) = ctx.sde.schools.values().map(|s| s.solar_system_id).find(|&sid| sid != jita) {
            let ex_bp = resolve_homefront_archetype_blueprint(&ctx.sde, 10_347, "Raid", ex_sys, 2);
            if ex_bp.is_exordium_system {
                assert_eq!(ex_bp.archetype, HomefrontArchetype::Raid);
                assert_eq!(ex_bp.base_pilot_isk, 7_500_000);
            }
        }
        assert!(get_homefront_runtime_state(&ctx, raid.instance_id).is_some_and(|s| s.completed));
    }

    #[tokio::test]
    async fn test_homefront_operations_live_rpc_mining_salvage_and_nonhub_seeding() {
        use eve_server::services::pve_homefront_sites::{
            tick_homefront_operations_sync, HOMEFRONT_SALVAGE_DATA_TYPE_ID,
        };
        use eve_server::services::pve_special_sites::{
            ensure_special_sites_spawned_for_system, with_pve_special_reg,
        };
        use eve_server::services::pve_special_sites_combat::on_special_site_npc_destroyed_core;

        fn count_nested_instances(rep: &PyRep, expected_sys: u32) -> usize {
            let PyRep::Dict(outer) = rep else {
                panic!("expected outer Dict, got {rep:?}");
            };
            let Some((_, inner)) = outer.iter().find(|(k, _)| k.as_i64() == Some(i64::from(expected_sys))) else {
                return 0;
            };
            let PyRep::List(entries) = inner else {
                panic!("expected List of dungeon KeyVals, got {inner:?}");
            };
            entries.len()
        }

        let ctx = ServerContext::new();
        let kw = HashMap::new();
        let jita = 30_000_142_u32;
        let perim = 30_000_144_u32;

        // 1. Non-hub Perimeter (30_000_144): cold GetHomefrontSiteInstances must remain strictly read-only (0 sites),
        // then ensure_special_sites_spawned_for_system seeds sovereign Caldari Homefront sites.
        let mut perim_sess = setup_pilot_in_space(&ctx, 893_100, 940_010, 743_200_010, 620, perim, DVec3::ZERO).await;
        let cold_jita = ctx
            .dispatch_rpc(&mut perim_sess, "dungeonInstanceCacheMgr", None, "GetHomefrontSiteInstances", &[PyRep::Integer(i64::from(jita))], &kw, None)
            .await
            .expect("GetHomefrontSiteInstances Jita");
        assert_eq!(count_nested_instances(&cold_jita, jita), 2);

        let (hf_keys_before, rt_len_before, sim_clk_before) = (
            with_pve_special_reg(&ctx, |reg| {
                (
                    reg.homefront_sites.keys().copied().collect::<Vec<_>>(),
                    reg.sim_clock_sec,
                )
            }),
            ctx.db.with_snapshot(|s| s.dungeon_runtime.homefront_runtimes.len()),
            with_pve_special_reg(&ctx, |reg| reg.sim_clock_sec),
        );
        let cold_perim = ctx
            .dispatch_rpc(&mut perim_sess, "dungeonInstanceCacheMgr", None, "GetHomefrontSiteInstances", &[PyRep::Integer(i64::from(perim))], &kw, None)
            .await
            .expect("cold GetHomefrontSiteInstances Perimeter");
        assert_eq!(count_nested_instances(&cold_perim, perim), 0);
        let (hf_keys_after_cold, rt_len_after_cold, sim_clk_after_cold) = (
            with_pve_special_reg(&ctx, |reg| {
                (
                    reg.homefront_sites.keys().copied().collect::<Vec<_>>(),
                    reg.sim_clock_sec,
                )
            }),
            ctx.db.with_snapshot(|s| s.dungeon_runtime.homefront_runtimes.len()),
            with_pve_special_reg(&ctx, |reg| reg.sim_clock_sec),
        );
        assert_eq!(hf_keys_after_cold, hf_keys_before, "GetHomefrontSiteInstances must not seed non-hub systems");
        assert_eq!(rt_len_after_cold, rt_len_before, "GetHomefrontSiteInstances must not create runtimes");
        assert_eq!(sim_clk_after_cold, sim_clk_before, "GetHomefrontSiteInstances must not mutate registry clock");

        ensure_special_sites_spawned_for_system(&ctx, perim).await;
        let seeded_perim = ctx
            .dispatch_rpc(&mut perim_sess, "dungeonInstanceCacheMgr", None, "GetHomefrontSiteInstances", &[PyRep::Integer(i64::from(perim))], &kw, None)
            .await
            .expect("warm GetHomefrontSiteInstances Perimeter after ensure_special_sites_spawned_for_system");
        assert!(
            count_nested_instances(&seeded_perim, perim) >= 1,
            "ensure_special_sites_spawned_for_system must seed sovereign Homefront sites in Perimeter"
        );

        // 2. Live 9-wave dogmaIM.Mine AAR lifecycle + Wave 9 escort evacuation guard + Basileus/Cache + Wreck
        let miner_cid = 940_011_i64;
        let miner_ship = 743_200_011_i64;
        let mut miner_sess = setup_pilot_in_space(&ctx, 893_101, miner_cid, miner_ship, 32_880, jita, DVec3::ZERO).await;
        seed_item(&ctx, 743_300_001, 483, miner_cid, miner_ship, InventoryFlag::HiSlot0, 1, true);
        grant_skills(&ctx, miner_cid, &[(3386, 5), (32_918, 5), (3410, 5), (3426, 5), (3413, 5)]);
        ctx.invalidate_ship_fitting(miner_ship);

        let aar = spawn_homefront_site_for_dungeon(&ctx, jita, 10_346).expect("spawn live AAR");
        position_ship_at(&ctx, jita, miner_ship, aar.site_position + DVec3::new(300.0, 0.0, 0.0)).await;
        activate_homefront_acceleration_gate(&ctx, &miner_sess, aar.instance_id)
            .await
            .expect("Venture enters AAR gate");

        let (bal_before, j_before) = ctx.db.with_snapshot(|snap| {
            let j_cnt = snap
                .wallet_journal
                .iter()
                .filter(|j| j.character_id == miner_cid && j.entry_type == "CorporateRewardPayout")
                .count();
            (snap.characters[&miner_cid].balance, j_cnt)
        });
        let _ = ctx.drain_notifications_for_char(miner_cid);
        let jita_h = ctx.systems.get_or_spawn(jita);
        let mut miner_cs = ctx.build_type_combat_state(32_880);
        miner_cs.shield_hp = 5_000_000.0;
        miner_cs.max_shield_hp = 5_000_000.0;
        miner_cs.armor_hp = 5_000_000.0;
        miner_cs.max_armor_hp = 5_000_000.0;
        miner_cs.structure_hp = 5_000_000.0;
        miner_cs.max_structure_hp = 5_000_000.0;
        let _ = jita_h.set_combat_state(miner_ship, miner_cs).await;

        for w in 1..=9_u8 {
            // Clear mining hold & cargo between waves so Venture (5,000 m3 MiningHold) never stalls on CargoFull.
            ctx.db.with_mut_snapshot(|snap| {
                snap.items.retain(|_, it| {
                    !(it.location_id == miner_ship
                        && matches!(it.flag, InventoryFlag::Cargo | InventoryFlag::SpecializedOreHold))
                });
            });

            let (rock_ids, hostiles) = ctx.db.with_snapshot(|snap| {
                match &snap.dungeon_runtime.homefront_runtimes[&aar.instance_id].progress {
                    HomefrontProgressRecord::AbyssalArtifactRecovery {
                        current_wave,
                        rock_ball_ids,
                        wave_hostile_ball_ids,
                        ..
                    } => {
                        assert_eq!(*current_wave, w, "expected AAR wave {w}");
                        (rock_ball_ids.clone(), wave_hostile_ball_ids.clone())
                    }
                    other => panic!("expected AAR record, got {other:?}"),
                }
            });

            for rid in rock_ids {
                if let Some((_, rb)) = jita_h.get_ball_snapshot(rid).await.unwrap() {
                    position_ship_at(&ctx, jita, miner_ship, rb.position + DVec3::new(250.0, 0.0, 0.0)).await;
                }
                for _ in 0..10 {
                    let rem = ctx.db.with_snapshot(|snap| snap.asteroids.get(&rid).map_or(0, |a| a.quantity));
                    if rem <= 0 {
                        break;
                    }
                    let _ = ctx
                        .dispatch_rpc(
                            &mut miner_sess,
                            "dogmaIM",
                            None,
                            "Mine",
                            &[PyRep::Integer(rid)],
                            &kw,
                            None,
                        )
                        .await
                        .unwrap_or_else(|e| panic!("dogmaIM.Mine failed on wave {w} rock {rid}: {e:?}"));
                }
            }

            // Confirm wave w paid live immediately upon finishing wave w's Mutanite
            ctx.db.with_snapshot(|snap| {
                match &snap.dungeon_runtime.homefront_runtimes[&aar.instance_id].progress {
                    HomefrontProgressRecord::AbyssalArtifactRecovery {
                        mutanite_units_remaining,
                        warp_bubble_active,
                        waves_paid_mask,
                        ..
                    } => {
                        assert_eq!(*mutanite_units_remaining, 0);
                        assert!(!*warp_bubble_active);
                        assert_ne!(*waves_paid_mask & (1_u16 << (w - 1)), 0, "wave {w} must pay live");
                    }
                    other => panic!("expected AAR record, got {other:?}"),
                }
            });

            if w == 1 {
                // Drain 90s setup window and advance 1 tick so wave 2 spawns its rocks and escorts
                ctx.advance_secs(90);
                tick_homefront_operations_sync(&ctx);
                ctx.advance_secs(1);
                tick_homefront_operations_sync(&ctx);
            } else {
                for hid in hostiles {
                    let _ = on_special_site_npc_destroyed_core(
                        &ctx.systems,
                        &ctx.db,
                        &ctx.sde,
                        jita,
                        hid,
                        Some(miner_cid),
                    )
                    .await;
                }
            }
        }

        // Right after Wave 9 rocks + Wave 9 escort kills during evacuation_active && !basileus_spawned:
        // site must NOT settle/despawn, and all 9 per-wave ISK payouts (27,648,000 ISK) are already in wallet!
        ctx.db.with_snapshot(|snap| {
            let rec = &snap.dungeon_runtime.homefront_runtimes[&aar.instance_id];
            assert!(!rec.completed, "killing Wave 9 escorts during evacuation must NOT settle before Basileus/Cache");
            assert!(matches!(
                &rec.progress,
                HomefrontProgressRecord::AbyssalArtifactRecovery {
                    evacuation_active: true,
                    basileus_spawned: false,
                    waves_paid_mask: 0x01FF,
                    ..
                }
            ));
            let rows: Vec<_> = snap
                .wallet_journal
                .iter()
                .filter(|j| j.character_id == miner_cid && j.entry_type == "CorporateRewardPayout")
                .skip(j_before)
                .collect();
            assert_eq!(rows.len(), 9);
            assert!(rows.iter().all(|j| j.amount > IskCents(0)));
            assert_eq!(
                snap.characters[&miner_cid].balance,
                bal_before.checked_add(IskCents::from_isk(27_648_000)).unwrap()
            );
        });
        assert!(ctx.drain_notifications_for_char(miner_cid).iter().any(|n| n.event == "OnAccountChange"));

        // Advance 60s evacuation countdown -> spawns Basileus (77_702) + Abyssal Cache (77_727)
        ctx.advance_secs(60);
        tick_homefront_operations_sync(&ctx);
        let cache_id = ctx.db.with_snapshot(|snap| {
            match &snap.dungeon_runtime.homefront_runtimes[&aar.instance_id].progress {
                HomefrontProgressRecord::AbyssalArtifactRecovery {
                    basileus_spawned: true,
                    cache_ball_id: Some(cid),
                    ..
                } => *cid,
                other => panic!("expected Basileus + Cache spawned after 60s evacuation, got {other:?}"),
            }
        });
        assert!(jita_h
            .list_balls()
            .await
            .unwrap_or_default()
            .iter()
            .any(|b| b.type_id == 77_702));
        assert!(jita_h.get_ball_snapshot(cache_id).await.unwrap().is_some());

        // Destroy Abyssal Artifact Cache -> drops Cache Wreck (84_467) into SystemHandle
        let _ = on_special_site_npc_destroyed_core(
            &ctx.systems,
            &ctx.db,
            &ctx.sde,
            jita,
            cache_id,
            Some(miner_cid),
        )
        .await;
        assert!(
            jita_h
                .list_balls()
                .await
                .unwrap_or_default()
                .iter()
                .any(|b| b.type_id == 84_467),
            "destroying Abyssal Cache must spawn Cache Wreck (84_467)"
        );
        let aar_pay = complete_homefront_operation_and_pay(&ctx, &miner_sess, aar.instance_id)
            .await
            .expect("complete AAR");
        assert_eq!(aar_pay.total_fleet_isk, 27_648_000);
        ctx.db.with_snapshot(|snap| {
            assert_eq!(
                snap.characters[&miner_cid].balance,
                bal_before.checked_add(IskCents::from_isk(27_648_000)).unwrap()
            );
            let j_after = snap
                .wallet_journal
                .iter()
                .filter(|j| j.character_id == miner_cid && j.entry_type == "CorporateRewardPayout")
                .count();
            assert_eq!(j_after, j_before + 9, "settlement must not double-pay ISK");
        });
        assert!(!ctx.drain_notifications_for_char(miner_cid).iter().any(|n| n.event == "OnAccountChange"));

        // 3. Live Salvage Research (10_713) execute_wreck_salvage_cycle:
        //    unentered pilot rejected, full cargo rejects with NotEnoughCargoSpace,
        //    free cargo deposits HOMEFRONT_SALVAGE_DATA_TYPE_ID (81_352) + emits OnItemChange.
        let salv_cid = 940_012_i64;
        let salv_ship = 743_200_012_i64;
        let unentered_cid = 940_013_i64;
        let unentered_ship = 743_200_013_i64;
        let salv_sess = setup_pilot_in_space(&ctx, 893_102, salv_cid, salv_ship, 16_236, jita, DVec3::ZERO).await;
        let _unentered_sess = setup_pilot_in_space(&ctx, 893_103, unentered_cid, unentered_ship, 16_236, jita, DVec3::ZERO).await;
        seed_item(&ctx, 743_300_002, 25_861, salv_cid, salv_ship, InventoryFlag::HiSlot0, 1, true);
        seed_item(&ctx, 743_300_003, 25_861, unentered_cid, unentered_ship, InventoryFlag::HiSlot0, 1, true);
        grant_skills(&ctx, salv_cid, &[(25_863, 5), (3426, 5), (3413, 5)]);
        grant_skills(&ctx, unentered_cid, &[(25_863, 5), (3426, 5), (3413, 5)]);
        ctx.invalidate_ship_fitting(salv_ship);
        ctx.invalidate_ship_fitting(unentered_ship);

        let sal = spawn_homefront_site_for_dungeon(&ctx, jita, 10_713).expect("spawn Salvage Research");
        position_ship_at(&ctx, jita, salv_ship, sal.site_position + DVec3::new(300.0, 0.0, 0.0)).await;
        activate_homefront_acceleration_gate(&ctx, &salv_sess, sal.instance_id)
            .await
            .expect("Coercer enters Salvage Research");
        let wreck_0 = ctx.db.with_snapshot(|snap| {
            match &snap.dungeon_runtime.homefront_runtimes[&sal.instance_id].progress {
                HomefrontProgressRecord::SalvageResearch { wreck_ball_ids, .. } => wreck_ball_ids[0],
                other => panic!("expected SalvageResearch, got {other:?}"),
            }
        });
        let (_, wr_ball) = jita_h.get_ball_snapshot(wreck_0).await.unwrap().expect("wreck ball");
        position_ship_at(&ctx, jita, salv_ship, wr_ball.position + DVec3::new(500.0, 0.0, 0.0)).await;
        position_ship_at(&ctx, jita, unentered_ship, wr_ball.position + DVec3::new(500.0, 0.0, 0.0)).await;
        for (sid, cid_lock) in [(salv_ship, wreck_0), (unentered_ship, wreck_0)] {
            let _ = jita_h
                .add_target_lock(
                    sid,
                    TargetLock {
                        target_id: cid_lock,
                        lock_time_ms: 0.0,
                        remaining_lock_ms: 0.0,
                        locked: true,
                        max_range_m: Some(50_000.0),
                    },
                )
                .await;
        }

        // Unentered pilot rejected with GateNotEntered
        let res_unentered = execute_wreck_salvage_cycle(&ctx, jita, unentered_cid, unentered_ship, wreck_0, Some(25_861))
            .await
            .expect("unentered Homefront salvage cycle returns non-salvaged status");
        assert_eq!(res_unentered.dict_get("salvaged"), Some(&PyRep::Bool(false)));
        assert_eq!(res_unentered.dict_get("reason"), Some(&PyRep::String("GateNotEntered".into())));

        // Fill entered Coercer cargo so capacity check fails with NotEnoughCargoSpace on successful salvage roll
        seed_item(&ctx, 743_399_999, 34, salv_cid, salv_ship, InventoryFlag::Cargo, 500_000, false);
        let mut hit_cap_err = false;
        for _ in 0..80 {
            match execute_wreck_salvage_cycle(&ctx, jita, salv_cid, salv_ship, wreck_0, Some(25_861)).await {
                Err(e) if e.code == "NotEnoughCargoSpace" => {
                    hit_cap_err = true;
                    break;
                }
                Ok(v) if v.dict_get("salvaged").and_then(|b| b.as_bool()) != Some(true) => continue,
                other => panic!("expected NotEnoughCargoSpace on full cargo, got {other:?}"),
            }
        }
        assert!(hit_cap_err, "Salvage Research full cargo must fail closed with NotEnoughCargoSpace");
        assert!(matches!(
            get_homefront_runtime_state(&ctx, sal.instance_id).unwrap().progress,
            HomefrontArchetypeProgress::SalvageResearch { wrecks_salvaged: 0, .. }
        ));

        // Clear cargo blocker -> salvage succeeds, deposits HOMEFRONT_SALVAGE_DATA_TYPE_ID (81_352) + OnItemChange
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.remove(&743_399_999);
        });
        let _ = ctx.drain_notifications_for_char(salv_cid);
        let mut salv_ok = false;
        for _ in 0..80 {
            let cyc = execute_wreck_salvage_cycle(&ctx, jita, salv_cid, salv_ship, wreck_0, Some(25_861))
                .await
                .expect("execute_wreck_salvage_cycle with free cargo");
            if cyc.dict_get("salvaged").and_then(|b| b.as_bool()) == Some(true) {
                salv_ok = true;
                break;
            }
        }
        assert!(salv_ok, "Salvager I must salvage Homefront wreck with free cargo");
        assert!(ctx.db.with_snapshot(|snap| {
            snap.items.values().any(|it| {
                it.location_id == salv_ship
                    && it.flag == InventoryFlag::Cargo
                    && it.type_id == HOMEFRONT_SALVAGE_DATA_TYPE_ID
                    && it.quantity >= 1
            })
        }), "salvage must deposit HOMEFRONT_SALVAGE_DATA_TYPE_ID into ship cargo");
        assert!(ctx.drain_notifications_for_char(salv_cid).iter().any(|n| n.event == "OnItemChange"));
        assert!(matches!(
            get_homefront_runtime_state(&ctx, sal.instance_id).unwrap().progress,
            HomefrontArchetypeProgress::SalvageResearch { wrecks_salvaged: 1, .. }
        ));
    }
}
