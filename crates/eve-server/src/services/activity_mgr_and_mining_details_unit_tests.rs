//! Semantic unit tests split from `fast_checkout_and_activity_extra_unit_tests`.

#![forbid(unsafe_code)]

use crate::services::account_services::account_services_tests::{
    auth_session, test_ctx, PyRepTestExt,
};
use crate::services::fast_checkout_and_activity::*;
use eve_db::{
    AutoMoonMinerRecord, InventoryFlag, InventoryItemRecord, MoonExtractionRecord,
    UpwellStructureRecord,
};
use eve_proto::PyRep;
use std::collections::HashMap;

#[test]
fn mining_details_cycle_output_and_activity_mgr_edge_cases() {
    let ctx = test_ctx();
    let sess = auth_session(1, 90_000_001, 98_000_001, 30_000_142);
    let kw = HashMap::new();

    // Direct helper compute_mining_materials_per_cycle with explicit moon_ores vs empty fallback
    let mats_explicit = compute_mining_materials_per_cycle(
        &ctx.sde,
        30_000_142,
        40_009_082,
        &[(45_490, 0.5)],
    );
    assert!(!mats_explicit.is_empty());

    let sid = 10_002_500_001i64;
    let fuel_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 1136)
        .map(|t| t.type_id)
        .unwrap_or(4247);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.corporation_id = 98_000_001;
        }
        snap.structures.insert(
            sid,
            UpwellStructureRecord {
                structure_id: sid,
                type_id: 35835,
                owner_corp_id: 98_000_001,
                solar_system_id: 30_000_142,
                name: "Athanor Miner".into(),
                state: "ShieldVulnerable".into(),
                fuel_blocks: 0,
                hull_hp: 100_000.0,
                ..Default::default()
            },
        );
        snap.auto_moon_miners.insert(
            sid,
            AutoMoonMinerRecord {
                structure_id: sid,
                fuel_units_remaining: 77,
                magmatic_gas_remaining: 100,
                efficiency: 0.4,
                cycle_duration_seconds: 3_600,
                last_cycle_unix: 1_700_000_000,
                ore_type_id: 45_490,
                hourly_yield: 500,
                status: "Active".into(),
            },
        );
        snap.items.insert(
            880_001,
            InventoryItemRecord {
                item_id: 880_001,
                type_id: 34,
                owner_id: 98_000_001,
                location_id: sid,
                flag: InventoryFlag::SpecializedMineralHold,
                quantity: 1_000,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    // AccessDenied for outsider character without ACL access
    let outsider = auth_session(2, 90_000_999, 98_000_999, 30_000_142);
    assert_eq!(
        handle(
            &ctx,
            &outsider,
            "FastCheckoutService",
            "RequestMiningDetails",
            &[PyRep::Integer(sid)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "AccessDenied"
    );

    // Owner corp member queries RequestMiningDetails via kwargs `structureID` -> uses auto_miner fuel (77)
    let mut m_kw = HashMap::new();
    m_kw.insert("structureID".to_string(), PyRep::Integer(sid));
    let details1 = handle(
        &ctx,
        &sess,
        "FastCheckoutService",
        "RequestMiningDetails",
        &[],
        &m_kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        details1.dict_get("availableFuel").and_then(|v| v.as_i64()),
        Some(77)
    );
    assert!(
        details1
            .dict_get("remainingOutputBayCapacity")
            .and_then(|v| v.as_f64())
            .unwrap()
            > 0.0
    );

    // Add SpecializedFuelBay fuel block item + moon_extractions record -> takes precedence over auto_miner
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            880_002,
            InventoryItemRecord {
                item_id: 880_002,
                type_id: fuel_tid,
                owner_id: 98_000_001,
                location_id: sid,
                flag: InventoryFlag::SpecializedFuelBay,
                quantity: 120,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.moon_extractions.insert(
            sid,
            MoonExtractionRecord {
                structure_id: sid,
                moon_id: 40_009_082,
                extraction_start_unix: 1_700_000_000,
                chunk_arrival_unix: 1_700_050_000,
                natural_decay_unix: 1_700_090_000,
                status: "Started".into(),
                auto_fracture: false,
                event_id: 9_001,
                moon_ores: vec![(45_490, 0.75)],
                spawned_asteroid_ids: Vec::new(),
            },
        );
    });
    let details2 = handle(
        &ctx,
        &sess,
        "FastCheckoutService",
        "RequestMiningDetails",
        &[PyRep::Integer(sid)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        details2.dict_get("availableFuel").and_then(|v| v.as_i64()),
        Some(120)
    );
    assert_eq!(
        details2.dict_get("moonID").and_then(|v| v.as_i64()),
        Some(40_009_082)
    );

    assert_eq!(
        handle(
            &ctx,
            &sess,
            "FastCheckoutService",
            "RequestMiningCycleOutput",
            &[PyRep::Integer(sid)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidStructureType"
    );
    ctx.db.with_mut_snapshot(|snap| {
        let st = snap.structures.get_mut(&sid).unwrap();
        st.type_id = 81826;
        st.solar_system_id = 30_002_537;
    });
    let cycle_out = handle(
        &ctx,
        &sess,
        "FastCheckoutService",
        "RequestMiningCycleOutput",
        &[PyRep::Integer(sid)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(!cycle_out.as_list().unwrap().is_empty());

    // activityMgr: SetActivitiesPopState with KeyVal and >100 truncation
    let mut big_kv = Vec::new();
    for i in 0..105 {
        big_kv.push((format!("k_{i}"), PyRep::Integer(i)));
    }
    let _ = handle(
        &ctx,
        &sess,
        "activityMgr",
        "SetActivitiesPopState",
        &[PyRep::KeyVal(big_kv.into_iter().collect())],
        &kw,
    )
    .unwrap()
    .unwrap();
    let pop_after = handle(
        &ctx,
        &sess,
        "activityMgr",
        "GetActivitiesPopState",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap();
    let PyRep::Dict(entries) = pop_after else {
        panic!("expected dict");
    };
    assert_eq!(entries.len(), 100);
}
