//! End-to-End Integration Tests closing Overheat, Nanite Paste, Nosferatu, Grappler,
//! Mutadaptive RAR & Blockade Runner parity rows (Part 3):
//! `feat:modules.overheat-rack-heat-generation`, `feat:modules.overheat-damage-propagation`,
//! `feat:modules.nanite-repair-paste-in-space`, `feat:modules.energy-nosferatu-relative-capacitor`,
//! `feat:modules.stasis-grappler-mechanics`, `feat:modules.mutadaptive-remote-armor-repairer`,
//! and `feat:ships.blockade-runner-cargo-scan-immunity`.

use crate::modules_advanced_tactical::*;
use eve_db::InventoryFlag;
use eve_destiny::{Ball, CombatState, DVec3, MovementMode};
use eve_proto::PyRep;

#[tokio::test]
async fn test_overheat_rack_heat_damage_propagation_and_nanite_paste() {
    // parity: feat:modules.overheat-rack-heat-generation, feat:modules.overheat-damage-propagation, feat:modules.nanite-repair-paste-in-space
    eve_testkit::attest(&[
        "feat:modules.overheat-rack-heat-generation",
        "feat:modules.overheat-damage-propagation",
        "feat:modules.nanite-repair-paste-in-space",
    ]);

    let ctx = build_ctx();
    let jita_sys: u32 = 30_000_142;
    let char_a: i64 = 94_041;
    let ship_a: i64 = 84_041;
    let mut sess_a = seed_pilot(&ctx, char_a, ship_a, 24_690, jita_sys, false); // Cyclone
    let sys = ctx.systems.get_or_spawn(jita_sys);

    // 1. Overheat Rack Heat Generation & Thermodynamics L0 vs L5 (`feat:modules.overheat-rack-heat-generation`)
    set_skill_level(&ctx, char_a, 28_164, 0); // Thermodynamics L0
    let mod_0: i64 = 76_001;
    let mod_1: i64 = 76_002;
    let mod_2: i64 = 76_003;
    let mod_6: i64 = 76_004;
    let med_0: i64 = 76_005;
    let low_0: i64 = 76_006;
    let ammo_0: i64 = 76_007;

    give_item(&ctx, mod_0, 3_170, char_a, ship_a, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, mod_1, 3_170, char_a, ship_a, InventoryFlag::HiSlot1, 1);
    give_item(&ctx, mod_2, 3_170, char_a, ship_a, InventoryFlag::HiSlot2, 1);
    give_item(&ctx, mod_6, 3_170, char_a, ship_a, InventoryFlag::HiSlot6, 1);
    give_item(&ctx, med_0, 12_058, char_a, ship_a, InventoryFlag::MedSlot0, 1);
    give_item(&ctx, low_0, 2_048, char_a, ship_a, InventoryFlag::LoSlot0, 1);
    give_item(&ctx, ammo_0, 222, char_a, ship_a, InventoryFlag::HiSlot0, 500);
    ctx.db.with_mut_snapshot(|snap| {
        for &mid in &[mod_0, mod_1, mod_2, mod_6, med_0, low_0] {
            snap.module_online_states.insert(mid, true);
        }
    });
    ctx.invalidate_ship_fitting(ship_a);

    let origin = DVec3::new(10_000_000.0, 0.0, 0.0);
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

    // Overload mod_0 for 4 cycles (12s) at Thermodynamics L0
    rpc(&ctx, &mut sess_a, "dogmaIM", "Overload", vec![
        PyRep::Integer(mod_0), PyRep::String("targetAttack".into()), PyRep::Integer(target_id), PyRep::Integer(1),
    ]).await.expect("Overload mod_0");
    for _ in 0..12 {
        sys.step_manual(1.0).await.unwrap();
    }
    let ship_info_1 = rpc(&ctx, &mut sess_a, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(ship_a)]).await.unwrap();
    let heat_t0 = attr_from_item_info(&ship_info_1, 1177);
    assert!(heat_t0 > 0.0, "High rack heat (1177) must accumulate when overloaded");
    let dmg_l0 = attr_from_item_info(
        &rpc(&ctx, &mut sess_a, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(mod_0)]).await.unwrap(),
        3,
    );
    assert!(dmg_l0 > 0.0, "Overloaded module must take heat damage");

    // Passive heat dissipation when overload stopped
    rpc(&ctx, &mut sess_a, "dogmaIM", "StopOverload", vec![PyRep::Integer(mod_0)]).await.unwrap();
    for _ in 0..10 {
        sys.step_manual(1.0).await.unwrap();
    }
    let heat_cooled = attr_from_item_info(
        &rpc(&ctx, &mut sess_a, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(ship_a)]).await.unwrap(),
        1177,
    );
    assert!(heat_cooled < heat_t0, "High rack heat must dissipate exponentially when not overloaded");

    // Thermodynamics L5 reduces heat damage by 25% (5%/lvl)
    rpc(&ctx, &mut sess_a, "dogmaIM", "Deactivate", vec![PyRep::Integer(mod_0)]).await.unwrap();
    for _ in 0..3 {
        sys.step_manual(1.0).await.unwrap();
    }
    set_skill_level(&ctx, char_a, 28_164, 5);
    set_item_dyn_attr(&ctx, mod_0, 3, 0.0);
    set_item_dyn_attr(&ctx, ship_a, 1177, 0.0);
    rpc(&ctx, &mut sess_a, "dogmaIM", "Overload", vec![
        PyRep::Integer(mod_0), PyRep::String("targetAttack".into()), PyRep::Integer(target_id), PyRep::Integer(1),
    ]).await.expect("Overload mod_0 at Thermodynamics L5");
    for _ in 0..12 {
        sys.step_manual(1.0).await.unwrap();
    }
    let dmg_l5 = attr_from_item_info(
        &rpc(&ctx, &mut sess_a, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(mod_0)]).await.unwrap(),
        3,
    );
    assert!((dmg_l5 - dmg_l0 * 0.75).abs() < 1e-3, "Thermodynamics L5 must reduce heat damage by 25%: got {dmg_l5} vs {}", dmg_l0 * 0.75);

    // 2. Heat Damage Propagation Across Non-Wrapping High Rack & Burnout (`feat:modules.overheat-damage-propagation`)
    for _ in 0..140 {
        sys.step_manual(1.0).await.unwrap();
    }
    let d0 = attr_from_item_info(&rpc(&ctx, &mut sess_a, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(mod_0)]).await.unwrap(), 3);
    let d1 = attr_from_item_info(&rpc(&ctx, &mut sess_a, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(mod_1)]).await.unwrap(), 3);
    let d2 = attr_from_item_info(&rpc(&ctx, &mut sess_a, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(mod_2)]).await.unwrap(), 3);
    let d6 = attr_from_item_info(&rpc(&ctx, &mut sess_a, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(mod_6)]).await.unwrap(), 3);
    let d_med = attr_from_item_info(&rpc(&ctx, &mut sess_a, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(med_0)]).await.unwrap(), 3);
    let d_low = attr_from_item_info(&rpc(&ctx, &mut sess_a, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(low_0)]).await.unwrap(), 3);

    assert!((d0 - 40.0).abs() < 1e-3, "mod_0 must burn out at 40.0 HP: got {d0}");
    assert!(d1 > 0.0 && d2 > 0.0 && d1 > d2, "Adjacent slots must attenuate by linear distance: d1={d1}, d2={d2}");
    assert!(d6 < d2 * 0.5, "Non-wrapping HiSlot6 (dist=6) must take far less bleed than HiSlot2 (dist=2): d6={d6}, d2={d2}");
    assert_eq!(d_med, 0.0, "High rack heat must not bleed into MedSlot0");
    assert_eq!(d_low, 0.0, "High rack heat must not bleed into LoSlot0");

    // 3. In-Space Nanite Repair Paste Consumption, Early Stop, Burnout Rejection, & Tether Free Repair
    // (`feat:modules.nanite-repair-paste-in-space`)
    // Burned-out mod_0 rejected in space
    let burn_err = rpc(&ctx, &mut sess_a, "dogmaIM", "RepairModule", vec![PyRep::Integer(ship_a), PyRep::Integer(mod_0)])
        .await.unwrap_err();
    assert_eq!(burn_err.code, "ModuleBurnedOut");

    // Active module rejected with CannotRepairActiveModule
    rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", vec![PyRep::Integer(med_0)]).await.unwrap();
    set_item_dyn_attr(&ctx, med_0, 3, 5.0);
    let active_rep_err = rpc(&ctx, &mut sess_a, "dogmaIM", "RepairModule", vec![PyRep::Integer(ship_a), PyRep::Integer(med_0)])
        .await.unwrap_err();
    assert_eq!(active_rep_err.code, "CannotRepairActiveModule");
    rpc(&ctx, &mut sess_a, "dogmaIM", "Deactivate", vec![PyRep::Integer(med_0)]).await.unwrap();
    set_item_dyn_attr(&ctx, med_0, 3, 0.0);

    // Ensure mod_1 has 30.0 HP damage (not burned out, requires >= 2 paste even with Nanite Operation L5) and 0 paste -> NotEnoughNanitePaste
    set_item_dyn_attr(&ctx, mod_1, 3, 30.0);
    let no_paste_err = rpc(&ctx, &mut sess_a, "dogmaIM", "RepairModule", vec![PyRep::Integer(ship_a), PyRep::Integer(mod_1)])
        .await.unwrap_err();
    assert_eq!(no_paste_err.code, "NotEnoughNanitePaste");

    // Seed 50 Nanite Repair Paste (28668) in cargo; start repair -> deducts 1 at t=0, StopRepairModule preserves unused paste
    let paste_id: i64 = 76_099;
    give_item(&ctx, paste_id, 28_668, char_a, ship_a, InventoryFlag::Cargo, 50);
    let rep_start = rpc(&ctx, &mut sess_a, "dogmaIM", "RepairModule", vec![PyRep::Integer(ship_a), PyRep::Integer(mod_1)])
        .await.expect("RepairModule with paste");
    assert_eq!(rep_start.dict_get("pasteConsumed"), Some(&PyRep::Integer(1)));
    let total_req = rep_start.dict_get("totalPasteRequired").and_then(|v| v.as_i64()).unwrap();
    assert!(total_req >= 2);
    sys.step_manual(1.0).await.unwrap();
    rpc(&ctx, &mut sess_a, "dogmaIM", "StopRepairModule", vec![PyRep::Integer(ship_a), PyRep::Integer(mod_1)])
        .await.expect("StopRepairModule preserves unused paste");
    let paste_after_stop = ctx.db.with_snapshot(|s| s.items.get(&paste_id).map(|i| i.quantity).unwrap_or(0));
    assert!(paste_after_stop > 50 - (total_req as i32), "Early stop must preserve unconsumed Nanite Repair Paste");

    // Finish repairing mod_1 to 0.0 HP
    rpc(&ctx, &mut sess_a, "dogmaIM", "RepairModule", vec![PyRep::Integer(ship_a), PyRep::Integer(mod_1)])
        .await.expect("Resume RepairModule");
    for _ in 0..25 {
        sys.step_manual(1.0).await.unwrap();
    }
    let d1_repaired = attr_from_item_info(&rpc(&ctx, &mut sess_a, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(mod_1)]).await.unwrap(), 3);
    assert_eq!(d1_repaired, 0.0);

    // Tethered repair repairs even completely burned-out mod_0 with 0 Nanite Repair Paste
    let paste_before_tether = ctx.db.with_snapshot(|s| s.items.get(&paste_id).map(|i| i.quantity).unwrap_or(0));
    let mut tether_cs = sys.get_combat_state(ship_a).await.unwrap().unwrap();
    tether_cs.is_tethered = true;
    sys.set_combat_state(ship_a, tether_cs).await.unwrap();
    let tether_rep = rpc(&ctx, &mut sess_a, "dogmaIM", "RepairModule", vec![PyRep::Integer(ship_a), PyRep::Integer(mod_0)])
        .await.expect("Tethered free repair of burned-out module");
    assert_eq!(tether_rep.dict_get("pasteConsumed"), Some(&PyRep::Integer(0)));
    assert_eq!(tether_rep.dict_get("tetherFreeRepair"), Some(&PyRep::Bool(true)));
    let paste_after_tether = ctx.db.with_snapshot(|s| s.items.get(&paste_id).map(|i| i.quantity).unwrap_or(0));
    assert_eq!(paste_after_tether, paste_before_tether);
    let d0_repaired = attr_from_item_info(&rpc(&ctx, &mut sess_a, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(mod_0)]).await.unwrap(), 3);
    assert_eq!(d0_repaired, 0.0);
}

#[tokio::test]
async fn test_nosferatu_relative_cap_grappler_mutadaptive_rar_and_blockade_runner() {
    // parity: feat:modules.energy-nosferatu-relative-capacitor, feat:modules.stasis-grappler-mechanics, feat:modules.mutadaptive-remote-armor-repairer, feat:ships.blockade-runner-cargo-scan-immunity
    eve_testkit::attest(&[
        "feat:modules.energy-nosferatu-relative-capacitor",
        "feat:modules.stasis-grappler-mechanics",
        "feat:modules.mutadaptive-remote-armor-repairer",
        "feat:ships.blockade-runner-cargo-scan-immunity",
    ]);

    let ctx = build_ctx();
    let jita_sys: u32 = 30_000_142;
    let char_src: i64 = 94_051;
    let ship_src: i64 = 84_051;
    let char_tgt: i64 = 94_052;
    let ship_tgt: i64 = 84_052;
    let char_tgt2: i64 = 94_053;
    let ship_tgt2: i64 = 84_053;

    let mut sess_src = seed_pilot(&ctx, char_src, ship_src, 640, jita_sys, false); // Raven
    let _sess_tgt = seed_pilot(&ctx, char_tgt, ship_tgt, 24_690, jita_sys, false); // Cyclone
    let _sess_tgt2 = seed_pilot(&ctx, char_tgt2, ship_tgt2, 12_729, jita_sys, false); // Crane (Blockade Runner)
    let sys = ctx.systems.get_or_spawn(jita_sys);

    // 1. Energy Nosferatu Relative Capacitor % & Blood Raider Override (`feat:modules.energy-nosferatu-relative-capacitor`)
    let nos_id: i64 = 77_001;
    give_item(&ctx, nos_id, 530, char_src, ship_src, InventoryFlag::HiSlot0, 1); // Small Energy Nosferatu I
    ctx.db.with_mut_snapshot(|snap| { snap.module_online_states.insert(nos_id, true); });
    ctx.invalidate_ship_fitting(ship_src);

    let origin = DVec3::new(50_000_000.0, 0.0, 0.0);
    spawn_in_space(&ctx, char_src, ship_src, 640, jita_sys, origin).await;
    spawn_in_space(&ctx, char_tgt, ship_tgt, 24_690, jita_sys, origin + DVec3::new(1_500.0, 0.0, 0.0)).await;
    sys.add_target_lock(ship_src, eve_destiny::TargetLock {
        target_id: ship_tgt, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(100_000.0),
    }).await.unwrap();

    // Target 1200/3000 (40%) < Source 800/1000 (80%): no drain on standard hull despite 1200 GJ > 800 GJ
    let mut s_cs = sys.get_combat_state(ship_src).await.unwrap().unwrap();
    s_cs.capacitor_recharge_time_ms = 0.0;
    s_cs.max_capacitor = 1_000.0;
    s_cs.capacitor = 800.0;
    sys.set_combat_state(ship_src, s_cs.clone()).await.unwrap();
    let mut t_cs = sys.get_combat_state(ship_tgt).await.unwrap().unwrap();
    t_cs.capacitor_recharge_time_ms = 0.0;
    t_cs.max_capacitor = 3_000.0;
    t_cs.capacitor = 1_200.0;
    sys.set_combat_state(ship_tgt, t_cs.clone()).await.unwrap();

    rpc(&ctx, &mut sess_src, "dogmaIM", "Activate", vec![
        PyRep::Integer(nos_id), PyRep::String("energyNosferatuFalloff".into()), PyRep::Integer(ship_tgt), PyRep::Integer(0),
    ]).await.expect("Activate Nosferatu");
    for _ in 0..3 { sys.step_manual(1.0).await.unwrap(); }
    assert_eq!(sys.get_combat_state(ship_tgt).await.unwrap().unwrap().capacitor, 1_200.0);
    rpc(&ctx, &mut sess_src, "dogmaIM", "Deactivate", vec![PyRep::Integer(nos_id)]).await.unwrap();

    // Target 400/500 (80%) > Source 500/2000 (25%): drains despite 400 GJ < 500 GJ
    let mut s_cs2 = sys.get_combat_state(ship_src).await.unwrap().unwrap();
    s_cs2.capacitor_recharge_time_ms = 0.0;
    s_cs2.max_capacitor = 2_000.0;
    s_cs2.capacitor = 500.0;
    sys.set_combat_state(ship_src, s_cs2).await.unwrap();
    let mut t_cs2 = sys.get_combat_state(ship_tgt).await.unwrap().unwrap();
    t_cs2.capacitor_recharge_time_ms = 0.0;
    t_cs2.max_capacitor = 500.0;
    t_cs2.capacitor = 400.0;
    sys.set_combat_state(ship_tgt, t_cs2).await.unwrap();
    rpc(&ctx, &mut sess_src, "dogmaIM", "Activate", vec![
        PyRep::Integer(nos_id), PyRep::String("energyNosferatuFalloff".into()), PyRep::Integer(ship_tgt), PyRep::Integer(0),
    ]).await.expect("Activate Nosferatu when target % > self %");
    for _ in 0..3 { sys.step_manual(1.0).await.unwrap(); }
    assert!(sys.get_combat_state(ship_tgt).await.unwrap().unwrap().capacitor < 400.0);
    assert!(sys.get_combat_state(ship_src).await.unwrap().unwrap().capacitor > 500.0);
    rpc(&ctx, &mut sess_src, "dogmaIM", "Deactivate", vec![PyRep::Integer(nos_id)]).await.unwrap();

    // Blood Raider Bhaalgorn (17920) overrides relative % limit
    ctx.db.with_mut_snapshot(|snap| { snap.items.get_mut(&ship_src).unwrap().type_id = 17_920; });
    ctx.invalidate_ship_fitting(ship_src);
    sys.set_combat_state(ship_src, s_cs).await.unwrap();
    sys.set_combat_state(ship_tgt, t_cs).await.unwrap();
    rpc(&ctx, &mut sess_src, "dogmaIM", "Activate", vec![
        PyRep::Integer(nos_id), PyRep::String("energyNosferatuFalloff".into()), PyRep::Integer(ship_tgt), PyRep::Integer(0),
    ]).await.expect("Activate Nosferatu on Bhaalgorn");
    for _ in 0..3 { sys.step_manual(1.0).await.unwrap(); }
    assert!(sys.get_combat_state(ship_tgt).await.unwrap().unwrap().capacitor < 1_200.0);
    rpc(&ctx, &mut sess_src, "dogmaIM", "Deactivate", vec![PyRep::Integer(nos_id)]).await.unwrap();

    // 2. Heavy Stasis Grappler Mechanics (`feat:modules.stasis-grappler-mechanics`)
    let mut cruiser_sess = seed_pilot(&ctx, 94_054, 84_054, 621, jita_sys, true); // Caracal
    assert!(fit_mod(&ctx, &mut cruiser_sess, 84_054, 77_010, 41_040, 19).await.is_err(), "Cruiser cannot fit Heavy Stasis Grappler");

    let mut bs_fit_sess = seed_pilot(&ctx, char_src, ship_src, 640, jita_sys, true);
    fit_mod(&ctx, &mut bs_fit_sess, ship_src, 77_011, 41_040, 19).await.expect("Battleship fits 1st Grappler");
    assert!(fit_mod(&ctx, &mut bs_fit_sess, ship_src, 77_012, 41_040, 20).await.is_err(), "maxGroupFitted=1 blocks 2nd Grappler");
    fit_mod(&ctx, &mut bs_fit_sess, ship_src, 77_013, 527, 20).await.expect("Fit Stasis Webifier II");

    sess_src.station_id = None;
    sess_src.solar_system_id = Some(jita_sys);
    spawn_in_space(&ctx, char_src, ship_src, 640, jita_sys, origin).await;
    spawn_in_space(&ctx, char_tgt, ship_tgt, 24_690, jita_sys, origin + DVec3::new(500.0, 0.0, 0.0)).await;
    let (_, base_t_ball) = sys.get_ball_snapshot(ship_tgt).await.unwrap().unwrap();
    let base_t_vel = base_t_ball.max_velocity;
    sys.add_target_lock(ship_src, eve_destiny::TargetLock {
        target_id: ship_tgt, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(100_000.0),
    }).await.unwrap();

    // At 500 m (within 1 km optimal) -> -80% speed (0.20x)
    rpc(&ctx, &mut sess_src, "dogmaIM", "Activate", vec![
        PyRep::Integer(77_011), PyRep::String("useMissiles".into()), PyRep::Integer(ship_tgt), PyRep::Integer(1),
    ]).await.expect("Activate Grappler at 500m");
    let (_, b_500) = sys.get_ball_snapshot(ship_tgt).await.unwrap().unwrap();
    assert!((b_500.max_velocity - base_t_vel * 0.20).abs() < 1.0);

    // Stacking penalty with Stasis Webifier II (527, -60%)
    rpc(&ctx, &mut sess_src, "dogmaIM", "Activate", vec![
        PyRep::Integer(77_013), PyRep::String("useMissiles".into()), PyRep::Integer(ship_tgt), PyRep::Integer(1),
    ]).await.expect("Activate Web II alongside Grappler");
    let (_, b_stacked) = sys.get_ball_snapshot(ship_tgt).await.unwrap().unwrap();
    assert!(b_stacked.max_velocity > base_t_vel * 0.085 && b_stacked.max_velocity < base_t_vel * 0.11, "got {}", b_stacked.max_velocity);
    rpc(&ctx, &mut sess_src, "dogmaIM", "Deactivate", vec![PyRep::Integer(77_013)]).await.unwrap();
    rpc(&ctx, &mut sess_src, "dogmaIM", "Deactivate", vec![PyRep::Integer(77_011)]).await.unwrap();

    // At 16,000 m (far into 8 km falloff) -> decays below 15% without artificial -15% floor
    sys.set_ball_position(ship_tgt, origin + DVec3::new(16_000.0, 0.0, 0.0)).await;
    rpc(&ctx, &mut sess_src, "dogmaIM", "Activate", vec![
        PyRep::Integer(77_011), PyRep::String("useMissiles".into()), PyRep::Integer(ship_tgt), PyRep::Integer(1),
    ]).await.expect("Activate Grappler at 16km");
    let (_, b_16k) = sys.get_ball_snapshot(ship_tgt).await.unwrap().unwrap();
    assert!(b_16k.max_velocity > base_t_vel * 0.90, "Grappler at 16km must decay below 10% web (no -15% floor): got {}", b_16k.max_velocity);
    rpc(&ctx, &mut sess_src, "dogmaIM", "Deactivate", vec![PyRep::Integer(77_011)]).await.unwrap();

    // 3. Mutadaptive Remote Armor Repairer (`feat:modules.mutadaptive-remote-armor-repairer`)
    assert!(fit_mod(&ctx, &mut cruiser_sess, 84_054, 77_020, 49_770, 27).await.is_err(), "Caracal cannot fit Mutadaptive RAR");
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, i| i.location_id != ship_src);
        snap.module_online_states.clear();
        snap.module_active_states.clear();
    });
    let mut zarm_fit_sess = seed_pilot(&ctx, char_src, ship_src, 49_713, jita_sys, true); // Zarmazd
    ctx.invalidate_ship_fitting(ship_src);
    fit_mod(&ctx, &mut zarm_fit_sess, ship_src, 77_021, 49_770, 27).await.expect("Zarmazd fits 1st Mutadaptive RAR");
    assert!(fit_mod(&ctx, &mut zarm_fit_sess, ship_src, 77_022, 49_770, 28).await.is_err(), "maxGroupFitted=1 blocks 2nd Mutadaptive RAR");

    sess_src.station_id = None;
    sess_src.solar_system_id = Some(jita_sys);
    spawn_in_space(&ctx, char_src, ship_src, 49_713, jita_sys, origin).await;
    spawn_in_space(&ctx, char_tgt, ship_tgt, 24_690, jita_sys, origin + DVec3::new(3_000.0, 0.0, 0.0)).await;
    spawn_in_space(&ctx, char_tgt2, ship_tgt2, 12_729, jita_sys, origin + DVec3::new(4_000.0, 0.0, 0.0)).await;
    for &tid in &[ship_tgt, ship_tgt2] {
        let mut dmg_cs = sys.get_combat_state(tid).await.unwrap().unwrap();
        dmg_cs.max_armor_hp = 10_000.0;
        dmg_cs.armor_hp = 1_000.0;
        sys.set_combat_state(tid, dmg_cs).await.unwrap();
        sys.add_target_lock(ship_src, eve_destiny::TargetLock {
            target_id: tid, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(100_000.0),
        }).await.unwrap();
    }

    rpc(&ctx, &mut sess_src, "dogmaIM", "Activate", vec![
        PyRep::Integer(77_021), PyRep::String("remoteArmorRepairFalloff".into()), PyRep::Integer(ship_tgt), PyRep::Integer(1),
    ]).await.expect("Activate Mutadaptive RAR");
    // Cycle 1 (6s) -> +784.0 HP (1.0x with Zarmazd Precursor Cruiser L5 +100% bonus), Cycle 2 (6s) -> +862.4 HP (1.1x)
    for _ in 0..6 { sys.step_manual(1.0).await.unwrap(); }
    let a_c1 = sys.get_combat_state(ship_tgt).await.unwrap().unwrap().armor_hp;
    assert!((a_c1 - 1_784.0).abs() < 1.0, "Cycle 1 must repair 784 HP (392 * 2x Zarmazd bonus): got {a_c1}");
    for _ in 0..6 { sys.step_manual(1.0).await.unwrap(); }
    let a_c2 = sys.get_combat_state(ship_tgt).await.unwrap().unwrap().armor_hp;
    assert!((a_c2 - (1_784.0 + 784.0 * 1.1)).abs() < 1.0, "Cycle 2 must spool +10% (862.4 HP): got {a_c2}");

    // Unlocking target resets spool immediately back to 1.0x (784.0)
    rpc(&ctx, &mut sess_src, "dogmaIM", "RemoveTargets", vec![PyRep::List(vec![PyRep::Integer(ship_tgt)])]).await.unwrap();
    let rep_after_unlock = attr_from_item_info(
        &rpc(&ctx, &mut sess_src, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(77_021)]).await.unwrap(),
        84,
    );
    assert!((rep_after_unlock - 784.0).abs() < 1e-2, "Unlocking target must reset Mutadaptive spool to 784.0: got {rep_after_unlock}");

    // Target switch resets spool back to 1.0x on new target
    sys.add_target_lock(ship_src, eve_destiny::TargetLock {
        target_id: ship_tgt, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(100_000.0),
    }).await.unwrap();
    rpc(&ctx, &mut sess_src, "dogmaIM", "Activate", vec![
        PyRep::Integer(77_021), PyRep::String("remoteArmorRepairFalloff".into()), PyRep::Integer(ship_tgt), PyRep::Integer(1),
    ]).await.unwrap();
    for _ in 0..12 { sys.step_manual(1.0).await.unwrap(); }
    rpc(&ctx, &mut sess_src, "dogmaIM", "Activate", vec![
        PyRep::Integer(77_021), PyRep::String("remoteArmorRepairFalloff".into()), PyRep::Integer(ship_tgt2), PyRep::Integer(1),
    ]).await.expect("Switch Mutadaptive RAR target to ship_tgt2");
    for _ in 0..6 { sys.step_manual(1.0).await.unwrap(); }
    let a_tgt2_c1 = sys.get_combat_state(ship_tgt2).await.unwrap().unwrap().armor_hp;
    assert!((a_tgt2_c1 - 1_784.0).abs() < 1.0, "Switching target must reset spool to 1.0x (784 HP): got {a_tgt2_c1}");
    rpc(&ctx, &mut sess_src, "dogmaIM", "Deactivate", vec![PyRep::Integer(77_021)]).await.unwrap();

    // 4. Blockade Runner Cargo Scan Immunity (`feat:ships.blockade-runner-cargo-scan-immunity`)
    let scanner_id: i64 = 77_031;
    give_item(&ctx, scanner_id, 2_038, char_src, ship_src, InventoryFlag::MedSlot0, 1); // Cargo Scanner II
    ctx.db.with_mut_snapshot(|snap| { snap.module_online_states.insert(scanner_id, true); });
    ctx.invalidate_ship_fitting(ship_src);

    // Crane (12729, Blockade Runner) carrying Tritanium & PLEX returns empty scan
    give_item(&ctx, 77_041, 34, char_tgt2, ship_tgt2, InventoryFlag::Cargo, 500);
    give_item(&ctx, 77_042, 44_992, char_tgt2, ship_tgt2, InventoryFlag::Cargo, 1);
    let scan_crane = rpc(&ctx, &mut sess_src, "dogmaIM", "Activate", vec![
        PyRep::Integer(scanner_id), PyRep::String("cargoScan".into()), PyRep::Integer(ship_tgt2), PyRep::Integer(1),
    ]).await.expect("Scan Crane Blockade Runner");
    assert_eq!(scan_crane.dict_get("cargoScanImmune"), Some(&PyRep::Bool(true)));
    assert_eq!(scan_crane.dict_get("totalScannedQuantity"), Some(&PyRep::Integer(0)));
    assert_eq!(scan_crane.dict_get("scannedItems"), Some(&PyRep::List(Vec::new())));

    // Bustard (12731, Deep Space Transport, cargoScanResistance = 0) exposes cargo manifest
    ctx.db.with_mut_snapshot(|snap| { snap.items.get_mut(&ship_tgt).unwrap().type_id = 12_731; });
    give_item(&ctx, 77_043, 34, char_tgt, ship_tgt, InventoryFlag::Cargo, 500);
    let scan_bustard = rpc(&ctx, &mut sess_src, "dogmaIM", "Activate", vec![
        PyRep::Integer(scanner_id), PyRep::String("cargoScan".into()), PyRep::Integer(ship_tgt), PyRep::Integer(1),
    ]).await.expect("Scan Bustard DST");
    assert_eq!(scan_bustard.dict_get("cargoScanImmune"), None);
    assert_eq!(scan_bustard.dict_get("totalScannedQuantity"), Some(&PyRep::Integer(500)));
}
