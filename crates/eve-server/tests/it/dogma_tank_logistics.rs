//! Phase 4 Dogma and Fitting integration tests (`DG-2`..`DG-6` and Phase 4 Exit Gate).

use crate::common;

use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, CombatState, DVec3, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::attr_ids;
use eve_server::router::ServerContext;
use std::collections::HashMap;

pub(super) fn seed_item(
    ctx: &ServerContext,
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id,
                owner_id,
                location_id,
                flag,
                quantity,
                singleton,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
}

pub(super) fn grant_all_required_skills_for_type(ctx: &ServerContext, char_id: i64, type_id: u32) {
    let Some(t) = ctx.sde.types.get(&type_id) else {
        return;
    };
    let pairs = [
        (attr_ids::REQUIRED_SKILL_1, attr_ids::REQUIRED_SKILL_1_LEVEL),
        (attr_ids::REQUIRED_SKILL_2, attr_ids::REQUIRED_SKILL_2_LEVEL),
        (attr_ids::REQUIRED_SKILL_3, attr_ids::REQUIRED_SKILL_3_LEVEL),
    ];
    ctx.db.with_mut_snapshot(|snap| {
        let entry = snap.skills.entry(char_id).or_default();
        for (s_attr, l_attr) in pairs {
            if let Some(&sid_f) = t.default_attributes.get(&s_attr) {
                let sid = sid_f as u32;
                if sid > 0 {
                    let lvl = t
                        .default_attributes
                        .get(&l_attr)
                        .copied()
                        .unwrap_or(5.0)
                        .max(5.0) as u8;
                    entry.insert(
                        sid,
                        eve_db::TrainedSkillRecord {
                            type_id: sid,
                            level: lvl,
                            skill_points: 256_000.0,
                        },
                    );
                }
            }
        }
    });
}

pub(super) fn extract_attr_from_info(info_rep: &PyRep, attr_id: i64) -> f64 {
    let attrs = info_rep
        .dict_get("attributes")
        .expect("attributes dict in ItemGetInfo");
    if let PyRep::Dict(entries) = attrs {
        for (k, v) in entries {
            if k.as_i64() == Some(attr_id) {
                return v.as_f64().unwrap_or(0.0);
            }
        }
    }
    0.0
}

#[tokio::test]
async fn test_phase_4_dg4_and_exit_gate_full_cycle_journey() {
    // parity: plan:DG-4, feat:dogma.module-cycle, notify:OnGodmaShipEffect, notify:OnModulesDeactivating, rpc:charUnboundMgr.SelectCharacterID, rpc:dogmaIM.LinkAllWeapons, rpc:dogmaIM.LoadAmmo
    eve_testkit::attest(&["feat:dogma.module-cycle", "notify:OnGodmaShipEffect", "notify:OnModulesDeactivating"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess = SessionState::new(4005);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None).await
        .expect("SelectCharacterID");

    let char_id = 90_000_001i64;
    let ship_id = 100_000_001i64;
    let station_id = 60_003_760i64;
    for tid in [439u32, 2889, 183, 30987] {
        grant_all_required_skills_for_type(&ctx, char_id, tid);
    }
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(entry) = snap.skills.get_mut(&char_id) {
            entry.remove(&3449);
        }
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);

    let ab_id = 100_000_850i64;
    let t1_id = 100_000_005i64;
    let t2_id = 100_000_851i64;
    let rig_id = 100_000_852i64;
    let ammo_id = 100_000_853i64;
    seed_item(&ctx, ab_id, 439, char_id, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, t2_id, 2889, char_id, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, rig_id, 30987, char_id, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, ammo_id, 183, char_id, ship_id, InventoryFlag::Cargo, 500, false);

    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "Add",
        &[PyRep::Integer(ab_id), PyRep::Integer(ship_id), PyRep::Integer(InventoryFlag::MedSlot0 as i64)],
        &kw,
        None,
    )
    .await
    .expect("Fit 1MN Afterburner I");
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "Add",
        &[PyRep::Integer(t2_id), PyRep::Integer(ship_id), PyRep::Integer(InventoryFlag::HiSlot1 as i64)],
        &kw,
        None,
    )
    .await
    .expect("Fit second 200mm AutoCannon II");
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "Add",
        &[PyRep::Integer(rig_id), PyRep::Integer(ship_id), PyRep::Integer(InventoryFlag::RigSlot0 as i64)],
        &kw,
        None,
    )
    .await
    .expect("Fit Small Trimark Armor Pump I");

    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "LinkAllWeapons", &[PyRep::Integer(ship_id)], &kw, None)
        .await
        .expect("LinkAllWeapons");
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "LoadAmmo",
        &[
            PyRep::Integer(ship_id),
            PyRep::List(vec![PyRep::Integer(t1_id)]),
            PyRep::List(vec![PyRep::Integer(ammo_id)]),
            PyRep::Integer(ship_id),
        ],
        &kw,
        None,
    )
    .await
    .expect("LoadAmmo Fusion S");

    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let sys = ctx.systems.get_or_spawn(30_000_142);
    let (_, ship_ball) = sys.get_ball_snapshot(ship_id).await.expect("get_ball_snapshot").expect("ship ball");

    let target_id = 990_000_020i64;
    sys.spawn_ball_with_combat(
        Ball {
            id: target_id,
            type_id: 587,
            position: DVec3::new(ship_ball.position.x + 2_000.0, ship_ball.position.y, ship_ball.position.z),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 3.2,
            mass: 1_350_000.0,
            radius: 35.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        CombatState::default(),
    )
    .await
    .expect("spawn target");

    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "AddTarget", &[PyRep::Integer(target_id)], &kw, None)
        .await
        .expect("AddTarget");
    for _ in 0..4 {
        sys.step_manual(1.0).await.expect("sys.step_manual lock");
    }
    let locks = sys.get_target_locks(ship_id).await.unwrap();
    assert!(locks[0].locked);

    let _ = sess.drain_notifications();
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(ab_id), PyRep::String("moduleBonusAfterburner".into()), PyRep::None, PyRep::Integer(1)],
        &kw,
        None,
    )
    .await
    .expect("Activate 1MN Afterburner I");

    let (_, ship_ball_boosted) = sys.get_ball_snapshot(ship_id).await.unwrap().expect("boosted ship ball");
    let ship_combat_boosted = sys.get_combat_state(ship_id).await.unwrap().expect("ship combat state");
    assert!(ship_ball_boosted.max_velocity > 600.0);
    assert!((ship_combat_boosted.capacitor - 240.0).abs() < 0.1);

    ctx.set_mechanic_rng_seed(Some(10));
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(t1_id), PyRep::String("projectileFired".into()), PyRep::Integer(target_id), PyRep::Integer(1)],
        &kw,
        None,
    )
    .await
    .expect("Activate linked 200mm AutoCannon II");

    let act_notes = sess.drain_notifications();
    let projectile_fired_id = ctx
        .sde
        .effects
        .values()
        .find(|e| e.name == "projectileFired")
        .map(|e| e.effect_id as i64)
        .expect("SDE effect projectileFired");
    let gse = act_notes
        .iter()
        .find(|(n, a)| n == "OnGodmaShipEffect" && a.first() == Some(&PyRep::Integer(t1_id)))
        .expect("OnGodmaShipEffect for the activated turret");
    let a = &gse.1;
    assert_eq!(a.len(), 10, "OnGodmaShipEffect positional args: {a:?}");
    assert_eq!(a[1], PyRep::Integer(projectile_fired_id));
    assert!(matches!(a[2], PyRep::Integer(t) if t > 116_444_736_000_000_000));
    assert_eq!(&a[3..5], &[PyRep::Integer(1), PyRep::Integer(1)], "start, active");
    let PyRep::Tuple(env) = &a[5] else { panic!("environment tuple expected") };
    assert_eq!(env.len(), 7, "ENV_IDX_SELF..ENV_IDX_EFFECT");
    let env_ids = [t1_id, 90_000_001, ship_id, target_id].map(PyRep::Integer);
    assert_eq!(&env[..4], &env_ids, "ENV_IDX_SELF, CHAR, SHIP, TARGET");
    assert_eq!(env[6], PyRep::Integer(projectile_fired_id));
    let fit_after_act = ctx
        .get_or_rebuild_ship_fitting(char_id, ship_id)
        .expect("ship fitting after turret activation");
    let expected_duration = {
        let d = fit_after_act.get_module_effective_attribute(t1_id, 73, &ctx.sde);
        if d > 0.0 {
            d
        } else {
            fit_after_act.get_module_effective_attribute(t1_id, 51, &ctx.sde)
        }
    };
    assert!(expected_duration > 0.0);
    assert_eq!(a[7], PyRep::Real(expected_duration));
    assert_eq!(a[8], PyRep::Integer(1000));
    assert!(act_notes.iter().all(|(n, _)| n != "OnModuleActivated"));

    let charges_after_init: Vec<i32> = ctx.db.with_snapshot(|s| {
        s.items
            .values()
            .filter(|i| i.location_id == ship_id && i.type_id == 183 && (i.flag as u16 == 27 || i.flag as u16 == 28))
            .map(|i| i.quantity)
            .collect()
    });
    assert_eq!(charges_after_init, vec![119, 119]);

    for _ in 0..4 {
        sys.step_manual(1.0).await.expect("sys.step_manual turret cycle");
    }
    let charges_after_cycle: Vec<i32> = ctx.db.with_snapshot(|s| {
        s.items
            .values()
            .filter(|i| i.location_id == ship_id && i.type_id == 183 && (i.flag as u16 == 27 || i.flag as u16 == 28))
            .map(|i| i.quantity)
            .collect()
    });
    assert_eq!(charges_after_cycle, vec![118, 118]);
    let target_combat_after = sys.get_combat_state(target_id).await.unwrap().expect("target combat state");
    assert!(target_combat_after.shield_hp < target_combat_after.max_shield_hp);

    let _ = sess.drain_notifications();
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Deactivate",
        &[PyRep::Integer(ab_id), PyRep::String("moduleBonusAfterburner".into())],
        &kw,
        None,
    )
    .await
    .expect("Deactivate 1MN Afterburner I");
    let deact_notes = sess.drain_notifications();
    assert!(deact_notes.iter().any(|(n, _)| n == "OnModulesDeactivating"));
    assert!(deact_notes.iter().any(|(n, _)| n == "OnGodmaShipEffect"));

    let (_, ship_ball_restored) = sys.get_ball_snapshot(ship_id).await.unwrap().expect("restored ship ball");
    assert!((ship_ball_restored.max_velocity - 365.0).abs() < 0.1);

    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(ab_id), PyRep::String("moduleBonusAfterburner".into()), PyRep::None, PyRep::Integer(0)],
        &kw,
        None,
    )
    .await
    .expect("Activate 1MN Afterburner I with repeat=0");
    for _ in 0..8 {
        sys.step_manual(1.0).await.expect("sys.step_manual single cycle");
    }
    let (_, ship_ball_single_cycle) = sys.get_ball_snapshot(ship_id).await.unwrap().expect("single cycle ship ball");
    assert!((ship_ball_single_cycle.max_velocity - 365.0).abs() < 0.1);
}

// Folded submodule: tank_ext_2
#[allow(unused_imports, dead_code)]
mod tank_ext_2 {
    use super::*;

// Phase 4 Dogma local tank integration tests (`DG-4` part 2: ASB, AAR, RAH, Hull Repair, ADC, Cap Booster).

use crate::common;
use super::{
    extract_attr_from_info, grant_all_required_skills_for_type, seed_item,
};

use eve_db::InventoryFlag;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::attr_ids;
use eve_server::router::ServerContext;
use std::collections::HashMap;

#[tokio::test]
async fn test_dg4_local_tank_asb_aar_rah_hull_adc_and_cap_booster() {
    // parity: feat:modules.ancillary-shield-booster, feat:modules.armor-repairer, feat:modules.ancillary-armor-repairer, feat:modules.reactive-armor-hardener, feat:modules.hull-repairer, feat:modules.assault-damage-control, feat:modules.cap-booster, rpc:charUnboundMgr.SelectCharacterID, rpc:dogmaIM.LoadAmmo, rpc:dogmaIM.UnloadAmmo
    eve_testkit::attest(&[
        "feat:modules.ancillary-shield-booster",
        "feat:modules.armor-repairer",
        "feat:modules.ancillary-armor-repairer",
        "feat:modules.reactive-armor-hardener",
        "feat:modules.hull-repairer",
        "feat:modules.assault-damage-control",
        "feat:modules.cap-booster",
    ]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(4201);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None)
        .await
        .expect("SelectCharacterID");

    let char_id = 90_000_001i64;
    let ship_id = 100_000_001i64;
    let station_id = 60_003_760i64;

    // Upgrade ship to Harpy (11381, Assault Frigate, group 324: 4 MedSlots, 3 LoSlots)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship_item) = snap.items.get_mut(&ship_id) {
            ship_item.type_id = 11381;
        }
    });
    ctx.invalidate_ship_fitting(ship_id);

    for tid in [11381u32, 32774, 3566, 263, 523, 33076, 28668, 4403, 524, 1335, 47257] {
        grant_all_required_skills_for_type(&ctx, char_id, tid);
    }
    ctx.db.with_mut_snapshot(|snap| {
        let entry = snap.skills.entry(char_id).or_default();
        for &(sid, lvl) in &[(3426u32, 5u8), (3413, 5), (3418, 5), (3417, 5)] {
            entry.insert(
                sid,
                eve_db::TrainedSkillRecord {
                    type_id: sid,
                    level: lvl,
                    skill_points: 256_000.0,
                },
            );
        }
    });

    // 1. Verify Reinforced Bulkheads II (1335) passive hull HP bonus (+25%) and cargo/inertia drawbacks (feat:modules.hull-repairer)
    let fit_base = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).expect("fit_base");
    let base_hull_hp = fit_base.ship.get_attribute(attr_ids::HP_STRUCT, &ctx.sde);
    let base_cargo = fit_base.ship.get_attribute(attr_ids::CAPACITY, &ctx.sde);
    let base_agility = fit_base.ship.get_attribute(attr_ids::AGILITY, &ctx.sde);

    let bulkheads_id = 100_002_001i64;
    seed_item(&ctx, bulkheads_id, 1335, char_id, station_id, InventoryFlag::Hangar, 1, true);
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "Add",
        &[PyRep::Integer(bulkheads_id), PyRep::Integer(ship_id), PyRep::Integer(InventoryFlag::LoSlot0 as i64)],
        &kw,
        None,
    )
    .await
    .expect("Fit Reinforced Bulkheads II");
    let fit_bulk = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).expect("fit_bulk");
    let bulk_hull_hp = fit_bulk.ship.get_attribute(attr_ids::HP_STRUCT, &ctx.sde);
    let bulk_cargo = fit_bulk.ship.get_attribute(attr_ids::CAPACITY, &ctx.sde);
    let bulk_agility = fit_bulk.ship.get_attribute(attr_ids::AGILITY, &ctx.sde);
    assert!(
        (bulk_hull_hp - base_hull_hp * 1.25).abs() < 0.5,
        "Reinforced Bulkheads II must increase structure HP by 25%: {bulk_hull_hp} vs {base_hull_hp}"
    );
    assert!(
        bulk_cargo < base_cargo && bulk_agility > base_agility,
        "Reinforced Bulkheads II must penalize cargo capacity ({bulk_cargo} < {base_cargo}) and agility ({bulk_agility} > {base_agility})"
    );
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "Add",
        &[PyRep::Integer(bulkheads_id), PyRep::Integer(station_id), PyRep::Integer(InventoryFlag::Hangar as i64)],
        &kw,
        None,
    )
    .await
    .expect("Unfit Reinforced Bulkheads II");

    let asb_id = 100_002_002i64;
    let cap_boost_id = 100_002_003i64;
    let hull_rep_id = 100_002_004i64;
    let armor_rep_id = 100_002_005i64;
    let aar_id = 100_002_006i64;
    let rah_id = 100_002_007i64;
    let adc_id = 100_002_008i64;

    seed_item(&ctx, asb_id, 32774, char_id, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, cap_boost_id, 3566, char_id, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, hull_rep_id, 524, char_id, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, armor_rep_id, 523, char_id, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, aar_id, 33076, char_id, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, rah_id, 4403, char_id, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, adc_id, 47257, char_id, ship_id, InventoryFlag::Cargo, 1, true);

    let cap25_stack_1 = 100_002_010i64;
    let cap25_stack_2 = 100_002_011i64;
    let paste_stack = 100_002_012i64;
    seed_item(&ctx, cap25_stack_1, 263, char_id, ship_id, InventoryFlag::Cargo, 20, false);
    seed_item(&ctx, cap25_stack_2, 263, char_id, ship_id, InventoryFlag::Cargo, 20, false);
    seed_item(&ctx, paste_stack, 28668, char_id, ship_id, InventoryFlag::Cargo, 50, false);

    for (mid, flag) in [
        (asb_id, InventoryFlag::MedSlot0),
        (cap_boost_id, InventoryFlag::MedSlot1),
        (hull_rep_id, InventoryFlag::MedSlot2),
        (armor_rep_id, InventoryFlag::LoSlot0),
        (aar_id, InventoryFlag::LoSlot1),
        (rah_id, InventoryFlag::LoSlot2),
    ] {
        ctx.dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "Add",
            &[PyRep::Integer(mid), PyRep::Integer(ship_id), PyRep::Integer(flag as i64)],
            &kw,
            None,
        )
        .await
        .expect("Fit local tank module");
    }

    let asb_load = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "LoadAmmo",
            &[
                PyRep::Integer(ship_id),
                PyRep::List(vec![PyRep::Integer(asb_id)]),
                PyRep::List(vec![PyRep::Integer(cap25_stack_1)]),
                PyRep::Integer(ship_id),
            ],
            &kw,
            None,
        )
        .await
        .expect("Load Cap Booster 25 into Small ASB");
    assert_eq!(asb_load.dict_get("reloadTimeMs").and_then(|v| v.as_f64()), Some(60_000.0));

    let cb_load = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "LoadAmmo",
            &[
                PyRep::Integer(ship_id),
                PyRep::List(vec![PyRep::Integer(cap_boost_id)]),
                PyRep::List(vec![PyRep::Integer(cap25_stack_2)]),
                PyRep::Integer(ship_id),
            ],
            &kw,
            None,
        )
        .await
        .expect("Load Cap Booster 25 into Small Capacitor Booster I");
    assert_eq!(cb_load.dict_get("reloadTimeMs").and_then(|v| v.as_f64()), Some(10_000.0));

    let aar_load = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "LoadAmmo",
            &[
                PyRep::Integer(ship_id),
                PyRep::List(vec![PyRep::Integer(aar_id)]),
                PyRep::List(vec![PyRep::Integer(paste_stack)]),
                PyRep::Integer(ship_id),
            ],
            &kw,
            None,
        )
        .await
        .expect("Load Nanite Repair Paste into Small AAR");
    assert_eq!(aar_load.dict_get("reloadTimeMs").and_then(|v| v.as_f64()), Some(60_000.0));

    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None).await.expect("Undock");
    let sys = ctx.systems.get_or_spawn(30_000_142);

    let mut cs = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    cs.shield_hp = 100.0;
    cs.armor_hp = 100.0;
    cs.structure_hp = 100.0;
    cs.capacitor = 100.0;
    cs.shield_recharge_time_ms = 10_000_000.0;
    cs.capacitor_recharge_time_ms = 10_000_000.0;
    sys.set_combat_state(ship_id, cs).await.unwrap();

    // 2. Verify Capacitor Booster (feat:modules.cap-booster)
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(cap_boost_id), PyRep::String("powerBooster".into()), PyRep::None, PyRep::Integer(0)],
        &kw,
        None,
    )
    .await
    .expect("Activate Small Capacitor Booster I");
    let cap_after_cb = sys.get_combat_state(ship_id).await.unwrap().unwrap().capacitor;
    assert!((cap_after_cb - 125.0).abs() < 0.5, "got {cap_after_cb}");

    // 3. Verify Ancillary Shield Booster (feat:modules.ancillary-shield-booster)
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(asb_id), PyRep::String("fueledShieldBoosting".into()), PyRep::None, PyRep::Integer(0)],
        &kw,
        None,
    )
    .await
    .expect("Activate fueled Small ASB");
    let cap_after_asb_act = sys.get_combat_state(ship_id).await.unwrap().unwrap().capacitor;
    assert!((cap_after_asb_act - 125.0).abs() < 0.5, "got {cap_after_asb_act}");
    for _ in 0..4 {
        sys.step_manual(1.0).await.unwrap();
    }
    let shield_after_asb = sys.get_combat_state(ship_id).await.unwrap().unwrap().shield_hp;
    assert!(shield_after_asb > 130.0, "got {shield_after_asb}");

    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "UnloadAmmo",
        &[PyRep::Integer(ship_id), PyRep::List(vec![PyRep::Integer(asb_id)])],
        &kw,
        None,
    )
    .await
    .expect("Unload Small ASB");
    sys.set_capacitor(ship_id, 200.0).await.unwrap();
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(asb_id), PyRep::String("fueledShieldBoosting".into()), PyRep::None, PyRep::Integer(0)],
        &kw,
        None,
    )
    .await
    .expect("Activate unfueled Small ASB");
    let cap_after_unfueled_asb = sys.get_combat_state(ship_id).await.unwrap().unwrap().capacitor;
    assert!(cap_after_unfueled_asb < 150.0, "got {cap_after_unfueled_asb}");

    // 4. Verify Local Armor Repairer (523) & Ancillary Armor Repairer (33076)
    sys.set_capacitor(ship_id, 300.0).await.unwrap();
    let armor_before_rep = sys.get_combat_state(ship_id).await.unwrap().unwrap().armor_hp;
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(armor_rep_id), PyRep::String("armorRepair".into()), PyRep::None, PyRep::Integer(0)],
        &kw,
        None,
    )
    .await
    .expect("Activate Small Armor Repairer I");
    for _ in 0..8 {
        sys.step_manual(1.0).await.unwrap();
    }
    let armor_after_rep = sys.get_combat_state(ship_id).await.unwrap().unwrap().armor_hp;
    assert!(armor_after_rep > armor_before_rep + 40.0);

    let mut cs_aar = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    cs_aar.armor_hp = 100.0;
    cs_aar.max_armor_hp = 1000.0;
    cs_aar.capacitor = 300.0;
    sys.set_combat_state(ship_id, cs_aar).await.unwrap();

    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(aar_id), PyRep::String("fueledArmorRepair".into()), PyRep::None, PyRep::Integer(0)],
        &kw,
        None,
    )
    .await
    .expect("Activate fueled Small AAR");
    for _ in 0..8 {
        sys.step_manual(1.0).await.unwrap();
    }
    let armor_after_fueled_aar = sys.get_combat_state(ship_id).await.unwrap().unwrap().armor_hp;
    let fueled_gain = armor_after_fueled_aar - 100.0;
    assert!((fueled_gain - 156.0).abs() < 1.0, "got {fueled_gain}");

    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "UnloadAmmo",
        &[PyRep::Integer(ship_id), PyRep::List(vec![PyRep::Integer(aar_id)])],
        &kw,
        None,
    )
    .await
    .expect("Unload Small AAR");
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(aar_id), PyRep::String("fueledArmorRepair".into()), PyRep::None, PyRep::Integer(0)],
        &kw,
        None,
    )
    .await
    .expect("Activate unfueled Small AAR");
    for _ in 0..8 {
        sys.step_manual(1.0).await.unwrap();
    }
    let armor_after_unfueled_aar = sys.get_combat_state(ship_id).await.unwrap().unwrap().armor_hp;
    let unfueled_gain = armor_after_unfueled_aar - armor_after_fueled_aar;
    assert!((unfueled_gain - 52.0).abs() < 1.0 && (fueled_gain - 3.0 * unfueled_gain).abs() < 1.0);

    // 5. Verify Local Hull Repairer (524) (feat:modules.hull-repairer)
    sys.set_capacitor(ship_id, 300.0).await.unwrap();
    let hull_before = sys.get_combat_state(ship_id).await.unwrap().unwrap().structure_hp;
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(hull_rep_id), PyRep::String("structureRepair".into()), PyRep::None, PyRep::Integer(0)],
        &kw,
        None,
    )
    .await
    .expect("Activate Small Hull Repairer I");
    for _ in 0..30 {
        sys.step_manual(1.0).await.unwrap();
    }
    let hull_after = sys.get_combat_state(ship_id).await.unwrap().unwrap().structure_hp;
    assert!(hull_after > hull_before + 10.0);

    // 6. Verify Reactive Armor Hardener (4403) (feat:modules.reactive-armor-hardener)
    sys.set_capacitor(ship_id, 300.0).await.unwrap();
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(rah_id), PyRep::String("adaptiveArmorHardener".into()), PyRep::None, PyRep::Integer(1)],
        &kw,
        None,
    )
    .await
    .expect("Activate Reactive Armor Hardener");
    let rah_info_init = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "ItemGetInfo", &[PyRep::Integer(rah_id)], &kw, None)
        .await
        .expect("ItemGetInfo RAH initial");
    for aid in 267..=270 {
        assert!((extract_attr_from_info(&rah_info_init, aid) - 0.85).abs() < 1e-4);
    }
    let _ = sys.apply_damage(ship_id, eve_destiny::DamageVector::new(100.0, 0.0, 0.0, 0.0)).await;
    for _ in 0..11 {
        sys.step_manual(1.0).await.unwrap();
    }
    let rah_info_shifted = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "ItemGetInfo", &[PyRep::Integer(rah_id)], &kw, None)
        .await
        .expect("ItemGetInfo RAH shifted");
    let em_res = extract_attr_from_info(&rah_info_shifted, 267);
    let th_res = extract_attr_from_info(&rah_info_shifted, 268);
    assert!(em_res < 0.85 && th_res > 0.85);
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Deactivate",
        &[PyRep::Integer(rah_id), PyRep::String("adaptiveArmorHardener".into())],
        &kw,
        None,
    )
    .await
    .expect("Deactivate Reactive Armor Hardener");
    let rah_info_reset = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "ItemGetInfo", &[PyRep::Integer(rah_id)], &kw, None)
        .await
        .expect("ItemGetInfo RAH reset");
    assert!((extract_attr_from_info(&rah_info_reset, 267) - 0.85).abs() < 1e-4);

    // 7. Verify Assault Damage Control II (47257) on Harpy (11381) (feat:modules.assault-damage-control)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(r_it) = snap.items.get_mut(&rah_id) {
            r_it.flag = InventoryFlag::Cargo;
        }
        if let Some(a_it) = snap.items.get_mut(&adc_id) {
            a_it.flag = InventoryFlag::LoSlot2;
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
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
    .expect("Activate Assault Damage Control II");
    let cs_during_adc = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    assert_eq!(cs_during_adc.shield_resonance, [0.25; 4]);
    assert_eq!(cs_during_adc.armor_resonance, [0.25; 4]);
    assert_eq!(cs_during_adc.structure_resonance, [0.25; 4]);
    for _ in 0..11 {
        sys.step_manual(1.0).await.unwrap();
    }
    let adc_react_err = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(adc_id), PyRep::String("moduleBonusAssaultDamageControl".into()), PyRep::None, PyRep::Integer(0)],
            &kw,
            None,
        )
        .await;
    assert_eq!(adc_react_err.expect_err("ADC II must enforce 150s reactivation delay").code, "ModuleReactivationDelay");
}

}

// Folded submodule: tank_ext_3
#[allow(unused_imports, dead_code)]
mod tank_ext_3 {
    use super::*;

// Phase 4 Dogma remote logistics, mutadaptive spool, and capital remote assistance impedance tests (`DG-4` part 3).

use crate::common;
use super::{grant_all_required_skills_for_type, seed_item};

use eve_db::InventoryFlag;
use eve_destiny::{Ball, CombatState, DVec3, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

#[tokio::test]
async fn test_dg4_remote_logistics_mutadaptive_spool_and_impedance() {
    // parity: feat:modules.remote-shield, feat:modules.remote-armor, feat:modules.remote-hull, feat:modules.remote-cap, feat:modules.remote-assistance-impedance, rpc:charUnboundMgr.SelectCharacterID
    eve_testkit::attest(&[
        "feat:modules.remote-shield",
        "feat:modules.remote-armor",
        "feat:modules.remote-hull",
        "feat:modules.remote-cap",
        "feat:modules.remote-assistance-impedance",
    ]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess = SessionState::new(4202);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None)
        .await
        .expect("SelectCharacterID");

    let char_id = 90_000_001i64;
    let ship_id = 100_000_001i64;
    let station_id = 60_003_760i64;

    // Upgrade ship to Zarmazd (49713, Triglavian Logistics Cruiser, 5 HiSlots)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship_item) = snap.items.get_mut(&ship_id) {
            ship_item.type_id = 49713;
        }
        snap.items.remove(&100_000_005);
    });
    ctx.invalidate_ship_fitting(ship_id);

    for tid in [49713u32, 3586, 11355, 49770, 4299, 529, 587, 42242, 24569] {
        grant_all_required_skills_for_type(&ctx, char_id, tid);
    }
    ctx.db.with_mut_snapshot(|snap| {
        let entry = snap.skills.entry(char_id).or_default();
        for &(sid, lvl) in &[(3426u32, 5u8), (3413, 5), (3418, 5), (3417, 5), (3428, 5), (3429, 5), (3431, 5), (49743, 5)] {
            entry.insert(
                sid,
                eve_db::TrainedSkillRecord {
                    type_id: sid,
                    level: lvl,
                    skill_points: 256_000.0,
                },
            );
        }
    });

    let rem_shield_id = 100_002_101i64;
    let rem_armor_1_id = 100_002_102i64;
    let rem_armor_2_id = 100_002_103i64;
    let mutadaptive_id = 100_002_104i64;
    let rem_hull_id = 100_002_105i64;
    let rem_cap_id = 100_002_106i64;

    seed_item(&ctx, rem_shield_id, 3586, char_id, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, rem_armor_1_id, 11355, char_id, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, rem_armor_2_id, 11355, char_id, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, mutadaptive_id, 49770, char_id, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, rem_hull_id, 4299, char_id, ship_id, InventoryFlag::Cargo, 1, true);
    seed_item(&ctx, rem_cap_id, 529, char_id, ship_id, InventoryFlag::Cargo, 1, true);

    for (mid, flag) in [
        (rem_shield_id, InventoryFlag::HiSlot0),
        (rem_armor_1_id, InventoryFlag::HiSlot1),
        (rem_armor_2_id, InventoryFlag::HiSlot2),
        (mutadaptive_id, InventoryFlag::HiSlot3),
    ] {
        ctx.dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "Add",
            &[PyRep::Integer(mid), PyRep::Integer(ship_id), PyRep::Integer(flag as i64)],
            &kw,
            None,
        )
        .await
        .expect("Fit remote logistics module");
    }

    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let sys = ctx.systems.get_or_spawn(30_000_142);
    let (_, ship_ball) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();

    let ally_char_id = 90_000_002i64;
    let ally_id = 100_002_120i64;
    seed_item(&ctx, ally_id, 587, ally_char_id, 30_000_142, InventoryFlag::None, 1, true);
    let mut ally_cs = CombatState::default();
    ally_cs.max_shield_hp = 2000.0;
    ally_cs.shield_hp = 100.0;
    ally_cs.max_armor_hp = 20_000.0;
    ally_cs.armor_hp = 100.0;
    ally_cs.max_structure_hp = 2000.0;
    ally_cs.structure_hp = 100.0;
    ally_cs.max_capacitor = 1000.0;
    ally_cs.capacitor = 100.0;
    ally_cs.shield_recharge_time_ms = 0.0;
    ally_cs.capacitor_recharge_time_ms = 0.0;
    sys.spawn_ball_with_combat(
        Ball {
            id: ally_id,
            type_id: 587,
            position: ship_ball.position + DVec3::new(3_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 365.0,
            inertia: 3.2,
            mass: 1_350_000.0,
            radius: 35.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        ally_cs,
    )
    .await
    .unwrap();
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "AddTarget", &[PyRep::Integer(ally_id)], &kw, None)
        .await
        .expect("AddTarget on ally_id");
    for _ in 0..24 {
        if sys.get_target_locks(ship_id).await.unwrap().iter().any(|l| l.target_id == ally_id && l.locked) {
            break;
        }
        sys.step_manual(0.5).await.unwrap();
    }
    assert!(sys.get_target_locks(ship_id).await.unwrap().iter().any(|l| l.target_id == ally_id && l.locked));

    let now_unix = ctx.now_unix_secs();
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        let a_state = cw.get_or_create(ally_char_id);
        a_state.suspect_flag_until_unix = now_unix + 900;
        a_state.weapon_timer_until_unix = now_unix + 60;
    }

    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(rem_shield_id),
            PyRep::String("shipModuleRemoteShieldBooster".into()),
            PyRep::Integer(ally_id),
            PyRep::Integer(0),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate Small Remote Shield Booster I");
    let ally_shield_after = sys.get_combat_state(ally_id).await.unwrap().unwrap().shield_hp;
    assert!(ally_shield_after > 130.0, "got {ally_shield_after}");
    {
        let cw = ctx.crimewatch.read().unwrap();
        let l_state = cw.states.get(&char_id).expect("logistics crimewatch state");
        assert!(l_state.is_suspect(now_unix) && l_state.is_weapon_timer_active(now_unix));
    }
    for _ in 0..5 {
        sys.step_manual(1.0).await.unwrap();
    }

    // 2. Verify Remote Hull Repairer (4299) & Remote Capacitor Transmitter (529)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(s_it) = snap.items.get_mut(&rem_shield_id) {
            s_it.type_id = 4299;
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(rem_shield_id),
            PyRep::String("RemoteHullRepair".into()),
            PyRep::Integer(ally_id),
            PyRep::Integer(0),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate Small Remote Hull Repairer I");
    for _ in 0..8 {
        sys.step_manual(1.0).await.unwrap();
    }
    let ally_hull_after = sys.get_combat_state(ally_id).await.unwrap().unwrap().structure_hp;
    assert!(ally_hull_after > 110.0, "got {ally_hull_after}");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(s_it) = snap.items.get_mut(&rem_shield_id) {
            s_it.type_id = 529;
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(rem_shield_id),
            PyRep::String("energyTransfer".into()),
            PyRep::Integer(ally_id),
            PyRep::Integer(0),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate Small Remote Capacitor Transmitter I");
    let ally_cap_after = sys.get_combat_state(ally_id).await.unwrap().unwrap().capacitor;
    assert!(ally_cap_after > 120.0, "got {ally_cap_after}");

    // 3a. Sub-capital Remote Armor Repairers (`11355`) stack 100% additively (`2.0x` single repair, `R-REMOTEREP-STACK-01`)
    let armor_before_single = sys.get_combat_state(ally_id).await.unwrap().unwrap().armor_hp;
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(rem_armor_1_id),
            PyRep::String("shipModuleRemoteArmorRepairer".into()),
            PyRep::Integer(ally_id),
            PyRep::Integer(0),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate first Small Remote Armor Repairer I");
    for _ in 0..6 {
        sys.step_manual(1.0).await.unwrap();
    }
    let armor_after_single = sys.get_combat_state(ally_id).await.unwrap().unwrap().armor_hp;
    let single_rep_gain = armor_after_single - armor_before_single;
    assert!(single_rep_gain > 30.0);

    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(rem_armor_1_id),
            PyRep::String("shipModuleRemoteArmorRepairer".into()),
            PyRep::Integer(ally_id),
            PyRep::Integer(0),
        ],
        &kw,
        None,
    )
    .await
    .unwrap();
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(rem_armor_2_id),
            PyRep::String("shipModuleRemoteArmorRepairer".into()),
            PyRep::Integer(ally_id),
            PyRep::Integer(0),
        ],
        &kw,
        None,
    )
    .await
    .unwrap();
    for _ in 0..6 {
        sys.step_manual(1.0).await.unwrap();
    }
    let armor_after_dual = sys.get_combat_state(ally_id).await.unwrap().unwrap().armor_hp;
    let dual_rep_gain = armor_after_dual - armor_after_single;
    assert!(
        (dual_rep_gain - 2.0 * single_rep_gain).abs() < 1e-3,
        "Sub-capital remote armor repairers must stack 100% additively (2.0x single={:.2}, got {:.2})",
        2.0 * single_rep_gain,
        dual_rep_gain
    );

    // 4. Verify Triglavian Heavy Mutadaptive Remote Armor Repairer I (49770) spool-up (+10%/cycle)
    sys.set_capacitor(ship_id, 5000.0).await.unwrap();
    let armor_before_mut = sys.get_combat_state(ally_id).await.unwrap().unwrap().armor_hp;
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(mutadaptive_id),
            PyRep::String("shipModuleRemoteArmorMutadaptiveRepairer".into()),
            PyRep::Integer(ally_id),
            PyRep::Integer(1),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate Heavy Mutadaptive Remote Armor Repairer I");
    for _ in 0..7 {
        sys.step_manual(1.0).await.unwrap();
    }
    let armor_after_mut_c1 = sys.get_combat_state(ally_id).await.unwrap().unwrap().armor_hp;
    let gain_c1 = armor_after_mut_c1 - armor_before_mut;
    for _ in 0..7 {
        sys.step_manual(1.0).await.unwrap();
    }
    let armor_after_mut_c2 = sys.get_combat_state(ally_id).await.unwrap().unwrap().armor_hp;
    let gain_c2 = armor_after_mut_c2 - armor_after_mut_c1;
    assert!((gain_c2 - gain_c1 * 1.10).abs() < 1.0, "c1={gain_c1}, c2={gain_c2}");
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Deactivate",
        &[PyRep::Integer(mutadaptive_id), PyRep::String("shipModuleRemoteArmorMutadaptiveRepairer".into())],
        &kw,
        None,
    )
    .await
    .expect("Deactivate Heavy Mutadaptive Remote Armor Repairer I");

    // 3b. Capital Remote Assistance Impedance (`feat:modules.remote-assistance-impedance`):
    // Two Capital Remote Armor Repairer I (`24569`) on Dagon FAX (`42242`) experience diminishing returns!
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship_item) = snap.items.get_mut(&ship_id) {
            ship_item.type_id = 42242;
        }
        if let Some(m1) = snap.items.get_mut(&rem_armor_1_id) {
            m1.type_id = 24569;
        }
        if let Some(m2) = snap.items.get_mut(&rem_armor_2_id) {
            m2.type_id = 24569;
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
    let mut fax_cs = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    fax_cs.max_capacitor = 80_000.0;
    fax_cs.capacitor = 80_000.0;
    sys.set_combat_state(ship_id, fax_cs).await.unwrap();

    let cap_armor_0 = sys.get_combat_state(ally_id).await.unwrap().unwrap().armor_hp;
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(rem_armor_1_id),
            PyRep::String("shipModuleRemoteArmorRepairer".into()),
            PyRep::Integer(ally_id),
            PyRep::Integer(0),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate single Capital Remote Armor Repairer I");
    for _ in 0..16 {
        sys.step_manual(1.0).await.unwrap();
    }
    let cap_armor_1 = sys.get_combat_state(ally_id).await.unwrap().unwrap().armor_hp;
    let single_cap_gain = cap_armor_1 - cap_armor_0;
    assert!(single_cap_gain > 500.0, "got {single_cap_gain}");

    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(rem_armor_1_id),
            PyRep::String("shipModuleRemoteArmorRepairer".into()),
            PyRep::Integer(ally_id),
            PyRep::Integer(0),
        ],
        &kw,
        None,
    )
    .await
    .unwrap();
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(rem_armor_2_id),
            PyRep::String("shipModuleRemoteArmorRepairer".into()),
            PyRep::Integer(ally_id),
            PyRep::Integer(0),
        ],
        &kw,
        None,
    )
    .await
    .unwrap();
    for _ in 0..16 {
        sys.step_manual(1.0).await.unwrap();
    }
    let cap_armor_2 = sys.get_combat_state(ally_id).await.unwrap().unwrap().armor_hp;
    let dual_cap_gain = cap_armor_2 - cap_armor_1;
    assert!(
        dual_cap_gain > single_cap_gain && dual_cap_gain < 2.0 * single_cap_gain - 10.0,
        "Capital Remote Assistance Impedance must attenuate second Capital Remote Armor Repairer I (dual={dual_cap_gain} vs 2x single={})",
        2.0 * single_cap_gain
    );
}

}
