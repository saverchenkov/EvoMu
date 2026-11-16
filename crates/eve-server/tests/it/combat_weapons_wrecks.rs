#![allow(unused_imports)]
//! End-to-End Integration Tests for SDE-Driven Combat Mechanics, Turrets, Missiles, Smartbombs, Bombs, Breacher Pods & Shield Recharge (`C-1, C-2`).

use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_destiny::{Ball, MovementMode, TargetLock};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{
    compute_probe_scan_signal_strength, dispatch_combat_rpc, evaluate_effective_signature_radius, passive_shield_recharge_rate_hps, step_combat_system,
    step_passive_shield_recharge, with_combat_runtime,
};
use glam::DVec3;
use std::collections::HashMap;

use self::triglavian_entropic_spool_edencom_vorton::*;

#[tokio::test]
async fn test_c1_turret_families_hit_formula_wrecking_shots_and_crystal_degradation() {
    // parity: feat:modules.turret-hybrid
    // parity: feat:modules.turret-projectile
    // parity: feat:modules.turret-laser
    // parity: feat:modules.turret-hit-chance
    // parity: feat:dogma.resistances
    eve_testkit::attest(&[
        "feat:modules.turret-hybrid",
        "feat:modules.turret-projectile",
        "feat:modules.turret-laser",
        "feat:modules.turret-hit-chance",
        "feat:dogma.resistances",
    ]);

    let ctx = ServerContext::new();
    let char_id = 90_000_001;
    let ship_id = 100_000_001;
    let sys_id = 30_000_142;
    let mut sess = setup_pilot_session(
        &ctx,
        7101,
        char_id,
        ship_id,
        587, // Rifter
        sys_id,
        DVec3::ZERO,
    )
    .await;

    // Spawn a high-HP target ball at 1,500m
    let target_id = 500_001;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let mut target_cs = ctx.build_type_combat_state(24690); // Maelstrom
    target_cs.max_shield_hp = 50_000.0;
    target_cs.shield_hp = 50_000.0;
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: target_id,
                type_id: 24690,
                position: DVec3::new(1_500.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 120.0,
                inertia: 0.5,
                mass: 100_000_000.0,
                radius: 420.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            target_cs,
        )
        .await;

    // 1. Fit Hybrid (125mm Railgun I 10678 + Antimatter Charge S 222),
    //    Projectile (200mm AutoCannon I 486 + EMP S 185),
    //    Energy Laser (Small Focused Pulse Laser II 3041 + Multifrequency S 246 / Conflagration S 12565 / Imperial Navy Multifrequency S 23071)
    let railgun_id = 200_101;
    let rail_ammo_id = 200_102;
    fit_module_and_charge(&ctx, char_id, ship_id, railgun_id, 10678, InventoryFlag::HiSlot0, Some((rail_ammo_id, 222, 100)));

    let autocannon_id = 200_103;
    let ac_ammo_id = 200_104;
    fit_module_and_charge(&ctx, char_id, ship_id, autocannon_id, 486, InventoryFlag::HiSlot1, Some((ac_ammo_id, 185, 100)));

    let pulse_t1_id = 200_105;
    let crystal_t1_id = 200_106;
    fit_module_and_charge(
        &ctx,
        char_id,
        ship_id,
        pulse_t1_id,
        3041,
        InventoryFlag::HiSlot2,
        Some((crystal_t1_id, 246, 1)), // Multifrequency S (T1 crystal)
    );

    let pulse_t2_id = 200_107;
    let crystal_t2_id = 200_108;
    fit_module_and_charge(
        &ctx,
        char_id,
        ship_id,
        pulse_t2_id,
        3041,
        InventoryFlag::HiSlot3,
        Some((crystal_t2_id, 12565, 1)), // Conflagration S (T2 crystal)
    );

    let pulse_fac_id = 200_109;
    let crystal_fac_id = 200_110;
    fit_module_and_charge(
        &ctx,
        char_id,
        ship_id,
        pulse_fac_id,
        3041,
        InventoryFlag::HiSlot4,
        Some((crystal_fac_id, 23071, 1)), // Imperial Navy Multifrequency S (Faction crystal)
    );

    // 2. Fire Hybrid turret (consumes capacitor and 1 Antimatter Charge S)
    let _ = sys_handle.add_target_lock(ship_id, TargetLock { target_id, lock_time_ms: 100.0, remaining_lock_ms: 0.0, locked: true, max_range_m: None }).await;
    let kw = HashMap::new();
    ctx.set_invention_roll_override(Some(0.50));
    let rail_res = dispatch_combat_rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(railgun_id), PyRep::Integer(target_id)], &kw)
        .await
        .expect("Railgun Activate");
    let rail_cap_before = dict_get(&rail_res, "capacitorBefore").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let rail_cap_after = dict_get(&rail_res, "capacitorAfter").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let rail_dmg = dict_get(&rail_res, "appliedDamage").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert!(rail_cap_before > rail_cap_after, "Hybrid railgun must consume capacitor");
    assert!(rail_dmg > 0.0, "Hybrid railgun must apply kinetic/thermal damage");
    let rem_rail_ammo = ctx.db.with_snapshot(|s| s.items[&rail_ammo_id].quantity);
    assert_eq!(rem_rail_ammo, 99, "Hybrid railgun must consume 1 charge per cycle");

    // 3. Fire Projectile AutoCannon (consumes 0 capacitor and 1 EMP S)
    let ac_res = dispatch_combat_rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(autocannon_id), PyRep::Integer(target_id)], &kw)
        .await
        .expect("AutoCannon Activate");
    let ac_cap_before = dict_get(&ac_res, "capacitorBefore").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let ac_cap_after = dict_get(&ac_res, "capacitorAfter").and_then(|v| v.as_f64()).unwrap_or(-1.0);
    let ac_dmg = dict_get(&ac_res, "appliedDamage").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert!((ac_cap_before - ac_cap_after).abs() < 1e-9, "Projectile turret must consume 0 capacitor");
    assert!(ac_dmg > 0.0, "Projectile turret must apply damage");
    let rem_ac_ammo = ctx.db.with_snapshot(|s| s.items[&ac_ammo_id].quantity);
    assert_eq!(rem_ac_ammo, 99, "Projectile turret must consume 1 round per cycle");

    // 4. Wrecking shot (`hitRoll < 0.01` -> 3.0x base damage) vs normal hit (`hitRoll = 0.50`)
    ctx.advance_secs(5);
    let kw_wreck = HashMap::new();
    ctx.set_invention_roll_override(Some(0.005));
    let wreck_res = dispatch_combat_rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(autocannon_id), PyRep::Integer(target_id)], &kw_wreck)
        .await
        .expect("Wrecking Activate");
    let is_wrecking = dict_get(&wreck_res, "isWreckingShot").and_then(|v| v.as_bool()).unwrap_or(false);
    let wreck_dmg = dict_get(&wreck_res, "appliedDamage").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert!(is_wrecking, "hitRoll < 0.01 must register a Wrecking Hit");
    assert!(
        (wreck_dmg / ac_dmg - (3.0 / 0.995)).abs() < 0.05,
        "Wrecking shot must deal 3.0x base damage compared to 0.995x normal hit (got {wreck_dmg} vs {ac_dmg})"
    );

    // 5. Miss when `hitRoll > hitChance` at high range/transversal within optimal + 3*falloff
    let far_target_id = 500_002;
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: far_target_id,
                type_id: 587,
                position: DVec3::new(15_000.0, 0.0, 0.0),
                velocity: DVec3::new(0.0, 2_500.0, 0.0),
                max_velocity: 2_500.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 25.0,
                speed_fraction: 1.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(587),
        )
        .await;
    let _ = sys_handle
        .add_target_lock(ship_id, TargetLock { target_id: far_target_id, lock_time_ms: 100.0, remaining_lock_ms: 0.0, locked: true, max_range_m: None })
        .await;
    ctx.advance_secs(5);
    let kw_miss = HashMap::new();
    ctx.set_invention_roll_override(Some(0.95));
    let miss_res = dispatch_combat_rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(autocannon_id), PyRep::Integer(far_target_id)], &kw_miss)
        .await
        .expect("Miss Activate");
    let miss_dmg = dict_get(&miss_res, "appliedDamage").and_then(|v| v.as_f64()).unwrap_or(1.0);
    let hit_chance = dict_get(&miss_res, "hitChance").and_then(|v| v.as_f64()).unwrap_or(1.0);
    assert!(hit_chance < 0.2, "High range + transversal must yield low hit chance");
    assert!(miss_dmg.abs() < 1e-9, "Roll above hitChance must miss and deal 0 damage");

    // 6. Crystal degradation:
    // - T1 Multifrequency S (246): `crystalsGetDamaged` is 0 -> 0 degradation
    // - Faction Imperial Navy Multifrequency S (23071): `0.00025` per cycle (1.0 probability)
    // - T2 Conflagration S (12565): `0.01` when `volatilityRoll < 0.1`
    let kw_crys = HashMap::new();
    ctx.set_invention_roll_override(Some(0.20));

    let t1_res = dispatch_combat_rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(pulse_t1_id), PyRep::Integer(target_id)], &kw_crys)
        .await
        .expect("T1 Laser Activate");
    let t1_deg = dict_get(&t1_res, "crystalDamageFraction").and_then(|v| v.as_f64()).unwrap_or(1.0);
    assert!(t1_deg.abs() < 1e-9, "T1 frequency crystals must never degrade");

    let fac_res = dispatch_combat_rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(pulse_fac_id), PyRep::Integer(target_id)], &kw_crys)
        .await
        .expect("Faction Laser Activate");
    let fac_deg = dict_get(&fac_res, "crystalDamageFraction").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert!((fac_deg - 0.00025).abs() < 1e-6, "Faction crystal must degrade by 0.00025 per shot (got {fac_deg})");

    ctx.set_mechanic_rng_seed(Some(1));
    let t2_no_proc = dispatch_combat_rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(pulse_t2_id), PyRep::Integer(target_id)], &kw_crys)
        .await
        .expect("T2 Laser Activate non-proc");
    let t2_no_proc_deg = dict_get(&t2_no_proc, "crystalDamageFraction").and_then(|v| v.as_f64()).unwrap_or(1.0);
    assert!(t2_no_proc_deg.abs() < 1e-9, "T2 Conflagration S must not degrade when mechanic_rng roll >= 0.10 (got {t2_no_proc_deg})");

    ctx.advance_secs(5);
    ctx.set_mechanic_rng_seed(Some(10));
    let t2_res = dispatch_combat_rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(pulse_t2_id), PyRep::Integer(target_id)], &kw_crys)
        .await
        .expect("T2 Laser Activate");
    let t2_deg = dict_get(&t2_res, "crystalDamageFraction").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert!((t2_deg - 0.01).abs() < 1e-6, "T2 Conflagration S must degrade by 0.01 on volatility proc (got {t2_deg})");

    // Query crystal damage via `invbroker.GetDamageForCrystals`
    let crys_map = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "invbroker",
        "GetDamageForCrystals",
        &[PyRep::List(vec![PyRep::Integer(crystal_t1_id), PyRep::Integer(crystal_t2_id), PyRep::Integer(crystal_fac_id)])],
        &HashMap::new(),
    )
    .await
    .expect("GetDamageForCrystals");
    if let PyRep::Dict(entries) = crys_map {
        let map: HashMap<i64, f64> = entries.into_iter().filter_map(|(k, v)| Some((k.as_i64()?, v.as_f64()?))).collect();
        assert_eq!(map.get(&crystal_t1_id).copied().unwrap_or(-1.0), 0.0);
        assert!((map.get(&crystal_t2_id).copied().unwrap_or(0.0) - 0.01).abs() < 1e-6);
        assert!((map.get(&crystal_fac_id).copied().unwrap_or(0.0) - 0.00025).abs() < 1e-6);
    } else {
        panic!("Expected Dict from GetDamageForCrystals");
    }

    // Shatter T2 crystal when damage reaches 1.0
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.items.get_mut(&crystal_t2_id) {
            c.dynamic_attributes.insert(3, 0.995);
        }
    });
    ctx.advance_secs(5);
    ctx.set_mechanic_rng_seed(Some(10));
    let shatter_res = dispatch_combat_rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(pulse_t2_id), PyRep::Integer(target_id)], &kw_crys)
        .await
        .expect("T2 Shatter Activate");
    let shattered = dict_get(&shatter_res, "crystalShattered").and_then(|v| v.as_bool()).unwrap_or(false);
    assert!(shattered, "T2 crystal must shatter when damage >= 1.0");
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&crystal_t2_id)), "Shattered crystal must be removed from inventory");
}

#[tokio::test]
async fn test_c2_missiles_drf_smartbombs_bombs_breacher_pods_damage_layers_and_shield_recharge() {
    // parity: feat:modules.missile-launchers
    // parity: feat:modules.missile-application
    // parity: feat:modules.missile-defender
    // parity: feat:modules.missile-fof
    // parity: feat:modules.smartbomb
    // parity: feat:modules.bomb-launcher
    // parity: feat:modules.capital-weapons
    // parity: feat:modules.breacher-pods
    // parity: feat:combat.missile-flight-physics-destruction
    // parity: feat:combat.bomb-damage-void-lockbreaker
    // parity: feat:combat.damage-pipeline
    // parity: feat:combat.damage-log
    // parity: feat:dogma.shield-recharge
    eve_testkit::attest(&[
        "feat:modules.missile-launchers",
        "feat:modules.missile-application",
        "feat:modules.missile-defender",
        "feat:modules.missile-fof",
        "feat:modules.smartbomb",
        "feat:modules.bomb-launcher",
        "feat:modules.capital-weapons",
        "feat:modules.breacher-pods",
        "feat:combat.missile-flight-physics-destruction",
        "feat:combat.bomb-damage-void-lockbreaker",
        "feat:combat.damage-pipeline",
        "feat:combat.damage-log",
        "feat:dogma.shield-recharge",
    ]);

    let ctx = ServerContext::new();
    let char_id = 90_000_001;
    let victim_char_id = 90_000_002;
    let ship_id = 100_000_001;
    let victim_ship_id = 100_000_002;
    let lowsec_sys_id = 30_002_537; // Amamake (low-sec < 0.45 so bombs are legal)

    let mut sess = setup_pilot_session(
        &ctx,
        7103,
        char_id,
        ship_id,
        11377, // Nemesis Stealth Bomber
        lowsec_sys_id,
        DVec3::ZERO,
    )
    .await;
    let mut victim_sess = setup_pilot_session(
        &ctx,
        7104,
        victim_char_id,
        victim_ship_id,
        587, // Rifter
        lowsec_sys_id,
        DVec3::new(3_000.0, 0.0, 0.0),
    )
    .await;

    let sys_handle = ctx.systems.get_or_spawn(lowsec_sys_id);

    // 1. Missile DRF formula (`feat:combat.missile-damage-formula`) & Missile Launchers (`feat:combat.missile-weapons`):
    // Compare XL Torpedo Launcher I (20539) + Scourge XL Torpedo (17859, exp_radius=1000m, exp_vel=66m/s)
    // against a small fast frigate vs a large stationary battleship.
    let xl_launcher_id = 202_001;
    let xl_ammo_id = 202_002;
    fit_module_and_charge(&ctx, char_id, ship_id, xl_launcher_id, 20539, InventoryFlag::HiSlot0, Some((xl_ammo_id, 17859, 50)));

    let fast_frig_id = 520_001;
    let mut frig_cs = ctx.build_type_combat_state(587);
    frig_cs.max_shield_hp = 50_000.0;
    frig_cs.shield_hp = 50_000.0;
    frig_cs.signature_radius = 35.0;
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: fast_frig_id,
                type_id: 587,
                position: DVec3::new(4_000.0, 0.0, 0.0),
                velocity: DVec3::new(0.0, 450.0, 0.0),
                max_velocity: 450.0,
                inertia: 2.0,
                mass: 1_000_000.0,
                radius: 35.0,
                speed_fraction: 1.0,
                mode: MovementMode::Stop,
            },
            frig_cs,
        )
        .await;

    let big_bs_id = 520_002;
    let mut bs_cs = ctx.build_type_combat_state(24690);
    bs_cs.max_shield_hp = 50_000.0;
    bs_cs.shield_hp = 50_000.0;
    bs_cs.signature_radius = 1_200.0;
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: big_bs_id,
                type_id: 24690,
                position: DVec3::new(4_000.0, 500.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 100.0,
                inertia: 1.0,
                mass: 100_000_000.0,
                radius: 1_200.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            bs_cs,
        )
        .await;
    for tid in [fast_frig_id, big_bs_id, victim_ship_id] {
        let _ = sys_handle
            .add_target_lock(ship_id, TargetLock { target_id: tid, lock_time_ms: 100.0, remaining_lock_ms: 0.0, locked: true, max_range_m: None })
            .await;
    }

    let res_frig =
        dispatch_combat_rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(xl_launcher_id), PyRep::Integer(fast_frig_id)], &HashMap::new())
            .await
            .expect("XL Torpedo vs Frigate");
    ctx.advance_secs(15);
    let res_bs = dispatch_combat_rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(xl_launcher_id), PyRep::Integer(big_bs_id)], &HashMap::new())
        .await
        .expect("XL Torpedo vs Battleship");
    let dmg_frig = dict_get(&res_frig, "appliedDamage").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let dmg_bs = dict_get(&res_bs, "appliedDamage").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert!(dmg_frig < dmg_bs * 0.15, "XL Torpedo DRF must heavily reduce damage against small fast frigate ({dmg_frig} vs {dmg_bs})");

    // 2. Auto-Targeting FoF Light Missile (`Light Missile Launcher I` 499 + `Scourge Auto-Targeting Light Missile I` 1810)
    //    and Deferred In-Flight Missile ball + Smartbomb mid-flight destruction (`feat:combat.smartbombs`)
    let lml_id = 202_003;
    let fof_ammo_id = 202_004;
    fit_module_and_charge(&ctx, char_id, ship_id, lml_id, 499, InventoryFlag::HiSlot1, Some((fof_ammo_id, 1810, 50)));
    // Fire FoF missile without specifying a target ID
    let fof_res = dispatch_combat_rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(lml_id)], &HashMap::new()).await.expect("FoF Missile Activate");
    assert!(
        dict_get(&fof_res, "appliedDamage").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0,
        "Auto-targeting FoF missile must automatically acquire and damage hostile target"
    );

    // Spawn an in-flight missile and destroy it mid-flight with a Smartbomb (`Small EMP Smartbomb I` 1563)
    ctx.advance_secs(10);
    let kw_defer = HashMap::new();
    let defer_res = dispatch_combat_rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(lml_id), PyRep::Integer(victim_ship_id)], &kw_defer)
        .await
        .expect("Deferred Missile");
    let missile_ball_id = dict_get(&defer_res, "missileBallID").and_then(|v| v.as_i64()).expect("missileBallID");
    assert!(sys_handle.get_ball_snapshot(missile_ball_id).await.unwrap().is_some(), "In-flight missile ball must exist in Destiny ballpark");

    let smartbomb_id = 202_005;
    fit_module_and_charge(
        &ctx,
        char_id,
        ship_id,
        smartbomb_id,
        3993, // Large EMP Smartbomb I (300 EM damage, 6,000m radius)
        InventoryFlag::HiSlot2,
        None,
    );
    let sb_res =
        dispatch_combat_rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(smartbomb_id)], &HashMap::new()).await.expect("Smartbomb Activate");
    let destroyed_missiles = match dict_get(&sb_res, "destroyedMissileIDs") {
        Some(PyRep::List(v)) => v.len(),
        _ => 0,
    };
    assert_eq!(destroyed_missiles, 1, "Smartbomb AoE must destroy in-flight missile within blast radius");
    assert!(sys_handle.get_ball_snapshot(missile_ball_id).await.unwrap().is_none(), "Destroyed missile ball must be removed from ballpark");

    // Also verify Defender Missile (`Defender Launcher I` 44102 + `Defender Missile I` 32782) intercepts an incoming missile
    ctx.advance_secs(10);
    let defer_res2 = dispatch_combat_rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(lml_id), PyRep::Integer(victim_ship_id)], &kw_defer)
        .await
        .expect("Deferred Missile 2");
    let missile_ball_id2 = dict_get(&defer_res2, "missileBallID").and_then(|v| v.as_i64()).expect("missileBallID 2");
    let defender_id = 202_006;
    let defender_ammo_id = 202_007;
    fit_module_and_charge(&ctx, victim_char_id, victim_ship_id, defender_id, 44102, InventoryFlag::HiSlot0, Some((defender_ammo_id, 32782, 20)));
    let def_res =
        dispatch_combat_rpc(&ctx, &mut victim_sess, "dogmaIM", "Activate", &[PyRep::Integer(defender_id)], &HashMap::new()).await.expect("Defender Activate");
    assert_eq!(
        dict_get(&def_res, "interceptedMissileID").and_then(|v| v.as_i64()),
        Some(missile_ball_id2),
        "Defender missile must intercept and destroy the incoming hostile missile"
    );

    // 3. Bombs (`feat:combat.bombs`):
    // - Blocked in high-sec (`BombDisallowedInHighSec`)
    // - Allowed in low-sec/null-sec, detonating at 30km with 15km blast radius (`Void Bomb` 27924 drains cap, `Lockbreaker Bomb` 27922 breaks locks, `Concussion Bomb` 27912 deals AoE damage)
    let bomb_launcher_id = 202_008;
    let bomb_ammo_id = 202_009;
    fit_module_and_charge(
        &ctx,
        char_id,
        ship_id,
        bomb_launcher_id,
        27914, // Bomb Launcher I
        InventoryFlag::HiSlot3,
        Some((bomb_ammo_id, 27924, 10)), // Void Bomb
    );

    // Temporarily test high-sec block
    let hs_handle = ctx.systems.get_or_spawn(30_000_142);
    let _ = hs_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 11377,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 3.0,
                mass: 1_200_000.0,
                radius: 35.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(11377),
        )
        .await;
    sess.solar_system_id = Some(30_000_142);
    let hs_bomb_err = dispatch_combat_rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(bomb_launcher_id)], &HashMap::new())
        .await
        .expect_err("Bombs must be disallowed in high-sec");
    assert_eq!(hs_bomb_err.code, "BombDisallowedInHighSec");
    sess.solar_system_id = Some(lowsec_sys_id);

    // Place a target at 29km along X-axis (within 15km of the 30km detonation point) with active target lock and capacitor
    let bomb_target_id = 520_003;
    let mut bt_cs = ctx.build_type_combat_state(24690);
    bt_cs.capacitor = 2_000.0;
    bt_cs.max_capacitor = 2_000.0;
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: bomb_target_id,
                type_id: 24690,
                position: DVec3::new(29_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 100.0,
                inertia: 1.0,
                mass: 50_000_000.0,
                radius: 400.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            bt_cs,
        )
        .await;
    let _ = sys_handle
        .add_target_lock(bomb_target_id, TargetLock { target_id: ship_id, lock_time_ms: 1_000.0, remaining_lock_ms: 0.0, locked: true, max_range_m: None })
        .await;

    // Fire Void Bomb (drains 1,800 GJ capacitor)
    let void_res =
        dispatch_combat_rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(bomb_launcher_id)], &HashMap::new()).await.expect("Void Bomb Activate");
    let cap_after_void = sys_handle.get_combat_state(bomb_target_id).await.unwrap().unwrap().capacitor;
    assert!(
        (cap_after_void - 200.0).abs() < 1e-3,
        "Void Bomb must neutralize 1,800 GJ capacitor from target within 15km blast radius (remaining={cap_after_void})"
    );
    assert!(dict_get(&void_res, "totalCapNeutralized").and_then(|v| v.as_f64()).unwrap_or(0.0) >= 1_800.0);

    // Switch charge to Lockbreaker Bomb (27922) and verify target locks are broken
    ctx.db.with_mut_snapshot(|s| {
        if let Some(c) = s.items.get_mut(&bomb_ammo_id) {
            c.type_id = 27922;
        }
    });
    ctx.advance_secs(120);
    let _ = dispatch_combat_rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(bomb_launcher_id)], &HashMap::new())
        .await
        .expect("Lockbreaker Bomb Activate");
    let locks_after = sys_handle.get_target_locks(bomb_target_id).await.unwrap();
    assert!(locks_after.is_empty(), "Lockbreaker Bomb must break active target locks on ships in blast radius");

    // 4. Breacher Pods (`feat:combat.breacher-pods`):
    // Small Breacher Pod Launcher (85084) + SCARAB Breacher Pod S (85088)
    // Non-stacking DoT: firing a second breacher pod refreshes duration rather than stacking tick damage.
    let breacher_id = 202_010;
    let breacher_ammo_id = 202_011;
    fit_module_and_charge(&ctx, char_id, ship_id, breacher_id, 85084, InventoryFlag::HiSlot4, Some((breacher_ammo_id, 85088, 20)));
    let bp1 = dispatch_combat_rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(breacher_id), PyRep::Integer(victim_ship_id)], &HashMap::new())
        .await
        .expect("Breacher Pod 1");
    let tick1 = dict_get(&bp1, "breacherTickDPS").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert!(tick1 > 0.0, "Breacher pod must apply initial DoT tick");
    let active_dots = with_combat_runtime(&ctx, |rt| rt.active_breacher_dots.len());
    assert_eq!(active_dots, 1, "Single active breacher DoT entry per target");
    // Step 5 seconds of combat system and verify DoT ticks without stacking when re-applied
    ctx.advance_secs(10);
    let _ = dispatch_combat_rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(breacher_id), PyRep::Integer(victim_ship_id)], &HashMap::new())
        .await
        .expect("Breacher Pod 2");
    assert_eq!(with_combat_runtime(&ctx, |rt| rt.active_breacher_dots.len()), 1, "Multiple breacher pods on the same target must refresh rather than stack");
    step_combat_system(&ctx, lowsec_sys_id, 5.0).await;

    // 5. Damage application layers (`feat:combat.damage-application-layers`) & Passive Shield Recharge (`feat:dogma.shield-recharge`)
    // Verify combat log notifications (`OnDamageMessage`, `OnDamageMessages`, `OnDamageStateChange`) on attacker session
    assert!(
        sess.notifications.iter().any(|(m, _)| m == "OnDamageMessage" || m == "OnDamageStateChange"),
        "Attacker session must receive OnDamageMessage / OnDamageStateChange"
    );

    // Verify non-linear passive shield recharge peaks around 25% shield capacity
    let rate_10 = passive_shield_recharge_rate_hps(100.0, 1_000.0, 100.0);
    let rate_25 = passive_shield_recharge_rate_hps(250.0, 1_000.0, 100.0);
    let rate_80 = passive_shield_recharge_rate_hps(800.0, 1_000.0, 100.0);
    assert!(
        rate_25 > rate_10 && rate_25 > rate_80,
        "Passive shield recharge must peak at 25% shield capacity (rate_10={rate_10}, rate_25={rate_25}, rate_80={rate_80})"
    );
    let (after_sh, recharged_delta) =
        step_passive_shield_recharge(&ctx, lowsec_sys_id, Some(victim_char_id), victim_ship_id, 100.0, 5.0).await.expect("step_passive_shield_recharge");
    assert!(recharged_delta > 0.0 && after_sh >= recharged_delta, "Passive shield recharge step must restore shield HP");
}

pub(crate) mod triglavian_entropic_spool_edencom_vorton {
//! End-to-End Integration Tests for SDE-Driven Combat Mechanics, Turrets, Missiles, Smartbombs, Bombs, Breacher Pods & Shield Recharge (`C-1, C-2`).

use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_destiny::{Ball, MovementMode, TargetLock};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{
    compute_probe_scan_signal_strength, dispatch_combat_rpc, evaluate_effective_signature_radius, passive_shield_recharge_rate_hps, step_combat_system,
    step_passive_shield_recharge, with_combat_runtime,
};
use glam::DVec3;
use std::collections::HashMap;

pub(crate) fn dict_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(map) => map.get(key),
        PyRep::Dict(entries) => entries.iter().find_map(|(k, v)| (k.as_str() == Some(key)).then_some(v)),
        _ => None,
    }
}

pub(crate) async fn setup_pilot_session(
    ctx: &ServerContext,
    client_id: u64,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    solar_system_id: u32,
    pos: DVec3,
) -> SessionState {
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = solar_system_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
            ch.balance = IskCents::from_isk(500_000_000);
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
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
    let sig_radius = ctx.sde.types.get(&ship_type_id).and_then(|t| t.default_attributes.get(&552).copied()).unwrap_or(40.0);
    let ship_ball = Ball {
        id: ship_id,
        type_id: ship_type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 3.0,
        mass: 1_200_000.0,
        radius: sig_radius,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = sys_handle.spawn_ball_with_combat(ship_ball, ctx.build_type_combat_state(ship_type_id)).await;

    let mut sess = SessionState::new(client_id);
    let (user_id, corp_id) = ctx.db.with_snapshot(|snap| snap.characters.get(&char_id).map(|c| (c.user_id, c.corporation_id)).unwrap_or((1, 1_000_002)));
    sess.user_id = Some(user_id);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(solar_system_id);
    sess.ship_id = Some(ship_id);
    sess.station_id = None;
    let _ = ctx.db.acquire_online_lock(user_id, client_id, Some(char_id), true, ctx.now_unix());
    sess
}

pub(crate) fn fit_module_and_charge(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    module_item_id: i64,
    module_type_id: u32,
    flag: InventoryFlag,
    charge_opt: Option<(i64, u32, i32)>,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            module_item_id,
            InventoryItemRecord {
                item_id: module_item_id,
                type_id: module_type_id,
                owner_id: char_id,
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
        if let Some((charge_id, charge_type_id, qty)) = charge_opt {
            snap.items.insert(
                charge_id,
                InventoryItemRecord {
                    item_id: charge_id,
                    type_id: charge_type_id,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag,
                    quantity: qty,
                    singleton: false,
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
}

#[tokio::test]
async fn test_c1_triglavian_entropic_spool_and_edencom_vorton_arc_and_sig_radius() {
    // parity: feat:modules.turret-precursor
    // parity: feat:modules.vorton-projector
    // parity: feat:dogma.signature-radius
    eve_testkit::attest(&["feat:modules.turret-precursor", "feat:modules.vorton-projector", "feat:dogma.signature-radius"]);

    let ctx = ServerContext::new();
    let char_id = 90_000_001;
    let ship_id = 100_000_001;
    let sys_id = 30_000_142;
    let mut sess = setup_pilot_session(
        &ctx,
        7102,
        char_id,
        ship_id,
        47269, // Damavik
        sys_id,
        DVec3::ZERO,
    )
    .await;

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    // Spawn 6 target balls clustered within 4km of each other so Vorton can arc between them
    let mut target_ids = Vec::new();
    for idx in 0..6 {
        let tid = 510_001 + idx;
        target_ids.push(tid);
        let mut cs = ctx.build_type_combat_state(24690);
        cs.max_shield_hp = 80_000.0;
        cs.shield_hp = 80_000.0;
        let _ = sys_handle
            .spawn_ball_with_combat(
                Ball {
                    id: tid,
                    type_id: 24690,
                    position: DVec3::new(2_000.0 + (idx as f64) * 500.0, 0.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 100.0,
                    inertia: 1.0,
                    mass: 50_000_000.0,
                    radius: 400.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                cs,
            )
            .await;
    }

    // 1. Triglavian Entropic Disintegrator (`Light Entropic Disintegrator I` 47272 + `Meson Exotic Plasma S` 47925)
    let entropic_id = 201_001;
    let entropic_ammo_id = 201_002;
    fit_module_and_charge(&ctx, char_id, ship_id, entropic_id, 47272, InventoryFlag::HiSlot0, Some((entropic_ammo_id, 47925, 200)));

    let kw = HashMap::new();
    ctx.set_invention_roll_override(Some(0.30));

    for &tid in &target_ids {
        let _ = sys_handle
            .add_target_lock(ship_id, TargetLock { target_id: tid, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(100_000.0) })
            .await;
    }

    let hp0_init = sys_handle.get_combat_state(target_ids[0]).await.unwrap().unwrap().shield_hp;

    // Cycle 0: spoolMultiplier == 1.0
    let c0 = dispatch_combat_rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(entropic_id), PyRep::Integer(target_ids[0])], &kw)
        .await
        .expect("Entropic Cycle 0");
    let m0 = dict_get(&c0, "spoolMultiplier").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert!((m0 - 1.0).abs() < 1e-6, "First entropic cycle must start at 1.0x");
    let hp_after_c0 = sys_handle.get_combat_state(target_ids[0]).await.unwrap().unwrap().shield_hp;
    let loss_c0 = hp0_init - hp_after_c0;
    assert!(loss_c0 > 0.0, "Cycle 0 must deal damage to target");

    // Activating already active module on same target returns AlreadyActive
    let err_already = dispatch_combat_rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(entropic_id), PyRep::Integer(target_ids[0])], &kw).await;
    assert!(
        matches!(err_already, Err(ref e) if e.code == "AlreadyActive"),
        "Activating already-active entropic module on same target must fail with AlreadyActive"
    );

    // Step 1 cycle (3.5s) on same target: spoolMultiplier == 1.07 (`attr 2733 = 0.07`)
    step_combat_system(&ctx, sys_id, 3.5).await;
    let m1 = eve_server::services::combat_weapons::get_entropic_spool_multiplier(&ctx, ship_id, entropic_id);
    assert!((m1 - 1.07).abs() < 1e-6, "Second entropic cycle must spool to 1.07x (got {m1})");
    let hp_after_c1 = sys_handle.get_combat_state(target_ids[0]).await.unwrap().unwrap().shield_hp;
    let loss_c1 = hp_after_c0 - hp_after_c1;
    assert!(loss_c1 > loss_c0, "Cycle 1 HP loss ({loss_c1}) must exceed cycle 0 loss ({loss_c0})");

    // Step 1 more cycle: spoolMultiplier == 1.14
    step_combat_system(&ctx, sys_id, 3.5).await;
    let hp_after_c2 = sys_handle.get_combat_state(target_ids[0]).await.unwrap().unwrap().shield_hp;
    let loss_c2 = hp_after_c1 - hp_after_c2;
    assert!(loss_c2 > loss_c1, "Cycle 2 HP loss ({loss_c2}) must exceed cycle 1 loss ({loss_c1})");

    // 32 more cycles on same target: capped at `1.0 + 2.125 = 3.125x` (`attr 2734 = 2.125`)
    for _ in 0..32 {
        step_combat_system(&ctx, sys_id, 3.5).await;
    }
    let hp_pre_cap = sys_handle.get_combat_state(target_ids[0]).await.unwrap().unwrap().shield_hp;
    step_combat_system(&ctx, sys_id, 3.5).await;
    let hp_cap = sys_handle.get_combat_state(target_ids[0]).await.unwrap().unwrap().shield_hp;
    let loss_cap = hp_pre_cap - hp_cap;
    assert!(loss_cap > loss_c2, "Cap cycle loss ({loss_cap}) must exceed cycle 2 loss ({loss_c2})");

    let m_max = eve_server::services::combat_weapons::get_entropic_spool_multiplier(&ctx, ship_id, entropic_id);
    assert!((m_max - 3.125).abs() < 1e-6, "Entropic spool must cap at 3.125x (got {m_max})");

    // Switching target resets spool to 1.0x
    ctx.advance_secs(5);
    let c_switch = dispatch_combat_rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(entropic_id), PyRep::Integer(target_ids[1])], &kw)
        .await
        .expect("Entropic Switch Target");
    let m_switch = dict_get(&c_switch, "spoolMultiplier").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert!((m_switch - 1.0).abs() < 1e-6, "Switching target must reset entropic spool to 1.0x");

    // Advance ticks so entropic multiplier spools above 1.0x on target_ids[1] before testing Deactivate
    step_combat_system(&ctx, sys_id, 3.5).await;
    let m_spooled = eve_server::services::combat_weapons::get_entropic_spool_multiplier(&ctx, ship_id, entropic_id);
    assert!(m_spooled > 1.05, "Spool multiplier on target 1 must spool up before deactivation (got {m_spooled})");

    // Deactivating module (`dogmaIM.Deactivate`) also resets spool
    let _ = dispatch_combat_rpc(&ctx, &mut sess, "dogmaIM", "Deactivate", &[PyRep::Integer(entropic_id)], &HashMap::new()).await.expect("Deactivate entropic");
    ctx.advance_secs(5);
    let c_after_deact = dispatch_combat_rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(entropic_id), PyRep::Integer(target_ids[1])], &kw)
        .await
        .expect("Entropic after Deactivate");
    let m_after_deact = dict_get(&c_after_deact, "spoolMultiplier").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert!((m_after_deact - 1.0).abs() < 1e-6, "Deactivating entropic disintegrator must reset spool to 1.0x");

    // 2. EDENCOM Vorton Projector (`Small Vorton Projector I` 54739 + `GalvaSurge Condenser Pack S` 54769)
    let vorton_id = 201_003;
    let vorton_ammo_id = 201_004;
    fit_module_and_charge(&ctx, char_id, ship_id, vorton_id, 54739, InventoryFlag::HiSlot1, Some((vorton_ammo_id, 54769, 100)));
    let vorton_res = dispatch_combat_rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(vorton_id), PyRep::Integer(target_ids[0])], &kw)
        .await
        .expect("Vorton Activate");
    let arc_targets = dict_get(&vorton_res, "secondaryTargetsHit").and_then(|v| v.as_i64()).unwrap_or(0);
    assert!((5..=9).contains(&arc_targets), "Vorton projector must chain from primary target to nearby secondary targets within 10km (got {arc_targets})");

    // 3. Signature radius evaluation (`feat:dogma.signature-radius`)
    let base_sig = evaluate_effective_signature_radius(&ctx, char_id, ship_id, sys_id).await;
    assert!(base_sig > 25.0 && base_sig < 100.0);
    // Fit and activate 5MN Microwarpdrive I (type 434, +500% sig radius bloom)
    let mwd_id = 201_005;
    fit_module_and_charge(&ctx, char_id, ship_id, mwd_id, 434, InventoryFlag::MedSlot0, None);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.insert(mwd_id);
    });
    let bloomed_sig = evaluate_effective_signature_radius(&ctx, char_id, ship_id, sys_id).await;
    assert!(bloomed_sig > base_sig * 5.0, "Active MWD must bloom signature radius by +500% (base={base_sig}, bloomed={bloomed_sig})");
    let scan_base = compute_probe_scan_signal_strength(40.0, base_sig, 1.0);
    let scan_bloomed = compute_probe_scan_signal_strength(40.0, bloomed_sig, 1.0);
    assert!(scan_bloomed > scan_base, "Larger signature radius must increase combat probe scan signal strength");
}

}

#[allow(unused_imports)]
pub(crate) mod cyno_jump_bridge_and_escape_bay {
//! Dedicated end-to-end integration tests for open `feat:modules.*`, `feat:ships.*`, and `feat:combat.*` parity rows.

use crate::modules_advanced_tactical::{build_ctx, seed_pilot, NULL_SYS};
use crate::features_bounties_ess::{give_item, put_in_fleet, set_skill, spawn_ship_in_space, DEST_NULL_SYS};
use eve_db::InventoryFlag;
use eve_destiny::{Ball, DVec3, MovementMode};
use eve_proto::PyRep;
use eve_server::combat::death_pipeline::execute_death_pipeline;
use eve_server::services::combat_crimewatch::with_combat_runtime;
use std::collections::HashMap;

#[tokio::test]
async fn test_cyno_lighting_rules_beacons_and_covert_cyno() {
    let ctx = build_ctx();
    let kw = HashMap::new();
    let jita_sys: u32 = 30_000_142; // 0.95 high-sec
    let pochven_sys: u32 = 30_000_021; // Kuharah (Pochven)

    let char_id = 981_001;
    let ship_id = 991_001;
    let mut sess = seed_pilot(&ctx, char_id, ship_id, 11957, jita_sys, false); // Falcon Force Recon (group 833)
    put_in_fleet(&ctx, 77_001, char_id, &[(char_id, true)]);
    set_skill(&ctx, char_id, 21603, 5); // Cynosural Field Theory V
    spawn_ship_in_space(&ctx, char_id, ship_id, 11957, jita_sys, DVec3::new(10_000.0, 0.0, 0.0)).await;

    let cyno_mod = 995_001;
    give_item(&ctx, cyno_mod, 21096, char_id, ship_id, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, 995_002, 16273, char_id, ship_id, InventoryFlag::Cargo, 2_000); // Liquid Ozone

    // 1. High-sec blocks Cynosural Field Generator I
    assert!(
        ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(cyno_mod)], &kw, None)
            .await
            .is_err(),
        "Cyno must be blocked in high-security space (Jita)"
    );

    // 2. Pochven (region 10000070) blocks Cynosural Field Generator I
    let _ = ctx.systems.get_or_spawn(jita_sys).remove_ball(ship_id).await;
    sess.solar_system_id = Some(pochven_sys);
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&char_id) {
            ch.solar_system_id = pochven_sys;
        }
    });
    spawn_ship_in_space(&ctx, char_id, ship_id, 11957, pochven_sys, DVec3::new(10_000.0, 0.0, 0.0)).await;
    assert_eq!(
        ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(cyno_mod)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "CynoJammedInPochven",
        "Pochven region-wide cyno jammer must block cyno activation"
    );

    // 3. Null-sec (30004759): moving >= 500 m/s blocks cyno; stopping allows cyno, consumes Liquid Ozone, spawns beacon ball, and immobilizes ship!
    let _ = ctx.systems.get_or_spawn(pochven_sys).remove_ball(ship_id).await;
    sess.solar_system_id = Some(NULL_SYS);
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&char_id) {
            ch.solar_system_id = NULL_SYS;
        }
    });
    let sys_null = ctx.systems.get_or_spawn(NULL_SYS);
    let fast_ball = Ball {
        id: ship_id,
        type_id: 11957,
        position: DVec3::new(20_000.0, 0.0, 0.0),
        velocity: DVec3::new(650.0, 0.0, 0.0),
        max_velocity: 1_000.0,
        inertia: 1.0,
        mass: 10_000_000.0,
        radius: 150.0,
        speed_fraction: 0.65,
        mode: MovementMode::GotoDirection { direction: DVec3::X },
    };
    let _ = sys_null
        .spawn_ball_with_combat(fast_ball, ctx.build_type_combat_state(11957))
        .await;
    assert_eq!(
        ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(cyno_mod)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "ShipMovingTooFast",
        "Cannot light cyno while moving >= 500 m/s"
    );

    let _ = sys_null.remove_ball(ship_id).await;
    spawn_ship_in_space(&ctx, char_id, ship_id, 11957, NULL_SYS, DVec3::new(20_000.0, 0.0, 0.0)).await;
    let ozone_before = ctx.db.with_snapshot(|s| s.items.get(&995_002).unwrap().quantity);
    let cyno_res = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(cyno_mod)], &kw, None)
        .await
        .expect("Activate Cynosural Field Generator I in Null-sec");
    let ozone_after = ctx.db.with_snapshot(|s| s.items.get(&995_002).unwrap().quantity);
    assert!(ozone_after < ozone_before, "Lighting cyno must consume Liquid Ozone ({ozone_after} < {ozone_before})");
    let beacon_id = cyno_res
        .dict_get("beaconID")
        .and_then(|v| v.as_i64())
        .expect("Beacon ID must be spawned");
    assert!(
        sys_null.get_ball_snapshot(beacon_id).await.unwrap().is_some(),
        "Cynosural beacon ball must exist in space"
    );
    assert!(
        eve_server::services::inventory_fitting::fitting_cyno_portal::is_ship_cyno_active(&ctx, ship_id),
        "Ship cyno state must be active"
    );

    // 4. Covert Cyno (28646) on Prowler Blockade Runner (12735) in cyno-jammed system succeeds!
    let covert_char = 981_002;
    let prowler_ship = 991_002;
    let mut covert_sess = seed_pilot(&ctx, covert_char, prowler_ship, 12735, NULL_SYS, false);
    put_in_fleet(&ctx, 77_001, char_id, &[(char_id, true), (covert_char, true)]);
    give_item(&ctx, 995_099, 33700, char_id, NULL_SYS as i64, InventoryFlag::None, 1); // Mobile Cyno Inhibitor
    set_skill(&ctx, covert_char, 21603, 5);
    spawn_ship_in_space(&ctx, covert_char, prowler_ship, 12735, NULL_SYS, DVec3::new(35_000.0, 0.0, 0.0)).await;
    let covert_mod = 995_010;
    give_item(&ctx, covert_mod, 28646, covert_char, prowler_ship, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, 995_011, 16273, covert_char, prowler_ship, InventoryFlag::Cargo, 500);
    let cov_res = ctx
        .dispatch_rpc(&mut covert_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(covert_mod)], &kw, None)
        .await
        .expect("Covert Cyno on Prowler must bypass cyno inhibitor in Null-sec");
    let cov_beacon_id = cov_res.dict_get("beaconID").and_then(|v| v.as_i64()).unwrap_or(0);
    assert!(cov_beacon_id > 0, "Covert cyno must spawn covert fleet beacon");
}

#[tokio::test]
async fn test_jump_drive_fatigue_and_titan_jump_portal_bridge() {
    let ctx = build_ctx();
    let kw = HashMap::new();
    let origin_sys = NULL_SYS; // 30004759
    let dest_sys = DEST_NULL_SYS; // 30004760

    let cyno_char = 981_101;
    let cyno_ship = 991_101;
    let carrier_char = 981_102;
    let carrier_ship = 991_102;
    let titan_char = 981_103;
    let titan_ship = 991_103;
    let subcap_char = 981_104;
    let subcap_ship = 991_104;

    let mut cyno_sess = seed_pilot(&ctx, cyno_char, cyno_ship, 11957, dest_sys, false);
    let mut carrier_sess = seed_pilot(&ctx, carrier_char, carrier_ship, 23757, origin_sys, false);
    let mut titan_sess = seed_pilot(&ctx, titan_char, titan_ship, 11567, origin_sys, false);
    let mut subcap_sess = seed_pilot(&ctx, subcap_char, subcap_ship, 626, origin_sys, false);

    put_in_fleet(
        &ctx,
        77_002,
        titan_char,
        &[
            (cyno_char, true),
            (carrier_char, true),
            (titan_char, true),
            (subcap_char, true),
        ],
    );

    for &(cid, sk) in &[
        (cyno_char, 21603),
        (carrier_char, 21610),
        (carrier_char, 21611),
        (titan_char, 21610),
        (titan_char, 21611),
        (titan_char, 24562),
    ] {
        set_skill(&ctx, cid, sk, 5);
    }

    spawn_ship_in_space(&ctx, cyno_char, cyno_ship, 11957, dest_sys, DVec3::new(15_000.0, 0.0, 0.0)).await;
    spawn_ship_in_space(&ctx, carrier_char, carrier_ship, 23757, origin_sys, DVec3::new(10_000.0, 0.0, 0.0)).await;
    spawn_ship_in_space(&ctx, titan_char, titan_ship, 11567, origin_sys, DVec3::new(20_000.0, 0.0, 0.0)).await;
    spawn_ship_in_space(&ctx, subcap_char, subcap_ship, 626, origin_sys, DVec3::new(21_000.0, 0.0, 0.0)).await;

    // Light Cyno in dest_sys
    give_item(&ctx, 995_101, 21096, cyno_char, cyno_ship, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, 995_102, 16273, cyno_char, cyno_ship, InventoryFlag::Cargo, 1_000);
    let cyno_res = ctx
        .dispatch_rpc(&mut cyno_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(995_101)], &kw, None)
        .await
        .expect("Light Cyno in dest_sys");
    let beacon_id = cyno_res.dict_get("beaconID").and_then(|v| v.as_i64()).unwrap_or(0);

    // 1. Archon Carrier jumps via CmdJumpToFleetModuleBeacon -> consumes Helium Isotopes (16274) and accumulates Jump Fatigue!
    give_item(&ctx, 995_103, 16274, carrier_char, carrier_ship, InventoryFlag::SpecializedFuelBay, 10_000);
    let fatigue_before = ctx.db.with_snapshot(|s| s.characters.get(&carrier_char).map(|c| c.jump_fatigue_until_unix).unwrap_or(0));
    assert_eq!(fatigue_before, 0);
    ctx.dispatch_rpc(
        &mut carrier_sess,
        "beyonce",
        None,
        "CmdJumpToFleetModuleBeacon",
        &[PyRep::Integer(cyno_char), PyRep::Integer(dest_sys as i64), PyRep::Integer(beacon_id)],
        &kw,
        None,
    )
    .await
    .expect("Carrier CmdJumpToFleetModuleBeacon");
    assert_eq!(carrier_sess.solar_system_id, Some(dest_sys), "Carrier must arrive in dest_sys");
    let fuel_left = ctx.db.with_snapshot(|s| s.items.get(&995_103).unwrap().quantity);
    assert!(fuel_left < 10_000, "Jump Drive must consume isotopes ({fuel_left} < 10000)");
    let (fat_until, cd_until, fat_min) = ctx.db.with_snapshot(|s| {
        let ch = s.characters.get(&carrier_char).unwrap();
        (ch.jump_fatigue_until_unix, ch.jump_cooldown_until_unix, ch.jump_fatigue_minutes)
    });
    let now_u = ctx.now_unix_secs();
    assert!(fat_min > 0.0 && fat_until > now_u + 300, "Jump fatigue timer must be set (> 5m, got {fat_min}m)");
    assert!(cd_until > now_u, "Jump reactivation (blue) timer must be set");

    // 2. Avatar Titan opens Jump Portal via CmdBridgeToFleetModuleBeacon & Subcap jumps through via CmdJumpThroughFleet!
    give_item(&ctx, 995_105, 23953, titan_char, titan_ship, InventoryFlag::HiSlot0, 1); // Jump Portal Generator I
    give_item(&ctx, 995_106, 16275, titan_char, titan_ship, InventoryFlag::SpecializedFuelBay, 5_000);
    give_item(&ctx, 995_107, 16274, titan_char, titan_ship, InventoryFlag::SpecializedFuelBay, 10_000);
    ctx.dispatch_rpc(
        &mut titan_sess,
        "beyonce",
        None,
        "CmdBridgeToFleetModuleBeacon",
        &[PyRep::Integer(cyno_char), PyRep::Integer(dest_sys as i64), PyRep::Integer(beacon_id)],
        &kw,
        None,
    )
    .await
    .expect("Titan CmdBridgeToFleetModuleBeacon");

    ctx.dispatch_rpc(
        &mut subcap_sess,
        "beyonce",
        None,
        "CmdJumpThroughFleet",
        &[PyRep::Integer(titan_char), PyRep::Integer(titan_ship)],
        &kw,
        None,
    )
    .await
    .expect("Fleetmate CmdJumpThroughFleet through Titan bridge");
    assert_eq!(subcap_sess.solar_system_id, Some(dest_sys), "Subcap must arrive in dest_sys via Titan Jump Portal");
    assert!(
        ctx.db.with_snapshot(|s| s.characters.get(&subcap_char).is_some_and(|c| c.jump_fatigue_until_unix > now_u)),
        "Jumping through Titan bridge must accumulate jump fatigue on passenger"
    );
}

#[tokio::test]
async fn test_blockade_runner_scan_immunity_and_frigate_escape_bay() {
    let ctx = build_ctx();
    let kw = HashMap::new();
    let sys_id = NULL_SYS;

    let scanner_char = 981_201;
    let scanner_ship = 991_201;
    let br_char = 981_202;
    let br_ship = 991_202;
    let rifter_char = 981_203;
    let rifter_ship = 991_203;

    let mut scanner_sess = seed_pilot(&ctx, scanner_char, scanner_ship, 626, sys_id, false);
    let _br_sess = seed_pilot(&ctx, br_char, br_ship, 12735, sys_id, false);
    let _rifter_sess = seed_pilot(&ctx, rifter_char, rifter_ship, 587, sys_id, false);

    spawn_ship_in_space(&ctx, scanner_char, scanner_ship, 626, sys_id, DVec3::new(0.0, 0.0, 0.0)).await;
    spawn_ship_in_space(&ctx, br_char, br_ship, 12735, sys_id, DVec3::new(5_000.0, 0.0, 0.0)).await;
    spawn_ship_in_space(&ctx, rifter_char, rifter_ship, 587, sys_id, DVec3::new(6_000.0, 0.0, 0.0)).await;

    let cargo_scanner = 995_201;
    give_item(&ctx, cargo_scanner, 442, scanner_char, scanner_ship, InventoryFlag::MedSlot0, 1);
    give_item(&ctx, 995_202, 34, br_char, br_ship, InventoryFlag::Cargo, 5_000);
    give_item(&ctx, 995_203, 34, rifter_char, rifter_ship, InventoryFlag::Cargo, 1_234);

    // 1. Cargo Scanner on Blockade Runner (Prowler 12735) -> immune!
    let br_scan = ctx
        .dispatch_rpc(
            &mut scanner_sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(cargo_scanner), PyRep::Integer(br_ship)],
            &kw,
            None,
        )
        .await
        .expect("Cargo Scan Prowler");
    assert_eq!(br_scan.dict_get("cargoScanImmune").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(br_scan.dict_get("totalScannedQuantity").and_then(|v| v.as_i64()), Some(0));

    // Cargo Scanner on normal Rifter (587) -> reveals 1,234 Tritanium!
    ctx.db.with_mut_snapshot(|s| {
        s.module_active_states.remove(&cargo_scanner);
    });
    let normal_scan = ctx
        .dispatch_rpc(
            &mut scanner_sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(cargo_scanner), PyRep::Integer(rifter_ship)],
            &kw,
            None,
        )
        .await
        .expect("Cargo Scan Rifter");
    assert_eq!(normal_scan.dict_get("totalScannedQuantity").and_then(|v| v.as_i64()), Some(1_234));

    // 2. Frigate Escape Bay on Megathron (641): assembled Rifter in flag 179 ejects pilot into Rifter on Battleship destruction!
    let bs_char = 981_204;
    let bs_ship = 991_204;
    let feb_frigate = 995_210;
    let _bs_sess = seed_pilot(&ctx, bs_char, bs_ship, 641, sys_id, false);
    spawn_ship_in_space(&ctx, bs_char, bs_ship, 641, sys_id, DVec3::new(10_000.0, 0.0, 0.0)).await;
    give_item(&ctx, feb_frigate, 587, bs_char, bs_ship, InventoryFlag::from_u16(179), 1);
    give_item(&ctx, 995_211, 34, bs_char, bs_ship, InventoryFlag::Cargo, 500);

    let death_out = execute_death_pipeline(&ctx, sys_id, bs_char, bs_ship, scanner_char, DVec3::new(10_000.0, 0.0, 0.0), 42)
        .await
        .expect("Execute Megathron death pipeline");
    assert_eq!(death_out.ejected_capsule_id, Some(feb_frigate), "Pilot must eject into Frigate Escape Bay ship ({feb_frigate})");
    let feb_item = ctx.db.with_snapshot(|s| s.items.get(&feb_frigate).cloned()).expect("FEB frigate must survive");
    assert_eq!(feb_item.location_id, sys_id as i64, "FEB frigate must be in space in solar system");
    assert!(ctx.systems.get_or_spawn(sys_id).get_ball_snapshot(feb_frigate).await.unwrap().is_some(), "FEB frigate ball must be spawned in space");
}

#[tokio::test]
async fn test_combat_insurance_void_and_expiry_and_pochven_loot_drop() {
    let ctx = build_ctx();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let jita_sys: u32 = 30_000_142;
    let pochven_sys: u32 = 30_000_021; // Kuharah

    let char_id = 981_301;
    let ship_id = 991_301;
    let killer_char = 981_302;
    let killer_ship = 991_302;
    let mut sess = seed_pilot(&ctx, char_id, ship_id, 587, jita_sys, true);
    let _killer_sess = seed_pilot(&ctx, killer_char, killer_ship, 626, pochven_sys, false);
    sess.station_id = Some(60_003_760);

    // 1. Insurance contract via insuranceSvc.InsureShip, voiding on repackage, and 12-week expiry fallback (40%)
    let base_price = ctx.sde.types.get(&587).map(|t| t.base_price).filter(|&p| p > 0.0).unwrap_or(230_000.0);
    let cost_isk = (base_price * 0.30).round();
    let _ins_res = ctx
        .dispatch_rpc(
            &mut sess,
            "insuranceSvc",
            None,
            "InsureShip",
            &[PyRep::Integer(ship_id), PyRep::Real(cost_isk), PyRep::Bool(false), PyRep::Bool(true)],
            &kw,
            None,
        )
        .await
        .expect("InsureShip RPC");
    assert!(with_combat_runtime(&ctx, |rt| rt.insurance_contracts.contains_key(&ship_id)));

    ctx.dispatch_rpc(
        &mut sess,
        "insuranceSvc",
        None,
        "UnInsureShip",
        &[PyRep::Integer(ship_id)],
        &kw,
        None,
    )
    .await
    .expect("UnInsureShip voids active insurance contract");
    assert!(
        !with_combat_runtime(&ctx, |rt| rt.insurance_contracts.contains_key(&ship_id)),
        "Voiding/repackaging ship must remove active insurance contract"
    );

    // Re-assemble, re-insure (while docked in station), undock to Pochven, advance clock > 12 weeks -> expired contract pays 40% fallback + EVE-mail!
    let station_id = 60_003_760i64;
    ctx.db.with_mut_snapshot(|s| {
        if let Some(it) = s.items.get_mut(&ship_id) {
            it.singleton = true;
            it.location_id = station_id;
        }
        if let Some(ch) = s.characters.get_mut(&char_id) {
            ch.station_id = Some(station_id);
            ch.solar_system_id = 30_000_142;
        }
    });
    sess.station_id = Some(station_id);
    ctx.dispatch_rpc(
        &mut sess,
        "insuranceSvc",
        None,
        "InsureShip",
        &[PyRep::Integer(ship_id), PyRep::Real(cost_isk), PyRep::Bool(false), PyRep::Bool(true)],
        &kw,
        None,
    )
    .await
    .expect("Re-InsureShip");
    ctx.db.with_mut_snapshot(|s| {
        if let Some(it) = s.items.get_mut(&ship_id) {
            it.location_id = pochven_sys as i64;
        }
        if let Some(ch) = s.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.solar_system_id = pochven_sys;
        }
    });
    sess.station_id = None;
    sess.solar_system_id = Some(pochven_sys);
    ctx.advance_time_seconds(12 * 7 * 86_400 + 30);

    // 2. Seed 20 cargo items on ship in Pochven (30000021) -> destroy ship -> ~60% loot drop (12/20) + expired 40% insurance payout + EVE-mail!
    for i in 0..20_i64 {
        give_item(&ctx, 995_300 + i, 34, char_id, ship_id, InventoryFlag::Cargo, 100);
    }
    let mails_before = ctx.db.with_snapshot(|s| s.mails.len());
    let poch_death = execute_death_pipeline(&ctx, pochven_sys, char_id, ship_id, killer_char, DVec3::ZERO, 42)
        .await
        .expect("Execute Pochven death pipeline");
    assert!(
        poch_death.loot_dropped_item_ids.len() > 10,
        "Pochven PvP loot drop (~60%) must drop more than 50% of 20 stacks (got {})",
        poch_death.loot_dropped_item_ids.len()
    );
    assert!(
        ctx.db.with_snapshot(|s| s.mails.len() > mails_before),
        "Ship loss must deliver Insurance Payout EVE-mail"
    );
}
}

#[allow(unused_imports)]
pub(crate) mod gh1082_weapons_and_death {
    use super::triglavian_entropic_spool_edencom_vorton::{fit_module_and_charge, setup_pilot_session};
    use eve_db::{InventoryFlag, TrainedSkillRecord};
    use eve_destiny::{Ball, CombatState, DVec3, MovementMode, TargetLock};
    use eve_machonet::SessionState;
    use eve_proto::{ClientError, PyRep, UserErrorKey};
    use eve_server::combat::death_pipeline::execute_death_pipeline;
    use eve_server::router::ServerContext;
    use eve_server::system_actor::SystemHandle;
    use std::collections::HashMap;

    const ATT_CHAR: i64 = 90_000_001;
    const VIC_CHAR: i64 = 90_000_002;
    const OBS_CHAR: i64 = 90_000_003;
    const SYS_ID: u32 = 30_000_142;

    fn grant_gunnery_and_targeting(ctx: &ServerContext, char_id: i64) {
        ctx.db.with_mut_snapshot(|snap| {
            let sk = snap.skills.entry(char_id).or_default();
            for sid in [3300u32, 3301, 3302, 3303, 3318, 3428, 3429] {
                sk.insert(
                    sid,
                    TrainedSkillRecord {
                        type_id: sid,
                        level: 5,
                        skill_points: 256_000.0,
                    },
                );
            }
        });
    }

    async fn setup_combatant_with_observer(
        ctx: &ServerContext,
        base_id: i64,
        att_type: u32,
        vic_type: u32,
        vic_hp_override: Option<f64>,
    ) -> (SessionState, i64, SessionState, i64, i64, SystemHandle) {
        ctx.set_clock_paused(true);
        ctx.set_invention_roll_override(Some(0.50));
        let att_ship = base_id;
        let vic_ship = base_id + 1;
        let obs_ship = base_id + 2;
        let att_sess = setup_pilot_session(
            ctx,
            (base_id as u64) + 10,
            ATT_CHAR,
            att_ship,
            att_type,
            SYS_ID,
            DVec3::ZERO,
        )
        .await;
        let vic_sess = setup_pilot_session(
            ctx,
            (base_id as u64) + 11,
            VIC_CHAR,
            vic_ship,
            vic_type,
            SYS_ID,
            DVec3::new(1_500.0, 0.0, 0.0),
        )
        .await;
        ctx.db.with_mut_snapshot(|snap| {
            let obs_uid = (base_id as u64) + 12;
            let obs_user = format!("obs_{base_id}");
            snap.accounts.entry(obs_user.clone()).or_insert_with(|| eve_db::AccountRecord {
                user_id: obs_uid,
                username: obs_user.clone(),
                password_token: eve_db::WorldRepository::hash_password_token(&obs_user, "obs-secret"),
                role: eve_db::service_roles::ROLE_PLAYER_DEFAULT,
                banned_until_unix: None,
                ban_reason: None,
            });
            if !snap.characters.contains_key(&OBS_CHAR) {
                if let Some(mut obs_ch) = snap.characters.get(&ATT_CHAR).cloned() {
                    obs_ch.character_id = OBS_CHAR;
                    obs_ch.user_id = obs_uid;
                    obs_ch.name = "Observer Pilot".to_string();
                    obs_ch.active_ship_id = obs_ship;
                    obs_ch.solar_system_id = SYS_ID;
                    obs_ch.station_id = None;
                    obs_ch.structure_id = None;
                    snap.characters.insert(OBS_CHAR, obs_ch);
                }
            }
        });
        let _obs_sess = setup_pilot_session(
            ctx,
            (base_id as u64) + 12,
            OBS_CHAR,
            obs_ship,
            587,
            SYS_ID,
            DVec3::new(3_000.0, 0.0, 0.0),
        )
        .await;
        grant_gunnery_and_targeting(ctx, ATT_CHAR);
        grant_gunnery_and_targeting(ctx, OBS_CHAR);
        let sys = ctx.systems.get_or_spawn(SYS_ID);
        for &bid in &[att_ship, vic_ship, obs_ship] {
            if let Ok(Some(mut cs)) = sys.get_combat_state(bid).await {
                cs.undock_invulnerable_remaining_ticks = 0;
                if bid == vic_ship {
                    if let Some(hp) = vic_hp_override {
                        cs.shield_hp = 0.0;
                        cs.armor_hp = 0.0;
                        cs.structure_hp = hp;
                        cs.max_structure_hp = hp.max(100.0);
                    }
                }
                let _ = sys.set_combat_state(bid, cs).await;
            }
        }
        (att_sess, att_ship, vic_sess, vic_ship, obs_ship, sys)
    }

    async fn lock_target_sync(sys: &SystemHandle, source: i64, target: i64) {
        let _ = sys
            .add_target_lock(
                source,
                TargetLock {
                    target_id: target,
                    lock_time_ms: 500.0,
                    remaining_lock_ms: 0.0,
                    locked: true,
                    max_range_m: None,
                },
            )
            .await;
    }

    fn parse_destiny_updates(updates: &PyRep) -> Vec<(String, Vec<PyRep>)> {
        let PyRep::List(entries) = updates else {
            return Vec::new();
        };
        entries
            .iter()
            .filter_map(|e| {
                let PyRep::Tuple(outer) = e else { return None };
                let PyRep::Tuple(action) = outer.get(1)? else { return None };
                let name = action.first()?.as_str()?.to_string();
                let args = match action.get(1) {
                    Some(PyRep::Tuple(a)) => a.clone(),
                    Some(other) => vec![other.clone()],
                    None => Vec::new(),
                };
                Some((name, args))
            })
            .collect()
    }

    fn remove_ball_ids(args: &[PyRep]) -> Vec<i64> {
        match args.first() {
            Some(PyRep::Integer(id)) => vec![*id],
            Some(PyRep::List(items)) => items.iter().filter_map(|x| x.as_i64()).collect(),
            _ => Vec::new(),
        }
    }

    fn add_balls2_has_id(args: &[PyRep], target_id: i64) -> bool {
        fn contains_int(r: &PyRep, target_id: i64) -> bool {
            match r {
                PyRep::Integer(v) => *v == target_id,
                PyRep::List(xs) | PyRep::Tuple(xs) => xs.iter().any(|x| contains_int(x, target_id)),
                PyRep::KeyVal(m) => m.values().any(|v| contains_int(v, target_id)),
                PyRep::Dict(kvs) => kvs.iter().any(|(_, v)| contains_int(v, target_id)),
                _ => false,
            }
        }
        args.iter().any(|a| contains_int(a, target_id))
    }

    fn is_fx(n: &eve_server::router::cache_notifications::PendingMachoNotification, mod_id: i64, start_val: i64, guid: Option<&str>, charge: Option<u32>) -> bool {
        if n.event != "OnSpecialFX" || n.args.len() != 14 {
            return false;
        }
        let mod_ok = n.args[1].as_i64() == Some(mod_id);
        let start_ok = n.args[7].as_i64() == Some(start_val);
        let guid_ok = guid.map_or(true, |g| n.args[5].as_str() == Some(g));
        let charge_ok = match charge {
            Some(cid) => n.args[4].as_i64() == Some(i64::from(cid)),
            None => matches!(n.args[4], PyRep::None),
        };
        mod_ok && start_ok && guid_ok && charge_ok
    }

    #[tokio::test]
    async fn turret_consumes_ammo_hits_on_activation_stops_when_empty() {
        // parity: feat:modules.turret-projectile, feat:modules.turret-hybrid, feat:modules.turret-laser, notify:OnSpecialFX, notify:OnDamageMessage, notify:OnDamageStateChange, notify:OnGodmaShipEffect
        eve_testkit::attest(&[
            "feat:modules.turret-projectile",
            "feat:modules.turret-hybrid",
            "feat:modules.turret-laser",
            "notify:OnSpecialFX",
            "notify:OnDamageMessage",
            "notify:OnDamageStateChange",
            "notify:OnGodmaShipEffect",
        ]);

        let ctx = ServerContext::new();
        let (mut att_sess, att_ship, _vic_sess, vic_ship, _obs_ship, sys) =
            setup_combatant_with_observer(&ctx, 790_000, 16236, 24690, None).await; // Coercer + Maelstrom
        let kw = HashMap::new();

        // 1. Negative: activating without a target lock raises UserErrorKey::TargetNotLocked
        let ac_id = 791_001i64;
        let ac_ammo = 791_002i64;
        fit_module_and_charge(&ctx, ATT_CHAR, att_ship, ac_id, 486, InventoryFlag::HiSlot0, Some((ac_ammo, 185, 1)));
        let hybrid_id = 791_003i64;
        fit_module_and_charge(&ctx, ATT_CHAR, att_ship, hybrid_id, 10678, InventoryFlag::HiSlot1, Some((791_004, 222, 5)));
        let laser_id = 791_005i64;
        fit_module_and_charge(&ctx, ATT_CHAR, att_ship, laser_id, 3041, InventoryFlag::HiSlot2, Some((791_006, 246, 1)));

        let no_lock_err = ctx
            .dispatch_rpc(
                &mut att_sess,
                "dogmaIM",
                None,
                "Activate",
                &[PyRep::Integer(ac_id), PyRep::String("projectileFired".into()), PyRep::Integer(vic_ship)],
                &kw,
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(
            no_lock_err.to_client_error("dogmaIM", "Activate"),
            ClientError::User { key: UserErrorKey::TargetNotLocked, args: Vec::new() }
        );

        lock_target_sync(&sys, att_ship, vic_ship).await;
        let _ = ctx.drain_notifications_for_char(ATT_CHAR);
        let _ = ctx.drain_notifications_for_char(VIC_CHAR);
        let _ = ctx.drain_notifications_for_char(OBS_CHAR);

        // 2. Projectile AutoCannon with 1 round: immediate volley damage, start FX (repeat=1000, effects.ProjectileFired),
        //    and EffectAlreadyActive2 on duplicate activation.
        let pre_cs = sys.get_combat_state(vic_ship).await.unwrap().unwrap();
        let act_res = ctx
            .dispatch_rpc(
                &mut att_sess,
                "dogmaIM",
                None,
                "Activate",
                &[PyRep::Integer(ac_id), PyRep::String("projectileFired".into()), PyRep::Integer(vic_ship), PyRep::Integer(1000)],
                &kw,
                None,
            )
            .await
            .expect("AutoCannon Activate");
        assert_eq!(act_res.dict_get("active"), Some(&PyRep::Bool(true)));
        let post_cs = sys.get_combat_state(vic_ship).await.unwrap().unwrap();
        assert!(post_cs.shield_hp < pre_cs.shield_hp, "first cycle must apply immediate damage");
        assert!(
            ctx.db.with_snapshot(|s| !s.items.contains_key(&ac_ammo)),
            "last round consumed must remove ammo item"
        );

        let att_n = ctx.drain_notifications_for_char(ATT_CHAR);
        let vic_n = ctx.drain_notifications_for_char(VIC_CHAR);
        let obs_n = ctx.drain_notifications_for_char(OBS_CHAR);
        assert!(att_n.iter().any(|n| n.event == "OnDamageMessage"), "shooter must receive OnDamageMessage");
        assert!(vic_n.iter().any(|n| n.event == "OnDamageStateChange"), "target must receive OnDamageStateChange");
        let start_fx = att_n.iter().find(|n| is_fx(n, ac_id, 1, Some("effects.ProjectileFired"), Some(185)))
            .expect("shooter must receive ProjectileFired start FX");
        assert_eq!(start_fx.args[10], PyRep::Integer(1000), "start FX repeat must be 1000");
        assert!(obs_n.iter().any(|n| is_fx(n, ac_id, 1, Some("effects.ProjectileFired"), Some(185))),
            "observer must receive ProjectileFired start FX");

        let dup_err = ctx
            .dispatch_rpc(
                &mut att_sess,
                "dogmaIM",
                None,
                "Activate",
                &[PyRep::Integer(ac_id), PyRep::String("projectileFired".into()), PyRep::Integer(vic_ship)],
                &kw,
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(
            dup_err.to_client_error("dogmaIM", "Activate"),
            ClientError::user_with_args(
                UserErrorKey::EffectAlreadyActive2,
                [("modulename", eve_proto::UserErrorArg::Str("200mm AutoCannon I".into()))],
            )
        );

        // Advance past cycle duration -> last round stops effect with stop OnSpecialFX (start=0, charge=185) to owner + observer
        let _ = ctx.drain_notifications_for_char(ATT_CHAR);
        let _ = ctx.drain_notifications_for_char(OBS_CHAR);
        for _ in 0..8 {
            let _ = sys.step_manual(1.0).await.unwrap();
        }
        let att_stop = ctx.drain_notifications_for_char(ATT_CHAR);
        let obs_stop = ctx.drain_notifications_for_char(OBS_CHAR);
        assert!(
            att_stop.iter().any(|n| is_fx(n, ac_id, 0, Some("effects.ProjectileFired"), Some(185))),
            "owner must receive stop OnSpecialFX with chargeTypeID=185 when last round spent: {att_stop:?}"
        );
        assert!(
            obs_stop.iter().any(|n| is_fx(n, ac_id, 0, Some("effects.ProjectileFired"), Some(185))),
            "observer must receive stop OnSpecialFX with chargeTypeID=185: {obs_stop:?}"
        );
        assert!(!ctx.db.with_snapshot(|s| s.module_active_states.contains(&ac_id)));

        // 3. Hybrid + Laser emit exact GUIDs (`effects.HybridFired`, `effects.Laser`) and decrement non-final charge stacksize - 1
        let _ = ctx
            .dispatch_rpc(&mut att_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(hybrid_id), PyRep::String("targetAttack".into()), PyRep::Integer(vic_ship)], &kw, None)
            .await
            .expect("Hybrid Activate");
        assert_eq!(ctx.db.with_snapshot(|s| s.items[&791_004].quantity), 4);
        let _ = ctx
            .dispatch_rpc(&mut att_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(laser_id), PyRep::String("targetAttack".into()), PyRep::Integer(vic_ship)], &kw, None)
            .await
            .expect("Laser Activate");
        let n_hyb_las = ctx.drain_notifications_for_char(ATT_CHAR);
        assert!(n_hyb_las.iter().any(|n| is_fx(n, hybrid_id, 1, Some("effects.HybridFired"), Some(222))));
        assert!(n_hyb_las.iter().any(|n| is_fx(n, laser_id, 1, Some("effects.Laser"), Some(246))));

        // 4. Grouped turrets fan out OnSpecialFX start & stop per member and consume 1 charge per turret
        let grp_a = 791_010i64;
        let grp_b = 791_011i64;
        let ammo_a = 791_012i64;
        let ammo_b = 791_013i64;
        fit_module_and_charge(&ctx, ATT_CHAR, att_ship, grp_a, 486, InventoryFlag::HiSlot3, Some((ammo_a, 185, 1)));
        fit_module_and_charge(&ctx, ATT_CHAR, att_ship, grp_b, 486, InventoryFlag::HiSlot4, Some((ammo_b, 185, 1)));
        let _ = ctx
            .dispatch_rpc(&mut att_sess, "dogmaIM", None, "LinkWeapons", &[PyRep::Integer(att_ship), PyRep::Integer(grp_a), PyRep::Integer(grp_b)], &kw, None)
            .await
            .expect("LinkWeapons");
        let _ = ctx.drain_notifications_for_char(ATT_CHAR);
        let _ = ctx.drain_notifications_for_char(OBS_CHAR);

        let _ = ctx
            .dispatch_rpc(&mut att_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(grp_a), PyRep::String("projectileFired".into()), PyRep::Integer(vic_ship)], &kw, None)
            .await
            .expect("Grouped Activate");
        assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&ammo_a) && !s.items.contains_key(&ammo_b)),
            "grouped volley must consume 1 round from each turret");
        let grp_start_att = ctx.drain_notifications_for_char(ATT_CHAR);
        let grp_start_obs = ctx.drain_notifications_for_char(OBS_CHAR);
        assert!(grp_start_att.iter().any(|n| is_fx(n, grp_a, 1, Some("effects.ProjectileFired"), Some(185))));
        assert!(grp_start_att.iter().any(|n| is_fx(n, grp_b, 1, Some("effects.ProjectileFired"), Some(185))));
        assert!(grp_start_obs.iter().any(|n| is_fx(n, grp_b, 1, Some("effects.ProjectileFired"), Some(185))));

        for _ in 0..8 {
            let _ = sys.step_manual(1.0).await.unwrap();
        }
        let grp_stop_obs = ctx.drain_notifications_for_char(OBS_CHAR);
        assert!(grp_stop_obs.iter().any(|n| is_fx(n, grp_a, 0, Some("effects.ProjectileFired"), Some(185))));
        assert!(grp_stop_obs.iter().any(|n| is_fx(n, grp_b, 0, Some("effects.ProjectileFired"), Some(185))));
    }

    #[tokio::test]
    async fn civilian_turret_fires_without_charge() {
        // parity: feat:modules.turret-projectile, notify:OnSpecialFX, notify:OnDamageMessage
        eve_testkit::attest(&[
            "feat:modules.turret-projectile",
            "notify:OnSpecialFX",
            "notify:OnDamageMessage",
        ]);

        let ctx = ServerContext::new();
        let (mut att_sess, att_ship, _vic_sess, vic_ship, _obs_ship, sys) =
            setup_combatant_with_observer(&ctx, 792_000, 587, 587, None).await;
        let kw = HashMap::new();
        let civ_gun = 792_010i64;
        // 3636 = Civilian Gatling Autocannon (capacity = 0.0, fires without charge)
        fit_module_and_charge(&ctx, ATT_CHAR, att_ship, civ_gun, 3636, InventoryFlag::HiSlot0, None);
        lock_target_sync(&sys, att_ship, vic_ship).await;
        let _ = ctx.drain_notifications_for_char(ATT_CHAR);
        let _ = ctx.drain_notifications_for_char(OBS_CHAR);

        let pre_cs = sys.get_combat_state(vic_ship).await.unwrap().unwrap();
        let act = ctx
            .dispatch_rpc(
                &mut att_sess,
                "dogmaIM",
                None,
                "Activate",
                &[PyRep::Integer(civ_gun), PyRep::String("projectileFired".into()), PyRep::Integer(vic_ship), PyRep::Integer(1000)],
                &kw,
                None,
            )
            .await
            .expect("Civilian turret must activate without ammo");
        assert_eq!(act.dict_get("active"), Some(&PyRep::Bool(true)));
        let post_cs = sys.get_combat_state(vic_ship).await.unwrap().unwrap();
        assert!(post_cs.shield_hp < pre_cs.shield_hp, "civilian turret must deal damage without charge");

        let att_n = ctx.drain_notifications_for_char(ATT_CHAR);
        let obs_n = ctx.drain_notifications_for_char(OBS_CHAR);
        assert!(att_n.iter().any(|n| n.event == "OnDamageMessage"));
        assert!(att_n.iter().any(|n| is_fx(n, civ_gun, 1, Some("effects.ProjectileFired"), None)));
        assert!(obs_n.iter().any(|n| is_fx(n, civ_gun, 1, Some("effects.ProjectileFired"), None)));
    }

    #[tokio::test]
    async fn laser_stop_fx_targeted_and_kill_clears_target() {
        // parity: feat:modules.turret-laser, notify:OnSpecialFX, notify:OnTarget, notify:OnGodmaShipEffect
        eve_testkit::attest(&[
            "feat:modules.turret-laser",
            "notify:OnSpecialFX",
            "notify:OnTarget",
            "notify:OnGodmaShipEffect",
        ]);

        let ctx = ServerContext::new();
        let (mut att_sess, att_ship, _vic_sess, vic_ship, _obs_ship, sys) =
            setup_combatant_with_observer(&ctx, 793_000, 16236, 587, Some(1.0)).await;
        let kw = HashMap::new();
        let laser_id = 793_010i64;
        fit_module_and_charge(&ctx, ATT_CHAR, att_ship, laser_id, 3041, InventoryFlag::HiSlot0, Some((793_011, 246, 1)));
        lock_target_sync(&sys, att_ship, vic_ship).await;
        let _ = ctx.drain_notifications_for_char(ATT_CHAR);
        let _ = ctx.drain_notifications_for_char(OBS_CHAR);

        // Fatal volley kills low-HP victim immediately, stops laser FX (no beam restart), and clears lock
        let _ = ctx
            .dispatch_rpc(
                &mut att_sess,
                "dogmaIM",
                None,
                "Activate",
                &[PyRep::Integer(laser_id), PyRep::String("targetAttack".into()), PyRep::Integer(vic_ship), PyRep::Integer(1000)],
                &kw,
                None,
            )
            .await
            .expect("Fatal laser activation");

        let att_n = ctx.drain_notifications_for_char(ATT_CHAR);
        let obs_n = ctx.drain_notifications_for_char(OBS_CHAR);
        assert!(
            att_n.iter().any(|n| is_fx(n, laser_id, 0, Some("effects.Laser"), Some(246)) && n.args[3] == PyRep::Integer(vic_ship)),
            "attacker must receive targeted stop OnSpecialFX with targetID={vic_ship}: {att_n:?}"
        );
        assert!(
            obs_n.iter().any(|n| is_fx(n, laser_id, 0, Some("effects.Laser"), Some(246)) && n.args[3] == PyRep::Integer(vic_ship)),
            "observer must receive targeted stop OnSpecialFX"
        );
        assert!(
            att_n.iter().any(|n| n.event == "OnGodmaShipEffect" && n.args.first() == Some(&PyRep::Integer(laser_id)) && n.args.get(3) == Some(&PyRep::Integer(0)) && n.args.get(4) == Some(&PyRep::Integer(0))),
            "laser OnGodmaShipEffect must deactivate in same tick"
        );
        assert!(
            att_n.iter().any(|n| n.event == "OnTarget" && n.args == vec![PyRep::Integer(vic_ship), PyRep::String("lost".into())]),
            "target kill must emit OnTarget lost"
        );
        assert!(sys.get_target_locks(att_ship).await.unwrap().is_empty());

        // Advancing further does not restart any beam FX
        let _ = sys.step_manual(1.0).await.unwrap();
        let after_n = ctx.drain_notifications_for_char(ATT_CHAR);
        assert!(!after_n.iter().any(|n| is_fx(n, laser_id, 1, None, None)), "no beam restart after kill");
    }

    #[tokio::test]
    async fn target_death_stops_attacker_modules_immediately() {
        // parity: feat:combat.ship-destruction, notify:OnSpecialFX, notify:OnGodmaShipEffect, notify:OnTarget
        eve_testkit::attest(&[
            "feat:combat.ship-destruction",
            "notify:OnSpecialFX",
            "notify:OnGodmaShipEffect",
            "notify:OnTarget",
        ]);

        let ctx = ServerContext::new();
        let (mut att_sess, att_ship, _vic_sess, vic_ship, _obs_ship, sys) =
            setup_combatant_with_observer(&ctx, 794_000, 16236, 587, Some(1.0)).await;
        let kw = HashMap::new();

        // Spawn second unrelated live target
        let live_target = 794_050i64;
        let _ = sys
            .spawn_ball_with_combat(
                Ball {
                    id: live_target,
                    type_id: 24690,
                    position: DVec3::new(1_800.0, 0.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 100.0,
                    inertia: 1.0,
                    mass: 1e7,
                    radius: 400.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                ctx.build_type_combat_state(24690),
            )
            .await;

        let scram_dead = 794_010i64;
        let gun_dead = 794_011i64;
        let gun_live = 794_012i64;
        fit_module_and_charge(&ctx, ATT_CHAR, att_ship, scram_dead, 447, InventoryFlag::MedSlot0, None);
        fit_module_and_charge(&ctx, ATT_CHAR, att_ship, gun_dead, 486, InventoryFlag::HiSlot0, Some((794_013, 185, 10)));
        fit_module_and_charge(&ctx, ATT_CHAR, att_ship, gun_live, 486, InventoryFlag::HiSlot1, Some((794_014, 185, 10)));
        lock_target_sync(&sys, att_ship, vic_ship).await;
        lock_target_sync(&sys, att_ship, live_target).await;

        // Activate scram on soon-to-die vic_ship + gun on live_target, then kill vic_ship with gun_dead
        let _ = ctx
            .dispatch_rpc(&mut att_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(scram_dead), PyRep::String("warpScramble".into()), PyRep::Integer(vic_ship)], &kw, None)
            .await
            .expect("scram on victim");
        let _ = ctx
            .dispatch_rpc(&mut att_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(gun_live), PyRep::String("projectileFired".into()), PyRep::Integer(live_target)], &kw, None)
            .await
            .expect("gun on live target");
        let _ = ctx.drain_notifications_for_char(ATT_CHAR);

        let _ = ctx
            .dispatch_rpc(&mut att_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(gun_dead), PyRep::String("projectileFired".into()), PyRep::Integer(vic_ship)], &kw, None)
            .await
            .expect("lethal shot");

        // Both modules targeting dead vic_ship stopped immediately; module targeting live_target remains active; lock on live_target kept
        assert_eq!(sys.update_module_overload(att_ship, scram_dead, false).await, Ok(false), "scram on dead target must stop");
        assert_eq!(sys.update_module_overload(att_ship, gun_dead, false).await, Ok(false), "turret on dead target must stop");
        assert_eq!(sys.update_module_overload(att_ship, gun_live, false).await, Ok(true), "module on unrelated live target stays active");
        let rem_locks: Vec<i64> = sys.get_target_locks(att_ship).await.unwrap().into_iter().map(|l| l.target_id).collect();
        assert_eq!(rem_locks, vec![live_target], "only dead target lock must be cleared");

        // Re-activating against dead target raises TargetNotLocked
        let dead_err = ctx
            .dispatch_rpc(&mut att_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(scram_dead), PyRep::String("warpScramble".into()), PyRep::Integer(vic_ship)], &kw, None)
            .await
            .unwrap_err();
        assert_eq!(
            dead_err.to_client_error("dogmaIM", "Activate"),
            ClientError::User { key: UserErrorKey::TargetNotLocked, args: Vec::new() }
        );
    }

    #[tokio::test]
    async fn ship_death_sequence_fx_remove_then_wreck_addballs() {
        // parity: feat:combat.ship-destruction, destiny:TerminalPlayDestructionEffect, destiny:RemoveBalls, destiny:AddBalls2
        eve_testkit::attest(&[
            "feat:combat.ship-destruction",
            "destiny:TerminalPlayDestructionEffect",
            "destiny:RemoveBalls",
            "destiny:AddBalls2",
        ]);

        let ctx = ServerContext::new();
        let (_att_sess, _att_ship, _vic_sess, vic_ship, obs_ship, sys) =
            setup_combatant_with_observer(&ctx, 795_000, 587, 587, None).await;
        let _ = sys.drain_observer_updates(obs_ship).await.unwrap();

        let res = execute_death_pipeline(&ctx, SYS_ID, VIC_CHAR, vic_ship, ATT_CHAR, DVec3::new(1_500.0, 0.0, 0.0), ctx.now_unix())
            .await
            .expect("ship death pipeline");
        let wreck_id = res.wreck_item_id;

        let (_tick, _evs, updates) = sys.drain_observer_updates(obs_ship).await.unwrap();
        let actions = parse_destiny_updates(&updates);
        let fx_pos = actions
            .iter()
            .position(|(n, a)| n == "TerminalPlayDestructionEffect" && a.first() == Some(&PyRep::Integer(vic_ship)))
            .unwrap_or_else(|| panic!("missing TerminalPlayDestructionEffect in {actions:?}"));
        let rem_pos = actions
            .iter()
            .position(|(n, a)| (n == "RemoveBall" || n == "RemoveBalls") && remove_ball_ids(a).contains(&vic_ship))
            .unwrap_or_else(|| panic!("missing victim RemoveBall(s) in {actions:?}"));
        let wreck_pos = actions
            .iter()
            .position(|(n, a)| n == "AddBalls2" && add_balls2_has_id(a, wreck_id))
            .unwrap_or_else(|| panic!("missing wreck AddBalls2({wreck_id}) in {actions:?}"));
        assert!(
            fx_pos < rem_pos && rem_pos < wreck_pos,
            "observer death sequence must be destruction FX ({fx_pos}) < victim remove ({rem_pos}) < wreck AddBalls2 ({wreck_pos}): {actions:?}"
        );
    }

    #[tokio::test]
    async fn pod_ego_ball_after_shipid_session_change() {
        // parity: feat:combat.ship-destruction, feat:combat.capsule-ejection-items, notify:OnSessionChanged, destiny:AddBalls2
        eve_testkit::attest(&[
            "feat:combat.ship-destruction",
            "feat:combat.capsule-ejection-items",
            "notify:OnSessionChanged",
            "destiny:AddBalls2",
        ]);

        let ctx = ServerContext::new();
        let (_att_sess, _att_ship, mut vic_sess, vic_ship, obs_ship, sys) =
            setup_combatant_with_observer(&ctx, 796_000, 587, 587, None).await;
        let _ = sys.drain_observer_updates(obs_ship).await.unwrap();
        let _ = ctx.drain_notifications_for_char(VIC_CHAR);

        let death = execute_death_pipeline(&ctx, SYS_ID, VIC_CHAR, vic_ship, ATT_CHAR, DVec3::new(1_500.0, 0.0, 0.0), ctx.now_unix())
            .await
            .expect("ship death");
        let pod_id = death.ejected_capsule_id.expect("capsule ejected");
        assert_ne!(pod_id, vic_ship);
        assert_eq!(
            ctx.db.with_snapshot(|s| s.characters[&VIC_CHAR].active_ship_id),
            pod_id
        );

        let (_tick, _evs, obs_updates) = sys.drain_observer_updates(obs_ship).await.unwrap();
        let actions = parse_destiny_updates(&obs_updates);
        assert!(
            actions.iter().any(|(n, a)| n == "AddBalls2" && add_balls2_has_id(a, pod_id)),
            "ejected capsule {pod_id} must appear via AddBalls2"
        );
        let pod_ball = sys.get_ball_snapshot(pod_id).await.unwrap().expect("pod ball present");
        assert_eq!(pod_ball.1.type_id, 670);

        // Victim's session shipid transitions to capsule cleanly after pod AddBalls2 is on grid
        let _ = ctx.execute_session_transition_sync(
            Some(&mut vic_sess),
            eve_server::router::SessionTransitionSpec::for_kind(
                VIC_CHAR,
                eve_server::router::TransitionKind::EjectToPod { capsule_id: pod_id },
                "eject",
            )
            .with_apply_db(false),
        );
        assert_eq!(vic_sess.ship_id, Some(pod_id));
        let vic_notifs = ctx.drain_notifications_for_char(VIC_CHAR);
        assert!(
            vic_notifs.iter().any(|n| n.event == "OnSessionChanged"),
            "victim must receive session change notification for capsule shipid"
        );
    }

    #[tokio::test]
    async fn podded_capsule_removeballs_reaches_observers() {
        // parity: feat:combat.pod-destruction, destiny:TerminalPlayDestructionEffect, destiny:RemoveBalls
        eve_testkit::attest(&[
            "feat:combat.pod-destruction",
            "destiny:TerminalPlayDestructionEffect",
            "destiny:RemoveBalls",
        ]);

        let ctx = ServerContext::new();
        let (_att_sess, _att_ship, _vic_sess, vic_ship, obs_ship, sys) =
            setup_combatant_with_observer(&ctx, 797_000, 587, 587, None).await;
        let death = execute_death_pipeline(&ctx, SYS_ID, VIC_CHAR, vic_ship, ATT_CHAR, DVec3::new(1_500.0, 0.0, 0.0), ctx.now_unix())
            .await
            .expect("ship death");
        let pod_id = death.ejected_capsule_id.expect("capsule ejected");
        let _ = sys.drain_observer_updates(obs_ship).await.unwrap();

        let _ = execute_death_pipeline(&ctx, SYS_ID, VIC_CHAR, pod_id, ATT_CHAR, DVec3::new(1_500.0, 0.0, 0.0), ctx.now_unix())
            .await
            .expect("pod death");

        let (_tick, _evs, obs_updates) = sys.drain_observer_updates(obs_ship).await.unwrap();
        let actions = parse_destiny_updates(&obs_updates);
        let fx_pos = actions
            .iter()
            .position(|(n, a)| n == "TerminalPlayDestructionEffect" && a.first() == Some(&PyRep::Integer(pod_id)))
            .unwrap_or_else(|| panic!("missing capsule TerminalPlayDestructionEffect in {actions:?}"));
        let rem_pos = actions
            .iter()
            .position(|(n, a)| (n == "RemoveBall" || n == "RemoveBalls") && remove_ball_ids(a).contains(&pod_id))
            .unwrap_or_else(|| panic!("missing capsule RemoveBall(s) in {actions:?}"));
        assert!(fx_pos < rem_pos, "pod destruction FX ({fx_pos}) must precede RemoveBall(s) ({rem_pos})");
    }
}
