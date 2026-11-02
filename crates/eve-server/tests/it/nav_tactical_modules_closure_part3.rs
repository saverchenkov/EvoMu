//! End-to-End Integration Tests closing Overheat Rack Heat and Probabilistic Heat Damage Propagation
//! parity rows (Part 3):
//! `feat:modules.overheat-rack-heat-generation` and `feat:modules.overheat-damage-propagation`.

use crate::modules_advanced_tactical::*;
use eve_db::InventoryFlag;
use eve_destiny::{Ball, CombatState, DVec3, MovementMode};
use eve_proto::PyRep;
use std::collections::HashMap;

fn set_ship_rack_heat(ctx: &eve_server::router::ServerContext, ship_id: i64, attr_id: u16, val: f64) {
    ctx.db.with_mut_snapshot(|snap| {
        let tr = snap.transient_item_attributes.entry(ship_id).or_default();
        if val > 0.0 {
            tr.insert(attr_id, val);
        } else {
            tr.remove(&attr_id);
        }
        if tr.is_empty() {
            snap.transient_item_attributes.remove(&ship_id);
        }
    });
}

#[tokio::test]
async fn test_overheat_rack_heat_analytical_curves_and_overload_bonuses() {
    // parity: feat:modules.overheat-rack-heat-generation
    eve_testkit::attest(&["feat:modules.overheat-rack-heat-generation"]);

    let ctx = build_ctx();
    let jita_sys: u32 = 30_000_142;
    let char_a: i64 = 94_041;
    let ship_a: i64 = 84_041;
    let mut sess_a = seed_pilot(&ctx, char_a, ship_a, 24_690, jita_sys, false); // Hyperion (attr 1224 = 0.5)
    let sys = ctx.systems.get_or_spawn(jita_sys);

    let mod_0: i64 = 76_001;
    let mod_1: i64 = 76_002;
    let ab_id: i64 = 76_005;
    let scram_id: i64 = 76_008;
    let rep_id: i64 = 76_009;
    give_item(&ctx, mod_0, 3_170, char_a, ship_a, InventoryFlag::HiSlot0, 1); // Light Ion Blaster II (attr 1180 = 0.02)
    give_item(&ctx, mod_1, 3_170, char_a, ship_a, InventoryFlag::HiSlot1, 1);
    give_item(&ctx, 76_007, 222, char_a, ship_a, InventoryFlag::HiSlot0, 500);
    give_item(&ctx, 76_017, 222, char_a, ship_a, InventoryFlag::HiSlot1, 500);
    give_item(&ctx, ab_id, 12_058, char_a, ship_a, InventoryFlag::MedSlot0, 1); // 10MN AB II (+50% overload speed)
    give_item(&ctx, scram_id, 448, char_a, ship_a, InventoryFlag::MedSlot1, 1); // Warp Scrambler II (+20% overload range: 9km -> 10.8km)
    give_item(&ctx, rep_id, 3_530, char_a, ship_a, InventoryFlag::LoSlot0, 1); // Medium Armor Repairer II (+10% rep, -15% dur)
    ctx.db.with_mut_snapshot(|snap| {
        for &mid in &[mod_0, mod_1, ab_id, scram_id, rep_id] {
            snap.module_online_states.insert(mid, true);
        }
    });
    ctx.invalidate_ship_fitting(ship_a);

    let origin = DVec3::new(50_000_000_000.0, 0.0, 0.0);
    spawn_in_space(&ctx, char_a, ship_a, 24_690, jita_sys, origin).await;
    let target_id: i64 = 88_801;
    let mut t_cs = CombatState::default();
    t_cs.max_shield_hp = 500_000.0;
    t_cs.shield_hp = 500_000.0;
    sys.spawn_ball_with_combat(
        Ball {
            id: target_id, type_id: 640, position: origin + DVec3::new(1_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO, max_velocity: 100.0, inertia: 1.0, mass: 1e7, radius: 100.0,
            speed_fraction: 0.0, mode: MovementMode::Stop,
        },
        t_cs,
    ).await.unwrap();
    sys.add_target_lock(ship_a, eve_destiny::TargetLock {
        target_id, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0),
    }).await.unwrap();

    // 1a. Analytical single-module heat curve on 1175 (`heatHi`): H(10) = 1 - exp(-10 * 0.5 * 0.02) = 1 - exp(-0.10)
    rpc(&ctx, &mut sess_a, "dogmaIM", "Overload", vec![
        PyRep::Integer(mod_0), PyRep::String("targetAttack".into()), PyRep::Integer(target_id), PyRep::Integer(1),
    ]).await.expect("Overload 1 blaster");
    for _ in 0..10 {
        sys.set_capacitor(ship_a, 10_000.0).await.unwrap();
        sys.step_manual(1.0).await.unwrap();
    }
    let h_1mod = attr_from_item_info(&rpc(&ctx, &mut sess_a, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(ship_a)]).await.unwrap(), 1175);
    let expected_1mod = 1.0 - (-0.10_f64).exp();
    assert!((h_1mod - expected_1mod).abs() < 1e-4, "1-module high rack heat (1175) must follow 1 - exp(-0.10): got {h_1mod} vs {expected_1mod}");
    assert!(
        ctx.db.with_snapshot(|s| {
            let sh = s.items.get(&ship_a).unwrap();
            !sh.dynamic_attributes.contains_key(&1175)
                && !sh.dynamic_attributes.contains_key(&1176)
                && !sh.dynamic_attributes.contains_key(&1177)
        }),
        "Transient ship rack heat (1175..=1177) must never be persisted to ship_item.dynamic_attributes"
    );

    // 1b. Exponential passive dissipation when overload is stopped: H_cool(10) = H_0 * exp(-10 * 0.01)
    rpc(&ctx, &mut sess_a, "dogmaIM", "StopOverload", vec![PyRep::Integer(mod_0)]).await.unwrap();
    rpc(&ctx, &mut sess_a, "dogmaIM", "Deactivate", vec![PyRep::Integer(mod_0)]).await.unwrap();
    for _ in 0..10 {
        sys.step_manual(1.0).await.unwrap();
    }
    let h_cooled = attr_from_item_info(&rpc(&ctx, &mut sess_a, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(ship_a)]).await.unwrap(), 1175);
    assert!((h_cooled - h_1mod * (-0.10_f64).exp()).abs() < 1e-4, "Passive cooling must follow H * exp(-t * diss): got {h_cooled}");

    // 1c. Linear 2-module sum (NOT N^2 * 2^N): 2 overloaded blasters -> gen_rate = 0.02 -> H(10) = 1 - exp(-0.20)
    set_ship_rack_heat(&ctx, ship_a, 1175, 0.0);
    set_item_dyn_attr(&ctx, mod_0, 3, 0.0);
    set_item_dyn_attr(&ctx, mod_1, 3, 0.0);
    for &mid in &[mod_0, mod_1] {
        rpc(&ctx, &mut sess_a, "dogmaIM", "Overload", vec![
            PyRep::Integer(mid), PyRep::String("targetAttack".into()), PyRep::Integer(target_id), PyRep::Integer(1),
        ]).await.unwrap();
    }
    for _ in 0..10 {
        sys.set_capacitor(ship_a, 10_000.0).await.unwrap();
        sys.step_manual(1.0).await.unwrap();
    }
    let h_2mod = attr_from_item_info(&rpc(&ctx, &mut sess_a, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(ship_a)]).await.unwrap(), 1175);
    let expected_2mod = 1.0 - (-0.20_f64).exp();
    assert!((h_2mod - expected_2mod).abs() < 1e-4, "2 overloaded modules must sum linearly to 1 - exp(-0.20): got {h_2mod} vs {expected_2mod}");
    for &mid in &[mod_0, mod_1] {
        rpc(&ctx, &mut sess_a, "dogmaIM", "Deactivate", vec![PyRep::Integer(mid)]).await.unwrap();
    }

    // 1d. Ship heatGenerationMultiplier (attr 1224): Rifter (587, 1224 = 1.0) heats at 2x rate of Hyperion (24690, 1224 = 0.5)
    let char_r: i64 = 94_042;
    let ship_r: i64 = 84_042;
    let mut sess_r = seed_pilot(&ctx, char_r, ship_r, 587, jita_sys, false);
    give_item(&ctx, 76_031, 3_170, char_r, ship_r, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, 76_032, 222, char_r, ship_r, InventoryFlag::HiSlot0, 500);
    ctx.db.with_mut_snapshot(|s| { s.module_online_states.insert(76_031, true); });
    ctx.invalidate_ship_fitting(ship_r);
    spawn_in_space(&ctx, char_r, ship_r, 587, jita_sys, origin + DVec3::new(0.0, 500.0, 0.0)).await;
    sys.add_target_lock(ship_r, eve_destiny::TargetLock {
        target_id, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0),
    }).await.unwrap();
    rpc(&ctx, &mut sess_r, "dogmaIM", "Overload", vec![
        PyRep::Integer(76_031), PyRep::String("targetAttack".into()), PyRep::Integer(target_id), PyRep::Integer(1),
    ]).await.unwrap();
    for _ in 0..10 {
        sys.set_capacitor(ship_r, 10_000.0).await.unwrap();
        sys.step_manual(1.0).await.unwrap();
    }
    let h_rifter = attr_from_item_info(&rpc(&ctx, &mut sess_r, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(ship_r)]).await.unwrap(), 1175);
    assert!((h_rifter - expected_2mod).abs() < 1e-4, "Rifter (1224=1.0) with 1 blaster must match 2x Hyperion (1224=0.5) rate: got {h_rifter}");
    rpc(&ctx, &mut sess_r, "dogmaIM", "Deactivate", vec![PyRep::Integer(76_031)]).await.unwrap();

    // 1e. SDE Overload bonuses: AB (+50% speed), Warp Scrambler II (+20% range at 10km), Armor Repairer (+10% rep / -15% dur), Weapon (-15% ROF)
    rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", vec![PyRep::Integer(ab_id)]).await.unwrap();
    let (_, ab_norm) = sys.get_ball_snapshot(ship_a).await.unwrap().unwrap();
    rpc(&ctx, &mut sess_a, "dogmaIM", "Deactivate", vec![PyRep::Integer(ab_id)]).await.unwrap();
    rpc(&ctx, &mut sess_a, "dogmaIM", "Overload", vec![PyRep::Integer(ab_id)]).await.unwrap();
    let (_, ab_ovl) = sys.get_ball_snapshot(ship_a).await.unwrap().unwrap();
    assert!(ab_ovl.max_velocity > ab_norm.max_velocity * 1.15, "Overloaded AB must boost max_velocity: {} vs {}", ab_ovl.max_velocity, ab_norm.max_velocity);
    rpc(&ctx, &mut sess_a, "dogmaIM", "Deactivate", vec![PyRep::Integer(ab_id)]).await.unwrap();

    sys.set_ball_position(target_id, origin + DVec3::new(10_200.0, 0.0, 0.0)).await;
    let scram_cold_err = rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", vec![
        PyRep::Integer(scram_id), PyRep::String("warpScramble".into()), PyRep::Integer(target_id), PyRep::Integer(0),
    ]).await.unwrap_err();
    assert_eq!(scram_cold_err.code, "TargetOutOfRange", "Unoverloaded Warp Scrambler II (9km) must fail at 10.2km");
    rpc(&ctx, &mut sess_a, "dogmaIM", "Overload", vec![
        PyRep::Integer(scram_id), PyRep::String("warpScramble".into()), PyRep::Integer(target_id), PyRep::Integer(0),
    ]).await.expect("Overloaded Warp Scrambler II (+20% -> 10.8km) must succeed at 10.2km");
    rpc(&ctx, &mut sess_a, "dogmaIM", "Deactivate", vec![PyRep::Integer(scram_id)]).await.unwrap();

    let fit_eval = ctx.get_or_rebuild_ship_fitting(char_a, ship_a).unwrap();
    let rep_norm = fit_eval.build_effective_module_state(rep_id, 3_530, false, &ctx.sde);
    let rep_ovl = fit_eval.build_effective_module_state(rep_id, 3_530, true, &ctx.sde);
    assert!((rep_ovl.get_attribute(84, &ctx.sde) - rep_norm.get_attribute(84, &ctx.sde) * 1.10).abs() < 1e-3);
    assert!((rep_ovl.get_attribute(73, &ctx.sde) - rep_norm.get_attribute(73, &ctx.sde) * 0.85).abs() < 1e-3);
    let gun_norm = fit_eval.build_effective_module_state(mod_0, 3_170, false, &ctx.sde);
    let gun_ovl = fit_eval.build_effective_module_state(mod_0, 3_170, true, &ctx.sde);
    assert!((gun_ovl.get_attribute(64, &ctx.sde) - gun_norm.get_attribute(64, &ctx.sde) * 1.15).abs() < 1e-3);

    // Mid-cycle UpdateModuleOverload on active Medium Armor Repairer II (`3530`):
    // Per EVE Overheating rules (`Overheating.md`), toggling overload mid-cycle takes effect on the NEXT cycle.
    let mut rep_cs = sys.get_combat_state(ship_a).await.unwrap().unwrap();
    rep_cs.max_armor_hp = 50_000.0;
    rep_cs.armor_hp = 1_000.0;
    sys.set_combat_state(ship_a, rep_cs).await.unwrap();
    rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", vec![
        PyRep::Integer(rep_id), PyRep::String("armorRepair".into()), PyRep::None, PyRep::Integer(1),
    ]).await.unwrap();
    rpc(&ctx, &mut sess_a, "dogmaIM", "Overload", vec![PyRep::Integer(rep_id)]).await.unwrap();
    // Base duration is 12.0s; after 11s, the in-progress cycle must NOT have finished early.
    for _ in 0..11 {
        sys.set_capacitor(ship_a, 10_000.0).await.unwrap();
        sys.step_manual(1.0).await.unwrap();
    }
    let base_rep = rep_norm.get_attribute(84, &ctx.sde);
    let armor_at_11s = sys.get_combat_state(ship_a).await.unwrap().unwrap().armor_hp;
    assert!(
        (armor_at_11s - (1_000.0 + base_rep)).abs() < 1.0,
        "Mid-cycle Overload must complete in-progress cycle at base repair ({}): got {armor_at_11s}",
        1_000.0 + base_rep
    );
    // After +11s (22s total), the next cycle completes at overloaded duration and +10% armor repair!
    for _ in 0..11 {
        sys.set_capacitor(ship_a, 10_000.0).await.unwrap();
        sys.step_manual(1.0).await.unwrap();
    }
    let armor_after_next_ovl = sys.get_combat_state(ship_a).await.unwrap().unwrap().armor_hp;
    let expected_armor = 1_000.0 + base_rep + base_rep * 1.10;
    assert!(
        (armor_after_next_ovl - expected_armor).abs() < 1.0,
        "Next cycle after mid-cycle Overload must apply +10% amount (attr 1230) and -15% duration (attr 1206, 10.2s): got {armor_after_next_ovl} vs {expected_armor}"
    );
    rpc(&ctx, &mut sess_a, "dogmaIM", "Deactivate", vec![PyRep::Integer(rep_id)]).await.unwrap();
}

#[tokio::test]
async fn test_probabilistic_heat_damage_propagation_and_nanite_paste() {
    // parity: feat:modules.overheat-damage-propagation
    eve_testkit::attest(&["feat:modules.overheat-damage-propagation"]);

    let ctx = build_ctx();
    let _kw: HashMap<String, PyRep> = HashMap::new();
    let jita_sys: u32 = 30_000_142;
    let char_a: i64 = 94_045;
    let ship_a: i64 = 84_045;
    let mut sess_a = seed_pilot(&ctx, char_a, ship_a, 24_690, jita_sys, false); // Hyperion (19 total hi+med+low slots)
    let sys = ctx.systems.get_or_spawn(jita_sys);

    set_skill_level(&ctx, char_a, 28_164, 0); // Thermodynamics L0
    let mod_0: i64 = 76_101;
    let mod_1: i64 = 76_102;
    let mod_2: i64 = 76_103;
    let mod_6: i64 = 76_104;
    let med_0: i64 = 76_105;
    let low_0: i64 = 76_106;
    give_item(&ctx, mod_0, 3_170, char_a, ship_a, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, mod_1, 3_170, char_a, ship_a, InventoryFlag::HiSlot1, 1);
    give_item(&ctx, mod_2, 3_170, char_a, ship_a, InventoryFlag::HiSlot2, 1);
    give_item(&ctx, mod_6, 3_170, char_a, ship_a, InventoryFlag::HiSlot6, 1);
    give_item(&ctx, 76_107, 222, char_a, ship_a, InventoryFlag::HiSlot0, 2_000);
    give_item(&ctx, med_0, 12_058, char_a, ship_a, InventoryFlag::MedSlot0, 1);
    give_item(&ctx, low_0, 3_530, char_a, ship_a, InventoryFlag::LoSlot0, 1);
    // Fill remaining med & low slots so slot_ratio = 16 / 19 (~0.84) when online
    let mut filler_ids = Vec::new();
    for (i, flag) in [
        InventoryFlag::MedSlot1, InventoryFlag::MedSlot2, InventoryFlag::MedSlot3, InventoryFlag::MedSlot4,
        InventoryFlag::LoSlot1, InventoryFlag::LoSlot2, InventoryFlag::LoSlot3, InventoryFlag::LoSlot4,
        InventoryFlag::LoSlot5, InventoryFlag::LoSlot6,
    ].into_iter().enumerate() {
        let fid = 76_200 + i as i64;
        give_item(&ctx, fid, 1_319, char_a, ship_a, flag, 1);
        filler_ids.push(fid);
    }
    ctx.db.with_mut_snapshot(|snap| {
        for &mid in &[mod_0, mod_1, mod_2, mod_6, med_0, low_0] {
            snap.module_online_states.insert(mid, true);
        }
        for &fid in &filler_ids {
            snap.module_online_states.insert(fid, true);
        }
    });
    ctx.invalidate_ship_fitting(ship_a);

    let origin = DVec3::new(50_000_000_000.0, 0.0, 0.0);
    spawn_in_space(&ctx, char_a, ship_a, 24_690, jita_sys, origin).await;
    let target_id: i64 = 88_810;
    let mut t_cs = CombatState::default();
    t_cs.max_shield_hp = 5_000_000.0;
    t_cs.shield_hp = 5_000_000.0;
    sys.spawn_ball_with_combat(
        Ball {
            id: target_id, type_id: 640, position: origin + DVec3::new(800.0, 0.0, 0.0),
            velocity: DVec3::ZERO, max_velocity: 100.0, inertia: 1.0, mass: 1e7, radius: 100.0,
            speed_fraction: 0.0, mode: MovementMode::Stop,
        },
        t_cs,
    ).await.unwrap();
    sys.add_target_lock(ship_a, eve_destiny::TargetLock {
        target_id, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0),
    }).await.unwrap();

    // 1. Seeded probabilistic heat damage: cold vs hot rack, non-wrapping distance attenuation, cross-rack isolation,
    // Thermodynamics L0 vs L5 (-25%), and Online Slots / Total Slots reduction (`feat:modules.overheat-damage-propagation`)
    ctx.set_mechanic_rng_seed(Some(2026));
    set_ship_rack_heat(&ctx, ship_a, 1175, 0.0);
    rpc(&ctx, &mut sess_a, "dogmaIM", "Overload", vec![
        PyRep::Integer(mod_0), PyRep::String("targetAttack".into()), PyRep::Integer(target_id), PyRep::Integer(1),
    ]).await.unwrap();
    for _ in 0..30 {
        sys.set_capacitor(ship_a, 10_000.0).await.unwrap();
        sys.step_manual(1.0).await.unwrap();
    }
    let dmg_cold = attr_from_item_info(&rpc(&ctx, &mut sess_a, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(mod_0)]).await.unwrap(), 3);
    rpc(&ctx, &mut sess_a, "dogmaIM", "Deactivate", vec![PyRep::Integer(mod_0)]).await.unwrap();

    // Hot rack (1175 = 0.85) under same seed 7 over 120s (~60 cycles on 400 HP cap): strictly probabilistic (0 < hits < 60),
    // monotonic attenuation d0 > d1 > d2 > d6, and zero cross-rack bleed!
    for &mid in &[mod_0, mod_1, mod_2, mod_6] {
        set_item_dyn_attr(&ctx, mid, 3, 0.0);
        set_item_dyn_attr(&ctx, mid, 40, 400.0);
    }
    set_ship_rack_heat(&ctx, ship_a, 1175, 0.85);
    ctx.set_mechanic_rng_seed(Some(7));
    rpc(&ctx, &mut sess_a, "dogmaIM", "Overload", vec![
        PyRep::Integer(mod_0), PyRep::String("targetAttack".into()), PyRep::Integer(target_id), PyRep::Integer(1),
    ]).await.unwrap();
    for _ in 0..120 {
        sys.set_capacitor(ship_a, 10_000.0).await.unwrap();
        sys.step_manual(1.0).await.unwrap();
    }
    rpc(&ctx, &mut sess_a, "dogmaIM", "Deactivate", vec![PyRep::Integer(mod_0)]).await.unwrap();
    let d0_l0 = attr_from_item_info(&rpc(&ctx, &mut sess_a, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(mod_0)]).await.unwrap(), 3);
    let d1_l0 = attr_from_item_info(&rpc(&ctx, &mut sess_a, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(mod_1)]).await.unwrap(), 3);
    let d2_l0 = attr_from_item_info(&rpc(&ctx, &mut sess_a, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(mod_2)]).await.unwrap(), 3);
    let d6_l0 = attr_from_item_info(&rpc(&ctx, &mut sess_a, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(mod_6)]).await.unwrap(), 3);
    let d_med = attr_from_item_info(&rpc(&ctx, &mut sess_a, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(med_0)]).await.unwrap(), 3);
    let d_low = attr_from_item_info(&rpc(&ctx, &mut sess_a, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(low_0)]).await.unwrap(), 3);

    assert!(d0_l0 > dmg_cold, "Hot rack (0.85) must produce more heat hits than cold rack: {d0_l0} vs {dmg_cold}");
    assert!(d0_l0 > 0.0 && d0_l0 < 60.0 * 0.8, "Heat hits must be probabilistic (0 < hits < N): d0={d0_l0}");
    assert!(d0_l0 > d1_l0 && d1_l0 > d2_l0 && d2_l0 > d6_l0, "Non-wrapping slot distance attenuation: d0={d0_l0}, d1={d1_l0}, d2={d2_l0}, d6={d6_l0}");
    assert_eq!(d_med, 0.0, "High rack heat must not bleed into MedSlot0");
    assert_eq!(d_low, 0.0, "High rack heat must not bleed into LoSlot0");

    // Thermodynamics L5 (-25% per-hit damage) under identical seed 7 -> exact 0.75x damage!
    set_skill_level(&ctx, char_a, 28_164, 5);
    for &mid in &[mod_0, mod_1, mod_2, mod_6] { set_item_dyn_attr(&ctx, mid, 3, 0.0); }
    set_ship_rack_heat(&ctx, ship_a, 1175, 0.85);
    ctx.set_mechanic_rng_seed(Some(7));
    rpc(&ctx, &mut sess_a, "dogmaIM", "Overload", vec![
        PyRep::Integer(mod_0), PyRep::String("targetAttack".into()), PyRep::Integer(target_id), PyRep::Integer(1),
    ]).await.unwrap();
    for _ in 0..120 {
        sys.set_capacitor(ship_a, 10_000.0).await.unwrap();
        sys.step_manual(1.0).await.unwrap();
    }
    rpc(&ctx, &mut sess_a, "dogmaIM", "Deactivate", vec![PyRep::Integer(mod_0)]).await.unwrap();
    let d0_l5 = attr_from_item_info(&rpc(&ctx, &mut sess_a, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(mod_0)]).await.unwrap(), 3);
    assert!((d0_l5 - d0_l0 * 0.75).abs() < 1e-4, "Thermodynamics L5 must scale heat damage by 0.75x: {d0_l5} vs {}", d0_l0 * 0.75);

    // Online Slots / Total Slots reduction: offlining 12 non-weapon modules drops slot_ratio from 16/19 to 4/19 -> fewer hits under seed 7!
    ctx.db.with_mut_snapshot(|snap| {
        for &fid in &filler_ids { snap.module_online_states.insert(fid, false); }
        snap.module_online_states.insert(med_0, false);
        snap.module_online_states.insert(low_0, false);
    });
    ctx.invalidate_ship_fitting(ship_a);
    for &mid in &[mod_0, mod_1, mod_2, mod_6] { set_item_dyn_attr(&ctx, mid, 3, 0.0); }
    set_ship_rack_heat(&ctx, ship_a, 1175, 0.85);
    ctx.set_mechanic_rng_seed(Some(7));
    rpc(&ctx, &mut sess_a, "dogmaIM", "Overload", vec![
        PyRep::Integer(mod_0), PyRep::String("targetAttack".into()), PyRep::Integer(target_id), PyRep::Integer(1),
    ]).await.unwrap();
    for _ in 0..120 {
        sys.set_capacitor(ship_a, 10_000.0).await.unwrap();
        sys.step_manual(1.0).await.unwrap();
    }
    rpc(&ctx, &mut sess_a, "dogmaIM", "Deactivate", vec![PyRep::Integer(mod_0)]).await.unwrap();
    let d0_sparse = attr_from_item_info(&rpc(&ctx, &mut sess_a, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(mod_0)]).await.unwrap(), 3);
    assert!(d0_sparse < d0_l5, "Offlining slots (lower Online/Total Slots) must reduce probabilistic heat hits: {d0_sparse} < {d0_l5}");

    // Analytical check of R-HEAT-03: total_slots includes rig slots (attr 1137), so 4 online hi+mid+low on a hull with rig slots
    // has slot_ratio = 4.0 / (hi + mid + low + rig).
    let thermo_eval = eve_server::services::tactical_module_mechanics::evaluate_ship_thermodynamics(
        ship_a,
        &Some(ctx.sde.clone()),
        &Some(ctx.db.clone()),
    );
    let sh_t = &ctx.sde.types[&ctx.db.with_snapshot(|s| s.items[&ship_a].type_id)];
    let expected_total_slots = sh_t.default_attributes.get(&14).copied().unwrap_or(0.0)
        + sh_t.default_attributes.get(&13).copied().unwrap_or(0.0)
        + sh_t.default_attributes.get(&12).copied().unwrap_or(0.0)
        + sh_t.default_attributes.get(&1137).copied().unwrap_or(0.0);
    assert!(
        (thermo_eval.slot_ratio - 4.0 / expected_total_slots).abs() < 1e-6,
        "slot_ratio must include rig slots (attr 1137) in denominator: got {} vs {}",
        thermo_eval.slot_ratio,
        4.0 / expected_total_slots
    );
}

#[tokio::test]
async fn test_nanite_repair_and_overload_cross_module_lockout() {
    let ctx = build_ctx();
    let (char_id, ship_id, mod_a, mod_b) = (94_081, 84_081, 75_801, 75_802);
    let mut sess = seed_pilot(&ctx, char_id, ship_id, 24_690, 30_000_142, false);
    let sys = ctx.systems.get_or_spawn(30_000_142);
    sys.set_capacitor(ship_id, 10_000.0).await.unwrap();
    give_item(&ctx, mod_a, 12_058, char_id, ship_id, InventoryFlag::MedSlot0, 1);
    give_item(&ctx, mod_b, 3_530, char_id, ship_id, InventoryFlag::LoSlot0, 1);
    give_item(&ctx, 75_803, 28668, char_id, ship_id, InventoryFlag::Cargo, 50);
    ctx.db.with_mut_snapshot(|s| {
        s.skills.entry(char_id).or_default().insert(28164, eve_db::TrainedSkillRecord { type_id: 28164, level: 5, skill_points: 256_000.0 });
        s.items.get_mut(&mod_a).unwrap().dynamic_attributes.insert(3, 10.0);
    });
    rpc(&ctx, &mut sess, "dogmaIM", "SetModuleOnline", vec![PyRep::Integer(ship_id), PyRep::Integer(mod_a)]).await.unwrap();
    rpc(&ctx, &mut sess, "dogmaIM", "SetModuleOnline", vec![PyRep::Integer(ship_id), PyRep::Integer(mod_b)]).await.unwrap();
    // Direction 1: Active nanite repair on mod_a blocks overloading mod_b
    rpc(&ctx, &mut sess, "dogmaIM", "RepairModule", vec![PyRep::Integer(mod_a)]).await.unwrap();
    let err1 = rpc(&ctx, &mut sess, "dogmaIM", "Overload", vec![PyRep::Integer(mod_b)]).await.unwrap_err();
    assert_eq!(err1.code, "CannotOverheatWhileRepairing");
    for _ in 0..30 {
        sys.step_manual(1.0).await.unwrap();
    }
    // Direction 2: Active overloaded cycle on mod_b blocks initiating nanite repair on mod_a
    ctx.db.with_mut_snapshot(|s| {
        s.items.get_mut(&mod_a).unwrap().dynamic_attributes.insert(3, 10.0);
    });
    rpc(&ctx, &mut sess, "dogmaIM", "Overload", vec![PyRep::Integer(mod_b)]).await.unwrap();
    let err2 = rpc(&ctx, &mut sess, "dogmaIM", "RepairModule", vec![PyRep::Integer(mod_a)]).await.unwrap_err();
    assert_eq!(err2.code, "CannotRepairWhileOverloaded");
}

#[tokio::test]
async fn test_interdiction_probe_excess_return_full_cargo_rejected() {
    let ctx = build_ctx();
    let sys_id = 30_004_608;
    let origin = DVec3::new(60_000_000_000.0, 0.0, 0.0);
    let (char_id, ship_id, launcher_id) = (94_082, 84_082, 75_811);
    let (char_b, ship_b) = (94_083, 84_083);
    let mut sess = seed_pilot(&ctx, char_id, ship_id, 22_456, sys_id, false);
    let _sess_b = seed_pilot(&ctx, char_b, ship_b, 587, sys_id, false);
    spawn_in_space(&ctx, char_id, ship_id, 22_456, sys_id, origin).await;
    spawn_in_space(&ctx, char_b, ship_b, 587, sys_id, origin + DVec3::new(2_000.0, 0.0, 0.0)).await;
    let sys = ctx.systems.get_or_spawn(sys_id);
    let (_, mut b_snap) = sys.get_ball_snapshot(ship_b).await.unwrap().unwrap();
    b_snap.velocity = DVec3::new(100.0, 0.0, 0.0);
    b_snap.mode = MovementMode::new_warp(
        origin + DVec3::new(2_000.0, 0.0, 0.0),
        origin + DVec3::new(20_000_000.0, 0.0, 0.0),
        3.0,
    );
    sys.spawn_ball_with_meta(
        b_snap,
        ctx.build_ship_combat_state(char_b, ship_b, 587),
        ctx.build_ship_slim_meta(char_b, 587),
    )
    .await
    .unwrap();
    let _ = sys.get_set_state(ship_b).await.unwrap();
    let _ = sys.drain_observer_updates(ship_b).await.unwrap();

    give_item(&ctx, launcher_id, 22_782, char_id, ship_id, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, 75_812, 22_778, char_id, ship_id, InventoryFlag::HiSlot0, 4);
    give_item(&ctx, 75_813, 34, char_id, ship_id, InventoryFlag::Cargo, 100_000);
    ctx.invalidate_ship_fitting(ship_id);
    rpc(&ctx, &mut sess, "dogmaIM", "SetModuleOnline", vec![PyRep::Integer(ship_id), PyRep::Integer(launcher_id)]).await.unwrap();
    let err = rpc(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(launcher_id)]).await.unwrap_err();
    assert_eq!(err.code, "NotEnoughCargoSpace");

    let (_tick, obs_evs, _rep) = sys.drain_observer_updates(ship_b).await.unwrap();
    assert!(
        !obs_evs.iter().any(|e| matches!(e, eve_destiny::DestinyEvent::AddBalls2 { .. })),
        "Rejected probe launch must emit zero AddBalls2 events"
    );
    let (_, post_b) = sys.get_ball_snapshot(ship_b).await.unwrap().unwrap();
    assert!(
        matches!(post_b.mode, MovementMode::Warp { phase: eve_destiny::WarpPhase::Aligning, .. }),
        "Rejected probe launch must not disturb nearby Aligning ship: got {:?}",
        post_b.mode
    );
    assert!((post_b.velocity.x - 100.0).abs() < 1e-6);
}

#[tokio::test]
async fn test_prealigned_warp_bubble_interception_no_overshoot() {
    let ctx = build_ctx();
    let sys_id = 30_004_608;
    let origin = DVec3::new(50_000_000_000.0, 0.0, 0.0);
    let (char_id, ship_id) = (94_084, 84_084);
    let _sess = seed_pilot(&ctx, char_id, ship_id, 11_198, sys_id, false);
    spawn_in_space(&ctx, char_id, ship_id, 11_198, sys_id, origin).await;
    let sys = ctx.systems.get_or_spawn(sys_id);
    let (_, mut ball) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    ball.velocity = DVec3::new(ball.max_velocity * 0.80, 0.0, 0.0);
    ball.mode = MovementMode::new_warp(origin, origin + DVec3::new(200_000.0, 0.0, 0.0), 10.0);
    sys.spawn_ball_with_meta(
        ball,
        ctx.build_ship_combat_state(char_id, ship_id, 11_198),
        ctx.build_ship_slim_meta(char_id, 11_198),
    )
    .await
    .unwrap();
    sys.add_interdiction_sphere(999_001, origin + DVec3::new(100_000.0, 0.0, 0.0), 20_000.0)
        .await
        .unwrap();

    let _ = sys.step_manual(1.0).await.unwrap();
    let (_, post) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let edge_x = origin.x + 80_000.0;
    assert!(
        post.position.x <= edge_x + 1e-3,
        "Pre-aligned ship must not overshoot catch bubble edge ({edge_x}) on Tick 1: got {}",
        post.position.x
    );
    if let MovementMode::Warp { destination, .. } = post.mode {
        assert!((destination.x - edge_x).abs() < 1e-3);
    }
}
