//! Integration tests for advanced modules, lances, deployables, and nanite paste:
//! - `feat:modules.energy-nosferatu-relative-capacitor`
//! - `feat:modules.mobile-depot-deployment-and-refitting`
//! - `feat:modules.nanite-repair-paste-in-space`
//! - `feat:modules.disruptive-lance`
//! - `feat:modules.lance-doomsday-directional-geometry`

#![forbid(unsafe_code)]

use crate::modules_advanced_tactical::{build_ctx, seed_pilot, NULL_SYS};
use crate::features_bounties_ess::{give_item, set_skill, spawn_ship_in_space};
use eve_db::InventoryFlag;
use eve_destiny::{DamageVector, DVec3};
use eve_proto::PyRep;
use std::collections::HashMap;

#[tokio::test]
async fn test_modules_energy_nosferatu_relative_capacitor() {
    // parity:feat:modules.energy-nosferatu-relative-capacitor
    eve_testkit::attest(&["feat:modules.energy-nosferatu-relative-capacitor"]);

    let ctx = build_ctx();
    let sys = ctx.systems.get_or_spawn(NULL_SYS);
    let kw = HashMap::new();

    // Verify Blood Raider role bonus hulls in dogma engine
    for &br_tid in &[17926u32, 17922, 17920, 11192, 11963] {
        assert!(
            eve_dogma::is_blood_raider_nosferatu_hull(br_tid, &ctx.sde),
            "Hull {br_tid} must have Blood Raider nosferatu override"
        );
    }
    assert!(!eve_dogma::is_blood_raider_nosferatu_hull(641, &ctx.sde), "Megathron is not Blood Raider");

    let c_norm = 952_101;
    let s_norm = 962_101;
    let mut sess_norm = seed_pilot(&ctx, c_norm, s_norm, 641, NULL_SYS, false);
    spawn_ship_in_space(&ctx, c_norm, s_norm, 641, NULL_SYS, DVec3::ZERO).await;
    set_skill(&ctx, c_norm, 3336, 5);
    set_skill(&ctx, c_norm, 3423, 5);
    let nos_mod = 972_101;
    give_item(&ctx, nos_mod, 12261, c_norm, s_norm, InventoryFlag::HiSlot0, 1);

    // Idle control ship to isolate Nosferatu capacitor transfer from passive recharge
    let c_ctrl = 952_109;
    let s_ctrl = 962_109;
    seed_pilot(&ctx, c_ctrl, s_ctrl, 641, NULL_SYS, false);
    spawn_ship_in_space(&ctx, c_ctrl, s_ctrl, 641, NULL_SYS, DVec3::new(0.0, 5_000.0, 0.0)).await;

    let c_tgt = 952_102;
    let s_tgt = 962_102;
    seed_pilot(&ctx, c_tgt, s_tgt, 587, NULL_SYS, false);
    spawn_ship_in_space(&ctx, c_tgt, s_tgt, 587, NULL_SYS, DVec3::new(5_000.0, 0.0, 0.0)).await;

    let set_caps = |norm_cap: f64, ctrl_cap: f64, tgt_cap: f64| {
        let sys = sys.clone();
        async move {
            for (sid, max_c, cur_c) in [(s_norm, 5_000.0, norm_cap), (s_ctrl, 5_000.0, ctrl_cap), (s_tgt, 300.0, tgt_cap)] {
                let mut cs = sys.get_combat_state(sid).await.unwrap().unwrap();
                cs.max_capacitor = max_c;
                cs.capacitor = cur_c;
                sys.set_combat_state(sid, cs).await.unwrap();
            }
        }
    };

    // 1. Higher cap % (80% > 50%): cycles without draining target
    set_caps(4_000.0, 4_000.0, 150.0).await;
    let _ = sys.add_target_lock(s_norm, eve_destiny::TargetLock { target_id: s_tgt, locked: true, remaining_lock_ms: 0.0, lock_time_ms: 0.0, max_range_m: Some(100_000.0) }).await;
    ctx.dispatch_rpc(&mut sess_norm, "dogmaIM", None, "Activate", &[PyRep::Integer(nos_mod), PyRep::String("energyDestabilizationNew".into()), PyRep::Integer(s_tgt), PyRep::Integer(1)], &kw, None).await.expect("Activate Nosferatu");
    let _ = sys.step_manual(15.0).await;
    let tgt_high = sys.get_combat_state(s_tgt).await.unwrap().unwrap();
    let src_high = sys.get_combat_state(s_norm).await.unwrap().unwrap();
    let ctrl_high = sys.get_combat_state(s_ctrl).await.unwrap().unwrap();
    assert!(tgt_high.capacitor >= 150.0, "Higher cap % (80% > 50%) must not drain target");
    assert!((src_high.capacitor - ctrl_high.capacitor).abs() < 1e-3, "Higher cap % must transfer 0 GJ to activator");

    // 2. Equal cap % (100% == 100%): must not drain
    set_caps(5_000.0, 5_000.0, 300.0).await;
    let _ = sys.step_manual(15.0).await;
    let tgt_eq = sys.get_combat_state(s_tgt).await.unwrap().unwrap();
    assert!(tgt_eq.capacitor >= 300.0 - 1e-3, "Equal cap % must not drain target");

    // 3. Lower cap % (80% < 95% and 400 GJ < 950 GJ): drains target AND transfers capacitor
    for (sid, max_c, cur_c) in [(s_norm, 500.0, 400.0), (s_ctrl, 500.0, 400.0), (s_tgt, 1_000.0, 950.0)] {
        let mut cs = sys.get_combat_state(sid).await.unwrap().unwrap();
        cs.capacitor_recharge_time_ms = 0.0;
        cs.max_capacitor = max_c;
        cs.capacitor = cur_c;
        sys.set_combat_state(sid, cs).await.unwrap();
    }
    let _ = sys.step_manual(15.0).await;
    assert!(sys.get_combat_state(s_tgt).await.unwrap().unwrap().capacitor < 950.0, "Lower cap % must drain target");
    assert!(sys.get_combat_state(s_norm).await.unwrap().unwrap().capacitor > sys.get_combat_state(s_ctrl).await.unwrap().unwrap().capacitor + 5.0);

    // 4. Blood Raider hull (Bhaalgorn 17920) at 85% > 50%: overrides % restriction and drains target
    let _ = sys.remove_module_cycle(s_norm, nos_mod).await;
    let c_br = 952_103;
    let s_br = 962_103;
    let mut sess_br = seed_pilot(&ctx, c_br, s_br, 17920, NULL_SYS, false);
    spawn_ship_in_space(&ctx, c_br, s_br, 17920, NULL_SYS, DVec3::new(1_000.0, 0.0, 0.0)).await;
    set_skill(&ctx, c_br, 3332, 5);
    set_skill(&ctx, c_br, 3335, 5);
    set_skill(&ctx, c_br, 3423, 5);
    let nos_br = 972_102;
    give_item(&ctx, nos_br, 12261, c_br, s_br, InventoryFlag::HiSlot0, 1);

    let c_br_ctrl = 952_104;
    let s_br_ctrl = 962_104;
    seed_pilot(&ctx, c_br_ctrl, s_br_ctrl, 17920, NULL_SYS, false);
    spawn_ship_in_space(&ctx, c_br_ctrl, s_br_ctrl, 17920, NULL_SYS, DVec3::new(1_000.0, 5_000.0, 0.0)).await;

    for (sid, max_c, cur_c) in [(s_br, 5_000.0, 4_250.0), (s_br_ctrl, 5_000.0, 4_250.0), (s_tgt, 300.0, 150.0)] {
        let mut cs = sys.get_combat_state(sid).await.unwrap().unwrap();
        cs.max_capacitor = max_c;
        cs.capacitor = cur_c;
        sys.set_combat_state(sid, cs).await.unwrap();
    }
    let _ = sys.add_target_lock(s_br, eve_destiny::TargetLock { target_id: s_tgt, locked: true, remaining_lock_ms: 0.0, lock_time_ms: 0.0, max_range_m: Some(100_000.0) }).await;
    ctx.dispatch_rpc(&mut sess_br, "dogmaIM", None, "Activate", &[PyRep::Integer(nos_br), PyRep::String("energyDestabilizationNew".into()), PyRep::Integer(s_tgt), PyRep::Integer(1)], &kw, None).await.expect("Activate Bhaalgorn Nosferatu");
    let _ = sys.step_manual(15.0).await;
    let tgt_br = sys.get_combat_state(s_tgt).await.unwrap().unwrap();
    let br_after = sys.get_combat_state(s_br).await.unwrap().unwrap();
    let br_ctrl_after = sys.get_combat_state(s_br_ctrl).await.unwrap().unwrap();
    assert!(tgt_br.capacitor < 150.0, "Blood Raider role bonus must override cap % restriction and drain target");
    assert!(br_after.capacitor > br_ctrl_after.capacitor + 5.0, "Blood Raider ship gains capacitor above idle control");
}

#[tokio::test]
async fn test_modules_mobile_depot_deployment_and_refitting() {

    let ctx = build_ctx();
    let sys = ctx.systems.get_or_spawn(NULL_SYS);
    let kw = HashMap::new();

    let char_id = 952_201;
    let ship_id = 962_201;
    let mut sess = seed_pilot(&ctx, char_id, ship_id, 641, NULL_SYS, false);
    spawn_ship_in_space(&ctx, char_id, ship_id, 641, NULL_SYS, DVec3::new(100_000.0, 0.0, 0.0)).await;
    for &(sk, lvl) in &[(3336, 5), (3300, 5), (3303, 5), (12213, 5)] {
        set_skill(&ctx, char_id, sk, lvl);
    }

    // 1. Spacing restrictions: reject deployment near stargate (<50km) and celestial (<5km)
    let gate = ctx.sde.stargates().values().find(|j| j.from_solar_system_id == NULL_SYS).expect("SDE stargate");
    let depot_item = 972_201;
    give_item(&ctx, depot_item, 33474, char_id, ship_id, InventoryFlag::Cargo, 1);
    sys.try_set_ball_position(ship_id, gate.position + DVec3::new(1_000.0, 0.0, 0.0));
    let near_gate = gate.position + DVec3::new(1_200.0, 0.0, 0.0);
    assert!(ctx.dispatch_rpc(&mut sess, "posMgr", None, "AnchorStructure", &[PyRep::Integer(depot_item), PyRep::Tuple(vec![PyRep::Real(near_gate.x), PyRep::Real(near_gate.y), PyRep::Real(near_gate.z)])], &kw, None).await.is_err(), "Deployment near stargate must fail");

    let cel_pos = ctx.sde.celestials_in_system(NULL_SYS).into_iter().find(|c| c.group_id == 7).unwrap().position + DVec3::new(2_000.0, 0.0, 0.0);
    sys.try_set_ball_position(ship_id, cel_pos);
    assert!(ctx.dispatch_rpc(&mut sess, "posMgr", None, "AnchorStructure", &[PyRep::Integer(depot_item), PyRep::Tuple(vec![PyRep::Real(cel_pos.x), PyRep::Real(cel_pos.y), PyRep::Real(cel_pos.z)])], &kw, None).await.is_err(), "Deployment <5km from celestial must fail");

    // 2. Launch >= 5km away: 60s anchoring countdown
    let dep_pos = DVec3::new(100_000.0, 0.0, 0.0);
    sys.try_set_ball_position(ship_id, dep_pos);
    let target_pos = dep_pos + DVec3::new(500.0, 0.0, 0.0);
    let dep_res = ctx
        .dispatch_rpc(&mut sess, "posMgr", None, "AnchorStructure", &[PyRep::Integer(depot_item), PyRep::Tuple(vec![PyRep::Real(target_pos.x), PyRep::Real(target_pos.y), PyRep::Real(target_pos.z)])], &kw, None)
        .await
        .expect("AnchorStructure succeeds");
    assert_eq!(dep_res.dict_get("anchoringDelaySeconds"), Some(&PyRep::Real(60.0)));

    // 5km spacing rule: second Mobile Depot within 5km is rejected
    let depot2_item = 972_202;
    give_item(&ctx, depot2_item, 33474, char_id, ship_id, InventoryFlag::Cargo, 1);
    let too_close = target_pos + DVec3::new(2_000.0, 0.0, 0.0);
    assert!(ctx.dispatch_rpc(&mut sess, "posMgr", None, "AnchorStructure", &[PyRep::Integer(depot2_item), PyRep::Tuple(vec![PyRep::Real(too_close.x), PyRep::Real(too_close.y), PyRep::Real(too_close.z)])], &kw, None).await.is_err());

    // Cannot refit while anchoring
    let mod_item = 972_210;
    give_item(&ctx, mod_item, 3041, char_id, ship_id, InventoryFlag::Cargo, 1);
    assert!(ctx.dispatch_rpc(&mut sess, "invbroker", None, "Add", &[PyRep::Integer(mod_item), PyRep::Integer(ship_id), PyRep::Integer(27)], &kw, None).await.is_err(), "Cannot refit while depot is anchoring");

    // Advance 61s: anchoring completes
    ctx.advance_time_seconds(61);
    let _ = sys.step_manual(1.0).await;
    let st_anchored = ctx.db.with_snapshot(|snap| eve_server::services::mobile_deployables::read_deployable_state_snap(snap).deployables.get(&depot_item).cloned()).unwrap();
    assert!(st_anchored.anchor_completed, "Mobile Depot must complete anchoring after 60s");

    // 3. Refitting within <= 2,500m of anchored Mobile Depot
    // (a) Fit module
    ctx.dispatch_rpc(&mut sess, "invbroker", None, "Add", &[PyRep::Integer(mod_item), PyRep::Integer(ship_id), PyRep::Integer(27)], &kw, None).await.expect("Fit module near Mobile Depot");
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&mod_item).unwrap().flag), InventoryFlag::HiSlot0);

    // (b) Weapons timer blocks refitting
    ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner()).get_or_create(char_id).weapon_timer_until_unix = ctx.now_unix_secs() + 60;
    assert!(ctx.dispatch_rpc(&mut sess, "invbroker", None, "Add", &[PyRep::Integer(mod_item), PyRep::Integer(ship_id), PyRep::Integer(5)], &kw, None).await.is_err(), "Weapons timer blocks refitting");
    ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner()).get_or_create(char_id).weapon_timer_until_unix = 0;

    // (c) Unfit module
    ctx.dispatch_rpc(&mut sess, "invbroker", None, "Add", &[PyRep::Integer(mod_item), PyRep::Integer(ship_id), PyRep::Integer(5)], &kw, None).await.expect("Unfit module near Mobile Depot");
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&mod_item).unwrap().flag), InventoryFlag::Cargo);

    // (d) Fit and destroy rig
    let rig_item = 972_211;
    give_item(&ctx, rig_item, 26302, char_id, ship_id, InventoryFlag::Cargo, 1);
    ctx.dispatch_rpc(&mut sess, "invbroker", None, "Add", &[PyRep::Integer(rig_item), PyRep::Integer(ship_id), PyRep::Integer(92)], &kw, None).await.expect("Fit rig near Mobile Depot");
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&rig_item).unwrap().flag), InventoryFlag::RigSlot0);
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "DestroyFitting", &[PyRep::Integer(rig_item)], &kw, None).await.expect("Destroy rig near Mobile Depot");
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&rig_item)));

    // (e) Stranger pilot cannot refit off personal Mobile Depot
    let stranger_char = 952_205;
    let stranger_ship = 962_205;
    let mut stranger_sess = seed_pilot(&ctx, stranger_char, stranger_ship, 641, NULL_SYS, false);
    spawn_ship_in_space(&ctx, stranger_char, stranger_ship, 641, NULL_SYS, target_pos + DVec3::new(800.0, 0.0, 0.0)).await;
    give_item(&ctx, 972_219, 3041, stranger_char, stranger_ship, InventoryFlag::Cargo, 1);
    assert!(ctx.dispatch_rpc(&mut stranger_sess, "invbroker", None, "Add", &[PyRep::Integer(972_219), PyRep::Integer(stranger_ship), PyRep::Integer(27)], &kw, None).await.is_err(), "Stranger pilot cannot refit off personal depot");

    // 4. Moving > 2,500m away (5,000m): refitting rejected
    let _ = sys.set_ball_position(ship_id, target_pos + DVec3::new(5_000.0, 0.0, 0.0)).await;
    assert!(ctx.dispatch_rpc(&mut sess, "invbroker", None, "Add", &[PyRep::Integer(mod_item), PyRep::Integer(ship_id), PyRep::Integer(27)], &kw, None).await.is_err(), "Refitting > 2,500m must fail");

    // 5. Damaging shield to <= 25% triggers 48h (172,800s) reinforcement
    let now_u = ctx.now_unix_secs();
    let max_sh = sys.get_combat_state(depot_item).await.unwrap().unwrap().max_shield_hp;
    let _ = eve_server::services::station_structure::structure_lifecycle_rpc::apply_damage_structure_or_ball(
        &ctx, &sys, NULL_SYS, char_id, depot_item, DamageVector::new(max_sh * 0.85, 0.0, 0.0, 0.0), 1.0, now_u,
    ).await;
    let st_reinf = ctx.db.with_snapshot(|snap| eve_server::services::mobile_deployables::read_deployable_state_snap(snap).deployables.get(&depot_item).cloned()).unwrap();
    assert!(st_reinf.reinforced_until_unix.unwrap_or(0) >= now_u + 172_800 - 5, "Mobile Depot must enter 48-hour reinforcement");
}

#[tokio::test]
async fn test_modules_nanite_repair_paste_in_space() {
    // parity:feat:modules.nanite-repair-paste-in-space
    eve_testkit::attest(&["feat:modules.nanite-repair-paste-in-space"]);

    let ctx = build_ctx();
    let sys = ctx.systems.get_or_spawn(NULL_SYS);
    let kw = HashMap::new();

    let char_id = 952_301;
    let ship_id = 962_301;
    let mut sess = seed_pilot(&ctx, char_id, ship_id, 641, NULL_SYS, false);
    spawn_ship_in_space(&ctx, char_id, ship_id, 641, NULL_SYS, DVec3::ZERO).await;

    let mod_5m3 = 972_301;  // Small Focused Pulse Laser II (3041, vol = 5 m³, max hp = 40)
    let mod_20m3 = 972_303; // Mega Pulse Laser II (3057, vol = 20 m³, max hp = 40)
    let paste_id = 972_310;
    give_item(&ctx, mod_5m3, 3041, char_id, ship_id, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, mod_20m3, 3057, char_id, ship_id, InventoryFlag::HiSlot1, 1);
    give_item(&ctx, paste_id, 28668, char_id, ship_id, InventoryFlag::Cargo, 500);

    // 1. Fully burned-out module (40.0 / 40.0 HP damage) cannot be repaired in space
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&mod_5m3).unwrap().dynamic_attributes.insert(3, 40.0);
    });
    assert!(
        ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "InitiateModuleRepair", &[PyRep::Integer(mod_5m3)], &kw, None).await.is_err(),
        "Burned-out module (0 HP) cannot be repaired in space with paste"
    );

    // 2. Active module cannot be repaired until deactivated
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&mod_5m3).unwrap().dynamic_attributes.insert(3, 20.0);
        snap.module_active_states.insert(mod_5m3);
    });
    assert!(ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "InitiateModuleRepair", &[PyRep::Integer(mod_5m3)], &kw, None).await.is_err(), "Active module cannot be repaired");
    ctx.db.with_mut_snapshot(|snap| { snap.module_active_states.remove(&mod_5m3); });

    // 3. Base skills (Nanite Op 0, Nanite Interfacing 0): repair rate = 10.0 HP/s, cost = 10 paste for 20 HP
    set_skill(&ctx, char_id, 28164, 0); // Thermodynamics 0
    set_skill(&ctx, char_id, 28879, 0); // Nanite Operation 0
    set_skill(&ctx, char_id, 28880, 0); // Nanite Interfacing 0
    let rep_init = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "InitiateModuleRepair", &[PyRep::Integer(mod_5m3)], &kw, None).await.expect("Repair 5m3");
    let c_init = rep_init.dict_get("totalPasteRequired").and_then(|v| v.as_i64()).unwrap();
    let rate_init = rep_init.dict_get("repairRateHpPerSec").and_then(|v| v.as_f64()).unwrap();
    assert_eq!(c_init, 10, "Base paste cost = 10 for 20 HP damage");
    assert!((rate_init - 10.0).abs() < 1e-3, "Base repair rate = 10.0 HP/s");
    let _ = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "StopModuleRepair", &[PyRep::Integer(mod_5m3)], &kw, None).await;

    // 4. Thermodynamics V does NOT reduce paste cost
    set_skill(&ctx, char_id, 28164, 5);
    ctx.db.with_mut_snapshot(|snap| { snap.items.get_mut(&mod_20m3).unwrap().dynamic_attributes.insert(3, 20.0); });
    let rep_thermo = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "InitiateModuleRepair", &[PyRep::Integer(mod_20m3)], &kw, None).await.expect("Repair with Thermo V");
    let c_thermo = rep_thermo.dict_get("totalPasteRequired").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(c_thermo, 10, "Thermodynamics V does not reduce paste cost");
    let _ = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "StopModuleRepair", &[PyRep::Integer(mod_20m3)], &kw, None).await;

    // 5. Nanite Operation V (-5%/lvl -> -25% paste cost: 10 -> 8)
    //    Nanite Interfacing V (+20%/lvl -> +100% rate: 10.0 -> 20.0 HP/s)
    set_skill(&ctx, char_id, 28879, 5);
    set_skill(&ctx, char_id, 28880, 5);
    ctx.db.with_mut_snapshot(|snap| { snap.items.get_mut(&mod_20m3).unwrap().dynamic_attributes.insert(3, 20.0); });
    let rep_skills = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "InitiateModuleRepair", &[PyRep::Integer(mod_20m3)], &kw, None).await.expect("Repair with Nanite skills V");
    let c_skills = rep_skills.dict_get("totalPasteRequired").and_then(|v| v.as_i64()).unwrap();
    let rate_skills = rep_skills.dict_get("repairRateHpPerSec").and_then(|v| v.as_f64()).unwrap();
    assert_eq!(c_skills, 8, "Nanite Operation V reduces paste cost by 25%");
    assert!((rate_skills - 20.0).abs() < 1e-2, "Nanite Interfacing V doubles repair rate to 20.0 HP/s");

    // Partial repair & stop: step 0.5s -> StopModuleRepair
    let _ = sys.step_manual(0.5).await;
    let stop_rep = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "StopModuleRepair", &[PyRep::Integer(mod_20m3)], &kw, None).await.expect("StopModuleRepair");
    let rem_dmg = stop_rep.dict_get("remainingDamage").and_then(|v| v.as_f64()).unwrap();
    assert!(rem_dmg < 20.0 && rem_dmg > 0.0, "Partial repair reduces damage: {rem_dmg}");
}

#[tokio::test]
async fn test_modules_disruptive_lance() {
    // parity:feat:modules.disruptive-lance
    eve_testkit::attest(&["feat:modules.disruptive-lance"]);

    let ctx = build_ctx();
    let _sys = ctx.systems.get_or_spawn(NULL_SYS);
    let kw = HashMap::new();

    // Lancer Dreadnought: Bane (77283)
    let char_id = 952_401;
    let ship_id = 962_401;
    let mut sess = seed_pilot(&ctx, char_id, ship_id, 77283, NULL_SYS, false);
    let siege_mod = 972_401; // Siege Module II (4292)
    let lance_mod = 972_402; // Disruptive Lance (77399)
    let cloak_mod = 972_403; // Cloak (11370)
    give_item(&ctx, siege_mod, 4292, char_id, ship_id, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, lance_mod, 77399, char_id, ship_id, InventoryFlag::HiSlot1, 1);
    give_item(&ctx, cloak_mod, 11370, char_id, ship_id, InventoryFlag::HiSlot2, 1);
    give_item(&ctx, 972_404, 16275, char_id, ship_id, InventoryFlag::Cargo, 500); // Strontium
    give_item(&ctx, 972_405, 16274, char_id, ship_id, InventoryFlag::Cargo, 30_000); // Isotopes
    spawn_ship_in_space(&ctx, char_id, ship_id, 77283, NULL_SYS, DVec3::ZERO).await;

    // 1. Lance fitted disables cloaks
    let cloak_err = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(cloak_mod)], &kw, None).await.unwrap_err();
    assert_eq!(cloak_err.code, "CannotCloakWithLanceFitted");

    // 2. Disruptive Lance rejected before Siege is active
    let no_siege_err = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(lance_mod), PyRep::String("default".into()), PyRep::Integer(1)], &kw, None).await.unwrap_err();
    assert_eq!(no_siege_err.code, "SiegeRequiredForLance");

    // Spawn victim ship in corridor at (50km, 0, 0) and bystander at (50km, 10km, 0)
    let v_char = 952_402;
    let v_ship = 962_402;
    let mut v_sess = seed_pilot(&ctx, v_char, v_ship, 19724, NULL_SYS, false);
    spawn_ship_in_space(&ctx, v_char, v_ship, 19724, NULL_SYS, DVec3::new(50_000.0, 0.0, 0.0)).await;

    let b_char = 952_403;
    let b_ship = 962_403;
    seed_pilot(&ctx, b_char, b_ship, 624, NULL_SYS, false);
    spawn_ship_in_space(&ctx, b_char, b_ship, 624, NULL_SYS, DVec3::new(50_000.0, 10_000.0, 0.0)).await;

    // Activate Siege first
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(siege_mod)], &kw, None).await.expect("Activate Siege");

    // 3. Fire Disruptive Lance at victim
    let lance_res = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(lance_mod), PyRep::String("default".into()), PyRep::Integer(v_ship)], &kw, None)
        .await
        .expect("Fire Disruptive Lance");
    assert_eq!(lance_res.dict_get("hitCount").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(lance_res.dict_get("casterLockSeconds").and_then(|v| v.as_i64()), Some(300), "Firing Lancer locked for 5 min");
    assert_eq!(lance_res.dict_get("victimDebuffSeconds").and_then(|v| v.as_i64()), Some(60));
    assert_eq!(lance_res.dict_get("remoteRepairImpedance").and_then(|v| v.as_f64()), Some(0.50), "Halves incoming remote repair");

    // 4. Affected ship cannot warp (debuff active)
    let beyonce_id = ctx.bind_object(&mut v_sess, "beyonce", NULL_SYS as i64).unwrap();
    let warp_err = ctx.dispatch_rpc(&mut v_sess, "beyonce", Some(beyonce_id), "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(40000001)], &kw, None).await.unwrap_err();
    assert_eq!(warp_err.code, "DisruptiveLanceDebuffActive");
}

#[tokio::test]
async fn test_modules_lance_doomsday_directional_geometry() {
    // parity:feat:modules.lance-doomsday-directional-geometry
    eve_testkit::attest(&["feat:modules.lance-doomsday-directional-geometry"]);

    let ctx = build_ctx();
    let sys = ctx.systems.get_or_spawn(NULL_SYS);
    let kw = HashMap::new();

    let titan_char = 952_501;
    let titan_ship = 962_501;
    let mut titan_sess = seed_pilot(&ctx, titan_char, titan_ship, 11567, NULL_SYS, false); // Avatar
    spawn_ship_in_space(&ctx, titan_char, titan_ship, 11567, NULL_SYS, DVec3::ZERO).await;
    let titan_beyonce = ctx.bind_object(&mut titan_sess, "beyonce", NULL_SYS as i64).unwrap();

    let lance_mod = 972_501; // Titan Lance Doomsday (40631)
    give_item(&ctx, lance_mod, 40631, titan_char, titan_ship, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, 972_502, 16274, titan_char, titan_ship, InventoryFlag::Cargo, 500_000);

    let origin_arg = PyRep::Tuple(vec![PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(0.0)]);
    let dir_arg = PyRep::Tuple(vec![PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)]);

    // Target in beam at 200km
    let v_char = 952_502;
    let v_ship = 962_502;
    seed_pilot(&ctx, v_char, v_ship, 23757, NULL_SYS, false); // Archon
    spawn_ship_in_space(&ctx, v_char, v_ship, 23757, NULL_SYS, DVec3::new(200_000.0, 0.0, 0.0)).await;

    // Target beyond range at 350km
    let out_char = 952_503;
    let out_ship = 962_503;
    seed_pilot(&ctx, out_char, out_ship, 641, NULL_SYS, false);
    spawn_ship_in_space(&ctx, out_char, out_ship, 641, NULL_SYS, DVec3::new(350_000.0, 0.0, 0.0)).await;

    // Target off-axis
    let off_char = 952_504;
    let off_ship = 962_504;
    seed_pilot(&ctx, off_char, off_ship, 641, NULL_SYS, false);
    spawn_ship_in_space(&ctx, off_char, off_ship, 641, NULL_SYS, DVec3::new(100_000.0, 50_000.0, 0.0)).await;

    let cs_v_init = sys.get_combat_state(v_ship).await.unwrap().unwrap();
    let cs_out_init = sys.get_combat_state(out_ship).await.unwrap().unwrap();
    let cs_off_init = sys.get_combat_state(off_ship).await.unwrap().unwrap();

    // 1. Activate Titan Lance Doomsday
    let slash_res = ctx
        .dispatch_rpc(&mut titan_sess, "superWeaponMgr", None, "ActivateSlashModule", &[PyRep::Integer(lance_mod), origin_arg, dir_arg], &kw, None)
        .await
        .expect("ActivateSlashModule");
    assert_eq!(slash_res.dict_get("warmupSeconds"), Some(&PyRep::Real(15.0)));
    assert_eq!(slash_res.dict_get("jumpBlockSeconds"), Some(&PyRep::Real(300.0)), "5-minute jump/warp suppression debuff");

    // 2. During 15s warmup: Titan cannot move, steer, or warp
    assert!(ctx.dispatch_rpc(&mut titan_sess, "beyonce", Some(titan_beyonce), "CmdGotoDirection", &[PyRep::Real(0.0), PyRep::Real(1.0), PyRep::Real(0.0)], &kw, None).await.is_err());
    assert!(ctx.dispatch_rpc(&mut titan_sess, "beyonce", Some(titan_beyonce), "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(40000001), PyRep::Real(0.0)], &kw, None).await.is_err());

    // Step 12s (< 15s warmup): no damage yet
    for _ in 0..12 {
        let _ = sys.try_step_full_tick(1.0);
    }
    let cs_v_mid = sys.get_combat_state(v_ship).await.unwrap().unwrap();
    assert_eq!(cs_v_mid.shield_hp, cs_v_init.shield_hp, "No damage before 15s warmup finishes");

    // Step remaining ticks to fire beam (> 15s)
    for _ in 0..10 {
        let _ = sys.try_step_full_tick(1.0);
    }
    let cs_v_after = sys.get_combat_state(v_ship).await.unwrap().unwrap();
    let cs_out_after = sys.get_combat_state(out_ship).await.unwrap().unwrap();
    let cs_off_after = sys.get_combat_state(off_ship).await.unwrap().unwrap();

    assert!(cs_v_after.shield_hp < cs_v_init.shield_hp, "Ship in beam cone takes massive damage");
    assert_eq!(cs_out_after.shield_hp, cs_out_init.shield_hp, "Ship beyond 300km takes 0 damage");
    assert_eq!(cs_off_after.shield_hp, cs_off_init.shield_hp, "Off-axis ship takes 0 damage");

    // Titan caster cannot warp during 300s lock
    assert!(ctx.dispatch_rpc(&mut titan_sess, "beyonce", Some(titan_beyonce), "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(40000001), PyRep::Real(0.0)], &kw, None).await.is_err());
}
