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

#[test]
fn crate_service_error_paths_multi_qty_and_skill_injection() {
    let ctx = test_ctx();
    let cid = 90_000_001_i64;
    let other_cid = 90_000_002_i64;
    let station_id = 60_003_760_i64;
    let mut sess = auth_session(1, cid, 98_000_001, 30_000_142);
    sess.station_id = Some(station_id);

    let specs = get_crate_specs(&ctx);
    let (&loot_crate_tid, _) = specs
        .iter()
        .find(|(_, s)| !s.items.is_empty())
        .expect("fixedLootItems crate");

    // Invalid crate_id <= 0 across all handlers
    for (m, args) in [
        ("OpenCrate", vec![PyRep::Integer(0)]),
        ("ClaimLoot", vec![PyRep::Integer(-1)]),
        ("GetCrateLocation", vec![PyRep::Integer(0)]),
        ("PeekInsideFixedCrate", vec![PyRep::Integer(0)]),
        ("ClaimFitting", vec![PyRep::Integer(0), PyRep::Integer(1)]),
        ("ClaimLootFromCrateStack", vec![PyRep::Integer(0)]),
        ("ClaimSkillBundle", vec![PyRep::Integer(0), PyRep::Integer(1)]),
    ] {
        assert!(handle_crate_service(&ctx, &sess, m, &args).unwrap().is_err());
    }

    // Missing crate -> CrateNotFound
    assert_eq!(
        handle_crate_service(&ctx, &sess, "OpenCrate", &[PyRep::Integer(999_999_888)])
            .unwrap()
            .unwrap_err()
            .code,
        "CrateNotFound"
    );

    // Pick a published skill typeID (category 16) from SDE
    let skill_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.category_id == 16 && t.published)
        .map(|t| t.type_id)
        .expect("published skill type");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.station_id = Some(station_id);
        }
        // Unowned crate
        snap.items.insert(
            931_001,
            InventoryItemRecord {
                item_id: 931_001,
                type_id: loot_crate_tid,
                owner_id: other_cid,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                ..Default::default()
            },
        );
        // Non-crate item (Tritanium)
        snap.items.insert(
            931_002,
            InventoryItemRecord {
                item_id: 931_002,
                type_id: 34,
                owner_id: cid,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                ..Default::default()
            },
        );
        // Stack of 2 loot crates for ClaimLoot qty>1 decrement branch
        snap.items.insert(
            931_003,
            InventoryItemRecord {
                item_id: 931_003,
                type_id: loot_crate_tid,
                owner_id: cid,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 2,
                ..Default::default()
            },
        );
        // Crate in different station -> InvalidCrateLocation
        snap.items.insert(
            931_004,
            InventoryItemRecord {
                item_id: 931_004,
                type_id: loot_crate_tid,
                owner_id: cid,
                location_id: 60_003_761,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                ..Default::default()
            },
        );
    });

    assert_eq!(
        handle_crate_service(&ctx, &sess, "OpenCrate", &[PyRep::Integer(931_001)])
            .unwrap()
            .unwrap_err()
            .code,
        "CrateNotFound"
    );
    assert_eq!(
        handle_crate_service(&ctx, &sess, "OpenCrate", &[PyRep::Integer(931_002)])
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidCrateType"
    );
    assert_eq!(
        handle_crate_service(&ctx, &sess, "OpenCrate", &[PyRep::Integer(931_004)])
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidCrateLocation"
    );

    // Open 931_003 (qty=2), test wrong flag on ClaimLoot, then ClaimLoot decrements qty to 1
    handle_crate_service(&ctx, &sess, "OpenCrate", &[PyRep::Integer(931_003)])
        .unwrap()
        .unwrap();
    assert_eq!(
        handle_crate_service(
            &ctx,
            &sess,
            "ClaimLoot",
            &[PyRep::Integer(931_003), PyRep::Integer(station_id), PyRep::Integer(5)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidCrateLocation"
    );
    // ClaimFitting when crate has no fitting
    assert_eq!(
        handle_crate_service(
            &ctx,
            &sess,
            "ClaimFitting",
            &[PyRep::Integer(931_003), PyRep::Integer(1)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidCrateFitting"
    );
    // ClaimSkillBundle when crate has no skill bundle (or mismatched bundle)
    assert!(handle_crate_service(
        &ctx,
        &sess,
        "ClaimSkillBundle",
        &[PyRep::Integer(931_003), PyRep::Integer(1)],
    )
    .unwrap()
    .is_err());

    let claim1 = handle_crate_service(&ctx, &sess, "ClaimLoot", &[PyRep::Integer(931_003)])
        .unwrap()
        .unwrap();
    assert_eq!(claim1.dict_get("crateID"), Some(&PyRep::Integer(931_003)));
    assert_eq!(
        ctx.db.with_snapshot(|s| s.items.get(&931_003).map(|i| i.quantity)),
        Some(1)
    );

    // Deplete stack of 1 via ClaimLootFromCrateStack
    let stack_done = handle_crate_service(
        &ctx,
        &sess,
        "ClaimLootFromCrateStack",
        &[PyRep::Integer(931_003), PyRep::Integer(5)],
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        stack_done,
        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(5)])
    );
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&931_003)));

    // Cover Fitting & SkillBundle error paths + multi-qty if present in SDE
    if let Some((&fit_tid, fit_spec)) = specs
        .iter()
        .find(|(_, s)| s.fitting_ship_type_id.is_some() && !s.fittings.is_empty())
    {
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                931_010,
                InventoryItemRecord {
                    item_id: 931_010,
                    type_id: fit_tid,
                    owner_id: cid,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 2,
                    ..Default::default()
                },
            );
        });
        // Not opened yet
        assert_eq!(
            handle_crate_service(
                &ctx,
                &sess,
                "ClaimFitting",
                &[PyRep::Integer(931_010), PyRep::Integer(fit_spec.fittings[0] as i64)],
            )
            .unwrap()
            .unwrap_err()
            .code,
            "CrateNotOpened"
        );
        handle_crate_service(&ctx, &sess, "OpenCrate", &[PyRep::Integer(931_010)])
            .unwrap()
            .unwrap();
        // Wrong fitting ID
        assert_eq!(
            handle_crate_service(
                &ctx,
                &sess,
                "ClaimFitting",
                &[PyRep::Integer(931_010), PyRep::Integer(999_999_111)],
            )
            .unwrap()
            .unwrap_err()
            .code,
            "InvalidCrateFitting"
        );
        // Valid fitting claim on qty=2 decrements to 1
        handle_crate_service(
            &ctx,
            &sess,
            "ClaimFitting",
            &[PyRep::Integer(931_010), PyRep::Integer(fit_spec.fittings[0] as i64)],
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            ctx.db.with_snapshot(|s| s.items.get(&931_010).map(|i| i.quantity)),
            Some(1)
        );
    }

    if let Some((&sb_tid, sb_spec)) = specs.iter().find(|(_, s)| s.skill_bundle.is_some()) {
        let (bid, _) = sb_spec.skill_bundle.unwrap();
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                931_020,
                InventoryItemRecord {
                    item_id: 931_020,
                    type_id: sb_tid,
                    owner_id: cid,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 2,
                    ..Default::default()
                },
            );
        });
        assert_eq!(
            handle_crate_service(
                &ctx,
                &sess,
                "ClaimSkillBundle",
                &[PyRep::Integer(931_020), PyRep::Integer(bid)],
            )
            .unwrap()
            .unwrap_err()
            .code,
            "CrateNotOpened"
        );
        handle_crate_service(&ctx, &sess, "OpenCrate", &[PyRep::Integer(931_020)])
            .unwrap()
            .unwrap();
        assert_eq!(
            handle_crate_service(
                &ctx,
                &sess,
                "ClaimSkillBundle",
                &[PyRep::Integer(931_020), PyRep::Integer(bid + 9999)],
            )
            .unwrap()
            .unwrap_err()
            .code,
            "InvalidSkillBundle"
        );
        handle_crate_service(
            &ctx,
            &sess,
            "ClaimSkillBundle",
            &[PyRep::Integer(931_020), PyRep::Integer(bid)],
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            ctx.db.with_snapshot(|s| s.items.get(&931_020).map(|i| i.quantity)),
            Some(1)
        );
    }

    // Also test ClaimLoot when character is undocked -> DockingRequired
    let mut undocked_sess = sess.clone();
    undocked_sess.station_id = None;
    undocked_sess.structure_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.station_id = None;
            ch.structure_id = None;
        }
        let mut ext = load_cosmetics_ui_state(snap);
        ext.opened_crates.insert(931_030, 1);
        save_cosmetics_ui_state(snap, &ext);
    });
    assert_eq!(
        handle_crate_service(&ctx, &undocked_sess, "ClaimLoot", &[PyRep::Integer(931_030)])
            .unwrap()
            .unwrap_err()
            .code,
        "DockingRequired"
    );

    // Additional branch coverage: unauthenticated session, quantity>1 ClaimLoot & ClaimSkillBundle & ClaimFitting,
    // ClaimLootFromCrateStack invalid crate type / NoCratesToOpen / partial stack, already-claimed errors.
    let unauth_sess = SessionState::default();
    for (m, args) in [
        ("OpenCrate", vec![PyRep::Integer(1)]),
        ("ClaimLoot", vec![PyRep::Integer(1)]),
        ("GetCrateLocation", vec![PyRep::Integer(1)]),
        ("PeekInsideFixedCrate", vec![PyRep::Integer(1)]),
        ("ClaimFitting", vec![PyRep::Integer(1), PyRep::Integer(1)]),
        ("ClaimLootFromCrateStack", vec![PyRep::Integer(1)]),
        ("ClaimSkillBundle", vec![PyRep::Integer(1), PyRep::Integer(1)]),
    ] {
        assert_eq!(
            handle_crate_service(&ctx, &unauth_sess, m, &args).unwrap().unwrap_err().code,
            "Unauthorized"
        );
    }

    let specs = get_crate_specs(&ctx);
    let (&loot_tid, _) = specs.iter().find(|(_, s)| !s.items.is_empty()).unwrap();
    let fit_only_opt = specs.iter().find_map(|(&tid, s)| {
        if s.items.is_empty() && s.fitting_ship_type_id.is_some() && !s.fittings.is_empty() {
            Some((tid, s.fittings[0]))
        } else {
            None
        }
    });
    let bundle_opt = specs.iter().find_map(|(&tid, s)| s.skill_bundle.map(|(bid, _)| (tid, bid)));
    let station_id = 60_003_760_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.station_id = Some(station_id);
        }
        snap.items.insert(931_040, InventoryItemRecord {
            item_id: 931_040, type_id: loot_tid, owner_id: cid, location_id: station_id,
            flag: InventoryFlag::Hangar, quantity: 3, ..Default::default()
        });
        snap.items.insert(931_041, InventoryItemRecord {
            item_id: 931_041, type_id: loot_tid, owner_id: cid, location_id: station_id,
            flag: InventoryFlag::Hangar, quantity: 0, ..Default::default()
        });
        let mut ext = load_cosmetics_ui_state(snap);
        ext.opened_crates.insert(931_040, 1);
        ext.claimed_crate_loot.entry(cid).or_default().push(931_042);
        ext.opened_crates.insert(931_042, 1);
        save_cosmetics_ui_state(snap, &ext);
        snap.items.insert(931_042, InventoryItemRecord {
            item_id: 931_042, type_id: loot_tid, owner_id: cid, location_id: station_id,
            flag: InventoryFlag::Hangar, quantity: 1, ..Default::default()
        });
    });
    assert!(handle_crate_service(&ctx, &sess, "ClaimLoot", &[PyRep::Integer(931_040)]).unwrap().is_ok());
    assert_eq!(handle_crate_service(&ctx, &sess, "ClaimLoot", &[PyRep::Integer(931_042)]).unwrap().unwrap_err().code, "LootAlreadyClaimed");
    assert!(handle_crate_service(&ctx, &sess, "ClaimLootFromCrateStack", &[PyRep::Integer(931_040), PyRep::Integer(1)]).unwrap().is_ok());
    assert_eq!(handle_crate_service(&ctx, &sess, "ClaimLootFromCrateStack", &[PyRep::Integer(931_041), PyRep::Integer(1)]).unwrap().unwrap_err().code, "InvalidCrateLocation");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) { ch.station_id = None; ch.structure_id = None; }
    });
    assert_eq!(handle_crate_service(&ctx, &undocked_sess, "ClaimLootFromCrateStack", &[PyRep::Integer(931_040), PyRep::Integer(1)]).unwrap().unwrap_err().code, "DockingRequired");

    if let Some((fit_tid, fit_id)) = fit_only_opt.or_else(|| specs.iter().find_map(|(&tid, s)| if s.fitting_ship_type_id.is_some() && !s.fittings.is_empty() { Some((tid, s.fittings[0])) } else { None })) {
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(931_043, InventoryItemRecord {
                item_id: 931_043, type_id: fit_tid, owner_id: cid, location_id: station_id,
                flag: InventoryFlag::Hangar, quantity: 2, ..Default::default()
            });
            let mut ext = load_cosmetics_ui_state(snap);
            ext.opened_crates.insert(931_043, 1);
            ext.claimed_crate_loot.entry(cid).or_default().push(931_043_i64 * 10_000 + i64::from(fit_id));
            save_cosmetics_ui_state(snap, &ext);
        });
        assert_eq!(handle_crate_service(&ctx, &undocked_sess, "ClaimFitting", &[PyRep::Integer(931_043), PyRep::Integer(i64::from(fit_id))]).unwrap().unwrap_err().code, "DockingRequired");
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&cid) { ch.station_id = Some(station_id); }
        });
        assert_eq!(handle_crate_service(&ctx, &sess, "ClaimFitting", &[PyRep::Integer(931_043), PyRep::Integer(i64::from(fit_id))]).unwrap().unwrap_err().code, "FittingAlreadyClaimed");
        ctx.db.with_mut_snapshot(|snap| {
            let mut ext = load_cosmetics_ui_state(snap);
            ext.claimed_crate_loot.remove(&cid);
            save_cosmetics_ui_state(snap, &ext);
        });
        assert!(handle_crate_service(&ctx, &sess, "ClaimFitting", &[PyRep::Integer(931_043), PyRep::Integer(i64::from(fit_id))]).unwrap().is_ok());
        if specs.get(&fit_tid).is_some_and(|s| s.items.is_empty()) {
            assert_eq!(handle_crate_service(&ctx, &sess, "ClaimLootFromCrateStack", &[PyRep::Integer(931_043), PyRep::Integer(1)]).unwrap().unwrap_err().code, "InvalidCrateType");
        }
    }
    if let Some((b_tid, b_id)) = bundle_opt {
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(931_044, InventoryItemRecord {
                item_id: 931_044, type_id: b_tid, owner_id: cid, location_id: station_id,
                flag: InventoryFlag::Hangar, quantity: 2, ..Default::default()
            });
            let mut ext = load_cosmetics_ui_state(snap);
            ext.opened_crates.insert(931_044, 1);
            save_cosmetics_ui_state(snap, &ext);
            if let Some(ch) = snap.characters.get_mut(&cid) { ch.station_id = None; ch.structure_id = None; }
        });
        assert_eq!(handle_crate_service(&ctx, &undocked_sess, "ClaimSkillBundle", &[PyRep::Integer(931_044), PyRep::Integer(b_id)]).unwrap().unwrap_err().code, "DockingRequired");
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&cid) { ch.station_id = Some(station_id); }
        });
        assert!(handle_crate_service(&ctx, &sess, "ClaimSkillBundle", &[PyRep::Integer(931_044), PyRep::Integer(b_id)]).unwrap().is_ok());
        ctx.db.with_mut_snapshot(|snap| {
            let mut ext = load_cosmetics_ui_state(snap);
            ext.opened_crates.insert(931_044, 1);
            ext.claimed_crate_loot.entry(cid).or_default().push(931_044_i64 * 1000 + b_id);
            save_cosmetics_ui_state(snap, &ext);
        });
        assert_eq!(handle_crate_service(&ctx, &sess, "ClaimSkillBundle", &[PyRep::Integer(931_044), PyRep::Integer(b_id)]).unwrap().unwrap_err().code, "SkillBundleAlreadyClaimed");
    }
    let _ = skill_tid;
}

#[test]
fn crate_service_error_branches_notifications_and_stack_edge_cases() {
    let ctx = test_ctx();
    let cid = 90_000_001_i64;
    let other_cid = 90_000_002_i64;
    let station_id = 60_003_760_i64;
    let other_station_id = 60_003_766_i64;
    let ship_id = 100_000_001_i64;
    let mut sess = auth_session(1, cid, 98_000_001, 30_000_142);
    sess.station_id = Some(station_id);

    let specs = get_crate_specs(&ctx);
    let char_uid = ctx.db.with_snapshot(|s| s.characters[&cid].user_id);
    let _ = ctx.db.acquire_online_lock(char_uid, 501, Some(cid), false, ctx.now_unix());
    let (&loot_crate_tid, _) = specs
        .iter()
        .find(|(_, s)| !s.items.is_empty() && s.skill_bundle.is_none())
        .expect("SDE crate with fixedLootItems");

    // 1. Unauthenticated rejected
    let unauth = SessionState::default();
    for m in [
        "OpenCrate",
        "ClaimLoot",
        "ClaimSkillBundle",
        "ClaimFitting",
        "ClaimLootFromCrateStack",
        "GetCrateLocation",
        "PeekInsideFixedCrate",
    ] {
        assert!(handle_crate_service(&ctx, &unauth, m, &[PyRep::Integer(1)])
            .unwrap()
            .is_err());
    }

    // 2. Non-positive or missing crate IDs
    for m in [
        "OpenCrate",
        "ClaimLoot",
        "ClaimLootFromCrateStack",
        "GetCrateLocation",
        "PeekInsideFixedCrate",
    ] {
        assert_eq!(
            handle_crate_service(&ctx, &sess, m, &[PyRep::Integer(0)])
                .unwrap()
                .unwrap_err()
                .code,
            "CrateNotFound"
        );
    }
    assert_eq!(
        handle_crate_service(&ctx, &sess, "ClaimFitting", &[PyRep::Integer(0), PyRep::Integer(1)])
            .unwrap()
            .unwrap_err()
            .code,
        "CrateNotFound"
    );
    assert_eq!(
        handle_crate_service(
            &ctx,
            &sess,
            "ClaimSkillBundle",
            &[PyRep::Integer(1), PyRep::Integer(0)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidSkillBundle"
    );

    // 3. Non-existent crate, wrong owner, unreachable location, non-crate item type
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.station_id = Some(station_id);
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            931_001,
            InventoryItemRecord {
                item_id: 931_001,
                type_id: loot_crate_tid,
                owner_id: other_cid,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            931_002,
            InventoryItemRecord {
                item_id: 931_002,
                type_id: loot_crate_tid,
                owner_id: cid,
                location_id: other_station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            931_003,
            InventoryItemRecord {
                item_id: 931_003,
                type_id: 34, // Tritanium, not a crate
                owner_id: cid,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                ..Default::default()
            },
        );
        // Stack of 2 crates in ship cargo for undocked GetCrateLocation + DockingRequired checks
        snap.items.insert(
            931_004,
            InventoryItemRecord {
                item_id: 931_004,
                type_id: loot_crate_tid,
                owner_id: cid,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 2,
                ..Default::default()
            },
        );
        // Zero-quantity stack in hangar for NoCratesToOpen
        snap.items.insert(
            931_005,
            InventoryItemRecord {
                item_id: 931_005,
                type_id: loot_crate_tid,
                owner_id: cid,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 0,
                ..Default::default()
            },
        );
        // Stack of 2 crates in hangar for quantity > 1 ClaimLoot & over-requested ClaimLootFromCrateStack
        snap.items.insert(
            931_006,
            InventoryItemRecord {
                item_id: 931_006,
                type_id: loot_crate_tid,
                owner_id: cid,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 2,
                ..Default::default()
            },
        );
    });

    assert_eq!(
        handle_crate_service(&ctx, &sess, "OpenCrate", &[PyRep::Integer(999_999)])
            .unwrap()
            .unwrap_err()
            .code,
        "CrateNotFound"
    );
    assert_eq!(
        handle_crate_service(&ctx, &sess, "OpenCrate", &[PyRep::Integer(931_001)])
            .unwrap()
            .unwrap_err()
            .code,
        "CrateNotFound"
    );
    assert_eq!(
        handle_crate_service(&ctx, &sess, "OpenCrate", &[PyRep::Integer(931_002)])
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidCrateLocation"
    );
    assert_eq!(
        handle_crate_service(&ctx, &sess, "OpenCrate", &[PyRep::Integer(931_003)])
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidCrateType"
    );

    // 4. Undocked checks: DockingRequired on ClaimLoot / ClaimSkillBundle / ClaimFitting / ClaimLootFromCrateStack,
    //    and GetCrateLocation falls back to crate_item.location_id (ship_id).
    let mut space_sess = auth_session(1, cid, 98_000_001, 30_000_142);
    space_sess.station_id = None;
    space_sess.structure_id = None;
    space_sess.ship_id = Some(ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.station_id = None;
            ch.structure_id = None;
        }
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.location_id = 30_000_142;
            ship.flag = InventoryFlag::None;
        }
    });

    assert_eq!(
        handle_crate_service(&ctx, &space_sess, "GetCrateLocation", &[PyRep::Integer(931_004)])
            .unwrap()
            .unwrap(),
        PyRep::Tuple(vec![PyRep::Integer(ship_id), PyRep::Integer(4)])
    );
    // Open crate in space cargo -> emits OnCrateLootSpawned
    let open_space = handle_crate_service(&ctx, &space_sess, "OpenCrate", &[PyRep::Integer(931_004)])
        .unwrap()
        .unwrap();
    assert_eq!(open_space.dict_get("opened"), Some(&PyRep::Bool(true)));
    let notifs = ctx.drain_notifications_for_char(cid);
    assert!(notifs.iter().any(|n| n.event == "OnCrateLootSpawned"));

    for m in ["ClaimLoot", "ClaimLootFromCrateStack"] {
        assert_eq!(
            handle_crate_service(&ctx, &space_sess, m, &[PyRep::Integer(931_004)])
                .unwrap()
                .unwrap_err()
                .code,
            "DockingRequired"
        );
    }
    assert_eq!(
        handle_crate_service(
            &ctx,
            &space_sess,
            "ClaimFitting",
            &[PyRep::Integer(931_004), PyRep::Integer(1)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "DockingRequired"
    );
    assert_eq!(
        handle_crate_service(
            &ctx,
            &space_sess,
            "ClaimSkillBundle",
            &[PyRep::Integer(931_004), PyRep::Integer(1)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "DockingRequired"
    );

    // 5. Re-dock and test wrong flag, quantity>1 ClaimLoot, LootAlreadyClaimed, Fitting/Bundle errors, NoCratesToOpen, over-claim stack
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.station_id = Some(station_id);
        }
    });

    handle_crate_service(&ctx, &sess, "OpenCrate", &[PyRep::Integer(931_006)])
        .unwrap()
        .unwrap();
    // Wrong destination flag (5 Cargo instead of 4 Hangar) -> InvalidCrateLocation
    assert_eq!(
        handle_crate_service(
            &ctx,
            &sess,
            "ClaimLoot",
            &[
                PyRep::Integer(931_006),
                PyRep::Integer(station_id),
                PyRep::Integer(i64::from(InventoryFlag::Cargo as u16)),
            ],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidCrateLocation"
    );
    // ClaimFitting & ClaimSkillBundle on non-fitting / non-bundle crate
    assert_eq!(
        handle_crate_service(
            &ctx,
            &sess,
            "ClaimFitting",
            &[PyRep::Integer(931_006), PyRep::Integer(1)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidCrateFitting"
    );
    assert_eq!(
        handle_crate_service(
            &ctx,
            &sess,
            "ClaimSkillBundle",
            &[PyRep::Integer(931_006), PyRep::Integer(1)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidSkillBundle"
    );
    // ClaimFitting & ClaimSkillBundle on unopened crate -> CrateNotOpened
    assert_eq!(
        handle_crate_service(
            &ctx,
            &sess,
            "ClaimFitting",
            &[PyRep::Integer(931_005), PyRep::Integer(1)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CrateNotOpened"
    );
    assert_eq!(
        handle_crate_service(
            &ctx,
            &sess,
            "ClaimSkillBundle",
            &[PyRep::Integer(931_005), PyRep::Integer(1)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CrateNotOpened"
    );

    // ClaimLoot on stack of 2 decrements to quantity 1
    let c1 = handle_crate_service(&ctx, &sess, "ClaimLoot", &[PyRep::Integer(931_006)])
        .unwrap()
        .unwrap();
    assert_eq!(c1.dict_get("crateID"), Some(&PyRep::Integer(931_006)));
    assert_eq!(
        ctx.db.with_snapshot(|s| s.items.get(&931_006).map(|i| i.quantity)),
        Some(1)
    );

    // ClaimLootFromCrateStack requesting 5 when only 1 left -> opens 1 and removes item
    let stack_rem = handle_crate_service(
        &ctx,
        &sess,
        "ClaimLootFromCrateStack",
        &[PyRep::Integer(931_006), PyRep::Integer(5)],
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        stack_rem,
        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(5)])
    );
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&931_006)));

    // Pre-mark 931_008 (quantity 1) as opened & claimed -> LootAlreadyClaimed on ClaimLoot,
    // and 931_005 (quantity 0) -> NoCratesToOpen on ClaimLootFromCrateStack
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            931_008,
            InventoryItemRecord {
                item_id: 931_008,
                type_id: loot_crate_tid,
                owner_id: cid,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                ..Default::default()
            },
        );
        let mut ext = load_cosmetics_ui_state(snap);
        ext.opened_crates.insert(931_008, ctx.now_unix());
        ext.claimed_crate_loot.entry(cid).or_default().push(931_008);
        save_cosmetics_ui_state(snap, &ext);
    });
    assert_eq!(
        handle_crate_service(&ctx, &sess, "ClaimLoot", &[PyRep::Integer(931_008)])
            .unwrap()
            .unwrap_err()
            .code,
        "LootAlreadyClaimed"
    );
    assert_eq!(
        handle_crate_service(
            &ctx,
            &sess,
            "ClaimLootFromCrateStack",
            &[PyRep::Integer(931_005), PyRep::Integer(1)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidCrateLocation"
    );

    // Crate without fixedLootItems (skill-bundle-only or fitting-only) -> ClaimLootFromCrateStack returns InvalidCrateType
    if let Some((sb_only_tid, _)) = specs.iter().find(|(_, s)| s.items.is_empty() && s.skill_bundle.is_some()) {
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                931_007,
                InventoryItemRecord {
                    item_id: 931_007,
                    type_id: *sb_only_tid,
                    owner_id: cid,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    ..Default::default()
                },
            );
        });
        assert_eq!(
            handle_crate_service(
                &ctx,
                &sess,
                "ClaimLootFromCrateStack",
                &[PyRep::Integer(931_007), PyRep::Integer(1)],
            )
            .unwrap()
            .unwrap_err()
            .code,
            "InvalidCrateType"
        );
    }
}
