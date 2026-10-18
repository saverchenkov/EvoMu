//! Industry and reprocessing integration tests.

#![forbid(unsafe_code)]

use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

#[tokio::test]
async fn test_phase_0_s1_reprocessing_sde_materials_and_remainders() {
    // parity: plan:S-1
    use eve_db::{InventoryFlag, InventoryItemRecord};

    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(901);
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
    .expect("Login");
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
    .expect("SelectCharacterID");

    // 1. Non-reprocessable items (Tritanium 100_000_011 type 34, and Rifter Blueprint 100_000_007 type 691)
    // must return ItemNotReprocessable and remain untouched.
    let trit_err = ctx
        .dispatch_rpc(
            &mut sess,
            "reprocessingSvc",
            None,
            "Reprocess",
            &[PyRep::Integer(100_000_011)],
            &kw,
            None,
        )
        .await;
    assert_eq!(trit_err.unwrap_err().code, "ItemNotReprocessable");
    assert_eq!(
        ctx.db.get_item(100_000_011).map(|i| i.quantity),
        Some(5_000)
    );

    let bp_err = ctx
        .dispatch_rpc(
            &mut sess,
            "reprocessingSvc",
            None,
            "Reprocess",
            &[PyRep::Integer(100_000_007)],
            &kw,
            None,
        )
        .await;
    assert_eq!(bp_err.unwrap_err().code, "ItemNotReprocessable");
    assert!(ctx.db.get_item(100_000_007).is_some());

    // 2. Active ship (100_000_001) and assembled (singleton = true) ships in hangar must be rejected.
    let active_ship_err = ctx
        .dispatch_rpc(
            &mut sess,
            "reprocessingSvc",
            None,
            "Reprocess",
            &[PyRep::Integer(100_000_001)],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        active_ship_err.unwrap_err().code,
        "CannotReprocessActiveShip"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            100_990_000,
            InventoryItemRecord {
                item_id: 100_990_000,
                type_id: 587, // Rifter (has SDE type_materials, but singleton = true means assembled)
                owner_id: 90_000_001,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    let assembled_err = ctx
        .dispatch_rpc(
            &mut sess,
            "reprocessingSvc",
            None,
            "Reprocess",
            &[PyRep::Integer(100_990_000)],
            &kw,
            None,
        )
        .await;
    assert_eq!(assembled_err.unwrap_err().code, "ItemMustBeRepackaged");
    assert!(ctx.db.get_item(100_990_000).is_some());

    // 3. Reprocess 10,000 Veldspar (100_000_012, type 1230, portionSize = 100, 34 -> 400 per batch).
    // With 0 skills, base_rate = 0.50 and Caldari Navy standing 2.0 -> station_tax_rate = 0.035 -> net_yield_rate = 0.4825 -> 100 * 400 * 0.4825 = 19,300 Tritanium.
    let reproc_veld = ctx
        .dispatch_rpc(
            &mut sess,
            "reprocessingSvc",
            None,
            "Reprocess",
            &[PyRep::Integer(100_000_012)],
            &kw,
            None,
        )
        .await
        .expect("Reprocess 10,000 Veldspar");
    let PyRep::KeyVal(veld_kv) = reproc_veld else {
        panic!("Expected KeyVal from Veldspar Reprocess");
    };
    assert_eq!(
        veld_kv.get("tritaniumQuantity").and_then(|v| v.as_i64()),
        Some(19_300),
        "S-1: 10,000 Veldspar at 0.50 facility yield and 3.5% standing-adjusted station tax must yield 19,300 Tritanium"
    );
    assert!(
        ctx.db.get_item(100_000_012).is_none(),
        "S-1: Exact multiple of portionSize (10,000 Veldspar) must consume entire stack"
    );

    // 4. Reprocess 250 Plagioclase (100_990_001, type 18, portionSize = 100, 34 -> 175, 36 -> 70):
    // consumes 200 units (2 batches), leaves 50 remainder in stack, and outputs floor(2*175*0.4825)=168 Tritanium + floor(2*70*0.4825)=67 Mexallon.
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            100_990_001,
            InventoryItemRecord {
                item_id: 100_990_001,
                type_id: 18,
                owner_id: 90_000_001,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 250,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    let reproc_plag = ctx
        .dispatch_rpc(
            &mut sess,
            "reprocessingSvc",
            None,
            "Reprocess",
            &[PyRep::Integer(100_990_001)],
            &kw,
            None,
        )
        .await
        .expect("Reprocess 250 Plagioclase");
    let PyRep::KeyVal(rp_kv) = reproc_plag else {
        panic!("Expected KeyVal from Reprocess");
    };
    assert_eq!(
        ctx.db.get_item(100_990_001).map(|i| i.quantity),
        Some(50),
        "S-1: 50 un-reprocessed Plagioclase units (< portionSize 100) must remain in stack"
    );
    assert_eq!(
        rp_kv.get("tritaniumQuantity").and_then(|v| v.as_i64()),
        Some(168)
    );
    let Some(PyRep::List(outputs)) = rp_kv.get("outputs") else {
        panic!("Expected outputs list from Reprocess");
    };
    assert_eq!(outputs.len(), 2);
    let mexallon_qty = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| {
                i.owner_id == 90_000_001
                    && i.location_id == 60_003_760
                    && i.type_id == 36
                    && i.item_id != 100_000_010
            })
            .map(|i| i.quantity as i64)
            .sum::<i64>()
    });
    assert_eq!(
        mexallon_qty, 67,
        "S-1: Reprocessing 200 Plagioclase at 0.4825 net yield must deposit 67 Mexallon (36)"
    );
}
