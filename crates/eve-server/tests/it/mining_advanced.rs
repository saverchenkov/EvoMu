//! Advanced Mining Systems Integration Tests (`MN-13`..`MN-15`).
//!
//! Tests cover:
//! - `MN-13`: Moon extraction lifecycle: drill timer, chunk fracture, and moon asteroid belt spawning.
//! - `MN-14`: Auto moon mining service: Metenox Moon Drill 40% efficiency, fuel consumption, and output bay.
//! - `MN-15`: Corporation mining ledger service: observer mining volume tracking and query RPCs.

#![forbid(unsafe_code)]

use eve_db::{
    AutoMoonMinerRecord, InventoryFlag, InventoryItemRecord, UpwellStructureRecord,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

#[path = "mining_advanced_part2.rs"]
mod mining_advanced_part2;

pub(crate) fn make_item(
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
        ..Default::default()
    }
}

pub(crate) fn make_structure(
    structure_id: i64,
    type_id: u32,
    owner_corp_id: i64,
    solar_system_id: u32,
    name: &str,
    position: [f64; 3],
    fuel_blocks: u32,
) -> UpwellStructureRecord {
    UpwellStructureRecord {
        structure_id,
        type_id,
        owner_corp_id,
        solar_system_id,
        name: name.into(),
        position,
        state: "ShieldVulnerable".into(),
        quantum_core_installed: true,
        fuel_blocks,
        shield_hp: 50_000.0,
        armor_hp: 50_000.0,
        hull_hp: 50_000.0,
        reinforced_until_unix: 0,
        last_damage_tick: 0,
        last_damage_unix: 0,
        self_destruct_at_unix: None,
    }
}

pub(crate) async fn create_auth_session(ctx: &ServerContext, session_id: u64) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(session_id);

    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[
            PyRep::String("demo".into()),
            PyRep::String("demo-secret".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Login failed");

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
    .expect("SelectCharacterID failed");

    sess
}

// ============================================================================
// MN-13: Moon Extraction Lifecycle & Fracture
// ============================================================================
#[tokio::test]
async fn test_mn13_moon_extraction_lifecycle_and_fracture() {
    // parity: rpc:moonExtractions.CancelExtraction, rpc:moonExtractions.FractureChunkWithStructure, rpc:moonExtractions.GetExtractionAndEventIDFromStructureID, rpc:moonExtractions.GetExtractionsForCorp, rpc:moonExtractions.GetMoonResourcesForStructure, rpc:moonExtractions.StartNewExtraction
    eve_testkit::attest(&[
        "rpc:moonExtractions.CancelExtraction",
        "rpc:moonExtractions.FractureChunkWithStructure",
        "rpc:moonExtractions.GetExtractionAndEventIDFromStructureID",
        "rpc:moonExtractions.GetExtractionsForCorp",
        "rpc:moonExtractions.GetMoonResourcesForStructure",
        "rpc:moonExtractions.StartNewExtraction",
    ]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = create_auth_session(&ctx, 1301).await;

    let structure_id = 1_040_000_000_131_i64;
    let corp_id = 1_000_170_i64;
    let system_id = 30_000_142_u32;

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            structure_id,
            make_structure(structure_id, 35835, corp_id, system_id, "Jita IV Moon Drill", [100_000.0, 50_000.0, -200_000.0], 500),
        );
    });

    let res_ores = ctx
        .dispatch_rpc(&mut sess, "moonExtractions", None, "GetMoonResourcesForStructure", &[PyRep::Integer(structure_id)], &kw, None)
        .await
        .expect("GetMoonResourcesForStructure RPC");

    let PyRep::List(ore_list) = res_ores else { panic!("Expected List of moon resources") };
    assert_eq!(ore_list.len(), 3, "Expected 3 moon ore types in composition");

    let short_err = ctx
        .dispatch_rpc(&mut sess, "moonExtractions", None, "StartNewExtraction", &[PyRep::Integer(structure_id), PyRep::Integer(86_400)], &kw, None)
        .await
        .expect_err("Duration < 6d must be rejected");
    assert_eq!(short_err.code, "InvalidExtractionDuration");

    let now = ctx.now_unix();
    let start_res = ctx
        .dispatch_rpc(&mut sess, "moonExtractions", None, "StartNewExtraction", &[PyRep::Integer(structure_id), PyRep::Integer((now + 518_400) as i64)], &kw, None)
        .await
        .expect("StartNewExtraction RPC");

    let PyRep::KeyVal(start_kv) = start_res else { panic!("Expected KeyVal from StartNewExtraction") };
    assert_eq!(start_kv["status"].as_str(), Some("Started"));
    assert_eq!(start_kv["structureID"].as_i64(), Some(structure_id));

    let active_err = ctx
        .dispatch_rpc(&mut sess, "moonExtractions", None, "StartNewExtraction", &[PyRep::Integer(structure_id)], &kw, None)
        .await
        .expect_err("StartNewExtraction while Started must fail");
    assert_eq!(active_err.code, "ExtractionAlreadyActive");

    let ext_details = ctx
        .dispatch_rpc(&mut sess, "moonExtractions", None, "GetExtractionAndEventIDFromStructureID", &[PyRep::Integer(structure_id)], &kw, None)
        .await
        .expect("GetExtractionAndEventIDFromStructureID RPC");
    let PyRep::Tuple(ext_tup) = ext_details else { panic!("Expected (extraction, eventID) Tuple") };
    let PyRep::KeyVal(ext_kv) = &ext_tup[0] else { panic!("Expected KeyVal") };
    assert_eq!(ext_kv["status"].as_str(), Some("Started"));

    let corp_exts = ctx
        .dispatch_rpc(&mut sess, "moonExtractions", None, "GetExtractionsForCorp", &[PyRep::Integer(corp_id)], &kw, None)
        .await
        .expect("GetExtractionsForCorp RPC");
    let PyRep::List(corp_list) = corp_exts else { panic!("Expected List") };
    assert!(!corp_list.is_empty());

    let cancel_res = ctx
        .dispatch_rpc(&mut sess, "moonExtractions", None, "CancelExtraction", &[PyRep::Integer(structure_id)], &kw, None)
        .await
        .expect("CancelExtraction RPC");
    assert_eq!(cancel_res, PyRep::Bool(true));

    let status = ctx.db.with_snapshot(|snap| snap.moon_extractions.get(&structure_id).map(|e| e.status.clone()).unwrap());
    assert_eq!(status, "Cancelled");

    ctx.dispatch_rpc(&mut sess, "moonExtractions", None, "StartNewExtraction", &[PyRep::Integer(structure_id)], &kw, None)
        .await
        .expect("Restart StartNewExtraction");

    let early_err = ctx
        .dispatch_rpc(&mut sess, "moonExtractions", None, "FractureChunkWithStructure", &[PyRep::Integer(structure_id)], &kw, None)
        .await
        .expect_err("Fracture before chunk arrival must fail");
    assert_eq!(early_err.code, "ChunkNotReadyToFracture");

    ctx.advance_secs(518_400);

    let frac_res = ctx
        .dispatch_rpc(&mut sess, "moonExtractions", None, "FractureChunkWithStructure", &[PyRep::Integer(structure_id)], &kw, None)
        .await
        .expect("FractureChunkWithStructure RPC");

    let dup_err = ctx
        .dispatch_rpc(&mut sess, "moonExtractions", None, "FractureChunkWithStructure", &[PyRep::Integer(structure_id)], &kw, None)
        .await
        .expect_err("Repeated FractureChunkWithStructure must fail");
    assert_eq!(dup_err.code, "ExtractionAlreadyFractured");

    let PyRep::KeyVal(frac_kv) = frac_res else { panic!("Expected KeyVal") };
    assert_eq!(frac_kv["fractured"], PyRep::Bool(true));
    assert_eq!(frac_kv["status"].as_str(), Some("Fractured"));
    assert!(matches!(frac_kv["asteroidCount"].as_i64(), Some(3 | 6)));
    let PyRep::List(ast_id_list) = &frac_kv["asteroidIDs"] else { panic!("Expected asteroidIDs") };
    let spawned_ast_id = ast_id_list[0].as_i64().unwrap();

    let handle = ctx.systems.get_or_spawn(system_id);
    let ast_state = handle.get_asteroid_state(spawned_ast_id).await.unwrap();
    assert!(ast_state.unwrap().remaining_units >= 500);
}

// ============================================================================
// MN-14: Auto Moon Mining Service (Metenox Moon Drill)
// ============================================================================
#[tokio::test]
async fn test_mn14_auto_moon_mining_service() {
    // parity: feat:mining.metenox, rpc:autoMoonMining.RequestMiningCycleOutput, rpc:autoMoonMining.RequestMiningDetails
    eve_testkit::attest(&["feat:mining.metenox", "rpc:autoMoonMining.RequestMiningCycleOutput", "rpc:autoMoonMining.RequestMiningDetails"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess = create_auth_session(&ctx, 1401).await;

    let highsec_structure_id = 1_040_000_000_140_i64;
    let structure_id = 1_040_000_000_141_i64;
    let gas_item_id = 710_000_141_i64;
    let corp_id = 1_000_170_i64;
    let highsec_system_id = 30_000_142_u32; // Jita (High-Sec 0.95)
    let lowsec_system_id = 30_000_012_u32; // Asabona (Low-Sec 0.32)

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            highsec_structure_id,
            make_structure(highsec_structure_id, 81826, corp_id, highsec_system_id, "Highsec Invalid Moon Drill", [200_000.0, 0.0, -100_000.0], 100),
        );
        snap.structures.insert(
            structure_id,
            make_structure(structure_id, 81826, corp_id, lowsec_system_id, "Asabona Auto Moon Drill", [200_000.0, 0.0, -100_000.0], 100),
        );
        snap.items.insert(
            gas_item_id,
            make_item(gas_item_id, 81143, corp_id, structure_id, InventoryFlag::Hangar, 50, false),
        );
        snap.items.insert(
            gas_item_id + 1,
            make_item(gas_item_id + 1, 81143, 999_999_999, structure_id, InventoryFlag::Hangar, 500, false),
        );
        snap.auto_moon_miners.insert(
            structure_id,
            AutoMoonMinerRecord {
                structure_id,
                fuel_units_remaining: 999,
                magmatic_gas_remaining: 999,
                efficiency: 0.40,
                cycle_duration_seconds: 3600,
                last_cycle_unix: ctx.now_unix(),
                ore_type_id: 16633,
                hourly_yield: 500,
                status: "Active".into(),
            },
        );
    });

    // 0. High-Sec Metenox Moon Drill must be rejected (`InvalidSystemSecurity`)
    let hs_err = ctx
        .dispatch_rpc(&mut sess, "autoMoonMining", None, "RequestMiningDetails", &[PyRep::Integer(highsec_structure_id)], &kw, None)
        .await
        .expect_err("Metenox Moon Drill in High-Sec must be rejected");
    assert_eq!(hs_err.code, "InvalidSystemSecurity");

    // 1. RequestMiningDetails in Low-Sec (`Asabona`)
    let details_res = ctx
        .dispatch_rpc(&mut sess, "autoMoonMining", None, "RequestMiningDetails", &[PyRep::Integer(structure_id)], &kw, None)
        .await
        .expect("RequestMiningDetails RPC");

    let PyRep::KeyVal(det_kv) = details_res else { panic!("Expected KeyVal from RequestMiningDetails") };
    assert_eq!(det_kv["status"].as_str(), Some("Active"));
    assert_eq!(det_kv["fuelRemaining"].as_i64(), Some(100));
    assert_eq!(det_kv["magmaticGasRemaining"].as_i64(), Some(50));
    let unmutated_fuel = ctx.db.with_snapshot(|snap| snap.auto_moon_miners.get(&structure_id).map(|m| m.fuel_units_remaining));
    assert_eq!(unmutated_fuel, Some(999), "RequestMiningDetails must be read-only");
    assert_eq!(det_kv["efficiency"].as_f64(), Some(0.40));
    let hourly_yield = det_kv["hourlyYield"].as_i64().unwrap();
    assert!(hourly_yield > 0, "Hourly moon material yield must be positive");
    let primary_mat_tid = det_kv["primaryMaterialTypeID"].as_i64().unwrap() as u32;
    assert_eq!(ctx.sde.types.get(&primary_mat_tid).map(|t| t.group_id), Some(427));

    // RequestMiningCycleOutput is a read-only query returning a List of (type_id, qty, vol) tuples and does NOT harvest
    let cycle_res = ctx
        .dispatch_rpc(&mut sess, "autoMoonMining", None, "RequestMiningCycleOutput", &[PyRep::Integer(structure_id)], &kw, None)
        .await
        .expect("RequestMiningCycleOutput read-only RPC");
    let PyRep::List(rate_tuples) = cycle_res else { panic!("Expected List of (type_id, qty, vol) tuples") };
    let rate_sum: i64 = rate_tuples
        .iter()
        .map(|t| match t {
            PyRep::Tuple(el) => el[1].as_i64().unwrap(),
            _ => panic!("Expected Tuple"),
        })
        .sum();
    assert_eq!(rate_sum, hourly_yield);

    // Advance virtual clock by 2 hours (7,200s): server clock tick automatically harvests 2 hourly cycles
    ctx.advance_secs(7_200);

    let details_after = ctx
        .dispatch_rpc(&mut sess, "autoMoonMining", None, "RequestMiningDetails", &[PyRep::Integer(structure_id)], &kw, None)
        .await
        .expect("RequestMiningDetails after 2h");
    let PyRep::KeyVal(after_kv) = details_after else { panic!("Expected KeyVal") };
    assert_eq!(after_kv["fuelRemaining"].as_i64(), Some(90), "2 cycles consume 10 fuel blocks (100 -> 90)");
    assert_eq!(after_kv["magmaticGasRemaining"].as_i64(), Some(30), "2 cycles consume 20 Magmatic Gas (50 -> 30)");

    let rem_gas_qty = ctx.db.with_snapshot(|snap| snap.items.get(&gas_item_id).map(|it| it.quantity).unwrap_or(0));
    assert_eq!(rem_gas_qty, 30, "Magmatic Gas item quantity must be deducted from 50 to 30");

    // Verify Moon Materials (group 427) deposited into SpecializedMineralHold (and no pilot rows in observer_mining_ledger)
    let moon_mats_in_bay = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|it| {
                it.location_id == structure_id
                    && it.flag == InventoryFlag::SpecializedMineralHold
                    && ctx.sde.types.get(&it.type_id).is_some_and(|t| t.group_id == 427)
            })
            .map(|it| it.quantity as i64)
            .sum::<i64>()
    });
    assert_eq!(moon_mats_in_bay, hourly_yield * 2, "SpecializedMineralHold must receive 2x hourly Moon Materials");
    assert!(
        ctx.db.with_snapshot(|snap| snap.observer_mining_ledger.get(&structure_id).is_none_or(|v| v.is_empty())),
        "Automated Metenox harvesting must not write pilot character rows to observer_mining_ledger"
    );
}

// ============================================================================
// MN-15: Corp Mining Ledger & Observer Tracking
// ============================================================================
#[tokio::test]
async fn test_mn15_corp_mining_ledger_service_and_observer_tracking() {
    // parity: rpc:corpMiningLedger.GetObserverLedger, rpc:corpMiningLedger.GetObserversWithMiningEvents
    eve_testkit::attest(&["rpc:corpMiningLedger.GetObserverLedger", "rpc:corpMiningLedger.GetObserversWithMiningEvents"]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = create_auth_session(&ctx, 1501).await;

    let observer_id = 1_040_000_000_151_i64;
    let corp_id = 1_000_170_i64;
    let system_id = 30_000_142_u32;
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            observer_id,
            make_structure(observer_id, 35835, corp_id, system_id, "Jita Mining Observer", [150_000.0, 20_000.0, -80_000.0], 500),
        );
    });

    let _ = ctx.db.record_mining_ledger_entry(90_000_001, system_id, 1230, 2_500, now);
    let _ = ctx.db.record_mining_ledger_entry(90_000_001, system_id, 45492, 1_200, now);

    sess.corp_role = 0;
    let role_err = ctx
        .dispatch_rpc(&mut sess, "corpMiningLedger", None, "GetObserversWithMiningEvents", &[], &kw, None)
        .await
        .expect_err("GetObserversWithMiningEvents without Accountant/StationManager/Director role must fail");
    assert_eq!(role_err.code, "CrpAccessDenied");

    sess.corp_role = eve_db::corp_roles::ROLE_ACCOUNTANT;
    let obs_list_res = ctx
        .dispatch_rpc(&mut sess, "corpMiningLedger", None, "GetObserversWithMiningEvents", &[], &kw, None)
        .await
        .expect("GetObserversWithMiningEvents RPC");
    assert!(obs_list_res.is_crowset(), "GetObserversWithMiningEvents must return CRowset");

    let ledger_res = ctx
        .dispatch_rpc(&mut sess, "corpMiningLedger", None, "GetObserverLedger", &[PyRep::Integer(observer_id)], &kw, None)
        .await
        .expect("GetObserverLedger RPC");
    assert!(ledger_res.is_crowset(), "GetObserverLedger must return CRowset");

    let ledger = ctx.db.get_observer_mining_ledger(observer_id);
    assert_eq!(ledger.len(), 2, "Observer should track both mining events");
    assert_eq!(ledger[0].type_id, 1230);
    assert_eq!(ledger[0].quantity, 2_500);
    assert_eq!(ledger[1].type_id, 45492);
    assert_eq!(ledger[1].quantity, 1_200);
}
