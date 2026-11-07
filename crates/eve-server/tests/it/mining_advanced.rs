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
            make_item(gas_item_id, 81143, corp_id, structure_id, InventoryFlag::StructureFuel, 50, false),
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

// ============================================================================
// R-P0-METENOX-FUEL-DUP: Fuel blocks single source of truth & exhaustion
// ============================================================================
#[tokio::test]
async fn test_metenox_moon_drill_fuel_exhaustion_regression() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess = create_auth_session(&ctx, 1402).await;

    let structure_id = 1_040_000_000_142_i64;
    let corp_id = sess.corporation_id.unwrap();
    let lowsec_system_id = 30_000_012_u32;
    let fuel_item_id = 890_001_i64;
    let gas_item_id = 890_002_i64;

    // Deposit N = 23 fuel blocks into SpecializedFuelBay (flag 133).
    // Fuel per cycle is 5; gas per cycle is 10.
    // Expected runnable cycles = floor(23 / 5) = 4 cycles.
    let initial_fuel_blocks = 23;
    let initial_gas = 100;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            structure_id,
            make_structure(structure_id, 81826, corp_id, lowsec_system_id, "Asabona Exhaustion Drill", [200_000.0, 0.0, -100_000.0], 0),
        );
        snap.items.insert(
            fuel_item_id,
            make_item(fuel_item_id, 4247, corp_id, structure_id, InventoryFlag::SpecializedFuelBay, initial_fuel_blocks as i32, false),
        );
        snap.items.insert(
            gas_item_id,
            make_item(gas_item_id, 81143, corp_id, structure_id, InventoryFlag::StructureFuel, initial_gas, false),
        );
        snap.auto_moon_miners.insert(
            structure_id,
            AutoMoonMinerRecord {
                structure_id,
                fuel_units_remaining: initial_fuel_blocks,
                magmatic_gas_remaining: initial_gas as u32,
                efficiency: 0.40,
                cycle_duration_seconds: 3600,
                last_cycle_unix: ctx.now_unix(),
                ore_type_id: 16633,
                hourly_yield: 500,
                status: "Active".into(),
            },
        );
    });

    // Advance by 10 hours (10 cycles). Only 4 cycles have enough fuel (20 blocks needed, 3 remainder).
    ctx.advance_secs(10 * 3600);

    let details1 = ctx
        .dispatch_rpc(&mut sess, "autoMoonMining", None, "RequestMiningDetails", &[PyRep::Integer(structure_id)], &kw, None)
        .await
        .expect("RequestMiningDetails after 10h");
    let PyRep::KeyVal(kv1) = details1 else { panic!("Expected KeyVal") };
    assert_eq!(kv1["fuelRemaining"].as_i64(), Some(3), "23 fuel blocks must leave exactly 3 after 4 cycles");
    assert_eq!(kv1["magmaticGasRemaining"].as_i64(), Some(60), "100 gas must leave 60 after 4 cycles (4 * 10 = 40 consumed)");
    assert_eq!(kv1["status"].as_str(), Some("OutOfFuel"), "Drill must be OutOfFuel when remaining fuel (3) < fuel_per_cycle (5)");

    // Assert bay items and st.fuel_blocks are in sync and not duplicated
    ctx.db.with_snapshot(|snap| {
        let st_fuel = snap.structures.get(&structure_id).unwrap().fuel_blocks;
        assert_eq!(st_fuel, 0, "st.fuel_blocks reserve remains 0 when fueled from SpecializedFuelBay item");
        let item_fuel = snap.items.get(&fuel_item_id).map(|i| i.quantity).unwrap_or(0);
        assert_eq!(item_fuel, 3, "InventoryItemRecord quantity must be decremented to 3");
    });

    // Advance by another 10 hours (10 more cycles) with drill in OutOfFuel state.
    // Assert NO infinite refill occurred, no extra cycles ran, and fuel remains exactly 3.
    ctx.advance_secs(10 * 3600);

    let details2 = ctx
        .dispatch_rpc(&mut sess, "autoMoonMining", None, "RequestMiningDetails", &[PyRep::Integer(structure_id)], &kw, None)
        .await
        .expect("RequestMiningDetails after additional 10h");
    let PyRep::KeyVal(kv2) = details2 else { panic!("Expected KeyVal") };
    assert_eq!(kv2["fuelRemaining"].as_i64(), Some(3), "Fuel must remain 3 without any infinite refill");
    assert_eq!(kv2["status"].as_str(), Some("OutOfFuel"));

    let moon_mats = ctx.db.with_snapshot(|snap| {
        snap.items.values()
            .filter(|it| it.location_id == structure_id && it.flag == InventoryFlag::SpecializedMineralHold && ctx.sde.types.get(&it.type_id).is_some_and(|t| t.group_id == 427))
            .map(|it| it.quantity as i64)
            .sum::<i64>()
    });
    let hourly_yield = kv1["hourlyYield"].as_i64().unwrap();
    assert!(hourly_yield > 0, "Hourly moon material yield must be positive");
    assert_eq!(moon_mats, hourly_yield * 4, "Must produce exactly 4 cycles of moon materials, never minting beyond fuel");
}


pub(crate) mod moon_extraction_metenox_and_mining_ledgers {
    #[allow(unused_imports)]
    use super::*;

// Acceptance test suite for Advanced Mining, Moons, Sites & Ledgers (`Phase 5M`: `MN-13`..`MN-16`).
// // Validates canonical tasks `plan:MN-13`, `plan:MN-14`, `plan:MN-15`, `plan:MN-16`,
// and `notify:OnMoonExtractionUpdate` against the authoritative "Done when" criteria in `action_plan.md`.


use crate::action_plan_mining::{get_dict_val, make_item, make_structure, setup_pilot_session};
use crate::common;
use eve_db::{AsteroidRecord, InventoryFlag, TrainedSkillRecord};
use eve_destiny::{Ball, MovementMode};
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::mining_sites;
use glam::DVec3;
use std::collections::HashMap;

fn decode_crowset_rows(rep: &PyRep) -> Vec<Vec<PyRep>> {
    if let PyRep::ObjectEx { list, .. } = rep {
        return list
            .iter()
            .filter_map(|r| match r {
                PyRep::DBRow { columns, .. } => {
                    Some(columns.iter().map(|(_, v)| v.clone()).collect())
                }
                PyRep::List(values) | PyRep::Tuple(values) => Some(values.clone()),
                _ => None,
            })
            .collect();
    }
    Vec::new()
}

#[tokio::test]
async fn test_action_plan_mn13_to_mn16_moons_metenox_sites_ledgers() {
    // parity: plan:MN-13, plan:MN-14, plan:MN-15, plan:MN-16, notify:OnMoonExtractionUpdate
    eve_testkit::attest(&[
        "plan:MN-13",
        "plan:MN-14",
        "plan:MN-15",
        "plan:MN-16",
        "notify:OnMoonExtractionUpdate",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    eve_server::services::serverops_cluster::set_scheduled_downtime_enabled(&ctx, false);
    let kw = HashMap::new();
    let char_id = 90_000_001i64;
    let mut sess = setup_pilot_session(&ctx, char_id).await;

    let undock = ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None).await.expect("Undock");
    let ship_id = get_dict_val(&undock, "shipID").unwrap().as_i64().unwrap();
    let jita = ctx.systems.get_or_spawn(30000142);

    // 1. plan:MN-13 & notify:OnMoonExtractionUpdate: Moon extraction start, cancel, fracture, ore mix, jackpot & corp ledger
    let moon_structure_id = 1_000_000_001_030i64;
    let jackpot_structure_id = 1_000_000_001_032i64;
    let metenox_id = 1_000_000_001_031i64;
    let char_corp_id = ctx.db.with_snapshot(|snap| snap.characters[&char_id].corporation_id);

    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(char_id).or_default();
        for sid in [3386u32, 3410, 17940, 32918, 3429] {
            sk.insert(sid, TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 });
        }
        let corp = snap.corporations.entry(char_corp_id).or_insert_with(|| eve_db::CorporationRecord {
            corporation_id: char_corp_id,
            name: "Test Mining Corp".into(),
            ticker: "TMC".into(),
            ceo_id: char_id,
            alliance_id: None,
            tax_rate: 0.0,
            master_wallet: eve_db::IskCents(0),
            wallet_divisions: Vec::new(),
            description: String::new(),
            url: String::new(),
            hq_station_id: Some(60003760),
            members: Default::default(),
            member_roles: Default::default(),
            applications: Default::default(),
        });
        corp.ceo_id = char_id;
        corp.member_roles.insert(char_id, eve_db::corp_roles::ROLE_DIRECTOR);
        snap.structures.insert(
            moon_structure_id,
            make_structure(moon_structure_id, char_corp_id, 35835, 30000142, "Moon Athanor", [1000.0, 2000.0, 3000.0]),
        );
        snap.structures.insert(
            jackpot_structure_id,
            make_structure(jackpot_structure_id, char_corp_id, 35835, 31000007, "Wormhole Athanor", [1000.0, 2000.0, 3000.0]),
        );
        let mut metenox = make_structure(metenox_id, char_corp_id, 81826, 30000012, "Asabona Metenox", [200_000.0, 0.0, -100_000.0]);
        metenox.fuel_blocks = 200;
        snap.structures.insert(metenox_id, metenox);
        snap.items.insert(
            800_000_701,
            make_item(800_000_701, 81143, char_corp_id, metenox_id, InventoryFlag::StructureFuel, 500, false), // 500 Magmatic Gas
        );
        let mut outsider_ch = snap.characters[&char_id].clone();
        outsider_ch.character_id = 90_000_002;
        outsider_ch.corporation_id = 98_999_999;
        outsider_ch.structure_id = Some(moon_structure_id);
        snap.characters.insert(90_000_002, outsider_ch);
        let mut unpriv_ch = snap.characters[&char_id].clone();
        unpriv_ch.character_id = 90_000_003;
        unpriv_ch.structure_id = Some(moon_structure_id);
        snap.characters.insert(90_000_003, unpriv_ch);
        snap.characters.get_mut(&char_id).unwrap().structure_id = Some(moon_structure_id);
        snap.corporations.get_mut(&char_corp_id).unwrap().member_roles.insert(90_000_003, 0);
    });

    // Corp ACL denial: outsider (foreign corp) and unprivileged member (role 0) must be denied StartNewExtraction & FractureChunkWithStructure
    for (denied_cid, denied_corp) in [(90_000_002i64, 98_999_999i64), (90_000_003i64, char_corp_id)] {
        let mut d_sess = sess.clone();
        d_sess.character_id = Some(denied_cid);
        d_sess.corporation_id = Some(denied_corp);
        d_sess.structure_id = Some(moon_structure_id);
        d_sess.corp_role = 0;
        assert_eq!(ctx.dispatch_rpc(&mut d_sess, "moonExtractions", None, "StartNewExtraction", &[PyRep::Integer(518_400)], &kw, None).await.unwrap_err().code, "AccessDenied");
        assert_eq!(ctx.dispatch_rpc(&mut d_sess, "moonExtractions", None, "FractureChunkWithStructure", &[], &kw, None).await.unwrap_err().code, "AccessDenied");
    }

    sess.structure_id = Some(moon_structure_id);
    ctx.drain_notifications_for_char(char_id);
    let start_cancel = ctx
        .dispatch_rpc(&mut sess, "moonExtractions", None, "StartNewExtraction", &[PyRep::Integer(518_400)], &kw, None)
        .await
        .expect("StartNewExtraction before cancel");
    assert_eq!(get_dict_val(&start_cancel, "status").unwrap().as_str(), Some("Started"));
    assert!(
        ctx.drain_notifications_for_char(char_id).iter().any(|n| n.event == "OnMoonExtractionUpdate"),
        "StartNewExtraction must emit OnMoonExtractionUpdate"
    );

    let cancel_res = ctx
        .dispatch_rpc(&mut sess, "moonExtractions", None, "CancelExtraction", &[], &kw, None)
        .await
        .expect("0-arg CancelExtraction from session structure_id");
    assert_eq!(cancel_res, PyRep::Bool(true));
    assert!(
        ctx.drain_notifications_for_char(char_id)
            .iter()
            .any(|n| n.event == "OnMoonExtractionUpdate" && n.args == vec![PyRep::None]),
        "CancelExtraction must emit OnMoonExtractionUpdate(None)"
    );

    let dur_secs = 864_000u64; // 10 days (240h -> 7,200,000 m³ chunk)
    let start_res = ctx
        .dispatch_rpc(&mut sess, "moonExtractions", None, "StartNewExtraction", &[PyRep::Integer(dur_secs as i64)], &kw, None)
        .await
        .expect("StartNewExtraction 10d");
    let PyRep::List(planned_ores) = get_dict_val(&start_res, "moonOres").unwrap() else { panic!() };
    assert_eq!(planned_ores.len(), 3, "Moon composition must contain 3 SDE moon ores");

    // Firing moon drill laser before chunkArrivalTime must fail with ChunkNotReadyToFracture
    let early_err = ctx
        .dispatch_rpc(&mut sess, "moonExtractions", None, "FractureChunkWithStructure", &[], &kw, None)
        .await
        .expect_err("0-arg FractureChunkWithStructure before chunkArrivalTime must fail");
    assert_eq!(early_err.code, "ChunkNotReadyToFracture");

    ctx.advance_secs(dur_secs + 1);
    let non_jp_seed = (1u64..10_000)
        .find(|&s| eve_server::rng::MechanicRng::seeded(s).try_unit_f64().unwrap() >= 0.15)
        .unwrap();
    ctx.set_mechanic_rng_seed(Some(non_jp_seed));
    ctx.drain_notifications_for_char(char_id);
    let frac_res = ctx
        .dispatch_rpc(&mut sess, "moonExtractions", None, "FractureChunkWithStructure", &[], &kw, None)
        .await
        .expect("0-arg FractureChunkWithStructure");
    assert_eq!(
        ctx.dispatch_rpc(&mut sess, "moonExtractions", None, "CancelExtraction", &[], &kw, None).await.unwrap_err().code,
        "CannotCancelExtraction",
        "CancelExtraction must be rejected after chunk has fractured"
    );
    let frac_notifs = ctx.drain_notifications_for_char(char_id);
    let frac_update = frac_notifs
        .iter()
        .find(|n| n.event == "OnMoonExtractionUpdate")
        .expect("FractureChunkWithStructure must emit OnMoonExtractionUpdate");
    assert_eq!(get_dict_val(&frac_update.args[0], "status").and_then(|v| v.as_str()), Some("Fractured"));
    assert!(get_dict_val(&frac_update.args[0], "moonID").and_then(|v| v.as_i64()).is_some(), "OnMoonExtractionUpdate must include moonID");
    assert!(get_dict_val(&frac_update.args[0], "extractionStartTime").and_then(|v| v.as_i64()).is_some(), "OnMoonExtractionUpdate must include extractionStartTime");

    let PyRep::List(ast_ids) = get_dict_val(&frac_res, "asteroidIDs").unwrap() else { panic!() };
    let PyRep::List(ore_types) = get_dict_val(&frac_res, "oreTypes").unwrap() else { panic!() };
    assert_eq!(ast_ids.len(), 6, "Non-jackpot fracture spawns 6 asteroids (base + +15% Brimful variant for each of the 3 moon ores)");
    assert!(ore_types.iter().any(|t| ctx.sde.types[&(t.as_i64().unwrap() as u32)].name.starts_with("Brimful ")));

    // Mine the fractured moon asteroid 2 cycles and 2 belt ores (Veldspar + Scordite) so 3 distinct ore types are logged
    let first_moon_ast = ast_ids[0].as_i64().unwrap();
    let first_moon_type = ore_types[0].as_i64().unwrap() as u32;
    let miner_mod_id = 800_000_702i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 32880; // Venture
        }
        snap.items.retain(|_, i| i.location_id != ship_id);
        snap.items.insert(miner_mod_id, make_item(miner_mod_id, 483, char_id, ship_id, InventoryFlag::HiSlot0, 1, true));
    });
    ctx.invalidate_ship_fitting(ship_id);

    let moon_ast_pos = jita.get_ball_snapshot(first_moon_ast).await.unwrap().unwrap().1.position;
    if let Ok(Some((_, mut sb))) = jita.get_ball_snapshot(ship_id).await {
        sb.position = moon_ast_pos + DVec3::new(500.0, 0.0, 0.0);
        let _ = jita.spawn_ball(sb).await;
    }
    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&char_id).unwrap().structure_id = None);
    sess.structure_id = None;
    let moon_c1 = crate::mining_cycle_engine::mine_once(&ctx, &mut sess, miner_mod_id, first_moon_ast).await.expect("Moon cycle 1");
    let moon_c2 = crate::mining_cycle_engine::mine_once(&ctx, &mut sess, miner_mod_id, first_moon_ast).await.expect("Moon cycle 2");
    let total_moon_mined = moon_c1.units_delivered + moon_c2.units_delivered;
    assert!(total_moon_mined > 0);

    // Advance past V24 48h (172,800s) natural decay window, run production tick_background_cluster, and verify moon asteroid belt despawns
    ctx.advance_secs(172_800);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&char_id).unwrap().structure_id = Some(moon_structure_id));
    sess.structure_id = Some(moon_structure_id);
    let PyRep::Tuple(decayed_tup) = ctx.dispatch_rpc(&mut sess, "moonExtractions", None, "GetExtractionAndEventIDFromStructureID", &[], &kw, None).await.expect("0-arg GetExtractionAndEventIDFromStructureID after 48h decay") else { panic!("Expected 2-tuple (extraction, eventID)") };
    assert_eq!(decayed_tup.len(), 2);
    assert_eq!(get_dict_val(&decayed_tup[0], "status").and_then(|v| v.as_str()), Some("Decayed"));
    assert_eq!(get_dict_val(&decayed_tup[0], "activeAsteroidCount").and_then(|v| v.as_i64()), Some(0));
    assert!(decayed_tup[1].as_i64().is_some());
    assert!(ctx.db.get_asteroid(first_moon_ast).is_none() && jita.get_ball_snapshot(first_moon_ast).await.unwrap().is_none(), "Expired moon asteroid field must despawn after 48h V24 duration");

    // Also verify Jackpot (+100% variants) and auto-fracture after chunkArrivalTime + 3h (10,800s) on jackpot_structure_id via tick_background_cluster
    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&char_id).unwrap().structure_id = Some(jackpot_structure_id));
    sess.structure_id = Some(jackpot_structure_id);
    ctx.dispatch_rpc(&mut sess, "moonExtractions", None, "StartNewExtraction", &[PyRep::Integer(518_400)], &kw, None)
        .await
        .expect("StartNewExtraction on jackpot_structure_id for auto-fracture");
    ctx.advance_secs(518_400 + 10_801);
    ctx.drain_notifications_for_char(char_id);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let PyRep::Tuple(auto_tup) = ctx
        .dispatch_rpc(&mut sess, "moonExtractions", None, "GetExtractionAndEventIDFromStructureID", &[], &kw, None)
        .await
        .expect("0-arg GetExtractionAndEventIDFromStructureID after 3h auto-fracture delay")
    else {
        panic!("Expected 2-tuple");
    };
    assert_eq!(get_dict_val(&auto_tup[0], "status").and_then(|v| v.as_str()), Some("Fractured"));
    assert_eq!(get_dict_val(&auto_tup[0], "autoFractured").and_then(|v| v.as_bool()), Some(true));
    let PyRep::List(auto_ast_ids) = get_dict_val(&auto_tup[0], "asteroidIDs").unwrap() else { panic!() };
    let first_auto_ast = auto_ast_ids[0].as_i64().unwrap();
    assert!(
        ctx.drain_notifications_for_char(char_id)
            .iter()
            .any(|n| n.event == "OnMoonExtractionUpdate" && get_dict_val(&n.args[0], "autoFractured").and_then(|v| v.as_bool()) == Some(true)),
        "Auto-fracture after 3h delay must emit OnMoonExtractionUpdate with autoFractured == true"
    );

    ctx.dispatch_rpc(&mut sess, "moonExtractions", None, "StartNewExtraction", &[PyRep::Integer(518_400)], &kw, None)
        .await
        .expect("StartNewExtraction on jackpot_structure_id");
    ctx.advance_secs(518_401);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert!(ctx.db.get_asteroid(first_auto_ast).is_none(), "Previous auto-fractured field must still despawn after its 48h window even after a new extraction starts");
    let jp_seed = (1u64..10_000)
        .find(|&s| eve_server::rng::MechanicRng::seeded(s).try_unit_f64().unwrap() < 0.15)
        .unwrap();
    ctx.set_mechanic_rng_seed(Some(jp_seed));
    let jp_frac = ctx
        .dispatch_rpc(&mut sess, "moonExtractions", None, "FractureChunkWithStructure", &[], &kw, None)
        .await
        .expect("0-arg FractureChunkWithStructure jackpot");
    assert_eq!(get_dict_val(&jp_frac, "isJackpot").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(get_dict_val(&jp_frac, "jackpotBonus").and_then(|v| v.as_f64()), Some(2.0));
    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&char_id).unwrap().structure_id = None);
    sess.structure_id = None;

    // 2. plan:MN-14: Metenox Moon Drill — server-driven hourly harvesting into SpecializedMineralHold (40% yield, 5 fuel + 10 Magmatic Gas/h)
    // Reset Metenox fuel/gas and clear SpecializedMineralHold so we measure exact 0h -> 24h -> fuel-out transitions on the production background cluster tick
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&metenox_id).unwrap().fuel_blocks = 200;
        snap.items.retain(|_, i| !(i.location_id == metenox_id && i.flag == InventoryFlag::SpecializedMineralHold));
        snap.items.insert(800_000_701, make_item(800_000_701, 81143, char_corp_id, metenox_id, InventoryFlag::StructureFuel, 500, false));
        snap.auto_moon_miners.remove(&metenox_id);
        snap.service_state_blobs.insert("metenox_last_tick_unix".into(), ctx.now_unix().to_string());
    });

    // Independently compute expected hourly 40% moon material yield from SDE type_materials for the Metenox moon's 3 ores (727 units/h)
    let PyRep::List(met_ores) = ctx.dispatch_rpc(&mut sess, "moonExtractions", None, "GetMoonResourcesForStructure", &[PyRep::Integer(metenox_id)], &kw, None).await.expect("GetMoonResourcesForStructure Metenox") else { panic!() };
    let mut sde_mat_totals: std::collections::BTreeMap<u32, f64> = std::collections::BTreeMap::new();
    for kv in &met_ores {
        let tid = get_dict_val(kv, "typeID").unwrap().as_i64().unwrap() as u32;
        let frac = get_dict_val(kv, "yield").unwrap().as_f64().unwrap();
        let portions = (30_000.0 * frac) / (ctx.sde.types[&tid].volume.max(0.1) * (ctx.sde.types[&tid].portion_size.max(1) as f64));
        for m in ctx.sde.type_materials[&tid].iter().filter(|m| ctx.sde.types[&m.material_type_id].group_id == 427) {
            *sde_mat_totals.entry(m.material_type_id).or_insert(0.0) += portions * (m.quantity as f64) * 0.40;
        }
    }
    let expected_hourly_sde: i64 = sde_mat_totals.values().map(|q| q.round() as i64).sum();
    assert_eq!(expected_hourly_sde, 727);

    let details_0h = ctx
        .dispatch_rpc(&mut sess, "autoMoonMining", None, "RequestMiningDetails", &[PyRep::Integer(metenox_id)], &kw, None)
        .await
        .expect("RequestMiningDetails at 0h");
    assert_eq!(get_dict_val(&details_0h, "efficiency").and_then(|v| v.as_f64()), Some(0.40));
    assert_eq!(get_dict_val(&details_0h, "fuelRemaining").and_then(|v| v.as_i64()), Some(200));
    assert_eq!(get_dict_val(&details_0h, "magmaticGasRemaining").and_then(|v| v.as_i64()), Some(500));
    assert!(get_dict_val(&details_0h, "availableFuel").is_some() && get_dict_val(&details_0h, "nextHarvestTime").is_some());
    assert!(get_dict_val(&details_0h, "outputMaterials").is_some() && get_dict_val(&details_0h, "remainingOutputBayCapacity").is_some());
    let hourly_yield = get_dict_val(&details_0h, "hourlyYield").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(hourly_yield, expected_hourly_sde, "hourlyYield must match independently computed SDE 40% moon material yield");

    // RequestMiningCycleOutput is a read-only query returning a list of (type_id, qty, vol) tuples and does NOT trigger harvesting
    let cyc_rate_res = ctx
        .dispatch_rpc(&mut sess, "autoMoonMining", None, "RequestMiningCycleOutput", &[PyRep::Integer(metenox_id)], &kw, None)
        .await
        .expect("RequestMiningCycleOutput read-only query");
    let PyRep::List(rate_tuples) = &cyc_rate_res else { panic!("Expected List of (type_id, qty, vol) tuples") };
    let rate_sum: i64 = rate_tuples
        .iter()
        .map(|t| match t {
            PyRep::Tuple(el) => el[1].as_i64().unwrap(),
            _ => panic!("Expected Tuple"),
        })
        .sum();
    assert_eq!(rate_sum, expected_hourly_sde, "Read-only RequestMiningCycleOutput per-cycle quantities sum to SDE hourlyYield");
    let bay_qty_0h: i32 = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| i.location_id == metenox_id && i.flag == InventoryFlag::SpecializedMineralHold)
            .map(|i| i.quantity)
            .sum()
    });
    assert_eq!(bay_qty_0h, 0, "Zero output exists before advancing the server clock, even after calling RequestMiningCycleOutput");

    // Advance 24 simulated hours (86,400 seconds) and run production tick_background_cluster: harvests 24 hourly cycles into SpecializedMineralHold
    ctx.clock().advance_secs(24 * 3_600);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let details_24h = ctx
        .dispatch_rpc(&mut sess, "autoMoonMining", None, "RequestMiningDetails", &[PyRep::Integer(metenox_id)], &kw, None)
        .await
        .expect("RequestMiningDetails after 24h");
    assert_eq!(get_dict_val(&details_24h, "fuelRemaining").and_then(|v| v.as_i64()), Some(80), "24h consumes 120 fuel blocks (5/h -> 80 left)");
    assert_eq!(get_dict_val(&details_24h, "magmaticGasRemaining").and_then(|v| v.as_i64()), Some(260), "24h consumes 240 Magmatic Gas (10/h -> 260 left)");
    let bay_qty_24h: i64 = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| i.location_id == metenox_id && i.flag == InventoryFlag::SpecializedMineralHold)
            .map(|i| {
                assert_eq!(ctx.sde.types[&i.type_id].group_id, 427, "Deposited item must be group 427 Moon Material");
                i.quantity as i64
            })
            .sum()
    });
    assert_eq!(bay_qty_24h, expected_hourly_sde * 24, "24 simulated hours deposit 24x SDE hourly 40% yield into SpecializedMineralHold");
    assert!(
        ctx.db.with_snapshot(|snap| snap.observer_mining_ledger.get(&metenox_id).is_none_or(|v| v.is_empty())),
        "Automated Metenox harvesting must not write pilot character rows to observer_mining_ledger"
    );

    // Advance another 20 hours and run production tick_background_cluster: only 16 cycles run (80 / 5 = 16h) before fuel blocks hit 0 and harvesting halts with status OutOfFuel
    ctx.clock().advance_secs(20 * 3_600);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let details_exhausted = ctx
        .dispatch_rpc(&mut sess, "autoMoonMining", None, "RequestMiningDetails", &[PyRep::Integer(metenox_id)], &kw, None)
        .await
        .expect("RequestMiningDetails after fuel exhaustion");
    assert_eq!(get_dict_val(&details_exhausted, "fuelRemaining").and_then(|v| v.as_i64()), Some(0));
    assert_eq!(get_dict_val(&details_exhausted, "status").and_then(|v| v.as_str()), Some("OutOfFuel"));
    let bay_qty_44h: i64 = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| i.location_id == metenox_id && i.flag == InventoryFlag::SpecializedMineralHold)
            .map(|i| i.quantity as i64)
            .sum()
    });
    assert_eq!(bay_qty_44h, expected_hourly_sde * 40, "Harvesting halts after 40 total funded hours when fuel is exhausted");

    // 3. plan:MN-15: Mining Sites — Ore anomaly, ice belt, and gas site spawn with SDE ore/ice/gas sets, probe scan resolution & despawn after depletion
    let osmon_id = 30_000_180u32; // Osmon (0.7 high-sec)
    mining_sites::seed_system_mining_sites_from_sde(&ctx, osmon_id).await;
    mining_sites::seed_system_mining_sites_from_sde(&ctx, 30_002_537).await; // Amamake (Low-Sec -> Saintly Nebula gas site)
    let ice_belt = mining_sites::resolve_regional_ice_belt(&ctx.sde, osmon_id).unwrap().expect("Blue Ice Belt in Osmon");
    assert_eq!((ice_belt.name.as_str(), ice_belt.asteroids.len(), ice_belt.ore_types(), mining_sites::ICE_BELT_RESPAWN_DELAY_SECS), ("Blue Ice Belt", 21, vec![16264], 14_400));
    let (gas_sig_id, gas_pos) = ctx.db.with_snapshot(|snap| snap.cosmic_signatures[&30_002_537].iter().find(|s| s.sig_group == "Gas" && s.type_name == "Saintly Nebula").map(|s| (s.signature_id.clone(), DVec3::from_array(s.position))).expect("Saintly Nebula gas sig"));
    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&char_id).unwrap().solar_system_id = 30_002_537);
    sess.solar_system_id = Some(30_002_537);
    let ls_scan_rows = crate::common::probe_scan(&ctx, &mut sess, gas_pos, 0.25 * 149_597_870_700.0).await;
    assert_eq!(crate::common::scan_row_field(&ls_scan_rows, &gas_sig_id, "signalStrength").as_f64(), Some(1.0), "scanMgr.RequestScans must resolve Saintly Nebula gas signature to 100%");
    assert_eq!(crate::common::scan_row_field(&ls_scan_rows, &gas_sig_id, "type").as_str(), Some("Saintly Nebula"));
    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&char_id).unwrap().solar_system_id = osmon_id);
    sess.solar_system_id = Some(osmon_id);
    let osmon = ctx.systems.get_or_spawn(osmon_id);

    let scan_before = ctx.dispatch_rpc(&mut sess, "scanSvc", None, "GetSystemScanMgr", &[], &kw, None).await.expect("GetSystemScanMgr");
    let PyRep::List(anoms_before) = get_dict_val(&scan_before, "anomalies").unwrap() else { panic!() };
    let ore_anom = anoms_before
        .iter()
        .find(|a| get_dict_val(a, "siteName").and_then(|v| v.as_str()) == Some("Small Kernite and Omber Deposit"))
        .expect("Small Kernite and Omber Deposit anomaly in Osmon");
    assert!(ctx.db.with_snapshot(|snap| snap.cosmic_signatures[&osmon_id].iter().any(|s| s.sig_group == "Ore" && s.type_name == "Hidden Omber Deposit")), "Hidden Omber Deposit ore signature in Osmon");
    let PyRep::List(anom_ast_reps) = get_dict_val(ore_anom, "asteroidBallIDs").unwrap() else { panic!() };
    let anom_ast_ids: Vec<i64> = anom_ast_reps.iter().map(|v| v.as_i64().unwrap()).collect();
    assert_eq!(anom_ast_ids.len(), 18, "Osmon ore anomaly spawns 18 SDE dungeon asteroids");
    let mut anom_ore_types: Vec<u32> = ctx.db.with_snapshot(|snap| anom_ast_ids.iter().map(|id| snap.asteroids[id].type_id).collect());
    anom_ore_types.sort_unstable();
    anom_ore_types.dedup();
    assert_eq!(anom_ore_types, vec![20, 1227, 17452, 17453, 17867, 17868], "Small Kernite and Omber Deposit must spawn exact SDE Kernite & Omber variant set");

    // Clear all but the last anomaly rock, then mine the final rock to 0 and verify the anomaly despawns
    let last_anom_rock = *anom_ast_ids.last().unwrap();
    let last_rock_pos = ctx.db.with_snapshot(|snap| {
        let p = snap.asteroids[&last_anom_rock].position;
        DVec3::new(p.0, p.1, p.2)
    });
    ctx.db.with_mut_snapshot(|snap| {
        for &id in &anom_ast_ids[..anom_ast_ids.len() - 1] {
            if let Some(a) = snap.asteroids.get_mut(&id) {
                a.quantity = 0;
            }
        }
        if let Some(a) = snap.asteroids.get_mut(&last_anom_rock) {
            a.quantity = 10;
        }
        snap.items.retain(|_, i| !(i.location_id == ship_id && i.flag == InventoryFlag::SpecializedOreHold));
    });
    osmon
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 32880,
            position: last_rock_pos + DVec3::new(500.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 2.0,
            mass: 1_200_000.0,
            radius: 30.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await
        .unwrap();
    let clear_cycle = crate::mining_cycle_engine::mine_once(&ctx, &mut sess, miner_mod_id, last_anom_rock)
        .await
        .expect("Mine final anomaly asteroid to 0");
    assert_eq!(clear_cycle.asteroid_remaining, 0);
    let scan_after = ctx.dispatch_rpc(&mut sess, "scanSvc", None, "GetSystemScanMgr", &[], &kw, None).await.expect("GetSystemScanMgr after clear");
    let PyRep::List(anoms_after) = get_dict_val(&scan_after, "anomalies").unwrap() else { panic!() };
    assert!(
        !anoms_after.iter().any(|a| get_dict_val(a, "siteName").and_then(|v| v.as_str()) == Some("Small Kernite and Omber Deposit")),
        "Cleared ore anomaly must despawn from scanSvc.GetSystemScanMgr"
    );

    // Verify regional Ice Belt ("Blue Ice Belt") spawned in Osmon cosmic_anomalies, depletes & despawns, and respawns after 4h (14,400s)
    let ice_anom = anoms_after
        .iter()
        .find(|a| get_dict_val(a, "siteName").and_then(|v| v.as_str()) == Some("Blue Ice Belt"))
        .expect("Blue Ice Belt cosmic anomaly spawned in Osmon");
    let PyRep::List(ice_ast_reps) = get_dict_val(ice_anom, "asteroidBallIDs").unwrap() else { panic!() };
    let ice_ast_ids: Vec<i64> = ice_ast_reps.iter().map(|v| v.as_i64().unwrap()).collect();
    assert_eq!(ice_ast_ids.len(), 21, "Blue Ice Belt spawns 21 SDE Blue Ice (16264) asteroids");
    assert!(ctx.db.with_snapshot(|snap| ice_ast_ids.iter().all(|id| snap.asteroids[id].type_id == 16264 && snap.asteroids[id].quantity == 1_000)));
    let last_ice_rock = *ice_ast_ids.last().unwrap();
    let last_ice_pos = ctx.db.with_snapshot(|snap| {
        let p = snap.asteroids[&last_ice_rock].position;
        DVec3::new(p.0, p.1, p.2)
    });
    let ice_harv_id = 800_000_720i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().type_id = 22544; // Hulk (can fit Ice Harvester I)
        snap.items.insert(ice_harv_id, make_item(ice_harv_id, 16278, char_id, ship_id, InventoryFlag::HiSlot1, 1, true));
        snap.skills.get_mut(&char_id).unwrap().insert(16281, eve_db::TrainedSkillRecord { type_id: 16281, level: 5, skill_points: 256_000.0 });
        for &id in &ice_ast_ids[..ice_ast_ids.len() - 1] {
            if let Some(a) = snap.asteroids.get_mut(&id) {
                a.quantity = 0;
            }
        }
        if let Some(a) = snap.asteroids.get_mut(&last_ice_rock) {
            a.quantity = 1;
        }
        snap.items.retain(|_, i| !(i.location_id == ship_id && i.flag == InventoryFlag::SpecializedOreHold));
    });
    ctx.invalidate_ship_fitting(ship_id);
    osmon.try_set_ball_position(ship_id, last_ice_pos + DVec3::new(500.0, 0.0, 0.0));
    let ice_clear = crate::mining_cycle_engine::mine_once(&ctx, &mut sess, ice_harv_id, last_ice_rock)
        .await
        .expect("Mine final Blue Ice asteroid to 0");
    assert_eq!(ice_clear.asteroid_remaining, 0);
    let scan_ice_cleared = ctx.dispatch_rpc(&mut sess, "scanSvc", None, "GetSystemScanMgr", &[], &kw, None).await.expect("GetSystemScanMgr after ice clear");
    let PyRep::List(anoms_ice_cleared) = get_dict_val(&scan_ice_cleared, "anomalies").unwrap() else { panic!() };
    assert!(
        !anoms_ice_cleared.iter().any(|a| get_dict_val(a, "siteName").and_then(|v| v.as_str()) == Some("Blue Ice Belt")),
        "Depleted Blue Ice Belt must despawn from scanSvc"
    );
    ctx.advance_secs(14_399);
    let scan_before_4h = ctx.dispatch_rpc(&mut sess, "scanSvc", None, "GetSystemScanMgr", &[], &kw, None).await.expect("GetSystemScanMgr at 4h - 1s");
    let PyRep::List(anoms_before_4h) = get_dict_val(&scan_before_4h, "anomalies").unwrap() else { panic!() };
    assert!(
        !anoms_before_4h.iter().any(|a| get_dict_val(a, "siteName").and_then(|v| v.as_str()) == Some("Blue Ice Belt")),
        "Blue Ice Belt must remain despawned at 14,399s (< 4h)"
    );
    ctx.advance_secs(1);
    let scan_at_4h = ctx.dispatch_rpc(&mut sess, "scanSvc", None, "GetSystemScanMgr", &[], &kw, None).await.expect("GetSystemScanMgr at 4h");
    let PyRep::List(anoms_at_4h) = get_dict_val(&scan_at_4h, "anomalies").unwrap() else { panic!() };
    let respawned_ice = anoms_at_4h
        .iter()
        .find(|a| get_dict_val(a, "siteName").and_then(|v| v.as_str()) == Some("Blue Ice Belt"))
        .expect("Blue Ice Belt must respawn 4 hours (14,400s) after depletion");
    let PyRep::List(respawned_ice_balls) = get_dict_val(respawned_ice, "asteroidBallIDs").unwrap() else { panic!() };
    assert_eq!(respawned_ice_balls.len(), 21, "Respawned Blue Ice Belt has 21 fresh Blue Ice asteroids");

    // 4. plan:MN-16: Character & Corp Mining Ledgers (90-day retention filter: 60d retained, 91d pruned + waste tracking + outsider CrpAccessDenied)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().type_id = 32880;
        snap.items.remove(&ice_harv_id);
        snap.items.retain(|_, i| !(i.location_id == ship_id && i.flag == InventoryFlag::SpecializedOreHold));
    });
    ctx.invalidate_ship_fitting(ship_id);
    let scordite_rock = 750_000_710i64;
    ctx.db.spawn_asteroid(AsteroidRecord {
        asteroid_id: scordite_rock,
        solar_system_id: osmon_id,
        belt_id: 40_000_001,
        type_id: 1228, // Scordite
        quantity: 10_000,
        initial_quantity: 10_000,
        position: (last_ice_pos.x + 600.0, last_ice_pos.y, last_ice_pos.z),
        radius: 60.0,
    });
    let _ = crate::mining_cycle_engine::mine_once(&ctx, &mut sess, miner_mod_id, scordite_rock).await.expect("Mine Scordite");
    let retained_60d_ts = ctx.now_unix().saturating_sub(60 * 86_400);
    let expired_ts = ctx.now_unix().saturating_sub(91 * 86_400);
    ctx.db.record_mining_ledger_entry_with_waste(char_id, osmon_id, 17452, 777, 111, retained_60d_ts).expect("Record 60d Silvery Omber with waste");
    ctx.db.with_mut_snapshot(|snap| {
        snap.mining_ledger.push(eve_db::MiningLedgerRecord {
            character_id: char_id, solar_system_id: osmon_id, type_id: 18, quantity: 999, date: "2026-01-01".into(), timestamp_unix: expired_ts,
        });
        snap.observer_mining_ledger.entry(moon_structure_id).or_default().push(eve_db::ObserverMiningRecord {
            observer_id: moon_structure_id, character_id: char_id, recorded_corporation_id: char_corp_id, type_id: 18, quantity: 999, last_updated_date: "2026-01-01".into(), timestamp_unix: expired_ts,
        });
    });

    let char_ledger = ctx
        .dispatch_rpc(&mut sess, "characterMiningLedger", None, "GetCharacterLogs", &[], &kw, None)
        .await
        .expect("GetCharacterLogs");
    let PyRep::List(char_entries) = &char_ledger else { panic!() };
    assert!(
        !char_entries.iter().any(|e| get_dict_val(e, "typeID").and_then(|v| v.as_i64()) == Some(18)),
        "GetCharacterLogs must prune 91-day-old Plagioclase (18) entry"
    );
    let omber_60d_row = char_entries
        .iter()
        .find(|e| get_dict_val(e, "typeID").and_then(|v| v.as_i64()) == Some(17452))
        .expect("60-day-old Silvery Omber (17452) entry must be retained within 90-day ledger window");
    assert_eq!(get_dict_val(omber_60d_row, "quantity").and_then(|v| v.as_i64()), Some(777));
    assert_eq!(get_dict_val(omber_60d_row, "quantityWasted").and_then(|v| v.as_i64()), Some(111));
    assert_eq!(get_dict_val(omber_60d_row, "waste").and_then(|v| v.as_i64()), Some(111));
    let moon_char_row = char_entries
        .iter()
        .find(|e| get_dict_val(e, "typeID").and_then(|v| v.as_i64()) == Some(first_moon_type as i64))
        .expect("Aggregated moon ore row in character ledger");
    assert_eq!(get_dict_val(moon_char_row, "quantity").and_then(|v| v.as_i64()), Some(total_moon_mined));

    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    let corp_ledger = ctx
        .dispatch_rpc(&mut sess, "corpMiningLedger", None, "GetObserverLedger", &[PyRep::Integer(moon_structure_id)], &kw, None)
        .await
        .expect("GetObserverLedger for owner corp director");
    assert!(corp_ledger.is_crowset());
    let obs_rows = decode_crowset_rows(&corp_ledger);
    assert!(!obs_rows.iter().any(|r| r.get(2).and_then(|v| v.as_i64()) == Some(18)), "GetObserverLedger must exclude 91-day-old expired entries");
    let moon_obs_rows: Vec<_> = obs_rows
        .iter()
        .filter(|r| r.first().and_then(|v| v.as_i64()) == Some(char_id) && r.get(2).and_then(|v| v.as_i64()) == Some(first_moon_type as i64))
        .collect();
    assert_eq!(moon_obs_rows.len(), 1, "Both moon mining cycles aggregate into 1 observer ledger row");
    assert_eq!(moon_obs_rows[0].get(3).and_then(|v| v.as_i64()), Some(total_moon_mined));

    // Outsider character (different corporation) must be denied access to corp observer ledger
    let outsider_char = 90_000_002i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters[&char_id].clone();
        ch.character_id = outsider_char;
        ch.corporation_id = 98_999_999;
        snap.characters.insert(outsider_char, ch);
    });
    let mut outsider_sess = sess.clone();
    outsider_sess.character_id = Some(outsider_char);
    outsider_sess.corporation_id = Some(98_999_999);
    let outsider_err = ctx
        .dispatch_rpc(&mut outsider_sess, "corpMiningLedger", None, "GetObserverLedger", &[PyRep::Integer(moon_structure_id)], &kw, None)
        .await
        .expect_err("Outsider must be denied GetObserverLedger");
    assert_eq!(outsider_err.code, "CrpAccessDenied");
}
}

#[allow(unused_imports, dead_code)]
mod ore_compression_npc_mining_and_goals {
    use super::*;

// Advanced Mining Systems Integration Tests (`MN-16`..`MN-18`).


use super::{create_auth_session, make_item, make_structure};
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use eve_proto::PyRep;
use eve_server::living_world::{LivingWorldConvoy, LivingWorldDirector, NpcArchetype};
use eve_server::router::ServerContext;
use eve_server::system_actor::AsteroidState;
use glam::DVec3;
use std::collections::HashMap;

// ============================================================================
// MN-16: In-Space & Structure Compression & Decompression
// ============================================================================
#[tokio::test]
async fn test_mn16_in_space_and_structure_compression_and_decompression() {
    // parity: rpc:inSpaceCompressionMgr.CompressItemInSpace, rpc:structureCompressionMgr.CompressItemInStructure, rpc:structureCompressionMgr.DecompressGasInStructure, rpc:structureCompressionMgr.GetMyGasDecompressionEfficiency
    eve_testkit::attest(&[
        "rpc:inSpaceCompressionMgr.CompressItemInSpace",
        "rpc:structureCompressionMgr.CompressItemInStructure",
        "rpc:structureCompressionMgr.DecompressGasInStructure",
        "rpc:structureCompressionMgr.GetMyGasDecompressionEfficiency",
    ]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = create_auth_session(&ctx, 1601).await;

    let char_id = 90_000_001_i64;
    let structure_id = 1_040_000_000_161_i64;
    let corp_id = 1_000_170_i64;
    let system_id = 30_000_142_u32;

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            structure_id,
            make_structure(structure_id, 35835, corp_id, system_id, "Jita Compression Facility", [0.0, 0.0, 0.0], 500),
        );
    });

    // A. In-Space Compression
    let undock = ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None).await.expect("Undock RPC");
    let PyRep::KeyVal(undock_kv) = undock else { panic!("Expected undock kv") };
    let ship_id = undock_kv["shipID"].as_i64().unwrap();

    let item_in_space = 710_000_001_i64;
    let comp_mod_id = 710_000_002_i64;
    let core_mod_id = 710_000_099_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship_it) = snap.items.get_mut(&ship_id) {
            ship_it.type_id = 28606; // Orca
        }
        snap.items.retain(|_, item| item.location_id != ship_id);
        for tid in [62625_u32, 58945] {
            let t = &ctx.sde.types[&tid];
            for (sk_attr, lvl_attr) in [(182_u16, 277_u16), (183, 278), (184, 279)] {
                if let Some(&sk) = t.default_attributes.get(&sk_attr) {
                    let lvl = t.default_attributes.get(&lvl_attr).copied().unwrap_or(1.0) as u8;
                    snap.skills.entry(char_id).or_default().insert(
                        sk as u32,
                        eve_db::TrainedSkillRecord { type_id: sk as u32, level: lvl.max(1), skill_points: 256_000.0 },
                    );
                }
            }
        }
        snap.items.insert(comp_mod_id, make_item(comp_mod_id, 62625, char_id, ship_id, InventoryFlag::HiSlot0, 1, true));
        snap.items.insert(item_in_space, make_item(item_in_space, 1230, char_id, ship_id, InventoryFlag::Cargo, 1_045, false));
    });

    let cargo_err = ctx
        .dispatch_rpc(&mut sess, "inSpaceCompressionMgr", None, "CompressItemInSpace", &[PyRep::Integer(item_in_space)], &kw, None)
        .await
        .expect_err("CompressItemInSpace from general Cargo must fail");
    assert_eq!(cargo_err.code, "NotInMiningHold");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&item_in_space) {
            it.flag = InventoryFlag::SpecializedOreHold;
        }
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);

    let inactive_err = ctx
        .dispatch_rpc(&mut sess, "inSpaceCompressionMgr", None, "CompressItemInSpace", &[PyRep::Integer(item_in_space)], &kw, None)
        .await
        .expect_err("CompressItemInSpace with an inactive compressor must fail");
    assert_eq!(inactive_err.code, "NoActiveCompressorInRange");

    let no_core_act_err = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(comp_mod_id), PyRep::String("compressor".into())], &kw, None)
        .await
        .expect_err("Activating the compressor without an active Industrial Core must fail");
    assert_eq!(no_core_act_err.code, "IndustrialCoreNotActive");
    let no_core_err = ctx
        .dispatch_rpc(&mut sess, "inSpaceCompressionMgr", None, "CompressItemInSpace", &[PyRep::Integer(item_in_space)], &kw, None)
        .await
        .expect_err("CompressItemInSpace without active Industrial Core must fail");
    assert_eq!(no_core_err.code, "NoActiveCompressorInRange");

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            core_mod_id,
            InventoryItemRecord {
                item_id: core_mod_id, type_id: 58945, owner_id: char_id, location_id: ship_id,
                flag: InventoryFlag::HiSlot1, quantity: 1, singleton: true, version: 1, ..Default::default()
            },
        );
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(core_mod_id), PyRep::String("industrialCore".into())], &kw, None)
        .await
        .expect("Activate Large Industrial Core I");
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(comp_mod_id), PyRep::String("compressor".into())], &kw, None)
        .await
        .expect("Activate Large Asteroid Ore Compressor I");
    assert!(ctx.db.with_snapshot(|snap| {
        snap.module_active_states.contains(&core_mod_id) && snap.module_active_states.contains(&comp_mod_id)
    }));

    let comp_space_res = ctx
        .dispatch_rpc(&mut sess, "inSpaceCompressionMgr", None, "CompressItemInSpace", &[PyRep::Integer(item_in_space)], &kw, None)
        .await
        .expect("CompressItemInSpace RPC");

    let PyRep::KeyVal(space_kv) = comp_space_res else { panic!("Expected KeyVal from CompressItemInSpace") };
    let compressed_tid = space_kv["compressedTypeID"].as_i64().unwrap() as u32;
    let compressed_qty = space_kv["quantity"].as_i64().unwrap();
    let rem_qty = space_kv["remainderUnits"].as_i64().unwrap();

    let expected_comp_tid = *ctx.sde.compressible_types.get(&1230).unwrap();
    assert_eq!(compressed_tid, expected_comp_tid, "Compressed Veldspar type ID matches SDE");
    assert_eq!(compressed_qty, 1_045, "V24 1:1 compression converts all 1,045 units");
    assert_eq!(rem_qty, 0, "V24 1:1 compression leaves 0 remainder units");

    let hold_items = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|it| it.location_id == ship_id && it.owner_id == char_id)
            .map(|it| (it.type_id, it.quantity))
            .collect::<Vec<_>>()
    });
    assert!(hold_items.contains(&(expected_comp_tid, 1_045)), "Mining hold must contain 1,045 compressed units");

    // B. Structure Compression
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.structure_id = Some(structure_id);
            ch.station_id = None;
        }
    });
    sess.station_id = None;
    sess.structure_id = Some(structure_id);

    let item_in_struct = 710_000_004_i64;
    let moon_ore_in_struct = 710_000_005_i64;
    let reproc_svc_mod_id = 710_000_009_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(item_in_struct, make_item(item_in_struct, 1228, char_id, structure_id, InventoryFlag::Hangar, 500, false));
        snap.items.insert(moon_ore_in_struct, make_item(moon_ore_in_struct, 45492, char_id, structure_id, InventoryFlag::Hangar, 500, false));
        snap.items.insert(reproc_svc_mod_id, make_item(reproc_svc_mod_id, 35892, corp_id, structure_id, InventoryFlag::Hangar, 1, true));
    });
    let unfitted_svc_err = ctx
        .dispatch_rpc(&mut sess, "structureCompressionMgr", None, "CompressItemInStructure", &[PyRep::Integer(structure_id), PyRep::Integer(item_in_struct)], &kw, None)
        .await
        .expect_err("Unfitted reprocessing module in hangar must not enable structure compression");
    assert_eq!(unfitted_svc_err.code, "ReprocessingServiceOffline");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(m) = snap.items.get_mut(&reproc_svc_mod_id) {
            m.flag = InventoryFlag::ServiceSlot0;
        }
    });

    let moon_comp_err = ctx
        .dispatch_rpc(&mut sess, "structureCompressionMgr", None, "CompressItemInStructure", &[PyRep::Integer(structure_id), PyRep::Integer(moon_ore_in_struct)], &kw, None)
        .await
        .expect_err("Moon Ore compression in structure must be rejected");
    assert_eq!(moon_comp_err.code, "StructureCannotCompressType");

    let struct_comp_res = ctx
        .dispatch_rpc(&mut sess, "structureCompressionMgr", None, "CompressItemInStructure", &[PyRep::Integer(structure_id), PyRep::Integer(item_in_struct)], &kw, None)
        .await
        .expect("CompressItemInStructure RPC");
    let PyRep::KeyVal(str_kv) = struct_comp_res else { panic!("Expected KeyVal from CompressItemInStructure") };
    assert_eq!(str_kv["quantity"].as_i64(), Some(500));

    // C. Structure Gas Decompression
    let eff_untrained = ctx
        .dispatch_rpc(&mut sess, "structureCompressionMgr", None, "GetMyGasDecompressionEfficiency", &[PyRep::Integer(structure_id)], &kw, None)
        .await
        .expect("GetMyGasDecompressionEfficiency untrained RPC");
    assert_eq!(eff_untrained.as_f64(), Some(0.84));

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            62452,
            eve_db::TrainedSkillRecord { type_id: 62452, level: 5, skill_points: 512_000.0 },
        );
    });

    let eff_res = ctx
        .dispatch_rpc(&mut sess, "structureCompressionMgr", None, "GetMyGasDecompressionEfficiency", &[PyRep::Integer(structure_id)], &kw, None)
        .await
        .expect("GetMyGasDecompressionEfficiency RPC");
    assert_eq!(eff_res.as_f64(), Some(0.89));

    let non_gas_err = ctx
        .dispatch_rpc(&mut sess, "structureCompressionMgr", None, "DecompressGasInStructure", &[PyRep::Integer(structure_id), PyRep::Integer(item_in_struct)], &kw, None)
        .await
        .expect_err("Decompressing non-gas compressed ore must fail");
    assert_eq!(non_gas_err.code, "NotCompressedGas");

    let comp_gas_item = 710_000_003_i64;
    let remote_gas_item = 710_000_006_i64;
    let courier_locked_gas_item = 710_000_007_i64;
    let other_structure_id = 1_040_000_000_169_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            other_structure_id,
            make_structure(other_structure_id, 35836, corp_id, system_id, "Remote Tatara", [50_000.0, 0.0, 0.0], 500),
        );
        snap.items.insert(comp_gas_item, make_item(comp_gas_item, 62399, char_id, structure_id, InventoryFlag::Hangar, 100, false));
        snap.items.insert(remote_gas_item, make_item(remote_gas_item, 62399, char_id, 60_003_760, InventoryFlag::Hangar, 100, false));
        snap.items.insert(courier_locked_gas_item, make_item(courier_locked_gas_item, 62399, char_id, 990_000_001, InventoryFlag::Locked, 100, false));
    });

    let spoof_struct_err = ctx
        .dispatch_rpc(&mut sess, "structureCompressionMgr", None, "DecompressGasInStructure", &[PyRep::Integer(other_structure_id), PyRep::Integer(comp_gas_item)], &kw, None)
        .await
        .expect_err("DecompressGasInStructure with spoofed structure_id must fail");
    assert_eq!(spoof_struct_err.code, "InvalidStructure");

    let remote_err = ctx
        .dispatch_rpc(&mut sess, "structureCompressionMgr", None, "DecompressGasInStructure", &[PyRep::Integer(structure_id), PyRep::Integer(remote_gas_item)], &kw, None)
        .await
        .expect_err("DecompressGasInStructure on remote item must fail");
    assert_eq!(remote_err.code, "ItemNotInStructureHangar");

    let locked_err = ctx
        .dispatch_rpc(&mut sess, "structureCompressionMgr", None, "DecompressGasInStructure", &[PyRep::Integer(structure_id), PyRep::Integer(courier_locked_gas_item)], &kw, None)
        .await
        .expect_err("DecompressGasInStructure on courier-locked item must fail");
    assert_eq!(locked_err.code, "ItemLocked");
    assert_eq!(ctx.db.with_snapshot(|snap| snap.items.get(&courier_locked_gas_item).map(|i| i.quantity)), Some(100));

    let decomp_res = ctx
        .dispatch_rpc(&mut sess, "structureCompressionMgr", None, "DecompressGasInStructure", &[PyRep::Integer(structure_id), PyRep::Integer(comp_gas_item)], &kw, None)
        .await
        .expect("DecompressGasInStructure RPC");
    let PyRep::KeyVal(dec_kv) = decomp_res else { panic!("Expected KeyVal from DecompressGasInStructure") };
    assert_eq!(dec_kv["decompressedTypeID"].as_i64(), Some(30370));
    assert_eq!(dec_kv["decompressedQuantity"].as_i64(), Some(89));
    assert_eq!(dec_kv["efficiency"].as_f64(), Some(0.89));

    let hangar_raw = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .find(|it| it.location_id == structure_id && it.owner_id == char_id && it.type_id == 30370 && it.quantity == 89)
            .cloned()
    });
    assert!(hangar_raw.is_some(), "Hangar must contain decompressed raw Fullerite-C50 with 89 units");
}

// ============================================================================
// MN-17: Living World NPC Mining Ops & Market Supply Hauling
// ============================================================================
#[tokio::test]
async fn test_mn17_living_world_npc_mining_ops_and_market_hauling() {
    let ctx = ServerContext::new();
    ctx.clock().set_paused(true);
    let director = LivingWorldDirector::new();

    let convoy_id = 800_000_099_i64;
    let system_id = 30_000_144_u32; // Perimeter
    let dest_station_id = 60_003_766_i64;
    let ore_type_id = 1230_u32;

    let handle = ctx.systems.get_or_spawn(system_id);
    let ast_id = 750_000_171_i64;
    handle
        .spawn_asteroid(
            AsteroidState {
                asteroid_id: ast_id, belt_id: 40_000_001, type_id: ore_type_id,
                initial_units: 60_000, remaining_units: 60_000, unit_volume: 0.1, depleted: false, respawn_at_tick: None,
            },
            Ball {
                id: ast_id, type_id: ore_type_id, position: DVec3::new(5_000.0, 0.0, 0.0), velocity: DVec3::ZERO,
                max_velocity: 0.0, inertia: 0.0, mass: 1.0e8, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop,
            },
        )
        .await
        .unwrap();

    director.register_convoy(LivingWorldConvoy {
        convoy_id, archetype: NpcArchetype::MiningOp, ship_type_id: 32880,
        origin_system_id: system_id, current_system_id: system_id, destination_system_id: system_id,
        destination_station_id: dest_station_id, cargo_type_id: ore_type_id, cargo_quantity: 49_200,
        lod_level: 1, destroyed: false,
    });

    // First step initializes mining cycle tracking (elapsed = 0.0s per R-LW-RATE-01)
    let _ = director.step_npc_mining_op(convoy_id, &ctx.systems, &ctx.db, &ctx.sde).await.expect("init mining op");
    ctx.advance_secs(60);
    let step_result = director.step_npc_mining_op(convoy_id, &ctx.systems, &ctx.db, &ctx.sde).await.expect("step_npc_mining_op succeeded");
    assert_eq!(step_result.convoy_id, convoy_id);
    assert!(step_result.asteroid_id > 0);
    assert_eq!(step_result.ore_type_id, ore_type_id);
    let expected_mined = if ctx.sde.types.contains_key(&32880) { 1_600 } else { 800 };
    assert_eq!(step_result.mined_units, expected_mined);
    assert!(step_result.remaining_asteroid_units > 0);
    assert!(!step_result.hauled_to_station);
    assert_eq!(step_result.convoy_cargo_units, 0);
    assert!(director.hauler_cargo_units(&ctx.db, convoy_id) >= 50_000);

    ctx.advance_secs(60);
    let deliver_result = director.step_npc_mining_op(convoy_id, &ctx.systems, &ctx.db, &ctx.sde).await.expect("deliver_result");
    assert!(deliver_result.hauled_to_station);
    assert_eq!(director.hauler_cargo_units(&ctx.db, convoy_id), 0);

    let market_order = ctx.db.with_snapshot(|snap| {
        snap.market_orders.values().find(|o| !o.is_bid && o.station_id == dest_station_id && o.type_id == ore_type_id).cloned()
    });
    assert!(market_order.unwrap().vol_remaining >= 50_000);
}

// ============================================================================
// MN-18: Mining Missions & Career Goals from Mining Ledger
// ============================================================================
#[tokio::test]
async fn test_mn18_mining_missions_and_career_goals_from_ledger() {
    // parity: rpc:agentMgr.GetAgents, rpc:agentMgr.GetMyJournalDetails, rpc:seasonManager.get_character_progress_by_character_id
    eve_testkit::attest(&["rpc:agentMgr.GetAgents", "rpc:agentMgr.GetMyJournalDetails", "rpc:seasonManager.get_character_progress_by_character_id"]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = create_auth_session(&ctx, 1801).await;

    let char_id = 90_000_001_i64;
    let now = ctx.now_unix();

    let agents_rep = ctx
        .dispatch_rpc(&mut sess, "agentMgr", None, "GetAgents", &[PyRep::Integer(1), PyRep::Integer(23)], &kw, None)
        .await
        .expect("agentMgr.GetAgents for L1 Mining");
    let PyRep::Rowset { rows: agent_rows, .. } = agents_rep else { panic!("Expected Rowset") };
    assert!(!agent_rows.is_empty());
    let mining_agent = &agent_rows[0];
    let mining_agent_id = mining_agent[0].as_i64().unwrap();
    let system_id = mining_agent[5].as_i64().unwrap() as u32;
    let station_id = mining_agent[6].as_i64().unwrap_or(60_003_760);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = system_id;
            ch.station_id = Some(station_id);
        }
    });
    sess.solar_system_id = Some(system_id);
    sess.station_id = Some(station_id);

    let bind_res = ctx
        .dispatch_rpc(&mut sess, "agentMgr", None, "MachoBindObject", &[PyRep::Integer(mining_agent_id), PyRep::None], &kw, None)
        .await
        .expect("MachoBindObject on agentMgr");
    let PyRep::Tuple(bind_tuple) = bind_res else { panic!("Expected Tuple") };
    let agent_oid = bind_tuple[0].as_i64().unwrap() as u64;

    let offer_res = ctx
        .dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(2)], &kw, None)
        .await
        .expect("agentMgr.DoAction(RequestMission)");
    let mission_id = offer_res.dict_get("missionID").and_then(|v| v.as_i64()).expect("missionID") as u32;
    assert_eq!(offer_res.dict_get("state").and_then(|v| v.as_str()), Some("Offered"));

    let accept_res = ctx
        .dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(3)], &kw, None)
        .await
        .expect("agentMgr.DoAction(Accept)");
    assert_eq!(accept_res.dict_get("state").and_then(|v| v.as_str()), Some("Accepted"));
    let bookmark_id = accept_res.dict_get("bookmarkID").and_then(|v| v.as_i64()).expect("bookmarkID");

    let journal_res = ctx.dispatch_rpc(&mut sess, "agentMgr", None, "GetMyJournalDetails", &[], &kw, None).await.expect("GetMyJournalDetails");
    let PyRep::Tuple(journal_tuple) = journal_res else { panic!("Expected Tuple") };
    let PyRep::List(journal_missions) = &journal_tuple[0] else { panic!("Expected List") };
    assert!(journal_missions.iter().any(|m| m.dict_get("missionID").and_then(|v| v.as_i64()) == Some(mission_id as i64) && m.dict_get("missionState").and_then(|v| v.as_i64()) == Some(2)));

    let pocket_ast_id = 910_000_000 + (mission_id as i64) * 1000 + 1;
    let handle = ctx.systems.get_or_spawn(system_id);
    let pocket_ast = handle.get_asteroid_state(pocket_ast_id).await.unwrap();
    assert_eq!(pocket_ast.unwrap().type_id, 1230);
    assert!(ctx.db.with_snapshot(|snap| snap.bookmarks.contains_key(&bookmark_id)));

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, it| !(it.owner_id == char_id && it.type_id == 1230));
    });

    let incomplete_res = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(6)], &kw, None).await;
    assert_eq!(incomplete_res.err().unwrap().code, "MissionIncomplete");

    let ore_item_id = 710_000_018_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(ore_item_id, make_item(ore_item_id, 1230, char_id, station_id, InventoryFlag::Hangar, 500, false));
    });

    let complete_res = ctx
        .dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(6)], &kw, None)
        .await
        .expect("agentMgr.DoAction(Complete)");
    assert_eq!(complete_res.dict_get("missionID").and_then(|v| v.as_i64()), Some(mission_id as i64));
    assert_eq!(ctx.db.with_snapshot(|snap| snap.active_missions.get(&(char_id, mission_id)).map(|m| m.state.clone())).as_deref(), Some("Completed"));
    assert_eq!(ctx.db.with_snapshot(|snap| snap.items.get(&ore_item_id).map(|it| it.quantity).unwrap_or(0)), 0);

    let mut earned_ind_cp = 0u32;
    for g in ctx.quasar_gateway.progression.career_goals.get_or_init_goals(char_id as u32) {
        if g.def.career_id == 2 && earned_ind_cp < 750 {
            let _ = ctx.quasar_gateway.progression.career_goals.mark_completed(char_id as u32, &g.uuid, now);
            earned_ind_cp += g.def.career_points;
        }
    }

    let goal_res = ctx
        .dispatch_rpc(&mut sess, "seasonManager", None, "get_character_progress_by_character_id", &[PyRep::Integer(2)], &kw, None)
        .await
        .expect("seasonManager.get_character_progress_by_character_id");
    let PyRep::KeyVal(goal_kv) = goal_res else { panic!("Expected KeyVal") };
    assert_eq!(goal_kv["goalID"].as_i64(), Some(2));
    assert_eq!(goal_kv["currentProgress"].as_i64(), Some(earned_ind_cp as i64));
    assert_eq!(goal_kv["verifiedFromCareerPoints"], PyRep::Bool(true));
}

}
