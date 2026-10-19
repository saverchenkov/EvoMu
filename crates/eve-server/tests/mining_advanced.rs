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
            UpwellStructureRecord {
                structure_id,
                type_id: 35835, // Athanor refinery
                owner_corp_id: corp_id,
                solar_system_id: system_id,
                name: "Jita IV Moon Drill".into(),
                position: [100_000.0, 50_000.0, -200_000.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                shield_hp: 50_000.0,
                armor_hp: 50_000.0,
                hull_hp: 50_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
            },
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

    // 2. Start new extraction
    let now = ctx.now_unix();
    let start_res = ctx
        .dispatch_rpc(
            &mut sess,
            "moonExtractions",
            None,
            "StartNewExtraction",
            &[
                PyRep::Integer(structure_id),
                PyRep::Integer(now as i64),
                PyRep::Integer((now + 86_400) as i64),
                PyRep::Integer((now + 172_800) as i64),
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

    let PyRep::KeyVal(frac_kv) = frac_res else {
        panic!("Expected KeyVal from FractureChunkWithStructure");
    };
    assert_eq!(frac_kv["fractured"], PyRep::Bool(true));
    assert_eq!(frac_kv["status"].as_str(), Some("Fractured"));
    assert_eq!(frac_kv["asteroidCount"].as_i64(), Some(3));

    // Verify spawned asteroid entities exist in system
    let handle = ctx.systems.get_or_spawn(system_id);
    let spawned_ast_id = 950_000_000 + structure_id % 100_000 * 100 + 1;
    let ast_state = handle.get_asteroid_state(spawned_ast_id).await.unwrap();
    assert!(ast_state.is_some(), "Spawned moon asteroid entity must exist in solar system");
    assert_eq!(ast_state.unwrap().remaining_units, 5_000);
}

// ============================================================================
// MN-14: Auto Moon Mining Service (Metenox Moon Drill)
// ============================================================================
#[tokio::test]
async fn test_mn14_auto_moon_mining_service() {
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = create_auth_session(&ctx, 1401).await;

    let structure_id = 1_040_000_000_141_i64;
    let corp_id = 1_000_170_i64;
    let system_id = 30_000_142_u32;

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            structure_id,
            UpwellStructureRecord {
                structure_id,
                type_id: 81824, // Metenox Moon Drill
                owner_corp_id: corp_id,
                solar_system_id: system_id,
                name: "Jita Auto Moon Drill".into(),
                position: [200_000.0, 0.0, -100_000.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 1_000,
                shield_hp: 50_000.0,
                armor_hp: 50_000.0,
                hull_hp: 50_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
            },
        );

        snap.auto_moon_miners.insert(
            structure_id,
            AutoMoonMinerRecord {
                structure_id,
                fuel_units_remaining: 100,
                magmatic_gas_remaining: 50,
                efficiency: 0.40, // 40% efficiency
                cycle_duration_seconds: 3600,
                last_cycle_unix: ctx.now_unix(),
                ore_type_id: 45492, // Bitumens
                hourly_yield: 500,
                status: "Online".into(),
            },
        );
    });

    // 1. RequestMiningDetails
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
    assert_eq!(det_kv["status"].as_str(), Some("Online"));
    assert_eq!(det_kv["hourlyYield"].as_i64(), Some(500));
    assert_eq!(det_kv["fuelRemaining"].as_i64(), Some(100));
    assert_eq!(det_kv["magmaticGasRemaining"].as_i64(), Some(50));
    assert_eq!(det_kv["efficiency"].as_f64(), Some(0.40));

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
    // Expected output: 500 yield * 0.40 efficiency * 2 cycles = 400 units
    assert_eq!(cyc_kv["cyclesProcessed"].as_i64(), Some(2));
    assert_eq!(cyc_kv["unitsProduced"].as_i64(), Some(400));
    assert_eq!(cyc_kv["oreTypeID"].as_i64(), Some(45492));
    assert_eq!(cyc_kv["fuelConsumed"].as_i64(), Some(10));
    assert_eq!(cyc_kv["magmaticGasConsumed"].as_i64(), Some(20));
    assert_eq!(cyc_kv["fuelRemaining"].as_i64(), Some(90));

    // 3. Verify deposit into structure hangar
    let items_in_hangar = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|it| {
                it.location_id == structure_id
                    && it.flag == InventoryFlag::Hangar
                    && it.type_id == 45492
            })
            .map(|it| it.quantity)
            .sum::<i32>()
    });
    assert_eq!(items_in_hangar, 400, "Structure hangar must receive 400 units of moon ore");

    // 4. Verify automatic entry logged in observer mining ledger
    let ledger_entries = ctx.db.with_snapshot(|snap| {
        snap.observer_mining_ledger
            .get(&structure_id)
            .cloned()
            .unwrap_or_default()
    });
    assert!(!ledger_entries.is_empty(), "Observer mining ledger must record automated extraction");
    assert_eq!(ledger_entries.last().unwrap().quantity, 400);
}

// ============================================================================
// MN-15: Corp Mining Ledger & Observer Tracking
// ============================================================================
#[tokio::test]
async fn test_mn15_corp_mining_ledger_service_and_observer_tracking() {
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
            UpwellStructureRecord {
                structure_id: observer_id,
                type_id: 35835, // Athanor refinery observer
                owner_corp_id: corp_id,
                solar_system_id: system_id,
                name: "Jita Mining Observer".into(),
                position: [150_000.0, 20_000.0, -80_000.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                shield_hp: 50_000.0,
                armor_hp: 50_000.0,
                hull_hp: 50_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
            },
        );
    });

    // Record mining ledger entries for character in the system
    let _ = ctx.db.record_mining_ledger_entry(90_000_001, system_id, 1230, 2_500, now);
    let _ = ctx.db.record_mining_ledger_entry(90_000_001, system_id, 45492, 1_200, now);

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
            UpwellStructureRecord {
                structure_id,
                type_id: 35835,
                owner_corp_id: corp_id,
                solar_system_id: system_id,
                name: "Jita Compression Facility".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                shield_hp: 50_000.0,
                armor_hp: 50_000.0,
                hull_hp: 50_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
            },
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

    // Place 1,045 units of Veldspar (type 1230) in ship cargo
    let item_in_space = 710_000_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
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
    assert_eq!(compressed_qty, 10, "1,045 units / 100 = 10 compressed units");
    assert_eq!(rem_qty, 45, "1,045 % 100 = 45 remainder units");

    // Verify remainder item was preserved in cargo hold
    let cargo_items = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|it| it.location_id == ship_id && it.owner_id == char_id)
            .map(|it| (it.type_id, it.quantity))
            .collect::<Vec<_>>()
    });
    assert!(cargo_items.contains(&(expected_comp_tid, 10)), "Cargo must contain compressed units");
    assert!(cargo_items.contains(&(1230, 45)), "Cargo must contain remainder units");

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

    let item_in_struct = 710_000_002_i64;
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
    });

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
    assert_eq!(str_kv["quantity"].as_i64(), Some(5)); // 500 / 100 = 5

    // ------------------------------------------------------------------------
    // C. Structure Gas Decompression
    // ------------------------------------------------------------------------
    // Query decompression efficiency
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
    assert_eq!(eff_res.as_f64(), Some(0.90));

    // Place compressed item in hangar to decompress (e.g. 10 units of Compressed Veldspar 28388)
    let comp_gas_item = 710_000_003_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            comp_gas_item,
            InventoryItemRecord {
                item_id: comp_gas_item,
                type_id: expected_comp_tid, // Compressed Veldspar
                owner_id: char_id,
                location_id: structure_id,
                flag: InventoryFlag::Hangar,
                quantity: 10,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

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
    // 10 compressed * 100 * 0.90 efficiency = 900 decompressed units
    assert_eq!(dec_kv["decompressedTypeID"].as_i64(), Some(1230));
    assert_eq!(dec_kv["decompressedQuantity"].as_i64(), Some(900));
    assert_eq!(dec_kv["efficiency"].as_f64(), Some(0.90));

    // Verify decompressed item in hangar
    let hangar_raw = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .find(|it| {
                it.location_id == structure_id
                    && it.owner_id == char_id
                    && it.type_id == 1230
                    && it.quantity == 900
            })
            .cloned()
    });
    assert!(hangar_raw.is_some(), "Hangar must contain decompressed raw item with 900 units");
}

// ============================================================================
// MN-17: Living World NPC Mining Ops & Market Supply Hauling
// ============================================================================
#[tokio::test]
async fn test_mn17_living_world_npc_mining_ops_and_market_hauling() {
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
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = create_auth_session(&ctx, 1801).await;

    let char_id = 90_000_001_i64;
    let system_id = 30_000_142_u32;
    let mission_id = 1957_u32; // "Ore Harvesting (1 of 3)"
    let now = ctx.now_unix();

    // ------------------------------------------------------------------------
    // A. Mining Mission with Pocket Asteroid Belt Spawning & Delivery
    // ------------------------------------------------------------------------
    // Offer mission to character
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.insert(
            (char_id, mission_id),
            eve_db::ActiveMissionRecord {
                mission_id,
                agent_id: 3011400,
                character_id: char_id,
                state: "Offered".into(),
                current_room_id: 1,
                npcs_remaining: 0,
                spawned_npc_ball_ids: Vec::new(),
                solar_system_id: system_id,
            },
        );
    });

    // Accept mission -> spawns pocket asteroid belt in mission room
    let accept_res = ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            None,
            "AcceptMission",
            &[PyRep::Integer(mission_id as i64)],
            &kw,
            None,
        )
        .await
        .expect("AcceptMission RPC");

    let PyRep::KeyVal(accept_kv) = accept_res else {
        panic!("Expected KeyVal from AcceptMission");
    };
    assert_eq!(accept_kv["state"].as_str(), Some("Accepted"));

    // Verify pocket asteroid was spawned in the system
    let pocket_ast_id = 910_000_000 + (mission_id as i64) * 1000 + 1;
    let handle = ctx.systems.get_or_spawn(system_id);
    let pocket_ast = handle.get_asteroid_state(pocket_ast_id).await.unwrap();
    assert!(pocket_ast.is_some(), "Pocket asteroid must be spawned in mission room");
    assert_eq!(pocket_ast.unwrap().type_id, 1230); // Veldspar

    // Clear any starter Veldspar from character inventory to test incomplete delivery
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, it| !(it.owner_id == char_id && it.type_id == 1230));
    });

    // Try completing mission before mining/delivering required ore -> must fail
    let incomplete_res = ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            None,
            "CompleteMission",
            &[PyRep::Integer(mission_id as i64)],
            &kw,
            None,
        )
        .await;

    assert!(
        incomplete_res.is_err(),
        "Completing mining mission without required ore must fail"
    );
    assert_eq!(incomplete_res.err().unwrap().code, "MissionIncomplete");

    // Provide required ore (500 units of Veldspar) into character inventory
    let ore_item_id = 710_000_018_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ore_item_id,
            InventoryItemRecord {
                item_id: ore_item_id,
                type_id: 1230,
                owner_id: char_id,
                location_id: 60_003_760, // Docked station
                flag: InventoryFlag::Hangar,
                quantity: 500,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    // Complete mission -> succeeds, consumes ore, pays rewards
    let complete_res = ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            None,
            "CompleteMission",
            &[PyRep::Integer(mission_id as i64)],
            &kw,
            None,
        )
        .await
        .expect("CompleteMission RPC");

    let PyRep::KeyVal(comp_kv) = complete_res else {
        panic!("Expected KeyVal from CompleteMission");
    };
    assert_eq!(comp_kv["missionID"].as_i64(), Some(mission_id as i64));
    let m_state = ctx.db.with_snapshot(|snap| {
        snap.active_missions.get(&(char_id, mission_id)).map(|m| m.state.clone())
    });
    assert_eq!(m_state.as_deref(), Some("Completed"));

    // Verify ore was consumed from inventory
    let remaining_ore = ctx.db.with_snapshot(|snap| {
        snap.items.get(&ore_item_id).map(|it| it.quantity).unwrap_or(0)
    });
    assert_eq!(remaining_ore, 0, "Mission delivery must consume the 500 units of ore");

    // ------------------------------------------------------------------------
    // B. Career Progression Verified from Mining Ledger
    // ------------------------------------------------------------------------
    // Record ledger mining events
    let _ = ctx.db.record_mining_ledger_entry(char_id, system_id, 1230, 3_500, now);
    let _ = ctx.db.record_mining_ledger_entry(char_id, system_id, 1228, 1_500, now);

    // Query career goal progress (e.g. goalID = 2 "Mine Ore")
    let goal_res = ctx
        .dispatch_rpc(
            &mut sess,
            "careerGoals",
            None,
            "GetMiningCareerProgress",
            &[PyRep::Integer(2)],
            &kw,
            None,
        )
        .await
        .expect("GetMiningCareerProgress RPC");

    let PyRep::KeyVal(goal_kv) = goal_res else {
        panic!("Expected KeyVal from GetMiningCareerProgress");
    };
    assert_eq!(goal_kv["goalID"].as_i64(), Some(2));
    assert_eq!(
        goal_kv["currentProgress"].as_i64(),
        Some(5_000), // 3,500 + 1,500
        "Progress must match exact sum from mining ledger entries"
    );
    assert_eq!(goal_kv["verifiedFromLedger"], PyRep::Bool(true));
}
