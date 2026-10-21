//! Phase 4 Dogma and Fitting integration tests (`DG-2`..`DG-6` and Phase 4 Exit Gate).


use crate::common;

use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, CombatState, DVec3, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::attr_ids;
use eve_server::router::ServerContext;
use std::collections::HashMap;

fn seed_item(
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

fn grant_all_required_skills_for_type(ctx: &ServerContext, char_id: i64, type_id: u32) {
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

fn extract_attr_from_info(info_rep: &PyRep, attr_id: i64) -> f64 {
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
async fn test_phase_4_dg6_ammo_loading_and_weapon_grouping() {
    // parity: feat:fitting.ammo-load, feat:fitting.ammo-unload, feat:fitting.weapon-grouping, feat:modules.scripts-swap, rpc:dogmaIM.LoadAmmo, rpc:dogmaIM.UnloadAmmo, rpc:dogmaIM.LinkWeapons, rpc:dogmaIM.LinkAllWeapons, rpc:dogmaIM.UnlinkModule, rpc:dogmaIM.UnlinkAllModules, rpc:dogmaIM.PeelAndLink, rpc:dogmaIM.MergeModuleGroups, rpc:dogmaIM.DestroyWeaponBank, rpc:charUnboundMgr.SelectCharacterID, rpc:invbroker.Add
    eve_testkit::attest(&["feat:fitting.ammo-load", "feat:fitting.ammo-unload", "feat:fitting.weapon-grouping", "feat:modules.scripts-swap", "rpc:dogmaIM.LoadAmmo", "rpc:dogmaIM.UnloadAmmo", "rpc:dogmaIM.LinkWeapons", "rpc:dogmaIM.LinkAllWeapons", "rpc:dogmaIM.UnlinkModule", "rpc:dogmaIM.UnlinkAllModules", "rpc:dogmaIM.PeelAndLink", "rpc:dogmaIM.MergeModuleGroups", "rpc:dogmaIM.DestroyWeaponBank"]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(4004);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None).await
    .expect("SelectCharacterID");

    let char_id = 90_000_001i64;
    let ship_id = 100_000_001i64;
    let station_id = 60_003_760i64;
    for tid in [2889u32, 183, 12625, 1978, 28999, 29001] {
        grant_all_required_skills_for_type(&ctx, char_id, tid);
    }

    // Fit 2 additional 200mm AutoCannon II (2889) into HiSlot1 (28) and HiSlot2 (29)
    let t1_id = 100_000_005i64; // already in HiSlot0 (27)
    let t2_id = 100_000_831i64;
    let t3_id = 100_000_832i64;
    seed_item(&ctx, t2_id, 2889, char_id, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, t3_id, 2889, char_id, station_id, InventoryFlag::Hangar, 1, true);
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "Add",
        &[
            PyRep::Integer(t2_id),
            PyRep::Integer(ship_id),
            PyRep::Integer(InventoryFlag::HiSlot1 as i64),
        ],
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
        &[
            PyRep::Integer(t3_id),
            PyRep::Integer(ship_id),
            PyRep::Integer(InventoryFlag::HiSlot2 as i64),
        ],
        &kw,
        None,
    )
    .await
    .expect("Fit third 200mm AutoCannon II");

    // 1. Exercise all 7 weapon-grouping RPCs
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "LinkWeapons",
        &[
            PyRep::Integer(ship_id),
            PyRep::Integer(t1_id),
            PyRep::Integer(t2_id),
        ],
        &kw,
        None,
    )
    .await
    .expect("LinkWeapons");
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "PeelAndLink",
        &[
            PyRep::Integer(ship_id),
            PyRep::Integer(t3_id),
            PyRep::Integer(t2_id),
        ],
        &kw,
        None,
    )
    .await
    .expect("PeelAndLink");
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "MergeModuleGroups",
        &[
            PyRep::Integer(ship_id),
            PyRep::Integer(t1_id),
            PyRep::Integer(t3_id),
        ],
        &kw,
        None,
    )
    .await
    .expect("MergeModuleGroups");
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "UnlinkModule", &[PyRep::Integer(ship_id), PyRep::Integer(t3_id)], &kw, None).await
    .expect("UnlinkModule");
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "DestroyWeaponBank", &[PyRep::Integer(ship_id), PyRep::Integer(t1_id)], &kw, None).await
    .expect("DestroyWeaponBank");
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "LinkAllWeapons", &[PyRep::Integer(ship_id)], &kw, None).await
    .expect("LinkAllWeapons");
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "UnlinkAllModules", &[PyRep::Integer(ship_id)], &kw, None).await
    .expect("UnlinkAllModules");
    // Re-link all 3 turrets into one bank for grouped ammo loading
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "LinkAllWeapons", &[PyRep::Integer(ship_id)], &kw, None).await
    .expect("LinkAllWeapons re-link");
    assert_eq!(
        ctx.db.with_snapshot(|s| {
            s.weapon_banks
                .get(&ship_id)
                .and_then(|b| b.get(&t1_id))
                .map(Vec::len)
        }),
        Some(3),
        "All 3 200mm AutoCannon II turrets must be in master turret's bank"
    );

    // 2. Load Fusion S (183, vol=0.0025, range mult=0.5) into the 3-turret bank via dogmaIM.LoadAmmo
    let fusion_stack_id = 100_000_833i64;
    seed_item(
        &ctx,
        fusion_stack_id,
        183,
        char_id,
        ship_id,
        InventoryFlag::Cargo,
        500,
        false,
    );
    let load_ret = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "LoadAmmo",
            &[
                PyRep::Integer(ship_id),
                PyRep::List(vec![PyRep::Integer(t1_id)]),
                PyRep::List(vec![PyRep::Integer(fusion_stack_id)]),
                PyRep::Integer(ship_id),
            ],
            &kw,
            None,
        )
        .await
        .expect("LoadAmmo Fusion S");

    let PyRep::KeyVal(load_dict) = load_ret else {
        panic!("Expected KeyVal from LoadAmmo, got {load_ret:?}");
    };
    assert_eq!(
        load_dict.get("reloadTimeMs"),
        Some(&PyRep::Real(10_000.0)),
        "Expected reloadTimeMs = 10000.0 from SDE attr 1795"
    );
    assert_eq!(
        ctx.db
            .with_snapshot(|s| s.items.get(&fusion_stack_id).map(|i| i.quantity)),
        Some(140),
        "500 - 3*120 = 140 rounds must remain in cargo"
    );

    // Verify Fusion S charge modifier (effect 596: ammoInfluenceRange, PreMul 0.5 on maxRange 54)
    let fit_fusion = ctx
        .get_or_rebuild_ship_fitting(char_id, ship_id)
        .expect("fit_fusion");
    let mod_state_fusion =
        fit_fusion.build_effective_module_state(t1_id, 2889, false, &ctx.sde);
    let range_with_fusion = mod_state_fusion.get_attribute(attr_ids::MAX_RANGE, &ctx.sde);
    assert!(
        (range_with_fusion - 600.0).abs() < 0.1,
        "Expected Fusion S to halve 200mm AutoCannon II maxRange from 1200 to 600, got {range_with_fusion}"
    );

    // 3. UnloadAmmo from the bank back to cargo
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "UnloadAmmo",
        &[
            PyRep::Integer(ship_id),
            PyRep::List(vec![PyRep::Integer(t1_id)]),
            PyRep::Tuple(vec![
                PyRep::Integer(ship_id),
                PyRep::Integer(InventoryFlag::Cargo as i64),
            ]),
        ],
        &kw,
        None,
    )
    .await
    .expect("UnloadAmmo");
    let fit_unloaded = ctx
        .get_or_rebuild_ship_fitting(char_id, ship_id)
        .expect("fit_unloaded");
    let mod_state_unloaded =
        fit_unloaded.build_effective_module_state(t1_id, 2889, false, &ctx.sde);
    assert!(
        (mod_state_unloaded.get_attribute(attr_ids::MAX_RANGE, &ctx.sde) - 1200.0).abs() < 0.1,
        "Unloaded turret maxRange must return to 1200.0"
    );

    // 4. Scripted module script loading & swapping (feat:modules.scripts-swap):
    // Tracking Computer II (1978, MedSlot0=19) loads Optimal Range Script (28999) and swaps to Tracking Speed Script (29001).
    let tc_id = 100_000_834i64;
    let opt_script_id = 100_000_835i64;
    let trk_script_id = 100_000_836i64;
    seed_item(&ctx, tc_id, 1978, char_id, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, opt_script_id, 28999, char_id, ship_id, InventoryFlag::Cargo, 1, false);
    seed_item(&ctx, trk_script_id, 29001, char_id, ship_id, InventoryFlag::Cargo, 1, false);
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "Add",
        &[
            PyRep::Integer(tc_id),
            PyRep::Integer(ship_id),
            PyRep::Integer(InventoryFlag::MedSlot0 as i64),
        ],
        &kw,
        None,
    )
    .await
    .expect("Fit Tracking Computer II into MedSlot0");

    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "LoadAmmo",
        &[
            PyRep::Integer(ship_id),
            PyRep::List(vec![PyRep::Integer(tc_id)]),
            PyRep::List(vec![PyRep::Integer(opt_script_id)]),
            PyRep::Integer(ship_id),
        ],
        &kw,
        None,
    )
    .await
    .expect("Load Optimal Range Script into Tracking Computer II");

    let fit_opt_script = ctx
        .get_or_rebuild_ship_fitting(char_id, ship_id)
        .expect("fit_opt_script");
    let tc_state_opt = fit_opt_script.build_effective_module_state(tc_id, 1978, false, &ctx.sde);
    assert!(
        (tc_state_opt.get_attribute(351, &ctx.sde) - 15.0).abs() < 0.1,
        "Optimal Range Script must double maxRangeBonus (351) from 7.5 to 15.0"
    );
    assert!(
        tc_state_opt.get_attribute(767, &ctx.sde).abs() < 0.1,
        "Optimal Range Script must zero trackingSpeedBonus (767)"
    );

    // Swap directly to Tracking Speed Script (29001) via LoadAmmo (automatically unloads Optimal Range Script to cargo)
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "LoadAmmo",
        &[
            PyRep::Integer(ship_id),
            PyRep::List(vec![PyRep::Integer(tc_id)]),
            PyRep::List(vec![PyRep::Integer(trk_script_id)]),
            PyRep::Integer(ship_id),
        ],
        &kw,
        None,
    )
    .await
    .expect("Swap to Tracking Speed Script in Tracking Computer II");

    let fit_trk_script = ctx
        .get_or_rebuild_ship_fitting(char_id, ship_id)
        .expect("fit_trk_script");
    let tc_state_trk = fit_trk_script.build_effective_module_state(tc_id, 1978, false, &ctx.sde);
    assert!(
        tc_state_trk.get_attribute(351, &ctx.sde).abs() < 0.1,
        "Tracking Speed Script must zero maxRangeBonus (351)"
    );
    assert!(
        (tc_state_trk.get_attribute(767, &ctx.sde) - 30.0).abs() < 0.1,
        "Tracking Speed Script must double trackingSpeedBonus (767) from 15.0 to 30.0"
    );
}


#[tokio::test]
async fn test_dg4_siege_triage_bastion_industrial_core_phenomena_and_zpme() {
    // parity: feat:modules.siege-module, feat:modules.triage-module, feat:modules.bastion-module, feat:modules.industrial-core, feat:modules.phenomena-generator, feat:modules.zero-point-mass-entangler, rpc:charUnboundMgr.SelectCharacterID, rpc:dogmaIM.Activate, rpc:dogmaIM.Deactivate, rpc:dogmaIM.ItemGetInfo, rpc:invbroker.Add, rpc:stationSvc.Undock
    eve_testkit::attest(&["feat:modules.siege-module", "feat:modules.triage-module", "feat:modules.bastion-module", "feat:modules.industrial-core", "feat:modules.phenomena-generator", "feat:modules.zero-point-mass-entangler"]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(4203);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None).await
    .expect("SelectCharacterID");

    let char_id = 90_000_001i64;
    let ship_id = 100_000_001i64;
    let station_id = 60_003_760i64;

    for tid in [
        19720u32, 20280, 37604, 27951, 28659, 33400, 28352, 28583, 11567, 43658, 12017, 49099, 434,
    ] {
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

    // 1. Siege Module I (20280) on Revelation Dreadnought (19720) (feat:modules.siege-module)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship_item) = snap.items.get_mut(&ship_id) {
            ship_item.type_id = 19720;
        }
        snap.items.remove(&100_000_005);
    });
    ctx.invalidate_ship_fitting(ship_id);

    let siege_id = 100_002_201i64;
    let stront_id = 100_002_202i64;
    let heavy_water_id = 100_002_203i64;
    seed_item(&ctx, siege_id, 20280, char_id, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, stront_id, 16275, char_id, ship_id, InventoryFlag::Cargo, 2_000, false);
    seed_item(&ctx, heavy_water_id, 16272, char_id, ship_id, InventoryFlag::Cargo, 2_000, false);

    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "Add",
        &[
            PyRep::Integer(siege_id),
            PyRep::Integer(ship_id),
            PyRep::Integer(InventoryFlag::HiSlot0 as i64),
        ],
        &kw,
        None,
    )
    .await
    .expect("Fit Siege Module I");

    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let sys = ctx.systems.get_or_spawn(30_000_142);
    sys.set_capacitor(ship_id, 50_000.0).await.unwrap();

    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(siege_id),
            PyRep::String("siegeModeEffect".into()),
            PyRep::None,
            PyRep::Integer(1),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate Siege Module I on Revelation");
    let stront_after_siege = ctx
        .db
        .with_snapshot(|s| s.items.get(&stront_id).map(|i| i.quantity).unwrap_or(0));
    assert_eq!(
        stront_after_siege, 1_750,
        "Siege Module I must consume 250 Strontium Clathrates on activation"
    );
    let (_, ball_sieged) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let cs_sieged = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    assert_eq!(
        ball_sieged.max_velocity, 0.0,
        "Siege Module I must immobilize the Dreadnought (max_velocity = 0.0)"
    );
    assert!(
        cs_sieged.disallow_assistance,
        "Siege Module I must set disallow_assistance = true"
    );
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Deactivate",
        &[
            PyRep::Integer(siege_id),
            PyRep::String("siegeModeEffect".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Deactivate Siege Module I");

    // 2. Triage Module I (27951) on Apostle Force Auxiliary (37604) (feat:modules.triage-module)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(s) = snap.items.get_mut(&ship_id) {
            s.type_id = 37604;
        }
        if let Some(m) = snap.items.get_mut(&siege_id) {
            m.type_id = 27951;
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(siege_id),
            PyRep::String("triageModeEffect".into()),
            PyRep::None,
            PyRep::Integer(1),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate Triage Module I on Apostle");
    let stront_after_triage = ctx
        .db
        .with_snapshot(|s| s.items.get(&stront_id).map(|i| i.quantity).unwrap_or(0));
    assert_eq!(
        stront_after_triage, 1_500,
        "Triage Module I must consume 250 Strontium Clathrates on activation"
    );
    let cs_triaged = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(
        cs_triaged.disallow_assistance,
        "Triage Module I must set disallow_assistance = true"
    );
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Deactivate",
        &[
            PyRep::Integer(siege_id),
            PyRep::String("triageModeEffect".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Deactivate Triage Module I");

    // 3. Bastion Module I (33400) on Paladin Marauder (28659) (feat:modules.bastion-module)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(s) = snap.items.get_mut(&ship_id) {
            s.type_id = 28659;
        }
        if let Some(m) = snap.items.get_mut(&siege_id) {
            m.type_id = 33400;
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(siege_id),
            PyRep::String("moduleBonusBastion".into()),
            PyRep::None,
            PyRep::Integer(1),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate Bastion Module I on Paladin");
    let (_, ball_bastion) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert_eq!(
        ball_bastion.max_velocity, 0.0,
        "Bastion Module I must immobilize the Marauder"
    );
    let fit_bastion = ctx
        .get_or_rebuild_ship_fitting(char_id, ship_id)
        .expect("fit_bastion");
    assert!(
        fit_bastion.ship.get_attribute(attr_ids::ARMOR_EM_DAMAGE_RESONANCE, &ctx.sde) < 0.50,
        "Bastion Module I must boost Marauder armor resistances"
    );
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Deactivate",
        &[
            PyRep::Integer(siege_id),
            PyRep::String("moduleBonusBastion".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Deactivate Bastion Module I");

    // 4. Capital Industrial Core I (28583) on Rorqual (28352) (feat:modules.industrial-core)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(s) = snap.items.get_mut(&ship_id) {
            s.type_id = 28352;
        }
        if let Some(m) = snap.items.get_mut(&siege_id) {
            m.type_id = 28583;
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(siege_id),
            PyRep::String("moduleBonusIndustrialCore".into()),
            PyRep::None,
            PyRep::Integer(1),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate Capital Industrial Core I on Rorqual");
    let hw_after_core = ctx
        .db
        .with_snapshot(|s| s.items.get(&heavy_water_id).map(|i| i.quantity).unwrap_or(0));
    assert!(
        hw_after_core < 2_000,
        "Capital Industrial Core I must consume Heavy Water on activation"
    );
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Deactivate",
        &[
            PyRep::Integer(siege_id),
            PyRep::String("moduleBonusIndustrialCore".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Deactivate Capital Industrial Core I");

    // 5. Amarr Phenomena Generator (43658) on Avatar Titan (11567) (feat:modules.phenomena-generator)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(s) = snap.items.get_mut(&ship_id) {
            s.type_id = 11567;
        }
        if let Some(m) = snap.items.get_mut(&siege_id) {
            m.type_id = 43658;
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
    let ship_info_before_phenom = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "ItemGetInfo", &[PyRep::Integer(ship_id)], &kw, None).await
        .expect("ItemGetInfo before Amarr Phenomena Generator");
    let cap_recharge_before_phenom = extract_attr_from_info(&ship_info_before_phenom, 55);
    let kin_res_before_phenom = extract_attr_from_info(&ship_info_before_phenom, 269);
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(siege_id),
            PyRep::String("TitanPhenomenaGenerator".into()),
            PyRep::None,
            PyRep::Integer(1),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate Amarr Phenomena Generator on Avatar");
    let ship_info_phenom = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "ItemGetInfo", &[PyRep::Integer(ship_id)], &kw, None).await
        .expect("ItemGetInfo under Amarr Phenomena Generator");
    let cap_recharge_under_phenom = extract_attr_from_info(&ship_info_phenom, 55);
    let kin_res_under_phenom = extract_attr_from_info(&ship_info_phenom, 269);
    assert!(
        cap_recharge_under_phenom < cap_recharge_before_phenom
            && kin_res_under_phenom < kin_res_before_phenom,
        "Amarr Phenomena Generator dbuff must improve cap recharge time ({cap_recharge_under_phenom} < {cap_recharge_before_phenom}) and kinetic resonance ({kin_res_under_phenom} < {kin_res_before_phenom})"
    );
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Deactivate",
        &[
            PyRep::Integer(siege_id),
            PyRep::String("TitanPhenomenaGenerator".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Deactivate Amarr Phenomena Generator");

    // 6. Zero-Point Mass Entangler (49099) on Devoter Heavy Interdiction Cruiser (12017) (feat:modules.zero-point-mass-entangler)
    let mwd_id = 100_002_204i64;
    seed_item(&ctx, mwd_id, 434, char_id, ship_id, InventoryFlag::MedSlot0, 1, true);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(s) = snap.items.get_mut(&ship_id) {
            s.type_id = 12017;
        }
        if let Some(m) = snap.items.get_mut(&siege_id) {
            m.type_id = 49099;
            m.flag = InventoryFlag::HiSlot0;
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
    let (_, ball_before_zpme) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let orig_mass = ball_before_zpme.mass;
    let orig_vel = ball_before_zpme.max_velocity.max(200.0);

    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(siege_id),
            PyRep::String("moduleBonusZeroPointMassEntangler".into()),
            PyRep::None,
            PyRep::Integer(1),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate Zero-Point Mass Entangler on Devoter");
    let (_, ball_zpme) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(
        (ball_zpme.mass - orig_mass * 0.20).abs() < 1.0,
        "Zero-Point Mass Entangler must reduce HIC mass by 80% (to 20%), got {} vs {}",
        ball_zpme.mass,
        orig_mass
    );
    assert!(
        ball_zpme.max_velocity <= orig_vel * 0.05 + 0.1,
        "Zero-Point Mass Entangler must reduce HIC max velocity by 95% (to 5%)"
    );
    let mwd_under_zpme = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[
                PyRep::Integer(mwd_id),
                PyRep::String("moduleBonusMicrowarpdrive".into()),
                PyRep::None,
                PyRep::Integer(1),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        mwd_under_zpme
            .expect_err("Zero-Point Mass Entangler must disable MWD activation")
            .code,
        "WarpScrambled"
    );
}

#[tokio::test]
async fn test_dg6_heat_rack_thermodynamics_nanite_repair_and_crystal_volatility() {
    // parity: feat:dogma.heat-rack-thermodynamics, feat:fitting.nanite-repair-paste-consumption, feat:modules.t2-crystal-volatility-destruction, notify:OnHeatAdded, notify:OnHeatRemoved, notify:OnMultiModuleRepairStarted, notify:OnRepairDone, rpc:charUnboundMgr.SelectCharacterID, rpc:dogmaIM.Deactivate, rpc:dogmaIM.ItemGetInfo, rpc:dogmaIM.LoadAmmo, rpc:dogmaIM.Overload, rpc:dogmaIM.OverloadRack, rpc:dogmaIM.RepairModule, rpc:dogmaIM.StopOverload, rpc:dogmaIM.StopOverloadRack, rpc:dogmaIM.StopRepairModule, rpc:invbroker.Add, rpc:stationSvc.Undock
    eve_testkit::attest(&["feat:dogma.heat-rack-thermodynamics", "feat:fitting.nanite-repair-paste-consumption", "feat:modules.t2-crystal-volatility-destruction", "notify:OnHeatAdded", "notify:OnHeatRemoved", "notify:OnMultiModuleRepairStarted", "notify:OnRepairDone"]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(4204);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None).await
    .expect("SelectCharacterID");

    let char_id = 90_000_001i64;
    let ship_id = 100_000_001i64;
    let station_id = 60_003_760i64;

    // Upgrade ship to Punisher (597, Amarr Frigate) and fit two Small Focused Pulse Laser II (2977) in HiSlot0 & HiSlot1
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|&id, it| id == ship_id || it.location_id != ship_id);
        if let Some(s) = snap.items.get_mut(&ship_id) {
            s.type_id = 597;
        }
    });
    ctx.invalidate_ship_fitting(ship_id);

    for tid in [597u32, 3041, 12565, 28668] {
        grant_all_required_skills_for_type(&ctx, char_id, tid);
    }
    ctx.db.with_mut_snapshot(|snap| {
        let entry = snap.skills.entry(char_id).or_default();
        for &(sid, lvl) in &[
            (3426u32, 5u8),
            (3413, 5),
            (3418, 5),
            (3417, 5),
            (28164, 4), // Thermodynamics IV (-20% heat damage)
            (28879, 4), // Nanite Operation IV (-20% nanite repair paste consumption)
            (28880, 5), // Nanite Interfacing V (+100% nanite repair speed)
        ] {
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

    let laser_1_id = 100_002_301i64;
    let laser_2_id = 100_002_302i64;
    let conflag_id = 100_002_303i64;
    let paste_id = 100_002_304i64;
    seed_item(&ctx, laser_1_id, 3041, char_id, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, laser_2_id, 3041, char_id, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, conflag_id, 12565, char_id, ship_id, InventoryFlag::Cargo, 1, true);
    seed_item(&ctx, paste_id, 28668, char_id, ship_id, InventoryFlag::Cargo, 50, false);

    for (mid, flag) in [
        (laser_1_id, InventoryFlag::HiSlot0),
        (laser_2_id, InventoryFlag::HiSlot1),
    ] {
        ctx.dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(mid),
                PyRep::Integer(ship_id),
                PyRep::Integer(flag as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect("Fit Small Focused Pulse Laser II");
    }

    // Load Conflagration S (12565, T2 frequency crystal) into laser_1_id
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "LoadAmmo",
        &[
            PyRep::Integer(ship_id),
            PyRep::List(vec![PyRep::Integer(laser_1_id)]),
            PyRep::List(vec![PyRep::Integer(conflag_id)]),
            PyRep::Integer(ship_id),
        ],
        &kw,
        None,
    )
    .await
    .expect("Load Conflagration S into Small Focused Pulse Laser II");

    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let sys = ctx.systems.get_or_spawn(30_000_142);
    let (_, ship_ball) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();

    // Spawn target dummy at 2 km and lock it
    let target_id = 100_002_320i64;
    let mut target_cs = CombatState::default();
    target_cs.max_shield_hp = 100_000.0;
    target_cs.shield_hp = 100_000.0;
    sys.spawn_ball_with_combat(
        Ball {
            id: target_id,
            type_id: 587,
            position: ship_ball.position + DVec3::new(2_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 3.2,
            mass: 1_350_000.0,
            radius: 35.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        target_cs,
    )
    .await
    .unwrap();
    sys.add_target_lock(
        ship_id,
        eve_destiny::TargetLock {
            target_id,
            lock_time_ms: 0.0,
            remaining_lock_ms: 0.0,
            locked: true,
            max_range_m: Some(50_000.0),
        },
    )
    .await
    .unwrap();

    // 1. Activate laser_1_id with Overload via dogmaIM.Overload and test OverloadRack / StopOverloadRack / StopOverload (feat:dogma.heat-rack-thermodynamics)
    let _ = sess.drain_notifications();
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Overload",
        &[
            PyRep::Integer(laser_1_id),
            PyRep::String("targetAttack".into()),
            PyRep::Integer(target_id),
            PyRep::Integer(1),
        ],
        &kw,
        None,
    )
    .await
    .expect("Overload laser_1_id");
    let notes_ovl = sess.drain_notifications();
    assert!(notes_ovl.iter().any(|(n, _)| n == "OnHeatAdded"));

    // Step 2 weapon cycles (8s) -> high rack heat (1177) increases, laser_1_id takes heat damage, adjacent laser_2_id takes bled heat damage,
    // and loaded Conflagration S crystal accumulates crystalVolatilityDamage (feat:modules.t2-crystal-volatility-destruction)!
    for _ in 0..8 {
        sys.step_manual(1.0).await.unwrap();
    }

    let ship_info_heated = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "ItemGetInfo", &[PyRep::Integer(ship_id)], &kw, None).await
        .unwrap();
    let hi_rack_heat = extract_attr_from_info(&ship_info_heated, 1177);
    assert!(
        hi_rack_heat > 0.0,
        "High rack heat (1177) must accumulate during overloaded weapon cycles, got {hi_rack_heat}"
    );

    let l1_info = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "ItemGetInfo", &[PyRep::Integer(laser_1_id)], &kw, None).await
        .unwrap();
    let l2_info = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "ItemGetInfo", &[PyRep::Integer(laser_2_id)], &kw, None).await
        .unwrap();
    let l1_dmg = extract_attr_from_info(&l1_info, 3);
    let l2_dmg = extract_attr_from_info(&l2_info, 3);
    assert!(
        l1_dmg > 0.0 && l2_dmg > 0.0 && l1_dmg > l2_dmg,
        "Overloaded laser_1 ({l1_dmg}) and adjacent laser_2 ({l2_dmg}) must take heat damage with slot-distance attenuation"
    );

    // Verify OverloadRack, StopOverloadRack, and StopOverload RPCs
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "OverloadRack", &[PyRep::Integer(laser_1_id)], &kw, None).await
    .expect("OverloadRack");
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "StopOverloadRack", &[PyRep::Integer(laser_1_id)], &kw, None).await
    .expect("StopOverloadRack");
    let _ = sess.drain_notifications();
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "StopOverload", &[PyRep::Integer(laser_1_id)], &kw, None).await
    .expect("StopOverload");
    assert!(sess.drain_notifications().iter().any(|(n, _)| n == "OnHeatRemoved"));

    // Step 5s with no overloaded modules -> rack heat (1177) dissipates exponentially!
    for _ in 0..5 {
        sys.step_manual(1.0).await.unwrap();
    }
    let ship_info_cooled = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "ItemGetInfo", &[PyRep::Integer(ship_id)], &kw, None).await
        .unwrap();
    let hi_rack_heat_cooled = extract_attr_from_info(&ship_info_cooled, 1177);
    assert!(
        hi_rack_heat_cooled < hi_rack_heat,
        "Rack heat must dissipate exponentially when no modules in the rack are overloaded: {hi_rack_heat_cooled} < {hi_rack_heat}"
    );

    // 2. Verify T2 crystal volatility damage & destruction at 1.0 HP (feat:modules.t2-crystal-volatility-destruction)
    let loaded_crystal_id = ctx
        .db
        .with_snapshot(|s| {
            s.items
                .values()
                .find(|i| {
                    i.location_id == ship_id
                        && i.flag == InventoryFlag::HiSlot0
                        && i.item_id != laser_1_id
                })
                .map(|i| (i.item_id, i.dynamic_attributes.get(&3).copied().unwrap_or(0.0)))
        })
        .expect("Loaded Conflagration S crystal");
    assert!(
        loaded_crystal_id.1 > 0.0,
        "Conflagration S must accumulate volatility damage per cycle, got {}",
        loaded_crystal_id.1
    );
    // Set crystal damage to 0.995 so the next cycle shatters it (>= 1.0 HP)
    ctx.db.with_mut_snapshot(|s| {
        if let Some(c_it) = s.items.get_mut(&loaded_crystal_id.0) {
            c_it.dynamic_attributes.insert(3, 0.995);
        }
    });
    for _ in 0..4 {
        sys.step_manual(1.0).await.unwrap();
    }
    let crystal_still_exists = ctx
        .db
        .with_snapshot(|s| s.items.contains_key(&loaded_crystal_id.0));
    assert!(
        !crystal_still_exists,
        "Conflagration S crystal must shatter and be destroyed when volatility damage reaches 1.0 HP"
    );

    // Deactivate laser_1_id so we can repair it with Nanite Repair Paste
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Deactivate",
        &[
            PyRep::Integer(laser_1_id),
            PyRep::String("targetAttack".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Deactivate laser_1_id");

    // 3. Verify Nanite Repair Paste consumption, RepairModule, StopRepairModule, and burnout rejection (feat:fitting.nanite-repair-paste-consumption)
    let paste_before = ctx
        .db
        .with_snapshot(|s| s.items.get(&paste_id).map(|i| i.quantity).unwrap_or(0));
    let _ = sess.drain_notifications();
    let rep_res = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "RepairModule", &[PyRep::Integer(ship_id), PyRep::Integer(laser_1_id)], &kw, None).await
        .expect("RepairModule on damaged laser_1_id");
    assert!(
        sess.drain_notifications()
            .iter()
            .any(|(n, _)| n == "OnMultiModuleRepairStarted")
    );
    let paste_consumed = rep_res
        .dict_get("pasteConsumed")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let paste_after = ctx
        .db
        .with_snapshot(|s| s.items.get(&paste_id).map(|i| i.quantity).unwrap_or(0));
    assert!(
        paste_consumed >= 1 && paste_after == paste_before - (paste_consumed as i32),
        "RepairModule must consume Nanite Repair Paste from ship cargo"
    );

    // Step 10s so nanite repair completes and zeroes module damage
    for _ in 0..10 {
        sys.step_manual(1.0).await.unwrap();
    }
    let l1_info_repaired = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "ItemGetInfo", &[PyRep::Integer(laser_1_id)], &kw, None).await
        .unwrap();
    assert_eq!(
        extract_attr_from_info(&l1_info_repaired, 3),
        0.0,
        "Completed nanite repair must restore module damage (3) to 0.0"
    );

    // Start repair on laser_2_id and test StopRepairModule
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "RepairModule", &[PyRep::Integer(ship_id), PyRep::Integer(laser_2_id)], &kw, None).await
    .expect("RepairModule on laser_2_id");
    let _ = sess.drain_notifications();
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "StopRepairModule", &[PyRep::Integer(ship_id), PyRep::Integer(laser_2_id)], &kw, None).await
    .expect("StopRepairModule on laser_2_id");
    assert!(
        sess.drain_notifications()
            .iter()
            .any(|(n, _)| n == "OnRepairDone")
    );

    // Completely burned-out module (damage == 40.0 HP) must be rejected by RepairModule with ModuleBurnedOut
    ctx.db.with_mut_snapshot(|s| {
        if let Some(m_it) = s.items.get_mut(&laser_2_id) {
            m_it.dynamic_attributes.insert(3, 40.0);
        }
    });
    let burn_rep_err = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "RepairModule", &[PyRep::Integer(ship_id), PyRep::Integer(laser_2_id)], &kw, None).await;
    assert_eq!(
        burn_rep_err
            .expect_err("Completely burned-out module cannot be repaired with Nanite Repair Paste in space")
            .code,
        "ModuleBurnedOut"
    );
}
