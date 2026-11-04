//! Comprehensive integration test suite closing the 8 `feat:combat.*` Repairs, Ancillary, Smartbomb, & Salvage rows:
//! - `feat:combat.passive-shield-recharge-peak`
//! - `feat:combat.active-shield-boost-cycle-start`
//! - `feat:combat.active-armor-repair-cycle-end`
//! - `feat:combat.remote-repair-falloff`
//! - `feat:combat.ancillary-armor-repairer-paste`
//! - `feat:combat.ancillary-shield-booster-charges`
//! - `feat:combat.smartbomb-aoe-damage`
//! - `feat:combat.wreck-salvaging-vs-looting-legality`

use crate::common;

use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, CombatState, DVec3, MovementMode};
use eve_dogma::{
    compute_falloff_with_cutoff, compute_passive_recharge_rate, DogmaItemState, ShipFittingEngine,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{with_combat_runtime, with_mut_combat_runtime, DroneSpaceState, InFlightMissileState};
use std::collections::HashMap;

pub(super) fn seed_item(ctx: &ServerContext, item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(item_id, InventoryItemRecord {
            item_id, type_id, owner_id, location_id, flag, quantity, singleton: quantity == 1, version: 1,
            dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None, insured_payout_isk: 0.0,
            is_blueprint_copy: false, blueprint_runs_remaining: None,
        });
    });
}

pub(super) async fn rpc_lock_target(ctx: &ServerContext, base_sess: &SessionState, char_id: i64, ship_id: i64, target_id: i64) {
    let sys = ctx.systems.get_or_spawn(base_sess.solar_system_id.unwrap());
    if sys.get_target_locks(ship_id).await.unwrap().iter().any(|l| l.target_id == target_id && l.locked) { return; }
    let mut s = base_sess.clone();
    s.character_id = Some(char_id);
    s.ship_id = Some(ship_id);
    ctx.dispatch_rpc(&mut s, "dogmaIM", None, "AddTarget", &[PyRep::Integer(target_id)], &HashMap::new(), None).await.expect("dogmaIM.AddTarget");
    for _ in 0..120 {
        if sys.get_target_locks(ship_id).await.unwrap().iter().any(|l| l.target_id == target_id && l.locked) { break; }
        sys.step_manual(1.0).await.unwrap();
    }
    assert!(sys.get_target_locks(ship_id).await.unwrap().iter().any(|l| l.target_id == target_id && l.locked), "Target lock must complete via dogmaIM.AddTarget");
}

pub(super) fn grant_all_skills_for_types(ctx: &ServerContext, char_id: i64, type_ids: &[u32]) {
    ctx.db.with_mut_snapshot(|snap| {
        let entry = snap.skills.entry(char_id).or_default();
        for &(sid, lvl) in &[
            (3426u32, 5u8), (3413, 5), (3418, 5), (3417, 5), (3428, 5), (3429, 5), (3431, 5), (3392, 5), (3393, 5), (3416, 5), (25863, 5), (3436, 5),
        ] {
            entry.insert(sid, eve_db::TrainedSkillRecord { type_id: sid, level: lvl, skill_points: 256_000.0 });
        }
        for &tid in type_ids {
            if let Some(t) = ctx.sde.types.get(&tid) {
                for (s_attr, l_attr) in [
                    (eve_sde::attr_ids::REQUIRED_SKILL_1, eve_sde::attr_ids::REQUIRED_SKILL_1_LEVEL),
                    (eve_sde::attr_ids::REQUIRED_SKILL_2, eve_sde::attr_ids::REQUIRED_SKILL_2_LEVEL),
                    (eve_sde::attr_ids::REQUIRED_SKILL_3, eve_sde::attr_ids::REQUIRED_SKILL_3_LEVEL),
                ] {
                    if let Some(&sid_f) = t.default_attributes.get(&s_attr) {
                        let sid = sid_f as u32;
                        if sid > 0 {
                            let lvl = t.default_attributes.get(&l_attr).copied().unwrap_or(5.0).max(5.0) as u8;
                            entry.insert(sid, eve_db::TrainedSkillRecord { type_id: sid, level: lvl, skill_points: 256_000.0 });
                        }
                    }
                }
            }
        }
    });
}

#[tokio::test]
async fn test_combat_repairs_ancillary_smartbomb_and_salvage_legality_closure() {
    // parity: feat:combat.passive-shield-recharge-peak, feat:combat.active-shield-boost-cycle-start, feat:combat.active-armor-repair-cycle-end, feat:combat.remote-repair-falloff, feat:combat.ancillary-armor-repairer-paste, feat:combat.ancillary-shield-booster-charges, feat:combat.smartbomb-aoe-damage, feat:combat.wreck-salvaging-vs-looting-legality, rpc:charUnboundMgr.SelectCharacterID
    eve_testkit::attest(&[
        "feat:combat.passive-shield-recharge-peak",
        "feat:combat.active-shield-boost-cycle-start",
        "feat:combat.active-armor-repair-cycle-end",
        "feat:combat.remote-repair-falloff",
        "feat:combat.ancillary-armor-repairer-paste",
        "feat:combat.ancillary-shield-booster-charges",
        "feat:combat.smartbomb-aoe-damage",
        "feat:combat.wreck-salvaging-vs-looting-legality",
    ]);

    // 10. Passive Shield & Capacitor Recharge Peak (`feat:combat.passive-shield-recharge-peak`)
    let c_max = 1_000.0;
    let tau = 100.0;
    let rate_0 = compute_passive_recharge_rate(0.0, c_max, tau);
    let rate_10 = compute_passive_recharge_rate(100.0, c_max, tau);
    let rate_25 = compute_passive_recharge_rate(250.0, c_max, tau);
    let rate_50 = compute_passive_recharge_rate(500.0, c_max, tau);
    let rate_100 = compute_passive_recharge_rate(1_000.0, c_max, tau);
    assert_eq!(rate_0, 0.0);
    assert_eq!(rate_100, 0.0);
    assert!((rate_25 - 2.5 * c_max / tau).abs() < 1e-6, "Passive recharge must peak at 2.5 * C_max / tau at 25% capacity");
    assert!(rate_25 > rate_10 && rate_25 > rate_50);

    // 13. Remote Repair Falloff with 3x Falloff Cutoff (`feat:combat.remote-repair-falloff`)
    assert!((compute_falloff_with_cutoff(5_000.0, 10_000.0, 5_000.0) - 1.0).abs() < 1e-6);
    assert!((compute_falloff_with_cutoff(15_000.0, 10_000.0, 5_000.0) - 0.5).abs() < 1e-6);
    assert_eq!(compute_falloff_with_cutoff(25_001.0, 10_000.0, 5_000.0), 0.0, "Remote repair must be 0% beyond optimal + 3 * falloff");

    let ctx = ServerContext::new();
    let test_fit = ShipFittingEngine::from_sde(1, 587, &ctx.sde);
    let rsb_item = DogmaItemState::from_sde(2, 3586, &ctx.sde);
    assert_eq!(test_fit.evaluate_projected_remote_repair(&rsb_item, 100_000.0, &ctx.sde), 0.0);
    let kw = HashMap::new();
    let mut sess = SessionState::new(9202);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None).await.unwrap();

    let char_id = 90_000_001i64;
    let ship_id = 100_000_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, i| i.location_id != ship_id);
        if let Some(ship_item) = snap.items.get_mut(&ship_id) { ship_item.type_id = 23919; }
    });
    ctx.invalidate_ship_fitting(ship_id);
    grant_all_skills_for_types(&ctx, char_id, &[23919, 399, 523, 3586, 11355, 33076, 32774, 3993, 25861, 32787, 24348]);

    let (sb_id, ar_id, aar_id, paste_id, asb_id, cap_charge_id, smartbomb_id, salvager_id, rsb_id, rar_id, cargo_paste_id, cargo_cap_id, tractor_id) = (
        100_921_001i64, 100_921_002i64, 100_921_003i64, 100_921_004i64, 100_921_005i64, 100_921_006i64,
        100_921_007i64, 100_921_008i64, 100_921_009i64, 100_921_010i64, 100_921_011i64, 100_921_012i64, 100_921_013i64,
    );

    seed_item(&ctx, sb_id, 399, char_id, ship_id, InventoryFlag::MedSlot0, 1);
    seed_item(&ctx, asb_id, 32774, char_id, ship_id, InventoryFlag::MedSlot1, 1);
    seed_item(&ctx, cap_charge_id, 263, char_id, ship_id, InventoryFlag::MedSlot1, 1);
    seed_item(&ctx, rsb_id, 3586, char_id, ship_id, InventoryFlag::HiSlot0, 1);
    seed_item(&ctx, ar_id, 523, char_id, ship_id, InventoryFlag::LoSlot0, 1);
    seed_item(&ctx, aar_id, 33076, char_id, ship_id, InventoryFlag::LoSlot1, 1);
    seed_item(&ctx, paste_id, 28668, char_id, ship_id, InventoryFlag::LoSlot1, 2);
    seed_item(&ctx, smartbomb_id, 3993, char_id, ship_id, InventoryFlag::HiSlot1, 1);
    seed_item(&ctx, salvager_id, 25861, char_id, ship_id, InventoryFlag::HiSlot2, 1);
    seed_item(&ctx, rar_id, 11355, char_id, ship_id, InventoryFlag::HiSlot3, 1);
    seed_item(&ctx, tractor_id, 24348, char_id, ship_id, InventoryFlag::HiSlot4, 1);
    seed_item(&ctx, cargo_paste_id, 28668, char_id, ship_id, InventoryFlag::Cargo, 32);
    seed_item(&ctx, cargo_cap_id, 263, char_id, ship_id, InventoryFlag::Cargo, 10);
    ctx.db.with_mut_snapshot(|snap| {
        for mid in [sb_id, asb_id, rsb_id, ar_id, aar_id, smartbomb_id, salvager_id, rar_id, tractor_id] {
            snap.module_online_states.insert(mid, true);
        }
    });
    ctx.invalidate_ship_fitting(ship_id);

    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None).await.expect("Undock");
    let sys_id = sess.solar_system_id.unwrap();
    let sys = ctx.systems.get_or_spawn(sys_id);
    let (_, mut ship_ball) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    ship_ball.position = DVec3::new(100_000_000.0, 0.0, 0.0);
    ship_ball.velocity = DVec3::ZERO;
    ship_ball.speed_fraction = 0.0;
    ship_ball.radius = 40.0;
    ship_ball.mode = MovementMode::Stop;
    let ship_cs_init = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    sys.spawn_ball_with_combat(ship_ball.clone(), ship_cs_init).await.unwrap();

    let mut cs = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    cs.capacitor_recharge_time_ms = 0.0;
    cs.shield_recharge_time_ms = 0.0;
    cs.max_capacitor = 5_000.0;
    cs.capacitor = 1_000.0;
    cs.max_shield_hp = 10_000.0;
    cs.shield_hp = 5_000.0;
    cs.max_armor_hp = 10_000.0;
    cs.armor_hp = 5_000.0;
    sys.set_combat_state(ship_id, cs).await.unwrap();

    // 10. Live 1Hz ballpark passive shield recharge peak on ship_a (`feat:combat.passive-shield-recharge-peak`)
    let mut cs_recharge = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    cs_recharge.shield_recharge_time_ms = 100_000.0;
    cs_recharge.max_shield_hp = 1_000.0;

    // 25% capacity
    cs_recharge.shield_hp = 250.0;
    sys.set_combat_state(ship_id, cs_recharge.clone()).await.unwrap();
    sys.tick().await.unwrap();
    let cs_live_25 = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    let live_delta_25 = cs_live_25.shield_hp - 250.0;

    // 10% capacity
    cs_recharge.shield_hp = 100.0;
    sys.set_combat_state(ship_id, cs_recharge.clone()).await.unwrap();
    sys.tick().await.unwrap();
    let cs_live_10 = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    let live_delta_10 = cs_live_10.shield_hp - 100.0;

    // 80% capacity
    cs_recharge.shield_hp = 800.0;
    sys.set_combat_state(ship_id, cs_recharge.clone()).await.unwrap();
    sys.tick().await.unwrap();
    let cs_live_80 = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    let live_delta_80 = cs_live_80.shield_hp - 800.0;

    assert!(
        live_delta_25 > live_delta_10 && live_delta_25 > live_delta_80,
        "Live 1Hz ballpark ticks must peak passive recharge at 25% shield capacity (got 25%={live_delta_25}, 10%={live_delta_10}, 80%={live_delta_80})"
    );
    assert!((live_delta_25 - rate_25).abs() < 0.1, "Live 1Hz tick delta must match analytical recharge rate");

    // 13. Live Remote Repair Falloff on ship_b assisting target_ship (`feat:combat.remote-repair-falloff`)
    let target_ship_id = 100_921_040i64;
    seed_item(&ctx, target_ship_id, 587, 90_000_002, sys_id as i64, InventoryFlag::None, 1);
    let target_cs = CombatState {
        shield_hp: 500.0,
        max_shield_hp: 2_000.0,
        armor_hp: 500.0,
        max_armor_hp: 2_000.0,
        structure_hp: 1_000.0,
        max_structure_hp: 1_000.0,
        shield_recharge_time_ms: 0.0,
        capacitor_recharge_time_ms: 0.0,
        ..CombatState::default()
    };
    sys.spawn_ball_with_combat(Ball {
        id: target_ship_id, type_id: 587, position: ship_ball.position + DVec3::new(3_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 3.0, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }, target_cs.clone()).await.unwrap();

    // Self-activation prohibition (CannotActivateOnSelf)
    let rsb_self_err = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[
        PyRep::Integer(rsb_id), PyRep::String("shipModuleRemoteShieldBooster".into()), PyRep::Integer(ship_id), PyRep::Integer(0),
    ], &kw, None).await.expect_err("RSB self-activation must fail");
    assert_eq!(rsb_self_err.code, "CannotActivateOnSelf");
    let rar_self_err = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[
        PyRep::Integer(rar_id), PyRep::String("shipModuleRemoteArmorRepairer".into()), PyRep::Integer(ship_id), PyRep::Integer(0),
    ], &kw, None).await.expect_err("RAR self-activation must fail");
    assert_eq!(rar_self_err.code, "CannotActivateOnSelf");

    // (a) d <= optimal (3000m <= RSB opt 4000m, RAR opt 7000m): 100% repair
    rpc_lock_target(&ctx, &sess, char_id, ship_id, target_ship_id).await;
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[
        PyRep::Integer(rsb_id), PyRep::String("shipModuleRemoteShieldBooster".into()), PyRep::Integer(target_ship_id), PyRep::Integer(0),
    ], &kw, None).await.expect("Activate RSB at d <= optimal");
    let rep_rsb_opt = sys.get_combat_state(target_ship_id).await.unwrap().unwrap().shield_hp - 500.0;
    assert!((rep_rsb_opt - 65.0).abs() < 1.0, "RSB within optimal must apply 100% repair (got {rep_rsb_opt})");
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(rsb_id), PyRep::String("shipModuleRemoteShieldBooster".into())], &kw, None).await.unwrap();

    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[
        PyRep::Integer(rar_id), PyRep::String("shipModuleRemoteArmorRepairer".into()), PyRep::Integer(target_ship_id), PyRep::Integer(0),
    ], &kw, None).await.expect("Activate RAR at d <= optimal");
    assert_eq!(sys.get_combat_state(target_ship_id).await.unwrap().unwrap().armor_hp, 500.0);
    for _ in 0..5 { let _ = sys.tick().await; }
    let rep_rar_opt = sys.get_combat_state(target_ship_id).await.unwrap().unwrap().armor_hp - 500.0;
    assert!((rep_rar_opt - 49.0).abs() < 1.0, "RAR within optimal must apply 100% repair (got {rep_rar_opt})");
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(rar_id), PyRep::String("shipModuleRemoteArmorRepairer".into())], &kw, None).await.unwrap();

    // (b) d = optimal + falloff (RSB: 4000 + 6500 = 10,500m; RAR: 7000 + 2500 = 9,500m): ~50% repair
    let (_, mut tb_snapshot) = sys.get_ball_snapshot(target_ship_id).await.unwrap().unwrap();
    tb_snapshot.position = ship_ball.position + DVec3::new(10_500.0, 0.0, 0.0);
    sys.spawn_ball_with_combat(tb_snapshot.clone(), target_cs.clone()).await.unwrap();
    rpc_lock_target(&ctx, &sess, char_id, ship_id, target_ship_id).await;
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[
        PyRep::Integer(rsb_id), PyRep::String("shipModuleRemoteShieldBooster".into()), PyRep::Integer(target_ship_id), PyRep::Integer(0),
    ], &kw, None).await.expect("Activate RSB at d = opt + falloff");
    let rep_rsb_fall = sys.get_combat_state(target_ship_id).await.unwrap().unwrap().shield_hp - 500.0;
    assert!((rep_rsb_fall - 65.0 * 0.5).abs() < 2.0, "RSB at opt + falloff must apply ~50% repair (got {rep_rsb_fall})");
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(rsb_id), PyRep::String("shipModuleRemoteShieldBooster".into())], &kw, None).await.unwrap();

    tb_snapshot.position = ship_ball.position + DVec3::new(9_500.0, 0.0, 0.0);
    sys.spawn_ball_with_combat(tb_snapshot.clone(), target_cs.clone()).await.unwrap();
    rpc_lock_target(&ctx, &sess, char_id, ship_id, target_ship_id).await;
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[
        PyRep::Integer(rar_id), PyRep::String("shipModuleRemoteArmorRepairer".into()), PyRep::Integer(target_ship_id), PyRep::Integer(0),
    ], &kw, None).await.expect("Activate RAR at d = opt + falloff");
    for _ in 0..5 { let _ = sys.tick().await; }
    let rep_rar_fall = sys.get_combat_state(target_ship_id).await.unwrap().unwrap().armor_hp - 500.0;
    assert!((rep_rar_fall - 49.0 * 0.5).abs() < 2.0, "RAR at opt + falloff must apply ~50% repair (got {rep_rar_fall})");
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(rar_id), PyRep::String("shipModuleRemoteArmorRepairer".into())], &kw, None).await.unwrap();

    // (c) d > optimal + 3 * falloff (RSB: 4000 + 19500 = 23,500m -> 24,000m; RAR: 7000 + 7500 = 14,500m -> 15,000m): 0% repair
    tb_snapshot.position = ship_ball.position + DVec3::new(24_000.0, 0.0, 0.0);
    sys.spawn_ball_with_combat(tb_snapshot.clone(), target_cs.clone()).await.unwrap();
    rpc_lock_target(&ctx, &sess, char_id, ship_id, target_ship_id).await;
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[
        PyRep::Integer(rsb_id), PyRep::String("shipModuleRemoteShieldBooster".into()), PyRep::Integer(target_ship_id), PyRep::Integer(0),
    ], &kw, None).await.expect("Activate RSB at d > opt + 3*falloff");
    let rep_rsb_cutoff = sys.get_combat_state(target_ship_id).await.unwrap().unwrap().shield_hp - 500.0;
    assert_eq!(rep_rsb_cutoff, 0.0, "RSB beyond opt + 3*falloff must apply 0 HP");
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(rsb_id), PyRep::String("shipModuleRemoteShieldBooster".into())], &kw, None).await.unwrap();

    tb_snapshot.position = ship_ball.position + DVec3::new(15_000.0, 0.0, 0.0);
    sys.spawn_ball_with_combat(tb_snapshot.clone(), target_cs.clone()).await.unwrap();
    rpc_lock_target(&ctx, &sess, char_id, ship_id, target_ship_id).await;
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[
        PyRep::Integer(rar_id), PyRep::String("shipModuleRemoteArmorRepairer".into()), PyRep::Integer(target_ship_id), PyRep::Integer(0),
    ], &kw, None).await.expect("Activate RAR at d > opt + 3*falloff");
    for _ in 0..4 { let _ = sys.tick().await; }
    let rep_rar_cutoff = sys.get_combat_state(target_ship_id).await.unwrap().unwrap().armor_hp - 500.0;
    assert_eq!(rep_rar_cutoff, 0.0, "RAR beyond opt + 3*falloff must apply 0 HP");
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(rar_id), PyRep::String("shipModuleRemoteArmorRepairer".into())], &kw, None).await.unwrap();

    // Reset ship_a state for self active booster tests
    let mut cs_active = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    cs_active.capacitor_recharge_time_ms = 0.0;
    cs_active.shield_recharge_time_ms = 0.0;
    cs_active.max_capacitor = 5_000.0;
    cs_active.capacitor = 1_000.0;
    cs_active.max_shield_hp = 10_000.0;
    cs_active.shield_hp = 5_000.0;
    cs_active.max_armor_hp = 10_000.0;
    cs_active.armor_hp = 5_000.0;
    sys.set_combat_state(ship_id, cs_active).await.unwrap();

    // 11. Active Shield Boost at Cycle Start (`t = 0`) & Cooldown Rejection (`feat:combat.active-shield-boost-cycle-start`)
    let cap_before_sb = sys.get_combat_state(ship_id).await.unwrap().unwrap().capacitor;
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[
        PyRep::Integer(sb_id), PyRep::String("shieldBoosting".into()), PyRep::None, PyRep::Integer(1),
    ], &kw, None).await.expect("Activate Small Shield Booster I (repeat=1)");
    let cs_after_sb_t0 = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    assert_eq!(cs_after_sb_t0.shield_hp, 5_026.0, "Active Shield Booster must boost shield immediately at t = 0 by shieldBonus (26 HP)");
    assert!((cap_before_sb - cs_after_sb_t0.capacitor - 20.0).abs() < 1e-3, "Active Shield Booster must deduct 20 GJ cap at cycle start t = 0");
    // Re-activating while active with repeat=1 or repeat=0 must be rejected (`R-SB-CYCLESTART-SPAM`)
    let sb_re_err = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[
        PyRep::Integer(sb_id), PyRep::String("shieldBoosting".into()), PyRep::None, PyRep::Integer(1),
    ], &kw, None).await.expect_err("Activating already cycling Shield Booster with repeat=1 must fail");
    assert_eq!(sb_re_err.code, "ModuleReactivationDelay");
    // Deactivating mid-cycle must preserve the remainder of the current cycle cooldown so Activate->Deactivate->Activate cannot spam boosts (`R-SB-CYCLESTART-SPAM`)
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(sb_id), PyRep::String("shieldBoosting".into())], &kw, None).await.unwrap();
    let sb_deact_err = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[
        PyRep::Integer(sb_id), PyRep::String("shieldBoosting".into()), PyRep::None, PyRep::Integer(1),
    ], &kw, None).await.expect_err("Activate -> Deactivate -> Activate within one cycle must fail");
    assert_eq!(sb_deact_err.code, "ModuleReactivationDelay");
    assert_eq!(sys.get_combat_state(ship_id).await.unwrap().unwrap().shield_hp, 5_026.0, "Rejected re-activations must not grant extra shield HP");
    for _ in 0..3 { let _ = sys.tick().await; }
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[
        PyRep::Integer(sb_id), PyRep::String("shieldBoosting".into()), PyRep::None, PyRep::Integer(0),
    ], &kw, None).await.expect("Activate Small Shield Booster I after cycle cooldown expires");
    assert_eq!(sys.get_combat_state(ship_id).await.unwrap().unwrap().shield_hp, 5_052.0);
    for _ in 0..3 { let _ = sys.tick().await; }

    // 12. Active Armor Repair: 0 HP at t=0, mid-cycle offline cancels repair, completed cycle repairs HP (`feat:combat.active-armor-repair-cycle-end`)
    let cap_before_ar = sys.get_combat_state(ship_id).await.unwrap().unwrap().capacitor;
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[
        PyRep::Integer(ar_id), PyRep::String("armorRepair".into()), PyRep::None, PyRep::Integer(0),
    ], &kw, None).await.expect("Activate Small Armor Repairer I");
    let cs_after_ar_t0 = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    assert_eq!(cs_after_ar_t0.armor_hp, 5_000.0, "Armor Repairer must NOT heal at t = 0");
    assert!((cap_before_ar - cs_after_ar_t0.capacitor - 40.0).abs() < 1e-3, "Armor Repairer must deduct 40 GJ cap at cycle start t = 0");
    sys.tick().await.unwrap();
    assert_eq!(sys.get_combat_state(ship_id).await.unwrap().unwrap().armor_hp, 5_000.0);
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "TakeModuleOffline", &[
        PyRep::Integer(ship_id), PyRep::Integer(ar_id),
    ], &kw, None).await.expect("Take AR offline mid-cycle");
    for _ in 0..8 { let _ = sys.tick().await; }
    assert_eq!(sys.get_combat_state(ship_id).await.unwrap().unwrap().armor_hp, 5_000.0, "Module taken offline mid-cycle must restore 0 HP");

    // Ship destruction cancelling pending armor repair:
    let destruct_ship_id = 100_921_060i64;
    let destruct_ar_id = 100_921_061i64;
    seed_item(&ctx, destruct_ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1);
    seed_item(&ctx, destruct_ar_id, 523, char_id, destruct_ship_id, InventoryFlag::LoSlot0, 1);
    ctx.db.with_mut_snapshot(|snap| { snap.module_online_states.insert(destruct_ar_id, true); });
    let mut d_cs = CombatState::default();
    d_cs.armor_hp = 100.0;
    d_cs.max_armor_hp = 500.0;
    d_cs.capacitor = 500.0;
    sys.spawn_ball_with_combat(Ball {
        id: destruct_ship_id, type_id: 587, position: ship_ball.position + DVec3::new(2_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 3.0, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }, d_cs).await.unwrap();
    let _ = sys.add_module_cycle(eve_server::system_actor::ActiveModuleCycle {
        ship_id: destruct_ship_id, module_id: destruct_ar_id, module_type_id: 523, duration_s: 6.0,
        elapsed_s: 0.0, repeat: 0, armor_repair_amount: 69.0, ..Default::default()
    }).await;
    sys.tick().await.unwrap();
    assert_eq!(sys.get_combat_state(destruct_ship_id).await.unwrap().unwrap().armor_hp, 100.0, "Armor is not repaired mid-cycle");
    assert!(sys.has_active_modules_or_cloak_sync(destruct_ship_id), "Cycle is active before destruction");
    sys.remove_ball(destruct_ship_id).await.unwrap();
    for _ in 0..8 { let _ = sys.tick().await; }
    assert!(sys.get_ball_snapshot(destruct_ship_id).await.unwrap().is_none(), "Destroyed ship must remain removed from space");
    assert!(!sys.has_active_modules_or_cloak_sync(destruct_ship_id), "Cycle must be cancelled when ship is destroyed before cycle end");
    assert!(sys.get_combat_state(destruct_ship_id).await.unwrap().is_none(), "Destroyed ship combat state cleared without repair");

    sys.set_capacitor(ship_id, 5_000.0).await.unwrap();
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "SetModuleOnline", &[
        PyRep::Integer(ship_id), PyRep::Integer(ar_id),
    ], &kw, None).await.expect("Set AR back online");
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[
        PyRep::Integer(ar_id), PyRep::String("armorRepair".into()), PyRep::None, PyRep::Integer(0),
    ], &kw, None).await.expect("Activate Small Armor Repairer I full cycle");
    for _ in 0..8 { let _ = sys.tick().await; }
    assert!(sys.get_combat_state(ship_id).await.unwrap().unwrap().armor_hp > 5_000.0, "Armor Repairer must heal at completed cycle end");

    // 14. Ancillary Armor Repairer Paste (`feat:combat.ancillary-armor-repairer-paste`)
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&paste_id).map(|i| i.quantity)).unwrap_or(0), 2);
    let armor_before_aar1 = sys.get_combat_state(ship_id).await.unwrap().unwrap().armor_hp;
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[
        PyRep::Integer(aar_id), PyRep::String("fueledArmorRepair".into()), PyRep::None, PyRep::Integer(0),
    ], &kw, None).await.expect("Activate fueled AAR cycle 1");
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&paste_id).map(|i| i.quantity)).unwrap_or(0), 1, "Paste stack decremented by 1");
    for _ in 0..8 { let _ = sys.tick().await; }
    let armor_after_aar1 = sys.get_combat_state(ship_id).await.unwrap().unwrap().armor_hp;
    let fueled_rep = armor_after_aar1 - armor_before_aar1;

    // Cycle 2: paste decrements from 1 to 0 (drained naturally to zero without UnloadAmmo):
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[
        PyRep::Integer(aar_id), PyRep::String("fueledArmorRepair".into()), PyRep::None, PyRep::Integer(0),
    ], &kw, None).await.expect("Activate fueled AAR cycle 2");
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&paste_id).map(|i| i.quantity)).unwrap_or(0), 0, "Paste stack drained to zero naturally");
    for _ in 0..8 { let _ = sys.tick().await; }
    let armor_after_aar2 = sys.get_combat_state(ship_id).await.unwrap().unwrap().armor_hp;

    // Cycle 3: with paste depleted to zero, cycle runs unfueled naturally:
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[
        PyRep::Integer(aar_id), PyRep::String("fueledArmorRepair".into()), PyRep::None, PyRep::Integer(0),
    ], &kw, None).await.expect("Activate unfueled AAR after paste depletion");
    for _ in 0..8 { let _ = sys.tick().await; }
    let armor_after_aar3 = sys.get_combat_state(ship_id).await.unwrap().unwrap().armor_hp;
    let unfueled_rep = armor_after_aar3 - armor_after_aar2;
    assert!((fueled_rep - unfueled_rep * 3.0).abs() < 1e-3 && unfueled_rep > 0.0, "Fueled AAR must repair 3x unfueled base (got fueled={fueled_rep}, unfueled={unfueled_rep})");

    let aar_reload = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "LoadAmmo", &[
        PyRep::Integer(ship_id), PyRep::List(vec![PyRep::Integer(aar_id)]), PyRep::List(vec![PyRep::Integer(cargo_paste_id)]), PyRep::Integer(ship_id),
    ], &kw, None).await.unwrap();
    assert_eq!(aar_reload.dict_get("reloadTimeMs").and_then(|v| v.as_f64()), Some(60_000.0));
    let aar_err = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[
        PyRep::Integer(aar_id), PyRep::String("fueledArmorRepair".into()), PyRep::None, PyRep::Integer(0),
    ], &kw, None).await.expect_err("Activate AAR during 60s reload must fail");
    assert_eq!(aar_err.code, "ModuleIsReloading");
    ctx.advance_secs(61);
    let armor_before_aar4 = sys.get_combat_state(ship_id).await.unwrap().unwrap().armor_hp;
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[
        PyRep::Integer(aar_id), PyRep::String("fueledArmorRepair".into()), PyRep::None, PyRep::Integer(0),
    ], &kw, None).await.expect("Activate AAR after 60s reload finished");
    for _ in 0..8 { let _ = sys.tick().await; }
    let armor_after_aar4 = sys.get_combat_state(ship_id).await.unwrap().unwrap().armor_hp;
    let reloaded_rep = armor_after_aar4 - armor_before_aar4;
    assert!((reloaded_rep - fueled_rep).abs() < 1e-3, "AAR after reload must cycle boosted (3x unfueled)");

    // 15. Ancillary Shield Booster Charges (`feat:combat.ancillary-shield-booster-charges`)
    let cs_before_asb1 = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[
        PyRep::Integer(asb_id), PyRep::String("fueledShieldBoosting".into()), PyRep::None, PyRep::Integer(1),
    ], &kw, None).await.expect("Activate fueled ASB");
    let cs_after_asb1 = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!((cs_after_asb1.capacitor - cs_before_asb1.capacitor).abs() < 1e-4 && cs_after_asb1.shield_hp > cs_before_asb1.shield_hp);
    let asb_re_err = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[
        PyRep::Integer(asb_id), PyRep::String("fueledShieldBoosting".into()), PyRep::None, PyRep::Integer(1),
    ], &kw, None).await.expect_err("Re-activating ASB within cycle must fail");
    assert_eq!(asb_re_err.code, "ModuleReactivationDelay");
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(asb_id), PyRep::String("fueledShieldBoosting".into())], &kw, None).await.unwrap();
    let asb_deact_err = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(asb_id), PyRep::String("fueledShieldBoosting".into()), PyRep::None, PyRep::Integer(1)], &kw, None).await.expect_err("ASB Activate->Deactivate->Activate within cycle must fail");
    assert_eq!(asb_deact_err.code, "ModuleReactivationDelay");
    for _ in 0..8 { let _ = sys.tick().await; }

    let cs_before_asb2 = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[
        PyRep::Integer(asb_id), PyRep::String("fueledShieldBoosting".into()), PyRep::None, PyRep::Integer(0),
    ], &kw, None).await.expect("Activate unfueled ASB");
    let cs_after_asb2 = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(cs_after_asb2.capacitor < cs_before_asb2.capacitor && cs_after_asb2.shield_hp > cs_before_asb2.shield_hp);
    for _ in 0..8 { let _ = sys.tick().await; }
    let asb_reload = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "LoadAmmo", &[
        PyRep::Integer(ship_id), PyRep::List(vec![PyRep::Integer(asb_id)]), PyRep::List(vec![PyRep::Integer(cargo_cap_id)]), PyRep::Integer(ship_id),
    ], &kw, None).await.unwrap();
    assert_eq!(asb_reload.dict_get("reloadTimeMs").and_then(|v| v.as_f64()), Some(60_000.0));
    let asb_err = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[
        PyRep::Integer(asb_id), PyRep::String("fueledShieldBoosting".into()), PyRep::None, PyRep::Integer(0),
    ], &kw, None).await.expect_err("Activate ASB during 60s reload must fail");
    assert_eq!(asb_err.code, "ModuleIsReloading");
    ctx.advance_secs(61);
    let cs_before_asb3 = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[
        PyRep::Integer(asb_id), PyRep::String("fueledShieldBoosting".into()), PyRep::None, PyRep::Integer(0),
    ], &kw, None).await.expect("Activate ASB after 60s reload finished");
    let cs_after_asb3 = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!((cs_after_asb3.capacitor - cs_before_asb3.capacitor).abs() < 1e-4 && cs_after_asb3.shield_hp > cs_before_asb3.shield_hp, "ASB after reload must cycle boosted with 0 capacitor cost");
    for _ in 0..8 { let _ = sys.tick().await; }

    // 16. Smartbomb AoE Damage (`feat:combat.smartbomb-aoe-damage`)
    let sb_mod_state = DogmaItemState::from_sde(char_id, 3993, &ctx.sde);
    assert_eq!(sb_mod_state.get_attribute(99, &ctx.sde), 5_000.0, "empRadius must be 5000m");
    assert_eq!(sb_mod_state.get_attribute(114, &ctx.sde), 250.0, "EM damage attribute 114");
    assert_eq!(sb_mod_state.get_attribute(116, &ctx.sde), 0.0, "Explosive damage attribute 116");
    assert_eq!(sb_mod_state.get_attribute(117, &ctx.sde), 0.0, "Kinetic damage attribute 117");
    assert_eq!(sb_mod_state.get_attribute(118, &ctx.sde), 0.0, "Thermal damage attribute 118");

    let (_, ship_ball) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let neutral_char_id = 90_000_002i64;
    let neutral_ship_id = 100_921_050i64;
    let cloaked_ship_id = 100_921_051i64;
    let drone_ball_id = 100_921_052i64;
    let probe_ball_id = 100_921_053i64;
    let missile_ball_id = 100_921_054i64;
    let outside_ship_id = 100_921_055i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&neutral_char_id) {
            ch.active_ship_id = neutral_ship_id;
            ch.solar_system_id = sys_id;
        }
    });
    seed_item(&ctx, neutral_ship_id, 587, neutral_char_id, sys_id as i64, InventoryFlag::None, 1);
    seed_item(&ctx, cloaked_ship_id, 587, neutral_char_id, sys_id as i64, InventoryFlag::None, 1);
    seed_item(&ctx, drone_ball_id, 2454, neutral_char_id, sys_id as i64, InventoryFlag::None, 1);
    seed_item(&ctx, probe_ball_id, 30013, neutral_char_id, sys_id as i64, InventoryFlag::None, 1);
    seed_item(&ctx, outside_ship_id, 587, neutral_char_id, sys_id as i64, InventoryFlag::None, 1);

    // Neutral ship is outside smartbomb blast radius (10,000m > 5,000m emp_range):
    sys.spawn_ball_with_combat(Ball {
        id: neutral_ship_id, type_id: 587, position: ship_ball.position + DVec3::new(10_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 3.0, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }, CombatState::default()).await.unwrap();
    let mut cloaked_sb_cs = CombatState::default();
    cloaked_sb_cs.is_cloaked = true;
    sys.spawn_ball_with_combat(Ball {
        id: cloaked_ship_id, type_id: 587, position: ship_ball.position + DVec3::new(1_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 3.0, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }, cloaked_sb_cs).await.unwrap();
    // Entity strictly outside emp_range (6,000m > 5,000m):
    sys.spawn_ball_with_combat(Ball {
        id: outside_ship_id, type_id: 587, position: ship_ball.position + DVec3::new(6_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 3.0, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }, CombatState::default()).await.unwrap();
    // In-flight missile within smartbomb range (1,500m <= 5,000m):
    sys.spawn_ball(Ball {
        id: missile_ball_id, type_id: 204, position: ship_ball.position + DVec3::new(1_500.0, 0.0, 0.0),
        velocity: DVec3::new(100.0, 0.0, 0.0), max_velocity: 1000.0, inertia: 1.0, mass: 100.0, radius: 1.0, speed_fraction: 1.0, mode: MovementMode::Follow { target_id: ship_id, range: 0.0 },
    }).await.unwrap();
    with_mut_combat_runtime(&ctx, |rt| {
        rt.in_flight_missiles.insert(missile_ball_id, InFlightMissileState {
            missile_ball_id, solar_system_id: sys_id, attacker_char_id: neutral_char_id, attacker_ship_id: neutral_ship_id,
            launcher_module_id: 0, launcher_type_id: 0, missile_type_id: 204, target_ball_id: ship_id,
            max_velocity: 1000.0, flight_time_s: 10.0, remaining_flight_s: 10.0, hp: 30.0, max_hp: 30.0,
            base_damage: eve_destiny::DamageVector::default(), explosion_radius: 50.0, explosion_velocity: 100.0,
            drf: 1.0, is_fof: false, is_breacher_pod: false, breacher_dot_duration_s: 0.0,
            breacher_flat_cap: 0.0, breacher_pct_cap: 0.0, destroyed: false, expired: false, impacted: false, applied_damage: 0.0,
        });
    });
    let mut low_hp_cs = CombatState::default();
    low_hp_cs.shield_hp = 5.0;
    low_hp_cs.armor_hp = 5.0;
    low_hp_cs.structure_hp = 5.0;
    sys.spawn_ball_with_combat(Ball {
        id: drone_ball_id, type_id: 2454, position: ship_ball.position + DVec3::new(800.0, 0.0, 0.0),
        velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 1.0, mass: 5_000.0, radius: 5.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }, low_hp_cs.clone()).await.unwrap();
    sys.spawn_ball_with_combat(Ball {
        id: probe_ball_id, type_id: 30013, position: ship_ball.position + DVec3::new(900.0, 0.0, 0.0),
        velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 1.0, mass: 1_000.0, radius: 2.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }, low_hp_cs).await.unwrap();
    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.insert(drone_ball_id, DroneSpaceState {
            ball_id: drone_ball_id, drone_item_id: drone_ball_id, type_id: 2454, group_id: 100,
            owner_char_id: neutral_char_id, original_owner_char_id: neutral_char_id, parent_ship_id: neutral_ship_id,
            solar_system_id: sys_id, target_id: None, activity_state: 2, assist_char_id: None, guard_char_id: None,
            is_abandoned: false, is_sentry: false, bandwidth_used: 5.0, max_velocity: 1000.0, optimal_range: 5000.0,
            damage_vector: eve_destiny::DamageVector::default(), shield_repair_amount: 0.0, armor_repair_amount: 0.0,
            hull_repair_amount: 0.0, web_speed_factor: 0.0, neut_amount: 0.0, ecm_strength: 0.0, tp_sig_bonus_pct: 0.0,
        });
    });

    ctx.dispatch_rpc(&mut sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(2)], &kw, None).await.unwrap();
    let sb_green_res = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[
        PyRep::Integer(smartbomb_id), PyRep::String("empWave".into()), PyRep::None, PyRep::Integer(0),
    ], &kw, None).await;
    assert!(sb_green_res.is_err(), "Smartbomb in High-sec with neutral entities must be rejected on Safety Green");

    ctx.dispatch_rpc(&mut sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(0)], &kw, None).await.unwrap();
    let neutral_hp_before = sys.get_combat_state(neutral_ship_id).await.unwrap().unwrap().shield_hp;
    let cloaked_hp_before = sys.get_combat_state(cloaked_ship_id).await.unwrap().unwrap().shield_hp;
    let outside_hp_before = sys.get_combat_state(outside_ship_id).await.unwrap().unwrap().shield_hp;
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[
        PyRep::Integer(smartbomb_id), PyRep::String("empWave".into()), PyRep::None, PyRep::Integer(0),
    ], &kw, None).await.expect("Smartbomb Activate on Safety Red");
    let sb_spam_err = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[
        PyRep::Integer(smartbomb_id), PyRep::String("empWave".into()), PyRep::None, PyRep::Integer(0),
    ], &kw, None).await.expect_err("Smartbomb re-activation within cycle must fail (`R-ACTIVATE-ROUTING-BYPASS`)");
    assert_eq!(sb_spam_err.code, "ModuleReactivationDelay");
    assert_eq!(sys.get_combat_state(neutral_ship_id).await.unwrap().unwrap().shield_hp, neutral_hp_before, "Neutral ship outside blast radius (10,000m > 5,000m) must remain unharmed");
    assert_eq!(sys.get_combat_state(cloaked_ship_id).await.unwrap().unwrap().shield_hp, cloaked_hp_before);
    assert_eq!(sys.get_combat_state(outside_ship_id).await.unwrap().unwrap().shield_hp, outside_hp_before, "Entity outside smartbomb range (6000m > 5000m) must be left unharmed");
    assert!(sys.get_ball_snapshot(drone_ball_id).await.unwrap().is_none() && sys.get_ball_snapshot(probe_ball_id).await.unwrap().is_none());
    assert!(with_combat_runtime(&ctx, |rt| !rt.active_drones_in_space.contains_key(&drone_ball_id)), "Drone destroyed by Smartbomb AoE must be removed from active space drones");
    assert!(with_combat_runtime(&ctx, |rt| rt.in_flight_missiles.get(&missile_ball_id).is_some_and(|m| m.destroyed)), "In-flight missile within smartbomb range must be destroyed");
    assert!(sys.get_ball_snapshot(missile_ball_id).await.unwrap().is_none(), "Destroyed missile ball must be removed from space");
    let cw_sb = ctx.crimewatch.with_crimewatch(|cw| cw.states.get(&char_id).cloned().unwrap_or_default());
    assert!(cw_sb.criminal_flag_until_unix > 0 && cw_sb.pending_concord_arrival.is_some(), "Smartbomb hitting only neutral drone must trigger Criminal flag and CONCORD");

    // 17. Wreck Salvaging vs Looting Legality (`feat:combat.wreck-salvaging-vs-looting-legality`)
    ctx.crimewatch.with_mut_crimewatch(|cw| { cw.states.remove(&char_id); });
    ctx.dispatch_rpc(&mut sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(2)], &kw, None).await.unwrap();

    let foreign_owner_id = 90_000_002i64;
    let wreck_id = 100_921_080i64;
    let wreck_loot_id = 100_921_081i64;
    seed_item(&ctx, wreck_id, 26561, foreign_owner_id, sys_id as i64, InventoryFlag::None, 1);
    seed_item(&ctx, wreck_loot_id, 34, foreign_owner_id, wreck_id, InventoryFlag::Cargo, 500);
    sys.spawn_ball_with_combat(Ball {
        id: wreck_id, type_id: 26561, position: ship_ball.position + DVec3::new(1_500.0, 0.0, 0.0),
        velocity: DVec3::ZERO, max_velocity: 0.0, inertia: 1.0, mass: 10_000.0, radius: 25.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }, CombatState::default()).await.unwrap();

    // Salvager & Tractor Beam without target lock must fail with TargetNotLocked (`R-ACTIVATE-ROUTING-BYPASS`)
    let salv_no_lock = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[
        PyRep::Integer(salvager_id), PyRep::String("salvaging".into()), PyRep::Integer(wreck_id), PyRep::Integer(0),
    ], &kw, None).await.expect_err("Salvager without target lock must fail");
    assert_eq!(salv_no_lock.code, "TargetNotLocked");
    let trac_no_lock = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[
        PyRep::Integer(tractor_id), PyRep::String("tractorBeamCan".into()), PyRep::Integer(wreck_id), PyRep::Integer(0),
    ], &kw, None).await.expect_err("Tractor Beam without target lock must fail");
    assert_eq!(trac_no_lock.code, "TargetNotLocked");

    rpc_lock_target(&ctx, &sess, char_id, ship_id, wreck_id).await;
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[
        PyRep::Integer(salvager_id), PyRep::String("salvaging".into()), PyRep::Integer(wreck_id), PyRep::Integer(0),
    ], &kw, None).await.expect("Salvaging a foreign yellow wreck on Safety Green must succeed");
    assert!(ctx.db.with_snapshot(|s| s.items.contains_key(&wreck_id)), "Wreck must remain intact at t = 0 until 10s Salvager cycle finishes");
    let salv_spam_err = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[
        PyRep::Integer(salvager_id), PyRep::String("salvaging".into()), PyRep::Integer(wreck_id), PyRep::Integer(0),
    ], &kw, None).await.expect_err("Re-activating Salvager within 10s cycle must fail");
    assert_eq!(salv_spam_err.code, "ModuleReactivationDelay");
    for _ in 0..10 { let _ = sys.tick().await; }
    let cw_after_salvage = ctx.crimewatch.with_crimewatch(|cw| cw.states.get(&char_id).cloned().unwrap_or_default());
    assert_eq!(cw_after_salvage.suspect_flag_until_unix, 0, "Salvaging a yellow wreck must NOT trigger Suspect flag");
    assert_eq!(cw_after_salvage.weapon_timer_until_unix, 0, "Salvaging a yellow wreck must NOT trigger aggression timer");

    // Salvage Drone (`entity.CmdSalvage`) on a foreign yellow wreck on Safety Green:
    let wreck3_id = 100_921_084i64;
    seed_item(&ctx, wreck3_id, 26561, foreign_owner_id, sys_id as i64, InventoryFlag::None, 1);
    sys.spawn_ball_with_combat(Ball {
        id: wreck3_id, type_id: 26561, position: ship_ball.position + DVec3::new(1_400.0, 0.0, 0.0),
        velocity: DVec3::ZERO, max_velocity: 0.0, inertia: 1.0, mass: 10_000.0, radius: 25.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }, CombatState::default()).await.unwrap();
    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.insert(100_921_085, DroneSpaceState {
            ball_id: 100_921_085, drone_item_id: 100_921_085, type_id: 32787, group_id: 1159,
            owner_char_id: char_id, original_owner_char_id: char_id, parent_ship_id: ship_id,
            solar_system_id: sys_id, target_id: None, activity_state: 1, assist_char_id: None, guard_char_id: None,
            is_abandoned: false, is_sentry: false, bandwidth_used: 5.0, max_velocity: 1000.0, optimal_range: 5000.0,
            damage_vector: eve_destiny::DamageVector::default(), shield_repair_amount: 0.0, armor_repair_amount: 0.0,
            hull_repair_amount: 0.0, web_speed_factor: 0.0, neut_amount: 0.0, ecm_strength: 0.0, tp_sig_bonus_pct: 0.0,
        });
    });
    ctx.dispatch_rpc(&mut sess, "entity", None, "CmdSalvage", &[PyRep::Integer(wreck3_id)], &kw, None)
        .await.expect("Salvage Drone CmdSalvage on foreign yellow wreck on Safety Green must succeed");
    let cw_after_drone_salvage = ctx.crimewatch.with_crimewatch(|cw| cw.states.get(&char_id).cloned().unwrap_or_default());
    assert_eq!(cw_after_drone_salvage.suspect_flag_until_unix, 0, "Salvage Drone on yellow wreck must NOT trigger Suspect flag");
    assert_eq!(cw_after_drone_salvage.criminal_flag_until_unix, 0, "Salvage Drone on yellow wreck must NOT trigger Criminal flag");

    let wreck2_id = 100_921_082i64;
    let wreck2_loot_id = 100_921_083i64;
    seed_item(&ctx, wreck2_id, 26561, foreign_owner_id, sys_id as i64, InventoryFlag::None, 1);
    seed_item(&ctx, wreck2_loot_id, 34, foreign_owner_id, wreck2_id, InventoryFlag::Cargo, 500);
    sys.spawn_ball_with_combat(Ball {
        id: wreck2_id, type_id: 26561, position: ship_ball.position + DVec3::new(1_200.0, 0.0, 0.0),
        velocity: DVec3::ZERO, max_velocity: 0.0, inertia: 1.0, mass: 10_000.0, radius: 25.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }, CombatState::default()).await.unwrap();

    let loot_green_err = ctx.dispatch_rpc(&mut sess, "invbroker", None, "LootAll", &[PyRep::Integer(wreck2_id)], &kw, None).await;
    assert!(loot_green_err.is_err(), "Looting a yellow wreck on Safety Green must be rejected");

    ctx.dispatch_rpc(&mut sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(1)], &kw, None).await.unwrap();
    ctx.dispatch_rpc(&mut sess, "invbroker", None, "LootAll", &[PyRep::Integer(wreck2_id)], &kw, None).await.unwrap();
    let cw_after_loot = ctx.crimewatch.with_crimewatch(|cw| cw.states.get(&char_id).cloned().unwrap_or_default());
    assert!(cw_after_loot.suspect_flag_until_unix >= ctx.now_unix_secs() + 895, "Looting a yellow wreck must flag Suspect for 15 minutes");
}

