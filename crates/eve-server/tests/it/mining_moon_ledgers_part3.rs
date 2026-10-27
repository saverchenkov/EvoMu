//! Phase 5M Reprocessing Yield, Tax, Scrapmetal, Structure & Rigs (`MN-12, C-6`).

#![forbid(unsafe_code)]

use super::{get_dict_val, make_item, make_structure, setup_test_session};
use eve_db::{InventoryFlag, TrainedSkillRecord};
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

// ====================================================================
// Test 9: MN-12, C-6: Real Reprocessing Yield, Tax, Scrapmetal, Structure & Rigs
// ====================================================================
#[tokio::test]
async fn test_mn12_c6_reprocessing_yield_tax_modules_and_structure() {
    // parity: feat:mining.reprocessing-yield, feat:mining.reprocessing-tax, feat:mining.reprocessing-modules, feat:structures.service-reprocessing, feat:mining.upwell-refinery-base-yield, feat:structures.reprocessing-rig-security-multipliers, rpc:structureCompressionMgr.CompressItemInStructure
    eve_testkit::attest(&[
        "feat:mining.reprocessing-yield",
        "feat:mining.reprocessing-tax",
        "feat:mining.reprocessing-modules",
        "feat:structures.service-reprocessing",
        "feat:mining.upwell-refinery-base-yield",
        "feat:structures.reprocessing-rig-security-multipliers",
    ]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001;
    let mut session = setup_test_session(&ctx, char_id).await;
    let kw = HashMap::new();

    let station_id = 60003760i64; // Jita IV - 4 (owner corp 1000035 Caldari Navy)
    session.station_id = Some(station_id);

    // 1. Reprocess 1,050 Veldspar (portionSize 100 = 10 portions + 50 remainder) with NPC standing 2.0
    let veld_item_id = 800_000_150i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(1000035, 2.0);
        }
        snap.items.insert(
            veld_item_id,
            make_item(veld_item_id, 1230, char_id, station_id, InventoryFlag::Hangar, 1_050, false),
        );
    });

    let reproc_res = ctx
        .dispatch_rpc(&mut session, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(veld_item_id)], &kw, None)
        .await
        .expect("reprocessingSvc.Reprocess");
    let yield_rate = get_dict_val(&reproc_res, "yieldRate").unwrap().as_f64().unwrap();
    let trit_qty = get_dict_val(&reproc_res, "tritaniumQuantity").unwrap().as_i64().unwrap();
    let tax_rate = get_dict_val(&reproc_res, "stationTaxRate").unwrap().as_f64().unwrap();
    let expected_yield: f64 = 0.50 * 1.15 * 1.10 * 1.10; // 0.69575
    let expected_tax: f64 = 0.05 - 0.0075 * 2.0; // 0.035
    let expected_trit = (10.0_f64 * 400.0 * expected_yield).floor() as i64; // 2783
    assert!((yield_rate - expected_yield).abs() < 1e-6);
    assert!((tax_rate - expected_tax).abs() < 1e-6);
    assert_eq!(trit_qty, expected_trit);

    let (rem_veld_qty, has_tax_journal) = ctx.db.with_snapshot(|snap| {
        let rem = snap.items.get(&veld_item_id).map(|it| it.quantity).unwrap_or(0);
        let has_journal = snap
            .wallet_journal
            .iter()
            .any(|e| e.character_id == char_id && e.entry_type == "ReprocessingTax" && e.amount.0 > 0);
        (rem, has_journal)
    });
    assert_eq!(rem_veld_qty, 50, "Non-multiple remainder of 50 Veldspar must remain in hangar");
    assert!(has_tax_journal, "NPC station reprocessing with tax > 0 must record a ReprocessingTax wallet journal entry");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(1000035, 6.67);
        }
    });
    let quotes_zero_tax = ctx
        .dispatch_rpc(&mut session, "reprocessingSvc", None, "GetQuotes", &[], &kw, None)
        .await
        .expect("GetQuotes with 6.67 standing");
    assert_eq!(get_dict_val(&quotes_zero_tax, "stationTaxRate").unwrap().as_f64(), Some(0.0));

    // 1b. Implant factor (`feat:mining.reprocessing-yield`): Zainou 'Beancounter' Reprocessing RX-804 (27174, +4% attr 379)
    let veld_imp_id = 800_000_152i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_implants.entry(char_id).or_default().push(27174);
        snap.items.insert(
            veld_imp_id,
            make_item(veld_imp_id, 1230, char_id, station_id, InventoryFlag::Hangar, 1_050, false),
        );
    });
    let imp_res = ctx
        .dispatch_rpc(&mut session, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(veld_imp_id)], &kw, None)
        .await
        .expect("Reprocess with RX-804 implant");
    let expected_imp_yield: f64 = 0.50 * 1.15 * 1.10 * 1.10 * 1.04; // 0.72358
    assert!((get_dict_val(&imp_res, "yieldRate").unwrap().as_f64().unwrap() - expected_imp_yield).abs() < 1e-6);
    assert_eq!(
        get_dict_val(&imp_res, "tritaniumQuantity").unwrap().as_i64(),
        Some((10.0_f64 * 400.0 * expected_imp_yield).floor() as i64)
    );
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&veld_imp_id).map(|i| i.quantity)), Some(50));
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_implants.remove(&char_id);
    });

    // 2. Reprocess Modules and Ships with Scrapmetal Processing (`12196`, `feat:mining.reprocessing-modules`):
    for (idx, (scrap_lvl, expected_mod_yield)) in [(0u8, 0.50_f64), (3u8, 0.53_f64), (5u8, 0.55_f64)].into_iter().enumerate() {
        let mod_id = 800_000_151 + idx as i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.skills.entry(char_id).or_default().insert(
                12196,
                TrainedSkillRecord { type_id: 12196, level: scrap_lvl, skill_points: 256_000.0 },
            );
            snap.active_implants.entry(char_id).or_default().push(27174);
            snap.items.insert(mod_id, make_item(mod_id, 482, char_id, station_id, InventoryFlag::Hangar, 1, false));
        });
        let mod_reproc_res = ctx
            .dispatch_rpc(&mut session, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(mod_id)], &kw, None)
            .await
            .expect("Reprocess module");
        let mod_yield = get_dict_val(&mod_reproc_res, "yieldRate").unwrap().as_f64().unwrap();
        assert!((mod_yield - expected_mod_yield).abs() < 1e-6);
        let PyRep::List(mod_outputs) = get_dict_val(&mod_reproc_res, "outputs").unwrap() else { panic!() };
        for mat in ctx.sde.type_materials.get(&482).expect("Miner II (482) SDE typeMaterials") {
            let expected_mat_qty = ((mat.quantity as f64) * expected_mod_yield).floor() as i64;
            if expected_mat_qty > 0 {
                let actual_mat_qty = mod_outputs
                    .iter()
                    .find(|o| get_dict_val(o, "typeID").and_then(|v| v.as_i64()) == Some(mat.material_type_id as i64))
                    .and_then(|o| get_dict_val(o, "quantity"))
                    .and_then(|v| v.as_i64());
                assert_eq!(actual_mat_qty, Some(expected_mat_qty));
            }
        }
    }
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_implants.remove(&char_id);
        snap.items.insert(800_000_155, make_item(800_000_155, 587, char_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(800_000_156, make_item(800_000_156, 587, char_id, station_id, InventoryFlag::Hangar, 1, false));
    });
    let assembled_err = ctx
        .dispatch_rpc(&mut session, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(800_000_155)], &kw, None)
        .await
        .expect_err("Assembled singleton ship must require repackaging before reprocessing");
    assert_eq!(assembled_err.code, "ItemMustBeRepackaged");
    let ship_reproc_res = ctx
        .dispatch_rpc(&mut session, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(800_000_156)], &kw, None)
        .await
        .expect("Reprocess repackaged Rifter (587) at Scrapmetal Processing V");
    assert!((get_dict_val(&ship_reproc_res, "yieldRate").unwrap().as_f64().unwrap() - 0.55).abs() < 1e-6);
    let PyRep::List(ship_outputs) = get_dict_val(&ship_reproc_res, "outputs").unwrap() else { panic!() };
    for mat in ctx.sde.type_materials.get(&587).expect("Rifter SDE typeMaterials") {
        let expected_qty = ((mat.quantity as f64) * 0.55).floor() as i64;
        if expected_qty > 0 {
            let actual_qty = ship_outputs
                .iter()
                .find(|o| get_dict_val(o, "typeID").and_then(|v| v.as_i64()) == Some(mat.material_type_id as i64))
                .and_then(|o| get_dict_val(o, "quantity"))
                .and_then(|v| v.as_i64());
            assert_eq!(actual_qty, Some(expected_qty));
        }
    }

    // 3. Upwell Structure Reprocessing Service (`feat:structures.service-reprocessing`, `feat:mining.upwell-refinery-base-yield`):
    let structure_id = 1_000_000_001_002i64;
    let tatara_id = 1_000_000_001_012i64;
    let citadel_id = 1_000_000_001_013i64;
    ctx.db.with_mut_snapshot(|snap| {
        for (idx, (sid, st_type, name)) in [
            (structure_id, 35835u32, "Athanor Refinery"),
            (tatara_id, 35836u32, "Tatara Refinery"),
            (citadel_id, 35825u32, "Astrahus Citadel"),
        ]
        .into_iter()
        .enumerate()
        {
            snap.structures.insert(sid, make_structure(sid, 1000001, st_type, 30000142, name, [0.0, 0.0, 0.0]));
            let svc_item_id = 800_000_160 + (idx as i64) * 2;
            let ore_item_id = svc_item_id + 1;
            snap.items.insert(svc_item_id, make_item(svc_item_id, 35892, 1000001, sid, InventoryFlag::ServiceSlot0, 1, true));
            snap.items.insert(ore_item_id, make_item(ore_item_id, 1230, char_id, sid, InventoryFlag::Hangar, 1_000, false));
        }
        snap.items.insert(800_000_167, make_item(800_000_167, 1230, char_id, structure_id, InventoryFlag::Hangar, 500, false));
        snap.items.insert(800_000_168, make_item(800_000_168, 16262, char_id, structure_id, InventoryFlag::Hangar, 5, false));
    });

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = Some(structure_id);
        }
        if let Some(st) = snap.structures.get_mut(&structure_id) {
            st.fuel_blocks = 0;
        }
    });
    session.station_id = None;
    session.structure_id = Some(structure_id);
    let unfueled_reproc_err = ctx
        .dispatch_rpc(&mut session, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(800_000_161)], &kw, None)
        .await
        .expect_err("Unfueled refinery must reject Reprocess");
    assert_eq!(unfueled_reproc_err.code, "ReprocessingServiceOffline");
    let unfueled_comp_err = ctx
        .dispatch_rpc(&mut session, "structureCompressionMgr", None, "CompressItemInStructure", &[PyRep::Integer(800_000_167)], &kw, None)
        .await
        .expect_err("Unfueled refinery must reject CompressItemInStructure");
    assert_eq!(unfueled_comp_err.code, "ReprocessingServiceOffline");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&structure_id) {
            st.fuel_blocks = 500;
        }
    });
    let comp_ore_res = ctx
        .dispatch_rpc(&mut session, "structureCompressionMgr", None, "CompressItemInStructure", &[PyRep::Integer(800_000_167)], &kw, None)
        .await
        .expect("CompressItemInStructure ore");
    assert_eq!(get_dict_val(&comp_ore_res, "compressedTypeID").unwrap().as_i64(), Some(62516));
    assert_eq!(get_dict_val(&comp_ore_res, "compressedUnits").unwrap().as_i64(), Some(500));
    let comp_ice_res = ctx
        .dispatch_rpc(&mut session, "structureCompressionMgr", None, "CompressItemInStructure", &[PyRep::Integer(800_000_168)], &kw, None)
        .await
        .expect("CompressItemInStructure ice");
    assert_eq!(get_dict_val(&comp_ice_res, "compressedTypeID").unwrap().as_i64(), Some(28434));
    assert_eq!(get_dict_val(&comp_ice_res, "compressedUnits").unwrap().as_i64(), Some(5));

    for (idx, (sid, expected_base, label)) in [
        (structure_id, 0.51_f64, "Athanor (35835)"),
        (tatara_id, 0.5275_f64, "Tatara (35836)"),
        (citadel_id, 0.50_f64, "Astrahus Citadel (35825)"),
    ]
    .into_iter()
    .enumerate()
    {
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.station_id = None;
                ch.structure_id = Some(sid);
            }
        });
        session.station_id = None;
        session.structure_id = Some(sid);

        let struct_info = ctx.dispatch_rpc(&mut session, "reprocessingSvc", None, "GetQuotes", &[], &kw, None).await.expect("GetQuotes");
        let struct_base = get_dict_val(&struct_info, "stationBaseYield").unwrap().as_f64().unwrap();
        assert!((struct_base - expected_base).abs() < 1e-6, "{label} base yield mismatch");

        let ore_item_id = 800_000_161 + (idx as i64) * 2;
        let struct_reproc_res = ctx
            .dispatch_rpc(&mut session, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(ore_item_id)], &kw, None)
            .await
            .expect("Reprocess in Upwell structure");
        let struct_yield = get_dict_val(&struct_reproc_res, "yieldRate").unwrap().as_f64().unwrap();
        let struct_tax = get_dict_val(&struct_reproc_res, "stationTaxRate").unwrap().as_f64().unwrap();
        let struct_trit = get_dict_val(&struct_reproc_res, "tritaniumQuantity").unwrap().as_i64().unwrap();
        let expected_struct_yield: f64 = expected_base * 1.15 * 1.10 * 1.10;
        assert!((struct_yield - expected_struct_yield).abs() < 1e-6);
        assert_eq!(struct_tax, 0.0);
        assert_eq!(struct_trit, (10.0_f64 * 400.0 * expected_struct_yield).floor() as i64);
    }

    // 4. Standup Reprocessing Rig Security Multipliers (`feat:structures.reprocessing-rig-security-multipliers`):
    let athanor_rig_id = 800_000_170i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(athanor_rig_id, make_item(athanor_rig_id, 46633, 1000001, structure_id, InventoryFlag::RigSlot0, 1, true));
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.structure_id = Some(structure_id);
        }
    });
    session.structure_id = Some(structure_id);

    for (idx, (sys_id, sec_mult)) in [(30000142u32, 1.0_f64), (30002537u32, 1.06_f64), (30000208u32, 1.12_f64)].into_iter().enumerate() {
        let ore_id = 800_000_171 + idx as i64;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(st) = snap.structures.get_mut(&structure_id) {
                st.solar_system_id = sys_id;
            }
            snap.items.insert(ore_id, make_item(ore_id, 1230, char_id, structure_id, InventoryFlag::Hangar, 1_000, false));
        });
        let expected_rigged_base = 0.51 * sec_mult * 1.02;
        let q = ctx.dispatch_rpc(&mut session, "reprocessingSvc", None, "GetQuotes", &[], &kw, None).await.unwrap();
        assert!((get_dict_val(&q, "stationBaseYield").unwrap().as_f64().unwrap() - expected_rigged_base).abs() < 1e-6);

        let r = ctx.dispatch_rpc(&mut session, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(ore_id)], &kw, None).await.unwrap();
        assert!((get_dict_val(&r, "stationBaseYield").unwrap().as_f64().unwrap() - expected_rigged_base).abs() < 1e-6);
    }

    let athanor_ice_id = 800_000_175i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(athanor_ice_id, make_item(athanor_ice_id, 16262, char_id, structure_id, InventoryFlag::Hangar, 10, false));
    });
    let ice_on_ore_rig = ctx.dispatch_rpc(&mut session, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(athanor_ice_id)], &kw, None).await.unwrap();
    assert!((get_dict_val(&ice_on_ore_rig, "stationBaseYield").unwrap().as_f64().unwrap() - 0.51).abs() < 1e-6);

    let tatara_rig_id = 800_000_180i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&tatara_id) {
            st.solar_system_id = 30000208;
        }
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.structure_id = Some(tatara_id);
        }
        snap.items.insert(tatara_rig_id, make_item(tatara_rig_id, 46640, 1000001, tatara_id, InventoryFlag::RigSlot0, 1, true));
    });
    session.structure_id = Some(tatara_id);
    let expected_tatara_t2_null_base = 0.53 * 1.12 * 1.055; // 0.626248
    for (idx, (type_id, qty)) in [(1230u32, 1_000), (16262u32, 10), (45490u32, 1_000)].into_iter().enumerate() {
        let item_id = 800_000_181 + idx as i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(item_id, make_item(item_id, type_id, char_id, tatara_id, InventoryFlag::Hangar, qty, false));
        });
        let res = ctx.dispatch_rpc(&mut session, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(item_id)], &kw, None).await.unwrap();
        assert!((get_dict_val(&res, "stationBaseYield").unwrap().as_f64().unwrap() - expected_tatara_t2_null_base).abs() < 1e-6);
    }

    // 5. Structure Owner Reprocessing Tax (`feat:mining.reprocessing-tax`):
    let owner_corp_id = 98_000_777i64;
    let taxed_ore_id = 800_000_189i64;
    let journal_before = ctx.db.with_snapshot(|snap| snap.wallet_journal.len());
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            owner_corp_id,
            eve_db::CorporationRecord {
                corporation_id: owner_corp_id, name: "Refinery Owner Corp".into(), ticker: "RFOWN".into(),
                ceo_id: char_id, alliance_id: None, tax_rate: 0.15, master_wallet: eve_db::IskCents(0),
                wallet_divisions: Vec::new(), description: String::new(), url: String::new(),
                hq_station_id: Some(station_id), members: Default::default(), member_roles: Default::default(), applications: Default::default(),
            },
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = owner_corp_id;
        }
        if let Some(st) = snap.structures.get_mut(&tatara_id) {
            st.owner_corp_id = owner_corp_id;
        }
        snap.items.insert(taxed_ore_id, make_item(taxed_ore_id, 1230, char_id, tatara_id, InventoryFlag::Hangar, 1_000, false));
    });
    session.corporation_id = Some(owner_corp_id);
    ctx.dispatch_rpc(&mut session, "structureProfiles", None, "SaveProfileSettings", &[PyRep::Integer(tatara_id), PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Real(0.03)])])], &kw, None).await.expect("SaveProfileSettings 3% reprocessing tax");
    let st_tax_res = ctx.dispatch_rpc(&mut session, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(taxed_ore_id)], &kw, None).await.expect("Reprocess in structure with 3% owner tax");
    assert!((get_dict_val(&st_tax_res, "stationTaxRate").unwrap().as_f64().unwrap() - 0.03).abs() < 1e-6);
    assert!(get_dict_val(&st_tax_res, "reprocessingTaxISK").unwrap().as_f64().unwrap() > 0.0);
    let (journal_added, corp_wallet) = ctx.db.with_snapshot(|snap| {
        let added = snap.wallet_journal[journal_before..]
            .iter()
            .any(|e| e.character_id == char_id && e.entry_type == "ReprocessingTax" && e.amount.0 > 0);
        let cw = snap.corporations.get(&owner_corp_id).map(|c| c.master_wallet.0).unwrap_or(0);
        (added, cw)
    });
    assert!(journal_added && corp_wallet > 0);
}
