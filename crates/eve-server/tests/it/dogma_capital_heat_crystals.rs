//! Phase 4 Dogma and Fitting integration tests (`DG-2`..`DG-6`, Part 1).

use crate::common;

use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::attr_ids;
use eve_server::router::ServerContext;
use std::collections::HashMap;

pub(crate) fn seed_item(
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

pub(crate) fn grant_all_required_skills_for_type(ctx: &ServerContext, char_id: i64, type_id: u32) {
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

pub(crate) fn extract_attr_from_info(info_rep: &PyRep, attr_id: i64) -> f64 {
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
    // parity: feat:fitting.ammo-load, feat:fitting.ammo-unload, feat:fitting.weapon-grouping, feat:modules.scripts-swap, rpc:dogmaIM.LoadAmmo, rpc:dogmaIM.UnloadAmmo, rpc:dogmaIM.LinkWeapons, rpc:dogmaIM.LinkAllWeapons, rpc:dogmaIM.UnlinkModule, rpc:dogmaIM.UnlinkAllModules, rpc:dogmaIM.PeelAndLink, rpc:dogmaIM.MergeModuleGroups, rpc:dogmaIM.DestroyWeaponBank, rpc:charUnboundMgr.SelectCharacterID
    eve_testkit::attest(&[
        "feat:fitting.ammo-load",
        "feat:fitting.ammo-unload",
        "feat:fitting.weapon-grouping",
        "feat:modules.scripts-swap",
        "rpc:dogmaIM.LoadAmmo",
        "rpc:dogmaIM.UnloadAmmo",
        "rpc:dogmaIM.LinkWeapons",
        "rpc:dogmaIM.LinkAllWeapons",
        "rpc:dogmaIM.UnlinkModule",
        "rpc:dogmaIM.UnlinkAllModules",
        "rpc:dogmaIM.PeelAndLink",
        "rpc:dogmaIM.MergeModuleGroups",
        "rpc:dogmaIM.DestroyWeaponBank",
    ]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(4004);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
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

    // 1. Undock and exercise all 7 weapon-grouping RPCs
    ctx.advance_secs(31);
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock before weapon-grouping RPCs");
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
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "UnlinkModule",
        &[PyRep::Integer(ship_id), PyRep::Integer(t3_id)],
        &kw,
        None,
    )
    .await
    .expect("UnlinkModule");
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "DestroyWeaponBank",
        &[PyRep::Integer(ship_id), PyRep::Integer(t1_id)],
        &kw,
        None,
    )
    .await
    .expect("DestroyWeaponBank");
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "LinkAllWeapons",
        &[PyRep::Integer(ship_id)],
        &kw,
        None,
    )
    .await
    .expect("LinkAllWeapons");
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "UnlinkAllModules",
        &[PyRep::Integer(ship_id)],
        &kw,
        None,
    )
    .await
    .expect("UnlinkAllModules");
    // Re-link all 3 turrets into one bank for grouped ammo loading
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "LinkAllWeapons",
        &[PyRep::Integer(ship_id)],
        &kw,
        None,
    )
    .await
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

    // Verify Fusion S charge modifier (effect 596: ammoInfluenceRange, PreMul 0.5 on maxRange 54),
    // accounting for the pilot's seeded Sharpshooter (3311) range bonus (+5%/level via attr 294).
    let base_max_range = ctx.sde.types[&2889].default_attributes[&attr_ids::MAX_RANGE];
    let sharpshooter_lvl = ctx.character_skill_levels(char_id).get(&3311).copied().unwrap_or(0) as f64;
    let sharpshooter_pct = ctx.sde.types[&3311].default_attributes[&294];
    let expected_unloaded_range = base_max_range * (1.0 + (sharpshooter_pct / 100.0) * sharpshooter_lvl);
    let fusion_range_mult = ctx.sde.types[&183].default_attributes[&120];
    let expected_fusion_range = expected_unloaded_range * fusion_range_mult;
    let fit_fusion = ctx
        .get_or_rebuild_ship_fitting(char_id, ship_id)
        .expect("fit_fusion");
    let mod_state_fusion =
        fit_fusion.build_effective_module_state(t1_id, 2889, false, &ctx.sde);
    let range_with_fusion = mod_state_fusion.get_attribute(attr_ids::MAX_RANGE, &ctx.sde);
    assert!(
        (range_with_fusion - expected_fusion_range).abs() < 0.1,
        "Expected Fusion S to halve 200mm AutoCannon II maxRange from {expected_unloaded_range} to {expected_fusion_range}, got {range_with_fusion}"
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
        (mod_state_unloaded.get_attribute(attr_ids::MAX_RANGE, &ctx.sde) - expected_unloaded_range).abs() < 0.1,
        "Unloaded turret maxRange must return to {expected_unloaded_range}"
    );

    // 4. Scripted module script loading & swapping (feat:modules.scripts-swap)
    ctx.advance_secs(31);
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        None,
        "CmdDock",
        &[PyRep::Integer(station_id)],
        &kw,
        None,
    )
    .await
    .expect("CmdDock before fitting Tracking Computer II");
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

    // Swap directly to Tracking Speed Script (29001) via LoadAmmo
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

// Folded submodule: cap_crystals_ext_2
#[allow(unused_imports, dead_code)]
mod cap_crystals_ext_2 {
    use super::*;

// Phase 4 Dogma Capital Siege/Triage/Bastion/Industrial Core/Phenomena/ZPME tests (`DG-4`, Part 2).

use crate::common;
use crate::dogma_capital_heat_crystals::*;

use eve_db::InventoryFlag;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::attr_ids;
use eve_server::router::ServerContext;
use std::collections::HashMap;

#[tokio::test]
async fn test_dg4_siege_triage_bastion_industrial_core_phenomena_and_zpme() {
    // parity: feat:modules.siege-module, feat:modules.triage-module, feat:modules.bastion-module, feat:modules.industrial-core, feat:modules.phenomena-generator, feat:modules.zero-point-mass-entangler, rpc:charUnboundMgr.SelectCharacterID
    eve_testkit::attest(&[
        "feat:modules.siege-module",
        "feat:modules.triage-module",
        "feat:modules.bastion-module",
        "feat:modules.industrial-core",
        "feat:modules.phenomena-generator",
        "feat:modules.zero-point-mass-entangler",
    ]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(4203);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
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
    assert!(
        stront_after_siege == 1_750 || stront_after_siege == 1_875,
        "Siege Module I must consume Strontium Clathrates on activation (got {stront_after_siege})"
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
    assert!(
        stront_after_triage == 1_500 || stront_after_triage == 1_800,
        "Triage Module I must consume Strontium Clathrates on activation (got {stront_after_triage})"
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
        snap.items.retain(|_, i| !(i.location_id == ship_id && matches!(i.type_id, 16274 | 16275)));
        snap.items.insert(
            901_000_991,
            eve_db::InventoryItemRecord {
                item_id: 901_000_991,
                type_id: 16274,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 50_000,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    ctx.invalidate_ship_fitting(ship_id);
    let ship_info_before_phenom = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "ItemGetInfo",
            &[PyRep::Integer(ship_id)],
            &kw,
            None,
        )
        .await
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
    let ship_info_phenom = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "ItemGetInfo",
            &[PyRep::Integer(ship_id)],
            &kw,
            None,
        )
        .await
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
    let ship_info_after_phenom = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "ItemGetInfo",
            &[PyRep::Integer(ship_id)],
            &kw,
            None,
        )
        .await
        .expect("ItemGetInfo after Deactivate Amarr Phenomena Generator");
    let cap_recharge_after_phenom = extract_attr_from_info(&ship_info_after_phenom, 55);
    let kin_res_after_phenom = extract_attr_from_info(&ship_info_after_phenom, 269);
    assert!(
        (cap_recharge_after_phenom - cap_recharge_before_phenom).abs() < 1e-3
            && (kin_res_after_phenom - kin_res_before_phenom).abs() < 1e-4,
        "Deactivating Phenomena Generator must clear dbuffs cleanly (cap {cap_recharge_after_phenom} vs {cap_recharge_before_phenom}, kin {kin_res_after_phenom} vs {kin_res_before_phenom})"
    );

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

}

// Folded submodule: cap_crystals_ext_3
#[allow(unused_imports, dead_code)]
mod cap_crystals_ext_3 {
    use super::*;

// Phase 4 Dogma Heat Rack Thermodynamics, Nanite Repair, & T2 Crystal Volatility tests (`DG-6`, Part 3).

use crate::common;
use crate::dogma_capital_heat_crystals::*;

use eve_db::InventoryFlag;
use eve_destiny::{Ball, CombatState, DVec3, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

#[tokio::test]
async fn test_dg6_heat_rack_thermodynamics_nanite_repair_and_crystal_volatility() {
    // parity: feat:dogma.heat-rack-thermodynamics, feat:fitting.nanite-repair-paste-consumption, feat:modules.t2-crystal-volatility-destruction, notify:OnHeatAdded, notify:OnHeatRemoved, notify:OnMultiModuleRepairStarted, notify:OnRepairDone, rpc:charUnboundMgr.SelectCharacterID, rpc:dogmaIM.LoadAmmo
    eve_testkit::attest(&[
        "feat:dogma.heat-rack-thermodynamics",
        "feat:fitting.nanite-repair-paste-consumption",
        "feat:modules.t2-crystal-volatility-destruction",
        "notify:OnHeatAdded",
        "notify:OnHeatRemoved",
        "notify:OnMultiModuleRepairStarted",
        "notify:OnRepairDone",
    ]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(4204);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    let char_id = 90_000_001i64;
    let ship_id = 100_000_001i64;
    let station_id = 60_003_760i64;

    // Upgrade ship to Punisher (597, Amarr Frigate) and fit two Small Focused Pulse Laser II (3041) in HiSlot0 & HiSlot1
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|&id, it| id == ship_id || it.location_id != ship_id);
        if let Some(s) = snap.items.get_mut(&ship_id) {
            s.type_id = 597;
        }
    });
    ctx.invalidate_ship_fitting(ship_id);

    for tid in [597u32, 3041, 12565, 28668, 1319] {
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

    // Populate low slots with online passive modules so Online Slots / Total Slots reflects a combat-fitted frigate
    for (idx, flag) in [
        InventoryFlag::LoSlot0,
        InventoryFlag::LoSlot1,
        InventoryFlag::LoSlot2,
        InventoryFlag::LoSlot3,
        InventoryFlag::LoSlot4,
    ]
    .into_iter()
    .enumerate()
    {
        let mod_id = 100_002_350 + idx as i64;
        seed_item(&ctx, mod_id, 1319, char_id, ship_id, flag, 1, true);
        ctx.db.with_mut_snapshot(|s| {
            s.module_online_states.insert(mod_id, true);
        });
    }

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
    sys.set_capacitor(ship_id, 50_000.0).await.unwrap();
    let (_, ship_ball) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();

    // Spawn target dummy at 2 km and lock it
    let target_id = 100_002_320i64;
    let mut target_cs = CombatState::default();
    target_cs.max_shield_hp = 500_000.0;
    target_cs.shield_hp = 500_000.0;
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

    ctx.set_mechanic_rng_seed(Some(7));
    ctx.db.with_mut_snapshot(|s| {
        if let Some(sh) = s.items.get_mut(&ship_id) {
            sh.dynamic_attributes.insert(1175, 0.85);
        }
    });

    // 1. Activate laser_1_id with Overload via dogmaIM.Overload and test OverloadRack / StopOverloadRack / StopOverload
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
    assert!(
        notes_ovl.iter().any(|(n, args)| {
            n == "OnHeatAdded" && args == &vec![PyRep::Integer(1175), PyRep::Integer(laser_1_id)]
        }),
        "expected OnHeatAdded([1175, laser_1_id]), got: {notes_ovl:?}"
    );

    // Step 20s of overloaded weapon cycles -> high rack heat (1175) stays above 0.85,
    // laser_1_id takes more probabilistic heat hits than attenuated adjacent laser_2_id,
    // and loaded Conflagration S crystal accumulates crystalVolatilityDamage!
    for _ in 0..20 {
        sys.set_capacitor(ship_id, 50_000.0).await.unwrap();
        sys.step_manual(1.0).await.unwrap();
    }

    let ship_info_heated = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "ItemGetInfo",
            &[PyRep::Integer(ship_id)],
            &kw,
            None,
        )
        .await
        .unwrap();
    let hi_rack_heat = extract_attr_from_info(&ship_info_heated, 1175);
    assert!(
        hi_rack_heat > 0.45,
        "High rack heat (1175) must accumulate during overloaded weapon cycles, got {hi_rack_heat}"
    );

    let l1_info = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "ItemGetInfo",
            &[PyRep::Integer(laser_1_id)],
            &kw,
            None,
        )
        .await
        .unwrap();
    let l2_info = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "ItemGetInfo",
            &[PyRep::Integer(laser_2_id)],
            &kw,
            None,
        )
        .await
        .unwrap();
    let l1_dmg = extract_attr_from_info(&l1_info, 3);
    let l2_dmg = extract_attr_from_info(&l2_info, 3);
    assert!(
        l1_dmg > 0.0 && l2_dmg > 0.0 && l1_dmg > l2_dmg,
        "Overloaded laser_1 ({l1_dmg}) and adjacent laser_2 ({l2_dmg}) must take probabilistic heat damage with slot-distance attenuation"
    );

    // Verify OverloadRack, StopOverloadRack, and StopOverload RPCs
    let _ = sess.drain_notifications();
    let ov_rack = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "OverloadRack", &[PyRep::Integer(laser_1_id)], &kw, None)
        .await
        .expect("OverloadRack");
    assert_eq!(ov_rack, PyRep::List(vec![PyRep::Integer(laser_2_id)]));
    let ov_rack_notes = sess.drain_notifications();
    assert!(
        ov_rack_notes.iter().any(|(n, args)| {
            n == "OnHeatAdded" && args == &vec![PyRep::Integer(1175), PyRep::Integer(laser_2_id)]
        }),
        "expected OnHeatAdded([1175, laser_2_id]), got: {ov_rack_notes:?}"
    );

    let stop_rack = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "StopOverloadRack", &[PyRep::Integer(laser_1_id)], &kw, None)
        .await
        .expect("StopOverloadRack");
    assert_eq!(
        stop_rack,
        PyRep::List(vec![PyRep::Integer(laser_1_id), PyRep::Integer(laser_2_id)])
    );
    let stop_rack_notes = sess.drain_notifications();
    assert!(
        stop_rack_notes.iter().any(|(n, args)| {
            n == "OnHeatRemoved" && args == &vec![PyRep::Integer(1175), PyRep::Integer(laser_1_id)]
        }) && stop_rack_notes.iter().any(|(n, args)| {
            n == "OnHeatRemoved" && args == &vec![PyRep::Integer(1175), PyRep::Integer(laser_2_id)]
        }),
        "expected OnHeatRemoved([1175, laser_1_id/laser_2_id]), got: {stop_rack_notes:?}"
    );

    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "StopOverload", &[PyRep::Integer(laser_1_id)], &kw, None)
        .await
        .expect("StopOverload");
    assert!(sess.drain_notifications().iter().any(|(n, args)| {
        n == "OnHeatRemoved" && args == &vec![PyRep::Integer(1175), PyRep::Integer(laser_1_id)]
    }));

    // Step 5s with no overloaded modules -> high rack heat (1175) dissipates exponentially!
    for _ in 0..5 {
        sys.step_manual(1.0).await.unwrap();
    }
    let ship_info_cooled = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "ItemGetInfo",
            &[PyRep::Integer(ship_id)],
            &kw,
            None,
        )
        .await
        .unwrap();
    let hi_rack_heat_cooled = extract_attr_from_info(&ship_info_cooled, 1175);
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

    // 3. Verify Nanite Repair Paste consumption, RepairModule, StopRepairModule, and burnout rejection
    let paste_before = ctx
        .db
        .with_snapshot(|s| s.items.get(&paste_id).map(|i| i.quantity).unwrap_or(0));
    let _ = sess.drain_notifications();
    let rep_res = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "RepairModule",
            &[PyRep::Integer(ship_id), PyRep::Integer(laser_1_id)],
            &kw,
            None,
        )
        .await
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
        "RepairModule must reserve Nanite Repair Paste from ship cargo"
    );

    for _ in 0..10 {
        sys.step_manual(1.0).await.unwrap();
    }
    let l1_info_repaired = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "ItemGetInfo",
            &[PyRep::Integer(laser_1_id)],
            &kw,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        extract_attr_from_info(&l1_info_repaired, 3),
        0.0,
        "Completed nanite repair must restore module damage (3) to 0.0"
    );

    // Start repair on laser_2_id and test StopRepairModule
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "RepairModule",
        &[PyRep::Integer(ship_id), PyRep::Integer(laser_2_id)],
        &kw,
        None,
    )
    .await
    .expect("RepairModule on laser_2_id");
    let _ = sess.drain_notifications();
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "StopRepairModule",
        &[PyRep::Integer(ship_id), PyRep::Integer(laser_2_id)],
        &kw,
        None,
    )
    .await
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
    let burn_rep_err = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "RepairModule",
            &[PyRep::Integer(ship_id), PyRep::Integer(laser_2_id)],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        burn_rep_err
            .expect_err("Completely burned-out module cannot be repaired with Nanite Repair Paste in space")
            .code,
        "ModuleBurnedOut"
    );
}

#[tokio::test]
async fn combat_weapons_overload_and_preoverloaded_activate_emit_on_heat_added() {
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(4205);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    let char_id = 90_000_001i64;
    let ship_id = 100_000_001i64;
    let lowsec_sys_id = 30_002_813u32; // Tama (lowsec 0.3) so Upwell structure attack routes cleanly via handle_weapons
    let station_id = 60_005_203i64; // Tama VII - Moon 9

    // Upgrade ship to Rupture (629, Minmatar Cruiser) docked at Tama and fit two 200mm AutoCannon II (2889)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|&id, it| id == ship_id || it.location_id != ship_id);
        if let Some(s) = snap.items.get_mut(&ship_id) {
            s.type_id = 629;
            s.location_id = station_id;
        }
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(station_id);
            ch.solar_system_id = lowsec_sys_id;
            ch.active_ship_id = ship_id;
        }
    });
    sess.station_id = Some(station_id);
    sess.solar_system_id = Some(lowsec_sys_id);
    ctx.invalidate_ship_fitting(ship_id);

    for tid in [
        629u32, 2889, 185, 499, 210, 1547, 12709, 54731, 54739, 54769, 85087, 85084, 85088, 28164,
    ] {
        grant_all_required_skills_for_type(&ctx, char_id, tid);
    }
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            28164,
            eve_db::TrainedSkillRecord {
                type_id: 28164,
                level: 4,
                skill_points: 181_020.0,
            },
        );
    });

    let ac1_id = 100_002_401i64;
    let ac2_id = 100_002_402i64;
    let ammo1_id = 100_002_403i64;
    let ammo2_id = 100_002_404i64;
    let tp_id = 100_002_405i64;
    seed_item(&ctx, ac1_id, 2889, char_id, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, ac2_id, 2889, char_id, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, tp_id, 12709, char_id, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, ammo1_id, 185, char_id, ship_id, InventoryFlag::Cargo, 80, false);
    seed_item(&ctx, ammo2_id, 185, char_id, ship_id, InventoryFlag::Cargo, 80, false);

    for (mid, flag) in [
        (ac1_id, InventoryFlag::HiSlot0),
        (ac2_id, InventoryFlag::HiSlot1),
        (tp_id, InventoryFlag::MedSlot0),
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
        .expect("Fit module on Rupture");
    }

    for (mid, aid) in [(ac1_id, ammo1_id), (ac2_id, ammo2_id)] {
        ctx.dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "LoadAmmo",
            &[
                PyRep::Integer(ship_id),
                PyRep::List(vec![PyRep::Integer(mid)]),
                PyRep::List(vec![PyRep::Integer(aid)]),
                PyRep::Integer(ship_id),
            ],
            &kw,
            None,
        )
        .await
        .expect("Load EMP S into 200mm AutoCannon II");
    }

    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    ctx.dispatch_rpc(
        &mut sess,
        "crimewatch",
        None,
        "SetSafetyLevel",
        &[PyRep::Integer(0)],
        &kw,
        None,
    )
    .await
    .expect("SetSafetyLevel Red");

    let sys = ctx.systems.get_or_spawn(lowsec_sys_id);
    sys.set_capacitor(ship_id, 50_000.0).await.unwrap();
    let (_, ship_ball) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();

    // Hostile Upwell Structure (Astrahus 35832) in Tama so weapons & Smartbomb route directly via combat_weapons::handle_weapons
    let target_id = 1_000_002_420_001i64;
    let enemy_ship_id = 100_002_421i64;
    let enemy_char_id = 90_000_099i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            target_id,
            eve_db::UpwellStructureRecord {
                structure_id: target_id,
                solar_system_id: lowsec_sys_id,
                type_id: 35_832,
                owner_corp_id: 98_999_001,
                state: "ShieldVulnerable".into(),
                shield_hp: 500_000.0,
                armor_hp: 500_000.0,
                hull_hp: 500_000.0,
                ..Default::default()
            },
        );
        let mut ec = snap.characters.get(&char_id).cloned().unwrap();
        ec.character_id = enemy_char_id;
        ec.corporation_id = 98_999_001;
        ec.active_ship_id = enemy_ship_id;
        ec.station_id = None;
        ec.solar_system_id = lowsec_sys_id;
        snap.characters.insert(enemy_char_id, ec);
    });
    let mut target_cs = CombatState::default();
    target_cs.max_shield_hp = 500_000.0;
    target_cs.shield_hp = 500_000.0;
    sys.spawn_ball_with_combat(
        Ball {
            id: target_id,
            type_id: 35_832,
            position: ship_ball.position + DVec3::new(2_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 100_000_000.0,
            radius: 1_500.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        target_cs.clone(),
    )
    .await
    .unwrap();
    target_cs.signature_radius = 40.0;
    sys.spawn_ball_with_combat(
        Ball {
            id: enemy_ship_id,
            type_id: 587,
            position: ship_ball.position + DVec3::new(5_000.0, 0.0, 0.0),
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
    sys.add_target_lock(
        ship_id,
        eve_destiny::TargetLock {
            target_id: enemy_ship_id,
            lock_time_ms: 0.0,
            remaining_lock_ms: 0.0,
            locked: true,
            max_range_m: Some(50_000.0),
        },
    )
    .await
    .unwrap();

    // 1. Direct dogmaIM.Overload on turret routed via combat_weapons::handle_weapons emits OnHeatAdded
    let _ = sess.drain_notifications();
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Overload",
        &[
            PyRep::Integer(ac1_id),
            PyRep::String("projectileFired".into()),
            PyRep::Integer(target_id),
            PyRep::Integer(1),
        ],
        &kw,
        None,
    )
    .await
    .expect("Overload ac1_id");
    let notes_ovl = sess.drain_notifications();
    assert!(
        notes_ovl.iter().any(|(n, args)| {
            n == "OnHeatAdded" && args == &vec![PyRep::Integer(1175), PyRep::Integer(ac1_id)]
        }),
        "expected OnHeatAdded([1175, ac1_id]) from dogmaIM.Overload on combat weapon, got: {notes_ovl:?}"
    );
    assert_eq!(
        ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap().modules[&ac1_id].state,
        eve_dogma::ModuleState::Overloaded
    );

    // 2. Pre-overload idle ac2_id via OverloadRack; failed Activate preserves ModuleState::Overloaded,
    //    then succeeding Activate emits OnHeatAdded([1175, ac2_id])
    let rack_add = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "OverloadRack",
            &[PyRep::Integer(ac2_id)],
            &kw,
            None,
        )
        .await
        .expect("OverloadRack ac2_id");
    assert_eq!(rack_add, PyRep::List(vec![PyRep::Integer(ac2_id)]));
    let _ = sess.drain_notifications();

    let unlocked_err = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[
                PyRep::Integer(ac2_id),
                PyRep::String("projectileFired".into()),
                PyRep::Integer(999_888_777),
                PyRep::Integer(1),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("unlocked target must fail");
    assert_eq!(unlocked_err.code, "TargetNotLocked");
    assert_eq!(
        ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap().modules[&ac2_id].state,
        eve_dogma::ModuleState::Overloaded,
        "Failed Activate on pre-overloaded turret must preserve ModuleState::Overloaded"
    );

    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(ac2_id),
            PyRep::String("projectileFired".into()),
            PyRep::Integer(target_id),
            PyRep::Integer(1),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate pre-overloaded ac2_id");
    let notes_act = sess.drain_notifications();
    assert!(
        notes_act.iter().any(|(n, args)| {
            n == "OnHeatAdded" && args == &vec![PyRep::Integer(1175), PyRep::Integer(ac2_id)]
        }),
        "expected OnHeatAdded([1175, ac2_id]) when activating pre-overloaded turret, got: {notes_act:?}"
    );
    assert_eq!(
        ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap().modules[&ac2_id].state,
        eve_dogma::ModuleState::Overloaded,
        "Activate on pre-overloaded module must preserve ModuleState::Overloaded"
    );

    // 3. StopOverloadRack picks up both active overloaded cycling turrets cleanly
    let rack_stop = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "StopOverloadRack",
            &[PyRep::Integer(ac1_id)],
            &kw,
            None,
        )
        .await
        .expect("StopOverloadRack");
    assert_eq!(
        rack_stop,
        PyRep::List(vec![PyRep::Integer(ac1_id), PyRep::Integer(ac2_id)])
    );
    let stop_notes = sess.drain_notifications();
    for mid in [ac1_id, ac2_id] {
        assert!(
            stop_notes.iter().any(|(n, args)| {
                n == "OnHeatRemoved" && args == &vec![PyRep::Integer(1175), PyRep::Integer(mid)]
            }),
            "expected OnHeatRemoved([1175, {mid}]), got: {stop_notes:?}"
        );
    }

    // 4. Missile Launcher (499 -> handle_weapons), Smartbomb (1547 -> handle_weapons), and Med-slot EWAR Target Painter (12709)
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(ac1_id)], &kw, None)
        .await
        .expect("Deactivate ac1_id");
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(ac2_id)], &kw, None)
        .await
        .expect("Deactivate ac2_id");
    ctx.clear_all_reactivation_delays();

    let lml_id = 100_002_406i64;
    let lml_ammo_id = 100_002_407i64;
    let sb_id = 100_002_408i64;
    seed_item(&ctx, lml_id, 499, char_id, ship_id, InventoryFlag::HiSlot2, 1, true);
    seed_item(&ctx, lml_ammo_id, 210, char_id, ship_id, InventoryFlag::HiSlot2, 40, false);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(lml_id, true);
    });
    ctx.invalidate_ship_fitting(ship_id);

    // Check overheat dogma attributes on ship (1224 heatGenerationMultiplier) and launcher (1180, 1205)
    let ship_info = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "ItemGetInfo", &[PyRep::Integer(ship_id)], &kw, None)
        .await
        .unwrap();
    let lml_info = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "ItemGetInfo", &[PyRep::Integer(lml_id)], &kw, None)
        .await
        .unwrap();
    assert!(extract_attr_from_info(&ship_info, 1224) > 0.0, "ship must expose attr 1224 (heatGenerationMultiplier)");
    assert!(extract_attr_from_info(&lml_info, 1180) > 0.0, "module must expose attr 1180 (heatAbsorbtionRateModifier)");
    assert!(extract_attr_from_info(&lml_info, 1205) < 0.0, "launcher must expose attr 1205 (overloadRofBonus)");

    // Missile Overload & pre-overloaded Activate via handle_weapons -> OnHeatAdded([1175, lml_id])
    let _ = sess.drain_notifications();
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Overload",
        &[PyRep::Integer(lml_id), PyRep::String("useMissiles".into()), PyRep::Integer(target_id), PyRep::Integer(1)],
        &kw,
        None,
    )
    .await
    .expect("Missile Overload vs Upwell");
    assert!(
        sess.drain_notifications().iter().any(|(n, a)| n == "OnHeatAdded" && a == &vec![PyRep::Integer(1175), PyRep::Integer(lml_id)]),
        "Missile Overload must emit OnHeatAdded([1175, lml_id])"
    );
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(lml_id)], &kw, None)
        .await
        .expect("Deactivate missile");
    ctx.clear_all_reactivation_delays();
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "OverloadRack", &[PyRep::Integer(lml_id)], &kw, None)
        .await
        .expect("Pre-overload missile");
    let _ = sess.drain_notifications();
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(lml_id), PyRep::String("useMissiles".into()), PyRep::Integer(target_id), PyRep::Integer(1)],
        &kw,
        None,
    )
    .await
    .expect("Pre-overloaded Missile Activate vs Upwell");
    assert!(
        sess.drain_notifications().iter().any(|(n, a)| n == "OnHeatAdded" && a == &vec![PyRep::Integer(1175), PyRep::Integer(lml_id)]),
        "Pre-overloaded Missile Activate must emit OnHeatAdded([1175, lml_id])"
    );
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(lml_id)], &kw, None)
        .await
        .expect("Deactivate missile before smartbomb");
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "StopOverloadRack", &[PyRep::Integer(ac1_id)], &kw, None)
        .await
        .ok();

    // Seed Smartbomb only after missile test so direct Overload tests clean Online -> Overloaded transition
    seed_item(&ctx, sb_id, 1547, char_id, ship_id, InventoryFlag::HiSlot3, 1, true);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(sb_id, true);
        if let Some(fi) = snap.ship_fit_inputs.get_mut(&ship_id) {
            fi.overloaded_modules.remove(&ac1_id);
            fi.overloaded_modules.remove(&ac2_id);
            fi.overloaded_modules.remove(&lml_id);
        }
    });
    ctx.invalidate_ship_fitting(ship_id);

    // AoE Smartbomb (1547 -> handle_weapons via Upwell in system) Overload & pre-overloaded Activate -> OnHeatAdded([1175, sb_id])
    let _ = sess.drain_notifications();
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Overload", &[PyRep::Integer(sb_id)], &kw, None)
        .await
        .expect("Smartbomb Overload");
    assert!(
        sess.drain_notifications().iter().any(|(n, a)| n == "OnHeatAdded" && a == &vec![PyRep::Integer(1175), PyRep::Integer(sb_id)]),
        "Smartbomb Overload must emit OnHeatAdded([1175, sb_id])"
    );
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(sb_id)], &kw, None)
        .await
        .expect("Deactivate smartbomb");
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "OverloadRack", &[PyRep::Integer(sb_id)], &kw, None)
        .await
        .expect("Pre-overload smartbomb");
    let _ = sess.drain_notifications();
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(sb_id)], &kw, None)
        .await
        .expect("Pre-overloaded Smartbomb Activate");
    assert!(
        sess.drain_notifications().iter().any(|(n, a)| n == "OnHeatAdded" && a == &vec![PyRep::Integer(1175), PyRep::Integer(sb_id)]),
        "Pre-overloaded Smartbomb Activate must emit OnHeatAdded([1175, sb_id])"
    );
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(sb_id)], &kw, None)
        .await
        .expect("Deactivate smartbomb after test");

    // EWAR Target Painter (MedSlot0 -> heatMed 1176) Overload & pre-overloaded Activate -> OnHeatAdded([1176, tp_id])
    let _ = sess.drain_notifications();
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Overload",
        &[PyRep::Integer(tp_id), PyRep::String("targetAttack".into()), PyRep::Integer(enemy_ship_id)],
        &kw,
        None,
    )
    .await
    .expect("Target Painter Overload");
    assert!(
        sess.drain_notifications().iter().any(|(n, a)| n == "OnHeatAdded" && a == &vec![PyRep::Integer(1176), PyRep::Integer(tp_id)]),
        "EWAR Target Painter Overload must emit OnHeatAdded([1176, tp_id])"
    );
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(tp_id)], &kw, None)
        .await
        .expect("Deactivate Target Painter");
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "OverloadRack", &[PyRep::Integer(tp_id)], &kw, None)
        .await
        .expect("Pre-overload Target Painter");
    let _ = sess.drain_notifications();
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(tp_id), PyRep::String("targetAttack".into()), PyRep::Integer(enemy_ship_id)],
        &kw,
        None,
    )
    .await
    .expect("Pre-overloaded Target Painter Activate");
    assert!(
        sess.drain_notifications().iter().any(|(n, a)| n == "OnHeatAdded" && a == &vec![PyRep::Integer(1176), PyRep::Integer(tp_id)]),
        "Pre-overloaded Target Painter Activate must emit OnHeatAdded([1176, tp_id])"
    );
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(tp_id)], &kw, None)
        .await
        .expect("Deactivate Target Painter after test");

    // 5. Burned-out weapon & reactivation delay rejections via handle_weapons
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ac1_id).unwrap().dynamic_attributes.insert(3, 40.0);
    });
    ctx.invalidate_ship_fitting(ship_id);
    assert_eq!(
        ctx.dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(ac1_id), PyRep::String("projectileFired".into()), PyRep::Integer(target_id)],
            &kw,
            None,
        )
        .await
        .unwrap_err()
        .code,
        "ModuleBurnedOut"
    );
    assert_eq!(
        ctx.dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Overload",
            &[PyRep::Integer(ac1_id), PyRep::String("projectileFired".into()), PyRep::Integer(target_id)],
            &kw,
            None,
        )
        .await
        .unwrap_err()
        .code,
        "ModuleBurnedOut"
    );

    ctx.record_module_reactivation_delay(ship_id, ac2_id, 10.0);
    assert_eq!(
        ctx.dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Overload",
            &[PyRep::Integer(ac2_id), PyRep::String("projectileFired".into()), PyRep::Integer(target_id)],
            &kw,
            None,
        )
        .await
        .unwrap_err()
        .code,
        "ModuleAlreadyActive"
    );
    ctx.clear_all_reactivation_delays();

    // 6. Vorton Projector (54739, group 4060 -> is_special_target -> handle_weapons) Thermodynamics L0 vs L4,
    //    and non-overloadable Breacher Pod Launcher (85084, group 4807 -> handle_weapons) -> CannotOverloadModule
    let vorton_ship_id = 100_002_450i64;
    let vorton_id = 100_002_451i64;
    let vorton_ammo_id = 100_002_452i64;
    seed_item(&ctx, vorton_ship_id, 54731, char_id, lowsec_sys_id as i64, InventoryFlag::None, 1, true);
    seed_item(&ctx, vorton_id, 54739, char_id, vorton_ship_id, InventoryFlag::HiSlot0, 1, true);
    seed_item(&ctx, vorton_ammo_id, 54769, char_id, vorton_ship_id, InventoryFlag::HiSlot0, 50, false);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(vorton_id, true);
        snap.characters.get_mut(&char_id).unwrap().active_ship_id = vorton_ship_id;
        snap.skills.get_mut(&char_id).unwrap().remove(&28164);
    });
    sess.ship_id = Some(vorton_ship_id);
    ctx.invalidate_ship_fitting(vorton_ship_id);
    let mut v_cs = ctx.build_ship_combat_state(char_id, vorton_ship_id, 54731);
    v_cs.capacitor = 10_000.0;
    v_cs.max_capacitor = 10_000.0;
    sys.spawn_ball_with_combat(
        Ball {
            id: vorton_ship_id,
            type_id: 54731,
            position: ship_ball.position + DVec3::new(100.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 400.0,
            inertia: 2.5,
            mass: 1_000_000.0,
            radius: 30.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        v_cs,
    )
    .await
    .unwrap();
    sys.add_target_lock(
        vorton_ship_id,
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

    assert_eq!(
        ctx.dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Overload",
            &[PyRep::Integer(vorton_id), PyRep::Integer(target_id)],
            &kw,
            None,
        )
        .await
        .unwrap_err()
        .code,
        "DontHaveThermoDynamicsSkillz"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            28164,
            eve_db::TrainedSkillRecord {
                type_id: 28164,
                level: 4,
                skill_points: 181_020.0,
            },
        );
    });
    let _ = sess.drain_notifications();
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Overload",
        &[PyRep::Integer(vorton_id), PyRep::Integer(target_id)],
        &kw,
        None,
    )
    .await
    .expect("Vorton Overload via handle_weapons");
    assert!(
        sess.drain_notifications().iter().any(|(n, a)| n == "OnHeatAdded" && a == &vec![PyRep::Integer(1175), PyRep::Integer(vorton_id)]),
        "Vorton Overload via handle_weapons must emit OnHeatAdded([1175, vorton_id])"
    );

    let tholos_id = 100_002_460i64;
    let breacher_id = 100_002_461i64;
    let breacher_ammo_id = 100_002_462i64;
    seed_item(&ctx, tholos_id, 85087, char_id, lowsec_sys_id as i64, InventoryFlag::None, 1, true);
    seed_item(&ctx, breacher_id, 85084, char_id, tholos_id, InventoryFlag::HiSlot0, 1, true);
    seed_item(&ctx, breacher_ammo_id, 85088, char_id, tholos_id, InventoryFlag::HiSlot0, 10, false);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(breacher_id, true);
        snap.characters.get_mut(&char_id).unwrap().active_ship_id = tholos_id;
    });
    sess.ship_id = Some(tholos_id);
    ctx.invalidate_ship_fitting(tholos_id);
    let mut t_cs = ctx.build_ship_combat_state(char_id, tholos_id, 85087);
    t_cs.capacitor = 10_000.0;
    t_cs.max_capacitor = 10_000.0;
    sys.spawn_ball_with_combat(
        Ball {
            id: tholos_id,
            type_id: 85087,
            position: ship_ball.position + DVec3::new(200.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 400.0,
            inertia: 2.5,
            mass: 1_000_000.0,
            radius: 35.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        t_cs,
    )
    .await
    .unwrap();
    sys.add_target_lock(
        tholos_id,
        eve_destiny::TargetLock {
            target_id: enemy_ship_id,
            lock_time_ms: 0.0,
            remaining_lock_ms: 0.0,
            locked: true,
            max_range_m: Some(50_000.0),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        ctx.dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Overload",
            &[PyRep::Integer(breacher_id), PyRep::Integer(enemy_ship_id)],
            &kw,
            None,
        )
        .await
        .unwrap_err()
        .code,
        "CannotOverloadModule"
    );
}

}
