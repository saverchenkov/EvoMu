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
    let t2889 = &ctx.sde.types[&2889].default_attributes;
    let expected_duration = *t2889.get(&73).or_else(|| t2889.get(&51)).expect("cycle time attr");
    assert!(expected_duration > 0.0);
    assert_eq!(a[7], PyRep::Real(expected_duration));
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
