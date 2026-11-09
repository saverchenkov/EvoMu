//! Stasis grappler, emergency hull energizer, mutadaptive remote armor repairer and entropic
//! disintegrator spool mechanics driven through `dogmaIM` with real system ticks.

use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_destiny::{Ball, MovementMode, SlimItemMeta, TargetLock};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

fn new_ctx() -> ServerContext {
    let sde = std::sync::Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path())
            .expect("open evomu_sde.sqlite"),
    );
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    ServerContext::with_sde_and_repository(sde, db)
        .with_seeded_rng(eve_server::rng::TEST_MODE_DEFAULT_SEED)
}

async fn call_service(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: Vec<PyRep>,
    kw: HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    eve_server::services::combat_crimewatch::dispatch_combat_rpc(ctx, sess, svc, method, &args, &kw)
        .await
}

fn make_session(
    sid: u64,
    user_id: u32,
    char_id: i64,
    corp_id: i64,
    station_id: Option<i64>,
    sys_id: u32,
    ship_id: Option<i64>,
) -> SessionState {
    let mut sess = SessionState::new(sid);
    sess.user_id = Some(user_id as u64);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.station_id = station_id;
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = ship_id;
    sess
}

fn seed_item(ctx: &ServerContext, item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32, singleton: bool) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(item_id, InventoryItemRecord {
            item_id, type_id, owner_id, location_id, flag, quantity, singleton,
            version: 1, dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None,
            insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
        });
    });
}

async fn spawn_ship_ball(ctx: &ServerContext, sys_id: u32, ship_id: i64, type_id: u32, owner_id: i64, x: f64) {
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let mut cs = ctx.build_type_combat_state(type_id);
    cs.armor_hp = cs.max_armor_hp * 0.2;
    let b = Ball {
        id: ship_id, type_id, position: DVec3::new(x, 0.0, 0.0), velocity: DVec3::ZERO,
        max_velocity: 150.0, inertia: 1.0, mass: 10_000_000.0, radius: 100.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    };
    let meta = SlimItemMeta {
        group_id: 26, category_id: 6, owner_id, char_id: Some(owner_id),
        corp_id: 1000009, alliance_id: -1, security_status: 0.0, is_cloaked: false, ..Default::default()
    };
    let _ = sys_h.spawn_ball_with_meta(b, cs, meta).await;
}

fn grant_type_skills(ctx: &ServerContext, char_id: i64, type_id: u32) {
    let Some(t) = ctx.sde.types.get(&type_id).cloned() else { return; };
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(char_id).or_default();
        for &(sa, la) in &[(182_u16, 277_u16), (183, 278), (184, 279), (1285, 1286), (1289, 1287), (1290, 1288)] {
            if let Some(&sid_f) = t.default_attributes.get(&sa) {
                let sid = sid_f.round() as u32;
                let lvl = t.default_attributes.get(&la).copied().unwrap_or(1.0).round() as u8;
                if sid > 0 {
                    sk.insert(sid, TrainedSkillRecord { type_id: sid, level: lvl.max(5), skill_points: 256_000.0 });
                }
            }
        }
    });
}

#[tokio::test]
async fn test_modules_grappler_ehe_mutadaptive_repairer_and_entropic_disintegrator() {
    // parity: feat:modules.entropic-disintegrator-damage-spool
    eve_testkit::attest(&[
        "feat:modules.entropic-disintegrator-damage-spool",
    ]);

    let ctx = new_ctx();
    let c1 = 90_000_001;
    let c2 = 90_000_002;
    let station_id = 60_003_760;
    let sys_id = 30_004_759;
    grant_type_skills(&ctx, c1, 626);
    grant_type_skills(&ctx, c1, 638);
    grant_type_skills(&ctx, c1, 24483);
    grant_type_skills(&ctx, c1, 47272);
    ctx.db.with_mut_snapshot(|snap| { if let Some(ch) = snap.characters.get_mut(&c1) { ch.clone_grade = 2; } });

    // 1. Heavy Stasis Grappler (group 1672): rejected on Cruiser (`626`), accepted on Battleship (`638`), max 1 per ship
    let grappler_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 1672 && t.category_id == 7)
        .map(|t| t.type_id)
        .expect("Heavy Stasis Grappler in SDE");
    let ehe_tid = 40714_u32;
    let vexor_id = 960_001;
    let raven_id = 960_002;
    let grappler_1 = 960_003;
    let grappler_2 = 960_004;
    seed_item(&ctx, vexor_id, 626, c1, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, raven_id, 638, c1, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, grappler_1, grappler_tid, c1, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, grappler_2, grappler_tid, c1, station_id, InventoryFlag::Hangar, 1, true);
    grant_type_skills(&ctx, c1, grappler_tid);

    let mut sess = make_session(1, 1, c1, 1000009, Some(station_id), sys_id, Some(vexor_id));

    let cruiser_err = call_service(&ctx, &mut sess, "invbroker", "Add", vec![PyRep::Integer(grappler_1), PyRep::Integer(vexor_id), PyRep::Integer(InventoryFlag::MedSlot0 as i64)], HashMap::new()).await.expect_err("Cruiser must not fit Heavy Stasis Grappler");
    assert_eq!(cruiser_err.code, "CanNotFitModuleToShip");

    sess.ship_id = Some(raven_id);
    ctx.db.with_mut_snapshot(|snap| { if let Some(ch) = snap.characters.get_mut(&c1) { ch.active_ship_id = raven_id; } });
    let _ = call_service(&ctx, &mut sess, "invbroker", "Add", vec![PyRep::Integer(grappler_1), PyRep::Integer(raven_id), PyRep::Integer(InventoryFlag::MedSlot0 as i64)], HashMap::new()).await.expect("Battleship must fit 1 Heavy Stasis Grappler");
    let max_group_err = call_service(&ctx, &mut sess, "invbroker", "Add", vec![PyRep::Integer(grappler_2), PyRep::Integer(raven_id), PyRep::Integer(InventoryFlag::MedSlot1 as i64)], HashMap::new()).await.expect_err("Max 1 Heavy Stasis Grappler per ship");
    assert_eq!(max_group_err.code, "MaxGroupFitted");

    // Undock Raven to activate Stasis Grappler on target at 1km and falloff
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(grappler_1, true);
        if let Some(ch) = snap.characters.get_mut(&c1) { ch.station_id = None; ch.solar_system_id = sys_id; ch.active_ship_id = raven_id; }
    });
    sess.station_id = None;
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(raven_id);
    spawn_ship_ball(&ctx, sys_id, raven_id, 638, c1, 0.0).await;
    let grappler_target = 960_005;
    seed_item(&ctx, grappler_target, 626, c2, sys_id as i64, InventoryFlag::None, 1, true);
    spawn_ship_ball(&ctx, sys_id, grappler_target, 626, c2, 1_000.0).await;
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h.add_target_lock(raven_id, TargetLock { target_id: grappler_target, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) }).await;
    let base_t_vel = sys_h.get_ball_snapshot(grappler_target).await.unwrap().unwrap().1.max_velocity;

    let _ = call_service(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(grappler_1), PyRep::String("stasisGrappler".into()), PyRep::Integer(grappler_target)], HashMap::new()).await.expect("Grappler activates at 1km");
    let grap_1km_vel = sys_h.get_ball_snapshot(grappler_target).await.unwrap().unwrap().1.max_velocity;
    assert!(grap_1km_vel < base_t_vel * 0.20, "Point blank 1km grappler must reduce speed by 85%");
    let _ = call_service(&ctx, &mut sess, "dogmaIM", "Deactivate", vec![PyRep::Integer(grappler_1)], HashMap::new()).await.expect("Grappler deactivates");

    let _ = sys_h.set_ball_position(grappler_target, DVec3::new(8_000.0, 0.0, 0.0)).await;
    let _ = call_service(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(grappler_1), PyRep::String("stasisGrappler".into()), PyRep::Integer(grappler_target)], HashMap::new()).await.expect("Grappler activates at 8km falloff");
    let grap_8km_vel = sys_h.get_ball_snapshot(grappler_target).await.unwrap().unwrap().1.max_velocity;
    assert!(grap_8km_vel > grap_1km_vel && grap_8km_vel < base_t_vel, "Grappler speed reduction must decay over falloff curve");
    let _ = call_service(&ctx, &mut sess, "dogmaIM", "Deactivate", vec![PyRep::Integer(grappler_1)], HashMap::new()).await.expect("Grappler deactivates");

    // 2. Capital Emergency Hull Energizer (`ehe_tid`): 95% hull resist (0.05 resonance) for 1 cycle, burns out, refuses nanite repair
    let mutadaptive_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 2018 && t.category_id == 7)
        .map(|t| t.type_id)
        .expect("Mutadaptive Remote Armor Repairer in SDE");
    let archon_id = 960_010;
    let ehe_id = 960_011;
    let mutadaptive_id = 960_012;
    let target_ship_a = 960_020;
    let target_ship_b = 960_021;
    seed_item(&ctx, archon_id, 24483, c1, sys_id as i64, InventoryFlag::None, 1, true);
    seed_item(&ctx, ehe_id, ehe_tid, c1, archon_id, InventoryFlag::LoSlot0, 1, true);
    seed_item(&ctx, mutadaptive_id, mutadaptive_tid, c1, archon_id, InventoryFlag::HiSlot0, 1, true);
    seed_item(&ctx, target_ship_a, 638, c2, sys_id as i64, InventoryFlag::None, 1, true);
    seed_item(&ctx, target_ship_b, 638, c2, sys_id as i64, InventoryFlag::None, 1, true);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(ehe_id, true);
        snap.module_online_states.insert(mutadaptive_id, true);
        if let Some(ch) = snap.characters.get_mut(&c1) {
            ch.station_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = archon_id;
        }
    });
    sess.station_id = None;
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(archon_id);

    spawn_ship_ball(&ctx, sys_id, archon_id, 24483, c1, 0.0).await;
    spawn_ship_ball(&ctx, sys_id, target_ship_a, 638, c2, 2_000.0).await;
    spawn_ship_ball(&ctx, sys_id, target_ship_b, 638, c2, 3_000.0).await;
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h.add_target_lock(archon_id, TargetLock { target_id: target_ship_a, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) }).await;

    let _ = call_service(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(ehe_id), PyRep::String("emergencyHullEnergizer".into())], HashMap::new()).await.expect("EHE must activate on capital hull");
    let active_cs = sys_h.get_combat_state(archon_id).await.unwrap().unwrap();
    assert_eq!(active_cs.structure_resonance, [0.05, 0.05, 0.05, 0.05], "EHE must grant 95% hull resistance across EM/Therm/Kin/Exp");
    let snap_res = ctx.db.with_snapshot(|s| s.transient_item_attributes.get(&archon_id).map(|t| (t.get(&109).copied(), t.get(&110).copied())).unwrap());
    assert_eq!(snap_res, (Some(0.05), Some(0.05)));

    let _ = sys_h.step_manual(25.0).await;
    let burned_dmg = ctx.db.with_snapshot(|s| s.items.get(&ehe_id).and_then(|i| i.dynamic_attributes.get(&3).copied()).unwrap_or(0.0));
    assert!(burned_dmg >= 39.9, "EHE must burn out after 1 cycle");
    let nanite_err = call_service(&ctx, &mut sess, "dogmaIM", "RepairModule", vec![PyRep::Integer(archon_id), PyRep::Integer(ehe_id)], HashMap::new()).await.expect_err("Burned-out EHE must refuse Nanite Repair Paste");
    assert_eq!(nanite_err.code, "ModuleBurnedOut");

    // 3. Mutadaptive Remote Armor Repairer (`mutadaptive_id`) spools up per cycle on `target_ship_a` and resets on `target_ship_b`
    let _ = call_service(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(mutadaptive_id), PyRep::String("remoteArmorRepair".into()), PyRep::Integer(target_ship_a)], HashMap::new()).await.expect("Mutadaptive RAR activates on target A");
    let _ = sys_h.step_manual(12.0).await;
    let rep_after_1 = ctx.db.with_snapshot(|s| s.transient_item_attributes.get(&mutadaptive_id).and_then(|t| t.get(&84).copied()).unwrap_or(0.0));
    let _ = sys_h.step_manual(12.0).await;
    let rep_after_2 = ctx.db.with_snapshot(|s| s.transient_item_attributes.get(&mutadaptive_id).and_then(|t| t.get(&84).copied()).unwrap_or(0.0));
    assert!(rep_after_2 > rep_after_1, "Mutadaptive RAR must spool up repair amount across consecutive cycles");

    let _ = sys_h.add_target_lock(archon_id, TargetLock { target_id: target_ship_b, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) }).await;
    let _ = call_service(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(mutadaptive_id), PyRep::String("remoteArmorRepair".into()), PyRep::Integer(target_ship_b)], HashMap::new()).await.expect("Mutadaptive RAR retargets to target B");
    let _ = sys_h.step_manual(12.0).await;
    let rep_after_switch = ctx.db.with_snapshot(|s| s.transient_item_attributes.get(&mutadaptive_id).and_then(|t| t.get(&84).copied()).unwrap_or(0.0));
    assert!((rep_after_switch - rep_after_1).abs() < 1e-4, "Switching target on Mutadaptive RAR must reset spool to cycle 0");
    let _ = call_service(&ctx, &mut sess, "dogmaIM", "Deactivate", vec![PyRep::Integer(mutadaptive_id)], HashMap::new()).await.expect("Deactivate Mutadaptive RAR");
    let rep_after_deact = ctx.db.with_snapshot(|s| s.transient_item_attributes.get(&mutadaptive_id).and_then(|t| t.get(&84).copied()).unwrap_or(rep_after_1));
    assert!((rep_after_deact - rep_after_1).abs() < 1e-4, "Deactivating Mutadaptive RAR must reset transient attr 84 to base");
    let _ = sys_h.remove_target_lock(archon_id, target_ship_b).await;

    // 4. Entropic Disintegrator (`47272` Light Entropic Disintegrator I) spools up per cycle, reaches damageMultiplierBonusMax, resets on target change / deactivation / lock loss
    let dis_mod_id = 960_030;
    let dis_ammo_id = 960_031;
    seed_item(&ctx, dis_mod_id, 47272, c1, archon_id, InventoryFlag::HiSlot1, 1, true);
    seed_item(&ctx, dis_ammo_id, 47885, c1, archon_id, InventoryFlag::HiSlot1, 100, false);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(dis_mod_id, true);
    });

    // Give target ship structure-only HP pool to test continuous structure damage ramp
    let mut cs_a = ctx.build_type_combat_state(638);
    cs_a.shield_hp = 0.0;
    cs_a.max_shield_hp = 0.0;
    cs_a.armor_hp = 0.0;
    cs_a.max_armor_hp = 0.0;
    cs_a.structure_hp = 100_000.0;
    cs_a.max_structure_hp = 100_000.0;
    let _ = sys_h.set_combat_state(target_ship_a, cs_a.clone()).await;
    let _ = sys_h.set_combat_state(target_ship_b, cs_a).await;
    let _ = sys_h.add_target_lock(archon_id, TargetLock { target_id: target_ship_a, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) }).await;
    ctx.set_invention_roll_override(Some(0.50));

    let _ = call_service(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(dis_mod_id), PyRep::String("targetAttack".into()), PyRep::Integer(target_ship_a)], HashMap::new()).await.expect("Initial entropic activation on target A");
    let m0 = eve_server::services::combat_weapons_spool::get_entropic_spool_multiplier(&ctx, archon_id, dis_mod_id);
    assert_eq!(m0, 1.0);

    // Advance spool cycle 1 and record damage
    let hp0 = sys_h.get_combat_state(target_ship_a).await.unwrap().unwrap().structure_hp;
    sys_h.step_manual(4.5).await.unwrap();
    let hp1 = sys_h.get_combat_state(target_ship_a).await.unwrap().unwrap().structure_hp;
    let dmg_cycle1 = hp0 - hp1;
    let m1 = eve_server::services::combat_weapons_spool::get_entropic_spool_multiplier(&ctx, archon_id, dis_mod_id);
    assert!(m1 > 1.0, "Entropic Disintegrator must ramp damage multiplier per cycle");

    // Advance spool cycle 2 and verify continuous damage ramps: cycle 2 damage > cycle 1 damage
    sys_h.step_manual(4.5).await.unwrap();
    let hp2 = sys_h.get_combat_state(target_ship_a).await.unwrap().unwrap().structure_hp;
    let dmg_cycle2 = hp1 - hp2;
    assert!(dmg_cycle2 > dmg_cycle1, "Entropic disintegrator damage ramping must make cycle 2 damage ({dmg_cycle2}) > cycle 1 damage ({dmg_cycle1})");

    // Advance 31 more cycles up to damageMultiplierBonusMax (1.0 + attr 2734 == 3.125)
    for _ in 0..31 {
        sys_h.step_manual(4.5).await.unwrap();
    }
    let m_max = eve_server::services::combat_weapons_spool::get_entropic_spool_multiplier(&ctx, archon_id, dis_mod_id);
    assert!((m_max - 3.125).abs() < 1e-4, "Damage multiplier must reach damageMultiplierBonusMax (3.125)");

    // Target switch: lock target B and switch target: resets multiplier to 1.0
    let _ = sys_h.add_target_lock(archon_id, TargetLock { target_id: target_ship_b, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) }).await;
    let _ = call_service(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(dis_mod_id), PyRep::String("targetAttack".into()), PyRep::Integer(target_ship_b)], HashMap::new()).await.expect("Target switch to target B");
    let m_switch = eve_server::services::combat_weapons_spool::get_entropic_spool_multiplier(&ctx, archon_id, dis_mod_id);
    assert_eq!(m_switch, 1.0, "Switching target must reset Entropic Disintegrator spool to 1.0");

    // Deactivation via dogmaIM.Deactivate: clears spool
    let deact_rep = call_service(&ctx, &mut sess, "dogmaIM", "Deactivate", vec![PyRep::Integer(dis_mod_id)], HashMap::new()).await.expect("dogmaIM.Deactivate must succeed");
    assert_eq!(deact_rep.dict_get("spoolMultiplier").and_then(|v| v.as_f64()), Some(1.0));
    assert_eq!(eve_server::services::combat_weapons_spool::get_entropic_spool_multiplier(&ctx, archon_id, dis_mod_id), 1.0);
    // Remove target B lock so only target A is locked
    let _ = sys_h.remove_target_lock(archon_id, target_ship_b).await;
    // Deactivate stops the module at the end of its running cycle; reactivation is only accepted after it ends.
    sys_h.step_manual(4.5).await.unwrap();
    // Reactivate on target A and remove target lock via dogmaIM.RemoveTarget: module must deactivate and spool stop immediately
    let _ = call_service(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(dis_mod_id), PyRep::String("targetAttack".into()), PyRep::Integer(target_ship_a)], HashMap::new()).await.expect("Reactivating on target A");
    sys_h.step_manual(4.5).await.unwrap();
    assert!(eve_server::services::combat_weapons_spool::get_entropic_spool_multiplier(&ctx, archon_id, dis_mod_id) > 1.0);
    let _ = call_service(&ctx, &mut sess, "dogmaIM", "RemoveTarget", vec![PyRep::Integer(target_ship_a)], HashMap::new()).await.expect("RemoveTarget on sole locked target");
    assert_eq!(eve_server::services::combat_weapons_spool::get_entropic_spool_multiplier(&ctx, archon_id, dis_mod_id), 1.0, "RemoveTarget must stop entropic spool");
    assert!(!ctx.db.with_snapshot(|s| s.module_active_states.contains(&dis_mod_id)), "RemoveTarget must deactivate entropic disintegrator");

    // Reactivate on target A, then clear all locks in space (empty locks list) during step: module deactivates
    let _ = sys_h.add_target_lock(archon_id, TargetLock { target_id: target_ship_a, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) }).await;
    let _ = call_service(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(dis_mod_id), PyRep::String("targetAttack".into()), PyRep::Integer(target_ship_a)], HashMap::new()).await.expect("Reactivating on target A");
    let _ = sys_h.clear_target_locks(archon_id).await;
    sys_h.step_manual(4.5).await.unwrap();
    assert_eq!(eve_server::services::combat_weapons_spool::get_entropic_spool_multiplier(&ctx, archon_id, dis_mod_id), 1.0, "Empty locks list must stop entropic spool on step");
    assert!(!ctx.db.with_snapshot(|s| s.module_active_states.contains(&dis_mod_id)), "Empty locks list must deactivate module on step");

    // a) Target moves beyond maximum range: spool multiplier resets to 1.0 and module deactivates
    let _ = sys_h.add_target_lock(archon_id, TargetLock { target_id: target_ship_a, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) }).await;
    let _ = call_service(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(dis_mod_id), PyRep::String("targetAttack".into()), PyRep::Integer(target_ship_a)], HashMap::new()).await.expect("Activate target A");
    sys_h.step_manual(4.5).await.unwrap();
    assert!(eve_server::services::combat_weapons_spool::get_entropic_spool_multiplier(&ctx, archon_id, dis_mod_id) > 1.0);
    let _ = sys_h.set_ball_position(target_ship_a, DVec3::new(100_000.0, 0.0, 0.0)).await;
    sys_h.step_manual(4.5).await.unwrap();
    assert_eq!(eve_server::services::combat_weapons_spool::get_entropic_spool_multiplier(&ctx, archon_id, dis_mod_id), 1.0, "Out of range must reset spool to 1.0");
    assert!(!ctx.db.with_snapshot(|s| s.module_active_states.contains(&dis_mod_id)), "Out of range must deactivate module");

    // b) Ship capacitor runs dry / insufficient capacitor: spool multiplier resets to 1.0 and module deactivates
    let _ = sys_h.set_ball_position(target_ship_a, DVec3::new(2_000.0, 0.0, 0.0)).await;
    let _ = sys_h.add_target_lock(archon_id, TargetLock { target_id: target_ship_a, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) }).await;
    let _ = call_service(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(dis_mod_id), PyRep::String("targetAttack".into()), PyRep::Integer(target_ship_a)], HashMap::new()).await.expect("Activate in range");
    sys_h.step_manual(4.5).await.unwrap();
    assert!(eve_server::services::combat_weapons_spool::get_entropic_spool_multiplier(&ctx, archon_id, dis_mod_id) > 1.0);
    if let Ok(Some(mut cs)) = sys_h.get_combat_state(archon_id).await {
        cs.capacitor = 0.0;
        cs.max_capacitor = 0.0;
        let _ = sys_h.set_combat_state(archon_id, cs).await;
    }
    sys_h.step_manual(4.5).await.unwrap();
    assert_eq!(eve_server::services::combat_weapons_spool::get_entropic_spool_multiplier(&ctx, archon_id, dis_mod_id), 1.0, "Capacitor dry must reset spool to 1.0");
    assert!(!ctx.db.with_snapshot(|s| s.module_active_states.contains(&dis_mod_id)), "Capacitor dry must deactivate module");
}
