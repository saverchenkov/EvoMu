//! Comprehensive integration test suite closing the 9 `feat:combat.*` EWAR & Capacitor Warfare rows.

use crate::common;

use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, CombatState, DVec3, MovementMode};
use eve_dogma::{
    eval_energy_neutralizer, eval_energy_nosferatu_with_hull, eval_scripted_sensor_dampener,
    eval_scripted_tracking_disruptor, eval_target_painters_sig_radius, is_blood_raider_nosferatu_hull, DogmaItemState,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
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
pub(super) async fn fire_weapon(ctx: &ServerContext, sess: &mut SessionState, mod_id: i64, target_id: i64) -> Result<PyRep, eve_proto::BridgeRpcError> {
    eve_server::services::combat_crimewatch::dispatch_combat_rpc(ctx, sess, "dogmaIM", "Activate", &[PyRep::Integer(mod_id), PyRep::Integer(target_id)], &HashMap::new()).await
}
pub(super) async fn rpc_lock_target(ctx: &ServerContext, base_sess: &SessionState, char_id: i64, ship_id: i64, target_id: i64) {
    let sys = ctx.systems.get_or_spawn(base_sess.solar_system_id.unwrap());
    if sys.get_target_locks(ship_id).await.unwrap().iter().any(|l| l.target_id == target_id && l.locked) { return; }
    let prev = ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&char_id).map(|c| std::mem::replace(&mut c.active_ship_id, ship_id)));
    let mut s = base_sess.clone();
    s.character_id = Some(char_id); s.ship_id = Some(ship_id);
    let res = ctx.dispatch_rpc(&mut s, "dogmaIM", None, "AddTarget", &[PyRep::Integer(target_id)], &HashMap::new(), None).await;
    if let Some(p) = prev { ctx.db.with_mut_snapshot(|snap| { if let Some(c) = snap.characters.get_mut(&char_id) { c.active_ship_id = p; } }); }
    res.expect("dogmaIM.AddTarget");
    for _ in 0..60 {
        if sys.get_target_locks(ship_id).await.unwrap().iter().any(|l| l.target_id == target_id && l.locked) { break; }
        sys.step_manual(1.0).await.unwrap();
    }
    let locks = sys.get_target_locks(ship_id).await.unwrap();
    assert!(locks.iter().any(|l| l.target_id == target_id && l.locked), "Target lock {ship_id}->{target_id} failed: {locks:?}");
}

pub(super) fn grant_all_skills_for_types(ctx: &ServerContext, char_id: i64, type_ids: &[u32]) {
    ctx.db.with_mut_snapshot(|snap| {
        let entry = snap.skills.entry(char_id).or_default();
        for &(sid, lvl) in &[(3426u32, 5u8), (3413, 5), (3418, 5), (3417, 5), (3428, 5), (3429, 5), (3431, 5), (3318, 5), (11207, 5), (3435, 5), (3427, 5), (3433, 5), (3434, 5), (19760, 5)] {
            entry.insert(sid, eve_db::TrainedSkillRecord { type_id: sid, level: lvl, skill_points: 256_000.0 });
        }
        for &tid in type_ids {
            if let Some(t) = ctx.sde.types.get(&tid) {
                for (s_attr, l_attr) in [(eve_sde::attr_ids::REQUIRED_SKILL_1, eve_sde::attr_ids::REQUIRED_SKILL_1_LEVEL), (eve_sde::attr_ids::REQUIRED_SKILL_2, eve_sde::attr_ids::REQUIRED_SKILL_2_LEVEL), (eve_sde::attr_ids::REQUIRED_SKILL_3, eve_sde::attr_ids::REQUIRED_SKILL_3_LEVEL)] {
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
async fn test_combat_ewar_and_capacitor_warfare_closure() {
    // parity: feat:combat.nosferatu-drain-cap, feat:combat.energy-neutralizer-cycle, feat:combat.target-painter-bloom, feat:combat.sensor-dampener-targeting-range, feat:combat.sensor-dampener-scan-resolution, feat:combat.tracking-disruptor-optimal-falloff, feat:combat.tracking-disruptor-tracking-speed, feat:combat.guidance-disruptor-missiles, feat:combat.ecm-burst-radius, rpc:charUnboundMgr.SelectCharacterID
    eve_testkit::attest(&[
        "feat:combat.nosferatu-drain-cap", "feat:combat.energy-neutralizer-cycle", "feat:combat.target-painter-bloom",
        "feat:combat.sensor-dampener-targeting-range", "feat:combat.sensor-dampener-scan-resolution",
        "feat:combat.tracking-disruptor-optimal-falloff", "feat:combat.tracking-disruptor-tracking-speed",
        "feat:combat.guidance-disruptor-missiles", "feat:combat.ecm-burst-radius",
    ]);

    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(9201);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None).await.expect("SelectCharacterID");

    let (char_id, ship_id) = (90_000_001i64, 100_000_001i64);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, i| i.location_id != ship_id);
        if let Some(ship_item) = snap.items.get_mut(&ship_id) { ship_item.type_id = 640; }
    });
    ctx.invalidate_ship_fitting(ship_id);
    grant_all_skills_for_types(&ctx, char_id, &[640, 17920, 17922, 17926, 533, 530, 12709, 1968, 2108, 37543, 580, 487, 499, 183, 210]);

    let (neut_id, nos_id, tp1_id, tp2_id, damp_id, damp2_id, td_id, gd_id, burst_id, turret_id, launcher_id, ammo_id, rocket_id) = (
        100_920_001i64, 100_920_002i64, 100_920_003i64, 100_920_004i64, 100_920_005i64, 100_920_022i64,
        100_920_006i64, 100_920_007i64, 100_920_008i64, 100_920_009i64, 100_920_010i64, 100_920_011i64, 100_920_012i64,
    );
    for (id, tid, flag, qty) in [
        (neut_id, 533, InventoryFlag::HiSlot0, 1), (nos_id, 530, InventoryFlag::HiSlot1, 1),
        (turret_id, 487, InventoryFlag::HiSlot2, 1), (ammo_id, 183, InventoryFlag::HiSlot2, 100),
        (launcher_id, 499, InventoryFlag::HiSlot3, 1), (rocket_id, 210, InventoryFlag::HiSlot3, 100),
        (tp1_id, 12709, InventoryFlag::MedSlot0, 1), (tp2_id, 12709, InventoryFlag::MedSlot1, 1),
        (damp_id, 1968, InventoryFlag::MedSlot2, 1), (td_id, 2108, InventoryFlag::MedSlot3, 1),
        (gd_id, 37543, InventoryFlag::MedSlot4, 1), (burst_id, 580, InventoryFlag::MedSlot5, 1),
        (damp2_id, 1968, InventoryFlag::MedSlot6, 1),
    ] { seed_item(&ctx, id, tid, char_id, ship_id, flag, qty); }
    ctx.db.with_mut_snapshot(|snap| {
        for mid in [neut_id, nos_id, turret_id, launcher_id, tp1_id, tp2_id, damp_id, damp2_id, td_id, gd_id, burst_id] {
            snap.module_online_states.insert(mid, true);
        }
    });
    ctx.invalidate_ship_fitting(ship_id);

    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None).await.expect("Undock");
    ctx.dispatch_rpc(&mut sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(0)], &kw, None).await.unwrap();

    let sys_id = sess.solar_system_id.unwrap();
    let sys = ctx.systems.get_or_spawn(sys_id);
    let (_, mut ship_ball) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    ship_ball.position = DVec3::new(100_000_000.0, 0.0, 0.0);
    ship_ball.velocity = DVec3::ZERO; ship_ball.speed_fraction = 0.0; ship_ball.radius = 40.0; ship_ball.mode = MovementMode::Stop;
    let mut ship_cs_init = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    ship_cs_init.undock_invulnerable_remaining_ticks = 0;
    ship_cs_init.warp_exit_invulnerable_remaining_ticks = 0;
    sys.spawn_ball_with_combat(ship_ball.clone(), ship_cs_init).await.unwrap();

    let victim_char_id = 90_000_002i64;
    let (victim_id, cloaked_id, distant_id, mid_target_id, v_turret_id, v_launcher_id, v_missile_id, v_td_id, v_td_opt_script_id, v_td_trk_script_id) = (
        100_920_050i64, 100_920_051i64, 100_920_052i64, 100_920_056i64, 100_920_053i64, 100_920_054i64, 100_920_055i64,
        100_920_060i64, 100_920_061i64, 100_920_062i64,
    );
    grant_all_skills_for_types(&ctx, victim_char_id, &[587, 2889, 499, 210, 2108, 29005, 29007, 3488]);
    seed_item(&ctx, victim_id, 587, victim_char_id, sys_id as i64, InventoryFlag::None, 1);
    seed_item(&ctx, cloaked_id, 587, victim_char_id, sys_id as i64, InventoryFlag::None, 1);
    seed_item(&ctx, distant_id, 587, victim_char_id, sys_id as i64, InventoryFlag::None, 1);
    seed_item(&ctx, mid_target_id, 587, victim_char_id, sys_id as i64, InventoryFlag::None, 1);
    for (id, tid, flag) in [
        (v_turret_id, 2889, InventoryFlag::HiSlot0), (v_launcher_id, 499, InventoryFlag::HiSlot1),
        (v_td_id, 2108, InventoryFlag::MedSlot0), (v_td_opt_script_id, 29005, InventoryFlag::Cargo),
        (v_td_trk_script_id, 29007, InventoryFlag::Cargo),
    ] { seed_item(&ctx, id, tid, victim_char_id, victim_id, flag, 1); }
    seed_item(&ctx, v_missile_id, 210, victim_char_id, victim_id, InventoryFlag::HiSlot1, 40);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&victim_char_id) {
            ch.active_ship_id = victim_id;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
        if let Some(vi) = snap.items.get_mut(&victim_id) {
            vi.dynamic_attributes.insert(30, 10_000.0);
            vi.dynamic_attributes.insert(48, 10_000.0);
        }
        snap.module_online_states.insert(v_turret_id, true);
        snap.module_online_states.insert(v_launcher_id, true);
        snap.module_online_states.insert(v_td_id, true);
    });
    ctx.invalidate_ship_fitting(victim_id);

    let victim_cs = CombatState {
        max_capacitor: 500.0, capacitor: 200.0, capacitor_recharge_time_ms: 0.0, signature_radius: 35.0, base_signature_radius: Some(35.0),
        shield_hp: 10_000.0, max_shield_hp: 10_000.0, shield_recharge_time_ms: 0.0, armor_hp: 10_000.0, max_armor_hp: 10_000.0,
        structure_hp: 10_000.0, max_structure_hp: 10_000.0, ..CombatState::default()
    };
    let spawn_test_ball = |id, pos, rad| Ball {
        id, type_id: 587, position: ship_ball.position + pos, velocity: DVec3::ZERO,
        max_velocity: 365.0, inertia: 3.2, mass: 1_350_000.0, radius: rad, speed_fraction: 0.0, mode: MovementMode::Stop,
    };
    sys.spawn_ball_with_combat(spawn_test_ball(victim_id, DVec3::new(1_000.0, 0.0, 0.0), 35.0), victim_cs).await.unwrap();

    let mut cloaked_cs = CombatState::default();
    cloaked_cs.is_cloaked = true;
    cloaked_cs.base_signature_radius = Some(40.0);
    sys.spawn_ball_with_combat(spawn_test_ball(cloaked_id, DVec3::new(0.0, 2_000.0, 0.0), 40.0), cloaked_cs).await.unwrap();
    sys.spawn_ball_with_combat(spawn_test_ball(mid_target_id, DVec3::new(21_000.0, 0.0, 0.0), 35.0), CombatState::default()).await.unwrap();
    sys.spawn_ball_with_combat(spawn_test_ball(distant_id, DVec3::new(24_000.0, 0.0, 0.0), 40.0), CombatState::default()).await.unwrap();

    rpc_lock_target(&ctx, &sess, char_id, ship_id, victim_id).await;
    let mut self_cs = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    self_cs.capacitor_recharge_time_ms = 0.0;
    self_cs.max_capacitor = 5000.0;
    self_cs.capacitor = 100.0;
    sys.set_combat_state(ship_id, self_cs).await.unwrap();
    sys.set_capacitor(victim_id, 200.0).await.unwrap();

    // 1 & 2. Energy Neutralizer (`feat:combat.energy-neutralizer-cycle`) & Nosferatu (`feat:combat.nosferatu-drain-cap`)
    let neut_item = DogmaItemState::from_sde(neut_id, 533, &ctx.sde);
    let neut_eval = eval_energy_neutralizer(&neut_item, 1_500.0, 200.0, &ctx.sde);
    assert!((neut_eval.actual_drain - 45.0).abs() < 1e-6 && (neut_eval.target_remaining_cap - 155.0).abs() < 1e-6);
    let neut_clamp = eval_energy_neutralizer(&neut_item, 1_500.0, 20.0, &ctx.sde);
    assert_eq!(neut_clamp.actual_drain, 20.0, "Neutralizer must clamp drain to target's remaining capacitor (0 GJ floor)");
    assert_eq!(neut_clamp.target_remaining_cap, 0.0);

    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[
        PyRep::Integer(neut_id), PyRep::String("energyNeutralizerFalloff".into()), PyRep::Integer(victim_id), PyRep::Integer(0),
    ], &kw, None).await.expect("Activate Small Energy Neutralizer I");
    let self_cap_after_neut = sys.get_combat_state(ship_id).await.unwrap().unwrap().capacitor;
    let victim_cap_after_neut = sys.get_combat_state(victim_id).await.unwrap().unwrap().capacitor;
    assert!(self_cap_after_neut < 100.0, "Neutralizer must deduct activation cap at cycle start");
    assert!((victim_cap_after_neut - 155.0).abs() < 0.5, "Neutralizer must drain 45 GJ at cycle start (200 -> 155)");
    for _ in 0..7 { let _ = sys.tick().await; }

    // Falloff + Cap Battery resistance test (auditor_rows.json:5797-5811)
    let cap_battery_id = 100_920_070i64;
    seed_item(&ctx, cap_battery_id, 3488, victim_char_id, victim_id, InventoryFlag::MedSlot1, 1);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(cap_battery_id, true);
    });
    ctx.invalidate_ship_fitting(victim_id);
    let victim_fit = ctx.get_or_rebuild_ship_fitting(victim_char_id, victim_id).unwrap();
    let r_neut_mult = victim_fit.ship.get_attribute(2045, &ctx.sde);
    assert!((r_neut_mult - 0.75).abs() < 1e-4, "Cap Battery II must provide 25% energy warfare resistance (got {r_neut_mult})");

    let (_, mut v_ball_snap) = sys.get_ball_snapshot(victim_id).await.unwrap().unwrap();
    v_ball_snap.position = ship_ball.position + DVec3::new(7_500.0, 0.0, 0.0);
    sys.spawn_ball_with_combat(v_ball_snap, sys.get_combat_state(victim_id).await.unwrap().unwrap()).await.unwrap();
    rpc_lock_target(&ctx, &sess, char_id, ship_id, victim_id).await;

    let neut_opt = neut_item.get_attribute(eve_dogma::attr_ids::MAX_RANGE, &ctx.sde);
    let neut_fall = eve_dogma::get_module_falloff(&neut_item, &ctx.sde);
    let falloff_att = eve_dogma::compute_falloff_with_cutoff(7_500.0, neut_opt, neut_fall);
    let expected_drain = 45.0 * falloff_att * r_neut_mult;
    sys.set_capacitor(victim_id, 200.0).await.unwrap();
    sys.set_capacitor(ship_id, 100.0).await.unwrap();
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[
        PyRep::Integer(neut_id), PyRep::String("energyNeutralizerFalloff".into()), PyRep::Integer(victim_id), PyRep::Integer(0),
    ], &kw, None).await.expect("Activate Small Energy Neutralizer I at falloff with battery");
    let victim_cap_after_falloff_neut = sys.get_combat_state(victim_id).await.unwrap().unwrap().capacitor;
    let actual_drain = 200.0 - victim_cap_after_falloff_neut;
    assert!((actual_drain - expected_drain).abs() < 0.5, "Neutralizer drain at falloff with battery must equal E0 * falloff * (1 - R_neut): got {actual_drain}, expected {expected_drain}");
    for _ in 0..7 { let _ = sys.tick().await; }

    let (_, mut v_ball_reset) = sys.get_ball_snapshot(victim_id).await.unwrap().unwrap();
    v_ball_reset.position = ship_ball.position + DVec3::new(1_500.0, 0.0, 0.0);
    sys.spawn_ball_with_combat(v_ball_reset, sys.get_combat_state(victim_id).await.unwrap().unwrap()).await.unwrap();
    rpc_lock_target(&ctx, &sess, char_id, ship_id, victim_id).await;
    ctx.db.with_mut_snapshot(|snap| { snap.items.remove(&cap_battery_id); });
    ctx.invalidate_ship_fitting(victim_id);
    // Small Energy Nosferatu I (530):
    // 1) Target cap higher in GJ (1200) but lower in % (40% < 80%): proves absolute GJ rule runs end-to-end:
    let mut ship_cs_nos = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    ship_cs_nos.capacitor_recharge_time_ms = 0.0;
    ship_cs_nos.max_capacitor = 1_000.0;
    ship_cs_nos.capacitor = 800.0; // 80%
    sys.set_combat_state(ship_id, ship_cs_nos).await.unwrap();
    let mut v_cs_nos = sys.get_combat_state(victim_id).await.unwrap().unwrap();
    v_cs_nos.capacitor_recharge_time_ms = 0.0;
    v_cs_nos.max_capacitor = 3_000.0;
    v_cs_nos.capacitor = 1_200.0; // 40%
    sys.set_combat_state(victim_id, v_cs_nos).await.unwrap();

    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[
        PyRep::Integer(nos_id), PyRep::String("energyNosferatuFalloff".into()), PyRep::Integer(victim_id), PyRep::Integer(0),
    ], &kw, None).await.expect("Activate Small Energy Nosferatu I");
    assert_eq!(sys.get_combat_state(victim_id).await.unwrap().unwrap().capacitor, 1_200.0, "Nosferatu must NOT drain target at t = 0");
    for _ in 0..6 { let _ = sys.tick().await; }
    let v_cap_abs = sys.get_combat_state(victim_id).await.unwrap().unwrap().capacitor;
    let s_cap_abs = sys.get_combat_state(ship_id).await.unwrap().unwrap().capacitor;
    assert!(v_cap_abs < 1_200.0 && s_cap_abs > 800.0, "Nosferatu must drain on absolute GJ (1200 > 800) even when target % (40%) < self % (80%): got victim={v_cap_abs}, self={s_cap_abs}");

    // Blood Raider hull dynamically derived from SDE attributes/effects overrides target_cap > self_cap:
    let nos_item = DogmaItemState::from_sde(nos_id, 530, &ctx.sde);
    assert!(!is_blood_raider_nosferatu_hull(640, &ctx.sde), "Raven is not Blood Raider");
    assert!(is_blood_raider_nosferatu_hull(17920, &ctx.sde), "Bhaalgorn is Blood Raider (nosOverride attr 1945)");
    assert_eq!(eval_energy_nosferatu_with_hull(&nos_item, 1_000.0, 500.0, 1000.0, 400.0, false, &ctx.sde).actual_transferred, 0.0);
    assert!(eval_energy_nosferatu_with_hull(&nos_item, 1_000.0, 500.0, 1000.0, 400.0, true, &ctx.sde).actual_transferred > 0.0);

    // Standard hull cannot drain when target absolute GJ (400) < source absolute GJ (500), even if target % (80%) > source % (25%):
    let mut s_cs_std = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    s_cs_std.capacitor_recharge_time_ms = 0.0;
    s_cs_std.max_capacitor = 2_000.0;
    s_cs_std.capacitor = 500.0; // 25%
    sys.set_combat_state(ship_id, s_cs_std).await.unwrap();
    let mut v_cs_std = sys.get_combat_state(victim_id).await.unwrap().unwrap();
    v_cs_std.capacitor_recharge_time_ms = 0.0;
    v_cs_std.max_capacitor = 500.0;
    v_cs_std.capacitor = 400.0; // 80%
    sys.set_combat_state(victim_id, v_cs_std).await.unwrap();

    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[
        PyRep::Integer(nos_id), PyRep::String("energyNosferatuFalloff".into()), PyRep::Integer(victim_id), PyRep::Integer(0),
    ], &kw, None).await.expect("Activate Nosferatu on standard hull when target GJ < self GJ");
    for _ in 0..6 { let _ = sys.tick().await; }
    assert_eq!(sys.get_combat_state(victim_id).await.unwrap().unwrap().capacitor, 400.0, "Standard hull cannot drain when target GJ < self GJ");

    // Blood Raider Bhaalgorn (17920) drains target even when target GJ (400) < self GJ (500):
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship_item) = snap.items.get_mut(&ship_id) { ship_item.type_id = 17920; }
    });
    ctx.invalidate_ship_fitting(ship_id);
    let mut ship_cs_br = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    ship_cs_br.capacitor_recharge_time_ms = 0.0;
    ship_cs_br.max_capacitor = 2000.0;
    ship_cs_br.capacitor = 500.0;
    sys.set_combat_state(ship_id, ship_cs_br).await.unwrap();
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[
        PyRep::Integer(nos_id), PyRep::String("energyNosferatuFalloff".into()), PyRep::Integer(victim_id), PyRep::Integer(0),
    ], &kw, None).await.expect("Activate Nosferatu on Blood Raider Bhaalgorn (17920)");
    for _ in 0..6 { let _ = sys.tick().await; }
    let v_cap_br = sys.get_combat_state(victim_id).await.unwrap().unwrap().capacitor;
    let s_cap_br = sys.get_combat_state(ship_id).await.unwrap().unwrap().capacitor;
    assert!(v_cap_br < 400.0 && s_cap_br > 500.0, "Blood Raider hull bonus must override absolute cap limitation and drain target");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship_item) = snap.items.get_mut(&ship_id) { ship_item.type_id = 640; }
    });
    ctx.invalidate_ship_fitting(ship_id);

    // 3. Target Painter Bloom (`feat:combat.target-painter-bloom`)
    let bloom_two = eval_target_painters_sig_radius(40.0, &[25.0, 25.0]);
    let expected_two = 40.0 * (1.0 + 0.25) * (1.0 + 0.25 * (-1.0_f64 / (2.67_f64 * 2.67_f64)).exp());
    assert!((bloom_two - expected_two).abs() < 1e-3);

    // Position victim at 6000m with transversal velocity 150 m/s
    let (_, mut v_ball) = sys.get_ball_snapshot(victim_id).await.unwrap().unwrap();
    v_ball.position = ship_ball.position + DVec3::new(6_000.0, 0.0, 0.0);
    v_ball.velocity = DVec3::new(0.0, 150.0, 0.0);
    v_ball.max_velocity = 150.0;
    v_ball.speed_fraction = 1.0;
    v_ball.mode = MovementMode::GotoDirection { direction: DVec3::Y };
    let mut v_cs_reset = sys.get_combat_state(victim_id).await.unwrap().unwrap();
    v_cs_reset.shield_hp = 5_000.0;
    v_cs_reset.max_shield_hp = 5_000.0;
    sys.spawn_ball_with_combat(v_ball.clone(), v_cs_reset.clone()).await.unwrap();

    // Negative test: Firing without a target lock must fail with TargetNotLocked (`R-WEAPON-LOCK-BYPASS`)
    let unlocked_err = fire_weapon(&ctx, &mut sess, turret_id, victim_id).await.unwrap_err();
    assert_eq!(unlocked_err.code, "TargetNotLocked", "Firing without a target lock must fail with TargetNotLocked");

    rpc_lock_target(&ctx, &sess, char_id, ship_id, victim_id).await;
    sys.set_ball_position(victim_id, ship_ball.position + DVec3::new(6_000.0, 0.0, 0.0)).await;

    let m_rep_unpainted = fire_weapon(&ctx, &mut sess, launcher_id, victim_id).await.unwrap();
    let dmg_unpainted = m_rep_unpainted.dict_get("appliedDamage").and_then(|v| v.as_f64()).unwrap();

    let turret_rep_unpainted = fire_weapon(&ctx, &mut sess, turret_id, victim_id).await.expect("Fire turret unpainted");
    let p_hit_unpainted = turret_rep_unpainted.dict_get("hitChance").and_then(|v| v.as_f64()).unwrap();

    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[
        PyRep::Integer(tp1_id), PyRep::String("targetPassively".into()), PyRep::Integer(victim_id), PyRep::Integer(1),
    ], &kw, None).await.expect("Activate Target Painter I #1");
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[
        PyRep::Integer(tp2_id), PyRep::String("targetPassively".into()), PyRep::Integer(victim_id), PyRep::Integer(1),
    ], &kw, None).await.expect("Activate Target Painter I #2");

    let painted_sig = sys.get_combat_state(victim_id).await.unwrap().unwrap().signature_radius;
    let expected_two_victim = 35.0 * (1.0 + 0.25) * (1.0 + 0.25 * (-1.0_f64 / (2.67_f64 * 2.67_f64)).exp());
    assert!((painted_sig - expected_two_victim).abs() < 1e-2, "Two Target Painters must bloom victim signature to exact {expected_two_victim:.4}m, got {painted_sig:.4}m");

    sys.set_combat_state(victim_id, v_cs_reset).await.unwrap();
    let m_rep_painted = fire_weapon(&ctx, &mut sess, launcher_id, victim_id).await.unwrap();
    let dmg_painted = m_rep_painted.dict_get("appliedDamage").and_then(|v| v.as_f64()).unwrap();
    assert!(dmg_painted > dmg_unpainted && dmg_painted > 0.0, "Target Painter bloom must apply to incoming weapon damage: got painted={dmg_painted}, unpainted={dmg_unpainted}");

    let turret_rep_painted = fire_weapon(&ctx, &mut sess, turret_id, victim_id).await.expect("Fire turret painted");
    let p_hit_painted = turret_rep_painted.dict_get("hitChance").and_then(|v| v.as_f64()).unwrap();
    assert!(p_hit_painted > p_hit_unpainted, "Target Painter bloom must increase turret hit chance (painted={p_hit_painted} > unpainted={p_hit_unpainted})");

    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(tp1_id), PyRep::String("targetPassively".into())], &kw, None).await.unwrap();
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(tp2_id), PyRep::String("targetPassively".into())], &kw, None).await.unwrap();
    let restored_sig = sys.get_combat_state(victim_id).await.unwrap().unwrap().signature_radius;
    assert!((restored_sig - 35.0).abs() < 1.0, "Deactivating Target Painter must restore victim signature_radius to 35.0m, got {restored_sig}");

    // NPC ball test for R-TP-DOUBLE-BLOOM:
    let npc_ball_id = 100_920_070i64;
    sys.spawn_ball_with_combat(Ball {
        id: npc_ball_id, type_id: 587, position: ship_ball.position + DVec3::new(5_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 3.0, mass: 1_000_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }, CombatState { signature_radius: 50.0, base_signature_radius: Some(50.0), ..CombatState::default() }).await.unwrap();
    rpc_lock_target(&ctx, &sess, char_id, ship_id, npc_ball_id).await;
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(tp1_id), PyRep::String("targetPassively".into()), PyRep::Integer(npc_ball_id), PyRep::Integer(1)], &kw, None).await.expect("Activate TP on NPC ball");
    let npc_sig = sys.get_combat_state(npc_ball_id).await.unwrap().unwrap().signature_radius;
    assert_eq!(npc_sig, 62.5, "Single TP (25%) blooms 50m NPC ball to exact 62.5m");
    let eff_npc_sig = ctx.effective_target_signature_radius(npc_ball_id, 50.0);
    assert_eq!(eff_npc_sig, 62.5, "effective_target_signature_radius must not compound (no double-bloom)");
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(tp1_id), PyRep::String("targetPassively".into())], &kw, None).await.unwrap();
    assert_eq!(sys.get_combat_state(npc_ball_id).await.unwrap().unwrap().signature_radius, 50.0, "Deactivating TP restores NPC ball base signature 50.0m");
    sys.remove_target_lock(ship_id, npc_ball_id).await.unwrap();
    let _ = sys.remove_ball(npc_ball_id).await;

    // 4 & 5. Sensor Dampeners (`feat:combat.sensor-dampener-targeting-range`, `feat:combat.sensor-dampener-scan-resolution`)
    let (_, mut v_ball_sd) = sys.get_ball_snapshot(victim_id).await.unwrap().unwrap();
    v_ball_sd.position = ship_ball.position + DVec3::new(1_000.0, 0.0, 0.0);
    v_ball_sd.velocity = DVec3::ZERO;
    v_ball_sd.speed_fraction = 0.0;
    v_ball_sd.mode = MovementMode::Stop;
    sys.spawn_ball_with_combat(v_ball_sd, sys.get_combat_state(victim_id).await.unwrap().unwrap()).await.unwrap();
    let mut victim_sess = sess.clone();
    victim_sess.character_id = Some(victim_char_id);
    victim_sess.ship_id = Some(victim_id);
    ctx.dispatch_rpc(&mut victim_sess, "dogmaIM", None, "AddTarget", &[PyRep::Integer(mid_target_id)], &kw, None).await.expect("Lock mid_target at 20km before dampening");
    sys.remove_target_lock(victim_id, mid_target_id).await.unwrap();
    rpc_lock_target(&ctx, &sess, victim_char_id, victim_id, distant_id).await;
    assert_eq!(sys.get_target_locks(victim_id).await.unwrap().len(), 1);
    rpc_lock_target(&ctx, &sess, char_id, ship_id, victim_id).await;

    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(damp_id), PyRep::String("remoteSensorDampFalloff".into()), PyRep::Integer(victim_id), PyRep::Integer(1)], &kw, None).await.expect("Activate unscripted Sensor Dampener #1");
    ctx.invalidate_ship_fitting(victim_id);
    let range_1damp = ctx.get_or_rebuild_ship_fitting(victim_char_id, victim_id).unwrap().ship.get_attribute(76, &ctx.sde);
    assert!((range_1damp - 22_330.125).abs() < 1.0, "1 dampener must reduce maxTargetRange to ~22330.1m, got {range_1damp}");
    assert_eq!(sys.get_target_locks(victim_id).await.unwrap().len(), 1, "Active lock on distant_id preserved under 1 dampener");

    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(damp2_id), PyRep::String("remoteSensorDampFalloff".into()), PyRep::Integer(victim_id), PyRep::Integer(1)], &kw, None).await.expect("Activate unscripted Sensor Dampener #2");
    ctx.invalidate_ship_fitting(victim_id);
    let range_2damp = ctx.get_or_rebuild_ship_fitting(victim_char_id, victim_id).unwrap().ship.get_attribute(76, &ctx.sde);
    let expected_2damp = 25_875.0 * (1.0 - 0.137) * (1.0 - 0.137 * (-1.0_f64 / (2.67_f64 * 2.67_f64)).exp());
    assert!((range_2damp - expected_2damp).abs() < 2.0, "2 dampeners must reduce maxTargetRange to ~19671.3m, got {range_2damp}");
    assert_eq!(sys.get_target_locks(victim_id).await.unwrap().len(), 1, "Active lock on distant_id preserved under 2 dampeners");

    let relock_mid_err = ctx.dispatch_rpc(&mut victim_sess, "dogmaIM", None, "AddTarget", &[PyRep::Integer(mid_target_id)], &kw, None).await;
    assert!(relock_mid_err.is_err(), "AddTarget at 20km beyond 2-dampened maxTargetRange (19.67km) must fail");
    sys.remove_target_lock(victim_id, distant_id).await.unwrap();
    let relock_distant_err = ctx.dispatch_rpc(&mut victim_sess, "dogmaIM", None, "AddTarget", &[PyRep::Integer(distant_id)], &kw, None).await;
    assert!(relock_distant_err.is_err(), "AddTarget on distant_id beyond reduced maxTargetRange must fail");
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(damp2_id), PyRep::String("remoteSensorDampFalloff".into())], &kw, None).await.unwrap();
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(damp_id), PyRep::String("remoteSensorDampFalloff".into())], &kw, None).await.unwrap();

    // 5. Sensor Dampener Scan Resolution Script & AddTarget Lock Time (`feat:combat.sensor-dampener-scan-resolution`)
    let scan_script_id = 100_920_021i64;
    seed_item(&ctx, scan_script_id, 29013, char_id, ship_id, InventoryFlag::MedSlot2, 1);
    ctx.invalidate_ship_fitting(ship_id);

    let base_victim_scan_res = ctx.get_or_rebuild_ship_fitting(victim_char_id, victim_id).unwrap().ship.get_attribute(564, &ctx.sde);
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(damp_id), PyRep::String("remoteSensorDampFalloff".into()), PyRep::Integer(victim_id), PyRep::Integer(1)], &kw, None).await.expect("Activate Scan-Res scripted Sensor Dampener");
    ctx.invalidate_ship_fitting(victim_id);
    let damped_victim_scan_res = ctx.get_or_rebuild_ship_fitting(victim_char_id, victim_id).unwrap().ship.get_attribute(564, &ctx.sde);
    assert!(damped_victim_scan_res < base_victim_scan_res, "Scan Resolution scripted dampener must reduce victim scanResolution (got {damped_victim_scan_res} < {base_victim_scan_res})");

    let add_target_rep = ctx.dispatch_rpc(&mut victim_sess, "dogmaIM", None, "AddTarget", &[PyRep::Integer(ship_id)], &kw, None).await.expect("AddTarget from damped ship");
    let lock_time_ms = add_target_rep.dict_get("lockTimeMs").and_then(|v| v.as_f64()).unwrap();
    let raw_sig = sys.get_combat_state(ship_id).await.unwrap().unwrap().signature_radius;
    let target_sig = ctx.effective_target_signature_radius(ship_id, raw_sig);
    let expected_lock_time = eve_destiny::combat::compute_lock_time_seconds(damped_victim_scan_res, target_sig) * 1000.0;
    assert!((lock_time_ms - expected_lock_time).abs() < 1e-3, "Lock time from AddTarget must match T_lock = 40000 / (R_scan * asinh(S)^2): got {lock_time_ms}, expected {expected_lock_time}");
    assert!(lock_time_ms >= 250.0, "Lock time must respect 250ms (0.25s) floor");
    assert_eq!(eve_destiny::combat::compute_lock_time_seconds(10_000_000.0, 10_000.0), 0.25, "compute_lock_time_seconds must enforce 0.25s floor");
    sys.remove_target_lock(victim_id, ship_id).await.unwrap();

    let damp_item = DogmaItemState::from_sde(damp_id, 1968, &ctx.sde);
    let (sr_unscripted, _) = eval_scripted_sensor_dampener(&damp_item, None, 1_500.0, &ctx.sde);
    let (sr_mult, r_mult) = eval_scripted_sensor_dampener(&damp_item, Some(29013), 1_500.0, &ctx.sde);
    assert_eq!(r_mult, 1.0, "Scan Resolution Script must suppress range penalty");
    assert!(((1.0 - sr_mult) - 2.0 * (1.0 - sr_unscripted)).abs() < 1e-4, "Scan Resolution Script must double scanResolution penalty");
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(damp_id), PyRep::String("remoteSensorDampFalloff".into())], &kw, None).await.unwrap();
    ctx.invalidate_ship_fitting(victim_id);

    // 6, 7, 8. Tracking Disruptor & Guidance Disruptor (`feat:combat.tracking-disruptor-optimal-falloff`, `feat:combat.tracking-disruptor-tracking-speed`, `feat:combat.guidance-disruptor-missiles`)
    let fit_baseline = ctx.get_or_rebuild_ship_fitting(victim_char_id, victim_id).unwrap();
    let base_turret_opt = fit_baseline.get_module_effective_attribute(v_turret_id, 54, &ctx.sde);
    let base_turret_falloff = fit_baseline.get_module_effective_attribute(v_turret_id, 158, &ctx.sde);
    let base_turret_track = fit_baseline.get_module_effective_attribute(v_turret_id, 160, &ctx.sde);

    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[
        PyRep::Integer(td_id), PyRep::String("shipModuleTrackingDisruptor".into()), PyRep::Integer(victim_id), PyRep::Integer(1),
    ], &kw, None).await.expect("Activate Tracking Disruptor I");
    let fit_td = ctx.get_or_rebuild_ship_fitting(victim_char_id, victim_id).unwrap();
    let disrupted_opt = fit_td.get_module_effective_attribute(v_turret_id, 54, &ctx.sde);
    let disrupted_falloff = fit_td.get_module_effective_attribute(v_turret_id, 158, &ctx.sde);
    let disrupted_track = fit_td.get_module_effective_attribute(v_turret_id, 160, &ctx.sde);
    assert!(disrupted_opt < base_turret_opt && disrupted_falloff < base_turret_falloff);
    assert!(disrupted_track < base_turret_track);
    let td_item = DogmaItemState::from_sde(td_id, 2108, &ctx.sde);
    let (trk_un, opt_un, fall_un) = eval_scripted_tracking_disruptor(&td_item, None, 1_000.0, &ctx.sde);
    let (trk_opt_scr, opt_opt_scr, fall_opt_scr) = eval_scripted_tracking_disruptor(&td_item, Some(29005), 1_000.0, &ctx.sde);
    assert_eq!(trk_opt_scr, 1.0, "Optimal Range Disruption Script must zero tracking penalty");
    assert!(((1.0 - opt_opt_scr) - 2.0 * (1.0 - opt_un)).abs() < 1e-4 && ((1.0 - fall_opt_scr) - 2.0 * (1.0 - fall_un)).abs() < 1e-4);
    let (trk_trk_scr, opt_trk_scr, fall_trk_scr) = eval_scripted_tracking_disruptor(&td_item, Some(29007), 1_000.0, &ctx.sde);
    assert!(((1.0 - trk_trk_scr) - 2.0 * (1.0 - trk_un)).abs() < 1e-4, "Tracking Speed Disruption Script must double tracking penalty");
    assert_eq!((opt_trk_scr, fall_trk_scr), (1.0, 1.0));
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(td_id), PyRep::String("shipModuleTrackingDisruptor".into())], &kw, None).await.unwrap();

    let (_, mut v_ball_td) = sys.get_ball_snapshot(victim_id).await.unwrap().unwrap();
    v_ball_td.position = ship_ball.position + DVec3::new(6_000.0, 0.0, 0.0);
    v_ball_td.velocity = DVec3::new(0.0, 150.0, 0.0);
    v_ball_td.max_velocity = 150.0;
    v_ball_td.speed_fraction = 1.0;
    v_ball_td.mode = MovementMode::GotoDirection { direction: DVec3::Y };
    sys.spawn_ball_with_combat(v_ball_td, sys.get_combat_state(victim_id).await.unwrap().unwrap()).await.unwrap();
    rpc_lock_target(&ctx, &sess, char_id, ship_id, victim_id).await;
    rpc_lock_target(&ctx, &sess, victim_char_id, victim_id, ship_id).await;
    sys.set_ball_position(victim_id, ship_ball.position + DVec3::new(6_000.0, 0.0, 0.0)).await;

    // Live turret firing before and after unscripted & scripted Tracking Disruptor:
    let turret_rep_base = fire_weapon(&ctx, &mut sess, turret_id, victim_id).await.expect("Fire turret baseline before TD");
    let p_hit_base = turret_rep_base.dict_get("hitChance").and_then(|v| v.as_f64()).unwrap();

    sys.set_capacitor(victim_id, 500.0).await.unwrap();

    // 1) Unscripted TD from victim on ship_id
    ctx.dispatch_rpc(&mut victim_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(v_td_id), PyRep::String("shipModuleTrackingDisruptor".into()), PyRep::Integer(ship_id), PyRep::Integer(1)], &kw, None).await.expect("Activate unscripted TD on ship_id");
    let turret_rep_td_un = fire_weapon(&ctx, &mut sess, turret_id, victim_id).await.expect("Fire turret while unscripted TD active");
    let p_hit_td_un = turret_rep_td_un.dict_get("hitChance").and_then(|v| v.as_f64()).unwrap();
    assert!(p_hit_td_un < p_hit_base, "Unscripted TD must reduce turret hit chance (td={p_hit_td_un} < base={p_hit_base})");
    ctx.dispatch_rpc(&mut victim_sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(v_td_id), PyRep::String("shipModuleTrackingDisruptor".into())], &kw, None).await.unwrap();

    // 2) Scripted TD: Optimal Range Disruption Script (29005)
    ctx.dispatch_rpc(&mut victim_sess, "dogmaIM", None, "LoadAmmo", &[
        PyRep::Integer(victim_id), PyRep::List(vec![PyRep::Integer(v_td_id)]), PyRep::List(vec![PyRep::Integer(v_td_opt_script_id)]), PyRep::Integer(victim_id),
    ], &kw, None).await.expect("Load Optimal Range script into TD");
    ctx.advance_secs(11);
    ctx.dispatch_rpc(&mut victim_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(v_td_id), PyRep::String("shipModuleTrackingDisruptor".into()), PyRep::Integer(ship_id), PyRep::Integer(1)], &kw, None).await.expect("Activate Range scripted TD on ship_id");
    let turret_rep_td_opt = fire_weapon(&ctx, &mut sess, turret_id, victim_id).await.expect("Fire turret while Range scripted TD active");
    let p_hit_td_opt = turret_rep_td_opt.dict_get("hitChance").and_then(|v| v.as_f64()).unwrap();
    assert!(p_hit_td_opt < p_hit_base, "Optimal range scripted TD must reduce turret hit chance (opt_td={p_hit_td_opt} < base={p_hit_base})");
    ctx.dispatch_rpc(&mut victim_sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(v_td_id), PyRep::String("shipModuleTrackingDisruptor".into())], &kw, None).await.unwrap();

    // 3) Scripted TD: Tracking Speed Disruption Script (29007)
    ctx.dispatch_rpc(&mut victim_sess, "dogmaIM", None, "LoadAmmo", &[
        PyRep::Integer(victim_id), PyRep::List(vec![PyRep::Integer(v_td_id)]), PyRep::List(vec![PyRep::Integer(v_td_trk_script_id)]), PyRep::Integer(victim_id),
    ], &kw, None).await.expect("Load Tracking Speed script into TD");
    ctx.advance_secs(11);
    ctx.dispatch_rpc(&mut victim_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(v_td_id), PyRep::String("shipModuleTrackingDisruptor".into()), PyRep::Integer(ship_id), PyRep::Integer(1)], &kw, None).await.expect("Activate Tracking Speed scripted TD on ship_id");
    let turret_rep_td_trk = fire_weapon(&ctx, &mut sess, turret_id, victim_id).await.expect("Fire turret while Tracking Speed scripted TD active");
    let p_hit_td_trk = turret_rep_td_trk.dict_get("hitChance").and_then(|v| v.as_f64()).unwrap();
    assert!(p_hit_td_trk < p_hit_base, "Tracking speed scripted TD must reduce turret hit chance (trk_td={p_hit_td_trk} < base={p_hit_base})");
    ctx.dispatch_rpc(&mut victim_sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(v_td_id), PyRep::String("shipModuleTrackingDisruptor".into())], &kw, None).await.unwrap();

    let base_m_vel = fit_baseline.get_charge_effective_attribute(v_launcher_id, 37, &ctx.sde);
    let base_m_time = fit_baseline.get_charge_effective_attribute(v_launcher_id, 281, &ctx.sde);
    let base_m_exp_vel = fit_baseline.get_charge_effective_attribute(v_launcher_id, 653, &ctx.sde);
    let base_m_exp_rad = fit_baseline.get_charge_effective_attribute(v_launcher_id, 654, &ctx.sde);

    // Live missile fire undisrupted against small/fast target
    let (_, mut s_ball_snap) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    s_ball_snap.velocity = DVec3::new(0.0, 500.0, 0.0);
    s_ball_snap.max_velocity = 500.0;
    s_ball_snap.speed_fraction = 1.0;
    s_ball_snap.mode = MovementMode::GotoDirection { direction: DVec3::Y };
    s_ball_snap.radius = 35.0;
    sys.spawn_ball_with_combat(s_ball_snap, sys.get_combat_state(ship_id).await.unwrap().unwrap()).await.unwrap();
    rpc_lock_target(&ctx, &sess, victim_char_id, victim_id, ship_id).await;
    rpc_lock_target(&ctx, &sess, char_id, ship_id, victim_id).await;
    sys.set_ball_position(ship_id, ship_ball.position).await;
    sys.set_ball_position(victim_id, ship_ball.position + DVec3::new(6_000.0, 0.0, 0.0)).await;

    let hp_before_undisrupted = sys.get_combat_state(ship_id).await.unwrap().unwrap().shield_hp;
    fire_weapon(&ctx, &mut victim_sess, v_launcher_id, ship_id).await.expect("Fire launcher undisrupted");
    for _ in 0..4 { let _ = sys.tick().await; }
    let hp_after_undisrupted = sys.get_combat_state(ship_id).await.unwrap().unwrap().shield_hp;
    let dmg_undisrupted = hp_before_undisrupted - hp_after_undisrupted;
    assert!(dmg_undisrupted > 0.0, "Undisrupted missile must deal damage (got {dmg_undisrupted})");

    // 1) Unscripted Guidance Disruptor
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[
        PyRep::Integer(gd_id), PyRep::String("shipModuleGuidanceDisruptor".into()), PyRep::Integer(victim_id), PyRep::Integer(1),
    ], &kw, None).await.expect("Activate Guidance Disruptor I unscripted");
    let fit_gd = ctx.get_or_rebuild_ship_fitting(victim_char_id, victim_id).unwrap();
    assert!(fit_gd.get_charge_effective_attribute(v_launcher_id, 37, &ctx.sde) < base_m_vel);
    assert!(fit_gd.get_charge_effective_attribute(v_launcher_id, 281, &ctx.sde) < base_m_time);
    assert!(fit_gd.get_charge_effective_attribute(v_launcher_id, 653, &ctx.sde) < base_m_exp_vel);
    assert!(fit_gd.get_charge_effective_attribute(v_launcher_id, 654, &ctx.sde) > base_m_exp_rad);
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(gd_id), PyRep::String("shipModuleGuidanceDisruptor".into())], &kw, None).await.unwrap();

    // 2) Scripted GD: Missile Range Disruption Script (40334)
    let gd_rng_script_id = 100_920_064i64;
    seed_item(&ctx, gd_rng_script_id, 40334, char_id, ship_id, InventoryFlag::MedSlot4, 1);
    ctx.invalidate_ship_fitting(ship_id);
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[
        PyRep::Integer(gd_id), PyRep::String("shipModuleGuidanceDisruptor".into()), PyRep::Integer(victim_id), PyRep::Integer(1),
    ], &kw, None).await.expect("Activate Range-scripted GD");
    let fit_gd_rng = ctx.get_or_rebuild_ship_fitting(victim_char_id, victim_id).unwrap();
    let rng_vel = fit_gd_rng.get_charge_effective_attribute(v_launcher_id, 37, &ctx.sde);
    let rng_time = fit_gd_rng.get_charge_effective_attribute(v_launcher_id, 281, &ctx.sde);
    assert!(rng_vel < fit_gd.get_charge_effective_attribute(v_launcher_id, 37, &ctx.sde), "Range script must double velocity reduction");
    assert!(rng_time < fit_gd.get_charge_effective_attribute(v_launcher_id, 281, &ctx.sde), "Range script must double flight time reduction");
    assert!((fit_gd_rng.get_charge_effective_attribute(v_launcher_id, 653, &ctx.sde) - base_m_exp_vel).abs() < 1e-4);
    assert!((fit_gd_rng.get_charge_effective_attribute(v_launcher_id, 654, &ctx.sde) - base_m_exp_rad).abs() < 1e-4);
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(gd_id), PyRep::String("shipModuleGuidanceDisruptor".into())], &kw, None).await.unwrap();
    ctx.db.with_mut_snapshot(|snap| { snap.items.remove(&gd_rng_script_id); });
    ctx.invalidate_ship_fitting(ship_id);

    // 3) Scripted GD: Missile Precision Disruption Script (40335) & Live missile damage reduction
    let gd_prec_script_id = 100_920_063i64;
    seed_item(&ctx, gd_prec_script_id, 40335, char_id, ship_id, InventoryFlag::MedSlot4, 1);
    ctx.invalidate_ship_fitting(ship_id);
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[
        PyRep::Integer(gd_id), PyRep::String("shipModuleGuidanceDisruptor".into()), PyRep::Integer(victim_id), PyRep::Integer(1),
    ], &kw, None).await.expect("Activate Precision-scripted GD");
    let fit_gd_prec = ctx.get_or_rebuild_ship_fitting(victim_char_id, victim_id).unwrap();
    assert!((fit_gd_prec.get_charge_effective_attribute(v_launcher_id, 37, &ctx.sde) - base_m_vel).abs() < 1e-4);
    assert!((fit_gd_prec.get_charge_effective_attribute(v_launcher_id, 281, &ctx.sde) - base_m_time).abs() < 1e-4);
    assert!(fit_gd_prec.get_charge_effective_attribute(v_launcher_id, 653, &ctx.sde) < fit_gd.get_charge_effective_attribute(v_launcher_id, 653, &ctx.sde));
    assert!(fit_gd_prec.get_charge_effective_attribute(v_launcher_id, 654, &ctx.sde) > fit_gd.get_charge_effective_attribute(v_launcher_id, 654, &ctx.sde));

    let hp_before_disrupted = sys.get_combat_state(ship_id).await.unwrap().unwrap().shield_hp;
    fire_weapon(&ctx, &mut victim_sess, v_launcher_id, ship_id).await.expect("Fire launcher disrupted");
    for _ in 0..4 { let _ = sys.tick().await; }
    let hp_after_disrupted = sys.get_combat_state(ship_id).await.unwrap().unwrap().shield_hp;
    let dmg_disrupted = hp_before_disrupted - hp_after_disrupted;
    assert!(dmg_disrupted > 0.0 && dmg_disrupted < dmg_undisrupted, "Guidance Disruption must degrade live missile applied damage against fast/small target: got {dmg_disrupted} < {dmg_undisrupted}");

    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(gd_id), PyRep::String("shipModuleGuidanceDisruptor".into())], &kw, None).await.unwrap();
    ctx.db.with_mut_snapshot(|snap| { snap.items.remove(&gd_prec_script_id); });
    ctx.invalidate_ship_fitting(ship_id);
    sys.remove_target_lock(victim_id, ship_id).await.unwrap();
    let (_, mut s_ball_reset) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    s_ball_reset.position = ship_ball.position;
    s_ball_reset.velocity = DVec3::ZERO;
    s_ball_reset.speed_fraction = 0.0;
    s_ball_reset.mode = MovementMode::Stop;
    sys.spawn_ball_with_combat(s_ball_reset, sys.get_combat_state(ship_id).await.unwrap().unwrap()).await.unwrap();

    // 9. ECM Burst Radius & Jam Roll (`feat:combat.ecm-burst-radius`)
    ctx.crimewatch.write().unwrap().states.clear();
    eve_server::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| { rt.limited_engagements.clear(); });
    // Verify analytical ECM jam chance formula: SDE jammer strengths are 6.0 for T1 (580)
    let p_jam_half = eve_dogma::compute_ecm_jam_chance(6.0, 12.0, 4_000.0, 10_000.0, 0.0);
    assert_eq!(p_jam_half, 0.5, "6.0 jam strength vs 12.0 sensor strength yields 50% jam chance");
    let p_jam_low = eve_dogma::compute_ecm_jam_chance(6.0, 60.0, 4_000.0, 10_000.0, 0.0);
    assert!((p_jam_low - 0.10).abs() < 1e-4, "High sensor strength (60) yields 10% jam chance");

    // Burst Jammer I (`580`) has exact 5,000 m AoE radius and Burst Jammer II (`2117`) has exact 6,000 m AoE radius (`R-BURST-ECM-01`).
    let burst_fit = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap();
    let burst_mod = burst_fit.build_effective_module_state(burst_id, 580, false, &ctx.sde);
    let burst_range = eve_dogma::compute_burst_jammer_radius_m(&ctx.sde.types[&580], &burst_mod, &ctx.sde);
    assert_eq!(burst_range, 5_000.0, "Burst Jammer I (580) AoE radius must be exact 5,000m (5 km)");
    let burst_t2_mod = DogmaItemState::from_sde(burst_id, 2117, &ctx.sde);
    assert_eq!(eve_dogma::compute_burst_jammer_radius_m(&ctx.sde.types[&2117], &burst_t2_mod, &ctx.sde), 6_000.0);

    // In-range victim at 4,000m (inside 5,000m Burst Jammer I radius):
    let (_, mut v_ball_burst) = sys.get_ball_snapshot(victim_id).await.unwrap().unwrap();
    v_ball_burst.position = ship_ball.position + DVec3::new(4_000.0, 0.0, 0.0);
    v_ball_burst.velocity = DVec3::ZERO;
    v_ball_burst.speed_fraction = 0.0;
    v_ball_burst.mode = MovementMode::Stop;
    sys.spawn_ball_with_combat(v_ball_burst, sys.get_combat_state(victim_id).await.unwrap().unwrap()).await.unwrap();
    rpc_lock_target(&ctx, &sess, victim_char_id, victim_id, ship_id).await;

    // Friendly uncloaked fleetmate inside radius (4,500m) to verify friendly fire:
    let friendly_id = 100_920_080i64;
    seed_item(&ctx, friendly_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1);
    sys.spawn_ball_with_combat(spawn_test_ball(friendly_id, DVec3::new(4_500.0, 0.0, 0.0), 40.0), CombatState::default()).await.unwrap();
    rpc_lock_target(&ctx, &sess, char_id, friendly_id, ship_id).await;

    // Boundary ship at 5,500m (> 5,000m T1 Burst Jammer radius):
    let outside_burst_id = 100_920_081i64;
    seed_item(&ctx, outside_burst_id, 587, 90_000_002, sys_id as i64, InventoryFlag::None, 1);
    sys.spawn_ball_with_combat(spawn_test_ball(outside_burst_id, DVec3::new(5_500.0, 0.0, 0.0), 40.0), CombatState::default()).await.unwrap();
    rpc_lock_target(&ctx, &sess, victim_char_id, outside_burst_id, ship_id).await;
    rpc_lock_target(&ctx, &sess, victim_char_id, distant_id, ship_id).await;

    let mut cloaked_cs_prep = sys.get_combat_state(cloaked_id).await.unwrap().unwrap();
    cloaked_cs_prep.is_cloaked = false;
    sys.set_combat_state(cloaked_id, cloaked_cs_prep).await.unwrap();
    rpc_lock_target(&ctx, &sess, victim_char_id, cloaked_id, distant_id).await;
    let mut cloaked_cs_now = sys.get_combat_state(cloaked_id).await.unwrap().unwrap();
    cloaked_cs_now.is_cloaked = true;
    sys.set_combat_state(cloaked_id, cloaked_cs_now).await.unwrap();
    assert_eq!(sys.get_target_locks(cloaked_id).await.unwrap().len(), 1, "Cloaked ship holds lock on distant_id prior to burst");

    ctx.dispatch_rpc(&mut sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(2)], &kw, None).await.unwrap();
    sys.set_capacitor(ship_id, 2000.0).await.unwrap();
    let green_burst_err = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(burst_id), PyRep::String("ecmBurst".into()), PyRep::None, PyRep::Integer(0)], &kw, None).await;
    assert!(green_burst_err.is_err(), "Activating Burst Jammer in High-sec on Safety Green must be rejected");

    ctx.dispatch_rpc(&mut sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(0)], &kw, None).await.unwrap();
    let mut burst_hit = false;
    for iter in 0..50 {
        sys.set_capacitor(ship_id, 2000.0).await.unwrap();
        ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(burst_id), PyRep::String("ecmBurst".into()), PyRep::None, PyRep::Integer(0)], &kw, None).await.expect("Activate Burst Jammer I");
        if iter == 0 {
            let spam_err = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(burst_id), PyRep::String("ecmBurst".into()), PyRep::None, PyRep::Integer(0)], &kw, None).await.unwrap_err();
            assert_eq!(spam_err.code, "ModuleReactivationDelay", "Re-activating Burst Jammer within cycle window must fail");
            assert_eq!(sys.get_target_locks(cloaked_id).await.unwrap().len(), 1, "Cloaked ship locks must NOT be cleared by ECM Burst");
        }
        assert_eq!(sys.get_target_locks(outside_burst_id).await.unwrap().len(), 1, "Ship outside ecmBurstRange must NOT be jammed");
        assert_eq!(sys.get_target_locks(distant_id).await.unwrap().len(), 1, "Distant ship must NOT be jammed");
        assert_eq!(sys.get_combat_state(cloaked_id).await.unwrap().unwrap().ecm_jammed_remaining_s, 0.0, "Cloaked ship must NOT be jammed");
        if sys.get_target_locks(victim_id).await.unwrap().is_empty() && sys.get_target_locks(friendly_id).await.unwrap().is_empty() {
            burst_hit = true;
            break;
        }
        sys.step_manual(31.0).await.unwrap();
    }
    assert!(burst_hit, "Burst Jammer pulse must break locks on uncloaked ships (including friendly fire) within range");
    assert_eq!(sys.get_combat_state(victim_id).await.unwrap().unwrap().ecm_jammed_remaining_s, 0.0, "Burst Jammer must NOT prevent relocking (0s lockout)");
    rpc_lock_target(&ctx, &sess, victim_char_id, victim_id, ship_id).await;
    assert_eq!(sys.get_target_locks(victim_id).await.unwrap().len(), 1, "Victim can immediately relock after ECM burst");
    let cw_st = ctx.crimewatch.with_crimewatch(|cw| cw.states.get(&char_id).cloned().unwrap_or_default());
    assert!(cw_st.criminal_flag_until_unix > 0 && cw_st.pending_concord_arrival.is_some(), "ECM Burst in High-sec must flag pilot Criminal and schedule CONCORD");
}
