//! Integration tests for Prismaticite & Unrefined Mineral reactions,
//! Scrapmetal yield exclusions, and Wormhole ore site seeding (`MN-08`).

#![forbid(unsafe_code)]

use eve_db::{
    CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord,
    UpwellStructureRecord,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::{HashMap, HashSet};

async fn setup_session(ctx: &ServerContext, char_id: i64) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(902);
    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())],
        &kw,
        None,
    )
    .await
    .expect("Login");
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");
    sess
}

fn dict_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(entries) => entries.iter().find(|(k, _)| k.as_str() == key).map(|(_, v)| v),
        PyRep::Dict(entries) => entries.iter().find(|(k, _)| k.as_str() == Some(key)).map(|(_, v)| v),
        _ => None,
    }
}

fn make_structure(sid: i64, stype: u32, corp_id: i64, ssys: u32, name: &str) -> UpwellStructureRecord {
    UpwellStructureRecord {
        structure_id: sid,
        type_id: stype,
        owner_corp_id: corp_id,
        solar_system_id: ssys,
        name: name.into(),
        position: [100_000.0, 0.0, 0.0],
        state: "ShieldVulnerable".into(),
        quantum_core_installed: true,
        fuel_blocks: 1_000,
        shield_hp: 1.0,
        armor_hp: 1.0,
        hull_hp: 1.0,
        reinforced_until_unix: 0,
        last_damage_tick: 0,
        last_damage_unix: 0,
        self_destruct_at_unix: None,
    }
}

fn make_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) -> InventoryItemRecord {
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
    }
}

#[tokio::test]
async fn test_mining_scrapmetal_yield_exclusions_at_npc_and_rigged_tatara() {
    // parity: feat:mining.scrapmetal-yield-exclusions
    eve_testkit::attest(&["feat:mining.scrapmetal-yield-exclusions"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let char_id = 90_000_001_i64;
    let mut sess = setup_session(&ctx, char_id).await;

    // 1. At Jita 50% NPC station: Scrapmetal Processing 0 -> 50.0%, level 5 -> 55.0%
    let jita_station = 60_003_760_i64;
    sess.station_id = Some(jita_station);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(1_000_035, 10.0);
        }
        snap.skills.entry(char_id).or_default().insert(
            12196,
            TrainedSkillRecord { type_id: 12196, level: 0, skill_points: 0.0 },
        );
        snap.items.insert(101_000_001, make_item(101_000_001, 482, char_id, jita_station, InventoryFlag::Hangar, 1, false));
    });
    let mod_lvl0 = ctx
        .dispatch_rpc(&mut sess, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(101_000_001)], &kw, None)
        .await
        .expect("Reprocess Miner II at Scrapmetal 0");
    assert!((dict_get(&mod_lvl0, "stationBaseYield").and_then(|v| v.as_f64()).unwrap() - 0.50).abs() < 1e-6);
    assert!((dict_get(&mod_lvl0, "yieldRate").and_then(|v| v.as_f64()).unwrap() - 0.50).abs() < 1e-6);

    // Train Scrapmetal Processing V (12196) -> 50% * (1 + 5 * 2%) = 55%
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            12196,
            TrainedSkillRecord { type_id: 12196, level: 5, skill_points: 256_000.0 },
        );
        snap.items.insert(101_000_002, make_item(101_000_002, 482, char_id, jita_station, InventoryFlag::Hangar, 1, false));
    });
    let mod_lvl5 = ctx
        .dispatch_rpc(&mut sess, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(101_000_002)], &kw, None)
        .await
        .expect("Reprocess Miner II at Scrapmetal 5");
    assert!((dict_get(&mod_lvl5, "stationBaseYield").and_then(|v| v.as_f64()).unwrap() - 0.50).abs() < 1e-6);
    assert!((dict_get(&mod_lvl5, "yieldRate").and_then(|v| v.as_f64()).unwrap() - 0.55).abs() < 1e-6);

    // 2. Negative exclusion test: docked at a W-space Tatara (35836) with T2 Reprocessing Rig (46493),
    // Reprocessing V (3385), Reprocessing Efficiency V (3389), Veldspar Processing V (12195), and
    // Zainou 'Beancounter' Reprocessing RX-804 (27170). Ore gets full bonuses (> 85%), while a
    // module (482 Miner II) and ship (587 Rifter) STILL exclude structure, rig, security, skill, and implant bonuses (55.0%).
    let tatara_id = 1_045_000_000_701_i64;
    let corp_id = 98_000_701_i64;
    let wh_sys = 31_000_007_u32;
    let rig_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 1944 && t.default_attributes.get(&1547).copied().unwrap_or(0.0) == 3.0)
        .map(|t| t.type_id)
        .unwrap_or(46493);

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Scrapmetal Test Corp".into(),
                ticker: "SCRP".into(),
                ceo_id: char_id,
                ..Default::default()
            },
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.solar_system_id = wh_sys;
            ch.station_id = None;
            ch.structure_id = Some(tatara_id);
        }
        snap.structures.insert(
            tatara_id,
            make_structure(tatara_id, 35836, corp_id, wh_sys, "W-Space Rigged Tatara"),
        );
        let svc = make_item(101_000_710, 35899, corp_id, tatara_id, InventoryFlag::ServiceSlot0, 1, true);
        snap.items.insert(svc.item_id, svc);
        let rig = make_item(101_000_711, rig_tid, corp_id, tatara_id, InventoryFlag::RigSlot0, 1, true);
        snap.items.insert(rig.item_id, rig);

        let sk = snap.skills.entry(char_id).or_default();
        for sid in [3385_u32, 3389, 12195, 12196] {
            sk.insert(sid, TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 });
        }
        snap.active_implants.insert(char_id, vec![27170]); // Zainou 'Beancounter' Reprocessing RX-804

        snap.items.insert(101_000_720, make_item(101_000_720, 1230, char_id, tatara_id, InventoryFlag::Hangar, 100, false));
        snap.items.insert(101_000_721, make_item(101_000_721, 482, char_id, tatara_id, InventoryFlag::Hangar, 1, false));
        snap.items.insert(101_000_722, make_item(101_000_722, 587, char_id, tatara_id, InventoryFlag::Hangar, 1, false));
    });
    sess.station_id = None;
    sess.structure_id = Some(tatara_id);
    sess.solar_system_id = Some(wh_sys);

    let ore_rep = ctx
        .dispatch_rpc(&mut sess, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(101_000_720)], &kw, None)
        .await
        .expect("Reprocess Veldspar at Tatara");
    let ore_base = dict_get(&ore_rep, "stationBaseYield").and_then(|v| v.as_f64()).unwrap();
    let ore_yield = dict_get(&ore_rep, "yieldRate").and_then(|v| v.as_f64()).unwrap();
    assert!(ore_base > 0.55, "Ore at rigged W-space Tatara must receive hull+rig+sec bonus (got {ore_base})");
    assert!(ore_yield > 0.75, "Ore yield at max skills + RX-804 + Tatara must exceed 75% (got {ore_yield})");

    for (iid, label) in [(101_000_721_i64, "Miner II"), (101_000_722_i64, "Rifter")] {
        let rep = ctx
            .dispatch_rpc(&mut sess, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(iid)], &kw, None)
            .await
            .unwrap_or_else(|e| panic!("Reprocess {label} failed: {e:?}"));
        let base = dict_get(&rep, "stationBaseYield").and_then(|v| v.as_f64()).unwrap();
        let yld = dict_get(&rep, "yieldRate").and_then(|v| v.as_f64()).unwrap();
        assert!((base - 0.50).abs() < 1e-6, "{label} must exclude Tatara hull/rig/sec bonus (got {base})");
        assert!((yld - 0.55).abs() < 1e-6, "{label} must exclude skills & implants and cap at 55% (got {yld})");
    }
}

#[tokio::test]
async fn test_mining_prismaticite_reprocessing_and_unrefined_mineral_reactions() {
    // parity: feat:mining.prismaticite-reprocessing-and-unrefined-reactions
    eve_testkit::attest(&["feat:mining.prismaticite-reprocessing-and-unrefined-reactions"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let char_id = 90_000_001_i64;
    let mut sess = setup_session(&ctx, char_id).await;

    let jita_station = 60_003_760_i64;
    sess.station_id = Some(jita_station);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(1_000_035, 10.0);
            ch.balance = IskCents(100_000_000_00);
        }
    });

    // 1. Verify SDE randomized_type_materials for Prismaticite (90041) & Compressed Prismaticite (90307)
    let prism_ranges: HashMap<u32, (u64, u64)> = ctx
        .sde
        .randomized_type_materials_for(90041)
        .iter()
        .map(|e| (e.material_type_id, (e.quantity_min, e.quantity_max)))
        .collect();
    assert_eq!(prism_ranges.len(), 8, "Prismaticite must have 8 SDE mineral entries");

    // Reprocess 12 batches of Prismaticite & Compressed Prismaticite; verify each batch yields
    // 1 of the 8 SDE minerals within [floor(min * yield_rate), floor(max * yield_rate)] and varies across batches.
    let mut seen_minerals = HashSet::new();
    let mut seen_quantities = HashSet::new();
    for idx in 0..12_i64 {
        let tid = if idx % 2 == 0 { 90041 } else { 90307 };
        let iid = 102_000_100 + idx;
        ctx.db.with_mut_snapshot(|snap| {
            snap.next_entity_id += (idx * 7) + 3;
            snap.items.insert(iid, make_item(iid, tid, char_id, jita_station, InventoryFlag::Hangar, 100, false));
        });
        let rep = ctx
            .dispatch_rpc(&mut sess, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(iid)], &kw, None)
            .await
            .expect("Reprocess Prismaticite batch");
        let yld = dict_get(&rep, "yieldRate").and_then(|v| v.as_f64()).unwrap();
        let PyRep::List(outs) = dict_get(&rep, "outputs").expect("outputs") else { panic!("Expected outputs list") };
        assert_eq!(outs.len(), 1, "Prismaticite batch must yield exactly 1 mineral type");
        let out_tid = dict_get(&outs[0], "typeID").and_then(|v| v.as_i64()).unwrap() as u32;
        let out_qty = dict_get(&outs[0], "quantity").and_then(|v| v.as_i64()).unwrap() as u64;
        let &(q_min, q_max) = prism_ranges.get(&out_tid).expect("Output must be one of the 8 SDE minerals");
        let exp_lo = ((q_min as f64) * yld).floor() as u64;
        let exp_hi = ((q_max as f64) * yld).floor() as u64;
        assert!(
            (exp_lo..=exp_hi).contains(&out_qty),
            "Prismaticite mineral {out_tid} qty {out_qty} outside SDE bounds [{exp_lo}, {exp_hi}]"
        );
        seen_minerals.insert(out_tid);
        seen_quantities.insert(out_qty);
    }
    assert!(seen_minerals.len() >= 2, "Multiple Prismaticite batches must select varying minerals");
    assert!(seen_quantities.len() >= 2, "Multiple Prismaticite batches must roll varying SDE quantities");

    // Multi-batch Prismaticite reprocessing (1,000 units = 10 batches) must roll each 100-unit
    // batch independently and distribute outputs across multiple distinct mineral types.
    let multi_iid = 102_000_200_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 41;
        snap.items.insert(multi_iid, make_item(multi_iid, 90041, char_id, jita_station, InventoryFlag::Hangar, 1_000, false));
    });
    let multi_rep = ctx
        .dispatch_rpc(&mut sess, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(multi_iid)], &kw, None)
        .await
        .expect("Reprocess 10-batch Prismaticite stack");
    let multi_yld = dict_get(&multi_rep, "yieldRate").and_then(|v| v.as_f64()).unwrap();
    let PyRep::List(multi_outs) = dict_get(&multi_rep, "outputs").expect("outputs") else { panic!() };
    assert!(
        multi_outs.len() >= 2,
        "10-batch Prismaticite stack must yield multiple distinct minerals across independent batch rolls, got {}",
        multi_outs.len()
    );
    for out in multi_outs {
        let out_tid = dict_get(out, "typeID").and_then(|v| v.as_i64()).unwrap() as u32;
        let out_qty = dict_get(out, "quantity").and_then(|v| v.as_i64()).unwrap() as u64;
        let &(q_min, q_max) = prism_ranges.get(&out_tid).expect("Output must be one of the 8 SDE minerals");
        let exp_lo = ((q_min as f64) * multi_yld).floor() as u64;
        let exp_hi = (((q_max * 10) as f64) * multi_yld).floor() as u64;
        assert!(
            (exp_lo..=exp_hi).contains(&out_qty),
            "Multi-batch Prismaticite mineral {out_tid} qty {out_qty} outside bounds [{exp_lo}, {exp_hi}]"
        );
    }

    // 2. Unrefined Mineral Reaction (90274 Unrefined Tritanium Formula -> 90283 Unrefined Tritanium)
    // Requires Lowsec/Nullsec/WH Tatara/Athanor with an online Standup Composite Reactor I (45537).
    let tatara_id = 1_045_000_000_801_i64;
    let corp_id = 98_000_801_i64;
    let ls_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.solar_system_id < 31_000_000 && s.security_status > 0.0 && s.security_status < 0.45)
        .map(|s| s.solar_system_id)
        .unwrap_or(30_002_537);
    let reactor_iid = 102_000_810_i64;
    let formula_iid = 102_000_811_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Reaction Test Corp".into(),
                ticker: "REAC".into(),
                ceo_id: char_id,
                ..Default::default()
            },
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.solar_system_id = ls_sys;
            ch.station_id = None;
            ch.structure_id = Some(tatara_id);
        }
        snap.structures.insert(
            tatara_id,
            make_structure(tatara_id, 35835, corp_id, ls_sys, "Lowsec Reaction Athanor"),
        );
        // Fit Biochemical Reactor (45539) first -> must fail with CompositeReactorRequired!
        snap.items.insert(reactor_iid, make_item(reactor_iid, 45539, corp_id, tatara_id, InventoryFlag::ServiceSlot0, 1, true));
        snap.items.insert(formula_iid, make_item(formula_iid, 90274, char_id, tatara_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(102_000_812, make_item(102_000_812, 90307, char_id, tatara_id, InventoryFlag::Hangar, 200, false));
        snap.items.insert(102_000_813, make_item(102_000_813, 16634, char_id, tatara_id, InventoryFlag::Hangar, 200, false));
        let sk = snap.skills.entry(char_id).or_default();
        sk.insert(45746, TrainedSkillRecord { type_id: 45746, level: 5, skill_points: 256_000.0 }); // Reactions V
        sk.insert(90398, TrainedSkillRecord { type_id: 90398, level: 5, skill_points: 256_000.0 }); // Unrefined Minerals Processing V
    });
    sess.station_id = None;
    sess.structure_id = Some(tatara_id);
    sess.solar_system_id = Some(ls_sys);

    let install_args = [
        PyRep::Integer(formula_iid),
        PyRep::Integer(90274),
        PyRep::Integer(9), // Reactions
        PyRep::Integer(1), // 1 run
        PyRep::Integer(tatara_id),
    ];
    let bad_err = ctx
        .dispatch_rpc(&mut sess, "industryManager", None, "InstallJob", &install_args, &kw, None)
        .await
        .expect_err("Unrefined Mineral reaction must require Standup Composite Reactor I (45537)");
    assert_eq!(bad_err.code, "CompositeReactorRequired");

    // Swap service module to Standup Composite Reactor I (45537) -> InstallJob succeeds
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(r) = snap.items.get_mut(&reactor_iid) {
            r.type_id = 45537;
        }
    });
    let job_res = ctx
        .dispatch_rpc(&mut sess, "industryManager", None, "InstallJob", &install_args, &kw, None)
        .await
        .expect("InstallJob with Standup Composite Reactor I");
    let job_id = dict_get(&job_res, "jobID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(dict_get(&job_res, "productTypeID").and_then(|v| v.as_i64()), Some(90283));

    // Advance clock past reaction duration and complete job -> delivers 100 Unrefined Tritanium (90283)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(j) = snap.industry_jobs.get_mut(&job_id) {
            j.start_unix = ctx.now_unix().saturating_sub(j.duration_seconds + 10);
        }
    });
    ctx.dispatch_rpc(&mut sess, "industryManager", None, "CompleteJob", &[PyRep::Integer(job_id)], &kw, None)
        .await
        .expect("CompleteJob for Unrefined Tritanium");

    let unref_item_id = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .find(|i| i.owner_id == char_id && i.location_id == tatara_id && i.type_id == 90283 && i.quantity == 100)
            .map(|i| i.item_id)
            .expect("100 Unrefined Tritanium (90283) must be delivered to hangar")
    });

    // Fit Reprocessing Facility (35899) + M-Set Asteroid Ore Grading Processor I (46633, group 1941)
    // on Tatara: Asteroid Ore rig must boost Prismaticite (group 4915) but NOT Unrefined Mineral (group 4932).
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(102_000_820, make_item(102_000_820, 35899, corp_id, tatara_id, InventoryFlag::ServiceSlot1, 1, true));
        snap.items.insert(102_000_821, make_item(102_000_821, 46633, corp_id, tatara_id, InventoryFlag::RigSlot0, 1, true));
        snap.items.insert(102_000_822, make_item(102_000_822, 90041, char_id, tatara_id, InventoryFlag::Hangar, 100, false));
    });
    let quotes = ctx
        .dispatch_rpc(
            &mut sess,
            "reprocessingSvc",
            None,
            "GetQuotes",
            &[PyRep::List(vec![PyRep::Integer(unref_item_id), PyRep::Integer(102_000_822)])],
            &kw,
            None,
        )
        .await
        .expect("GetQuotes on rigged Tatara");
    let PyRep::List(quote_list) = quotes else { panic!("Expected quote list") };
    let unref_quote = quote_list
        .iter()
        .find(|q| dict_get(q, "itemID").and_then(|v| v.as_i64()) == Some(unref_item_id))
        .expect("Unrefined Tritanium quote");
    let prism_quote = quote_list
        .iter()
        .find(|q| dict_get(q, "itemID").and_then(|v| v.as_i64()) == Some(102_000_822))
        .expect("Prismaticite quote");
    let unref_base = dict_get(unref_quote, "stationBaseYield").and_then(|v| v.as_f64()).unwrap();
    let prism_base = dict_get(prism_quote, "stationBaseYield").and_then(|v| v.as_f64()).unwrap();
    assert!(
        (unref_base - 0.51).abs() < 1e-6,
        "Unrefined Mineral (group 4932) must NOT receive M-Set Asteroid Ore Grading Processor rig bonus on Athanor (got {unref_base})"
    );
    assert!(
        prism_base > 0.55,
        "Prismaticite (group 4915) must receive M-Set Asteroid Ore Grading Processor rig bonus on Athanor (got {prism_base})"
    );
    let unref_rep = ctx
        .dispatch_rpc(&mut sess, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(unref_item_id)], &kw, None)
        .await
        .expect("Reprocess Unrefined Tritanium");
    let unref_yld = dict_get(&unref_rep, "yieldRate").and_then(|v| v.as_f64()).unwrap();
    let PyRep::List(unref_outs) = dict_get(&unref_rep, "outputs").expect("outputs") else { panic!() };
    assert_eq!(unref_outs.len(), 1);
    assert_eq!(dict_get(&unref_outs[0], "typeID").and_then(|v| v.as_i64()), Some(34));
    let trit_qty = dict_get(&unref_outs[0], "quantity").and_then(|v| v.as_i64()).unwrap() as u64;
    let trit_lo = (368_000.0 * unref_yld).floor() as u64;
    let trit_hi = (496_800.0 * unref_yld).floor() as u64;
    assert!(
        (trit_lo..=trit_hi).contains(&trit_qty),
        "Unrefined Tritanium output {trit_qty} outside SDE bounds [{trit_lo}, {trit_hi}]"
    );
}

#[tokio::test]
async fn test_mining_wormhole_ore_sites_and_sleeper_guards() {
    let ctx = ServerContext::new().with_seeded_rng(0x5EED_1234_5678_9ABC);
    crate::common::pin_clock(&ctx);

    // 1. Verify full 11-site W-space deposit catalog across C1..=C6 and Shattered C12..=C18
    assert_eq!(eve_server::services::mining_sites::WORMHOLE_ORE_SITES.len(), 11);
    for def in eve_server::services::mining_sites::WORMHOLE_ORE_SITES {
        let (did, rocks) = eve_server::services::mining_sites::resolve_wormhole_ore_site_with_dungeon(&ctx.sde, def)
            .unwrap_or_else(|e| panic!("Failed to resolve {}: {e}", def.dungeon_name));
        assert!(did > 0 && !rocks.is_empty(), "{} must resolve SDE asteroids", def.dungeon_name);
    }
    for cls in 1..=4_u8 {
        let names: Vec<&str> = eve_server::services::mining_sites::wormhole_ore_sites_for_class(cls)
            .into_iter()
            .map(|d| d.dungeon_name)
            .collect();
        assert_eq!(names.len(), 4, "C{cls} must have 4 Perimeter/Frontier sites: {names:?}");
        assert!(!names.iter().any(|n| n.contains("Core") || n.contains("Shattered")));
    }
    for cls in 5..=6_u8 {
        let names: Vec<&str> = eve_server::services::mining_sites::wormhole_ore_sites_for_class(cls)
            .into_iter()
            .map(|d| d.dungeon_name)
            .collect();
        assert_eq!(names.len(), 10, "C{cls} must have 10 Perimeter/Frontier/Core sites: {names:?}");
        assert!(names.contains(&"Isolated Core Deposit") && names.contains(&"Rarified Core Deposit"));
        assert!(names.contains(&"Uncommon Core Deposit") && names.contains(&"Unusual Core Deposit"));
        assert!(names.contains(&"Infrequent Core Deposit") && names.contains(&"Exceptional Core Deposit"));
        assert!(!names.contains(&"Shattered Debris Field"));
    }
    for cls in [12_u8, 13_u8] {
        let names: Vec<&str> = eve_server::services::mining_sites::wormhole_ore_sites_for_class(cls)
            .into_iter()
            .map(|d| d.dungeon_name)
            .collect();
        assert_eq!(names.len(), 11, "C{cls} Shattered must include all 11 sites: {names:?}");
        assert!(names.contains(&"Shattered Debris Field"));
    }

    // 2. Seed representative W-space systems across C1, C3, C5, C6, and C13 Shattered;
    // verify bounded anomaly pool sampling (4 distinct anomalies per system), Sleeper guard waves,
    // SDE maxVelocity/inertia, and idempotency.
    let mut seen_core_deposits = HashSet::new();
    let sample_systems: &[(u32, u8)] = &[
        (31_000_007, 1),  // C1
        (31_000_880, 3),  // C3
        (31_001_880, 5),  // C5
        (31_001_881, 5),  // C5
        (31_001_882, 5),  // C5
        (31_002_367, 6),  // C6
        (31_002_368, 6),  // C6
        (31_002_580, 13), // C13 Shattered
    ];
    for &(sys_id, expected_class) in sample_systems {
        let sys = ctx.sde.solar_systems.get(&sys_id).expect("Wormhole system in SDE");
        assert_eq!(sys.wormhole_class_id, Some(i32::from(expected_class)));

        eve_server::router::dispatch::seed_system_mining_sites_from_sde(&ctx, sys_id).await;
        eve_server::router::dispatch::seed_system_mining_sites_from_sde(&ctx, sys_id).await;

        let (anom_names, sig_names, ast_types, gas_count) = ctx.db.with_snapshot(|snap| {
            let anoms = snap.cosmic_anomalies.get(&sys_id).cloned().unwrap_or_default();
            let an_names: Vec<String> = anoms
                .iter()
                .filter(|a| a.anomaly_type == "OreAnomaly")
                .map(|a| a.name.clone())
                .collect();
            let s_names: Vec<String> = snap
                .cosmic_signatures
                .get(&sys_id)
                .map(|sigs| {
                    sigs.iter()
                        .filter(|s| s.sig_group == "OreAnomaly")
                        .map(|s| s.type_name.clone())
                        .collect()
                })
                .unwrap_or_default();
            let sys_asts: Vec<_> = snap
                .asteroids
                .values()
                .filter(|a| a.solar_system_id == sys_id)
                .filter(|a| ctx.sde.types.get(&a.type_id).is_some_and(|t| t.group_id != 711))
                .collect();
            let g_count = snap
                .asteroids
                .values()
                .filter(|a| a.solar_system_id == sys_id)
                .filter(|a| ctx.sde.types.get(&a.type_id).is_some_and(|t| t.group_id == 711))
                .count();
            let a_tids: HashSet<u32> = sys_asts.iter().map(|a| a.type_id).collect();
            (an_names, s_names, a_tids, g_count)
        });

        let unique_anoms: HashSet<&String> = anom_names.iter().collect();
        assert_eq!(anom_names.len(), 4, "C{expected_class} {sys_id} must sample 4 anomalies: {anom_names:?}");
        assert_eq!(unique_anoms.len(), 4, "C{expected_class} {sys_id} sampled anomalies must be distinct: {anom_names:?}");
        assert_eq!(gas_count, 2, "C{expected_class} {sys_id} Barren Perimeter Reservoir gas cloud count mismatch");
        if expected_class <= 4 {
            assert!(!anom_names.iter().any(|n| n.contains("Core") || n.contains("Shattered")));
        } else if expected_class <= 6 {
            assert!(!anom_names.iter().any(|n| n == "Shattered Debris Field"));
            for n in &anom_names {
                if n.contains("Core Deposit") {
                    seen_core_deposits.insert(n.clone());
                }
            }
        } else if expected_class == 13 {
            assert!(anom_names.iter().any(|n| n == "Shattered Debris Field"), "C13 {sys_id} must include Shattered Debris Field");
        }

        let mut expected_guards: HashMap<u32, usize> = HashMap::new();
        for site_name in &anom_names {
            assert!(sig_names.iter().any(|n| n == site_name), "Missing OreAnomaly signature {site_name}");
            let def = eve_server::services::mining_sites::WORMHOLE_ORE_SITES
                .iter()
                .find(|d| d.dungeon_name == site_name)
                .expect("Spawned anomaly must be in WORMHOLE_ORE_SITES");
            for &(gid, cnt) in def.guards {
                *expected_guards.entry(gid).or_insert(0) += cnt as usize;
            }
        }

        assert!(ast_types.contains(&46686) && ast_types.contains(&46683));
        assert!(ast_types.contains(&46676) && ast_types.contains(&46678) && ast_types.contains(&46679));

        let balls = ctx.systems.get_or_spawn(sys_id).list_balls().await.expect("list_balls");
        for (&npc_tid, &expected_count) in &expected_guards {
            let matching: Vec<_> = balls.iter().filter(|b| b.type_id == npc_tid).collect();
            assert_eq!(
                matching.len(),
                expected_count,
                "Expected {expected_count} Sleeper guards of type {npc_tid} in C{expected_class} {sys_id} (got {})",
                matching.len()
            );
            let sde_max_vel = ctx.sde.types[&npc_tid]
                .default_attributes
                .get(&eve_sde::attr_ids::MAX_VELOCITY)
                .copied()
                .expect("Sleeper NPC maxVelocity in SDE");
            for b in matching {
                assert_eq!(b.max_velocity, sde_max_vel, "Sleeper NPC {npc_tid} must use SDE maxVelocity");
                assert_eq!(b.inertia, 1.0, "Sleeper NPC {npc_tid} must use canonical agility fallback 1.0");
            }
        }
    }
    assert!(
        seen_core_deposits.len() >= 3,
        "Seeded C5/C6 systems must sample multiple distinct Core deposits, got {seen_core_deposits:?}"
    );

    // 3. Verify server restart respawns exact Sleeper guards for persisted anomalies
    let restarted_ctx = ServerContext::with_sde_and_repository(
        std::sync::Arc::clone(&ctx.sde),
        std::sync::Arc::clone(&ctx.db),
    );
    crate::common::pin_clock(&restarted_ctx);
    eve_server::router::dispatch::seed_system_mining_sites_from_sde(&restarted_ctx, 31_001_880).await;
    let restarted_balls = restarted_ctx.systems.get_or_spawn(31_001_880).list_balls().await.expect("list_balls after restart");
    let guard_ball_count = restarted_balls
        .iter()
        .filter(|b| eve_server::services::mining_sites::WORMHOLE_GUARD_TYPE_IDS.contains(&b.type_id))
        .count();
    assert!(guard_ball_count > 0, "Restarted C5 system must respawn Sleeper guards for persisted anomalies");
}
