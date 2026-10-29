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
            make_structure(tatara_id, 35836, corp_id, ls_sys, "Lowsec Reaction Tatara"),
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

    // Fit Reprocessing Facility (35899) on Tatara and reprocess the 100 Unrefined Tritanium -> Tritanium (34)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(102_000_820, make_item(102_000_820, 35899, corp_id, tatara_id, InventoryFlag::ServiceSlot1, 1, true));
    });
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
    // parity: feat:mining.wormhole-ore-sites
    eve_testkit::attest(&["feat:mining.wormhole-ore-sites"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let cases: &[(u32, u8, &[&str], &[&str], &[u32])] = &[
        (
            31_000_007, // C1
            1,
            &["Ordinary Perimeter Deposit", "Common Perimeter Deposit"],
            &["Unexceptional Frontier Deposit", "Exceptional Core Deposit"],
            &[30209],
        ),
        (
            31_000_880, // C3
            3,
            &[
                "Ordinary Perimeter Deposit",
                "Common Perimeter Deposit",
                "Unexceptional Frontier Deposit",
                "Average Frontier Deposit",
            ],
            &["Exceptional Core Deposit", "Infrequent Core Deposit"],
            &[30209, 30210],
        ),
        (
            31_001_880, // C5
            5,
            &[
                "Unexceptional Frontier Deposit",
                "Average Frontier Deposit",
                "Exceptional Core Deposit",
                "Infrequent Core Deposit",
            ],
            &["Ordinary Perimeter Deposit", "Common Perimeter Deposit"],
            &[30210, 30200],
        ),
        (
            31_002_367, // C6
            6,
            &[
                "Exceptional Core Deposit",
                "Infrequent Core Deposit",
            ],
            &["Ordinary Perimeter Deposit", "Common Perimeter Deposit", "Unexceptional Frontier Deposit"],
            &[30200],
        ),
        (
            31_002_580, // C13 Shattered
            13,
            &[
                "Ordinary Perimeter Deposit",
                "Common Perimeter Deposit",
                "Unexceptional Frontier Deposit",
                "Average Frontier Deposit",
                "Exceptional Core Deposit",
                "Infrequent Core Deposit",
            ],
            &[],
            &[30209, 30210, 30200],
        ),
    ];

    for &(sys_id, expected_class, expected_sites, forbidden_sites, expected_guard_types) in cases {
        let sys = ctx.sde.solar_systems.get(&sys_id).expect("Wormhole system in SDE");
        assert_eq!(sys.wormhole_class_id, Some(i32::from(expected_class)));

        eve_server::router::dispatch::seed_system_mining_sites_from_sde(&ctx, sys_id).await;
        // Second call must be idempotent
        eve_server::router::dispatch::seed_system_mining_sites_from_sde(&ctx, sys_id).await;

        let (anom_names, sig_names, ast_types) = ctx.db.with_snapshot(|snap| {
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
            let a_tids: HashSet<u32> = snap
                .asteroids
                .values()
                .filter(|a| a.solar_system_id == sys_id)
                .map(|a| a.type_id)
                .collect();
            (an_names, s_names, a_tids)
        });

        assert_eq!(
            anom_names.len(),
            expected_sites.len(),
            "C{expected_class} system {sys_id} anomaly count mismatch: {anom_names:?}"
        );
        for &site_name in expected_sites {
            assert!(anom_names.iter().any(|n| n == site_name), "Missing {site_name} in C{expected_class} {sys_id}");
            assert!(sig_names.iter().any(|n| n == site_name), "Missing OreAnomaly signature {site_name}");
        }
        for &bad_name in forbidden_sites {
            assert!(!anom_names.iter().any(|n| n == bad_name), "Unexpected {bad_name} in C{expected_class} {sys_id}");
        }

        // Verify spawned asteroids span Highsec (Pyroxeres 46686), Lowsec (Kernite 46683 / Omber 46684),
        // and Nullsec (Bistot 46676 / Arkonor 46678 / Gneiss 46679) ore families.
        assert!(ast_types.contains(&46686), "Wormhole ore sites in {sys_id} must include Highsec-tier Pyroxeres");
        assert!(
            ast_types.contains(&46683) || ast_types.contains(&46684),
            "Wormhole ore sites in {sys_id} must include Lowsec-tier Kernite/Omber"
        );
        assert!(
            ast_types.contains(&46676) || ast_types.contains(&46678) || ast_types.contains(&46679),
            "Wormhole ore sites in {sys_id} must include Nullsec-tier Bistot/Arkonor/Gneiss"
        );

        // Verify Sleeper guard NPCs are spawned in the system ballpark
        let handle = ctx.systems.get_or_spawn(sys_id);
        let ball_types: Vec<u32> = handle
            .list_balls()
            .await
            .expect("list_balls")
            .into_iter()
            .map(|b| b.type_id)
            .collect();
        for &npc_tid in expected_guard_types {
            let count = ball_types.iter().filter(|&&t| t == npc_tid).count();
            assert!(count >= 3, "Expected Sleeper guard NPCs of type {npc_tid} in C{expected_class} {sys_id} (got {count})");
        }
    }
}
