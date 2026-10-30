//! Integration tests for modules, ships, and overheat closure rows.
//!
//! parity: feat:modules.emergency-hull-energizer, feat:modules.overheat-burnout-state

use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode, TargetLock};
use eve_dogma::attr_ids;
use eve_machonet::SessionState;
use eve_proto::PyRep;
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

fn make_test_ball(id: i64, type_id: u32, position: DVec3, radius: f64) -> Ball {
    Ball {
        id, type_id, position, velocity: DVec3::ZERO, max_velocity: 150.0,
        inertia: 1.0, mass: 10_000_000.0, radius, speed_fraction: 0.0, mode: MovementMode::Stop,
    }
}

#[tokio::test]
async fn test_emergency_hull_energizer_and_overheat_burnout_state() {
    // parity: feat:modules.emergency-hull-energizer, feat:modules.overheat-burnout-state
    eve_testkit::attest(&["feat:modules.emergency-hull-energizer", "feat:modules.overheat-burnout-state"]);

    let ctx = new_ctx();
    let kw = HashMap::new();
    let mut sess = SessionState::new(8801);
    crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

    let char_id = 90_000_001i64;
    let ship_id = 100_000_101i64;
    let sys_id = 30_000_142u32;
    let sys = ctx.systems.get_or_spawn(sys_id);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = ship_id;
            ch.station_id = None;
            ch.solar_system_id = sys_id;
        }
    });

    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None).await.expect("SelectCharacterID");

    // 1. Capital-only restriction check: Battleship Raven (group 27, type 638) cannot activate Capital EHE
    let raven_id = 100_000_109i64;
    let raven_ehe_id = 881_099i64;
    seed_item(&ctx, raven_id, 638, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    seed_item(&ctx, raven_ehe_id, 40715, char_id, raven_id, InventoryFlag::LoSlot0, 1, true);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(raven_ehe_id, true);
        if let Some(ch) = snap.characters.get_mut(&char_id) { ch.active_ship_id = raven_id; }
    });
    sess.ship_id = Some(raven_id);
    ctx.invalidate_ship_fitting(raven_id);
    let raven_ball = make_test_ball(raven_id, 638, DVec3::new(20_000.0, 0.0, 0.0), 100.0);
    let raven_cs = ctx.build_ship_combat_state(char_id, raven_id, 638);
    let raven_meta = ctx.build_ship_slim_meta(char_id, 638);
    let _ = sys.spawn_ball_with_meta(raven_ball, raven_cs, raven_meta).await;

    let non_cap_err = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(raven_ehe_id), PyRep::String("emergencyHullEnergizer".into())], &kw, None).await.unwrap_err();
    assert_eq!(non_cap_err.code, "InvalidShipGroup", "Capital EHE on Battleship must fail with InvalidShipGroup");

    // 2. Setup Archon (Carrier, group 547, type 23757) with Compact EHE (40715) and 500MN MWD (439)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) { ch.active_ship_id = ship_id; }
    });
    sess.ship_id = Some(ship_id);

    seed_item(&ctx, ship_id, 23757, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    let ehe_mod_id = 881_001i64;
    let mwd_mod_id = 881_002i64;
    // Use Compact EHE variant 40715 (effect 6484) to verify non-T1/T2 variant handling
    seed_item(&ctx, ehe_mod_id, 40715, char_id, ship_id, InventoryFlag::LoSlot0, 1, true);
    seed_item(&ctx, mwd_mod_id, 439, char_id, ship_id, InventoryFlag::MedSlot0, 1, true);

    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(ehe_mod_id, true);
        snap.module_online_states.insert(mwd_mod_id, true);
    });
    ctx.invalidate_ship_fitting(ship_id);

    let ball = make_test_ball(ship_id, 23757, DVec3::new(10_000.0, 0.0, 0.0), 200.0);
    let cs = ctx.build_ship_combat_state(char_id, ship_id, 23757);
    let meta = ctx.build_ship_slim_meta(char_id, 23757);
    let _ = sys.spawn_ball_with_meta(ball, cs, meta).await;

    // Record pre-activation base structure resonance (Archon base: 0.67 across damage types)
    let pre_res = sys.get_combat_state(ship_id).await.unwrap().expect("Ship combat state").structure_resonance;
    assert!(pre_res[0] > 0.50, "Base hull resonance must be unbuffed before EHE");

    // Activate Capital Emergency Hull Energizer (`feat:modules.emergency-hull-energizer`)
    let ehe_rep = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(ehe_mod_id), PyRep::String("emergencyHullEnergizer".into())], &kw, None).await.expect("Activate EHE");
    assert_eq!(ehe_rep.dict_get("active").and_then(|v| v.as_bool()), Some(true));

    // Assert cycle duration (e.g. 14-25 seconds for capital EHE variants)
    let duration_s = ehe_rep.dict_get("duration").and_then(|v| v.as_f64()).unwrap_or_else(|| {
        ctx.sde.types.get(&40715).and_then(|t| t.default_attributes.get(&attr_ids::DURATION).copied()).unwrap_or(0.0) / 1000.0
    });
    assert!(duration_s >= 14.0 && duration_s <= 25.0, "EHE duration must be short burst (14-25s), got {duration_s}");

    // Assert that active EHE grants 95% resistance to all hull damage layers (0.05 resonance)
    let active_res = sys.get_combat_state(ship_id).await.unwrap().expect("Active combat state").structure_resonance;
    assert_eq!(active_res, [0.05, 0.05, 0.05, 0.05], "Active EHE must grant 95% hull resistance");

    let (em_res, exp_res, kin_res, therm_res) = ctx.db.with_snapshot(|snap| {
        let tr = snap.transient_item_attributes.get(&ship_id);
        let em = tr.and_then(|s| s.get(&113).copied()).unwrap_or(0.0);
        let exp = tr.and_then(|s| s.get(&111).copied()).unwrap_or(0.0);
        let kin = tr.and_then(|s| s.get(&109).copied()).unwrap_or(0.0);
        let therm = tr.and_then(|s| s.get(&110).copied()).unwrap_or(0.0);
        (em, exp, kin, therm)
    });
    assert_eq!(em_res, 0.05, "Ship EM hull resonance attribute must be 0.05");
    assert_eq!(exp_res, 0.05, "Ship Explosive hull resonance attribute must be 0.05");
    assert_eq!(kin_res, 0.05, "Ship Kinetic hull resonance attribute must be 0.05");
    assert_eq!(therm_res, 0.05, "Ship Thermal hull resonance attribute must be 0.05");

    // Step cycle duration (26 ticks) so EHE completes cycle and burns out
    for _ in 0..26 { sys.tick().await.unwrap(); }

    // Verify EHE burnout state: burned out (damage == max HP), offlined, and deactivated
    let (is_online, is_active, dmg, max_hp) = ctx.db.with_snapshot(|snap| {
        let on = snap.module_online_states.get(&ehe_mod_id).copied().unwrap_or(false);
        let act = snap.module_active_states.contains(&ehe_mod_id);
        let it = snap.items.get(&ehe_mod_id);
        let cur_d = it.and_then(|i| i.dynamic_attributes.get(&attr_ids::DAMAGE).copied()).unwrap_or(0.0);
        let m_hp = it.and_then(|i| i.dynamic_attributes.get(&attr_ids::HP_STRUCT).copied()).unwrap_or(40.0);
        (on, act, cur_d, m_hp)
    });
    assert!(!is_online, "Burned-out EHE must be offlined automatically");
    assert!(!is_active, "Burned-out EHE must be deactivated");
    assert!(dmg >= max_hp - 1e-4, "Burned-out EHE damage ({dmg}) must equal max HP ({max_hp})");

    // Structure resonance must be restored after EHE burns out
    let post_res = sys.get_combat_state(ship_id).await.unwrap().expect("Post combat state").structure_resonance;
    assert_eq!(post_res, pre_res, "Resistances must revert after EHE deactivates");

    // Cannot reactivate burned-out EHE in space
    let react_err = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(ehe_mod_id), PyRep::String("emergencyHullEnergizer".into()), PyRep::Integer(1)], &kw, None).await.unwrap_err();
    assert_eq!(react_err.code, "ModuleBurnedOut");

    // Fully burned-out module refuses nanite repair in space
    let rep_err = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "RepairModule", &[PyRep::Integer(ship_id), PyRep::Integer(ehe_mod_id)], &kw, None).await.unwrap_err();
    assert_eq!(rep_err.code, "ModuleBurnedOut");

    // Cannot refit damaged or burned-out EHE: moving to cargo or fitting from cargo is rejected
    let unrefit_err = ctx.dispatch_rpc(&mut sess, "invbroker", None, "Add", &[PyRep::Integer(ehe_mod_id), PyRep::Integer(ship_id), PyRep::Integer(0)], &kw, None).await.unwrap_err();
    assert_eq!(unrefit_err.code, "CannotRefitDamagedModule", "Unfitting damaged EHE must fail");

    let damaged_cargo_ehe = 881_098i64;
    seed_item(&ctx, damaged_cargo_ehe, 40715, char_id, ship_id, InventoryFlag::Cargo, 1, true);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&damaged_cargo_ehe) {
            it.dynamic_attributes.insert(attr_ids::DAMAGE, 10.0);
            it.dynamic_attributes.insert(attr_ids::HP_STRUCT, 40.0);
        }
    });
    let fit_damaged_err = ctx.dispatch_rpc(&mut sess, "invbroker", None, "Add", &[PyRep::Integer(damaged_cargo_ehe), PyRep::Integer(ship_id), PyRep::Integer(12)], &kw, None).await.unwrap_err();
    assert_eq!(fit_damaged_err.code, "CannotRefitDamagedModule", "Fitting damaged EHE must fail");

    // 3. Heat-driven module burnout on overloaded module (`feat:modules.overheat-burnout-state`)
    // Pre-accumulate heat damage to 38.0 / 40.0 HP on the 500MN MWD
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(m_item) = snap.items.get_mut(&mwd_mod_id) {
            m_item.dynamic_attributes.insert(attr_ids::DAMAGE, 38.0);
            m_item.dynamic_attributes.insert(attr_ids::HP_STRUCT, 40.0);
        }
    });

    let ovl_rep = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Overload", &[PyRep::Integer(mwd_mod_id)], &kw, None).await.expect("Overload MWD");
    assert_eq!(ovl_rep.dict_get("overloaded").and_then(|v| v.as_bool()), Some(true));

    // Activate MWD in overload mode
    let _ = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(mwd_mod_id), PyRep::String("propulsion".into())], &kw, None).await.expect("Activate overloaded MWD");

    // Step several cycles so heat damage accumulates past 100% of hitpoints (40 HP)
    for _ in 0..15 { sys.tick().await.unwrap(); }

    // Verify heat-driven burnout: module immediately deactivates, goes offline, damage == max HP
    let (mwd_online, mwd_active, mwd_dmg, mwd_max_hp) = ctx.db.with_snapshot(|snap| {
        let on = snap.module_online_states.get(&mwd_mod_id).copied().unwrap_or(false);
        let act = snap.module_active_states.contains(&mwd_mod_id);
        let it = snap.items.get(&mwd_mod_id);
        let cur_d = it.and_then(|i| i.dynamic_attributes.get(&attr_ids::DAMAGE).copied()).unwrap_or(0.0);
        let m_hp = it.and_then(|i| i.dynamic_attributes.get(&attr_ids::HP_STRUCT).copied()).unwrap_or(40.0);
        (on, act, cur_d, m_hp)
    });
    assert!(!mwd_online, "Heat-burned module must be taken offline");
    assert!(!mwd_active, "Heat-burned module must be deactivated");
    assert!(mwd_dmg >= mwd_max_hp - 1e-4, "Heat-burned module damage ({mwd_dmg}) must equal max HP ({mwd_max_hp})");

    // Reactivation of heat-burned module in space must fail with ModuleBurnedOut
    let mwd_react_err = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(mwd_mod_id), PyRep::String("propulsion".into())], &kw, None).await.unwrap_err();
    assert_eq!(mwd_react_err.code, "ModuleBurnedOut");

    // Online & offline attempts on burned-out module in space must fail with ModuleBurnedOut
    let online_err = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "SetModuleOnline", &[PyRep::Integer(ship_id), PyRep::Integer(mwd_mod_id)], &kw, None).await.unwrap_err();
    assert_eq!(online_err.code, "ModuleBurnedOut", "Onlining burned module in space must fail");

    let offline_err = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "TakeModuleOffline", &[PyRep::Integer(ship_id), PyRep::Integer(mwd_mod_id)], &kw, None).await.unwrap_err();
    assert_eq!(offline_err.code, "ModuleBurnedOut", "Offlining burned module in space must fail");

    // 4. Module group / weapon bank disabling when containing a burned module (`feat:modules.overheat-burnout-state`)
    // Fit two 250mm Light Artillery Cannon I (487) with Fusion S ammo (183) in high slots and link them via LinkWeapons RPC
    let gun1_id = 881_003i64;
    let gun2_id = 881_004i64;
    let ammo1_id = 881_005i64;
    let ammo2_id = 881_006i64;
    seed_item(&ctx, gun1_id, 487, char_id, ship_id, InventoryFlag::HiSlot0, 1, true);
    seed_item(&ctx, gun2_id, 487, char_id, ship_id, InventoryFlag::HiSlot1, 1, true);
    seed_item(&ctx, ammo1_id, 183, char_id, ship_id, InventoryFlag::HiSlot0, 100, false);
    seed_item(&ctx, ammo2_id, 183, char_id, ship_id, InventoryFlag::HiSlot1, 100, false);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(gun1_id, true);
        snap.module_online_states.insert(gun2_id, true);
    });
    ctx.invalidate_ship_fitting(ship_id);

    // Target lock raven_id for weapon activation
    let _ = sys.add_target_lock(ship_id, TargetLock {
        target_id: raven_id, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(100_000.0),
    }).await;

    // Form weapon group using client RPC LinkWeapons
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "LinkWeapons", &[PyRep::Integer(ship_id), PyRep::Integer(gun1_id), PyRep::Integer(gun2_id)], &kw, None).await.expect("LinkWeapons");

    // Burn out gun2
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&gun2_id) {
            it.dynamic_attributes.insert(attr_ids::DAMAGE, 40.0);
            it.dynamic_attributes.insert(attr_ids::HP_STRUCT, 40.0);
        }
    });

    let group_err = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(gun1_id), PyRep::String("projectileFired".into()), PyRep::Integer(raven_id)], &kw, None).await.unwrap_err();
    assert_eq!(group_err.code, "ModuleBurnedOut");
    assert!(
        group_err.message.contains("disabled until repaired or ungrouped"),
        "Error message must specify group disabling until repaired or ungrouped, got: {}",
        group_err.message
    );

    // Ungrouping permits activating unburned module
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "UnlinkAllModules", &[PyRep::Integer(ship_id)], &kw, None).await.expect("UnlinkAllModules");
    let ungrouped_res = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(gun1_id), PyRep::String("projectileFired".into()), PyRep::Integer(raven_id)], &kw, None).await;
    assert!(ungrouped_res.is_ok(), "Ungrouped healthy module can be activated");

    // Repairing burned module also re-enables grouped activation
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&gun2_id) {
            it.dynamic_attributes.insert(attr_ids::DAMAGE, 0.0);
        }
    });
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "LinkWeapons", &[PyRep::Integer(ship_id), PyRep::Integer(gun1_id), PyRep::Integer(gun2_id)], &kw, None).await.expect("LinkWeapons re-link");
    let repaired_group_res = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(gun1_id), PyRep::String("projectileFired".into()), PyRep::Integer(raven_id)], &kw, None).await;
    assert!(repaired_group_res.is_ok(), "Repaired module group can be activated");
}

