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
            make_structure(structure_id, 81826, corp_id, lowsec_system_id, "Asabona Exhaustion Drill", [200_000.0, 0.0, -100_000.0], initial_fuel_blocks),
        );
        snap.items.insert(
            fuel_item_id,
            make_item(fuel_item_id, 4247, corp_id, structure_id, InventoryFlag::SpecializedFuelBay, initial_fuel_blocks as i32, false),
        );
        snap.items.insert(
            gas_item_id,
            make_item(gas_item_id, 81143, corp_id, structure_id, InventoryFlag::Hangar, initial_gas, false),
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
        assert_eq!(st_fuel, 3, "st.fuel_blocks must be synced to 3");
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

pub(crate) mod in_space_structure_compression_decompression {
    // Advanced Mining Systems Integration Tests Part 2 (`MN-16`..`MN-18`).


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

        let _ = ctx.db.record_mining_ledger_entry(char_id, system_id, 1230, 3_500, now);
        let _ = ctx.db.record_mining_ledger_entry(char_id, system_id, 1228, 1_500, now);

        let goal_res = ctx
            .dispatch_rpc(&mut sess, "seasonManager", None, "get_character_progress_by_character_id", &[PyRep::Integer(2)], &kw, None)
            .await
            .expect("seasonManager.get_character_progress_by_character_id");
        let PyRep::KeyVal(goal_kv) = goal_res else { panic!("Expected KeyVal") };
        assert_eq!(goal_kv["goalID"].as_i64(), Some(2));
        assert_eq!(goal_kv["currentProgress"].as_i64(), Some(5_000));
        assert_eq!(goal_kv["verifiedFromLedger"], PyRep::Bool(true));
    }
}
