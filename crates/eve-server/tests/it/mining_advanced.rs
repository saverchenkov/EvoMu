//! Advanced Mining Systems Integration Tests (`MN-13`..`MN-18`).
//!
//! Tests cover:
//! - `MN-13`: Moon extraction lifecycle: drill timer, chunk fracture, and moon asteroid belt spawning.
//! - `MN-14`: Auto moon mining service: Metenox Moon Drill 40% efficiency, fuel consumption, and output bay.
//! - `MN-15`: Corporation mining ledger service: observer mining volume tracking and query RPCs.
//! - `MN-16`: In-space and structure compression & decompression: 100:1 ratio, remainder splits, gas decompression efficiency.
//! - `MN-17`: Living world NPC mining ops: asteroid harvesting, cargo accumulation, and market supply replenishment.
//! - `MN-18`: Mining missions and career progression verified strictly from mining ledger entries.

#![forbid(unsafe_code)]

use eve_db::{
    AutoMoonMinerRecord, InventoryFlag, InventoryItemRecord, UpwellStructureRecord,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::living_world::{LivingWorldConvoy, LivingWorldDirector, NpcArchetype};
use eve_server::router::ServerContext;
use eve_server::system_actor::AsteroidState;
use glam::DVec3;
use std::collections::HashMap;

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
        ..Default::default()
    }
}

fn make_structure(
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
    }
}

// ============================================================================
// Helper: Authenticate session with character 90_000_001
// ============================================================================
async fn create_auth_session(ctx: &ServerContext, session_id: u64) -> SessionState {
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
    // parity: plan:MN-13, feat:mining.moon-extraction, feat:mining.moon-fracture, feat:mining.moon-resources, rpc:moonExtractions.CancelExtraction, rpc:moonExtractions.FractureChunkWithStructure, rpc:moonExtractions.GetExtractionAndEventIDFromStructureID, rpc:moonExtractions.GetExtractionsForCorp, rpc:moonExtractions.GetMoonResourcesForStructure, rpc:moonExtractions.StartNewExtraction
    eve_testkit::attest(&["plan:MN-13", "feat:mining.moon-extraction", "feat:mining.moon-fracture", "feat:mining.moon-resources"]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = create_auth_session(&ctx, 1301).await;

    let structure_id = 1_040_000_000_131_i64;
    let corp_id = 1_000_170_i64; // Republic Military School (character's corp)
    let system_id = 30_000_142_u32; // Jita

    // Insert Upwell structure configured for moon drilling
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            structure_id,
            make_structure(structure_id, 35835, corp_id, system_id, "Jita IV Moon Drill", [100_000.0, 50_000.0, -200_000.0], 500),
        );
    });

    // 1. Query moon resources for structure
    let res_ores = ctx
        .dispatch_rpc(
            &mut sess,
            "moonExtractions",
            None,
            "GetMoonResourcesForStructure",
            &[PyRep::Integer(structure_id)],
            &kw,
            None,
        )
        .await
        .expect("GetMoonResourcesForStructure RPC");

    let PyRep::List(ore_list) = res_ores else {
        panic!("Expected List of moon resources");
    };
    assert_eq!(ore_list.len(), 3, "Expected 3 moon ore types in composition");

    // 2. Start new extraction (verify < 6d duration is rejected, then start 6d = 518,400s extraction)
    let short_err = ctx
        .dispatch_rpc(
            &mut sess,
            "moonExtractions",
            None,
            "StartNewExtraction",
            &[PyRep::Integer(structure_id), PyRep::Integer(86_400)],
            &kw,
            None,
        )
        .await
        .expect_err("Duration < 6d must be rejected");
    assert_eq!(short_err.code, "InvalidExtractionDuration");

    let now = ctx.now_unix();
    let start_res = ctx
        .dispatch_rpc(
            &mut sess,
            "moonExtractions",
            None,
            "StartNewExtraction",
            &[
                PyRep::Integer(structure_id),
                PyRep::Integer((now + 518_400) as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect("StartNewExtraction RPC");

    let PyRep::KeyVal(start_kv) = start_res else {
        panic!("Expected KeyVal from StartNewExtraction");
    };
    assert_eq!(start_kv["status"].as_str(), Some("Started"));
    assert_eq!(start_kv["structureID"].as_i64(), Some(structure_id));

    // Starting a second extraction while one is already active must fail
    let active_err = ctx
        .dispatch_rpc(
            &mut sess,
            "moonExtractions",
            None,
            "StartNewExtraction",
            &[PyRep::Integer(structure_id)],
            &kw,
            None,
        )
        .await
        .expect_err("StartNewExtraction while Started must fail");
    assert_eq!(active_err.code, "ExtractionAlreadyActive");

    // 3. Query active extraction details
    let ext_details = ctx
        .dispatch_rpc(
            &mut sess,
            "moonExtractions",
            None,
            "GetExtractionAndEventIDFromStructureID",
            &[PyRep::Integer(structure_id)],
            &kw,
            None,
        )
        .await
        .expect("GetExtractionAndEventIDFromStructureID RPC");

    let PyRep::KeyVal(ext_kv) = ext_details else {
        panic!("Expected KeyVal from GetExtractionAndEventIDFromStructureID");
    };
    assert_eq!(ext_kv["status"].as_str(), Some("Started"));

    // 4. Query corp extractions list
    let corp_exts = ctx
        .dispatch_rpc(
            &mut sess,
            "moonExtractions",
            None,
            "GetExtractionsForCorp",
            &[PyRep::Integer(corp_id)],
            &kw,
            None,
        )
        .await
        .expect("GetExtractionsForCorp RPC");

    let PyRep::List(corp_list) = corp_exts else {
        panic!("Expected List from GetExtractionsForCorp");
    };
    assert!(!corp_list.is_empty(), "Corp extractions should contain active extraction");

    // 5. Test CancelExtraction
    let cancel_res = ctx
        .dispatch_rpc(
            &mut sess,
            "moonExtractions",
            None,
            "CancelExtraction",
            &[PyRep::Integer(structure_id)],
            &kw,
            None,
        )
        .await
        .expect("CancelExtraction RPC");
    assert_eq!(cancel_res, PyRep::Bool(true));

    // Verify status updated to Cancelled
    let status = ctx.db.with_snapshot(|snap| {
        snap.moon_extractions
            .get(&structure_id)
            .map(|e| e.status.clone())
            .unwrap()
    });
    assert_eq!(status, "Cancelled");

    // 6. Start extraction again and test FractureChunkWithStructure
    ctx.dispatch_rpc(
        &mut sess,
        "moonExtractions",
        None,
        "StartNewExtraction",
        &[PyRep::Integer(structure_id)],
        &kw,
        None,
    )
    .await
    .expect("Restart StartNewExtraction");

    // Fracturing before chunk arrival must be rejected
    let early_err = ctx
        .dispatch_rpc(
            &mut sess,
            "moonExtractions",
            None,
            "FractureChunkWithStructure",
            &[PyRep::Integer(structure_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Fracture before chunk arrival must fail");
    assert_eq!(early_err.code, "ChunkNotReadyToFracture");

    // Advance virtual clock to chunk arrival (6d = 518,400s)
    ctx.advance_secs(518_400);

    let frac_res = ctx
        .dispatch_rpc(
            &mut sess,
            "moonExtractions",
            None,
            "FractureChunkWithStructure",
            &[PyRep::Integer(structure_id)],
            &kw,
            None,
        )
        .await
        .expect("FractureChunkWithStructure RPC");

    // Repeating FractureChunkWithStructure on an already-fractured extraction must fail
    let dup_err = ctx
        .dispatch_rpc(
            &mut sess,
            "moonExtractions",
            None,
            "FractureChunkWithStructure",
            &[PyRep::Integer(structure_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Repeated FractureChunkWithStructure must fail");
    assert_eq!(dup_err.code, "ExtractionAlreadyFractured");

    let PyRep::KeyVal(frac_kv) = frac_res else {
        panic!("Expected KeyVal from FractureChunkWithStructure");
    };
    assert_eq!(frac_kv["fractured"], PyRep::Bool(true));
    assert_eq!(frac_kv["status"].as_str(), Some("Fractured"));
    assert!(
        matches!(frac_kv["asteroidCount"].as_i64(), Some(3 | 6)),
        "Fractured moon chunk must spawn 6 mixed basic/+15% asteroids (or 3 on jackpot)"
    );
    let PyRep::List(ast_id_list) = &frac_kv["asteroidIDs"] else {
        panic!("Expected asteroidIDs list");
    };
    let spawned_ast_id = ast_id_list[0].as_i64().unwrap();

    // Verify spawned asteroid entities exist in system
    let handle = ctx.systems.get_or_spawn(system_id);
    let ast_state = handle.get_asteroid_state(spawned_ast_id).await.unwrap();
    assert!(ast_state.is_some(), "Spawned moon asteroid entity must exist in solar system");
    assert!(ast_state.unwrap().remaining_units >= 500);
}

// ============================================================================
// MN-14: Auto Moon Mining Service (Metenox Moon Drill)
// ============================================================================
#[tokio::test]
async fn test_mn14_auto_moon_mining_service() {
    // parity: feat:mining.metenox, rpc:autoMoonMining.RequestMiningCycleOutput, rpc:autoMoonMining.RequestMiningDetails
    eve_testkit::attest(&["feat:mining.metenox"]);
    let ctx = ServerContext::new();
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
        // Seed actual Magmatic Gas (type 81143) item in the Low-Sec Metenox structure inventory
        snap.items.insert(
            gas_item_id,
            make_item(gas_item_id, 81143, corp_id, structure_id, InventoryFlag::Hangar, 50, false),
        );
        // Seed a third-party Magmatic Gas stack (owner_id != corp_id) in the structure hangar;
        // it must NOT be counted toward the Metenox drill's reagent inventory
        snap.items.insert(
            gas_item_id + 1,
            make_item(gas_item_id + 1, 81143, 999_999_999, structure_id, InventoryFlag::Hangar, 500, false),
        );

        // Seed a stale record with inflated `magmatic_gas_remaining: 999` to verify authoritative inventory sync
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
        .dispatch_rpc(
            &mut sess,
            "autoMoonMining",
            None,
            "RequestMiningDetails",
            &[PyRep::Integer(highsec_structure_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Metenox Moon Drill in High-Sec must be rejected");
    assert_eq!(hs_err.code, "InvalidSystemSecurity");

    // 1. RequestMiningDetails in Low-Sec (`Asabona`)
    let details_res = ctx
        .dispatch_rpc(
            &mut sess,
            "autoMoonMining",
            None,
            "RequestMiningDetails",
            &[PyRep::Integer(structure_id)],
            &kw,
            None,
        )
        .await
        .expect("RequestMiningDetails RPC");

    let PyRep::KeyVal(det_kv) = details_res else {
        panic!("Expected KeyVal from RequestMiningDetails");
    };
    assert_eq!(det_kv["status"].as_str(), Some("Active"));
    // Authoritative inventory read must return actual 100 fuel and 50 corp-owned gas (ignoring third-party 500 gas),
    // and RequestMiningDetails must NOT mutate the underlying `snap.auto_moon_miners` state on read
    assert_eq!(det_kv["fuelRemaining"].as_i64(), Some(100));
    assert_eq!(det_kv["magmaticGasRemaining"].as_i64(), Some(50));
    let unmutated_fuel = ctx.db.with_snapshot(|snap| {
        snap.auto_moon_miners.get(&structure_id).map(|m| m.fuel_units_remaining)
    });
    assert_eq!(
        unmutated_fuel,
        Some(999),
        "RequestMiningDetails must be read-only and not mutate persisted AutoMoonMinerRecord before cycle tick"
    );
    assert_eq!(det_kv["efficiency"].as_f64(), Some(0.40));
    let hourly_yield = det_kv["hourlyYield"].as_i64().unwrap();
    assert!(hourly_yield > 0, "Hourly moon material yield must be positive");
    let primary_mat_tid = det_kv["primaryMaterialTypeID"].as_i64().unwrap() as u32;
    assert_eq!(
        ctx.sde.types.get(&primary_mat_tid).map(|t| t.group_id),
        Some(427),
        "Metenox primary output type {primary_mat_tid} must be a Moon Material (group 427)"
    );

    // Requesting output before any cycle has elapsed must fail with CycleNotComplete
    let early_cycle_err = ctx
        .dispatch_rpc(
            &mut sess,
            "autoMoonMining",
            None,
            "RequestMiningCycleOutput",
            &[PyRep::Integer(structure_id), PyRep::Integer(2)],
            &kw,
            None,
        )
        .await
        .expect_err("RequestMiningCycleOutput before cycle completion must fail");
    assert_eq!(early_cycle_err.code, "CycleNotComplete");

    // Advance virtual clock by 2 hours (7,200s) for 2 hourly cycles
    ctx.advance_secs(7_200);

    // 2. RequestMiningCycleOutput (2 cycles)
    let cycle_res = ctx
        .dispatch_rpc(
            &mut sess,
            "autoMoonMining",
            None,
            "RequestMiningCycleOutput",
            &[PyRep::Integer(structure_id), PyRep::Integer(2)],
            &kw,
            None,
        )
        .await
        .expect("RequestMiningCycleOutput RPC");

    let PyRep::KeyVal(cyc_kv) = cycle_res else {
        panic!("Expected KeyVal from RequestMiningCycleOutput");
    };
    let units_produced = cyc_kv["unitsProduced"].as_i64().unwrap();
    assert_eq!(cyc_kv["cyclesProcessed"].as_i64(), Some(2));
    assert!(units_produced > 0, "2 Metenox cycles must produce moon materials");
    assert_eq!(cyc_kv["fuelConsumed"].as_i64(), Some(10));
    assert_eq!(cyc_kv["magmaticGasConsumed"].as_i64(), Some(20));
    assert_eq!(cyc_kv["fuelRemaining"].as_i64(), Some(90));

    // Verify actual Magmatic Gas item (type 81143) was deducted from 50 -> 30
    let rem_gas_qty = ctx.db.with_snapshot(|snap| {
        snap.items
            .get(&gas_item_id)
            .map(|it| it.quantity)
            .unwrap_or(0)
    });
    assert_eq!(rem_gas_qty, 30, "Magmatic Gas item quantity must be deducted from 50 to 30");

    // 3. Verify Moon Materials (group 427) deposited into structure hangar
    let moon_mats_in_hangar = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|it| {
                it.location_id == structure_id
                    && it.flag == InventoryFlag::Hangar
                    && ctx.sde.types.get(&it.type_id).is_some_and(|t| t.group_id == 427)
            })
            .map(|it| it.quantity as i64)
            .sum::<i64>()
    });
    assert_eq!(
        moon_mats_in_hangar, units_produced,
        "Structure hangar must receive exact harvested Moon Materials (group 427)"
    );

    // 4. Verify automatic entries logged in observer mining ledger
    let ledger_entries = ctx.db.with_snapshot(|snap| {
        snap.observer_mining_ledger
            .get(&structure_id)
            .cloned()
            .unwrap_or_default()
    });
    assert!(!ledger_entries.is_empty(), "Observer mining ledger must record automated extraction");
    let ledger_sum: i64 = ledger_entries.iter().map(|e| e.quantity).sum();
    assert_eq!(ledger_sum, units_produced);
}

// ============================================================================
// MN-15: Corp Mining Ledger & Observer Tracking
// ============================================================================
#[tokio::test]
async fn test_mn15_corp_mining_ledger_service_and_observer_tracking() {
    // parity: plan:MN-16, feat:mining.corp-ledger, rpc:corpMiningLedger.GetObserverLedger, rpc:corpMiningLedger.GetObserversWithMiningEvents
    eve_testkit::attest(&["plan:MN-16", "feat:mining.corp-ledger"]);
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

    // Record mining ledger entries for character in the system
    let _ = ctx.db.record_mining_ledger_entry(90_000_001, system_id, 1230, 2_500, now);
    let _ = ctx.db.record_mining_ledger_entry(90_000_001, system_id, 45492, 1_200, now);

    // 0. Unprivileged member (corp_role == 0) must be rejected with CrpAccessDenied
    sess.corp_role = 0;
    let role_err = ctx
        .dispatch_rpc(
            &mut sess,
            "corpMiningLedger",
            None,
            "GetObserversWithMiningEvents",
            &[],
            &kw,
            None,
        )
        .await
        .expect_err("GetObserversWithMiningEvents without Accountant/StationManager/Director role must fail");
    assert_eq!(role_err.code, "CrpAccessDenied");

    // Grant Accountant role
    sess.corp_role = eve_db::corp_roles::ROLE_ACCOUNTANT;

    // 1. GetObserversWithMiningEvents
    let obs_list_res = ctx
        .dispatch_rpc(
            &mut sess,
            "corpMiningLedger",
            None,
            "GetObserversWithMiningEvents",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetObserversWithMiningEvents RPC");

    assert!(obs_list_res.is_crowset(), "GetObserversWithMiningEvents must return CRowset");

    // 2. GetObserverLedger
    let ledger_res = ctx
        .dispatch_rpc(
            &mut sess,
            "corpMiningLedger",
            None,
            "GetObserverLedger",
            &[PyRep::Integer(observer_id)],
            &kw,
            None,
        )
        .await
        .expect("GetObserverLedger RPC");

    assert!(ledger_res.is_crowset(), "GetObserverLedger must return CRowset");

    // Verify observer mining ledger entries in database snapshot
    let ledger = ctx.db.get_observer_mining_ledger(observer_id);
    assert_eq!(ledger.len(), 2, "Observer should track both mining events");
    assert_eq!(ledger[0].type_id, 1230);
    assert_eq!(ledger[0].quantity, 2_500);
    assert_eq!(ledger[1].type_id, 45492);
    assert_eq!(ledger[1].quantity, 1_200);
}

// ============================================================================
// MN-16: In-Space & Structure Compression & Decompression
// ============================================================================
#[tokio::test]
async fn test_mn16_in_space_and_structure_compression_and_decompression() {
    // parity: feat:mining.compression-space, feat:mining.compression-structure, feat:mining.gas-decompression, rpc:dogmaIM.Activate, rpc:inSpaceCompressionMgr.CompressItemInSpace, rpc:stationSvc.Undock, rpc:structureCompressionMgr.CompressItemInStructure, rpc:structureCompressionMgr.DecompressGasInStructure, rpc:structureCompressionMgr.GetMyGasDecompressionEfficiency
    eve_testkit::attest(&["feat:mining.compression-space", "feat:mining.compression-structure", "feat:mining.gas-decompression"]);
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

    // ------------------------------------------------------------------------
    // A. In-Space Compression
    // ------------------------------------------------------------------------
    // Undock character to space
    let undock = ctx
        .dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock RPC");
    let PyRep::KeyVal(undock_kv) = undock else { panic!("Expected undock kv") };
    let ship_id = undock_kv["shipID"].as_i64().unwrap();

    // Place 1,045 units of Veldspar (type 1230) in ship cargo on an Orca (28606) with active Compressor (62625)
    let item_in_space = 710_000_001_i64;
    let comp_mod_id = 710_000_002_i64;
    let core_mod_id = 710_000_099_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship_it) = snap.items.get_mut(&ship_id) {
            ship_it.type_id = 28606; // Orca
        }
        // Unfit the starter ship's modules so the compressor and core get free high slots
        snap.items.retain(|_, item| item.location_id != ship_id);
        // Train the SDE skill prerequisites (requiredSkill1..3 / level) of the compressor and the core
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
        snap.items.insert(
            comp_mod_id,
            InventoryItemRecord {
                item_id: comp_mod_id,
                type_id: 62625, // Large Asteroid Ore Compressor I
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            item_in_space,
            InventoryItemRecord {
                item_id: item_in_space,
                type_id: 1230, // Veldspar
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1_045,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    // Item in general Cargo hold must be rejected (`NotInMiningHold`)
    let cargo_err = ctx
        .dispatch_rpc(
            &mut sess,
            "inSpaceCompressionMgr",
            None,
            "CompressItemInSpace",
            &[PyRep::Integer(item_in_space)],
            &kw,
            None,
        )
        .await
        .expect_err("CompressItemInSpace from general Cargo must fail");
    assert_eq!(cargo_err.code, "NotInMiningHold");

    // Move item into the ship's Mining Hold (`SpecializedOreHold`)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&item_in_space) {
            it.flag = InventoryFlag::SpecializedOreHold;
        }
    });

    ctx.ship_fittings.write().unwrap().remove(&ship_id);

    // A fitted but inactive compressor can't compress
    let inactive_err = ctx
        .dispatch_rpc(
            &mut sess,
            "inSpaceCompressionMgr",
            None,
            "CompressItemInSpace",
            &[PyRep::Integer(item_in_space)],
            &kw,
            None,
        )
        .await
        .expect_err("CompressItemInSpace with an inactive compressor must fail");
    assert_eq!(inactive_err.code, "NoActiveCompressorInRange");

    // Without an active Industrial Core (`58945` Large Industrial Core I, group 515),
    // compressor module `62625` (`3265` activationRequiresActiveIndustrialCore = 1.0) can't be activated
    let no_core_act_err = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(comp_mod_id), PyRep::String("compressor".into())],
            &kw,
            None,
        )
        .await
        .expect_err("Activating the compressor without an active Industrial Core must fail");
    assert_eq!(no_core_act_err.code, "IndustrialCoreNotActive");
    let no_core_err = ctx
        .dispatch_rpc(
            &mut sess,
            "inSpaceCompressionMgr",
            None,
            "CompressItemInSpace",
            &[PyRep::Integer(item_in_space)],
            &kw,
            None,
        )
        .await
        .expect_err("CompressItemInSpace without active Industrial Core must fail");
    assert_eq!(no_core_err.code, "NoActiveCompressorInRange");

    // Fit Large Industrial Core I (type 58945, group 515, canFitShipType1 = 28606) on the Orca, then
    // activate the core and the compressor through dogmaIM.Activate
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            core_mod_id,
            InventoryItemRecord {
                item_id: core_mod_id,
                type_id: 58945, // Large Industrial Core I
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot1,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(core_mod_id), PyRep::String("industrialCore".into())],
        &kw,
        None,
    )
    .await
    .expect("Activate Large Industrial Core I");
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(comp_mod_id), PyRep::String("compressor".into())],
        &kw,
        None,
    )
    .await
    .expect("Activate Large Asteroid Ore Compressor I");
    assert!(ctx.db.with_snapshot(|snap| {
        snap.module_active_states.contains(&core_mod_id)
            && snap.module_active_states.contains(&comp_mod_id)
    }));

    let comp_space_res = ctx
        .dispatch_rpc(
            &mut sess,
            "inSpaceCompressionMgr",
            None,
            "CompressItemInSpace",
            &[PyRep::Integer(item_in_space)],
            &kw,
            None,
        )
        .await
        .expect("CompressItemInSpace RPC");

    let PyRep::KeyVal(space_kv) = comp_space_res else {
        panic!("Expected KeyVal from CompressItemInSpace");
    };
    let compressed_tid = space_kv["compressedTypeID"].as_i64().unwrap() as u32;
    let compressed_qty = space_kv["quantity"].as_i64().unwrap();
    let rem_qty = space_kv["remainderUnits"].as_i64().unwrap();

    let expected_comp_tid = *ctx.sde.compressible_types.get(&1230).unwrap();
    assert_eq!(compressed_tid, expected_comp_tid, "Compressed Veldspar type ID matches SDE");
    assert_eq!(compressed_qty, 1_045, "V24 1:1 compression converts all 1,045 units");
    assert_eq!(rem_qty, 0, "V24 1:1 compression leaves 0 remainder units");

    // Verify compressed item in mining hold
    let hold_items = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|it| it.location_id == ship_id && it.owner_id == char_id)
            .map(|it| (it.type_id, it.quantity))
            .collect::<Vec<_>>()
    });
    assert!(hold_items.contains(&(expected_comp_tid, 1_045)), "Mining hold must contain 1,045 compressed units");

    // ------------------------------------------------------------------------
    // B. Structure Compression
    // ------------------------------------------------------------------------
    // Dock into structure (updating character in DB so dispatch_rpc syncs structure_id)
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
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_in_struct,
            InventoryItemRecord {
                item_id: item_in_struct,
                type_id: 1228, // Scordite
                owner_id: char_id,
                location_id: structure_id,
                flag: InventoryFlag::Hangar,
                quantity: 500,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            moon_ore_in_struct,
            InventoryItemRecord {
                item_id: moon_ore_in_struct,
                type_id: 45492, // Bitumens (Moon Ore, group 1884)
                owner_id: char_id,
                location_id: structure_id,
                flag: InventoryFlag::Hangar,
                quantity: 500,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    // An unfitted Standup Reprocessing Facility I (`35892`, group 1322) sitting in the structure hangar
    // (`InventoryFlag::Hangar`, flag_id == 4) does NOT count as a fitted online service module
    let reproc_svc_mod_id = 710_000_009_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            reproc_svc_mod_id,
            InventoryItemRecord {
                item_id: reproc_svc_mod_id,
                type_id: 35892, // Standup Reprocessing Facility I
                owner_id: corp_id,
                location_id: structure_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let unfitted_svc_err = ctx
        .dispatch_rpc(
            &mut sess,
            "structureCompressionMgr",
            None,
            "CompressItemInStructure",
            &[PyRep::Integer(structure_id), PyRep::Integer(item_in_struct)],
            &kw,
            None,
        )
        .await
        .expect_err("Unfitted reprocessing module in hangar must not enable structure compression");
    assert_eq!(unfitted_svc_err.code, "ReprocessingServiceOffline");

    // Fit the Standup Reprocessing Facility I into `InventoryFlag::ServiceSlot0` (`flag_id == 164`)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(m) = snap.items.get_mut(&reproc_svc_mod_id) {
            m.flag = InventoryFlag::ServiceSlot0;
        }
    });

    // Moon Ore cannot be compressed in a structure (`StructureCannotCompressType`)
    let moon_comp_err = ctx
        .dispatch_rpc(
            &mut sess,
            "structureCompressionMgr",
            None,
            "CompressItemInStructure",
            &[PyRep::Integer(structure_id), PyRep::Integer(moon_ore_in_struct)],
            &kw,
            None,
        )
        .await
        .expect_err("Moon Ore compression in structure must be rejected");
    assert_eq!(moon_comp_err.code, "StructureCannotCompressType");

    let struct_comp_res = ctx
        .dispatch_rpc(
            &mut sess,
            "structureCompressionMgr",
            None,
            "CompressItemInStructure",
            &[PyRep::Integer(structure_id), PyRep::Integer(item_in_struct)],
            &kw,
            None,
        )
        .await
        .expect("CompressItemInStructure RPC");

    let PyRep::KeyVal(str_kv) = struct_comp_res else {
        panic!("Expected KeyVal from CompressItemInStructure");
    };
    assert_eq!(str_kv["quantity"].as_i64(), Some(500)); // V24 1:1 compression

    // ------------------------------------------------------------------------
    // C. Structure Gas Decompression
    // ------------------------------------------------------------------------
    // Query decompression efficiency untrained on Athanor (0.80 base + 0.04 Athanor = 0.84)
    let eff_untrained = ctx
        .dispatch_rpc(
            &mut sess,
            "structureCompressionMgr",
            None,
            "GetMyGasDecompressionEfficiency",
            &[PyRep::Integer(structure_id)],
            &kw,
            None,
        )
        .await
        .expect("GetMyGasDecompressionEfficiency untrained RPC");
    assert_eq!(eff_untrained.as_f64(), Some(0.84));

    // Train Gas Decompression Efficiency (type 62452) to level 5 (+5% -> 0.89 on Athanor)
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            62452,
            eve_db::TrainedSkillRecord {
                type_id: 62452,
                level: 5,
                skill_points: 512_000.0,
            },
        );
    });

    let eff_res = ctx
        .dispatch_rpc(
            &mut sess,
            "structureCompressionMgr",
            None,
            "GetMyGasDecompressionEfficiency",
            &[PyRep::Integer(structure_id)],
            &kw,
            None,
        )
        .await
        .expect("GetMyGasDecompressionEfficiency RPC");
    assert_eq!(eff_res.as_f64(), Some(0.89));

    // Compressed ore (non-gas) must be rejected by DecompressGasInStructure (`NotCompressedGas`)
    let non_gas_err = ctx
        .dispatch_rpc(
            &mut sess,
            "structureCompressionMgr",
            None,
            "DecompressGasInStructure",
            &[PyRep::Integer(structure_id), PyRep::Integer(item_in_struct)],
            &kw,
            None,
        )
        .await
        .expect_err("Decompressing non-gas compressed ore must fail");
    assert_eq!(non_gas_err.code, "NotCompressedGas");

    // Place 100 units of Compressed Fullerite-C50 (type 62399, raw type 30370 in group 711) in hangar,
    // plus a remote stack at another station (`60_003_760`) and a courier-escrowed stack (`InventoryFlag::Locked`)
    let comp_gas_item = 710_000_003_i64;
    let remote_gas_item = 710_000_006_i64;
    let courier_locked_gas_item = 710_000_007_i64;
    let other_structure_id = 1_040_000_000_169_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            other_structure_id,
            make_structure(other_structure_id, 35836, corp_id, system_id, "Remote Tatara", [50_000.0, 0.0, 0.0], 500),
        );
        snap.items.insert(
            comp_gas_item,
            make_item(comp_gas_item, 62399, char_id, structure_id, InventoryFlag::Hangar, 100, false),
        );
        snap.items.insert(
            remote_gas_item,
            make_item(remote_gas_item, 62399, char_id, 60_003_760, InventoryFlag::Hangar, 100, false),
        );
        snap.items.insert(
            courier_locked_gas_item,
            make_item(courier_locked_gas_item, 62399, char_id, 990_000_001, InventoryFlag::Locked, 100, false),
        );
    });

    // Spoofing another structure ID (Tatara 35836) while docked at Athanor must fail (`InvalidStructure`)
    let spoof_struct_err = ctx
        .dispatch_rpc(
            &mut sess,
            "structureCompressionMgr",
            None,
            "DecompressGasInStructure",
            &[PyRep::Integer(other_structure_id), PyRep::Integer(comp_gas_item)],
            &kw,
            None,
        )
        .await
        .expect_err("DecompressGasInStructure with spoofed structure_id must fail");
    assert_eq!(spoof_struct_err.code, "InvalidStructure");

    // Decompressing an owned compressed gas item at a remote location must fail (`ItemNotInStructureHangar`)
    let remote_err = ctx
        .dispatch_rpc(
            &mut sess,
            "structureCompressionMgr",
            None,
            "DecompressGasInStructure",
            &[PyRep::Integer(structure_id), PyRep::Integer(remote_gas_item)],
            &kw,
            None,
        )
        .await
        .expect_err("DecompressGasInStructure on remote item must fail");
    assert_eq!(remote_err.code, "ItemNotInStructureHangar");

    // Decompressing an owned compressed gas item locked in courier escrow must fail (`ItemLocked`)
    let locked_err = ctx
        .dispatch_rpc(
            &mut sess,
            "structureCompressionMgr",
            None,
            "DecompressGasInStructure",
            &[PyRep::Integer(structure_id), PyRep::Integer(courier_locked_gas_item)],
            &kw,
            None,
        )
        .await
        .expect_err("DecompressGasInStructure on courier-locked item must fail");
    assert_eq!(locked_err.code, "ItemLocked");
    assert_eq!(
        ctx.db.with_snapshot(|snap| snap.items.get(&courier_locked_gas_item).map(|i| i.quantity)),
        Some(100),
        "Courier-locked item must remain untouched"
    );

    let decomp_res = ctx
        .dispatch_rpc(
            &mut sess,
            "structureCompressionMgr",
            None,
            "DecompressGasInStructure",
            &[PyRep::Integer(structure_id), PyRep::Integer(comp_gas_item)],
            &kw,
            None,
        )
        .await
        .expect("DecompressGasInStructure RPC");

    let PyRep::KeyVal(dec_kv) = decomp_res else {
        panic!("Expected KeyVal from DecompressGasInStructure");
    };
    // 100 compressed gas * 0.89 efficiency = 89 decompressed raw Fullerite-C50 units
    assert_eq!(dec_kv["decompressedTypeID"].as_i64(), Some(30370));
    assert_eq!(dec_kv["decompressedQuantity"].as_i64(), Some(89));
    assert_eq!(dec_kv["efficiency"].as_f64(), Some(0.89));

    // Verify decompressed gas item in hangar
    let hangar_raw = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .find(|it| {
                it.location_id == structure_id
                    && it.owner_id == char_id
                    && it.type_id == 30370
                    && it.quantity == 89
            })
            .cloned()
    });
    assert!(hangar_raw.is_some(), "Hangar must contain decompressed raw Fullerite-C50 with 89 units");
}

// ============================================================================
// MN-17: Living World NPC Mining Ops & Market Supply Hauling
// ============================================================================
#[tokio::test]
async fn test_mn17_living_world_npc_mining_ops_and_market_hauling() {
    // parity: feat:mining.npc-mining-ops, feat:npc.mining-op-npc
    eve_testkit::attest(&["feat:mining.npc-mining-ops", "feat:npc.mining-op-npc"]);
    let ctx = ServerContext::new();
    let director = LivingWorldDirector::new();

    let convoy_id = 800_000_099_i64;
    let system_id = 30_000_144_u32; // Perimeter
    let dest_station_id = 60_003_766_i64; // Perimeter II - Moon 1 - Caldari Navy
    let ore_type_id = 1230_u32; // Veldspar

    // Spawn an asteroid in Jita for the NPC fleet to mine
    let handle = ctx.systems.get_or_spawn(system_id);
    let ast_id = 750_000_171_i64;
    handle
        .spawn_asteroid(
            AsteroidState {
                asteroid_id: ast_id,
                belt_id: 40_000_001,
                type_id: ore_type_id,
                initial_units: 10_000,
                remaining_units: 10_000,
                unit_volume: 0.1,
                depleted: false,
                respawn_at_tick: None,
            },
            Ball {
                id: ast_id,
                type_id: ore_type_id,
                position: DVec3::new(5_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 0.0,
                mass: 1.0e8,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
        )
        .await
        .unwrap();

    // Register MiningOp convoy near hauling threshold (4,800 / 5,000 units)
    director.register_convoy(LivingWorldConvoy {
        convoy_id,
        archetype: NpcArchetype::MiningOp,
        ship_type_id: 32880, // Venture
        origin_system_id: system_id,
        current_system_id: system_id,
        destination_system_id: system_id,
        destination_station_id: dest_station_id,
        cargo_type_id: ore_type_id,
        cargo_quantity: 4_800,
        lod_level: 1,
        destroyed: false,
    });

    // Advance NPC mining op step
    let step_result = director
        .step_npc_mining_op(convoy_id, &ctx.systems, &ctx.db, &ctx.sde)
        .await
        .expect("step_npc_mining_op succeeded");

    assert_eq!(step_result.convoy_id, convoy_id);
    assert!(step_result.asteroid_id > 0, "Must target and mine a real asteroid");
    assert_eq!(step_result.ore_type_id, ore_type_id);
    assert_eq!(step_result.mined_units, 250, "Standard cycle mined 250 units");
    assert!(step_result.remaining_asteroid_units > 0);
    // 4,800 + 250 = 5,050 >= 5,000 cargo threshold -> hauled to station!
    assert!(step_result.hauled_to_station, "Must trigger hauling when cargo >= 5,000");
    assert_eq!(step_result.convoy_cargo_units, 0, "Cargo reset after hauling");

    // Verify regional destination station market order was replenished
    let market_order = ctx.db.with_snapshot(|snap| {
        snap.market_orders
            .values()
            .find(|o| !o.is_bid && o.station_id == dest_station_id && o.type_id == ore_type_id)
            .cloned()
    });
    assert!(market_order.is_some(), "NPC sell order must exist at destination station");
    assert!(
        market_order.unwrap().vol_remaining >= 5_000,
        "Market sell order volume must be replenished by hauled cargo"
    );
}

// ============================================================================
// MN-18: Mining Missions & Career Goals from Mining Ledger
// ============================================================================
#[tokio::test]
async fn test_mn18_mining_missions_and_career_goals_from_ledger() {
    // parity: feat:mining.mining-missions, feat:mining.mining-goals, rpc:agentMgr.DoAction, rpc:agentMgr.GetAgents, rpc:agentMgr.GetMyJournalDetails, rpc:agentMgr.MachoBindObject, rpc:seasonManager.get_character_progress_by_character_id
    eve_testkit::attest(&["feat:mining.mining-missions", "feat:mining.mining-goals"]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = create_auth_session(&ctx, 1801).await;

    let char_id = 90_000_001_i64;
    let now = ctx.now_unix();

    // ------------------------------------------------------------------------
    // A. Mining Mission with Pocket Asteroid Belt Spawning, Mining & Delivery
    // ------------------------------------------------------------------------
    // 1. Find a Level 1 Mining (divisionID = 23) agent via agentMgr.GetAgents
    let agents_rep = ctx
        .dispatch_rpc(&mut sess, "agentMgr", None, "GetAgents", &[PyRep::Integer(1), PyRep::Integer(23)], &kw, None)
        .await
        .expect("agentMgr.GetAgents for L1 Mining");
    let PyRep::Rowset { rows: agent_rows, .. } = agents_rep else {
        panic!("Expected Rowset from agentMgr.GetAgents");
    };
    assert!(!agent_rows.is_empty(), "L1 Mining agents must exist in SDE");
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

    // 2. Bind to the mining agent moniker via MachoBindObject on agentMgr
    let bind_res = ctx
        .dispatch_rpc(&mut sess, "agentMgr", None, "MachoBindObject", &[PyRep::Integer(mining_agent_id), PyRep::None], &kw, None)
        .await
        .expect("MachoBindObject on agentMgr");
    let PyRep::Tuple(bind_tuple) = bind_res else {
        panic!("Expected Tuple from MachoBindObject");
    };
    let agent_oid = bind_tuple[0].as_i64().unwrap() as u64;

    // 3. Request mining mission via bound agentMgr.DoAction(2) (agentDialogueButtonRequestMission)
    let offer_res = ctx
        .dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(2)], &kw, None)
        .await
        .expect("agentMgr.DoAction(RequestMission)");
    let mission_id = offer_res.dict_get("missionID").and_then(|v| v.as_i64()).expect("missionID from DoAction offer") as u32;
    assert_eq!(offer_res.dict_get("state").and_then(|v| v.as_str()), Some("Offered"));

    // 4. Accept mission via bound agentMgr.DoAction(3) (agentDialogueButtonAccept) -> spawns pocket asteroid
    let accept_res = ctx
        .dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(3)], &kw, None)
        .await
        .expect("agentMgr.DoAction(Accept)");
    assert_eq!(accept_res.dict_get("state").and_then(|v| v.as_str()), Some("Accepted"));
    let bookmark_id = accept_res.dict_get("bookmarkID").and_then(|v| v.as_i64()).expect("bookmarkID on accept");

    // Verify mission journal via agentMgr.GetMyJournalDetails
    let journal_res = ctx
        .dispatch_rpc(&mut sess, "agentMgr", None, "GetMyJournalDetails", &[], &kw, None)
        .await
        .expect("agentMgr.GetMyJournalDetails");
    let PyRep::Tuple(journal_tuple) = journal_res else {
        panic!("Expected (missions, research) Tuple from GetMyJournalDetails");
    };
    let PyRep::List(journal_missions) = &journal_tuple[0] else {
        panic!("Expected List of active missions");
    };
    assert!(
        journal_missions
            .iter()
            .any(|m| m.dict_get("missionID").and_then(|v| v.as_i64()) == Some(mission_id as i64)
                && m.dict_get("missionState").and_then(|v| v.as_i64()) == Some(2)),
        "Accepted mining mission must appear in GetMyJournalDetails with missionState=2"
    );

    // Verify pocket asteroid was spawned in the system
    let pocket_ast_id = 910_000_000 + (mission_id as i64) * 1000 + 1;
    let handle = ctx.systems.get_or_spawn(system_id);
    let pocket_ast = handle.get_asteroid_state(pocket_ast_id).await.unwrap();
    assert!(pocket_ast.is_some(), "Pocket asteroid must be spawned in mission room");
    assert_eq!(pocket_ast.unwrap().type_id, 1230); // Veldspar
    assert!(ctx.db.with_snapshot(|snap| snap.bookmarks.contains_key(&bookmark_id)), "Mission pocket bookmark must exist");

    // Clear any starter Veldspar from character inventory to test incomplete delivery
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, it| !(it.owner_id == char_id && it.type_id == 1230));
    });

    // Try completing mission via DoAction(6) before mining/delivering required ore -> must fail
    let incomplete_res = ctx
        .dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(6)], &kw, None)
        .await;
    assert!(incomplete_res.is_err(), "Completing mining mission without required ore must fail");
    assert_eq!(incomplete_res.err().unwrap().code, "MissionIncomplete");

    // Provide required ore (500 units of Veldspar) into character station hangar
    let ore_item_id = 710_000_018_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ore_item_id,
            make_item(ore_item_id, 1230, char_id, station_id, InventoryFlag::Hangar, 500, false),
        );
    });

    // Complete mission via DoAction(6) (agentDialogueButtonComplete) -> succeeds, consumes ore, pays rewards
    let complete_res = ctx
        .dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(6)], &kw, None)
        .await
        .expect("agentMgr.DoAction(Complete) RPC");
    assert_eq!(complete_res.dict_get("missionID").and_then(|v| v.as_i64()), Some(mission_id as i64));
    let m_state = ctx.db.with_snapshot(|snap| snap.active_missions.get(&(char_id, mission_id)).map(|m| m.state.clone()));
    assert_eq!(m_state.as_deref(), Some("Completed"));

    // Verify ore was consumed from inventory
    let remaining_ore = ctx.db.with_snapshot(|snap| snap.items.get(&ore_item_id).map(|it| it.quantity).unwrap_or(0));
    assert_eq!(remaining_ore, 0, "Mission delivery must consume the 500 units of ore");

    // ------------------------------------------------------------------------
    // B. Career Progression Verified from Mining Ledger
    // ------------------------------------------------------------------------
    let _ = ctx.db.record_mining_ledger_entry(char_id, system_id, 1230, 3_500, now);
    let _ = ctx.db.record_mining_ledger_entry(char_id, system_id, 1228, 1_500, now);

    // Query career goal progress via V24 seasonManager.get_character_progress_by_character_id
    let goal_res = ctx
        .dispatch_rpc(&mut sess, "seasonManager", None, "get_character_progress_by_character_id", &[PyRep::Integer(2)], &kw, None)
        .await
        .expect("seasonManager.get_character_progress_by_character_id RPC");

    let PyRep::KeyVal(goal_kv) = goal_res else {
        panic!("Expected KeyVal from get_character_progress_by_character_id");
    };
    assert_eq!(goal_kv["goalID"].as_i64(), Some(2));
    assert_eq!(
        goal_kv["currentProgress"].as_i64(),
        Some(5_000), // 3,500 + 1,500
        "Progress must match exact sum from mining ledger entries"
    );
    assert_eq!(goal_kv["verifiedFromLedger"], PyRep::Bool(true));
}
