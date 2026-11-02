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

    // 4. Scripted module script loading & swapping (feat:modules.scripts-swap)
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
