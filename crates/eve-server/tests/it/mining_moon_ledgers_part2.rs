//! Phase 5M Moon Survey Probes, Moon Extraction, Fracture, Decay, Metenox Drill & Corp Ledger (`MN-13, MN-14, MN-16`).

#![forbid(unsafe_code)]

use super::{decode_crowset_rows, get_dict_val, make_item, make_structure, setup_test_session};
use eve_db::InventoryFlag;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

// ====================================================================
// Test 10a: MN-13: Moon Survey Probes, Launch Rules, Flight Time & Security-Band Moon Ores
// ====================================================================
#[tokio::test]
async fn test_mn13_moon_survey_probes_launch_alignment_and_resources() {
    // parity: feat:scanning.moon-survey-probe, feat:scanning.moon-survey-probe-launch-rules, feat:mining.moon-resources, feat:mining.moon-ore-types, feat:mining.moon-ore-class-by-security, rpc:moonExtractions.GetMoonResourcesForStructure
    eve_testkit::attest(&[
        "feat:scanning.moon-survey-probe",
        "feat:scanning.moon-survey-probe-launch-rules",
        "feat:mining.moon-resources",
        "feat:mining.moon-ore-types",
        "feat:mining.moon-ore-class-by-security",
    ]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001;
    let mut session = setup_test_session(&ctx, char_id).await;
    let kw = HashMap::new();
    let station_id = 60_003_760i64;

    let ship_id = ctx.db.with_mut_snapshot(|snap| {
        let sid = snap.characters.get(&char_id).map(|c| c.active_ship_id).unwrap_or(100_000_001);
        if let Some(ship) = snap.items.get_mut(&sid) {
            ship.type_id = 32880; // Venture
        }
        snap.items.retain(|_, item| item.location_id != sid);
        snap.items.insert(800_000_301, make_item(800_000_301, 33270, char_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(800_000_302, make_item(800_000_302, 33272, char_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(800_000_303, make_item(800_000_303, 18635, char_id, sid, InventoryFlag::Cargo, 3, false));
        sid
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);

    ctx.dispatch_rpc(&mut session, "invbroker", None, "Add", &[PyRep::Integer(800_000_301), PyRep::Integer(ship_id), PyRep::Integer(InventoryFlag::HiSlot0 as i64)], &kw, None)
        .await
        .expect("Survey Probe Launcher I fits on ship with 0 launcher hardpoints");
    let second_launcher_err = ctx
        .dispatch_rpc(&mut session, "invbroker", None, "Add", &[PyRep::Integer(800_000_302), PyRep::Integer(ship_id), PyRep::Integer(InventoryFlag::HiSlot1 as i64)], &kw, None)
        .await
        .expect_err("Second Survey Probe Launcher must fail maxGroupFitted == 1");
    assert_eq!(second_launcher_err.code, "FittingValidationFailed");
    assert!(second_launcher_err.message.contains("MaxGroupFittedExceeded"));

    ctx.dispatch_rpc(&mut session, "stationSvc", None, "Undock", &[], &kw, None).await.expect("Undock");
    let _ = session.drain_notifications();
    let _ = ctx.drain_notifications_for_char(char_id);
    let jita = ctx.systems.get_or_spawn(30000142);
    let moon_40009087_pos = jita.get_ball_snapshot(40009087).await.unwrap().unwrap().1.position;
    let ship_near_moon = jita.get_ball_snapshot(ship_id).await.unwrap().unwrap().1.position;

    if let Ok(Some((_, mut sb))) = jita.get_ball_snapshot(ship_id).await {
        sb.position = ship_near_moon;
        sb.velocity = -(moon_40009087_pos - ship_near_moon).normalize() * 100.0;
        let _ = jita.spawn_ball(sb).await;
    }
    let miss_unaligned = ctx
        .dispatch_rpc(&mut session, "dogmaIM", None, "Activate", &[PyRep::Integer(800_000_301), PyRep::String("useMissiles".into()), PyRep::Integer(40009087)], &kw, None)
        .await
        .expect("Launch while aligned away");
    assert_eq!(get_dict_val(&miss_unaligned, "moonID"), Some(&PyRep::None));
    assert_eq!(get_dict_val(&miss_unaligned, "flightTimeSec").unwrap().as_i64(), Some(300));
    assert_eq!(get_dict_val(&miss_unaligned, "remainingInLauncher").unwrap().as_i64(), Some(1));
    assert!(!ctx.db.with_snapshot(|s| s.module_active_states.contains(&800_000_301)));

    let miss_probe_id = get_dict_val(&miss_unaligned, "probeID").unwrap().as_i64().unwrap();
    let launch_notifs = session.drain_notifications();
    assert!(launch_notifs.iter().any(|n| n.0 == "OnNewMoonProbe"));
    assert!(!launch_notifs.iter().any(|n| n.0 == "OnMoonScanComplete"));

    let recover_res = ctx
        .dispatch_rpc(&mut session, "scanMgr", None, "RecoverProbes", &[PyRep::List(vec![PyRep::Integer(miss_probe_id)])], &kw, None)
        .await
        .expect("RecoverProbes");
    assert_eq!(get_dict_val(&recover_res, "recoveredCount").unwrap().as_i64(), Some(0));

    ctx.advance_time_seconds(300);
    let miss_arr_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(miss_arr_notifs.iter().any(|n| n.event == "OnRemoveMoonProbe"));
    assert!(!miss_arr_notifs.iter().any(|n| n.event == "OnMoonScanComplete"));

    let moon_40009081_pos = jita.get_ball_snapshot(40009081).await.unwrap().unwrap().1.position;
    if let Ok(Some((_, mut sb))) = jita.get_ball_snapshot(ship_id).await {
        sb.position = ship_near_moon;
        sb.velocity = (moon_40009081_pos - ship_near_moon).normalize() * 100.0;
        let _ = jita.spawn_ball(sb).await;
    }
    let miss_range = ctx
        .dispatch_rpc(&mut session, "dogmaIM", None, "Activate", &[PyRep::Integer(800_000_301), PyRep::String("useMissiles".into()), PyRep::Integer(40009081)], &kw, None)
        .await
        .expect("Launch at moon > 100,000 km");
    assert_eq!(get_dict_val(&miss_range, "moonID"), Some(&PyRep::None));
    assert_eq!(get_dict_val(&miss_range, "remainingInLauncher").unwrap().as_i64(), Some(1));
    ctx.advance_time_seconds(300);
    let _ = ctx.drain_notifications_for_char(char_id);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(m) = snap.items.get_mut(&800_000_301) {
            m.type_id = 33272;
        }
        snap.items.retain(|_, i| !(i.location_id == ship_id && i.item_id != 800_000_301));
        snap.items.insert(800_000_304, make_item(800_000_304, 18637, char_id, ship_id, InventoryFlag::HiSlot0, 1, false));
        snap.items.insert(800_000_305, make_item(800_000_305, 18626, char_id, ship_id, InventoryFlag::Cargo, 1, false));
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);

    if let Ok(Some((_, mut sb))) = jita.get_ball_snapshot(ship_id).await {
        sb.position = ship_near_moon;
        sb.velocity = (moon_40009087_pos - ship_near_moon).normalize() * 100.0;
        let _ = jita.spawn_ball(sb).await;
    }
    let gaze_launch = ctx
        .dispatch_rpc(&mut session, "dogmaIM", None, "Activate", &[PyRep::Integer(800_000_301), PyRep::String("useMissiles".into()), PyRep::Integer(40009087)], &kw, None)
        .await
        .expect("Launch Gaze Survey Probe I from T2 launcher");
    assert_eq!(get_dict_val(&gaze_launch, "moonID").unwrap().as_i64(), Some(40009087));
    assert_eq!(get_dict_val(&gaze_launch, "flightTimeSec").unwrap().as_i64(), Some(135));

    let quest_launch = ctx
        .dispatch_rpc(&mut session, "dogmaIM", None, "Activate", &[PyRep::Integer(800_000_301), PyRep::String("useMissiles".into()), PyRep::Integer(40009087)], &kw, None)
        .await
        .expect("Launch Quest Survey Probe I from T2 launcher");
    assert_eq!(get_dict_val(&quest_launch, "flightTimeSec").unwrap().as_i64(), Some(1080));

    ctx.dispatch_rpc(&mut session, "stationSvc", None, "Dock", &[PyRep::Integer(station_id)], &kw, None).await.expect("Dock before probes arrive");
    let _ = session.drain_notifications();
    ctx.advance_time_seconds(1080);
    let docked_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(docked_notifs.iter().any(|n| n.event == "OnRemoveMoonProbe"));
    assert!(!docked_notifs.iter().any(|n| n.event == "OnMoonScanComplete"));

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(800_000_306, make_item(800_000_306, 18635, char_id, ship_id, InventoryFlag::HiSlot0, 1, false));
    });
    ctx.dispatch_rpc(&mut session, "stationSvc", None, "Undock", &[], &kw, None).await.expect("Undock again");
    if let Ok(Some((_, mut sb))) = jita.get_ball_snapshot(ship_id).await {
        sb.position = ship_near_moon;
        sb.velocity = (moon_40009087_pos - ship_near_moon).normalize() * 100.0;
        let _ = jita.spawn_ball(sb).await;
    }
    let _ = session.drain_notifications();
    let _ = ctx.drain_notifications_for_char(char_id);

    let disc_launch = ctx
        .dispatch_rpc(&mut session, "dogmaIM", None, "Activate", &[PyRep::Integer(800_000_301), PyRep::String("useMissiles".into()), PyRep::Integer(40009087)], &kw, None)
        .await
        .expect("Launch Discovery Survey Probe I from T2 launcher");
    assert_eq!(get_dict_val(&disc_launch, "moonID").unwrap().as_i64(), Some(40009087));
    assert_eq!(get_dict_val(&disc_launch, "flightTimeSec").unwrap().as_i64(), Some(270));
    let _ = session.drain_notifications();

    ctx.advance_time_seconds(269);
    let early_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(!early_notifs.iter().any(|n| n.event == "OnMoonScanComplete" || n.event == "OnRemoveMoonProbe"));
    ctx.advance_time_seconds(1);
    let scan_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(scan_notifs.iter().any(|n| n.event == "OnRemoveMoonProbe"));
    let scan_complete = scan_notifs.iter().find(|n| n.event == "OnMoonScanComplete").expect("Must emit OnMoonScanComplete");
    assert_eq!(scan_complete.args.first().and_then(|v| v.as_i64()), Some(40009087));
    let Some(PyRep::Dict(comp_entries)) = scan_complete.args.get(1) else { panic!("Expected Dict") };
    assert!(!comp_entries.is_empty());
    for (k, v) in comp_entries {
        let tid = k.as_i64().unwrap() as u32;
        assert!(v.as_f64().unwrap() > 0.0);
        assert_eq!(ctx.sde.types.get(&tid).map(|t| t.group_id), Some(1884));
    }

    let char_corp_id = ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(1000170));
    let wh_structure_id = 1_000_000_001_005i64;
    let ls_structure_id = 1_000_000_001_006i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(wh_structure_id, make_structure(wh_structure_id, char_corp_id, 35835, 31000007, "WH Athanor", [1000.0, 2000.0, 3000.0]));
        snap.structures.insert(ls_structure_id, make_structure(ls_structure_id, char_corp_id, 35835, 30000012, "LS Athanor", [1000.0, 2000.0, 3000.0]));
    });
    let PyRep::List(wh_ores) = ctx.dispatch_rpc(&mut session, "moonExtractions", None, "GetMoonResourcesForStructure", &[PyRep::Integer(wh_structure_id)], &kw, None).await.unwrap() else { panic!() };
    for entry in &wh_ores {
        let tid = get_dict_val(entry, "typeID").unwrap().as_i64().unwrap() as u32;
        assert_eq!(ctx.sde.types.get(&tid).map(|t| t.group_id), Some(1884));
    }
    let PyRep::List(ls_ores) = ctx.dispatch_rpc(&mut session, "moonExtractions", None, "GetMoonResourcesForStructure", &[PyRep::Integer(ls_structure_id)], &kw, None).await.unwrap() else { panic!() };
    assert!(ls_ores.iter().any(|e| ctx.sde.types.get(&(get_dict_val(e, "typeID").unwrap().as_i64().unwrap() as u32)).is_some_and(|t| matches!(t.group_id, 1920..=1923))));
}

// ====================================================================
// Test 10b: MN-13, MN-14, MN-16: Moon Extraction, Fracture, Decay, Drill & Ledger
// ====================================================================
#[tokio::test]
async fn test_mn13_mn14_mn16_moon_mining_probes_jackpot_decay_and_ledger() {
    // parity: feat:mining.moon-extraction, feat:mining.moon-extraction-duration-yield, feat:mining.moon-fracture, feat:mining.moon-chunk-auto-fracture, feat:mining.moon-jackpot, feat:mining.moon-field-lifetime, feat:structures.service-moon-drill, feat:modules.mining-survey-chipset, feat:mining.metenox, feat:mining.corp-ledger, rpc:autoMoonMining.RequestMiningCycleOutput, rpc:autoMoonMining.RequestMiningDetails, rpc:corpMiningLedger.GetObserverLedger, rpc:corpMiningLedger.GetObserversWithMiningEvents, rpc:moonExtractions.CancelExtraction, rpc:moonExtractions.FractureChunkWithStructure, rpc:moonExtractions.GetExtractionAndEventIDFromStructureID, rpc:moonExtractions.GetExtractionsForCorp, rpc:moonExtractions.StartNewExtraction
    eve_testkit::attest(&[
        "feat:mining.moon-extraction",
        "feat:mining.moon-extraction-duration-yield",
        "feat:mining.moon-fracture",
        "feat:mining.moon-chunk-auto-fracture",
        "feat:mining.moon-jackpot",
        "feat:mining.moon-field-lifetime",
        "feat:structures.service-moon-drill",
        "feat:modules.mining-survey-chipset",
        "feat:mining.metenox",
        "feat:mining.corp-ledger",
    ]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001;
    let mut session = setup_test_session(&ctx, char_id).await;
    let kw = HashMap::new();

    let undock = ctx.dispatch_rpc(&mut session, "stationSvc", None, "Undock", &[], &kw, None).await.expect("Undock");
    let ship_id = get_dict_val(&undock, "shipID").unwrap().as_i64().unwrap();
    let jita = ctx.systems.get_or_spawn(30000142);

    let structure_id = 1_000_000_001_003i64;
    let metenox_id = 1_000_000_001_004i64;
    let wh_structure_id = 1_000_000_001_005i64;
    let auto_structure_id = 1_000_000_001_006i64;
    let char_corp_id = ctx.db.with_snapshot(|snap| snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(1000170));
    ctx.db.with_mut_snapshot(|snap| {
        for (sid, sys_id, st_type, name) in [
            (structure_id, 30000142u32, 35835u32, "Moon Mining Athanor (Jita High-Sec)"),
            (metenox_id, 30000012u32, 81826u32, "Automated Metenox Drill (Asabona Low-Sec)"),
            (wh_structure_id, 31000007u32, 35835u32, "Wormhole Athanor (J102512)"),
            (auto_structure_id, 30000012u32, 35835u32, "Low-Sec Auto-Fracture Athanor"),
        ] {
            snap.structures.insert(sid, make_structure(sid, char_corp_id, st_type, sys_id, name, [1000.0, 2000.0, 3000.0]));
        }
        snap.items.insert(800_000_305, make_item(800_000_305, 81143, char_corp_id, metenox_id, InventoryFlag::Hangar, 200, false));
    });

    for invalid_dur in [86_400i64, 5_000_000i64] {
        let dur_err = ctx
            .dispatch_rpc(&mut session, "moonExtractions", None, "StartNewExtraction", &[PyRep::Integer(structure_id), PyRep::Integer(invalid_dur)], &kw, None)
            .await
            .expect_err("Duration outside 6d..=56d must be rejected");
        assert_eq!(dur_err.code, "InvalidExtractionDuration");
    }

    let start_cancel_res = ctx
        .dispatch_rpc(&mut session, "moonExtractions", None, "StartNewExtraction", &[PyRep::Integer(structure_id), PyRep::Integer(518_400)], &kw, None)
        .await
        .expect("StartNewExtraction before cancel");
    assert_eq!(get_dict_val(&start_cancel_res, "status").unwrap().as_str(), Some("Started"));

    let cancel_res = ctx
        .dispatch_rpc(&mut session, "moonExtractions", None, "CancelExtraction", &[PyRep::Integer(structure_id)], &kw, None)
        .await
        .expect("CancelExtraction");
    assert_eq!(cancel_res, PyRep::Bool(true));
    let cancel_frac_err = ctx
        .dispatch_rpc(&mut session, "moonExtractions", None, "FractureChunkWithStructure", &[PyRep::Integer(structure_id)], &kw, None)
        .await
        .expect_err("Cancelled extraction must produce no chunk and refuse fracture");
    assert_eq!(cancel_frac_err.code, "InvalidExtractionState");

    let dur_10d = 864_000i64;
    let start_res = ctx
        .dispatch_rpc(&mut session, "moonExtractions", None, "StartNewExtraction", &[PyRep::Integer(structure_id), PyRep::Integer(dur_10d)], &kw, None)
        .await
        .expect("StartNewExtraction 10d");
    assert_eq!(get_dict_val(&start_res, "status").unwrap().as_str(), Some("Started"));
    let start_ts = get_dict_val(&start_res, "extractionStartTime").unwrap().as_i64().unwrap();
    let arr_ts = get_dict_val(&start_res, "chunkArrivalTime").unwrap().as_i64().unwrap();
    let chunk_vol = get_dict_val(&start_res, "chunkVolumeM3").unwrap().as_f64().unwrap();
    assert_eq!(arr_ts - start_ts, dur_10d);
    assert!((chunk_vol - 7_200_000.0).abs() < 1.0);

    let early_err = ctx
        .dispatch_rpc(&mut session, "moonExtractions", None, "FractureChunkWithStructure", &[PyRep::Integer(structure_id)], &kw, None)
        .await
        .expect_err("Fracture before chunk arrival must fail");
    assert_eq!(early_err.code, "ChunkNotReadyToFracture");

    ctx.advance_time_seconds(864_000);
    let non_jp_seed = (1u64..10_000)
        .find(|&s| eve_server::rng::MechanicRng::seeded(s).try_unit_f64().unwrap() >= 0.15)
        .expect("non-jackpot seed");
    ctx.set_mechanic_rng_seed(Some(non_jp_seed));
    let frac1_res = ctx
        .dispatch_rpc(&mut session, "moonExtractions", None, "FractureChunkWithStructure", &[PyRep::Integer(structure_id)], &kw, None)
        .await
        .expect("FractureChunkWithStructure #1");
    let PyRep::List(ast_ids_1) = get_dict_val(&frac1_res, "asteroidIDs").unwrap() else { panic!() };
    let PyRep::List(ore_types_1) = get_dict_val(&frac1_res, "oreTypes").unwrap() else { panic!() };
    assert_eq!(ast_ids_1.len(), 6);
    assert!(ore_types_1.iter().any(|t| ctx.sde.types.get(&(t.as_i64().unwrap() as u32)).is_some_and(|ty| ty.name.starts_with("Brimful "))));
    assert!(ore_types_1.iter().any(|t| ctx.sde.types.get(&(t.as_i64().unwrap() as u32)).is_some_and(|ty| !ty.name.starts_with("Brimful "))));

    let first_ast_id = ast_ids_1[0].as_i64().unwrap();
    let first_ast_type = ore_types_1[0].as_i64().unwrap() as u32;
    assert!(jita.get_ball_snapshot(first_ast_id).await.unwrap().is_some());

    let miner_mod_id = 800_000_310i64;
    let chipset_id = 800_000_311i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 32880;
        }
        snap.items.retain(|_, item| item.location_id != ship_id);
        snap.items.insert(miner_mod_id, make_item(miner_mod_id, 483, char_id, ship_id, InventoryFlag::HiSlot0, 1, true));
        snap.items.insert(chipset_id, make_item(chipset_id, 83463, char_id, ship_id, InventoryFlag::LoSlot0, 1, true));
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);
    let ast_pos = jita.get_ball_snapshot(first_ast_id).await.unwrap().unwrap().1.position;
    if let Ok(Some((_, mut sb))) = jita.get_ball_snapshot(ship_id).await {
        sb.position = ast_pos + DVec3::new(500.0, 0.0, 0.0);
        let _ = jita.spawn_ball(sb).await;
    }
    let mine_moon_res = crate::mining_cycle_engine::mine_once(&ctx, &mut session, miner_mod_id, first_ast_id).await.expect("Mine #1");
    let mined_moon_qty_1 = mine_moon_res.units_delivered;
    assert_eq!(mine_moon_res.ore_type_id, first_ast_type);
    assert!(mined_moon_qty_1 > 0);
    let mine_moon_res_2 = crate::mining_cycle_engine::mine_once(&ctx, &mut session, miner_mod_id, first_ast_id).await.expect("Mine #2");
    let mined_moon_qty = mined_moon_qty_1 + mine_moon_res_2.units_delivered;

    let dup_err = ctx
        .dispatch_rpc(&mut session, "moonExtractions", None, "FractureChunkWithStructure", &[PyRep::Integer(structure_id)], &kw, None)
        .await
        .expect_err("Repeated FractureChunkWithStructure must fail");
    assert_eq!(dup_err.code, "ExtractionAlreadyFractured");

    ctx.advance_time_seconds(172_801);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let PyRep::Tuple(ext_tup) = ctx
        .dispatch_rpc(&mut session, "moonExtractions", None, "GetExtractionAndEventIDFromStructureID", &[PyRep::Integer(structure_id)], &kw, None)
        .await
        .expect("GetExtractionAndEventIDFromStructureID after decay")
    else {
        panic!("Expected 2-tuple");
    };
    assert_eq!(get_dict_val(&ext_tup[0], "status").unwrap().as_str(), Some("Decayed"));
    assert!(jita.get_ball_snapshot(first_ast_id).await.unwrap().is_none());

    ctx.dispatch_rpc(&mut session, "moonExtractions", None, "StartNewExtraction", &[PyRep::Integer(auto_structure_id), PyRep::Integer(518_400)], &kw, None)
        .await
        .expect("StartNewExtraction on auto_structure_id");
    ctx.advance_time_seconds(518_400 + 10_800);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let PyRep::Tuple(auto_tup) = ctx
        .dispatch_rpc(&mut session, "moonExtractions", None, "GetExtractionAndEventIDFromStructureID", &[PyRep::Integer(auto_structure_id)], &kw, None)
        .await
        .expect("GetExtractionAndEventIDFromStructureID after auto-fracture window")
    else {
        panic!("Expected 2-tuple");
    };
    assert_eq!(get_dict_val(&auto_tup[0], "status").unwrap().as_str(), Some("Fractured"));
    assert_eq!(get_dict_val(&auto_tup[0], "autoFractured").unwrap().as_bool(), Some(true));
    assert!(get_dict_val(&auto_tup[0], "activeAsteroidCount").unwrap().as_i64().unwrap() >= 6);

    let wh_start = ctx
        .dispatch_rpc(&mut session, "moonExtractions", None, "StartNewExtraction", &[PyRep::Integer(wh_structure_id), PyRep::Integer(518_400)], &kw, None)
        .await
        .expect("StartNewExtraction on wh_structure_id");
    let PyRep::List(wh_base_ores) = get_dict_val(&wh_start, "moonOres").unwrap() else { panic!() };
    ctx.advance_time_seconds(518_400);
    let jp_seed = (1u64..10_000)
        .find(|&s| eve_server::rng::MechanicRng::seeded(s).try_unit_f64().unwrap() < 0.15)
        .expect("jackpot seed");
    ctx.set_mechanic_rng_seed(Some(jp_seed));
    let jp_frac = ctx
        .dispatch_rpc(&mut session, "moonExtractions", None, "FractureChunkWithStructure", &[PyRep::Integer(wh_structure_id)], &kw, None)
        .await
        .expect("FractureChunkWithStructure jackpot");
    assert_eq!(get_dict_val(&jp_frac, "isJackpot").unwrap().as_bool(), Some(true));
    assert_eq!(get_dict_val(&jp_frac, "jackpotBonus").unwrap().as_f64(), Some(2.0));
    let PyRep::List(jp_ores) = get_dict_val(&jp_frac, "oreTypes").unwrap() else { panic!() };
    assert_eq!(jp_ores.len(), wh_base_ores.len());
    for (idx, t) in jp_ores.iter().enumerate() {
        let jp_tid = t.as_i64().unwrap() as u32;
        let base_tid = get_dict_val(&wh_base_ores[idx], "typeID").unwrap().as_i64().unwrap() as u32;
        let name = &ctx.sde.types.get(&jp_tid).unwrap().name;
        assert!(name.starts_with("Glistening ") || name.starts_with("Bountiful ") || name.starts_with("Shimmering ") || name.starts_with("Shining ") || name.starts_with("Resplendent ") || name.starts_with("Sparkling ") || name.starts_with("Dazzling "));
        let base_mats = ctx.sde.type_materials.get(&base_tid).unwrap();
        let jp_mats = ctx.sde.type_materials.get(&jp_tid).unwrap();
        for bm in base_mats {
            let jm = jp_mats.iter().find(|m| m.material_type_id == bm.material_type_id).unwrap();
            assert_eq!(jm.quantity, bm.quantity * 2);
        }
    }

    let corp_exts = ctx.dispatch_rpc(&mut session, "moonExtractions", None, "GetExtractionsForCorp", &[], &kw, None).await.expect("GetExtractionsForCorp");
    let PyRep::List(corp_ext_list) = corp_exts else { panic!() };
    assert!(corp_ext_list.len() >= 3);

    // 5. Automated Metenox Moon Drill (`feat:mining.metenox`):
    let err_athanor = ctx
        .dispatch_rpc(&mut session, "autoMoonMining", None, "RequestMiningDetails", &[PyRep::Integer(structure_id)], &kw, None)
        .await
        .expect_err("Athanor must be rejected by autoMoonMining.RequestMiningDetails");
    assert_eq!(err_athanor.code, "InvalidStructureType");

    // Reset Metenox reagents & output bay before testing exact 2-hour clock progression
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&metenox_id).unwrap().fuel_blocks = 100;
        snap.items.retain(|_, i| !(i.location_id == metenox_id && i.flag == InventoryFlag::SpecializedMineralHold));
        snap.items.insert(800_000_305, make_item(800_000_305, 81143, char_corp_id, metenox_id, InventoryFlag::Hangar, 200, false));
        snap.auto_moon_miners.remove(&metenox_id);
    });
    let details = ctx
        .dispatch_rpc(&mut session, "autoMoonMining", None, "RequestMiningDetails", &[PyRep::Integer(metenox_id)], &kw, None)
        .await
        .expect("RequestMiningDetails on Metenox Moon Drill");
    assert_eq!(get_dict_val(&details, "efficiency").unwrap().as_f64(), Some(0.40));
    let hourly_yield = get_dict_val(&details, "hourlyYield").unwrap().as_i64().unwrap();

    let output = ctx
        .dispatch_rpc(&mut session, "autoMoonMining", None, "RequestMiningCycleOutput", &[PyRep::Integer(metenox_id)], &kw, None)
        .await
        .expect("RequestMiningCycleOutput read-only on Metenox Moon Drill");
    let PyRep::List(rate_tuples) = output else { panic!("Expected List") };
    assert!(!rate_tuples.is_empty());

    ctx.advance_time_seconds(7_200);
    let details_2h = ctx
        .dispatch_rpc(&mut session, "autoMoonMining", None, "RequestMiningDetails", &[PyRep::Integer(metenox_id)], &kw, None)
        .await
        .expect("RequestMiningDetails after 2h");
    assert_eq!(get_dict_val(&details_2h, "fuelRemaining").unwrap().as_i64(), Some(90));
    assert_eq!(get_dict_val(&details_2h, "magmaticGasRemaining").unwrap().as_i64(), Some(180));
    assert_eq!(get_dict_val(&details_2h, "harvestedUnitsInBay").unwrap().as_i64(), Some(hourly_yield * 2));

    // 6. Corporate Mining Ledger (`feat:mining.corp-ledger`):
    session.corp_role = 0;
    let unpriv_err = ctx
        .dispatch_rpc(&mut session, "corpMiningLedger", None, "GetObserversWithMiningEvents", &[], &kw, None)
        .await
        .expect_err("Unprivileged pilot must be denied corpMiningLedger access");
    assert_eq!(unpriv_err.code, "CrpAccessDenied");

    session.corp_role = eve_db::corp_roles::ROLE_ACCOUNTANT;
    let corp_ledger = ctx
        .dispatch_rpc(&mut session, "corpMiningLedger", None, "GetObserversWithMiningEvents", &[], &kw, None)
        .await
        .expect("GetObserversWithMiningEvents");
    assert!(corp_ledger.is_crowset());
    let obs_rows = decode_crowset_rows(&corp_ledger);
    assert!(obs_rows.iter().any(|r| r.first().and_then(|v| v.as_i64()) == Some(structure_id)));

    let entries = ctx
        .dispatch_rpc(&mut session, "corpMiningLedger", None, "GetObserverLedger", &[PyRep::Integer(structure_id)], &kw, None)
        .await
        .expect("GetObserverLedger for structure_id");
    assert!(entries.is_crowset());
    let ledger_rows = decode_crowset_rows(&entries);
    let matching_rows: Vec<&Vec<PyRep>> = ledger_rows
        .iter()
        .filter(|r| {
            r.first().and_then(|v| v.as_i64()) == Some(char_id)
                && r.get(1).and_then(|v| v.as_i64()) == Some(char_corp_id)
                && r.get(2).and_then(|v| v.as_i64()) == Some(first_ast_type as i64)
        })
        .collect();
    assert_eq!(matching_rows.len(), 1);
    assert_eq!(matching_rows[0].get(3).and_then(|v| v.as_i64()), Some(mined_moon_qty));
}
