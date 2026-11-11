//! Additional edge-case and branch-coverage unit tests for `mining_advanced.rs`.

#![forbid(unsafe_code)]

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

fn mk_type(tid: u32, gid: u32, cid: u32, name: &str, vol: f64, cap: f64, portion: u64) -> InvType {
    InvType {
        type_id: tid,
        group_id: gid,
        category_id: cid,
        name: name.into(),
        mass: 1.0,
        volume: vol,
        capacity: cap,
        radius: 10.0,
        base_price: 100.0,
        published: true,
        portion_size: portion,
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

fn setup_mining_ctx(ts: u64) -> (ServerContext, Arc<VirtualServerClock>) {
    let mut sde = SdeRegistry::with_canonical_seed();
    sde.types
        .entry(35835)
        .or_insert_with(|| mk_type(35835, 1406, 65, "Athanor", 10_000.0, 0.0, 1));
    sde.types.entry(81826).or_insert_with(|| {
        mk_type(
            81826,
            4744,
            65,
            "Metenox Moon Drill",
            10_000.0,
            480_000.0,
            1,
        )
    });
    sde.types
        .entry(45009)
        .or_insert_with(|| mk_type(45009, 1322, 66, "Standup Moon Drill I", 100.0, 0.0, 1));
    for (tid, gid, name) in [
        (45490, 1884, "Sylvite"),
        (46282, 1884, "Brimful Sylvite"),
        (46283, 1884, "Glistening Sylvite"),
        (45491, 1884, "Bitumens"),
        (45492, 1884, "Zeolites"),
        (45494, 1920, "Cobaltite"),
    ] {
        sde.types
            .insert(tid, mk_type(tid, gid, 25, name, 10.0, 0.0, 100));
        sde.type_materials.insert(
            tid,
            vec![TypeMaterialEntry {
                material_type_id: 16634,
                quantity: 65,
            }],
        );
    }
    sde.types
        .entry(16634)
        .or_insert_with(|| mk_type(16634, 427, 4, "Atmospheric Gases", 0.05, 0.0, 1));
    sde.types
        .entry(4247)
        .or_insert_with(|| mk_type(4247, 1136, 4, "Helium Fuel Block", 5.0, 0.0, 1));
    sde.types
        .entry(81143)
        .or_insert_with(|| mk_type(81143, 427, 4, "Magmatic Gas", 0.5, 0.0, 1));

    let db = Arc::new(WorldRepository::with_demo_seed());
    let _ = db.acquire_online_lock(1, 501, Some(90_000_001), true, ts);
    let clock = Arc::new(VirtualServerClock::with_fixed_base(ts));
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db);
    ctx.set_clock(clock.clone());
    (ctx, clock)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn moon_drill_online_checks_duration_bounds_no_moon_and_decay_tick() {
    let now = 1_775_400_000_u64;
    let (ctx, _) = setup_mining_ctx(now);
    let char_id = 90_000_001_i64;
    let corp_id = 98_444_001_i64;
    let other_corp = 98_444_002_i64;
    let sys_id = 30_002_537_u32; // lowsec with moons
    let struct_id = 1_047_000_001_i64;
    let drill_item_id = 1_047_000_099_i64;
    let kw = HashMap::new();

    let moon = ctx
        .sde
        .celestials_in_system(sys_id)
        .into_iter()
        .find(|c| c.group_id == 8)
        .unwrap();

    // Missing structure -> structure_has_online_moon_drill == false
    assert!(ctx
        .db
        .with_snapshot(|s| !structure_has_online_moon_drill(s, struct_id)));

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.corporation_id = corp_id;
        }
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default(); __corp.corporation_id = corp_id; __corp.name = "Moon Corp".into(); __corp.ticker = "MOON".into(); __corp.ceo_id = char_id; __corp.ensure_wallet_divisions(); __corp },
        );
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35835,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Moon Athanor".into(),
                state: "HullReinforce".into(), // non-operational first
                position: [moon.position.x + 1_000.0, moon.position.y, moon.position.z],
                quantum_core_installed: false,
                fuel_blocks: 100,
                ..Default::default()
            },
        );
        snap.items.insert(
            drill_item_id,
            InventoryItemRecord {
                item_id: drill_item_id,
                type_id: 45009,
                owner_id: corp_id,
                location_id: struct_id,
                flag: InventoryFlag::Hangar, // not in ServiceSlot yet
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    // Check HullReinforce / no core / Hangar / ServiceSlot offline / ServiceSlot online transitions
    assert!(ctx
        .db
        .with_snapshot(|s| !structure_has_online_moon_drill(s, struct_id)));
    ctx.db.with_mut_snapshot(|snap| {
        let st = snap.structures.get_mut(&struct_id).unwrap();
        st.state = "ShieldVulnerable".into();
        st.quantum_core_installed = true;
    });
    assert!(ctx
        .db
        .with_snapshot(|s| !structure_has_online_moon_drill(s, struct_id)));
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&drill_item_id).unwrap().flag = InventoryFlag::ServiceSlot0;
        snap.module_online_states.insert(drill_item_id, false);
    });
    assert!(ctx
        .db
        .with_snapshot(|s| !structure_has_online_moon_drill(s, struct_id)));
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(drill_item_id, true);
    });
    assert!(ctx
        .db
        .with_snapshot(|s| structure_has_online_moon_drill(s, struct_id)));

    let mut sess = SessionState::default();
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);

    // InvalidExtractionDuration bounds: <=0, < 6d (518_400), > 56d (4_838_400)
    for bad_dur in [0_i64, 100_000, 5_000_000] {
        let err = handle(
            &ctx,
            &mut sess,
            "moonExtractions",
            "StartNewExtraction",
            &[PyRep::Integer(struct_id), PyRep::Integer(bad_dur)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(err.code, "InvalidExtractionDuration");
    }

    // Timestamp-form duration (now + 600_000) and kwargs "extractionLength"
    let mut kw_len = HashMap::new();
    kw_len.insert(
        "extractionLength".into(),
        PyRep::Integer((now + 600_000) as i64),
    );
    let prev_struct_id = sess.structure_id;
    sess.structure_id = Some(struct_id);
    let started = handle(
        &ctx,
        &mut sess,
        "moonExtractions",
        "StartNewExtraction",
        &[],
        &kw_len,
    )
    .await
    .unwrap()
    .expect("timestamp duration accepted");
    sess.structure_id = prev_struct_id;
    assert_eq!(
        started.dict_get("chunkArrivalTime"),
        Some(&PyRep::Integer((now + 600_000) as i64))
    );

    // GetExtractionsForCorp: non-GM foreign corp -> AccessDenied, GM -> Ok, owner -> Ready when clock >= chunk_arrival
    let mut other_sess = sess.clone();
    other_sess.character_id = Some(90_999_999);
    other_sess.corporation_id = Some(other_corp);
    assert_eq!(
        handle(
            &ctx,
            &mut other_sess,
            "moonExtractions",
            "GetExtractionsForCorp",
            &[PyRep::Integer(corp_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "AccessDenied"
    );
    other_sess.role = eve_db::service_roles::ROLE_GMH;
    assert!(handle(
        &ctx,
        &mut other_sess,
        "moonExtractions",
        "GetExtractionsForCorp",
        &[PyRep::Integer(corp_id)],
        &kw,
    )
    .await
    .unwrap()
    .is_ok());

    // Advance past chunk_arrival_unix -> GetExtractionsForCorp reports "Ready"; fracture explicitly
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(now + 600_005)));
    let list_ready = handle(
        &ctx,
        &mut sess,
        "moonExtractions",
        "GetExtractionsForCorp",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    if let PyRep::List(rows) = list_ready {
        assert_eq!(
            rows[0].dict_get("status"),
            Some(&PyRep::String("Ready".into()))
        );
    } else {
        panic!("expected List");
    }

    let frac = handle(
        &ctx,
        &mut sess,
        "moonExtractions",
        "FractureChunkWithStructure",
        &[PyRep::Integer(struct_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("Ready chunk fractures cleanly");
    assert!(
        frac.dict_get("asteroidCount")
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            > 0
    );

    // Fracturing again -> ExtractionAlreadyFractured
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "moonExtractions",
            "FractureChunkWithStructure",
            &[PyRep::Integer(struct_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "ExtractionAlreadyFractured"
    );

    // Cancelling fractured extraction -> CannotCancelExtraction
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "moonExtractions",
            "CancelExtraction",
            &[PyRep::Integer(struct_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CannotCancelExtraction"
    );

    // Advance past natural_decay_unix -> tick_all_moon_extractions marks Decayed and clears asteroids
    let decay_at = ctx
        .db
        .with_snapshot(|s| s.moon_extractions[&struct_id].natural_decay_unix);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(decay_at + 10)));
    tick_all_moon_extractions(&ctx).await;
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.moon_extractions[&struct_id].status, "Decayed");
        assert!(s.mining_and_ess_state.moon_fractured_fields.is_empty());
    });

    // GetExtractionAndEventIDFromStructureID reports activeAsteroidCount == 0 when Decayed
    let info = handle(
        &ctx,
        &mut sess,
        "moonExtractions",
        "GetExtractionAndEventIDFromStructureID",
        &[PyRep::Integer(struct_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        info.dict_get("activeAsteroidCount"),
        Some(&PyRep::Integer(0))
    );

    // System with no moons -> resolve_structure_moon_id returns None & StartNewExtraction -> NoMoonInSystem
    let mut no_moon_sys = None;
    for (&sid, _) in &ctx.sde.solar_systems {
        if ctx
            .sde
            .celestials_in_system(sid)
            .iter()
            .all(|c| c.group_id != 8)
        {
            no_moon_sys = Some(sid);
            break;
        }
    }
    if let Some(empty_sys) = no_moon_sys {
        assert!(resolve_structure_moon_id(&ctx.sde, empty_sys, [0.0, 0.0, 0.0]).is_none());
        ctx.db.with_mut_snapshot(|snap| {
            snap.structures.get_mut(&struct_id).unwrap().solar_system_id = empty_sys;
        });
        assert_eq!(
            handle(
                &ctx,
                &mut sess,
                "moonExtractions",
                "StartNewExtraction",
                &[PyRep::Integer(struct_id), PyRep::Integer(518_400)],
                &kw,
            )
            .await
            .unwrap()
            .unwrap_err()
            .code,
            "NoMoonInSystem"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn metenox_highsec_skip_partial_cycle_and_output_bay_full_branches() {
    let now = 1_775_400_000_u64;
    let (ctx, _) = setup_mining_ctx(now);
    let char_id = 90_000_001_i64;
    let corp_id = 98_445_001_i64;
    let metenox_hs = 1_047_100_001_i64;
    let metenox_ls = 1_047_100_002_i64;
    let hs_sys = 30_000_142_u32;
    let ls_sys = 30_002_537_u32;
    let kw = HashMap::new();

    let moon_ls = ctx
        .sde
        .celestials_in_system(ls_sys)
        .into_iter()
        .find(|c| c.group_id == 8)
        .unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.corporation_id = corp_id;
        }
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default(); __corp.corporation_id = corp_id; __corp.name = "Metenox Corp".into(); __corp.ticker = "METX".into(); __corp.ceo_id = char_id; __corp.ensure_wallet_divisions(); __corp },
        );
        // Highsec Metenox (skipped by tick_all_metenox_moon_drills)
        snap.structures.insert(
            metenox_hs,
            UpwellStructureRecord {
                structure_id: metenox_hs,
                type_id: 81826,
                owner_corp_id: corp_id,
                solar_system_id: hs_sys,
                name: "Highsec Metenox".into(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                ..Default::default()
            },
        );
        // Lowsec Metenox with enough fuel (10 blocks via st.fuel_blocks fallback) & 200 magmatic gas (covers only 1 cycle if 200/cycle)
        snap.structures.insert(
            metenox_ls,
            UpwellStructureRecord {
                structure_id: metenox_ls,
                type_id: 81826,
                owner_corp_id: corp_id,
                solar_system_id: ls_sys,
                name: "Lowsec Metenox".into(),
                state: "ShieldVulnerable".into(),
                position: [
                    moon_ls.position.x + 500.0,
                    moon_ls.position.y,
                    moon_ls.position.z,
                ],
                quantum_core_installed: true,
                fuel_blocks: 100, // fallback fuel when no FuelBay item
                ..Default::default()
            },
        );
        // Magmatic gas for multiple cycles + existing stack in SpecializedMineralHold to test merge stacking
        snap.items.insert(
            99_100_001,
            InventoryItemRecord {
                item_id: 99_100_001,
                type_id: 81143,
                owner_id: corp_id,
                location_id: metenox_ls,
                flag: InventoryFlag::StructureFuel,
                quantity: 10_000,
                ..Default::default()
            },
        );
        // Pre-fill SpecializedMineralHold to capacity (500_000 m3) using type 16634 (0.05 m3 each -> 10_000_000 units = 500_000 m3)
        snap.items.insert(
            99_100_002,
            InventoryItemRecord {
                item_id: 99_100_002,
                type_id: 16634,
                owner_id: corp_id,
                location_id: metenox_ls,
                flag: InventoryFlag::SpecializedMineralHold,
                quantity: 10_000_000,
                ..Default::default()
            },
        );
    });

    let mut sess = SessionState::default();
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);

    // Unhandled service/method returns None, RequestMiningDetails without ID -> InvalidArgument
    assert!(
        handle(&ctx, &mut sess, "charMgr", "GetCharacterInfo", &[], &kw)
            .await
            .is_none()
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "autoMoonMining",
            "RequestMiningDetails",
            &[],
            &kw
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidArgument"
    );

    // OutputBayFull status reported in RequestMiningDetails and stops RequestMiningCycleOutput
    let details = handle(
        &ctx,
        &mut sess,
        "autoMoonMining",
        "RequestMiningDetails",
        &[PyRep::Integer(metenox_ls)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        details.dict_get("status"),
        Some(&PyRep::String("OutputBayFull".into()))
    );

    let full_cycle = handle(
        &ctx,
        &mut sess,
        "autoMoonMining",
        "RequestMiningCycleOutput",
        &[PyRep::Integer(metenox_ls)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(full_cycle, PyRep::List(ref v) if !v.is_empty()));
    let qty_before_full_tick = ctx.db.with_snapshot(|s| s.items[&99_100_002].quantity);
    tick_all_metenox_moon_drills(&ctx, now - 7_200, now);
    assert_eq!(
        ctx.db.with_snapshot(|s| s.items[&99_100_002].quantity),
        qty_before_full_tick
    );

    // Free space in SpecializedMineralHold and advance 10 hours -> stacks onto existing stack (99_100_002) if same type, and skips highsec Metenox
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&99_100_002).unwrap().quantity = 100;
        if let Some(m) = snap.auto_moon_miners.get_mut(&metenox_ls) {
            m.last_cycle_unix = now - 7_200;
        }
    });
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(now + 10_800)));
    tick_all_metenox_moon_drills(&ctx, now - 7_200, now + 10_800);
    ctx.db.with_snapshot(|snap| {
        // Highsec Metenox was never created/ticked
        assert!(!snap.auto_moon_miners.contains_key(&metenox_hs));
        // Lowsec Metenox processed cycles using st.fuel_blocks fallback
        assert!(snap.structures[&metenox_ls].fuel_blocks < 100);
    });
}
