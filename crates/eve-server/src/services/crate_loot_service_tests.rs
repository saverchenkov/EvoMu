//! Unit tests for `crate_loot_service` (`crateService` open, peek, location, loot, stack, fitting, skill bundle).

#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::{
    auth_session, test_ctx, PyRepTestExt,
};
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_proto::PyRep;

#[test]
fn crate_service_open_peek_claim_loot_stack_and_bundles() {
    let ctx = test_ctx();
    let cid = 90_000_001_i64;
    let station_id = 60_003_760_i64;
    let mut sess = auth_session(1, cid, 98_000_001, 30_000_142);
    sess.station_id = Some(station_id);

    assert!(handle_crate_service(&ctx, &sess, "UnknownMethod", &[]).is_none());

    // Find a crate type with fixedLootItems from SDE crates.fsdbinary
    let specs = get_crate_specs(&ctx);
    let (&loot_crate_tid, _) = specs
        .iter()
        .find(|(_, s)| !s.items.is_empty())
        .expect("SDE must have a crate with fixedLootItems");

    let bundle_crate_opt = specs
        .iter()
        .find_map(|(&tid, s)| s.skill_bundle.map(|(bid, _)| (tid, bid)));
    let fitting_crate_opt = specs.iter().find_map(|(&tid, s)| {
        if s.fitting_ship_type_id.is_some() && !s.fittings.is_empty() {
            Some((tid, s.fittings[0]))
        } else {
            None
        }
    });

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.station_id = Some(station_id);
        }
        // Single crate for OpenCrate + ClaimLoot
        snap.items.insert(
            930_001,
            InventoryItemRecord {
                item_id: 930_001,
                type_id: loot_crate_tid,
                owner_id: cid,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                ..Default::default()
            },
        );
        // Stack of 3 crates for ClaimLootFromCrateStack
        snap.items.insert(
            930_002,
            InventoryItemRecord {
                item_id: 930_002,
                type_id: loot_crate_tid,
                owner_id: cid,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 3,
                ..Default::default()
            },
        );
    });

    // GetCrateLocation & PeekInsideFixedCrate
    let loc_rep = handle_crate_service(&ctx, &sess, "GetCrateLocation", &[PyRep::Integer(930_001)])
        .unwrap()
        .unwrap();
    assert_eq!(
        loc_rep,
        PyRep::Tuple(vec![PyRep::Integer(station_id), PyRep::Integer(4)])
    );
    let peek_rep = handle_crate_service(
        &ctx,
        &sess,
        "PeekInsideFixedCrate",
        &[PyRep::Integer(930_001)],
    )
    .unwrap()
    .unwrap();
    assert!(peek_rep.as_list().is_some_and(|l| !l.is_empty()));

    // ClaimLoot before OpenCrate fails with CrateNotOpened
    assert_eq!(
        handle_crate_service(&ctx, &sess, "ClaimLoot", &[PyRep::Integer(930_001)])
            .unwrap()
            .unwrap_err()
            .code,
        "CrateNotOpened"
    );

    let open_rep = handle_crate_service(&ctx, &sess, "OpenCrate", &[PyRep::Integer(930_001)])
        .unwrap()
        .unwrap();
    assert_eq!(open_rep.dict_get("opened"), Some(&PyRep::Bool(true)));

    // Wrong destination station rejected
    assert_eq!(
        handle_crate_service(
            &ctx,
            &sess,
            "ClaimLoot",
            &[PyRep::Integer(930_001), PyRep::Integer(60_009_999)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidCrateLocation"
    );

    let claim_rep = handle_crate_service(&ctx, &sess, "ClaimLoot", &[PyRep::Integer(930_001)])
        .unwrap()
        .unwrap();
    assert_eq!(claim_rep.dict_get("crateID"), Some(&PyRep::Integer(930_001)));

    // ClaimLootFromCrateStack opens 2 of 3 in stack
    let stack_rep = handle_crate_service(
        &ctx,
        &sess,
        "ClaimLootFromCrateStack",
        &[PyRep::Integer(930_002), PyRep::Integer(2)],
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        stack_rep,
        PyRep::Tuple(vec![PyRep::Integer(2), PyRep::Integer(2)])
    );
    assert_eq!(
        ctx.db.with_snapshot(|s| s.items.get(&930_002).map(|i| i.quantity)),
        Some(1)
    );

    // Optional SDE skill bundle & fitting crate tests
    if let Some((bundle_tid, bundle_id)) = bundle_crate_opt {
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                930_003,
                InventoryItemRecord {
                    item_id: 930_003,
                    type_id: bundle_tid,
                    owner_id: cid,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    ..Default::default()
                },
            );
        });
        handle_crate_service(&ctx, &sess, "OpenCrate", &[PyRep::Integer(930_003)])
            .unwrap()
            .unwrap();
        let sb_rep = handle_crate_service(
            &ctx,
            &sess,
            "ClaimSkillBundle",
            &[PyRep::Integer(930_003), PyRep::Integer(bundle_id)],
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            sb_rep.dict_get("skillBundleID"),
            Some(&PyRep::Integer(bundle_id))
        );
    }

    if let Some((fit_tid, fit_id)) = fitting_crate_opt {
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                930_004,
                InventoryItemRecord {
                    item_id: 930_004,
                    type_id: fit_tid,
                    owner_id: cid,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    ..Default::default()
                },
            );
        });
        handle_crate_service(&ctx, &sess, "OpenCrate", &[PyRep::Integer(930_004)])
            .unwrap()
            .unwrap();
        let fit_rep = handle_crate_service(
            &ctx,
            &sess,
            "ClaimFitting",
            &[PyRep::Integer(930_004), PyRep::Integer(fit_id as i64)],
        )
        .unwrap()
        .unwrap();
        assert_eq!(fit_rep.dict_get("claimed"), Some(&PyRep::Bool(true)));
    }
}
