//! Focused unit tests for uncovered branches in `space_entity_mining`
//! (`reprocessingSvc.Reprocess` single/batch/corp/structure/remainder/errors,
//! `reprocessingSvc.GetQuotes` summary/list/tuple, and `handle_mine`).

use super::*;
use crate::router::ServerContext;
use crate::services::station_structure::structure_upwell_ops::{self, save_ext_state, StructureAccessProfile};
use eve_db::clock::VirtualServerClock;
use eve_db::types::UpwellStructureRecord;
use eve_db::{CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

fn full_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::clone(&ServerContext::new().sde)))
}

#[tokio::test]
async fn reprocessing_quotes_and_reprocess_npc_structure_and_error_paths() {
    let ctx = ServerContext::with_sde_and_repository(
        full_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_700_000_000)));

    let char_id = 96_940_001i64;
    let corp_id = 98_940_001i64;
    let station_id = 60_003_760i64;
    let ship_id = 96_940_500i64;
    let ore_stack_id = 96_940_101i64;
    let small_stack_id = 96_940_102i64;
    let singleton_item_id = 96_940_103i64;
    let structure_id = 1_040_000_000_999i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = 9601;
        ch.corporation_id = corp_id;
        ch.station_id = Some(station_id);
        ch.solar_system_id = 30_000_142;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);

        let mut corp = { let mut __corp = CorporationRecord::default();
            __corp.corporation_id = corp_id;
            __corp.name = "Reproc Corp".into();
            __corp.ticker = "RPRC".into();
            __corp.ceo_id = char_id;
            __corp.set_division_balance(1, IskCents::from_f64_isk(50_000_000.0));
            __corp };
        corp.ensure_wallet_divisions();
        snap.corporations.insert(corp_id, corp);

        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        // 350 Veldspar (portion_size 100 -> 3 portions + 50 remainder!)
        snap.items.insert(
            ore_stack_id,
            InventoryItemRecord {
                item_id: ore_stack_id,
                type_id: 1230, // Veldspar
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 350,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        // 25 Veldspar (< 1 portion -> ItemQuantityTooLow)
        snap.items.insert(
            small_stack_id,
            InventoryItemRecord {
                item_id: small_stack_id,
                type_id: 1230,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 25,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        // Unrepackaged singleton Veldspar -> ItemMustBeRepackaged
        snap.items.insert(
            singleton_item_id,
            InventoryItemRecord {
                item_id: singleton_item_id,
                type_id: 1230,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 100,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );

        // Online Athanor refinery (type 35835) with offline Standup Reprocessing service (35899)
        snap.structures.insert(
            structure_id,
            UpwellStructureRecord {
                structure_id,
                solar_system_id: 30_000_142,
                type_id: 35835,
                owner_corp_id: corp_id,
                name: "Refinery Test".into(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 200,
                ..Default::default()
            },
        );
        snap.items.insert(
            96_940_801,
            InventoryItemRecord {
                item_id: 96_940_801,
                type_id: 35899,
                owner_id: corp_id,
                location_id: structure_id,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(96_940_801, false);
        let mut ext = structure_upwell_ops::load_ext_state(snap);
        ext.access_profiles.insert(
            structure_id,
            StructureAccessProfile {
                profile_id: 7200,
                owner_corp_id: corp_id,
                name: "RefineryACL".into(),
                allowed_corporations: HashSet::from([corp_id]),
                ..Default::default()
            },
        );
        save_ext_state(snap, &ext);
    });

    let mut sess = SessionState {
        user_id: Some(9601),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        station_id: Some(station_id),
        solar_system_id: Some(30_000_142),
        ship_id: Some(ship_id),
        ..Default::default()
    };

    // 1. GetQuotes empty summary dict and 2-arg tuple quote
    let summary = handle(&ctx, &mut sess, "reprocessingSvc", "GetQuotes", &[], None)
        .await
        .expect("handled")
        .expect("summary quote");
    assert!(matches!(summary, PyRep::KeyVal(_)));

    let tuple_quote = handle(
        &ctx,
        &mut sess,
        "reprocessingSvc",
        "GetQuotes",
        &[PyRep::List(vec![PyRep::Integer(ore_stack_id)]), PyRep::Integer(ship_id)],
        None,
    )
    .await
    .expect("handled")
    .expect("tuple quote");
    assert!(matches!(tuple_quote, PyRep::Tuple(ref t) if t.len() == 3));

    // V24 client sends Dict {itemID: invItem} to GetQuotes; quote must return item typeID and outputs
    let dict_quote = handle(
        &ctx,
        &mut sess,
        "reprocessingSvc",
        "GetQuotes",
        &[
            PyRep::Dict(vec![(
                PyRep::Integer(ore_stack_id),
                PyRep::keyval([("itemID", PyRep::Integer(ore_stack_id))]),
            )]),
            PyRep::Integer(ship_id),
        ],
        None,
    )
    .await
    .expect("handled")
    .expect("dict quote");
    let PyRep::Tuple(ref dq_parts) = dict_quote else {
        panic!("expected 3-tuple from GetQuotes Dict, got {dict_quote:?}");
    };
    let PyRep::List(ref dq_list) = dq_parts[2] else {
        panic!("expected quote list");
    };
    assert_eq!(dq_list.len(), 1);
    assert_eq!(dq_list[0].dict_get("itemID").and_then(|v| v.as_i64()), Some(ore_stack_id));
    assert_eq!(dq_list[0].dict_get("typeID").and_then(|v| v.as_i64()), Some(1230));
    assert!(!matches!(dq_list[0].dict_get("outputs"), Some(PyRep::List(o)) if o.is_empty()));

    // 2. Reprocess active ship rejected
    let err = handle(&ctx, &mut sess, "reprocessingSvc", "Reprocess", &[PyRep::Integer(ship_id)], None)
        .await
        .expect("handled")
        .expect_err("cannot reprocess active ship");
    assert_eq!(err.code, "CannotReprocessActiveShip");

    // 3. Reprocess singleton item rejected
    let err = handle(&ctx, &mut sess, "reprocessingSvc", "Reprocess", &[PyRep::Integer(singleton_item_id)], None)
        .await
        .expect("handled")
        .expect_err("unrepackaged");
    assert_eq!(err.code, "ItemMustBeRepackaged");

    // 4. Reprocess insufficient portion rejected
    let err = handle(&ctx, &mut sess, "reprocessingSvc", "Reprocess", &[PyRep::Integer(small_stack_id)], None)
        .await
        .expect("handled")
        .expect_err("portion too small");
    assert_eq!(err.code, "InsufficientBatchPortion");

    // 5. Reprocess 350 Veldspar -> leaves 50 remainder in stack and outputs Tritanium (34)
    let out = handle(
        &ctx,
        &mut sess,
        "reprocessingSvc",
        "Reprocess",
        &[PyRep::Integer(ore_stack_id)],
        None,
    )
    .await
    .expect("handled")
    .expect("reprocess succeeds");
    assert!(matches!(out, PyRep::KeyVal(_)));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&ore_stack_id].quantity, 50);
        assert!(snap.items.values().any(|i| i.owner_id == char_id && i.type_id == 34 && i.quantity > 0));
    });

    // 6. Undocked Reprocess rejected
    let mut space_sess = sess.clone();
    space_sess.station_id = None;
    let err = handle(&ctx, &mut space_sess, "reprocessingSvc", "Reprocess", &[PyRep::Integer(ore_stack_id)], None)
        .await
        .expect("handled")
        .expect_err("must be docked");
    assert_eq!(err.code, "NotDocked");

    // 7. Structure with offline reprocessing service -> ReprocessingServiceOffline
    let mut struct_sess = SessionState {
        user_id: Some(9601),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        station_id: None,
        structure_id: Some(structure_id),
        solar_system_id: Some(30_000_142),
        ship_id: Some(ship_id),
        ..Default::default()
    };
    let err = handle(
        &ctx,
        &mut struct_sess,
        "reprocessingSvc",
        "Reprocess",
        &[PyRep::List(vec![PyRep::Integer(ore_stack_id)])],
        None,
    )
    .await
    .expect("handled")
    .expect_err("service offline");
    assert_eq!(err.code, "ReprocessingServiceOffline");

    // 8. CmdMineRepeatedly validation: missing asteroidID rejected
    let err = handle(
        &ctx,
        &mut space_sess,
        "beyonce",
        "CmdMineRepeatedly",
        &[PyRep::Integer(0)],
        None,
    )
    .await
    .expect("handled")
    .expect_err("invalid target asteroid");
    assert_eq!(err.code, "InvalidArgument");
    let _ = (DVec3::ZERO, HashMap::<String, PyRep>::new());
}
