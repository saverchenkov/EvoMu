//! Phase 4 Dogma local tank integration tests (`DG-4` part 2: ASB, AAR, RAH, Hull Repair, ADC, Cap Booster).

use crate::common;
use super::dogma_tank_logistics::{
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
