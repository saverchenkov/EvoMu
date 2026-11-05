//! Targeted unit and mutation-killing tests for `inventory.rs`.

use super::*;

#[test]
fn move_item_transactional_guards_version_and_idempotency() {
    let repo = WorldRepository::with_demo_seed();
    let cid = 90_000_001_i64;
    let ch = repo.get_character(cid).unwrap();
    let ship_id = ch.active_ship_id;
    let station_id = ch.station_id.unwrap();

    let item = repo
        .add_or_stack_item(cid, station_id, InventoryFlag::Hangar, 38, 1_000)
        .unwrap();
    assert_eq!(repo.get_item(item.item_id).unwrap().quantity, 1_000);
    assert!(repo.get_items_for_character(cid).iter().any(|i| i.item_id == item.item_id));

    // Self-containment, active ship move, and Locked flag manipulation rejected
    assert!(repo.move_item_transactional(cid, item.item_id, item.item_id, InventoryFlag::Cargo, None, None).is_err());
    assert!(repo.move_item_transactional(cid, ship_id, station_id, InventoryFlag::Hangar, None, None).is_err());
    assert_eq!(
        repo.move_item_transactional(cid, item.item_id, ship_id, InventoryFlag::Locked, None, None),
        Err(DbError::ItemLocked(item.item_id))
    );
    // Unreachable destination rejected
    assert!(repo.move_item_transactional(cid, item.item_id, 60_009_999, InventoryFlag::Hangar, None, None).is_err());

    // Optimistic concurrency check: mismatched expected_version returns VersionConflict
    assert_eq!(
        repo.move_item_transactional(cid, item.item_id, ship_id, InventoryFlag::Cargo, Some(99), None),
        Err(DbError::VersionConflict { expected: 99, actual: item.version })
    );

    // Valid move with expected_version and idempotency_key
    let moved = repo
        .move_item_transactional(
            cid,
            item.item_id,
            ship_id,
            InventoryFlag::Cargo,
            Some(item.version),
            Some("idem-move-1"),
        )
        .unwrap();
    assert_eq!(moved.location_id, ship_id);
    assert_eq!(moved.flag, InventoryFlag::Cargo);
    assert_eq!(moved.version, item.version + 1);

    // Repeating the same idempotency_key returns the current item without bumping version
    let dup = repo
        .move_item_transactional(
            cid,
            item.item_id,
            station_id,
            InventoryFlag::Hangar,
            Some(999),
            Some("idem-move-1"),
        )
        .unwrap();
    assert_eq!(dup.version, moved.version);
    assert_eq!(dup.location_id, ship_id);
}

#[test]
fn repackage_stack_split_delete_and_add_or_stack_edge_cases() {
    let repo = WorldRepository::with_demo_seed();
    let cid = 90_000_001_i64;
    let ch = repo.get_character(cid).unwrap();
    let station_id = ch.station_id.unwrap();

    // Create a spare assembled ship with insurance to test repackage_item
    let spare_ship_id = 200_700_001_i64;
    let module_inside_id = 200_700_002_i64;
    repo.with_mut_snapshot(|snap| {
        snap.items.insert(
            spare_ship_id,
            InventoryItemRecord {
                item_id: spare_ship_id,
                type_id: 587,
                owner_id: cid,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                insured_payout_isk: 500_000.0,
                ..InventoryItemRecord::default()
            },
        );
        snap.items.insert(
            module_inside_id,
            InventoryItemRecord {
                item_id: module_inside_id,
                type_id: 440,
                owner_id: cid,
                location_id: spare_ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..InventoryItemRecord::default()
            },
        );
    });

    // Cannot repackage active ship or non-empty ship
    assert!(repo.repackage_item(cid, ch.active_ship_id).is_err());
    assert!(repo.repackage_item(cid, spare_ship_id).is_err());

    // Remove fitted module, then repackage spare ship
    repo.delete_item(cid, module_inside_id).unwrap();
    let repackaged = repo.repackage_item(cid, spare_ship_id).unwrap();
    assert!(!repackaged.singleton);
    assert_eq!(repackaged.insured_payout_isk, 0.0);

    // Split stack validation & custom name preservation
    assert!(repo.add_or_stack_item(cid, station_id, InventoryFlag::Hangar, 39, 0).is_err());
    let stack = repo.add_or_stack_item(cid, station_id, InventoryFlag::Hangar, 39, 100).unwrap();
    repo.with_mut_snapshot(|snap| {
        snap.item_custom_names.insert(stack.item_id, "Custom Pyerite".into());
    });
    assert!(repo.split_stack(cid, stack.item_id, 0).is_err());
    assert!(repo.split_stack(cid, stack.item_id, 100).is_err());
    let (orig_after, split_new) = repo.split_stack(cid, stack.item_id, 40).unwrap();
    assert_eq!(orig_after.quantity, 60);
    assert_eq!(split_new.quantity, 40);
    assert_eq!(
        repo.with_snapshot(|s| s.item_custom_names.get(&split_new.item_id).cloned()),
        Some("Custom Pyerite".into())
    );

    // Courier cargo tag prevents stacking and prevents add_or_stack_item from merging into it
    repo.with_mut_snapshot(|snap| {
        snap.item_custom_names.insert(split_new.item_id, "courier_cargo:99".into());
    });
    assert!(repo.stack_items(cid, orig_after.item_id, split_new.item_id).is_err());
    repo.with_mut_snapshot(|snap| {
        snap.item_custom_names.remove(&split_new.item_id);
    });

    // Stack items back together
    assert!(repo.stack_items(cid, orig_after.item_id, orig_after.item_id).is_err());
    let merged = repo.stack_items(cid, orig_after.item_id, split_new.item_id).unwrap();
    assert_eq!(merged.quantity, 100);
    assert!(repo.get_item(split_new.item_id).is_none());

    // Recursive delete_item removes container and nested contents
    let box_id = 200_700_010_i64;
    let inner_id = 200_700_011_i64;
    repo.with_mut_snapshot(|snap| {
        snap.items.insert(box_id, InventoryItemRecord {
            item_id: box_id, type_id: 3293, owner_id: cid, location_id: station_id,
            flag: InventoryFlag::Hangar, quantity: 1, singleton: true, version: 1, ..InventoryItemRecord::default()
        });
        snap.items.insert(inner_id, InventoryItemRecord {
            item_id: inner_id, type_id: 34, owner_id: cid, location_id: box_id,
            flag: InventoryFlag::Cargo, quantity: 50, singleton: false, version: 1, ..InventoryItemRecord::default()
        });
    });
    repo.delete_item(cid, box_id).unwrap();
    assert!(repo.get_item(box_id).is_none());
    assert!(repo.get_item(inner_id).is_none());
}
