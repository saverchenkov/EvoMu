//! Extra unit and mutation-killing tests for `mining_advanced.rs`.

use crate::router::ServerContext;
use crate::services::mining_advanced::*;
use eve_db::clock::VirtualServerClock;
use eve_db::{
    CorporationRecord, InventoryFlag, InventoryItemRecord, UpwellStructureRecord, WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry, TypeMaterialEntry};
use std::collections::HashMap;
use std::sync::Arc;

fn make_inv_type(
    type_id: u32,
    group_id: u32,
    category_id: u32,
    name: &str,
    volume: f64,
    capacity: f64,
    portion_size: u64,
) -> InvType {
    InvType {
        type_id,
        group_id,
        category_id,
        name: name.into(),
        mass: 1.0,
        volume,
        capacity,
        radius: 10.0,
        base_price: 100.0,
        published: true,
        portion_size,
        market_group_id: None,
        race_id: None,
        faction_id: None,
        variation_parent_type_id: None,
        description_id: None,
        icon_id: None,
        graphic_id: None,
        sound_id: None,
        meta_group_id: None,
        meta_level: None,
        tech_level: None,
        default_effect_id: None,
        description: String::new(),
        effect_ids: Vec::new(),
        default_attributes: HashMap::new(),
    }
}

fn make_ctx_and_clock(ts: u64) -> (ServerContext, Arc<VirtualServerClock>) {
    let mut sde = SdeRegistry::with_canonical_seed();
    sde.types
        .entry(35835)
        .or_insert_with(|| make_inv_type(35835, 1406, 65, "Athanor", 10_000.0, 0.0, 1));
    sde.types
        .entry(35832)
        .or_insert_with(|| make_inv_type(35832, 1657, 65, "Astrahus", 10_000.0, 0.0, 1));
    sde.types.insert(
        81826,
        make_inv_type(81826, 4744, 65, "Metenox Moon Drill", 10_000.0, 480_000.0, 1),
    );
    sde.types
        .entry(35894)
        .or_insert_with(|| make_inv_type(35894, 1322, 66, "Standup Moon Drill I", 100.0, 0.0, 1));

    // Refinery stability rig with attribute 2707 (+50% auto-fracture delay)
    let mut rig = make_inv_type(37180, 1912, 66, "Standup Moon Drill Stability Rig I", 20.0, 0.0, 1);
    rig.default_attributes.insert(2707, 50.0);
    sde.types.insert(37180, rig);

    for (tid, gid, name) in [
        (45490, 1884, "Sylvite"),
        (46282, 1884, "Brimful Sylvite"),
        (46283, 1884, "Glistening Sylvite"),
        (45491, 1884, "Bitumens"),
        (45492, 1884, "Zeolites"),
        (45493, 1884, "Coesite"),
        (45494, 1920, "Cobaltite"),
        (45498, 1921, "Vanadinite"),
    ] {
        sde.types
            .insert(tid, make_inv_type(tid, gid, 25, name, 10.0, 0.0, 100));
        sde.type_materials.insert(
            tid,
            vec![TypeMaterialEntry {
                material_type_id: 16634,
                quantity: 65,
            }],
        );
    }
    sde.types.insert(
        16634,
        make_inv_type(16634, 427, 4, "Atmospheric Gases", 0.05, 0.0, 1),
    );
    sde.types.insert(
        4247,
        make_inv_type(4247, 1136, 4, "Helium Fuel Block", 5.0, 0.0, 1),
    );
    sde.types.insert(
        81143,
        make_inv_type(81143, 427, 4, "Magmatic Gas", 0.5, 0.0, 1),
    );

    let db = Arc::new(WorldRepository::with_demo_seed());
    db.acquire_online_lock(1, 501, Some(90_000_001), true, ts)
        .unwrap();
    let clock = Arc::new(VirtualServerClock::with_fixed_base(ts));
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db);
    ctx.set_clock(clock.clone());
    (ctx, clock)
}

#[test]
fn derive_moon_ores_from_sde_covers_zarzakh_highsec_r4_lowsec_and_sparse_fallback() {
    let (ctx, _) = make_ctx_and_clock(1_775_400_000);
    // Zarzakh (30_100_000) returns empty Vec
    assert!(derive_moon_ores_from_sde(&ctx.sde, 30_100_000, 40_000_001).is_empty());

    // Highsec (Jita 30_000_142, sec >= 0.45) restricts all 3 ores to R4 (group 1884)
    let hs_ores = derive_moon_ores_from_sde(&ctx.sde, 30_000_142, 40_009_080);
    assert_eq!(hs_ores.len(), 3);
    for (tid, _) in &hs_ores {
        let g = ctx.sde.types.get(tid).map(|t| t.group_id).unwrap_or(0);
        assert_eq!(g, 1884, "highsec moon ore must be R4 group 1884");
    }

    // Lowsec (Amamake 30_002_537, sec < 0.45) selects higher moon ore (1920..=1923) for idx0
    let ls_ores = derive_moon_ores_from_sde(&ctx.sde, 30_002_537, 40_161_450);
    assert_eq!(ls_ores.len(), 3);
    let g0 = ctx.sde.types.get(&ls_ores[0].0).map(|t| t.group_id).unwrap_or(0);
    assert!(
        (1920..=1923).contains(&g0),
        "lowsec primary moon ore should come from 1920..=1923"
    );

    // Sparse SDE (< 3 moon ores) falls back to canonical 3-tuple
    let empty_sde = SdeRegistry::default();
    let fb = derive_moon_ores_from_sde(&empty_sde, 30_002_537, 40_000_001);
    assert_eq!(fb, vec![(45492, 0.40), (45490, 0.35), (45494, 0.25)]);
}

#[tokio::test]
async fn start_cancel_and_fracture_extraction_error_and_rig_bonus_branches() {
    let (ctx, _clock) = make_ctx_and_clock(1_775_400_000);
    let char_id = 90_000_001_i64;
    let corp_id = 1_000_009_i64;
    let struct_id = 1_000_000_000_500_i64;
    let sys_id = 30_002_537_u32;
    let kw = HashMap::new();
    let moon = ctx
        .sde
        .celestials_in_system(sys_id)
        .into_iter()
        .find(|c| c.group_id == 8)
        .unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.corporation_id = corp_id;
        }
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Refinery Corp".into(),
                ticker: "REFI".into(),
                ceo_id: char_id,
                tax_rate: 0.05,
                ..Default::default()
            },
        );
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35832, // Astrahus (invalid for moon extraction)
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Test Refinery".into(),
                state: "ShieldVulnerable".into(),
                position: [moon.position.x + 500.0, moon.position.y, moon.position.z],
                quantum_core_installed: true,
                fuel_blocks: 1000,
                ..Default::default()
            },
        );
    });

    let mut session = SessionState::default();
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    session.corporation_id = Some(corp_id);

    // 1. Missing structure_id -> InvalidArgument; 2. StructureNotFound
    let moon_svc = "moonExtractions";
    for m in [
        "StartNewExtraction",
        "CancelExtraction",
        "FractureChunkWithStructure",
        "GetMoonResourcesForStructure",
        "GetExtractionAndEventIDFromStructureID",
    ] {
        let err = handle(&ctx, &mut session, moon_svc, m, &[], &kw)
            .await
            .unwrap()
            .expect_err("missing structure_id");
        assert_eq!(err.code, "InvalidArgument");

        let nf = handle(
            &ctx,
            &mut session,
            moon_svc,
            m,
            &[PyRep::Integer(999_999_999)],
            &kw,
        )
        .await
        .unwrap()
        .expect_err("structure not found");
        assert_eq!(nf.code, "StructureNotFound");
    }

    // 3. InvalidStructureType (Astrahus 35832)
    let err = handle(
        &ctx,
        &mut session,
        "moonExtractions",
        "StartNewExtraction",
        &[PyRep::Integer(struct_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect_err("Astrahus cannot extract moons");
    assert_eq!(err.code, "InvalidStructureType");

    // 4. Switch to Athanor (35835) with fuel_blocks == 0 -> StructureServiceOffline
    ctx.db.with_mut_snapshot(|snap| {
        let st = snap.structures.get_mut(&struct_id).unwrap();
        st.type_id = 35835;
        st.fuel_blocks = 0;
    });
    let err = handle(
        &ctx,
        &mut session,
        "moonExtractions",
        "StartNewExtraction",
        &[PyRep::Integer(struct_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect_err("zero fuel blocks");
    assert_eq!(err.code, "StructureServiceOffline");

    // Restore fuel blocks and fit stability rig (37180, +50% delay -> 16,200s)
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&struct_id).unwrap().fuel_blocks = 500;
        snap.items.insert(
            700_101,
            InventoryItemRecord {
                item_id: 700_101,
                type_id: 37180,
                owner_id: corp_id,
                location_id: struct_id,
                flag: InventoryFlag::RigSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    // GetExtractionAndEventIDFromStructureID before any extraction -> (None, None)
    let empty_tup = handle(
        &ctx,
        &mut session,
        "moonExtractions",
        "GetExtractionAndEventIDFromStructureID",
        &[PyRep::Integer(struct_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("empty tuple");
    assert_eq!(empty_tup, PyRep::Tuple(vec![PyRep::None, PyRep::None]));

    // StartNewExtraction using single-arg duration inference when session.structure_id is set
    session.structure_id = Some(struct_id);
    let now = ctx.now_unix();
    let started = handle(
        &ctx,
        &mut session,
        "moonExtractions",
        "StartNewExtraction",
        &[PyRep::Integer(600_000)],
        &kw,
    )
    .await
    .unwrap()
    .expect("start with single-arg duration");
    let ev_id = started.dict_get("eventID").and_then(|v| v.as_i64()).unwrap();

    // Starting again while active -> ExtractionAlreadyActive
    let dup = handle(
        &ctx,
        &mut session,
        "moonExtractions",
        "StartNewExtraction",
        &[PyRep::Integer(struct_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect_err("already active");
    assert_eq!(dup.code, "ExtractionAlreadyActive");

    // Fracture before chunk arrival -> ChunkNotReadyToFracture
    let early = handle(
        &ctx,
        &mut session,
        "moonExtractions",
        "FractureChunkWithStructure",
        &[PyRep::Integer(struct_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect_err("too early to fracture");
    assert_eq!(early.code, "ChunkNotReadyToFracture");

    // Verify rig bonus (+50% -> 16,200s) in GetExtractionAndEventIDFromStructureID
    let tup = handle(
        &ctx,
        &mut session,
        "moonExtractions",
        "GetExtractionAndEventIDFromStructureID",
        &[PyRep::Integer(struct_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("extraction tuple");
    if let PyRep::Tuple(parts) = tup {
        let arr = parts[0]
            .dict_get("chunkArrivalTime")
            .and_then(|v| v.as_i64())
            .unwrap();
        let auto_t = parts[0]
            .dict_get("autoFractureTime")
            .and_then(|v| v.as_i64())
            .unwrap();
        assert_eq!(auto_t - arr, 16_200);
    } else {
        panic!("expected tuple");
    }

    // CancelExtraction marks calendar event deleted and status "Cancelled"
    let cancelled = handle(
        &ctx,
        &mut session,
        "moonExtractions",
        "CancelExtraction",
        &[PyRep::Integer(struct_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("cancel active extraction");
    assert_eq!(cancelled, PyRep::Bool(true));
    ctx.db.with_snapshot(|snap| {
        assert!(snap.calendar_events.get(&ev_id).unwrap().is_deleted);
    });

    // Cancelling again -> CannotCancelExtraction; fracturing cancelled -> InvalidExtractionState
    let cancel_err = handle(
        &ctx,
        &mut session,
        "moonExtractions",
        "CancelExtraction",
        &[PyRep::Integer(struct_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect_err("already cancelled");
    assert_eq!(cancel_err.code, "CannotCancelExtraction");

    let frac_err = handle(
        &ctx,
        &mut session,
        "moonExtractions",
        "FractureChunkWithStructure",
        &[PyRep::Integer(struct_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect_err("fracture cancelled");
    assert_eq!(frac_err.code, "InvalidExtractionState");

    // Start new extraction and advance clock past chunk_arrival_unix + 16,200s to test tick_all_moon_extractions auto-fracture
    handle(
        &ctx,
        &mut session,
        "moonExtractions",
        "StartNewExtraction",
        &[PyRep::Integer(struct_id), PyRep::Integer(518_400)],
        &kw,
    )
    .await
    .unwrap()
    .expect("restart extraction");

    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(
        now + 518_400 + 16_205,
    )));
    tick_all_moon_extractions(&ctx).await;
    ctx.db.with_snapshot(|snap| {
        let ext = snap.moon_extractions.get(&struct_id).unwrap();
        assert_eq!(ext.status, "Fractured");
        assert!(ext.auto_fracture);
        assert!(!ext.spawned_asteroid_ids.is_empty());
    });
}

#[tokio::test]
async fn metenox_auto_moon_mining_statuses_and_production_tick() {
    let (ctx, _) = make_ctx_and_clock(1_775_400_000);
    let char_id = 90_000_001_i64;
    let corp_id = 1_000_009_i64;
    let metenox_id = 1_000_000_000_600_i64;
    let sys_id = 30_002_537_u32;
    let kw = HashMap::new();
    let moon = ctx
        .sde
        .celestials_in_system(sys_id)
        .into_iter()
        .find(|c| c.group_id == 8)
        .unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.corporation_id = corp_id;
        }
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Metenox Corp".into(),
                ticker: "MTNX".into(),
                ceo_id: char_id,
                tax_rate: 0.05,
                ..Default::default()
            },
        );
        snap.structures.insert(
            metenox_id,
            UpwellStructureRecord {
                structure_id: metenox_id,
                type_id: 81826, // Metenox Moon Drill
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Amamake Metenox".into(),
                state: "ShieldVulnerable".into(),
                position: [moon.position.x + 500.0, moon.position.y, moon.position.z],
                quantum_core_installed: true,
                fuel_blocks: 0,
                ..Default::default()
            },
        );
    });

    let mut session = SessionState::default();
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    session.corporation_id = Some(corp_id);

    // 1. OutOfFuel when fuel_blocks == 0
    let details_no_fuel = handle(
        &ctx,
        &mut session,
        "autoMoonMining",
        "RequestMiningDetails",
        &[PyRep::Integer(metenox_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("details out of fuel");
    assert_eq!(
        details_no_fuel.dict_get("status").and_then(|v| v.as_str()),
        Some("OutOfFuel")
    );

    // 2. Add fuel blocks (4247), still 0 Magmatic Gas (81143) -> OutOfMagmaticGas
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            800_101,
            InventoryItemRecord {
                item_id: 800_101,
                type_id: 4247,
                owner_id: corp_id,
                location_id: metenox_id,
                flag: InventoryFlag::StructureFuel,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    let details_no_gas = handle(
        &ctx,
        &mut session,
        "autoMoonMining",
        "RequestMiningDetails",
        &[PyRep::Integer(metenox_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("details out of gas");
    assert_eq!(
        details_no_gas.dict_get("status").and_then(|v| v.as_str()),
        Some("OutOfMagmaticGas")
    );

    // 3. Add Magmatic Gas (81143) -> Active, then run tick_all_metenox_moon_drills for 2 cycles (7200s)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            800_102,
            InventoryItemRecord {
                item_id: 800_102,
                type_id: 81143,
                owner_id: corp_id,
                location_id: metenox_id,
                flag: InventoryFlag::StructureFuel,
                quantity: 1_000,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    let details_active = handle(
        &ctx,
        &mut session,
        "autoMoonMining",
        "RequestMiningDetails",
        &[PyRep::Integer(metenox_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("details active");
    assert_eq!(
        details_active.dict_get("status").and_then(|v| v.as_str()),
        Some("Active")
    );

    let cycle_out = handle(
        &ctx,
        &mut session,
        "autoMoonMining",
        "RequestMiningCycleOutput",
        &[PyRep::Integer(metenox_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("cycle output");
    assert!(matches!(cycle_out, PyRep::List(v) if !v.is_empty()));

    tick_all_metenox_moon_drills(&ctx, 1_775_400_000, 1_775_407_200);
    ctx.db.with_snapshot(|snap| {
        let am = snap.auto_moon_miners.get(&metenox_id).expect("miner state");
        assert_eq!(am.status, "Active");
        assert!(am.fuel_units_remaining < 100);
        assert!(am.magmatic_gas_remaining < 1_000);
        let harvested = snap.items.values().any(|it| {
            it.location_id == metenox_id && it.flag == InventoryFlag::SpecializedMineralHold && it.quantity > 0
        });
        assert!(harvested, "Metenox tick must deposit harvested moon materials");
    });
}
