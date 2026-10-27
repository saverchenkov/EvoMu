//! Part 2 integration tests closing review `20261028-015440` Repairs, Cooldown, Smartbomb, & Salvage findings:
//! - `R-SHIELD-ZERO-01`: `0.0` shield HP ships passively recharge above `0.0` on live 1Hz ticks
//! - `R-CAP-DOUBLE-01`: Single `1x` passive capacitor recharge per tick using ship's fitted `capacitor_recharge_time_ms`
//! - `R-COOLDOWN-DECAY-01`: Combined `ctx.advance_secs(1)` + `sys.step_manual(1.0)` decays module cooldown at `1x`, and stepping unrelated systems never wipes unmanned ship cooldowns
//! - `R-REMOTEREP-STACK-01`: Sub-capital remote shield/armor/hull repairers stack 100% additively (`2.0x`)
//! - `R-AOE-SALVAGE-CYCLE-01`: Pre-notification rejection hygiene, Salvager `t = 10s` cycle-end execution, `repeat = 1` Smartbomb auto-pulsing, and Lowsec Crimewatch

use crate::common;
use super::combat_repairs_smartbomb_closure::{grant_all_skills_for_types, rpc_lock_target, seed_item};

use eve_db::InventoryFlag;
use eve_destiny::{combat, Ball, CombatState, DVec3, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::with_mut_combat_runtime;
use std::collections::HashMap;

#[tokio::test]
async fn test_regression_spawn_ball_with_meta_untackled_warp_scramble_strength_zero() {
    let ctx = ServerContext::new();
    let sys = ctx.systems.get_or_spawn(30000142);
    let ball_id = 999_888_777i64;
    let ball = Ball {
        id: ball_id,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 3.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let cs = CombatState { warp_core_strength: 2, warp_scramble_strength: 0, ..CombatState::default() };
    let meta = ctx.build_ship_slim_meta(90_000_001, 587);
    sys.spawn_ball_with_meta(ball, cs, meta).await.unwrap();

    let fetched = sys.get_combat_state(ball_id).await.unwrap().expect("combat state exists");
    assert_eq!(fetched.warp_scramble_strength, 0);
    assert_eq!(fetched.warp_core_strength, 2);

    sys.tick().await.unwrap();
    let ticked = sys.get_combat_state(ball_id).await.unwrap().expect("combat state after tick");
    assert_eq!(ticked.warp_scramble_strength, 0);
}

#[tokio::test]
async fn test_zero_shield_passive_recharge_and_single_rate_capacitor_and_cooldown_decay() {
    // parity: feat:combat.passive-shield-recharge-peak, feat:combat.ancillary-armor-repairer-paste, feat:combat.ancillary-shield-booster-charges, rpc:charUnboundMgr.SelectCharacterID
    eve_testkit::attest(&[
        "feat:combat.passive-shield-recharge-peak",
        "feat:combat.ancillary-armor-repairer-paste",
        "feat:combat.ancillary-shield-booster-charges",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess = SessionState::new(9261);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None)
        .await
        .expect("SelectCharacterID");

    let char_id = 90_000_001i64;
    let ship_id = 100_000_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, i| i.location_id != ship_id);
        if let Some(ship_item) = snap.items.get_mut(&ship_id) {
            ship_item.type_id = 11381; // Harpy
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
    grant_all_skills_for_types(&ctx, char_id, &[11381, 47257, 33076, 32774]);

    let adc_id = 100_927_001i64;
    let aar_id = 100_927_002i64;
    let asb_id = 100_927_003i64;
    let cargo_paste_id = 100_927_004i64;
    let cargo_cap_id = 100_927_005i64;
    seed_item(&ctx, adc_id, 47257, char_id, ship_id, InventoryFlag::LoSlot0, 1);
    seed_item(&ctx, aar_id, 33076, char_id, ship_id, InventoryFlag::LoSlot1, 1);
    seed_item(&ctx, asb_id, 32774, char_id, ship_id, InventoryFlag::MedSlot0, 1);
    seed_item(&ctx, cargo_paste_id, 28668, char_id, ship_id, InventoryFlag::Cargo, 32);
    seed_item(&ctx, cargo_cap_id, 263, char_id, ship_id, InventoryFlag::Cargo, 10);
    ctx.db.with_mut_snapshot(|snap| {
        for mid in [adc_id, aar_id, asb_id] {
            snap.module_online_states.insert(mid, true);
        }
    });
    ctx.invalidate_ship_fitting(ship_id);

    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None).await.expect("Undock");
    let sys_id = sess.solar_system_id.unwrap();
    let sys = ctx.systems.get_or_spawn(sys_id);

    // 1. `R-SHIELD-ZERO-01` & `R-CAP-DOUBLE-01`:
    //    - Ship knocked down to `0.0` shield HP MUST passively recharge above `0.0` on 1Hz ticks!
    //    - Capacitor at `25%` (`250 / 1000 GJ`) with `capacitor_recharge_time_ms = 100_000.0` (100s)
    //      MUST recharge by exact `1x` `compute_passive_recharge(250.0, 1000.0, 100_000.0, 1.0)` (~`24.69` GJ),
    //      NOT `1x + hardcoded 250s` double-tick (~`34.69` GJ)!
    let mut cs = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    cs.max_shield_hp = 1_000.0;
    cs.shield_hp = 0.0;
    cs.shield_recharge_time_ms = 100_000.0;
    cs.max_capacitor = 1_000.0;
    cs.capacitor = 250.0;
    cs.capacitor_recharge_time_ms = 100_000.0;
    sys.set_combat_state(ship_id, cs).await.unwrap();

    let expected_cap_after_1s = combat::compute_passive_recharge(250.0, 1_000.0, 100_000.0, 1_000.0);
    let expected_sh_from_zero = combat::compute_passive_recharge(0.0, 1_000.0, 100_000.0, 1_000.0);
    assert!(expected_sh_from_zero > 0.0, "Analytical compute_passive_recharge at 0.0 shield must be > 0.0");

    sys.tick().await.unwrap();
    let cs_t1 = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(
        cs_t1.shield_hp > 0.0 && (cs_t1.shield_hp - expected_sh_from_zero).abs() < 1e-4,
        "0.0 shield HP ship must passively recover on live 1Hz tick (got {}, expected {})",
        cs_t1.shield_hp,
        expected_sh_from_zero
    );
    assert!(
        (cs_t1.capacitor - expected_cap_after_1s).abs() < 1e-3,
        "Capacitor must recharge at exact 1x fitted rate without duplicate 250s loop (got {}, expected {})",
        cs_t1.capacitor,
        expected_cap_after_1s
    );
    for _ in 0..5 {
        sys.tick().await.unwrap();
    }
    let cs_t6 = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(cs_t6.shield_hp > cs_t1.shield_hp, "Shield HP must continue climbing out of 0.0 over subsequent ticks");

    // 2. `R-COOLDOWN-DECAY-01`:
    //    (a) 60s AAR & ASB reload timers under combined `ctx.advance_secs(1)` + `sys.step_manual(1.0)`:
    //        still `ModuleIsReloading` at `t = 59s`, unlocked at `t = 60s`!
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "LoadAmmo",
        &[PyRep::Integer(ship_id), PyRep::List(vec![PyRep::Integer(aar_id)]), PyRep::List(vec![PyRep::Integer(cargo_paste_id)]), PyRep::Integer(ship_id)],
        &kw,
        None,
    )
    .await
    .unwrap();
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "LoadAmmo",
        &[PyRep::Integer(ship_id), PyRep::List(vec![PyRep::Integer(asb_id)]), PyRep::List(vec![PyRep::Integer(cargo_cap_id)]), PyRep::Integer(ship_id)],
        &kw,
        None,
    )
    .await
    .unwrap();
    for _ in 0..59 {
        ctx.advance_secs(1);
        sys.step_manual(1.0).await.unwrap();
    }
    assert_eq!(
        ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(aar_id), PyRep::String("fueledArmorRepair".into()), PyRep::None, PyRep::Integer(0)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "ModuleIsReloading"
    );
    assert_eq!(
        ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(asb_id), PyRep::String("fueledShieldBoosting".into()), PyRep::None, PyRep::Integer(0)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "ModuleIsReloading"
    );
    ctx.advance_secs(1);
    sys.step_manual(1.0).await.unwrap();
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(asb_id), PyRep::String("fueledShieldBoosting".into()), PyRep::None, PyRep::Integer(0)], &kw, None)
        .await
        .expect("ASB must unlock at exact t = 60s");
    for _ in 0..5 {
        sys.step_manual(1.0).await.unwrap();
    }

    //    (b) 150s reactivation delay on Assault Damage Control II (`47257`) (`duration ~10.8s + 150s` total delay):
    //        Advancing BOTH `ctx.advance_secs(1)` AND `sys.step_manual(1.0)` in lockstep must decay at `1x`, NOT `2x`!
    sys.set_capacitor(ship_id, 1_000.0).await.unwrap();
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(adc_id), PyRep::String("moduleBonusAssaultDamageControl".into()), PyRep::None, PyRep::Integer(0)],
        &kw,
        None,
    )
    .await
    .expect("Activate ADC II");
    // Under the old double-decay bug, 90 lockstep ticks decayed 180s > 161s and prematurely unlocked ADC II!
    for _ in 0..90 {
        ctx.advance_secs(1);
        sys.step_manual(1.0).await.unwrap();
    }
    let premature_err = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(adc_id), PyRep::String("moduleBonusAssaultDamageControl".into()), PyRep::None, PyRep::Integer(0)],
            &kw,
            None,
        )
        .await
        .expect_err("ADC II must STILL be on reactivation cooldown at 90s under lockstep wall+sim progression");
    assert_eq!(premature_err.code, "ModuleReactivationDelay");

    // Advance remaining 75s (`90 + 75 = 165s >= duration + 150s`) -> cooldown expires!
    for _ in 0..75 {
        ctx.advance_secs(1);
        sys.step_manual(1.0).await.unwrap();
    }
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(adc_id), PyRep::String("moduleBonusAssaultDamageControl".into()), PyRep::None, PyRep::Integer(0)],
        &kw,
        None,
    )
    .await
    .expect("ADC II must reactivate once 1x total delay elapses");

    //    (c) Unmanned/ejected ship in Amarr (`30002187`) must NOT have its cooldown wiped when Jita (`30000142`) steps!
    let amarr_sys_id = 30_002_187u32;
    let amarr_sys = ctx.systems.get_or_spawn(amarr_sys_id);
    let unmanned_ship_id = 100_927_050i64;
    let unmanned_mod_id = 100_927_051i64;
    seed_item(&ctx, unmanned_ship_id, 587, char_id, amarr_sys_id as i64, InventoryFlag::None, 1);
    seed_item(&ctx, unmanned_mod_id, 47257, char_id, unmanned_ship_id, InventoryFlag::LoSlot0, 1);
    amarr_sys
        .spawn_ball_with_combat(
            Ball {
                id: unmanned_ship_id,
                type_id: 587,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 3.0,
                mass: 1_000_000.0,
                radius: 35.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await
        .unwrap();
    ctx.record_module_reactivation_delay(unmanned_ship_id, unmanned_mod_id, 120.0);
    assert!(ctx.is_module_in_reactivation_delay(unmanned_ship_id, unmanned_mod_id));
    // Step Jita (`sys`) for 500s without advancing wall clock or Amarr (`amarr_sys`):
    sys.step_manual(500.0).await.unwrap();
    assert!(
        ctx.is_module_in_reactivation_delay(unmanned_ship_id, unmanned_mod_id),
        "Stepping Jita must NEVER decay cooldowns on an unmanned ship in Amarr"
    );
    // Stepping Amarr (`amarr_sys`) for 121s DOES expire the Amarr ship's cooldown:
    amarr_sys.step_manual(121.0).await.unwrap();
    assert!(!ctx.is_module_in_reactivation_delay(unmanned_ship_id, unmanned_mod_id));
}

#[tokio::test]
async fn test_subcap_additive_remote_repairs_and_smartbomb_salvager_cycle_and_lowsec() {
    // parity: feat:combat.remote-repair-falloff, feat:combat.smartbomb-aoe-damage, feat:combat.wreck-salvaging-vs-looting-legality, rpc:charUnboundMgr.SelectCharacterID
    eve_testkit::attest(&[
        "feat:combat.remote-repair-falloff",
        "feat:combat.smartbomb-aoe-damage",
        "feat:combat.wreck-salvaging-vs-looting-legality",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let win_salv_seed = (0u64..1000).find(|&s| {
        ctx.set_mechanic_rng_seed(Some(s));
        ctx.mechanic_rng.try_unit_f64().unwrap() < 0.01
    }).unwrap();
    ctx.set_mechanic_rng_seed(Some(win_salv_seed));
    let kw = HashMap::new();
    let mut sess = SessionState::new(9262);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None)
        .await
        .expect("SelectCharacterID");

    let char_id = 90_000_001i64;
    let ship_id = 100_000_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, i| i.location_id != ship_id);
        if let Some(ship_item) = snap.items.get_mut(&ship_id) {
            ship_item.type_id = 24692; // Abaddon (8 HiSlots, 21k PG, 560 CPU)
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
    grant_all_skills_for_types(&ctx, char_id, &[24692, 3586, 11355, 4299, 3993, 25861]);
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            3438,
            eve_db::TrainedSkillRecord { type_id: 3438, level: 5, skill_points: 256_000.0 },
        );
    });

    let (rsb1, rsb2, rar1, rar2, rhr1, rhr2, sb_id, salv_id) = (
        100_928_001i64,
        100_928_002i64,
        100_928_003i64,
        100_928_004i64,
        100_928_005i64,
        100_928_006i64,
        100_928_007i64,
        100_928_008i64,
    );
    for (id, tid, flag) in [
        (rsb1, 3586u32, InventoryFlag::HiSlot0),
        (rsb2, 3586, InventoryFlag::HiSlot1),
        (rar1, 11355, InventoryFlag::HiSlot2),
        (rar2, 11355, InventoryFlag::HiSlot3),
        (rhr1, 4299, InventoryFlag::HiSlot4),
        (rhr2, 4299, InventoryFlag::HiSlot5),
        (sb_id, 3993, InventoryFlag::HiSlot6),
        (salv_id, 25861, InventoryFlag::HiSlot7),
    ] {
        seed_item(&ctx, id, tid, char_id, ship_id, flag, 1);
    }
    ctx.db.with_mut_snapshot(|snap| {
        for mid in [rsb1, rsb2, rar1, rar2, rhr1, rhr2, sb_id, salv_id] {
            snap.module_online_states.insert(mid, true);
        }
    });
    ctx.invalidate_ship_fitting(ship_id);

    // Move pilot & ship directly into Lowsec Amamake (`30002537`, sec ~0.4) to test Lowsec Crimewatch & additive sub-cap reps!
    let lowsec_id = 30_002_537u32;
    let sys = ctx.systems.get_or_spawn(lowsec_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.location_id = lowsec_id as i64;
            sh.flag = InventoryFlag::None;
        }
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = lowsec_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
    });
    sess.solar_system_id = Some(lowsec_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);

    let ship_pos = DVec3::new(50_000_000.0, 0.0, 0.0);
    sys.spawn_ball_with_combat(
        Ball {
            id: ship_id,
            type_id: 24692,
            position: ship_pos,
            velocity: DVec3::ZERO,
            max_velocity: 100.0,
            inertia: 5.0,
            mass: 10_000_000.0,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        CombatState {
            max_capacitor: 50_000.0,
            capacitor: 50_000.0,
            capacitor_recharge_time_ms: 0.0,
            shield_recharge_time_ms: 0.0,
            ..CombatState::default()
        },
    )
    .await
    .unwrap();

    // 1. `R-REMOTEREP-STACK-01`: Sub-capital Remote Shield (`3586`), Armor (`11355`), and Hull (`4299`)
    //    repairers MUST stack 100% additively (`2.0x` single-module HP, zero stacking penalty)!
    let ally_char = 90_000_002i64;
    let ally_ship = 100_928_020i64;
    seed_item(&ctx, ally_ship, 587, ally_char, lowsec_id as i64, InventoryFlag::None, 1);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&ally_char) {
            ch.solar_system_id = lowsec_id;
            ch.active_ship_id = ally_ship;
        }
    });
    let base_ally_cs = CombatState {
        shield_hp: 200.0,
        max_shield_hp: 5_000.0,
        armor_hp: 200.0,
        max_armor_hp: 5_000.0,
        structure_hp: 200.0,
        max_structure_hp: 5_000.0,
        shield_recharge_time_ms: 0.0,
        capacitor_recharge_time_ms: 0.0,
        ..CombatState::default()
    };
    sys.spawn_ball_with_combat(
        Ball {
            id: ally_ship,
            type_id: 587,
            position: ship_pos + DVec3::new(2_500.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 3.0,
            mass: 1_000_000.0,
            radius: 35.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        base_ally_cs.clone(),
    )
    .await
    .unwrap();
    rpc_lock_target(&ctx, &sess, char_id, ship_id, ally_ship).await;

    // Dual RSB (`t = 0`): 65 + 65 = 130 HP
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(rsb1), PyRep::String("shipModuleRemoteShieldBooster".into()), PyRep::Integer(ally_ship), PyRep::Integer(0)], &kw, None).await.unwrap();
    let sh_1 = sys.get_combat_state(ally_ship).await.unwrap().unwrap().shield_hp - 200.0;
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(rsb2), PyRep::String("shipModuleRemoteShieldBooster".into()), PyRep::Integer(ally_ship), PyRep::Integer(0)], &kw, None).await.unwrap();
    let sh_2 = sys.get_combat_state(ally_ship).await.unwrap().unwrap().shield_hp - 200.0;
    assert!((sh_2 - 2.0 * sh_1).abs() < 1e-3, "2 sub-capital RSBs must apply exact 2.0x shield HP (got {sh_2} vs 2x {sh_1})");

    // Dual RAR (`t = duration_s`): 2.0x single armor repair
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(rar1), PyRep::String("shipModuleRemoteArmorRepairer".into()), PyRep::Integer(ally_ship), PyRep::Integer(0)], &kw, None).await.unwrap();
    for _ in 0..6 {
        sys.step_manual(1.0).await.unwrap();
        ctx.advance_secs(1);
    }
    let ar_1 = sys.get_combat_state(ally_ship).await.unwrap().unwrap().armor_hp - 200.0;
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(rar1), PyRep::String("shipModuleRemoteArmorRepairer".into()), PyRep::Integer(ally_ship), PyRep::Integer(0)], &kw, None).await.unwrap();
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(rar2), PyRep::String("shipModuleRemoteArmorRepairer".into()), PyRep::Integer(ally_ship), PyRep::Integer(0)], &kw, None).await.unwrap();
    for _ in 0..6 {
        sys.step_manual(1.0).await.unwrap();
        ctx.advance_secs(1);
    }
    let ar_dual = sys.get_combat_state(ally_ship).await.unwrap().unwrap().armor_hp - (200.0 + ar_1);
    assert!((ar_dual - 2.0 * ar_1).abs() < 1e-3, "2 sub-capital RARs must apply exact 2.0x armor HP (got {ar_dual} vs 2x {ar_1})");

    // Dual Remote Hull Repairer (`4299`, `t = duration_s`): 2.0x single hull repair
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(rhr1), PyRep::String("RemoteHullRepair".into()), PyRep::Integer(ally_ship), PyRep::Integer(0)], &kw, None).await.unwrap();
    for _ in 0..8 {
        sys.step_manual(1.0).await.unwrap();
        ctx.advance_secs(1);
    }
    let hr_1 = sys.get_combat_state(ally_ship).await.unwrap().unwrap().structure_hp - 200.0;
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(rhr1), PyRep::String("RemoteHullRepair".into()), PyRep::Integer(ally_ship), PyRep::Integer(0)], &kw, None).await.unwrap();
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(rhr2), PyRep::String("RemoteHullRepair".into()), PyRep::Integer(ally_ship), PyRep::Integer(0)], &kw, None).await.unwrap();
    for _ in 0..8 {
        sys.step_manual(1.0).await.unwrap();
        ctx.advance_secs(1);
    }
    let hr_dual = sys.get_combat_state(ally_ship).await.unwrap().unwrap().structure_hp - (200.0 + hr_1);
    assert!((hr_dual - 2.0 * hr_1).abs() < 1e-3, "2 sub-capital RHRs must apply exact 2.0x hull HP (got {hr_dual} vs 2x {hr_1})");

    // 2. `R-AOE-SALVAGE-CYCLE-01`:
    //    (a) Failed Smartbomb (Safety Green) & failed Salvager (non-wreck ship target) emit ZERO `OnGodmaShipEffect`/`OnHeatAdded`!
    ctx.crimewatch.write().unwrap().states.clear();
    with_mut_combat_runtime(&ctx, |rt| {
        rt.limited_engagements.clear();
    });
    ctx.dispatch_rpc(&mut sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(2)], &kw, None).await.unwrap();
    let _ = sess.drain_notifications();
    let sb_green_err = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "Overload", &[PyRep::Integer(sb_id), PyRep::String("empWave".into()), PyRep::None, PyRep::Integer(1)], &kw, None)
        .await
        .expect_err("Smartbomb on Safety Green in Lowsec must fail");
    assert_eq!(sb_green_err.code, "SafetyLevelRestricted");
    let leaked_sb_notes = sess.drain_notifications();
    assert!(
        leaked_sb_notes.iter().all(|(n, _)| n != "OnGodmaShipEffect" && n != "OnHeatAdded"),
        "Rejected Smartbomb activation must NOT leak OnGodmaShipEffect or OnHeatAdded: {leaked_sb_notes:?}"
    );

    let salv_bad_err = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(salv_id), PyRep::String("salvaging".into()), PyRep::Integer(ally_ship), PyRep::Integer(0)], &kw, None)
        .await
        .expect_err("Salvager on non-wreck ship must fail at t=0");
    assert_eq!(salv_bad_err.code, "InvalidSalvageTarget");
    let leaked_salv_notes = sess.drain_notifications();
    assert!(
        leaked_salv_notes.iter().all(|(n, _)| n != "OnGodmaShipEffect" && n != "OnHeatAdded"),
        "Rejected Salvager activation must NOT leak OnGodmaShipEffect or OnHeatAdded: {leaked_salv_notes:?}"
    );

    //    (b) Lowsec Smartbomb on Safety Yellow with `repeat = 1`:
    //        - Deducts exact `1x` capNeed on cycle 1 (`t = 0`), damages `ally_ship` (`2,500m <= 5,000m`),
    //          flags pilot **Suspect** (`900s`) WITHOUT Criminal/CONCORD!
    //        - Auto-pulses again on cycle 2 (`t = 10s`) when `sys.step_manual` advances without re-calling `Activate`!
    ctx.dispatch_rpc(&mut sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(1)], &kw, None).await.unwrap();
    sys.set_capacitor(ship_id, 10_000.0).await.unwrap();
    let mut sb_ally_cs = base_ally_cs.clone();
    sb_ally_cs.max_shield_hp = 5_000.0;
    sb_ally_cs.shield_hp = 5_000.0;
    sys.set_combat_state(ally_ship, sb_ally_cs).await.unwrap();
    let sb_fit = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap();
    let sb_cap_need = sb_fit.build_effective_module_state(sb_id, 3993, false, &ctx.sde).get_attribute(6, &ctx.sde);
    let sb_dur_s = sb_fit.build_effective_module_state(sb_id, 3993, false, &ctx.sde).get_attribute(73, &ctx.sde) / 1000.0;

    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(sb_id), PyRep::String("empWave".into()), PyRep::None, PyRep::Integer(1)], &kw, None)
        .await
        .expect("Lowsec Smartbomb on Safety Yellow (repeat=1)");
    let cap_after_c1 = sys.get_combat_state(ship_id).await.unwrap().unwrap().capacitor;
    assert!(
        (10_000.0 - cap_after_c1 - sb_cap_need).abs() < 1e-2,
        "Smartbomb cycle 1 must deduct exact 1x capNeed ({sb_cap_need}), got {}",
        10_000.0 - cap_after_c1
    );
    let ally_sh_c1 = sys.get_combat_state(ally_ship).await.unwrap().unwrap().shield_hp;
    assert!(ally_sh_c1 < 5_000.0 && ally_sh_c1 > 1_000.0, "Smartbomb pulse 1 must damage neutral ship shield at t=0");
    let cw_low_sb = ctx.crimewatch.with_crimewatch(|cw| cw.states.get(&char_id).cloned().unwrap_or_default());
    assert!(cw_low_sb.suspect_flag_until_unix >= ctx.now_unix_secs() + 895, "Lowsec Smartbomb hitting neutral must grant 15m Suspect flag");
    assert_eq!(cw_low_sb.criminal_flag_until_unix, 0, "Lowsec Smartbomb on non-pod ship must NOT grant Criminal flag");
    assert!(cw_low_sb.pending_concord_arrival.is_none(), "Lowsec Smartbomb must NOT trigger CONCORD");

    // Step across `sb_dur_s` so `repeat = 1` auto-fires cycle 2!
    let steps = (sb_dur_s.ceil() as usize) + 1;
    for _ in 0..steps {
        sys.step_manual(1.0).await.unwrap();
        ctx.advance_secs(1);
    }
    let ally_sh_c2 = sys.get_combat_state(ally_ship).await.unwrap().unwrap().shield_hp;
    assert!(ally_sh_c2 < ally_sh_c1, "Repeating Smartbomb (repeat=1) must auto-pulse AoE damage on cycle 2 ({ally_sh_c2} < {ally_sh_c1})");
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(sb_id), PyRep::String("empWave".into())], &kw, None).await.unwrap();

    //    (c) Lowsec Yellow-Wreck Salvaging (`t = 0` intact, `t = 10s` salvaged, Green legal, 0 Suspect) vs `LootAll` (Green blocked, Yellow Suspect)!
    ctx.crimewatch.write().unwrap().states.clear();
    ctx.dispatch_rpc(&mut sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(2)], &kw, None).await.unwrap();
    let low_wreck_salv = 100_928_040i64;
    let low_wreck_loot = 100_928_041i64;
    let low_loot_item = 100_928_042i64;
    seed_item(&ctx, low_wreck_salv, 26473, ally_char, lowsec_id as i64, InventoryFlag::None, 1);
    seed_item(&ctx, low_wreck_loot, 26473, ally_char, lowsec_id as i64, InventoryFlag::None, 1);
    seed_item(&ctx, low_loot_item, 34, ally_char, low_wreck_loot, InventoryFlag::Cargo, 100);
    for wid in [low_wreck_salv, low_wreck_loot] {
        sys.spawn_ball_with_combat(
            Ball {
                id: wid,
                type_id: 26473,
                position: ship_pos + DVec3::new(1_500.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 10_000.0,
                radius: 25.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await
        .unwrap();
    }
    rpc_lock_target(&ctx, &sess, char_id, ship_id, low_wreck_salv).await;

    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(salv_id), PyRep::String("salvaging".into()), PyRep::Integer(low_wreck_salv), PyRep::Integer(0)], &kw, None)
        .await
        .expect("Lowsec foreign yellow wreck Salvaging on Safety Green must succeed");
    assert!(ctx.db.with_snapshot(|s| s.items.contains_key(&low_wreck_salv)), "Wreck must NOT be salvaged at t=0");
    for _ in 0..9 {
        sys.step_manual(1.0).await.unwrap();
        ctx.advance_secs(1);
    }
    assert!(ctx.db.with_snapshot(|s| s.items.contains_key(&low_wreck_salv)), "Wreck must still be intact at t=9s before 10s cycle completes");
    ctx.set_mechanic_rng_seed(Some(win_salv_seed));
    sys.step_manual(1.0).await.unwrap();
    ctx.advance_secs(1);
    assert!(!ctx.db.with_snapshot(|s| s.items.contains_key(&low_wreck_salv)), "Wreck must be salvaged and removed at t=10s cycle end");
    let cw_low_salv = ctx.crimewatch.with_crimewatch(|cw| cw.states.get(&char_id).cloned().unwrap_or_default());
    assert_eq!(cw_low_salv.suspect_flag_until_unix, 0, "Salvaging Lowsec yellow wreck must NOT flag Suspect");

    assert!(ctx.dispatch_rpc(&mut sess, "invbroker", None, "LootAll", &[PyRep::Integer(low_wreck_loot)], &kw, None).await.is_err());
    ctx.dispatch_rpc(&mut sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(1)], &kw, None).await.unwrap();
    ctx.dispatch_rpc(&mut sess, "invbroker", None, "LootAll", &[PyRep::Integer(low_wreck_loot)], &kw, None).await.expect("LootAll on Safety Yellow in Lowsec");
    let cw_low_loot = ctx.crimewatch.with_crimewatch(|cw| cw.states.get(&char_id).cloned().unwrap_or_default());
    assert!(cw_low_loot.suspect_flag_until_unix >= ctx.now_unix_secs() + 895, "Looting Lowsec yellow wreck must flag Suspect for 15m");
}
