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
        snap.kv_mut(crate::Ns::AlscLockedItem)
            .set(alsc_item_id, "", &true)
            .expect("lock alsc item");

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

#[test]
fn structure_and_slot_flag_predicates_and_container_non_empty() {
    let mut snap = WorldStateSnapshot::default();
    let struct_id = 1_055_800_001_i64;
    let non_struct_id = 1_055_800_999_i64;
    snap.structures.insert(
        struct_id,
        UpwellStructureRecord {
            structure_id: struct_id,
            type_id: 35_832,
            owner_corp_id: 98_000_001,
            solar_system_id: 30_000_142,
            name: "Flag Pred Fortizar".into(),
            position: [1.0, 2.0, 3.0],
            state: "ShieldVulnerable".into(),
            quantum_core_installed: true,
            fuel_blocks: 100,
            shield_hp: 100.0,
            armor_hp: 100.0,
            hull_hp: 100.0,
            reinforced_until_unix: 0,
            last_damage_tick: 0,
            last_damage_unix: 0,
            self_destruct_at_unix: None,
        },
    );

    // 1. is_fitted_structure_slot_flag: [92..=94, 164..=171, 180] true; adjacent & storage flags false.
    let struct_true_flags = [
        InventoryFlag::RigSlot0,
        InventoryFlag::RigSlot1,
        InventoryFlag::RigSlot2,
        InventoryFlag::ServiceSlot0,
        InventoryFlag::ServiceSlot1,
        InventoryFlag::ServiceSlot2,
        InventoryFlag::ServiceSlot3,
        InventoryFlag::ServiceSlot4,
        InventoryFlag::ServiceSlot5,
        InventoryFlag::ServiceSlot6,
        InventoryFlag::ServiceSlot7,
        InventoryFlag::QuantumCoreRoom,
    ];
    for f in struct_true_flags {
        assert!(
            WorldStateSnapshot::is_fitted_structure_slot_flag(f),
            "{f:?} must be fitted structure slot"
        );
        assert!(
            snap.is_fitted_structure_slot_flag_at(struct_id, f),
            "{f:?} at structure must be true"
        );
    }
    let struct_false_flags = [
        InventoryFlag::None,
        InventoryFlag::Hangar,
        InventoryFlag::Cargo,
        InventoryFlag::LoSlot0,
        InventoryFlag::HiSlot7,
        InventoryFlag::ShipHangar,
        InventoryFlag::SubSystemSlot0,
        InventoryFlag::SpecializedFuelBay,
        InventoryFlag::FighterBay,
        InventoryFlag::StructureFuel,
        InventoryFlag::Deliveries,
        InventoryFlag::SpecializedMobileDepotHold,
        InventoryFlag::SpecializedIceHold,
    ];
    for f in struct_false_flags {
        assert!(
            !WorldStateSnapshot::is_fitted_structure_slot_flag(f),
            "{f:?} must NOT be fitted structure slot"
        );
        assert!(
            !snap.is_fitted_structure_slot_flag_at(struct_id, f),
            "{f:?} at structure must be false"
        );
        assert!(
            !snap.is_fitted_structure_slot_flag_at(non_struct_id, f),
            "{f:?} at non-structure must be false"
        );
    }

    // 2. is_fitted_structure_slot_flag_at: rigs (92..=94) depend on snap.structures, services/core do not.
    for rig in [InventoryFlag::RigSlot0, InventoryFlag::RigSlot1, InventoryFlag::RigSlot2] {
        assert!(!snap.is_fitted_structure_slot_flag_at(non_struct_id, rig));
        assert!(snap.is_fitted_structure_slot_flag_at(struct_id, rig));
    }
    for srv_core in [
        InventoryFlag::ServiceSlot0,
        InventoryFlag::ServiceSlot7,
        InventoryFlag::QuantumCoreRoom,
    ] {
        assert!(snap.is_fitted_structure_slot_flag_at(non_struct_id, srv_core));
    }

    // 3. is_fitted_slot_flag: covers every range (7, 11..=34, 56, 57, 88, 89, 92..=94, 125..=129, 164..=171, 180)
    // plus all boundary/storage false flags.
    let all_fitted_true = [
        InventoryFlag::Skill,
        InventoryFlag::LoSlot0,
        InventoryFlag::LoSlot7,
        InventoryFlag::MedSlot0,
        InventoryFlag::MedSlot7,
        InventoryFlag::HiSlot0,
        InventoryFlag::HiSlot7,
        InventoryFlag::Capsule,
        InventoryFlag::Pilot,
        InventoryFlag::Booster,
        InventoryFlag::Implant,
        InventoryFlag::RigSlot0,
        InventoryFlag::RigSlot2,
        InventoryFlag::SubSystemSlot0,
        InventoryFlag::SubSystemSlot4,
        InventoryFlag::ServiceSlot0,
        InventoryFlag::ServiceSlot7,
        InventoryFlag::QuantumCoreRoom,
    ];
    for f in all_fitted_true {
        assert!(WorldStateSnapshot::is_fitted_slot_flag(f), "{f:?} should be fitted");
    }
    let all_fitted_false = [
        InventoryFlag::None,
        InventoryFlag::Wardrobe,
        InventoryFlag::Hangar,
        InventoryFlag::Cargo,
        InventoryFlag::CorpDeliveries,
        InventoryFlag::Locked,
        InventoryFlag::Unlocked,
        InventoryFlag::Impounded,
        InventoryFlag::DroneBay,
        InventoryFlag::ShipHangar,
        InventoryFlag::CorpSAG1,
        InventoryFlag::CorpSAG7,
        InventoryFlag::SpecializedFuelBay,
        InventoryFlag::FleetHangar,
        InventoryFlag::FighterBay,
        InventoryFlag::StructureFuel,
        InventoryFlag::Deliveries,
        InventoryFlag::FrigateEscapeBay,
        InventoryFlag::SpecializedMobileDepotHold,
        InventoryFlag::SpecializedIceHold,
        InventoryFlag::SpecializedInfrastructureHold,
    ];
    for f in all_fitted_false {
        assert!(!WorldStateSnapshot::is_fitted_slot_flag(f), "{f:?} should NOT be fitted");
    }

    // 4. is_container_non_empty(cid): ignores self-referencing and quantity <= 0; true when positive child exists.
    let cid = 400_001_001_i64;
    assert!(!snap.is_container_non_empty(cid));
    snap.items.insert(
        cid,
        InventoryItemRecord {
            item_id: cid,
            type_id: 3293,
            owner_id: 90_000_001,
            location_id: cid, // self-reference with qty=1 must still report false
            flag: InventoryFlag::Hangar,
            quantity: 1,
            singleton: true,
            ..InventoryItemRecord::default()
        },
    );
    assert!(!snap.is_container_non_empty(cid));

    snap.items.insert(
        400_001_002,
        InventoryItemRecord {
            item_id: 400_001_002,
            type_id: 34,
            owner_id: 90_000_001,
            location_id: cid,
            flag: InventoryFlag::Cargo,
            quantity: 0,
            ..InventoryItemRecord::default()
        },
    );
    snap.items.insert(
        400_001_003,
        InventoryItemRecord {
            item_id: 400_001_003,
            type_id: 35,
            owner_id: 90_000_001,
            location_id: cid,
            flag: InventoryFlag::Cargo,
            quantity: -2,
            ..InventoryItemRecord::default()
        },
    );
    assert!(!snap.is_container_non_empty(cid));

    snap.items.insert(
        400_001_004,
        InventoryItemRecord {
            item_id: 400_001_004,
            type_id: 36,
            owner_id: 90_000_001,
            location_id: cid,
            flag: InventoryFlag::Cargo,
            quantity: 1,
            ..InventoryItemRecord::default()
        },
    );
    assert!(snap.is_container_non_empty(cid));
}

#[test]
fn alsc_lock_asset_safety_wrap_war_hq_and_ancestor_lock_checks() {
    let mut snap = WorldStateSnapshot::default();
    let station_id = 60_003_760_i64;

    // 1. is_item_in_alsc_lock: absent -> false, corrupt JSON -> false, entity_kv check true/false.
    assert!(!snap.is_item_in_alsc_lock(500_001));
    snap.entity_kv.insert(
        crate::EntityKvKey::new(crate::Ns::AlscLockedItem, 500_001, ""),
        crate::EntityKvRow {
            value_json: "{not-json".into(),
            updated_unix: 1,
        },
    );
    assert!(!snap.is_item_in_alsc_lock(500_001));
    snap.kv_mut(crate::Ns::AlscLockedItem)
        .set(500_001, "", &true)
        .expect("lock 500_001");
    snap.kv_mut(crate::Ns::AlscLockedItem)
        .set(500_002, "", &true)
        .expect("lock 500_002");
    assert!(snap.is_item_in_alsc_lock(500_001));
    assert!(!snap.is_item_in_alsc_lock(500_003));

    // 2. is_item_in_active_asset_safety_wrap: direct type 60, unpaid wraps via wrap_id & key parse,
    // paid false, item_ids match, 8-parent cap vs 9th parent, and cycle termination.
    let direct_wrap_item = 500_010_i64;
    snap.items.insert(
        direct_wrap_item,
        InventoryItemRecord {
            item_id: direct_wrap_item,
            type_id: 60, // TYPE_ASSET_SAFETY_WRAP
            owner_id: 90_000_001,
            location_id: station_id,
            flag: InventoryFlag::Hangar,
            quantity: 1,
            singleton: true,
            ..InventoryItemRecord::default()
        },
    );
    assert!(snap.is_item_in_active_asset_safety_wrap(direct_wrap_item));

    // Corrupt ext state returns false safely for ordinary item
    snap.item_custom_names.insert(-7_700_001, "{bad-json".into());
    assert!(!snap.is_item_in_active_asset_safety_wrap(500_099));

    // Build an 8-hop container chain under wrap_by_field (depth 8 -> inside chain; depth 9 -> outside)
    let wrap_by_field = 500_100_i64;
    let wrap_by_key = 500_200_i64;
    let wrap_paid = 500_300_i64;
    let listed_box = 500_400_i64;
    let inside_listed = 500_401_i64;
    snap.items.insert(
        listed_box,
        InventoryItemRecord {
            item_id: listed_box,
            location_id: station_id,
            ..InventoryItemRecord::default()
        },
    );
    snap.items.insert(
        inside_listed,
        InventoryItemRecord {
            item_id: inside_listed,
            location_id: listed_box,
            ..InventoryItemRecord::default()
        },
    );

    let mut prev = wrap_by_field;
    for step in 1..=9_i64 {
        let id = 500_100 + step;
        snap.items.insert(
            id,
            InventoryItemRecord {
                item_id: id,
                location_id: prev,
                ..InventoryItemRecord::default()
            },
        );
        prev = id;
    }
    let depth8_item = 500_108_i64; // 8 hops to wrap_by_field -> fits in 0..8
    let depth9_item = 500_109_i64; // 9 hops -> beyond cap

    // Self-cycle containers
    let cyc_a = 500_501_i64;
    let cyc_b = 500_502_i64;
    snap.items.insert(
        cyc_a,
        InventoryItemRecord {
            item_id: cyc_a,
            location_id: cyc_b,
            ..InventoryItemRecord::default()
        },
    );
    snap.items.insert(
        cyc_b,
        InventoryItemRecord {
            item_id: cyc_b,
            location_id: cyc_a,
            ..InventoryItemRecord::default()
        },
    );

    snap.item_custom_names.insert(
        -7_700_001,
        serde_json::json!({
            "asset_safety_wraps": {
                "non-numeric-key": {
                    "wrap_id": wrap_by_field,
                    "retrieval_fee_paid": false,
                    "item_ids": [listed_box]
                },
                wrap_by_key.to_string(): {
                    "retrieval_fee_paid": false
                },
                wrap_paid.to_string(): {
                    "wrap_id": wrap_paid,
                    "retrieval_fee_paid": true,
                    "item_ids": [999_999]
                }
            },
            "war_hq_structures": [500_802]
        })
        .to_string(),
    );
    assert!(snap.is_item_in_active_asset_safety_wrap(wrap_by_field));
    assert!(snap.is_item_in_active_asset_safety_wrap(depth8_item));
    assert!(!snap.is_item_in_active_asset_safety_wrap(depth9_item));
    assert!(snap.is_item_in_active_asset_safety_wrap(wrap_by_key));
    assert!(!snap.is_item_in_active_asset_safety_wrap(wrap_paid));
    assert!(snap.is_item_in_active_asset_safety_wrap(inside_listed));
    assert!(!snap.is_item_in_active_asset_safety_wrap(cyc_a));

    // 3. is_war_hq_structure: war_state.wars + wars.active + finish_at_unix, plus war_hq_structures fallback.
    let hq_active = 500_801_i64;
    let hq_blob = 500_802_i64;
    let hq_finished = 500_803_i64;
    let hq_inactive_war = 500_804_i64;
    snap.war_state.wars.insert(
        1,
        WarMetaAlias {
            war_hq_id: Some(hq_active),
            open_for_allies: false,
            finish_at_unix: None,
        },
    );
    snap.wars.insert(
        1,
        WarRecord {
            war_id: 1,
            aggressor_corp_id: 98_000_001,
            defender_corp_id: 98_000_002,
            mutual: false,
            started_unix: 10,
            active: true,
        },
    );
    snap.war_state.wars.insert(
        2,
        WarMetaAlias {
            war_hq_id: Some(hq_finished),
            open_for_allies: false,
            finish_at_unix: Some(500),
        },
    );
    snap.war_state.wars.insert(
        3,
        WarMetaAlias {
            war_hq_id: Some(hq_inactive_war),
            open_for_allies: false,
            finish_at_unix: None,
        },
    );
    snap.wars.insert(
        3,
        WarRecord {
            war_id: 3,
            aggressor_corp_id: 98_000_001,
            defender_corp_id: 98_000_003,
            mutual: false,
            started_unix: 10,
            active: false,
        },
    );
    assert!(snap.is_war_hq_structure(hq_active));
    assert!(snap.is_war_hq_structure(hq_blob));
    assert!(!snap.is_war_hq_structure(hq_finished));
    assert!(!snap.is_war_hq_structure(hq_inactive_war));
    assert!(!snap.is_war_hq_structure(500_899));

    // 4. is_escrow_locked_opt & is_item_or_ancestor_locked_or_impounded(_opt):
    let trade_item = 500_901_i64;
    let trade_child = 500_902_i64;
    let contract_item = 500_903_i64;
    snap.items.insert(
        trade_item,
        InventoryItemRecord {
            item_id: trade_item,
            location_id: station_id,
            ..InventoryItemRecord::default()
        },
    );
    snap.items.insert(
        trade_child,
        InventoryItemRecord {
            item_id: trade_child,
            location_id: trade_item,
            ..InventoryItemRecord::default()
        },
    );
    snap.items.insert(
        contract_item,
        InventoryItemRecord {
            item_id: contract_item,
            location_id: station_id,
            ..InventoryItemRecord::default()
        },
    );
    snap.owner_notes.insert(
        -73_001,
        OwnerNoteRecord {
            note_id: -73_001,
            character_id: -73,
            item_id: 1,
            label: "trade".into(),
            note: serde_json::json!({
                "trade_id": 777_i64,
                "items_a": [[trade_item, 1]],
                "items_b": [],
                "completed": false,
                "cancelled": false
            })
            .to_string(),
            timestamp_unix: 1,
        },
    );
    snap.contracts.insert(
        88_001,
        ContractRecord {
            contract_id: 88_001,
            issuer_id: 90_000_001,
            acceptor_id: None,
            contract_type: "ItemExchange".into(),
            status: "Outstanding".into(),
            start_station_id: station_id,
            end_station_id: station_id,
            price: IskCents(0),
            reward: IskCents(0),
            collateral: IskCents(0),
            item_ids: vec![contract_item],
            expiration_unix: 9_999_999_999,
            issuer_corp_id: 98_000_001,
            assignee_id: 0,
            availability: "Public".into(),
            buyout: IskCents(0),
            current_bid: IskCents(0),
            current_bidder_id: None,
            requested_items: Vec::new(),
            bid_history: Vec::new(),
            date_issued_unix: 1,
            title: "Escrow".into(),
            for_corp: false,
            num_days: 1,
        },
    );
    assert!(snap.is_escrow_locked_opt(trade_item, None));
    assert!(!snap.is_escrow_locked_opt(trade_item, Some(777)));
    assert!(snap.is_escrow_locked_opt(trade_item, Some(778)));
    assert!(snap.is_escrow_locked_opt(contract_item, Some(777)));

    // Ancestor lock via direct plastic wrap / impounded / locked and 8-hop limit vs 9-hop limit
    let pwrap_root = 500_920_i64;
    snap.items.insert(
        pwrap_root,
        InventoryItemRecord {
            item_id: pwrap_root,
            type_id: 3_468, // TYPE_PLASTIC_WRAP
            location_id: station_id,
            ..InventoryItemRecord::default()
        },
    );
    let mut anc_prev = pwrap_root;
    for step in 1..=9_i64 {
        let id = 500_920 + step;
        snap.items.insert(
            id,
            InventoryItemRecord {
                item_id: id,
                location_id: anc_prev,
                ..InventoryItemRecord::default()
            },
        );
        anc_prev = id;
    }
    assert!(snap.is_item_or_ancestor_locked_or_impounded(pwrap_root));
    assert!(snap.is_item_or_ancestor_locked_or_impounded(500_928)); // 8 ancestors up
    assert!(!snap.is_item_or_ancestor_locked_or_impounded(500_929)); // 9 ancestors up -> past cap
    assert!(snap.is_item_or_ancestor_locked_or_impounded(trade_child));
    assert!(!snap.is_item_or_ancestor_locked_or_impounded_opt(trade_child, Some(777)));
    assert!(snap.is_item_or_ancestor_locked_or_impounded_opt(trade_child, Some(778)));
    assert!(!snap.is_item_or_ancestor_locked_or_impounded(999_888_777)); // missing item

    let impounded_box = 500_950_i64;
    let impounded_child = 500_951_i64;
    snap.items.insert(
        impounded_box,
        InventoryItemRecord {
            item_id: impounded_box,
            flag: InventoryFlag::Impounded,
            location_id: station_id,
            ..InventoryItemRecord::default()
        },
    );
    snap.items.insert(
        impounded_child,
        InventoryItemRecord {
            item_id: impounded_child,
            location_id: impounded_box,
            ..InventoryItemRecord::default()
        },
    );
    assert!(snap.is_item_or_ancestor_locked_or_impounded(impounded_box));
    assert!(snap.is_item_or_ancestor_locked_or_impounded(impounded_child));
    assert!(snap.is_item_or_ancestor_locked_or_impounded(500_001)); // direct ALSC
}

#[test]
fn structure_acl_corp_roles_and_corp_take_access() {
    let mut snap = WorldStateSnapshot::default();
    let owner_corp = 98_100_001_i64;
    let ally_corp = 98_100_002_i64;
    let blocked_corp = 98_100_003_i64;
    let other_corp = 98_100_004_i64;
    let ally_id = 99_100_001_i64;
    let blocked_ally_id = 99_100_002_i64;

    let ceo_id = 90_100_001_i64;
    let group_allowed_char = 90_100_002_i64;
    let group_blocked_overrides_char = 90_100_003_i64;
    let corp_blocked_char = 90_100_004_i64;
    let ally_blocked_char = 90_100_005_i64;
    let outsider_char = 90_100_006_i64;

    let hq_st = 1_055_100_001_i64;
    let base_st = 60_003_760_i64;
    let other_st = 60_008_494_i64;

    let seed_ch = WorldRepository::with_demo_seed()
        .read_state()
        .characters
        .values()
        .next()
        .unwrap()
        .clone();
    let mk_char = |cid: i64, corp: i64, ally: Option<i64>| -> CharacterRecord {
        let mut c = seed_ch.clone();
        c.character_id = cid;
        c.user_id = 1;
        c.name = format!("Char{cid}");
        c.corporation_id = corp;
        c.alliance_id = ally;
        c.solar_system_id = 30_000_142;
        c.constellation_id = 20_000_020;
        c.region_id = 10_000_002;
        c.station_id = Some(other_st);
        c.structure_id = None;
        c.home_station_id = base_st;
        c
    };
    snap.characters.insert(ceo_id, mk_char(ceo_id, owner_corp, Some(ally_id)));
    snap.characters
        .insert(group_allowed_char, mk_char(group_allowed_char, other_corp, Some(ally_id)));
    snap.characters.insert(
        group_blocked_overrides_char,
        mk_char(group_blocked_overrides_char, other_corp, None),
    );
    snap.characters
        .insert(corp_blocked_char, mk_char(corp_blocked_char, blocked_corp, None));
    snap.characters
        .insert(ally_blocked_char, mk_char(ally_blocked_char, other_corp, Some(blocked_ally_id)));
    snap.characters
        .insert(outsider_char, mk_char(outsider_char, ally_corp, None));

    snap.structures.insert(
        hq_st,
        UpwellStructureRecord {
            structure_id: hq_st,
            type_id: 35_832,
            owner_corp_id: owner_corp,
            solar_system_id: 30_000_142,
            name: "ACL Citadel".into(),
            position: [0.0, 0.0, 0.0],
            state: "ShieldVulnerable".into(),
            quantum_core_installed: true,
            fuel_blocks: 100,
            shield_hp: 100.0,
            armor_hp: 100.0,
            hull_hp: 100.0,
            reinforced_until_unix: 0,
            last_damage_tick: 0,
            last_damage_unix: 0,
            self_destruct_at_unix: None,
        },
    );

    // 1. has_structure_acl_access across deny sets, blocked corp/ally, group role 3/-1 override, corp_only, and owner bypass.
    snap.item_custom_names.insert(
        -7_700_001,
        serde_json::json!({
            "owner_groups": {
                "10": {
                    "members": {
                        group_allowed_char.to_string(): 1,
                        outsider_char.to_string(): -1
                    }
                },
                "11": {
                    "members": {
                        group_blocked_overrides_char.to_string(): 3
                    }
                }
            },
            "access_profiles": {
                hq_st.to_string(): {
                    "blocked_characters": [],
                    "blocked_corporations": [blocked_corp],
                    "blocked_alliances": [blocked_ally_id],
                    "access_group_ids": [10, 11],
                    "allowed_characters": [group_blocked_overrides_char],
                    "corp_only": false
                }
            }
        })
        .to_string(),
    );

    // CEO in owner_corp passes allow-lists unless explicitly in structure_docking_acl_denied
    assert!(snap.has_structure_acl_access(hq_st, ceo_id));
    snap.structure_docking_acl_denied
        .entry(hq_st)
        .or_default()
        .insert(ceo_id);
    assert!(!snap.has_structure_acl_access(hq_st, ceo_id));
    snap.structure_docking_acl_denied.remove(&hq_st);

    assert!(snap.has_structure_acl_access(hq_st, group_allowed_char));
    assert!(!snap.has_structure_acl_access(hq_st, group_blocked_overrides_char));
    assert!(!snap.has_structure_acl_access(hq_st, corp_blocked_char));
    assert!(!snap.has_structure_acl_access(hq_st, ally_blocked_char));
    assert!(!snap.has_structure_acl_access(hq_st, outsider_char));

    // Test corp_only: true blocks outsider while allowing owner_corp
    snap.item_custom_names.insert(
        -7_700_001,
        serde_json::json!({
            "access_profiles": {
                hq_st.to_string(): {
                    "corp_only": true
                }
            }
        })
        .to_string(),
    );
    assert!(snap.has_structure_acl_access(hq_st, ceo_id));
    assert!(!snap.has_structure_acl_access(hq_st, group_allowed_char));
    // Non-structure IDs with no profile / no deny default true
    assert!(snap.has_structure_acl_access(base_st, group_allowed_char));
    // Clear access profile on hq_st for role & take checks below
    snap.item_custom_names.remove(&-7_700_001);

    // 2. effective_corp_roles_at_location: HQ, Base, Other, container resolution, and pending exclusion.
    snap.corporations.insert(
        owner_corp,
        CorporationRecord {
            corporation_id: owner_corp,
            name: "Roles Corp".into(),
            ticker: "ROLC".into(),
            ceo_id,
            hq_station_id: Some(hq_st),
            member_roles: HashMap::from([(group_allowed_char, corp_roles::ROLE_AUDITOR)]),
            ..CorporationRecord::default()
        },
    );
    snap.corp_governance
        .entry(owner_corp)
        .or_default()
        .member_bases
        .insert(group_allowed_char, base_st);
    snap.corp_governance
        .get_mut(&owner_corp)
        .unwrap()
        .pending_role_grants
        .insert(group_allowed_char, (corp_roles::ROLE_DIRECTOR, 9_999_999_999));
    snap.corp_role_assignments.insert(
        (owner_corp, group_allowed_char),
        CorpMemberRoleAssignment {
            character_id: group_allowed_char,
            roles: corp_roles::ROLE_HANGAR_CAN_TAKE[0],
            grantable_roles: 0,
            roles_at_hq: corp_roles::ROLE_HANGAR_CAN_TAKE[1],
            grantable_roles_at_hq: 0,
            roles_at_base: corp_roles::ROLE_HANGAR_CAN_TAKE[2],
            grantable_roles_at_base: 0,
            roles_at_other: corp_roles::ROLE_HANGAR_CAN_TAKE[3],
            grantable_roles_at_other: 0,
        },
    );

    let (missing_ceo, missing_mask) = snap.effective_corp_roles_at_location(98_999_999, ceo_id, None);
    assert!(!missing_ceo);
    assert_eq!(missing_mask, 0);

    let (is_ceo_t, _) = snap.effective_corp_roles_at_location(owner_corp, ceo_id, None);
    assert!(is_ceo_t);

    let box_in_hq = 600_001_i64;
    snap.items.insert(
        box_in_hq,
        InventoryItemRecord {
            item_id: box_in_hq,
            owner_id: owner_corp,
            location_id: hq_st,
            flag: InventoryFlag::CorpSAG2,
            ..InventoryItemRecord::default()
        },
    );

    let (is_ceo_m, hq_roles) =
        snap.effective_corp_roles_at_location(owner_corp, group_allowed_char, Some(box_in_hq));
    assert!(!is_ceo_m);
    assert_eq!(hq_roles & corp_roles::ROLE_DIRECTOR, 0, "pending role must stay excluded");
    assert_ne!(hq_roles & corp_roles::ROLE_AUDITOR, 0);
    assert_ne!(hq_roles & corp_roles::ROLE_HANGAR_CAN_TAKE[0], 0);
    assert_ne!(hq_roles & corp_roles::ROLE_HANGAR_CAN_TAKE[1], 0);
    assert_eq!(hq_roles & corp_roles::ROLE_HANGAR_CAN_TAKE[2], 0);
    assert_eq!(hq_roles & corp_roles::ROLE_HANGAR_CAN_TAKE[3], 0);

    let (_, base_roles) =
        snap.effective_corp_roles_at_location(owner_corp, group_allowed_char, Some(base_st));
    assert_ne!(base_roles & corp_roles::ROLE_HANGAR_CAN_TAKE[2], 0);
    assert_eq!(base_roles & corp_roles::ROLE_HANGAR_CAN_TAKE[1], 0);
    assert_eq!(base_roles & corp_roles::ROLE_HANGAR_CAN_TAKE[3], 0);

    // location_id: None resolves char.station_id (other_st) -> uses roles_at_other
    let (_, other_roles) = snap.effective_corp_roles_at_location(owner_corp, group_allowed_char, None);
    assert_ne!(other_roles & corp_roles::ROLE_HANGAR_CAN_TAKE[3], 0);
    assert_eq!(other_roles & corp_roles::ROLE_HANGAR_CAN_TAKE[1], 0);
    assert_eq!(other_roles & corp_roles::ROLE_HANGAR_CAN_TAKE[2], 0);

    // 3. resolve_corp_root_location & validate_corp_take_access across SAG1..=7, Deliveries, Containers, and ACL
    assert_eq!(snap.resolve_corp_root_location(owner_corp, 999_777), (0, InventoryFlag::None, false));
    assert!(snap.validate_corp_take_access(group_allowed_char, owner_corp, 999_777).is_err());

    let nested_in_box = 600_002_i64;
    snap.items.insert(
        nested_in_box,
        InventoryItemRecord {
            item_id: nested_in_box,
            owner_id: owner_corp,
            location_id: box_in_hq,
            flag: InventoryFlag::Cargo,
            quantity: 5,
            ..InventoryItemRecord::default()
        },
    );
    assert_eq!(
        snap.resolve_corp_root_location(owner_corp, nested_in_box),
        (hq_st, InventoryFlag::CorpSAG2, true)
    );
    // group_allowed_char has ROLE_HANGAR_CAN_TAKE[1] at hq_st, but NOT ROLE_CONTAINER_CAN_TAKE[1]
    assert!(snap.validate_corp_take_access(group_allowed_char, owner_corp, box_in_hq).is_ok());
    assert!(snap.validate_corp_take_access(group_allowed_char, owner_corp, nested_in_box).is_err());

    // Grant ONLY CONTAINER_CAN_TAKE without HANGAR_CAN_TAKE on SAG7 -> still fails
    let sag7_item = 600_003_i64;
    snap.items.insert(
        sag7_item,
        InventoryItemRecord {
            item_id: sag7_item,
            owner_id: owner_corp,
            location_id: hq_st,
            flag: InventoryFlag::CorpSAG7,
            quantity: 1,
            ..InventoryItemRecord::default()
        },
    );
    snap.corporations.get_mut(&owner_corp).unwrap().member_roles.insert(
        outsider_char,
        corp_roles::ROLE_CONTAINER_CAN_TAKE[6],
    );
    assert!(snap.validate_corp_take_access(outsider_char, owner_corp, sag7_item).is_err());
    snap.corporations.get_mut(&owner_corp).unwrap().member_roles.insert(
        outsider_char,
        corp_roles::ROLE_HANGAR_CAN_TAKE[6],
    );
    assert!(snap.validate_corp_take_access(outsider_char, owner_corp, sag7_item).is_ok());

    // Deliveries & nested Deliveries checks
    let deliv_box = 600_004_i64;
    let deliv_nested = 600_005_i64;
    let non_corp_flag_item = 600_006_i64;
    snap.items.insert(
        deliv_box,
        InventoryItemRecord {
            item_id: deliv_box,
            owner_id: owner_corp,
            location_id: base_st,
            flag: InventoryFlag::CorpDeliveries,
            quantity: 1,
            ..InventoryItemRecord::default()
        },
    );
    snap.items.insert(
        deliv_nested,
        InventoryItemRecord {
            item_id: deliv_nested,
            owner_id: owner_corp,
            location_id: deliv_box,
            flag: InventoryFlag::Cargo,
            quantity: 1,
            ..InventoryItemRecord::default()
        },
    );
    snap.items.insert(
        non_corp_flag_item,
        InventoryItemRecord {
            item_id: non_corp_flag_item,
            owner_id: owner_corp,
            location_id: hq_st,
            flag: InventoryFlag::Hangar,
            quantity: 1,
            ..InventoryItemRecord::default()
        },
    );
    assert!(snap.validate_corp_take_access(outsider_char, owner_corp, deliv_box).is_err());
    snap.corporations.get_mut(&owner_corp).unwrap().member_roles.insert(
        outsider_char,
        corp_roles::ROLE_DELIVERIES_HANGAR_TAKE,
    );
    assert!(snap.validate_corp_take_access(outsider_char, owner_corp, deliv_box).is_ok());
    assert!(snap.validate_corp_take_access(outsider_char, owner_corp, deliv_nested).is_err());
    snap.corporations.get_mut(&owner_corp).unwrap().member_roles.insert(
        outsider_char,
        corp_roles::ROLE_DELIVERIES_HANGAR_TAKE | corp_roles::ROLE_DELIVERIES_HANGAR_CONTAINER_TAKE,
    );
    assert!(snap.validate_corp_take_access(outsider_char, owner_corp, deliv_nested).is_ok());
    assert!(snap.validate_corp_take_access(outsider_char, owner_corp, non_corp_flag_item).is_err());

    // CapsuleerDeliveries (187) & nested CapsuleerDeliveries checks
    let cap_deliv_box = 600_007_i64;
    let cap_deliv_nested = 600_008_i64;
    snap.items.insert(
        cap_deliv_box,
        InventoryItemRecord {
            item_id: cap_deliv_box,
            owner_id: owner_corp,
            location_id: base_st,
            flag: InventoryFlag::CapsuleerDeliveries,
            quantity: 1,
            ..InventoryItemRecord::default()
        },
    );
    snap.items.insert(
        cap_deliv_nested,
        InventoryItemRecord {
            item_id: cap_deliv_nested,
            owner_id: owner_corp,
            location_id: cap_deliv_box,
            flag: InventoryFlag::Cargo,
            quantity: 1,
            ..InventoryItemRecord::default()
        },
    );
    snap.corporations
        .get_mut(&owner_corp)
        .unwrap()
        .member_roles
        .insert(outsider_char, 0);
    assert!(snap
        .validate_corp_take_access(outsider_char, owner_corp, cap_deliv_box)
        .is_err());
    snap.corporations
        .get_mut(&owner_corp)
        .unwrap()
        .member_roles
        .insert(outsider_char, corp_roles::ROLE_DELIVERIES_HANGAR_TAKE);
    assert!(snap
        .validate_corp_take_access(outsider_char, owner_corp, cap_deliv_box)
        .is_ok());
    assert!(snap
        .validate_corp_take_access(outsider_char, owner_corp, cap_deliv_nested)
        .is_err());
    snap.corporations
        .get_mut(&owner_corp)
        .unwrap()
        .member_roles
        .insert(
            outsider_char,
            corp_roles::ROLE_DELIVERIES_HANGAR_TAKE
                | corp_roles::ROLE_DELIVERIES_HANGAR_CONTAINER_TAKE,
        );
    assert!(snap
        .validate_corp_take_access(outsider_char, owner_corp, cap_deliv_nested)
        .is_ok());

    // Director bypasses flag check; structure ACL denial still blocks CEO/Director
    assert!(snap.validate_corp_take_access(ceo_id, owner_corp, non_corp_flag_item).is_ok());
    snap.corporations
        .get_mut(&owner_corp)
        .unwrap()
        .member_roles
        .insert(outsider_char, corp_roles::ROLE_DIRECTOR);
    assert!(snap.validate_corp_take_access(outsider_char, owner_corp, non_corp_flag_item).is_ok());
    snap.structure_docking_acl_denied
        .entry(hq_st)
        .or_default()
        .insert(outsider_char);
    assert!(snap.validate_corp_take_access(outsider_char, owner_corp, non_corp_flag_item).is_err());
}

#[test]
fn root_location_constraint_consumption_and_system_cleanup() {
    let mut snap = WorldStateSnapshot::default();
    let owner_id = 90_000_001_i64;
    let other_id = 90_000_002_i64;
    let station_id = 60_003_760_i64;
    let struct_id = 1_055_200_001_i64;

    snap.structures.insert(
        struct_id,
        UpwellStructureRecord {
            structure_id: struct_id,
            type_id: 35_832,
            owner_corp_id: 98_000_001,
            solar_system_id: 30_000_142,
            name: "Root Test".into(),
            position: [0.0, 0.0, 0.0],
            state: "ShieldVulnerable".into(),
            quantum_core_installed: true,
            fuel_blocks: 100,
            shield_hp: 100.0,
            armor_hp: 100.0,
            hull_hp: 100.0,
            reinforced_until_unix: 0,
            last_damage_tick: 0,
            last_damage_unix: 0,
            self_destruct_at_unix: None,
        },
    );

    // 1. resolve_root_location_id across 3-level nested containers + cycle safety + missing item
    assert_eq!(snap.resolve_root_location_id(999_001), 0);
    let c1 = 700_001_i64;
    let c2 = 700_002_i64;
    let c3 = 700_003_i64;
    let leaf = 700_004_i64;
    let cyc_x = 700_010_i64;
    let cyc_y = 700_011_i64;
    snap.items.insert(
        c1,
        InventoryItemRecord {
            item_id: c1,
            owner_id,
            location_id: struct_id,
            flag: InventoryFlag::Hangar,
            quantity: 1,
            singleton: true,
            ..InventoryItemRecord::default()
        },
    );
    snap.items.insert(
        c2,
        InventoryItemRecord {
            item_id: c2,
            owner_id,
            location_id: c1,
            flag: InventoryFlag::Cargo,
            quantity: 1,
            singleton: true,
            ..InventoryItemRecord::default()
        },
    );
    snap.items.insert(
        c3,
        InventoryItemRecord {
            item_id: c3,
            owner_id,
            location_id: c2,
            flag: InventoryFlag::Cargo,
            quantity: 1,
            singleton: true,
            ..InventoryItemRecord::default()
        },
    );
    snap.items.insert(
        leaf,
        InventoryItemRecord {
            item_id: leaf,
            owner_id,
            location_id: c3,
            flag: InventoryFlag::Cargo,
            quantity: 10,
            ..InventoryItemRecord::default()
        },
    );
    snap.items.insert(
        cyc_x,
        InventoryItemRecord {
            item_id: cyc_x,
            owner_id,
            location_id: cyc_y,
            ..InventoryItemRecord::default()
        },
    );
    snap.items.insert(
        cyc_y,
        InventoryItemRecord {
            item_id: cyc_y,
            owner_id,
            location_id: cyc_x,
            ..InventoryItemRecord::default()
        },
    );
    assert_eq!(snap.resolve_root_location_id(leaf), struct_id);
    let cyc_root = snap.resolve_root_location_id(cyc_x);
    assert!(cyc_root == cyc_x || cyc_root == cyc_y);

    // 2. matches_location_constraint: None, exact, nested flag & container flag, allow_nested false/true, cycle false.
    let leaf_rec = snap.items[&leaf].clone();
    let mut opts = ConsumeExactOptions::for_character_stack(owner_id);
    assert!(snap.matches_location_constraint(&leaf_rec, &opts));
    opts.required_flag = Some(InventoryFlag::Hangar);
    assert!(!snap.matches_location_constraint(&leaf_rec, &opts));
    opts.required_flag = Some(InventoryFlag::Cargo);
    assert!(snap.matches_location_constraint(&leaf_rec, &opts));

    opts.required_location_id = Some(struct_id);
    opts.required_flag = Some(InventoryFlag::Hangar);
    opts.allow_nested_in_location = false;
    assert!(!snap.matches_location_constraint(&leaf_rec, &opts));
    opts.allow_nested_in_location = true;
    assert!(snap.matches_location_constraint(&leaf_rec, &opts));
    opts.required_flag = Some(InventoryFlag::Cargo);
    assert!(!snap.matches_location_constraint(&leaf_rec, &opts));
    let cyc_rec = snap.items[&cyc_x].clone();
    assert!(!snap.matches_location_constraint(&cyc_rec, &opts));

    // 3. validate_item_for_consumption: owner, qty <= 0, location, locked/trade-excl/war-hq,
    // boarded active_ship, ACL, fitted, singleton, BPC, non-empty container, and success.
    let ok_opts = ConsumeExactOptions::for_character_stack(owner_id);
    assert!(snap.validate_item_for_consumption(&leaf_rec, &ok_opts).is_ok());

    let mut wrong_owner = leaf_rec.clone();
    wrong_owner.owner_id = other_id;
    assert_eq!(
        snap.validate_item_for_consumption(&wrong_owner, &ok_opts),
        Err(DbError::ItemOwnershipViolation {
            character_id: owner_id,
            item_id: leaf
        })
    );

    let mut zero_qty = leaf_rec.clone();
    zero_qty.quantity = 0;
    assert!(snap.validate_item_for_consumption(&zero_qty, &ok_opts).is_err());

    let mut fitted_item = leaf_rec.clone();
    fitted_item.flag = InventoryFlag::HiSlot0;
    assert!(snap.validate_item_for_consumption(&fitted_item, &ok_opts).is_err());

    let mut singleton_item = leaf_rec.clone();
    singleton_item.singleton = true;
    assert!(snap.validate_item_for_consumption(&singleton_item, &ok_opts).is_err());

    let mut bpc_item = leaf_rec.clone();
    bpc_item.is_blueprint_copy = true;
    let mut bpc_opts = ok_opts;
    bpc_opts.reject_blueprint_copy = true;
    assert!(snap.validate_item_for_consumption(&bpc_item, &bpc_opts).is_err());
    let mut run_bounded_item = leaf_rec.clone();
    run_bounded_item.blueprint_runs_remaining = Some(1);
    assert!(snap.validate_item_for_consumption(&run_bounded_item, &bpc_opts).is_err());
    assert!(snap.is_clean_stackable_item(&leaf_rec));
    assert!(!snap.is_clean_stackable_item(&run_bounded_item));

    assert!(snap.validate_item_for_consumption(&snap.items[&c1], &ConsumeExactOptions::for_owner_exact(Some(owner_id), owner_id)).is_err());

    let mut boarded_opts = ok_opts;
    boarded_opts.active_ship_id = Some(leaf);
    assert!(snap.validate_item_for_consumption(&leaf_rec, &boarded_opts).is_err());

    // Structure ACL denial blocks consumption
    snap.structure_docking_acl_denied
        .entry(struct_id)
        .or_default()
        .insert(owner_id);
    assert!(snap.validate_item_for_consumption(&leaf_rec, &ok_opts).is_err());
    snap.structure_docking_acl_denied.remove(&struct_id);

    // Non-corp actor != owner rejected when item owned by expected_owner_id != actor
    let diff_actor = ConsumeExactOptions::for_owner_exact(Some(other_id), owner_id);
    assert_eq!(
        snap.validate_item_for_consumption(&leaf_rec, &diff_actor),
        Err(DbError::ItemOwnershipViolation {
            character_id: other_id,
            item_id: leaf
        })
    );
    // System maintenance path actor_char_id: None succeeds
    let sys_actor = ConsumeExactOptions::for_owner_exact(None, owner_id);
    assert!(snap.validate_item_for_consumption(&leaf_rec, &sys_actor).is_ok());

    // War HQ designation rejects consumption with ItemLocked
    snap.item_custom_names.insert(
        -7_700_001,
        serde_json::json!({"war_hq_structures": [leaf]}).to_string(),
    );
    assert_eq!(
        snap.validate_item_for_consumption(&leaf_rec, &ok_opts),
        Err(DbError::ItemLocked(leaf))
    );
    snap.item_custom_names.remove(&-7_700_001);

    // 4. remove_system_cleanup_item: ignores metadata cleanup for <= 0, cleans all 6 auxiliary maps for > 0.
    let sentinel_id = -7_700_001_i64;
    snap.item_custom_names.insert(sentinel_id, "keep-sentinel".into());
    snap.item_positions.insert(0, [1.0, 2.0, 3.0]);
    assert!(snap.remove_system_cleanup_item(0).is_none());
    assert!(snap.remove_system_cleanup_item(sentinel_id).is_none());
    assert_eq!(
        snap.item_custom_names.get(&sentinel_id).map(String::as_str),
        Some("keep-sentinel")
    );
    assert!(snap.item_positions.contains_key(&0));

    let clean_id = 700_999_i64;
    snap.items.insert(
        clean_id,
        InventoryItemRecord {
            item_id: clean_id,
            owner_id,
            location_id: station_id,
            flag: InventoryFlag::Hangar,
            quantity: 1,
            singleton: true,
            ..InventoryItemRecord::default()
        },
    );
    snap.item_custom_names.insert(clean_id, "My Rifter".into());
    snap.item_positions.insert(clean_id, [10.0, 20.0, 30.0]);
    snap.module_online_states.insert(clean_id, true);
    snap.module_active_states.insert(clean_id);
    snap.module_active_charges.insert(clean_id, 219);
    snap.applied_ship_skins.insert(clean_id, 42_001);

    let removed = snap.remove_system_cleanup_item(clean_id).expect("removed item");
    assert_eq!(removed.item_id, clean_id);
    assert!(!snap.items.contains_key(&clean_id));
    assert!(!snap.item_custom_names.contains_key(&clean_id));
    assert!(!snap.item_positions.contains_key(&clean_id));
    assert!(!snap.module_online_states.contains_key(&clean_id));
    assert!(!snap.module_active_states.contains(&clean_id));
    assert!(!snap.module_active_charges.contains_key(&clean_id));
    assert!(!snap.applied_ship_skins.contains_key(&clean_id));
    assert!(snap.remove_system_cleanup_item(clean_id).is_none());
}

#[test]
fn transfer_and_consume_inventory_edge_paths() {
    let mut snap = WorldStateSnapshot::default();
    let owner_id = 90_000_001_i64;
    let other_id = 90_000_002_i64;
    let corp_id = 98_000_001_i64;
    let station_id = 60_003_760_i64;
    let struct_id = 1_055_300_001_i64;
    snap.next_entity_id = 850_000;

    snap.corporations.insert(
        corp_id,
        CorporationRecord {
            corporation_id: corp_id,
            name: "Transfer Corp".into(),
            ticker: "TRFC".into(),
            ceo_id: owner_id,
            hq_station_id: Some(station_id),
            ..CorporationRecord::default()
        },
    );
    snap.structures.insert(
        struct_id,
        UpwellStructureRecord {
            structure_id: struct_id,
            type_id: 35_832,
            owner_corp_id: corp_id,
            solar_system_id: 30_000_142,
            name: "Edge Struct".into(),
            position: [0.0, 0.0, 0.0],
            state: "ShieldVulnerable".into(),
            quantum_core_installed: true,
            fuel_blocks: 100,
            shield_hp: 100.0,
            armor_hp: 100.0,
            hull_hp: 100.0,
            reinforced_until_unix: 0,
            last_damage_tick: 0,
            last_damage_unix: 0,
            self_destruct_at_unix: None,
        },
    );

    let box_id = 800_001_i64;
    let stack_id = 800_002_i64;
    let stack_b = 800_003_i64;
    let locked_dest = 800_004_i64;
    let ctrl_ship = 800_005_i64;
    let singleton_id = 800_006_i64;
    let courier_src = 800_007_i64;
    let courier_dst = 800_008_i64;
    let corp_item = 800_009_i64;
    let child_in_box = 800_010_i64;

    snap.items.insert(
        box_id,
        InventoryItemRecord {
            item_id: box_id,
            type_id: 3293,
            owner_id,
            location_id: station_id,
            flag: InventoryFlag::Hangar,
            quantity: 1,
            singleton: false,
            ..InventoryItemRecord::default()
        },
    );
    snap.items.insert(
        child_in_box,
        InventoryItemRecord {
            item_id: child_in_box,
            type_id: 34,
            owner_id,
            location_id: box_id,
            flag: InventoryFlag::Cargo,
            quantity: 5,
            ..InventoryItemRecord::default()
        },
    );
    snap.items.insert(
        stack_id,
        InventoryItemRecord {
            item_id: stack_id,
            type_id: 34,
            owner_id,
            location_id: box_id,
            flag: InventoryFlag::Cargo,
            quantity: 10,
            ..InventoryItemRecord::default()
        },
    );
    snap.items.insert(
        stack_b,
        InventoryItemRecord {
            item_id: stack_b,
            type_id: 34,
            owner_id,
            location_id: station_id,
            flag: InventoryFlag::Hangar,
            quantity: 20,
            ..InventoryItemRecord::default()
        },
    );
    snap.items.insert(
        locked_dest,
        InventoryItemRecord {
            item_id: locked_dest,
            type_id: 3293,
            owner_id,
            location_id: station_id,
            flag: InventoryFlag::Locked,
            quantity: 1,
            singleton: true,
            ..InventoryItemRecord::default()
        },
    );
    snap.items.insert(
        ctrl_ship,
        InventoryItemRecord {
            item_id: ctrl_ship,
            type_id: 587,
            owner_id,
            location_id: struct_id,
            flag: InventoryFlag::Hangar,
            quantity: 1,
            singleton: true,
            ..InventoryItemRecord::default()
        },
    );
    snap.items.insert(
        singleton_id,
        InventoryItemRecord {
            item_id: singleton_id,
            type_id: 587,
            owner_id,
            location_id: station_id,
            flag: InventoryFlag::Hangar,
            quantity: 2,
            singleton: true,
            ..InventoryItemRecord::default()
        },
    );
    snap.items.insert(
        courier_src,
        InventoryItemRecord {
            item_id: courier_src,
            type_id: 35,
            owner_id,
            location_id: station_id,
            flag: InventoryFlag::Hangar,
            quantity: 4,
            ..InventoryItemRecord::default()
        },
    );
    snap.items.insert(
        courier_dst,
        InventoryItemRecord {
            item_id: courier_dst,
            type_id: 35,
            owner_id,
            location_id: box_id,
            flag: InventoryFlag::Cargo,
            quantity: 6,
            ..InventoryItemRecord::default()
        },
    );
    snap.item_custom_names.insert(courier_src, "courier_cargo:src".into());
    snap.item_custom_names.insert(courier_dst, "courier_cargo:dst".into());
    snap.items.insert(
        corp_item,
        InventoryItemRecord {
            item_id: corp_item,
            type_id: 36,
            owner_id: corp_id,
            location_id: station_id,
            flag: InventoryFlag::CorpSAG1,
            quantity: 3,
            ..InventoryItemRecord::default()
        },
    );

    // matches_location_constraint direct flag match & nested missing parent
    let mut loc_opts = ConsumeExactOptions::for_character_stack(owner_id);
    loc_opts.required_location_id = Some(station_id);
    loc_opts.required_flag = Some(InventoryFlag::Hangar);
    assert!(snap.matches_location_constraint(&snap.items[&stack_b], &loc_opts));
    loc_opts.allow_nested_in_location = true;
    let orphan = InventoryItemRecord {
        item_id: 899_001,
        owner_id,
        location_id: 899_999,
        flag: InventoryFlag::Cargo,
        quantity: 1,
        ..InventoryItemRecord::default()
    };
    assert!(!snap.matches_location_constraint(&orphan, &loc_opts));

    // validate_item_for_consumption location mismatch & structure controller saved ship
    let mut wrong_loc_opts = ConsumeExactOptions::for_character_stack(owner_id);
    wrong_loc_opts.required_location_id = Some(struct_id);
    assert!(snap
        .validate_item_for_consumption(&snap.items[&stack_b], &wrong_loc_opts)
        .is_err());

    snap.item_custom_names.insert(
        -7_700_001,
        serde_json::json!({
            "active_pilot_by_structure": { struct_id.to_string(): owner_id },
            "previous_ship_by_structure": { struct_id.to_string(): ctrl_ship }
        })
        .to_string(),
    );
    assert!(snap
        .validate_item_for_consumption(
            &snap.items[&ctrl_ship],
            &ConsumeExactOptions::for_owner_exact(Some(owner_id), owner_id)
        )
        .is_err());
    assert!(snap
        .transfer_inventory_item(
            ctrl_ship,
            TransferItemSpec::move_whole(Some(owner_id), Some(owner_id), None, station_id, InventoryFlag::Hangar),
        )
        .is_err());
    snap.item_custom_names.remove(&-7_700_001);

    // consume_exact_item_quantity edge cases (<= 0, insufficient, exact removal cleans metadata)
    assert!(snap
        .consume_exact_item_quantity(stack_b, 0, ConsumeExactOptions::for_character_stack(owner_id))
        .is_err());
    assert!(snap
        .consume_exact_item_quantity(stack_b, 999, ConsumeExactOptions::for_character_stack(owner_id))
        .is_err());
    snap.item_custom_names.insert(child_in_box, "temp-meta".into());
    assert!(snap
        .consume_exact_item_quantity(child_in_box, 5, ConsumeExactOptions::for_character_stack(owner_id))
        .unwrap()
        .is_none());
    assert!(!snap.items.contains_key(&child_in_box));
    assert!(!snap.item_custom_names.contains_key(&child_in_box));

    // consume_owner_items: <= 0, missing type_id (first_err None), and multi-stack with remaining <= 0 break
    assert!(snap
        .consume_owner_items(34, 0, ConsumeExactOptions::for_character_stack(owner_id))
        .is_err());
    assert!(snap
        .consume_owner_items(999_999, 1, ConsumeExactOptions::for_character_stack(owner_id))
        .is_err());
    let stack_c = 800_011_i64;
    snap.items.insert(
        stack_c,
        InventoryItemRecord {
            item_id: stack_c,
            type_id: 34,
            owner_id,
            location_id: station_id,
            flag: InventoryFlag::Hangar,
            quantity: 5,
            ..InventoryItemRecord::default()
        },
    );
    let touched = snap
        .consume_owner_items(34, 12, ConsumeExactOptions::for_character_stack(owner_id))
        .unwrap();
    assert_eq!(touched, vec![stack_id, stack_b]);
    assert!(!snap.items.contains_key(&stack_id));
    assert_eq!(snap.items[&stack_b].quantity, 18);
    assert_eq!(snap.items[&stack_c].quantity, 5);

    // transfer_inventory_item guardrails
    assert!(matches!(
        snap.transfer_inventory_item(
            stack_b,
            TransferItemSpec::move_whole(Some(owner_id), Some(owner_id), None, stack_b, InventoryFlag::Cargo),
        ),
        Err(DbError::InvalidDestinationLocation { .. })
    ));
    assert_eq!(
        snap.transfer_inventory_item(
            stack_b,
            TransferItemSpec::move_whole(Some(owner_id), Some(owner_id), None, station_id, InventoryFlag::Locked),
        ),
        Err(DbError::ItemLocked(stack_b))
    );
    assert_eq!(
        snap.transfer_inventory_item(
            stack_b,
            TransferItemSpec::move_whole(Some(other_id), Some(other_id), None, station_id, InventoryFlag::Cargo),
        ),
        Err(DbError::ItemOwnershipViolation {
            character_id: other_id,
            item_id: stack_b
        })
    );
    assert_eq!(
        snap.transfer_inventory_item(
            stack_b,
            TransferItemSpec::move_whole(Some(owner_id), Some(owner_id), None, locked_dest, InventoryFlag::Cargo),
        ),
        Err(DbError::ItemLocked(locked_dest))
    );
    // Reject transferring into Upwell structure RigSlot0
    assert!(snap
        .transfer_inventory_item(
            stack_b,
            TransferItemSpec::move_whole(Some(owner_id), Some(owner_id), None, struct_id, InventoryFlag::RigSlot0),
        )
        .is_err());

    // actor_char_id checks: boarded ship, corp take success vs outsider rejection, source & dest ACL
    assert!(snap
        .transfer_inventory_item(
            singleton_id,
            TransferItemSpec {
                active_ship_id: Some(singleton_id),
                ..TransferItemSpec::move_whole(Some(owner_id), Some(owner_id), None, station_id, InventoryFlag::Hangar)
            },
        )
        .is_err());
    assert!(snap
        .transfer_inventory_item(
            corp_item,
            TransferItemSpec::move_whole(Some(owner_id), None, Some(owner_id), station_id, InventoryFlag::Hangar),
        )
        .is_ok());
    assert_eq!(
        snap.transfer_inventory_item(
            stack_b,
            TransferItemSpec::move_whole(Some(other_id), None, None, station_id, InventoryFlag::Hangar),
        ),
        Err(DbError::ItemOwnershipViolation {
            character_id: other_id,
            item_id: stack_b
        })
    );
    snap.structure_docking_acl_denied
        .entry(struct_id)
        .or_default()
        .insert(owner_id);
    assert!(snap
        .transfer_inventory_item(
            ctrl_ship,
            TransferItemSpec::move_whole(Some(owner_id), Some(owner_id), None, station_id, InventoryFlag::Hangar),
        )
        .is_err());
    assert!(matches!(
        snap.transfer_inventory_item(
            stack_b,
            TransferItemSpec::move_whole(Some(owner_id), Some(owner_id), None, struct_id, InventoryFlag::Hangar),
        ),
        Err(DbError::InvalidDestinationLocation { .. })
    ));
    snap.structure_docking_acl_denied.remove(&struct_id);

    // Invalid quantity & partial singleton / non-empty container guards
    for bad_q in [0, 999] {
        assert!(snap
            .transfer_inventory_item(
                stack_b,
                TransferItemSpec {
                    quantity: Some(bad_q),
                    ..TransferItemSpec::move_whole(Some(owner_id), Some(owner_id), None, box_id, InventoryFlag::Cargo)
                },
            )
            .is_err());
    }
    assert!(snap
        .transfer_inventory_item(
            singleton_id,
            TransferItemSpec {
                quantity: Some(1),
                ..TransferItemSpec::move_whole(Some(owner_id), Some(owner_id), None, box_id, InventoryFlag::Cargo)
            },
        )
        .is_err());
    snap.items.get_mut(&box_id).unwrap().quantity = 3;
    snap.items.insert(
        800_012,
        InventoryItemRecord {
            item_id: 800_012,
            type_id: 34,
            owner_id,
            location_id: box_id,
            flag: InventoryFlag::Cargo,
            quantity: 1,
            ..InventoryItemRecord::default()
        },
    );
    assert!(snap
        .transfer_inventory_item(
            box_id,
            TransferItemSpec {
                quantity: Some(1),
                ..TransferItemSpec::move_whole(Some(owner_id), Some(owner_id), None, struct_id, InventoryFlag::Hangar)
            },
        )
        .is_err());

    // Courier cargo skips merge_at_destination even when matching type_id exists
    let moved_courier = snap
        .transfer_inventory_item(
            courier_src,
            TransferItemSpec {
                merge_at_destination: true,
                ..TransferItemSpec::move_whole(Some(owner_id), Some(owner_id), None, box_id, InventoryFlag::Cargo)
            },
        )
        .unwrap();
    assert_eq!(moved_courier.item_id, courier_src);
    assert_eq!(moved_courier.location_id, box_id);
    assert_eq!(snap.items[&courier_dst].quantity, 6);

    // Partial split creating a brand new stack (no merge target)
    let split_fresh = snap
        .transfer_inventory_item(
            stack_b,
            TransferItemSpec {
                quantity: Some(3),
                ..TransferItemSpec::move_whole(Some(owner_id), Some(owner_id), None, struct_id, InventoryFlag::Hangar)
            },
        )
        .unwrap();
    assert_ne!(split_fresh.item_id, stack_b);
    assert_eq!(split_fresh.quantity, 3);
    assert_eq!(snap.items[&stack_b].quantity, 15);

    // Partial merge into destination stack, full merge into destination stack, and overflow guard
    let merged_part = snap
        .transfer_inventory_item(
            stack_b,
            TransferItemSpec {
                quantity: Some(5),
                merge_at_destination: true,
                ..TransferItemSpec::move_whole(Some(owner_id), Some(owner_id), None, struct_id, InventoryFlag::Hangar)
            },
        )
        .unwrap();
    assert_eq!(merged_part.item_id, split_fresh.item_id);
    assert_eq!(merged_part.quantity, 8);
    assert_eq!(snap.items[&stack_b].quantity, 10);

    let merged_full = snap
        .transfer_inventory_item(
            stack_c,
            TransferItemSpec {
                merge_at_destination: true,
                ..TransferItemSpec::move_whole(Some(owner_id), Some(owner_id), None, struct_id, InventoryFlag::Hangar)
            },
        )
        .unwrap();
    assert_eq!(merged_full.item_id, split_fresh.item_id);
    assert_eq!(merged_full.quantity, 13);
    assert!(!snap.items.contains_key(&stack_c));

    snap.items.get_mut(&split_fresh.item_id).unwrap().quantity = i32::MAX;
    assert!(snap
        .transfer_inventory_item(
            stack_b,
            TransferItemSpec {
                merge_at_destination: true,
                ..TransferItemSpec::move_whole(Some(owner_id), Some(owner_id), None, struct_id, InventoryFlag::Hangar)
            },
        )
        .is_err());
}

#[test]
fn move_stack_split_delete_and_acl_missing_branches() {
    let repo = WorldRepository::with_demo_seed();
    let cid = 90_000_001_i64;
    let struct_id = 1_055_400_001_i64;
    let rig_item = 810_001_i64;
    let non_empty_box = 810_002_i64;
    let inner_locked_item = 810_003_i64;
    let stack_a = 810_004_i64;
    let stack_b = 810_005_i64;
    let locked_box = 810_006_i64;

    repo.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&cid).unwrap();
        ch.station_id = None;
        ch.structure_id = Some(struct_id);
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35_832,
                owner_corp_id: 98_000_009,
                solar_system_id: 30_000_142,
                name: "ACL Box Struct".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 100.0,
                armor_hp: 100.0,
                hull_hp: 100.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.items.insert(
            rig_item,
            InventoryItemRecord {
                item_id: rig_item,
                type_id: 31_790,
                owner_id: cid,
                location_id: struct_id,
                flag: InventoryFlag::RigSlot0,
                quantity: 1,
                ..InventoryItemRecord::default()
            },
        );
        snap.items.insert(
            non_empty_box,
            InventoryItemRecord {
                item_id: non_empty_box,
                type_id: 3293,
                owner_id: cid,
                location_id: struct_id,
                flag: InventoryFlag::Hangar,
                quantity: 2,
                singleton: false,
                ..InventoryItemRecord::default()
            },
        );
        snap.items.insert(
            inner_locked_item,
            InventoryItemRecord {
                item_id: inner_locked_item,
                type_id: 34,
                owner_id: cid,
                location_id: non_empty_box,
                flag: InventoryFlag::Locked,
                quantity: 5,
                ..InventoryItemRecord::default()
            },
        );
        snap.items.insert(
            stack_a,
            InventoryItemRecord {
                item_id: stack_a,
                type_id: 34,
                owner_id: cid,
                location_id: struct_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 2,
                ..InventoryItemRecord::default()
            },
        );
        snap.items.insert(
            stack_b,
            InventoryItemRecord {
                item_id: stack_b,
                type_id: 34,
                owner_id: cid,
                location_id: struct_id,
                flag: InventoryFlag::Hangar,
                quantity: 5,
                ..InventoryItemRecord::default()
            },
        );
        snap.items.insert(
            locked_box,
            InventoryItemRecord {
                item_id: locked_box,
                type_id: 3293,
                owner_id: cid,
                location_id: struct_id,
                flag: InventoryFlag::Locked,
                quantity: 1,
                ..InventoryItemRecord::default()
            },
        );

        // has_structure_acl_access branches: unknown char (corp 0) + group with no allow + allow mismatch
        snap.item_custom_names.insert(
            -7_700_001,
            serde_json::json!({
                "access_profiles": {
                    struct_id.to_string(): {
                        "group_ids": ["g1"],
                        "allowed_characters": [cid]
                    }
                },
                "access_groups": {
                    "g1": {
                        "members": {
                            "123": 1
                        }
                    }
                }
            })
            .to_string(),
        );
        assert!(!snap.has_structure_acl_access(struct_id, 90_999_999));
        snap.item_custom_names.remove(&-7_700_001);
    });

    // move_item_transactional fitted structure rig + locked destination + source ACL denied
    assert!(repo
        .move_item_transactional(cid, rig_item, struct_id, InventoryFlag::Hangar, None, None)
        .is_err());
    assert!(repo
        .move_item_transactional(cid, stack_b, struct_id, InventoryFlag::RigSlot0, None, None)
        .is_err());
    repo.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&cid).unwrap().active_ship_id = locked_box;
    });
    assert_eq!(
        repo.move_item_transactional(cid, stack_b, locked_box, InventoryFlag::Cargo, None, None),
        Err(DbError::ItemLocked(locked_box))
    );
    repo.with_mut_snapshot(|snap| {
        snap.structure_docking_acl_denied
            .entry(struct_id)
            .or_default()
            .insert(cid);
    });
    assert!(repo
        .move_item_transactional(cid, stack_b, struct_id, InventoryFlag::Hangar, None, None)
        .is_err());
    repo.with_mut_snapshot(|snap| {
        snap.structure_docking_acl_denied.remove(&struct_id);
    });

    // stack_items, split_stack, and delete_item fitted/non-empty/locked-child branches
    assert!(repo.stack_items(cid, stack_a, stack_b).is_err());
    assert_eq!(repo.split_stack(cid, stack_a, 1), Err(DbError::ItemLocked(stack_a)));
    assert!(repo.split_stack(cid, non_empty_box, 1).is_err());
    assert!(repo.delete_item(cid, rig_item).is_err());
    assert_eq!(
        repo.delete_item(cid, non_empty_box),
        Err(DbError::ItemLocked(inner_locked_item))
    );
}

#[test]
fn add_or_stack_item_never_overwrites_existing_item_and_rejects_i32_overflow() {
    let repo = WorldRepository::with_demo_seed();
    let owner_id = 90_000_001_i64;
    let station_id = 60_003_760_i64;

    // Seed an item at `next_entity_id` and `next_entity_id + 1` to simulate high-ID pre-seeded records.
    let (colliding_id_1, colliding_id_2) = repo.with_mut_snapshot(|snap| {
        let base = snap.next_entity_id;
        for (offset, type_id) in [(0_i64, 587_u32), (1_i64, 588_u32)] {
            let id = base + offset;
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id,
                    owner_id,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    ..InventoryItemRecord::default()
                },
            );
        }
        (base, base + 1)
    });

    let added = repo
        .add_or_stack_item(owner_id, station_id, InventoryFlag::Cargo, 34, 250)
        .expect("add_or_stack_item should allocate a fresh non-colliding item_id");
    assert_ne!(added.item_id, colliding_id_1);
    assert_ne!(added.item_id, colliding_id_2);
    assert_eq!(added.quantity, 250);

    repo.with_snapshot(|snap| {
        assert_eq!(snap.items.get(&colliding_id_1).map(|i| i.type_id), Some(587));
        assert_eq!(snap.items.get(&colliding_id_2).map(|i| i.type_id), Some(588));
        assert_eq!(snap.items.get(&added.item_id).map(|i| i.quantity), Some(250));
    });

    // Reject quantity > i32::MAX on creation
    let overflow_new = repo.add_or_stack_item(
        owner_id,
        station_id,
        InventoryFlag::Cargo,
        35,
        i64::from(i32::MAX) + 1,
    );
    assert!(matches!(overflow_new, Err(DbError::ValidationError(_))));

    // Reject stack overflow when adding to an existing stack
    let overflow_stack = repo.add_or_stack_item(
        owner_id,
        station_id,
        InventoryFlag::Cargo,
        34,
        i64::from(i32::MAX),
    );
    assert!(matches!(overflow_stack, Err(DbError::ValidationError(_))));
}

#[test]
fn issue_887_and_849_merge_overflow_preserves_source_and_recursive_owner_transfer() {
    let repo = WorldRepository::with_demo_seed();
    let cid = 90_000_001_i64;
    let corp_id = 98_000_001_i64;
    let station_id = repo.get_character(cid).unwrap().station_id.unwrap();

    let src_stack_id = 910_887_001_i64;
    let dst_stack_id = 910_887_002_i64;
    let ship_box_id = 910_849_001_i64;
    let mod_child_id = 910_849_002_i64;
    let nested_cont_id = 910_849_003_i64;
    let deep_ammo_id = 910_849_004_i64;

    repo.with_mut_snapshot(|snap| {
        // Remove demo-seeded Tritanium (type 34) in cid's hangar so dst_stack_id is the sole merge target.
        snap.items.retain(|_, it| {
            !(it.owner_id == cid
                && it.location_id == station_id
                && it.flag == InventoryFlag::Hangar
                && it.type_id == 34)
        });
        snap.items.insert(
            src_stack_id,
            InventoryItemRecord {
                item_id: src_stack_id,
                type_id: 34,
                owner_id: cid,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 500,
                singleton: false,
                version: 1,
                ..InventoryItemRecord::default()
            },
        );
        snap.items.insert(
            dst_stack_id,
            InventoryItemRecord {
                item_id: dst_stack_id,
                type_id: 34,
                owner_id: cid,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: i32::MAX - 100,
                singleton: false,
                version: 1,
                ..InventoryItemRecord::default()
            },
        );

        // #887: full-stack merge overflow must fail BEFORE removing src_stack_id
        let mut full_spec = TransferItemSpec::move_whole(
            Some(cid),
            Some(cid),
            None,
            station_id,
            InventoryFlag::Hangar,
        );
        full_spec.merge_at_destination = true;
        assert!(snap.transfer_inventory_item(src_stack_id, full_spec).is_err());
        assert_eq!(
            snap.items.get(&src_stack_id).map(|i| i.quantity),
            Some(500),
            "full merge overflow must not remove source stack"
        );
        assert_eq!(
            snap.items.get(&dst_stack_id).map(|i| i.quantity),
            Some(i32::MAX - 100)
        );

        // #887: partial-stack merge overflow must fail BEFORE decrementing src_stack_id
        let mut partial_spec = TransferItemSpec::move_whole(
            Some(cid),
            Some(cid),
            None,
            station_id,
            InventoryFlag::Hangar,
        );
        partial_spec.quantity = Some(250);
        partial_spec.merge_at_destination = true;
        assert!(snap.transfer_inventory_item(src_stack_id, partial_spec).is_err());
        assert_eq!(
            snap.items.get(&src_stack_id).map(|i| i.quantity),
            Some(500),
            "partial merge overflow must not decrement source stack"
        );

        // #849: transferring a container/ship to a new owner recursively updates nested contents' owner_id
        snap.items.insert(
            ship_box_id,
            InventoryItemRecord {
                item_id: ship_box_id,
                type_id: 587,
                owner_id: cid,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..InventoryItemRecord::default()
            },
        );
        snap.items.insert(
            mod_child_id,
            InventoryItemRecord {
                item_id: mod_child_id,
                type_id: 440,
                owner_id: cid,
                location_id: ship_box_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..InventoryItemRecord::default()
            },
        );
        snap.items.insert(
            nested_cont_id,
            InventoryItemRecord {
                item_id: nested_cont_id,
                type_id: 3293,
                owner_id: cid,
                location_id: ship_box_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                version: 1,
                ..InventoryItemRecord::default()
            },
        );
        snap.items.insert(
            deep_ammo_id,
            InventoryItemRecord {
                item_id: deep_ammo_id,
                type_id: 222,
                owner_id: cid,
                location_id: nested_cont_id,
                flag: InventoryFlag::Cargo,
                quantity: 100,
                singleton: false,
                version: 1,
                ..InventoryItemRecord::default()
            },
        );

        let corp_spec = TransferItemSpec::move_whole(
            Some(cid),
            Some(cid),
            Some(corp_id),
            station_id,
            InventoryFlag::CorpSAG1,
        );
        let outcome = snap
            .transfer_inventory_item(ship_box_id, corp_spec)
            .expect("transfer ship to corp hangar");
        assert_eq!(outcome.owner_id, corp_id);
        for id in [ship_box_id, mod_child_id, nested_cont_id, deep_ammo_id] {
            assert_eq!(
                snap.items.get(&id).map(|i| i.owner_id),
                Some(corp_id),
                "nested descendant {id} must have owner_id updated to {corp_id}"
            );
        }
    });
}

#[test]
fn stack_all_items_preserves_eligibility_guards_tiebreak_and_overflow() {
    let repo = WorldRepository::with_demo_seed();
    let cid = 90_000_001_i64;
    let ch = repo.get_character(cid).unwrap();
    let station_id = ch.station_id.unwrap();

    let mk = |id: i64, type_id: u32, owner: i64, loc: i64, flag: InventoryFlag, qty: i32| {
        InventoryItemRecord {
            item_id: id,
            type_id,
            owner_id: owner,
            location_id: loc,
            flag,
            quantity: qty,
            singleton: false,
            version: 1,
            ..InventoryItemRecord::default()
        }
    };

    // Valid Tritanium (type 34) stacks inserted in non-sorted ID order + released/finished items
    let id_valid_high = 800_100_005_i64;
    let id_valid_low = 800_100_002_i64;
    let id_finished_contract = 800_100_006_i64;
    let id_delivered_job = 800_100_007_i64;
    let id_completed_trade = 800_100_008_i64;
    let id_expired_raffle = 800_100_009_i64;
    let id_paid_wrap_item = 800_100_010_i64;

    // Ineligible stacks (all type 34)
    let id_tube = 800_100_020_i64;
    let id_singleton = 800_100_021_i64;
    let id_bpc = 800_100_022_i64;
    let id_runs = 800_100_023_i64;
    let id_dyn = 800_100_024_i64;
    let id_muta = 800_100_025_i64;
    let id_courier = 800_100_026_i64;
    let id_alsc = 800_100_027_i64;
    let id_contract_out = 800_100_028_i64;
    let id_contract_prog = 800_100_029_i64;
    let id_job_active = 800_100_030_i64;
    let id_job_paused = 800_100_031_i64;
    let id_trade_a = 800_100_032_i64;
    let id_trade_b = 800_100_033_i64;
    let id_raffle_active = 800_100_034_i64;
    let id_unpaid_wrap = 800_100_035_i64;
    let id_non_empty_box = 800_100_036_i64;
    let id_non_empty_child = 800_100_037_i64;
    let id_zero_qty = 800_100_038_i64;
    let id_other_owner = 800_100_039_i64;
    let id_flag_locked = 800_100_040_i64;
    let id_flag_impounded = 800_100_041_i64;
    let id_flag_asset_safety = 800_100_042_i64;
    let id_pwrap_type = 800_100_043_i64;

    // Ancestor-locked containers & nested duplicate stacks
    let box_contract = 800_100_100_i64;
    let box_contract_c1 = 800_100_101_i64;
    let box_contract_c2 = 800_100_102_i64;
    let box_impounded = 800_100_110_i64;
    let box_impounded_c1 = 800_100_111_i64;
    let box_impounded_c2 = 800_100_112_i64;
    let box_alsc = 800_100_120_i64;
    let box_alsc_c1 = 800_100_121_i64;
    let box_alsc_c2 = 800_100_122_i64;

    // Overflow test (type 35 Pyerite)
    let id_ovf_dst = 800_100_201_i64;
    let id_ovf_big = 800_100_202_i64;
    let id_ovf_small = 800_100_203_i64;

    repo.with_mut_snapshot(|snap| {
        // Remove any demo-seed hangar stacks so assertions are 100% deterministic.
        let seed_cleanup: Vec<i64> = snap
            .items
            .values()
            .filter(|i| {
                i.owner_id == cid
                    && i.location_id == station_id
                    && i.flag == InventoryFlag::Hangar
                    && i.item_id != ch.active_ship_id
            })
            .map(|i| i.item_id)
            .collect();
        for sid in seed_cleanup {
            snap.items.remove(&sid);
        }

        for (id, qty) in [
            (id_valid_high, 30),
            (id_valid_low, 20),
            (id_finished_contract, 5),
            (id_delivered_job, 6),
            (id_completed_trade, 7),
            (id_expired_raffle, 8),
            (id_paid_wrap_item, 9),
        ] {
            snap.items
                .insert(id, mk(id, 34, cid, station_id, InventoryFlag::Hangar, qty));
        }

        for id in [
            id_tube,
            id_alsc,
            id_contract_out,
            id_contract_prog,
            id_job_active,
            id_job_paused,
            id_trade_a,
            id_trade_b,
            id_raffle_active,
            id_unpaid_wrap,
            id_non_empty_box,
        ] {
            snap.items
                .insert(id, mk(id, 34, cid, station_id, InventoryFlag::Hangar, 10));
        }
        snap.items.insert(
            id_non_empty_child,
            mk(id_non_empty_child, 34, cid, id_non_empty_box, InventoryFlag::Cargo, 4),
        );

        let mut single = mk(id_singleton, 34, cid, station_id, InventoryFlag::Hangar, 1);
        single.singleton = true;
        snap.items.insert(id_singleton, single);

        let mut bpc = mk(id_bpc, 34, cid, station_id, InventoryFlag::Hangar, 2);
        bpc.is_blueprint_copy = true;
        snap.items.insert(id_bpc, bpc);

        let mut runs = mk(id_runs, 34, cid, station_id, InventoryFlag::Hangar, 2);
        runs.blueprint_runs_remaining = Some(5);
        snap.items.insert(id_runs, runs);

        let mut dyn_rec = mk(id_dyn, 34, cid, station_id, InventoryFlag::Hangar, 2);
        dyn_rec.dynamic_attributes.insert(1, 2.0);
        snap.items.insert(id_dyn, dyn_rec);

        let mut muta = mk(id_muta, 34, cid, station_id, InventoryFlag::Hangar, 2);
        muta.source_mutaplasmid_id = Some(47_740);
        snap.items.insert(id_muta, muta);

        snap.items.insert(
            id_courier,
            mk(id_courier, 34, cid, station_id, InventoryFlag::Hangar, 10),
        );
        snap.item_custom_names
            .insert(id_courier, "courier_cargo:55".into());

        snap.items
            .insert(id_zero_qty, mk(id_zero_qty, 34, cid, station_id, InventoryFlag::Hangar, 0));
        snap.items.insert(
            id_other_owner,
            mk(id_other_owner, 34, cid + 99, station_id, InventoryFlag::Hangar, 10),
        );
        snap.items.insert(
            id_flag_locked,
            mk(id_flag_locked, 34, cid, station_id, InventoryFlag::Locked, 10),
        );
        snap.items.insert(
            id_flag_impounded,
            mk(id_flag_impounded, 34, cid, station_id, InventoryFlag::Impounded, 10),
        );
        snap.items.insert(
            id_flag_asset_safety,
            mk(
                id_flag_asset_safety,
                34,
                cid,
                station_id,
                InventoryFlag::from_u16(FLAG_ASSET_SAFETY),
                10,
            ),
        );
        snap.items.insert(
            id_pwrap_type,
            mk(id_pwrap_type, TYPE_PLASTIC_WRAP, cid, station_id, InventoryFlag::Hangar, 10),
        );
        // Second plastic wrap item so pair check would trigger if type wasn't excluded
        snap.items.insert(
            id_pwrap_type + 1,
            mk(
                id_pwrap_type + 1,
                TYPE_PLASTIC_WRAP,
                cid,
                station_id,
                InventoryFlag::Hangar,
                10,
            ),
        );

        // Contracts (Outstanding + InProgress vs Finished)
        for (c_id, status, item_ids) in [
            (910_001_i64, "Outstanding", vec![id_contract_out, box_contract]),
            (910_002_i64, "InProgress", vec![id_contract_prog]),
            (910_003_i64, "Finished", vec![id_finished_contract]),
        ] {
            snap.contracts.insert(
                c_id,
                ContractRecord {
                    contract_id: c_id,
                    issuer_id: cid,
                    contract_type: "ItemExchange".into(),
                    status: status.into(),
                    start_station_id: station_id,
                    end_station_id: station_id,
                    item_ids,
                    ..ContractRecord::default()
                },
            );
        }

        // Industry jobs (Active + Paused vs Delivered)
        for (j_id, status, bp_id) in [
            (920_001_i64, "Active", id_job_active),
            (920_002_i64, "Paused", id_job_paused),
            (920_003_i64, "Delivered", id_delivered_job),
        ] {
            snap.industry_jobs.insert(
                j_id,
                IndustryJobRecord {
                    job_id: j_id,
                    installer_id: cid,
                    facility_id: station_id,
                    blueprint_item_id: bp_id,
                    status: status.into(),
                    ..IndustryJobRecord::default()
                },
            );
        }

        // Direct trades (-73): active vs completed
        snap.owner_notes.insert(
            -73_101,
            OwnerNoteRecord {
                note_id: -73_101,
                character_id: -73,
                item_id: 1,
                label: "trade-active".into(),
                note: serde_json::json!({
                    "trade_id": 8801_i64,
                    "station_id": 0_i64,
                    "items_a": [[id_trade_a, 10]],
                    "items_b": [[id_trade_b, 10]],
                    "completed": false,
                    "cancelled": false
                })
                .to_string(),
                timestamp_unix: 1,
            },
        );
        snap.owner_notes.insert(
            -73_102,
            OwnerNoteRecord {
                note_id: -73_102,
                character_id: -73,
                item_id: 1,
                label: "trade-done".into(),
                note: serde_json::json!({
                    "trade_id": 8802_i64,
                    "items_a": [[id_completed_trade, 7]],
                    "items_b": [],
                    "completed": true,
                    "cancelled": false
                })
                .to_string(),
                timestamp_unix: 1,
            },
        );

        // HyperNet raffles (-74): active vs expired
        snap.owner_notes.insert(
            -74_101,
            OwnerNoteRecord {
                note_id: -74_101,
                character_id: -74,
                item_id: 1,
                label: "raffle-active".into(),
                note: serde_json::json!({
                    "item_id": id_raffle_active,
                    "winner_id": null,
                    "expired": false
                })
                .to_string(),
                timestamp_unix: 1,
            },
        );
        snap.owner_notes.insert(
            -74_102,
            OwnerNoteRecord {
                note_id: -74_102,
                character_id: -74,
                item_id: 1,
                label: "raffle-expired".into(),
                note: serde_json::json!({
                    "item_id": id_expired_raffle,
                    "winner_id": null,
                    "expired": true
                })
                .to_string(),
                timestamp_unix: 1,
            },
        );

        // ALSC lock records (`Ns::AlscLockedItem`)
        snap.kv_mut(crate::Ns::AlscLockedItem)
            .set(id_alsc, "", &true)
            .expect("lock id_alsc");
        snap.kv_mut(crate::Ns::AlscLockedItem)
            .set(box_alsc, "", &true)
            .expect("lock box_alsc");

        // Unpaid vs paid Asset Safety wraps (-7_700_001)
        snap.item_custom_names.insert(
            STRUCTURE_EXT_STATE_KEY,
            serde_json::json!({
                "asset_safety_wraps": {
                    "950001": {
                        "wrap_id": 950_001_i64,
                        "retrieval_fee_paid": false,
                        "item_ids": [id_unpaid_wrap]
                    },
                    "950002": {
                        "wrap_id": 950_002_i64,
                        "retrieval_fee_paid": true,
                        "item_ids": [id_paid_wrap_item]
                    }
                }
            })
            .to_string(),
        );

        // Locked ancestor containers + children
        for (box_id, flag, c1, c2) in [
            (box_contract, InventoryFlag::Hangar, box_contract_c1, box_contract_c2),
            (box_impounded, InventoryFlag::Impounded, box_impounded_c1, box_impounded_c2),
            (box_alsc, InventoryFlag::Hangar, box_alsc_c1, box_alsc_c2),
        ] {
            let mut b = mk(box_id, 3293, cid, station_id, flag, 1);
            b.singleton = true;
            snap.items.insert(box_id, b);
            snap.items
                .insert(c1, mk(c1, 34, cid, box_id, InventoryFlag::Cargo, 15));
            snap.items
                .insert(c2, mk(c2, 34, cid, box_id, InventoryFlag::Cargo, 25));
        }

        // Pyerite overflow group
        snap.items.insert(
            id_ovf_dst,
            mk(id_ovf_dst, 35, cid, station_id, InventoryFlag::Hangar, 2_000_000_000),
        );
        snap.items.insert(
            id_ovf_big,
            mk(id_ovf_big, 35, cid, station_id, InventoryFlag::Hangar, 2_000_000_000),
        );
        snap.items.insert(
            id_ovf_small,
            mk(id_ovf_small, 35, cid, station_id, InventoryFlag::Hangar, 15),
        );
    });

    let mut extra_excluded = HashSet::new();
    extra_excluded.insert(id_tube);

    let commits_before = repo.tx_commit_count();
    let outcome = repo
        .stack_all_items(cid, station_id, InventoryFlag::Hangar, &extra_excluded)
        .unwrap();
    assert_eq!(repo.tx_commit_count(), commits_before + 1);
    // 6 Tritanium merges into id_valid_low + 1 Pyerite merge (id_ovf_small into id_ovf_dst) = 7
    assert_eq!(outcome.merged_count, 7);
    assert_eq!(outcome.survivors.len(), 2);

    // Lowest ID Tritanium survivor receives sum = 20+30+5+6+7+8+9 = 85; old_quantity = 20
    let trit = repo.get_item(id_valid_low).expect("lowest Tritanium ID must survive");
    assert_eq!(trit.quantity, 85);
    assert_eq!(trit.version, 2);
    for removed in [
        id_valid_high,
        id_finished_contract,
        id_delivered_job,
        id_completed_trade,
        id_expired_raffle,
        id_paid_wrap_item,
    ] {
        assert!(repo.get_item(removed).is_none(), "merged source {removed} must be deleted");
    }

    // Overflow skip: id_ovf_big stays intact while id_ovf_small merged into id_ovf_dst
    assert_eq!(repo.get_item(id_ovf_dst).unwrap().quantity, 2_000_000_015);
    assert_eq!(repo.get_item(id_ovf_big).unwrap().quantity, 2_000_000_000);
    assert!(repo.get_item(id_ovf_small).is_none());

    // All ineligible stacks remain untouched
    for kept in [
        id_tube,
        id_singleton,
        id_bpc,
        id_runs,
        id_dyn,
        id_muta,
        id_courier,
        id_alsc,
        id_contract_out,
        id_contract_prog,
        id_job_active,
        id_job_paused,
        id_trade_a,
        id_trade_b,
        id_raffle_active,
        id_unpaid_wrap,
        id_non_empty_box,
        id_zero_qty,
        id_other_owner,
        id_flag_locked,
        id_flag_impounded,
        id_flag_asset_safety,
        id_pwrap_type,
        id_pwrap_type + 1,
    ] {
        assert!(repo.get_item(kept).is_some(), "ineligible stack {kept} must remain intact");
    }

    // Re-running StackAll and targeting locked/fitted flags or locked ancestor containers merges 0 & commits 0
    let commits_idle = repo.tx_commit_count();
    let noop = repo
        .stack_all_items(cid, station_id, InventoryFlag::Hangar, &extra_excluded)
        .unwrap();
    assert_eq!(noop.merged_count, 0);
    assert_eq!(repo.tx_commit_count(), commits_idle);

    for locked_loc in [box_contract, box_impounded, box_alsc] {
        let locked_out = repo
            .stack_all_items(cid, locked_loc, InventoryFlag::Cargo, &HashSet::new())
            .unwrap();
        assert_eq!(locked_out.merged_count, 0);
    }
    let locked_flag_out = repo
        .stack_all_items(cid, station_id, InventoryFlag::Locked, &HashSet::new())
        .unwrap();
    assert_eq!(locked_flag_out.merged_count, 0);
    assert_eq!(repo.tx_commit_count(), commits_idle);
}

#[test]
fn stack_all_items_single_commit_and_linear_scaling() {
    let repo = WorldRepository::with_demo_seed();
    let cid = 90_000_001_i64;
    let ch = repo.get_character(cid).unwrap();
    let station_id = ch.station_id.unwrap();

    let count_per_type = 400_i64;
    let type_ids = [34_u32, 35_u32, 36_u32];
    repo.with_mut_snapshot(|snap| {
        let seed_cleanup: Vec<i64> = snap
            .items
            .values()
            .filter(|i| {
                i.owner_id == cid
                    && i.location_id == station_id
                    && i.flag == InventoryFlag::Hangar
                    && i.item_id != ch.active_ship_id
            })
            .map(|i| i.item_id)
            .collect();
        for sid in seed_cleanup {
            snap.items.remove(&sid);
        }

        let mut next_id = 850_000_000_i64;
        for &tid in &type_ids {
            for idx in 0..count_per_type {
                let id = next_id;
                next_id += 1;
                snap.items.insert(
                    id,
                    InventoryItemRecord {
                        item_id: id,
                        type_id: tid,
                        owner_id: cid,
                        location_id: station_id,
                        flag: InventoryFlag::Hangar,
                        quantity: (idx as i32) + 1,
                        singleton: false,
                        version: 1,
                        ..InventoryItemRecord::default()
                    },
                );
            }
        }

        // Populate contracts, jobs, trades, and raffles so any O(N^2) scan would stand out.
        for i in 0..200_i64 {
            snap.contracts.insert(
                860_000 + i,
                ContractRecord {
                    contract_id: 860_000 + i,
                    issuer_id: cid,
                    contract_type: "ItemExchange".into(),
                    status: "Outstanding".into(),
                    start_station_id: station_id,
                    end_station_id: station_id,
                    item_ids: vec![890_000_000 + i],
                    ..ContractRecord::default()
                },
            );
            snap.industry_jobs.insert(
                870_000 + i,
                IndustryJobRecord {
                    job_id: 870_000 + i,
                    installer_id: cid,
                    facility_id: station_id,
                    blueprint_item_id: 891_000_000 + i,
                    status: "Active".into(),
                    ..IndustryJobRecord::default()
                },
            );
            snap.owner_notes.insert(
                -73_000 - i,
                OwnerNoteRecord {
                    note_id: -73_000 - i,
                    character_id: -73,
                    item_id: 1,
                    label: "t".into(),
                    note: serde_json::json!({
                        "trade_id": 70_000 + i,
                        "items_a": [[892_000_000 + i, 1]],
                        "items_b": [],
                        "completed": false,
                        "cancelled": false
                    })
                    .to_string(),
                    timestamp_unix: 1,
                },
            );
            snap.owner_notes.insert(
                -74_000 - i,
                OwnerNoteRecord {
                    note_id: -74_000 - i,
                    character_id: -74,
                    item_id: 1,
                    label: "r".into(),
                    note: serde_json::json!({
                        "item_id": 893_000_000 + i,
                        "winner_id": null,
                        "expired": false
                    })
                    .to_string(),
                    timestamp_unix: 1,
                },
            );
        }
    });

    let expected_sum: i32 = (1..=count_per_type as i32).sum();
    let commits_before = repo.tx_commit_count();
    let started = std::time::Instant::now();
    let outcome = repo
        .stack_all_items(cid, station_id, InventoryFlag::Hangar, &HashSet::new())
        .unwrap();
    let elapsed = started.elapsed();

    assert_eq!(repo.tx_commit_count(), commits_before + 1);
    assert_eq!(outcome.merged_count, (count_per_type - 1) * (type_ids.len() as i64));
    assert_eq!(
        outcome.deleted_sources.len(),
        ((count_per_type - 1) * (type_ids.len() as i64)) as usize
    );
    assert_eq!(outcome.survivors.len(), type_ids.len());
    for (idx, surv) in outcome.survivors.iter().enumerate() {
        assert_eq!(surv.old_quantity, 1);
        assert_eq!(surv.updated.type_id, type_ids[idx]);
        assert_eq!(surv.updated.quantity, expected_sum);
        assert_eq!(surv.updated.item_id, 850_000_000 + (idx as i64) * count_per_type);
    }
    assert!(
        elapsed < std::time::Duration::from_secs(3),
        "expected linear O(N+M) single-tx stack_all_items across 1,200 items, took {elapsed:?}"
    );
}

#[test]
fn rollback_restore_consumed_stacks_prevents_overwrite_and_duplication_f01() {
    let repo = WorldRepository::with_demo_seed();
    let (cid, other_cid) = (90_000_001_i64, 90_000_002_i64);
    let ch = repo.get_character(cid).unwrap();
    let (ship_id, alt_loc) = (ch.active_ship_id, ch.station_id.unwrap());
    let s1 = repo.add_or_stack_item(cid, ship_id, InventoryFlag::Cargo, 16_274, 500).unwrap();
    repo.with_mut_snapshot(|snap| {
        let reserved = snap.reserve_consume_stack_for_rollback(s1.item_id, 200).unwrap();
        assert_eq!((reserved.1, snap.items[&s1.item_id].quantity), (200, 300));
        snap.rollback_restore_consumed_stacks(&[reserved]);
        let r = &snap.items[&s1.item_id];
        assert_eq!(r.quantity, 500);
        assert!(r.version > s1.version);
    });
    for (case_idx, label) in ["moved", "owner", "locked", "singleton"].iter().enumerate() {
        let s_id = repo.with_mut_snapshot(|snap| {
            let new_id = snap.alloc_entity_id();
            snap.items.insert(new_id, InventoryItemRecord {
                item_id: new_id, type_id: 16_274, owner_id: cid, location_id: ship_id,
                flag: InventoryFlag::SpecializedFuelBay, quantity: 500, version: 1, ..Default::default()
            });
            new_id
        });
        let reserved = repo.with_mut_snapshot(|snap| snap.reserve_consume_stack_for_rollback(s_id, 200).unwrap());
        let orig_post_qty = if case_idx == 3 { 1 } else { 300 };
        repo.with_mut_snapshot(|snap| {
            let it = snap.items.get_mut(&s_id).unwrap();
            match case_idx {
                0 => { it.location_id = alt_loc; it.flag = InventoryFlag::Hangar; }
                1 => it.owner_id = other_cid,
                2 => it.flag = InventoryFlag::Locked,
                _ => { it.singleton = true; it.quantity = 1; }
            }
            snap.rollback_restore_consumed_stacks(&[reserved]);
            assert_eq!(snap.items[&s_id].quantity, orig_post_qty, "{label}");
            let restored: i32 = snap.items.values()
                .filter(|i| i.owner_id == cid && i.location_id == ship_id && i.flag == InventoryFlag::SpecializedFuelBay && i.type_id == 16_274 && !i.singleton)
                .map(|i| i.quantity).sum();
            assert_eq!(restored, 200, "{label}");
            snap.items.retain(|&id, i| id != s1.item_id && (i.location_id != ship_id || i.type_id != 16_274) && id != s_id);
        });
    }
    repo.with_mut_snapshot(|snap| {
        let ov_id = snap.alloc_entity_id();
        snap.items.insert(ov_id, InventoryItemRecord {
            item_id: ov_id, type_id: 16_274, owner_id: cid, location_id: ship_id,
            flag: InventoryFlag::SpecializedAmmoHold, quantity: 100, version: 1, ..Default::default()
        });
        let reserved = snap.reserve_consume_stack_for_rollback(ov_id, 50).unwrap();
        snap.items.get_mut(&ov_id).unwrap().quantity = i32::MAX - 10;
        snap.rollback_restore_consumed_stacks(&[reserved]);
        assert_eq!(snap.items[&ov_id].quantity, i32::MAX - 10);
        let total: i64 = snap.items.values()
            .filter(|i| i.location_id == ship_id && i.flag == InventoryFlag::SpecializedAmmoHold && i.type_id == 16_274)
            .map(|i| i.quantity as i64).sum();
        assert_eq!(total, (i32::MAX as i64) - 10 + 50);
    });
}

#[test]
fn rollback_restore_consumed_stacks_exact_id_dead_ship_and_diverged_diffs_r5() {
    let repo = WorldRepository::with_demo_seed();
    let cid = 90_000_001_i64;
    let station_id = repo.get_character(cid).unwrap().station_id.unwrap();

    let (live_ship_id, dead_ship_id, dead_box_id) = repo.with_mut_snapshot(|snap| {
        let live = snap.alloc_entity_id();
        let dead = snap.alloc_entity_id();
        let dbox = snap.alloc_entity_id();
        for (item_id, type_id, location_id, flag) in [
            (live, 587_u32, station_id, InventoryFlag::Hangar),
            (dead, 587_u32, station_id, InventoryFlag::Hangar),
            (dbox, 23_u32, dead, InventoryFlag::Cargo),
        ] {
            snap.items.insert(
                item_id,
                InventoryItemRecord {
                    item_id,
                    type_id,
                    owner_id: cid,
                    location_id,
                    flag,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        (live, dead, dbox)
    });

    // 1. Exact full-stack depletion + partial depletion must restore in place under pre.item_id with 0 diffs.
    let s_exact = repo
        .add_or_stack_item(cid, live_ship_id, InventoryFlag::Cargo, 16_273, 500)
        .unwrap();
    repo.with_mut_snapshot(|snap| {
        let res_full = snap
            .reserve_consume_stack_for_rollback(s_exact.item_id, 500)
            .unwrap();
        assert!(!snap.items.contains_key(&s_exact.item_id));
        let diffs_full = snap.rollback_restore_consumed_stacks(&[res_full]);
        assert!(diffs_full.is_empty(), "exact pre.item_id restore must emit no spurious diffs");
        let restored = &snap.items[&s_exact.item_id];
        assert_eq!(restored.quantity, 500);
        assert_eq!(restored.version, s_exact.version + 1);

        let pre_partial = restored.clone();
        let res_part = snap
            .reserve_consume_stack_for_rollback(s_exact.item_id, 200)
            .unwrap();
        assert_eq!(snap.items[&s_exact.item_id].quantity, 300);
        let diffs_part = snap.rollback_restore_consumed_stacks(&[res_part]);
        assert!(diffs_part.is_empty(), "clean partial merge back into pre.item_id needs no diff");
        assert_eq!(snap.items[&s_exact.item_id].quantity, pre_partial.quantity);
    });

    // 2. P2-3: non-clean siblings (BPC / mutated) must never absorb rollback, and divergence must emit both diffs.
    let (bpc_id, mut_id, src_id) = repo.with_mut_snapshot(|snap| {
        let bpc = snap.alloc_entity_id();
        let mutated = snap.alloc_entity_id();
        let src = snap.alloc_entity_id();
        snap.items.insert(
            bpc,
            InventoryItemRecord {
                item_id: bpc,
                type_id: 16_274,
                owner_id: cid,
                location_id: live_ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 10,
                singleton: false,
                is_blueprint_copy: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            mutated,
            InventoryItemRecord {
                item_id: mutated,
                type_id: 16_274,
                owner_id: cid,
                location_id: live_ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 10,
                singleton: false,
                source_mutaplasmid_id: Some(47_730),
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            src,
            InventoryItemRecord {
                item_id: src,
                type_id: 16_274,
                owner_id: cid,
                location_id: live_ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        (bpc, mutated, src)
    });
    repo.with_mut_snapshot(|snap| {
        let res = snap.reserve_consume_stack_for_rollback(src_id, 40).unwrap();
        // Diverge remaining stack so rollback cannot merge back into src_id.
        snap.items.get_mut(&src_id).unwrap().singleton = true;
        let diffs = snap.rollback_restore_consumed_stacks(&[res]);
        assert_eq!(snap.items[&bpc_id].quantity, 10, "BPC sibling must not absorb rollback");
        assert_eq!(snap.items[&mut_id].quantity, 10, "mutated sibling must not absorb rollback");
        assert_eq!(snap.items[&src_id].quantity, 60);
        let minted: Vec<&InventoryItemRecord> = snap
            .items
            .values()
            .filter(|i| {
                i.location_id == live_ship_id
                    && i.type_id == 16_274
                    && !matches!(i.item_id, x if x == bpc_id || x == mut_id || x == src_id)
            })
            .collect();
        assert_eq!(minted.len(), 1);
        assert_eq!(minted[0].quantity, 40);
        assert_eq!(diffs.len(), 2, "diverged rollback must emit reservation + minted diffs");
        assert!(diffs.iter().any(|(pre, cur)| {
            pre.as_ref().is_some_and(|p| p.item_id == src_id)
                && cur.as_ref().is_some_and(|c| c.item_id == src_id && c.quantity == 60)
        }));
        assert!(diffs.iter().any(|(pre, cur)| {
            pre.is_none()
                && cur.as_ref().is_some_and(|c| c.item_id == minted[0].item_id && c.quantity == 40)
        }));
    });

    // 3. F1486-2B-R5-01: destroyed ship and nested container must emit deletion tombstones and zero orphans.
    let (ship_fuel_id, box_fuel_id) = repo.with_mut_snapshot(|snap| {
        let f1 = snap.alloc_entity_id();
        let f2 = snap.alloc_entity_id();
        snap.items.insert(
            f1,
            InventoryItemRecord {
                item_id: f1,
                type_id: 16_275,
                owner_id: cid,
                location_id: dead_ship_id,
                flag: InventoryFlag::SpecializedFuelBay,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            f2,
            InventoryItemRecord {
                item_id: f2,
                type_id: 16_275,
                owner_id: cid,
                location_id: dead_box_id,
                flag: InventoryFlag::Cargo,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        (f1, f2)
    });
    repo.with_mut_snapshot(|snap| {
        let r1 = snap.reserve_consume_stack_for_rollback(ship_fuel_id, 100).unwrap();
        let r2 = snap.reserve_consume_stack_for_rollback(box_fuel_id, 100).unwrap();
        snap.items.remove(&dead_ship_id);
        let dead_diffs = snap.rollback_restore_consumed_stacks(&[r1, r2]);
        assert!(
            !snap.items.values().any(|i| i.type_id == 16_275),
            "destroyed ship/ancestor rollback must not insert orphaned fuel stacks"
        );
        assert_eq!(dead_diffs.len(), 2);
        assert!(dead_diffs.iter().all(|(pre, cur)| pre.is_some() && cur.is_none()));
    });

    // 4. P2-R6-01: Destroyed Upwell structures and locked/impounded ancestors must block resurrection/minting.
    let (destroyed_struct_id, locked_box_id, struct_fuel_id, locked_box_fuel_id) = repo
        .with_mut_snapshot(|snap| {
            let sid = snap.alloc_entity_id();
            let lbox = snap.alloc_entity_id();
            let sf = snap.alloc_entity_id();
            let lbf = snap.alloc_entity_id();
            snap.structures.insert(
                sid,
                crate::types::UpwellStructureRecord {
                    structure_id: sid,
                    type_id: 35_832,
                    owner_corp_id: 98_000_001,
                    solar_system_id: 30_000_142,
                    state: "ShieldVulnerable".into(),
                    ..Default::default()
                },
            );
            snap.items.insert(
                sf,
                InventoryItemRecord {
                    item_id: sf,
                    type_id: 16_275,
                    owner_id: cid,
                    location_id: sid,
                    flag: InventoryFlag::Cargo,
                    quantity: 100,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.items.insert(
                lbox,
                InventoryItemRecord {
                    item_id: lbox,
                    type_id: 23,
                    owner_id: cid,
                    location_id: live_ship_id,
                    flag: InventoryFlag::Cargo,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.items.insert(
                lbf,
                InventoryItemRecord {
                    item_id: lbf,
                    type_id: 16_275,
                    owner_id: cid,
                    location_id: lbox,
                    flag: InventoryFlag::Cargo,
                    quantity: 100,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
            (sid, lbox, sf, lbf)
        });
    repo.with_mut_snapshot(|snap| {
        let r_struct = snap.reserve_consume_stack_for_rollback(struct_fuel_id, 100).unwrap();
        let r_lbox = snap.reserve_consume_stack_for_rollback(locked_box_fuel_id, 100).unwrap();
        snap.structures.get_mut(&destroyed_struct_id).unwrap().state = "Destroyed".into();
        snap.items.get_mut(&locked_box_id).unwrap().flag = InventoryFlag::Locked;
        let blocked_diffs = snap.rollback_restore_consumed_stacks(&[r_struct, r_lbox]);
        assert!(
            !snap.items.values().any(|i| i.type_id == 16_275),
            "destroyed structure and locked ancestor rollback must not insert orphaned fuel stacks"
        );
        assert_eq!(blocked_diffs.len(), 2);
        assert!(blocked_diffs.iter().all(|(pre, cur)| pre.is_some() && cur.is_none()));
    });
}

#[test]
fn rollback_restore_consumed_stacks_cross_owner_and_exposed_exact_r7() {
    let repo = WorldRepository::with_demo_seed();
    let owner_a = 90_000_001_i64;
    let owner_b = 90_000_002_i64;
    let ship_id = 990_777_100_i64;
    let stack_id = 990_777_101_i64;

    repo.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 11_567,
                owner_id: owner_a,
                location_id: 30_000_142,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            stack_id,
            InventoryItemRecord {
                item_id: stack_id,
                type_id: 16_274,
                owner_id: owner_a,
                location_id: ship_id,
                flag: InventoryFlag::SpecializedFuelBay,
                quantity: 100,
                singleton: false,
                ..Default::default()
            },
        );

        let res = snap.reserve_consume_stack_for_rollback(stack_id, 40).unwrap();
        // Concurrent transfer changes owner of the surviving stack to owner_b (`BF-1486-2B-R7-02`).
        snap.items.get_mut(&stack_id).unwrap().owner_id = owner_b;

        let diffs = snap.rollback_restore_consumed_stacks(&[res]);
        assert_eq!(diffs.len(), 2, "must tombstone old stack for owner_a and mint fallback stack");
        assert!(
            diffs[0].0.as_ref().is_some_and(|p| p.item_id == stack_id && p.owner_id == owner_a)
                && diffs[0].1.is_none(),
            "cross-owner conflict diff must tombstone only pre's owner, never expose owner_b's stack"
        );
        assert!(
            diffs[1].0.is_none()
                && diffs[1].1.as_ref().is_some_and(|m| m.owner_id == owner_a && m.quantity == 40),
            "minted fallback stack must be owned by owner_a with qty 40"
        );
        assert_eq!(snap.items[&stack_id].owner_id, owner_b);
        assert_eq!(snap.items[&stack_id].quantity, 60);

        // Exposed-exact predicate overrides silent suppression for both merge and full re-insert (`BF-1486-2B-R7-01`).
        let stack_b = 990_777_102_i64;
        snap.items.insert(
            stack_b,
            InventoryItemRecord {
                item_id: stack_b,
                type_id: 16_274,
                owner_id: owner_a,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 500,
                singleton: false,
                ..Default::default()
            },
        );
        let res_part = snap.reserve_consume_stack_for_rollback(stack_b, 200).unwrap();
        let diffs_part = snap.rollback_restore_consumed_stacks_if_exposed(&[res_part], |_, _, _| true);
        assert_eq!(diffs_part.len(), 1);
        assert_eq!(diffs_part[0].0.as_ref().unwrap().quantity, 300);
        assert_eq!(diffs_part[0].1.as_ref().unwrap().quantity, 500);

        let res_full = snap.reserve_consume_stack_for_rollback(stack_b, 500).unwrap();
        let diffs_full = snap.rollback_restore_consumed_stacks_if_exposed(&[res_full], |_, _, _| true);
        assert_eq!(diffs_full.len(), 1);
        assert!(diffs_full[0].0.is_none());
        assert_eq!(diffs_full[0].1.as_ref().unwrap().item_id, stack_b);
        assert_eq!(diffs_full[0].1.as_ref().unwrap().quantity, 500);
    });
}

