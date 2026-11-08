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

#[test]
fn r9_p0_549_impounded_locked_and_asset_safety_blocks_db_mutations_but_allows_paid_deliveries() {
    let repo = WorldRepository::with_demo_seed();
    let cid = 90_000_001_i64;
    let station_id = repo.get_character(cid).unwrap().station_id.unwrap();

    let impounded_id = 200_700_101_i64;
    let locked_id = 200_700_102_i64;
    let wrap_box_id = 200_700_103_i64;
    let wrap_child_id = 200_700_104_i64;
    let paid_item_id = 200_700_105_i64;
    let paid_wrap_id = 200_700_199_i64;
    let unpaid_wrap_id = 200_700_198_i64;

    repo.with_mut_snapshot(|snap| {
        snap.items.insert(
            impounded_id,
            InventoryItemRecord {
                item_id: impounded_id,
                type_id: 34,
                owner_id: cid,
                location_id: station_id,
                flag: InventoryFlag::Impounded,
                quantity: 100,
                singleton: false,
                version: 1,
                ..InventoryItemRecord::default()
            },
        );
        snap.items.insert(
            locked_id,
            InventoryItemRecord {
                item_id: locked_id,
                type_id: 34,
                owner_id: cid,
                location_id: station_id,
                flag: InventoryFlag::Locked,
                quantity: 100,
                singleton: false,
                version: 1,
                ..InventoryItemRecord::default()
            },
        );
        snap.items.insert(
            wrap_box_id,
            InventoryItemRecord {
                item_id: wrap_box_id,
                type_id: 60,
                owner_id: cid,
                location_id: station_id,
                flag: InventoryFlag::Deliveries,
                quantity: 1,
                singleton: true,
                version: 1,
                ..InventoryItemRecord::default()
            },
        );
        snap.items.insert(
            wrap_child_id,
            InventoryItemRecord {
                item_id: wrap_child_id,
                type_id: 34,
                owner_id: cid,
                location_id: wrap_box_id,
                flag: InventoryFlag::Cargo,
                quantity: 50,
                singleton: false,
                version: 1,
                ..InventoryItemRecord::default()
            },
        );
        snap.items.insert(
            paid_item_id,
            InventoryItemRecord {
                item_id: paid_item_id,
                type_id: 34,
                owner_id: cid,
                location_id: station_id,
                flag: InventoryFlag::Deliveries,
                quantity: 20,
                singleton: false,
                version: 1,
                ..InventoryItemRecord::default()
            },
        );
        snap.item_custom_names.insert(
            -7_700_001,
            serde_json::json!({
                "asset_safety_wraps": {
                    unpaid_wrap_id.to_string(): {
                        "wrap_id": unpaid_wrap_id,
                        "retrieval_fee_paid": false,
                        "item_ids": [wrap_child_id]
                    },
                    paid_wrap_id.to_string(): {
                        "wrap_id": paid_wrap_id,
                        "retrieval_fee_paid": true,
                        "item_ids": [paid_item_id]
                    }
                }
            })
            .to_string(),
        );
    });

    for blocked in [impounded_id, locked_id, wrap_box_id, wrap_child_id] {
        assert_eq!(
            repo.move_item_transactional(cid, blocked, station_id, InventoryFlag::Hangar, None, None),
            Err(DbError::ItemLocked(blocked))
        );
        assert_eq!(repo.delete_item(cid, blocked), Err(DbError::ItemLocked(blocked)));
    }
    assert_eq!(
        repo.split_stack(cid, impounded_id, 10),
        Err(DbError::ItemLocked(impounded_id))
    );
    assert_eq!(
        repo.split_stack(cid, wrap_child_id, 10),
        Err(DbError::ItemLocked(wrap_child_id))
    );
    assert_eq!(
        repo.repackage_item(cid, wrap_box_id),
        Err(DbError::ItemLocked(wrap_box_id))
    );
    assert_eq!(
        repo.move_item_transactional(cid, paid_item_id, station_id, InventoryFlag::Impounded, None, None),
        Err(DbError::ItemLocked(paid_item_id))
    );

    // Paid asset safety item moved to Deliveries can legitimately move to Hangar
    let moved = repo
        .move_item_transactional(cid, paid_item_id, station_id, InventoryFlag::Hangar, None, None)
        .expect("paid asset safety wrap item should move cleanly");
    assert_eq!(moved.flag, InventoryFlag::Hangar);
}

#[test]
fn centralized_consumption_and_transfer_enforce_all_six_invariants() {
    let repo = WorldRepository::with_demo_seed();
    let cid = 90_000_001_i64;
    let other_cid = 90_000_002_i64;
    let ch = repo.get_character(cid).unwrap();
    let station_id = ch.station_id.unwrap();
    let ship_id = ch.active_ship_id;
    let corp_id = 98_000_123_i64;
    let struct_id = 1_000_000_583_001_i64;

    // Inv 1: Impounded, Locked, ALSC, Sealed Plastic Wrap, and ancestor container lock
    repo.with_mut_snapshot(|snap| {
        let locked_id = 300_000_001_i64;
        let impounded_id = 300_000_002_i64;
        let alsc_item_id = 300_000_003_i64;
        let plastic_wrap_id = 300_000_004_i64;
        let inside_wrap_id = 300_000_005_i64;
        let free_id = 300_000_006_i64;

        for &(id, flag, qty) in &[
            (locked_id, InventoryFlag::Locked, 100),
            (impounded_id, InventoryFlag::Impounded, 100),
            (alsc_item_id, InventoryFlag::Hangar, 100),
            (free_id, InventoryFlag::Hangar, 50),
        ] {
            snap.items.insert(id, InventoryItemRecord {
                item_id: id,
                type_id: 11399,
                owner_id: cid,
                location_id: station_id,
                flag,
                quantity: qty,
                singleton: false,
                version: 1,
                ..InventoryItemRecord::default()
            });
        }
        snap.items.insert(plastic_wrap_id, InventoryItemRecord {
            item_id: plastic_wrap_id,
            type_id: 3_468,
            owner_id: cid,
            location_id: station_id,
            flag: InventoryFlag::Hangar,
            quantity: 1,
            singleton: true,
            version: 1,
            ..InventoryItemRecord::default()
        });
        snap.items.insert(inside_wrap_id, InventoryItemRecord {
            item_id: inside_wrap_id,
            type_id: 11399,
            owner_id: cid,
            location_id: plastic_wrap_id,
            flag: InventoryFlag::Cargo,
            quantity: 200,
            singleton: false,
            version: 1,
            ..InventoryItemRecord::default()
        });
        snap.owner_notes.insert(-8_400_000_000_000, OwnerNoteRecord {
            note_id: -8_400_000_000_000,
            character_id: 0,
            item_id: 0,
            label: "S:CONTAINER_SHIP_RUNTIME_STATE".into(),
            note: serde_json::json!({ "alsc_locked_items": [alsc_item_id] }).to_string(),
            timestamp_unix: 100,
        });

        let opts = ConsumeExactOptions::for_character_stack(cid);
        assert!(matches!(
            snap.consume_exact_item_quantity(locked_id, 10, opts),
            Err(DbError::ItemLocked(_))
        ));
        assert!(matches!(
            snap.consume_exact_item_quantity(impounded_id, 10, opts),
            Err(DbError::ItemLocked(_))
        ));
        assert!(matches!(
            snap.consume_exact_item_quantity(alsc_item_id, 10, opts),
            Err(DbError::ItemLocked(_))
        ));
        assert!(matches!(
            snap.consume_exact_item_quantity(inside_wrap_id, 10, opts),
            Err(DbError::ItemLocked(_))
        ));
        assert!(matches!(
            snap.transfer_inventory_item(
                inside_wrap_id,
                TransferItemSpec::move_whole(Some(cid), Some(cid), None, station_id, InventoryFlag::Hangar)
            ),
            Err(DbError::ItemLocked(_))
        ));
        // Trying to consume more than the unlocked stack must leave locked/ALSC/wrap untouched
        assert!(snap.consume_owner_items(11399, 60, opts).is_err());
        assert_eq!(snap.items.get(&free_id).unwrap().quantity, 50);
        // Consuming <= unlocked amount succeeds and removes only from unlocked stack
        let touched = snap.consume_owner_items(11399, 30, opts).unwrap();
        assert_eq!(touched, vec![free_id]);
        assert_eq!(snap.items.get(&free_id).unwrap().quantity, 20);
        assert_eq!(snap.items.get(&locked_id).unwrap().quantity, 100);
        assert_eq!(snap.items.get(&impounded_id).unwrap().quantity, 100);
        assert_eq!(snap.items.get(&alsc_item_id).unwrap().quantity, 100);
    });

    // Inv 2: Active unpaid AssetSafetyWrap vs paid wrap
    repo.with_mut_snapshot(|snap| {
        let wrap_id = 300_000_100_i64;
        let wrapped_item_id = 300_000_101_i64;
        snap.items.insert(wrap_id, InventoryItemRecord {
            item_id: wrap_id,
            type_id: 60,
            owner_id: cid,
            location_id: station_id,
            flag: InventoryFlag::Hangar,
            quantity: 1,
            singleton: true,
            version: 1,
            ..InventoryItemRecord::default()
        });
        snap.items.insert(wrapped_item_id, InventoryItemRecord {
            item_id: wrapped_item_id,
            type_id: 35,
            owner_id: cid,
            location_id: wrap_id,
            flag: InventoryFlag::Cargo,
            quantity: 25,
            singleton: false,
            version: 1,
            ..InventoryItemRecord::default()
        });
        snap.item_custom_names.insert(-7_700_001, serde_json::json!({
            "asset_safety_wraps": {
                wrap_id.to_string(): {
                    "wrap_id": wrap_id,
                    "owner_id": cid,
                    "item_ids": [wrapped_item_id],
                    "retrieval_fee_paid": false
                }
            }
        }).to_string());

        let opts = ConsumeExactOptions::for_character_stack(cid);
        assert!(matches!(
            snap.consume_exact_item_quantity(wrapped_item_id, 5, opts),
            Err(DbError::ItemLocked(_))
        ));
        assert!(matches!(
            snap.transfer_inventory_item(
                wrapped_item_id,
                TransferItemSpec::move_whole(Some(cid), Some(cid), None, station_id, InventoryFlag::Hangar)
            ),
            Err(DbError::ItemLocked(_))
        ));

        // Once fee is paid and item is extracted from wrap_id container, consumption succeeds
        snap.item_custom_names.insert(-7_700_001, serde_json::json!({
            "asset_safety_wraps": {
                wrap_id.to_string(): {
                    "wrap_id": wrap_id,
                    "owner_id": cid,
                    "item_ids": [wrapped_item_id],
                    "retrieval_fee_paid": true
                }
            }
        }).to_string());
        snap.items.get_mut(&wrapped_item_id).unwrap().location_id = station_id;
        snap.items.get_mut(&wrapped_item_id).unwrap().flag = InventoryFlag::Hangar;
        snap.consume_exact_item_quantity(wrapped_item_id, 5, opts).unwrap();
        assert_eq!(snap.items.get(&wrapped_item_id).unwrap().quantity, 20);
    });

    // Inv 3 & 4: Contract/trade escrow lock, War HQ structure, active ship, fitted structure slots
    repo.with_mut_snapshot(|snap| {
        let contract_item_id = 300_000_200_i64;
        let war_hq_id = 300_000_201_i64;
        let core_id = 300_000_202_i64;
        let rig_id = 300_000_203_i64;
        let service_id = 300_000_204_i64;

        let trade_item_id = 300_000_205_i64;
        snap.items.insert(contract_item_id, InventoryItemRecord {
            item_id: contract_item_id,
            type_id: 36,
            owner_id: cid,
            location_id: station_id,
            flag: InventoryFlag::Hangar,
            quantity: 10,
            singleton: false,
            version: 1,
            ..InventoryItemRecord::default()
        });
        snap.items.insert(trade_item_id, InventoryItemRecord {
            item_id: trade_item_id,
            type_id: 36,
            owner_id: cid,
            location_id: station_id,
            flag: InventoryFlag::Hangar,
            quantity: 10,
            singleton: false,
            version: 1,
            ..InventoryItemRecord::default()
        });
        snap.contracts.insert(700_001, ContractRecord {
            contract_id: 700_001,
            issuer_id: cid,
            issuer_corp_id: 0,
            assignee_id: other_cid,
            acceptor_id: Some(other_cid),
            contract_type: "ItemExchange".into(),
            status: "Outstanding".into(),
            title: "Escrow Contract".into(),
            start_station_id: station_id,
            end_station_id: station_id,
            item_ids: vec![contract_item_id],
            expiration_unix: u64::MAX,
            ..ContractRecord::default()
        });
        snap.owner_notes.insert(73_000_001, OwnerNoteRecord {
            note_id: 73_000_001,
            character_id: -73,
            item_id: 0,
            label: "S:DIRECT_TRADE".into(),
            note: serde_json::json!({
                "trade_id": 73_000_001_i64,
                "items_a": [[trade_item_id, 10]],
                "items_b": [],
                "completed": false,
                "cancelled": false
            }).to_string(),
            timestamp_unix: 100,
        });

        let opts = ConsumeExactOptions::for_character_stack(cid);
        assert!(matches!(
            snap.consume_exact_item_quantity(contract_item_id, 5, opts),
            Err(DbError::ItemLocked(_))
        ));
        assert!(matches!(
            snap.consume_exact_item_quantity(trade_item_id, 5, opts),
            Err(DbError::ItemLocked(_))
        ));
        // Direct trade settlement using exclude_trade_lock_id succeeds
        let mut trade_spec = TransferItemSpec::move_whole(
            None,
            Some(cid),
            Some(other_cid),
            station_id,
            InventoryFlag::Hangar,
        );
        trade_spec.exclude_trade_lock_id = Some(73_000_001);
        let traded = snap.transfer_inventory_item(trade_item_id, trade_spec).unwrap();
        assert_eq!(traded.owner_id, other_cid);

        // War HQ structure cannot be consumed or transferred
        snap.items.insert(war_hq_id, InventoryItemRecord {
            item_id: war_hq_id,
            type_id: 35832,
            owner_id: cid,
            location_id: station_id,
            flag: InventoryFlag::Hangar,
            quantity: 1,
            singleton: true,
            version: 1,
            ..InventoryItemRecord::default()
        });
        snap.war_state.wars.insert(700_001, WarMetaAlias {
            war_hq_id: Some(war_hq_id),
            open_for_allies: false,
            finish_at_unix: None,
        });
        assert!(matches!(
            snap.consume_exact_item_quantity(
                war_hq_id,
                1,
                ConsumeExactOptions::for_owner_exact(Some(cid), cid)
            ),
            Err(DbError::ItemLocked(_))
        ));

        // Boarded active ship cannot be consumed/transferred
        assert!(snap
            .consume_exact_item_quantity(
                ship_id,
                1,
                ConsumeExactOptions {
                    active_ship_id: Some(ship_id),
                    ..ConsumeExactOptions::for_owner_exact(Some(cid), cid)
                }
            )
            .is_err());

        // Inv 4: Fitted structure slots (QuantumCoreRoom, RigSlot0, ServiceSlot0)
        for &(id, flag) in &[
            (core_id, InventoryFlag::QuantumCoreRoom),
            (rig_id, InventoryFlag::RigSlot0),
            (service_id, InventoryFlag::ServiceSlot0),
        ] {
            snap.items.insert(id, InventoryItemRecord {
                item_id: id,
                type_id: 56201,
                owner_id: cid,
                location_id: struct_id,
                flag,
                quantity: 1,
                singleton: false,
                version: 1,
                ..InventoryItemRecord::default()
            });
            assert!(snap
                .consume_exact_item_quantity(id, 1, ConsumeExactOptions::for_character_stack(cid))
                .is_err());
            assert!(snap
                .transfer_inventory_item(
                    id,
                    TransferItemSpec::move_whole(Some(cid), Some(cid), None, station_id, InventoryFlag::Hangar)
                )
                .is_err());
        }
    });

    // Inv 5: Assembled singleton and non-empty container
    repo.with_mut_snapshot(|snap| {
        let assembled_item = 300_000_300_i64;
        let empty_box = 300_000_301_i64;
        let box_content = 300_000_302_i64;

        snap.items.insert(assembled_item, InventoryItemRecord {
            item_id: assembled_item,
            type_id: 2454,
            owner_id: cid,
            location_id: station_id,
            flag: InventoryFlag::Hangar,
            quantity: 1,
            singleton: true,
            version: 1,
            ..InventoryItemRecord::default()
        });
        assert!(snap
            .consume_exact_item_quantity(assembled_item, 1, ConsumeExactOptions::for_character_stack(cid))
            .is_err());

        snap.items.insert(empty_box, InventoryItemRecord {
            item_id: empty_box,
            type_id: 3293,
            owner_id: cid,
            location_id: station_id,
            flag: InventoryFlag::Hangar,
            quantity: 1,
            singleton: true,
            version: 1,
            ..InventoryItemRecord::default()
        });
        snap.items.insert(box_content, InventoryItemRecord {
            item_id: box_content,
            type_id: 34,
            owner_id: cid,
            location_id: empty_box,
            flag: InventoryFlag::Cargo,
            quantity: 5,
            singleton: false,
            version: 1,
            ..InventoryItemRecord::default()
        });
        let mut container_opts = ConsumeExactOptions::for_owner_exact(Some(cid), cid);
        container_opts.reject_non_empty_container = true;
        assert!(snap.consume_exact_item_quantity(empty_box, 1, container_opts).is_err());
        let mut transfer_box = TransferItemSpec::move_whole(
            Some(cid),
            Some(cid),
            Some(other_cid),
            station_id,
            InventoryFlag::Hangar,
        );
        transfer_box.reject_non_empty_container = true;
        assert!(snap.transfer_inventory_item(empty_box, transfer_box).is_err());
    });

    // Inv 6: Matured vs unmatured 24h corp roles, container role, and structure ACL denial
    repo.with_mut_snapshot(|snap| {
        let ceo_id = 90_000_099_i64;
        let member_id = cid;
        let sag1_item = 300_000_400_i64;
        let sag1_box = 300_000_401_i64;
        let sag1_nested_item = 300_000_402_i64;
        let struct_item = 300_000_403_i64;

        snap.corporations.insert(corp_id, CorporationRecord {
            corporation_id: corp_id,
            name: "Test Corp".into(),
            ticker: "TST".into(),
            ceo_id,
            hq_station_id: Some(station_id),
            members: HashSet::from([ceo_id, member_id]),
            member_roles: HashMap::new(),
            ..CorporationRecord::default()
        });
        // Unmatured 24h pending grant must NOT confer take access yet
        snap.corp_governance
            .entry(corp_id)
            .or_default()
            .pending_role_grants
            .insert(member_id, (corp_roles::ROLE_HANGAR_CAN_TAKE[0], 1000 + 86_400));

        snap.items.insert(sag1_item, InventoryItemRecord {
            item_id: sag1_item,
            type_id: 37,
            owner_id: corp_id,
            location_id: struct_id,
            flag: InventoryFlag::CorpSAG1,
            quantity: 50,
            singleton: false,
            version: 1,
            ..InventoryItemRecord::default()
        });
        snap.items.insert(sag1_box, InventoryItemRecord {
            item_id: sag1_box,
            type_id: 3293,
            owner_id: corp_id,
            location_id: struct_id,
            flag: InventoryFlag::CorpSAG1,
            quantity: 1,
            singleton: true,
            version: 1,
            ..InventoryItemRecord::default()
        });
        snap.items.insert(sag1_nested_item, InventoryItemRecord {
            item_id: sag1_nested_item,
            type_id: 37,
            owner_id: corp_id,
            location_id: sag1_box,
            flag: InventoryFlag::Cargo,
            quantity: 50,
            singleton: false,
            version: 1,
            ..InventoryItemRecord::default()
        });

        let corp_opts = ConsumeExactOptions {
            actor_char_id: Some(member_id),
            expected_owner_id: corp_id,
            ..ConsumeExactOptions::for_character_stack(member_id)
        };
        // Denied while only pending 24h grant exists
        assert!(snap.consume_exact_item_quantity(sag1_item, 10, corp_opts).is_err());

        // Grant matured ROLE_HANGAR_CAN_TAKE[0], without ROLE_CONTAINER_CAN_TAKE[0]
        snap.corporations
            .get_mut(&corp_id)
            .unwrap()
            .member_roles
            .insert(member_id, corp_roles::ROLE_HANGAR_CAN_TAKE[0]);
        assert!(snap.consume_exact_item_quantity(sag1_nested_item, 10, corp_opts).is_err());
        snap.consume_exact_item_quantity(sag1_item, 10, corp_opts).unwrap();
        assert_eq!(snap.items.get(&sag1_item).unwrap().quantity, 40);

        // Grant ROLE_CONTAINER_CAN_TAKE[0] -> nested item succeeds
        snap.corporations
            .get_mut(&corp_id)
            .unwrap()
            .member_roles
            .insert(
                member_id,
                corp_roles::ROLE_HANGAR_CAN_TAKE[0] | corp_roles::ROLE_CONTAINER_CAN_TAKE[0],
            );
        snap.consume_exact_item_quantity(sag1_nested_item, 10, corp_opts).unwrap();
        assert_eq!(snap.items.get(&sag1_nested_item).unwrap().quantity, 40);

        // Structure ACL denial blocks member even with corp hangar roles
        snap.structure_docking_acl_denied.entry(struct_id).or_default().insert(member_id);
        assert!(snap.consume_exact_item_quantity(sag1_item, 10, corp_opts).is_err());

        // Personal item in structure blocked when access_profiles blocks character
        snap.structure_docking_acl_denied.remove(&struct_id);
        snap.items.insert(struct_item, InventoryItemRecord {
            item_id: struct_item,
            type_id: 37,
            owner_id: member_id,
            location_id: struct_id,
            flag: InventoryFlag::Hangar,
            quantity: 20,
            singleton: false,
            version: 1,
            ..InventoryItemRecord::default()
        });
        let cur_ext = snap.item_custom_names.get(&-7_700_001).cloned().unwrap_or_else(|| "{}".into());
        let mut ext_val: serde_json::Value = serde_json::from_str(&cur_ext).unwrap();
        ext_val["access_profiles"] = serde_json::json!({
            struct_id.to_string(): {
                "blocked_characters": [member_id]
            }
        });
        snap.item_custom_names.insert(-7_700_001, ext_val.to_string());
        assert!(snap
            .consume_exact_item_quantity(
                struct_item,
                5,
                ConsumeExactOptions::for_character_stack(member_id)
            )
            .is_err());
    });
}

type WarMetaAlias = crate::durable_domain_types::PersistedWarMeta;
