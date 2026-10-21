//! Ship repackaging, inventory stack/split/trash, and DNA fitting encoder/decoder integration tests.


use crate::common;

use eve_db::{InventoryFlag, InventoryItemRecord, SavedFittingRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::{dna_to_fitting, fitting_to_dna, saved_fitting_to_dna};
use std::collections::HashMap;

#[tokio::test]
async fn test_inv1_repackage_item() {
    // parity: feat:ships.repackage, rpc:charUnboundMgr.SelectCharacterID, rpc:repackagingSvc.RepackageItems, rpc:repairSvc.RepairItems, rpc:ship.AssembleShip
    eve_testkit::attest(&["feat:ships.repackage"]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(924);

    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

    let char_id = 90_000_001;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    let station_id = 60003760;
    let packaged_item_id = 991_000_201;

    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.station_id = Some(station_id);
        sess.station_id = Some(station_id);

        // Put an unpackaged item in station hangar (singleton = true)
        snap.items.insert(
            packaged_item_id,
            InventoryItemRecord {
                item_id: packaged_item_id,
                type_id: 587, // Rifter
                owner_id: char_id,
                location_id: station_id,
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

    // Repackage item via packaging service
    let rep = ctx
        .dispatch_rpc(
            &mut sess,
            "repackagingSvc",
            None,
            "RepackageItems",
            &[PyRep::Integer(packaged_item_id)],
            &kw,
            None,
        )
        .await
        .expect("RepackageItems");
    let PyRep::KeyVal(kv) = rep else {
        panic!("Expected KeyVal from RepackageItems");
    };
    assert_eq!(kv.get("singleton"), Some(&PyRep::Bool(false)));

    ctx.db.with_snapshot(|snap| {
        let item = &snap.items[&packaged_item_id];
        assert!(!item.singleton, "Item should now be packaged (singleton=false)");
    });

    // Re-assemble the packaged ship via ship.AssembleShip and then repackage via V24 dict(itemsByStation) payload
    let asm_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "ship",
            None,
            "AssembleShip",
            &[PyRep::Integer(packaged_item_id)],
            &kw,
            None,
        )
        .await
        .expect("AssembleShip");
    assert_eq!(asm_rep.dict_get("singleton"), Some(&PyRep::Bool(true)));

    let v24_repack_arg = PyRep::Dict(vec![(
        PyRep::Integer(station_id),
        PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(packaged_item_id),
            PyRep::Integer(station_id),
        ])]),
    )]);
    let v24_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "repackagingSvc",
            None,
            "RepackageItems",
            &[v24_repack_arg],
            &kw,
            None,
        )
        .await
        .expect("RepackageItems V24 dict");
    assert_eq!(v24_rep.dict_get("singleton"), Some(&PyRep::Bool(false)));

    // Error case 1: cannot repackage active ship
    let active_ship_id = ctx.db.with_snapshot(|snap| snap.characters[&char_id].active_ship_id);
    let active_ship_err = ctx
        .dispatch_rpc(
            &mut sess,
            "repackagingSvc",
            None,
            "RepackageItems",
            &[PyRep::Integer(active_ship_id)],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        active_ship_err.unwrap_err().code,
        "CannotRepackageActiveShip"
    );

    // Error case 2: cannot repackage or assemble an item that is currently fitted to a ship (IsShipFittingFlag)
    let fitted_module_id = 991_000_202;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            fitted_module_id,
            InventoryItemRecord {
                item_id: fitted_module_id,
                type_id: 2889, // 200mm AutoCannon I
                owner_id: char_id,
                location_id: active_ship_id,
                flag: InventoryFlag::HiSlot0,
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
    let fitted_repack_err = ctx
        .dispatch_rpc(
            &mut sess,
            "repackagingSvc",
            None,
            "RepackageItems",
            &[PyRep::Integer(fitted_module_id)],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        fitted_repack_err.unwrap_err().code,
        "CanNotUnassembleInThisItemLoc"
    );
    let fitted_asm_err = ctx
        .dispatch_rpc(
            &mut sess,
            "ship",
            None,
            "AssembleShip",
            &[PyRep::Integer(fitted_module_id)],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        fitted_asm_err.unwrap_err().code,
        "CanNotUnassembleInThisItemLoc"
    );

    // Error case 3: cannot repackage or assemble a ship/module with structural or armor damage
    let damaged_ship_id = 991_000_203;
    ctx.db.with_mut_snapshot(|snap| {
        let mut dmg_attrs = HashMap::new();
        dmg_attrs.insert(eve_sde::attr_ids::DAMAGE, 42.0);
        dmg_attrs.insert(eve_sde::attr_ids::ARMOR_DAMAGE, 15.0);
        snap.items.insert(
            damaged_ship_id,
            InventoryItemRecord {
                item_id: damaged_ship_id,
                type_id: 587, // Rifter
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: dmg_attrs,
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    let dmg_repack_err = ctx
        .dispatch_rpc(
            &mut sess,
            "repackagingSvc",
            None,
            "RepackageItems",
            &[PyRep::Integer(damaged_ship_id)],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        dmg_repack_err.unwrap_err().code,
        "CantRepackageDamagedItem"
    );
    let dmg_asm_err = ctx
        .dispatch_rpc(
            &mut sess,
            "ship",
            None,
            "AssembleShip",
            &[PyRep::Integer(damaged_ship_id)],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        dmg_asm_err.unwrap_err().code,
        "CantRepackageDamagedItem"
    );

    // Repairing the damaged ship via repairSvc.RepairItems clears hull/armor damage and allows repackaging
    ctx.dispatch_rpc(
        &mut sess,
        "repairSvc",
        None,
        "RepairItems",
        &[PyRep::List(vec![PyRep::Integer(damaged_ship_id)])],
        &kw,
        None,
    )
    .await
    .expect("RepairItems on damaged hangar ship");
    let repaired_repack = ctx
        .dispatch_rpc(
            &mut sess,
            "repackagingSvc",
            None,
            "RepackageItems",
            &[PyRep::Integer(damaged_ship_id)],
            &kw,
            None,
        )
        .await
        .expect("RepackageItems after repair");
    assert_eq!(repaired_repack.dict_get("singleton"), Some(&PyRep::Bool(false)));

    // Error case 4: cannot repackage while undocked
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().station_id = None;
    });
    sess.station_id = None;
    let undocked_err = ctx
        .dispatch_rpc(
            &mut sess,
            "repackagingSvc",
            None,
            "RepackageItems",
            &[PyRep::Integer(packaged_item_id)],
            &kw,
            None,
        )
        .await;
    assert_eq!(undocked_err.unwrap_err().code, "NotDocked");
}

#[tokio::test]
async fn test_inv2_stack_split_destroy_inventory() {
    // parity: feat:inventory.split-merge, feat:inventory.trash-items, rpc:charUnboundMgr.SelectCharacterID, rpc:invbroker.Add, rpc:invbroker.MultiMerge, rpc:invbroker.SplitStack, rpc:invbroker.StackAll, rpc:invbroker.TrashItems
    eve_testkit::attest(&["feat:inventory.split-merge", "feat:inventory.trash-items"]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(925);

    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

    let char_id = 90_000_001;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    let station_id = 60003760;
    let item_a_id = 991_000_301;
    let item_b_id = 991_000_302;

    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.station_id = Some(station_id);
        sess.station_id = Some(station_id);
        snap.items
            .retain(|_, it| !(it.owner_id == char_id && it.location_id == station_id && it.type_id == 34));
        snap.next_entity_id = snap.next_entity_id.max(item_b_id);

        // Item A: Tritanium (type 34), qty 20
        snap.items.insert(
            item_a_id,
            InventoryItemRecord {
                item_id: item_a_id,
                type_id: 34,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 20,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );

        // Item B: Tritanium (type 34), qty 30
        snap.items.insert(
            item_b_id,
            InventoryItemRecord {
                item_id: item_b_id,
                type_id: 34,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 30,
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

    // 1. Merge stack B (30) into stack A (20) via canonical invbroker.MultiMerge
    let merge_ops = PyRep::List(vec![PyRep::Tuple(vec![
        PyRep::Integer(item_b_id),
        PyRep::Integer(item_a_id),
        PyRep::Integer(30),
    ])]);
    let stack_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "MultiMerge",
            &[merge_ops, PyRep::Integer(station_id)],
            &kw,
            None,
        )
        .await
        .expect("invbroker.MultiMerge");
    let PyRep::KeyVal(stack_kv) = stack_rep else {
        panic!("Expected KeyVal from MultiMerge");
    };
    assert_eq!(stack_kv.get("quantity"), Some(&PyRep::Integer(50)));

    ctx.db.with_snapshot(|snap| {
        let item_a = &snap.items[&item_a_id];
        assert_eq!(item_a.quantity, 50);
        assert!(
            !snap.items.contains_key(&item_b_id),
            "Source stack must be deleted after full merge"
        );
    });

    // 2. Split stack: split 15 out of item A via canonical invbroker.SplitStack
    let split_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "SplitStack",
            &[
                PyRep::Integer(station_id),
                PyRep::Integer(item_a_id),
                PyRep::Integer(15),
                PyRep::Integer(char_id),
            ],
            &kw,
            None,
        )
        .await
        .expect("invbroker.SplitStack");
    let PyRep::KeyVal(split_kv) = split_rep else {
        panic!("Expected KeyVal from SplitStack");
    };
    assert_eq!(split_kv.get("originalQuantity"), Some(&PyRep::Integer(35)));
    assert_eq!(split_kv.get("newQuantity"), Some(&PyRep::Integer(15)));
    let new_item_id = split_kv.get("newItemID").unwrap().as_i64().unwrap();

    ctx.db.with_snapshot(|snap| {
        let item_a = &snap.items[&item_a_id];
        assert_eq!(item_a.quantity, 35);
        let new_item = &snap.items[&new_item_id];
        assert_eq!(new_item.quantity, 15);
        assert_eq!(new_item.type_id, 34);
    });

    // 3. Split-via-Add: move 5 units out of item_a_id (35 -> 30 + 5) using invbroker.Add with qty kwarg
    let mut add_kw = HashMap::new();
    add_kw.insert("flag".to_string(), PyRep::Integer(InventoryFlag::Hangar as i64));
    add_kw.insert("qty".to_string(), PyRep::Integer(5));
    let add_split_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "Add",
            &[PyRep::Integer(item_a_id), PyRep::Integer(station_id)],
            &add_kw,
            None,
        )
        .await
        .expect("invbroker.Add with qty split");
    let split_via_add_id = add_split_rep.dict_get("itemID").unwrap().as_i64().unwrap();
    assert_ne!(split_via_add_id, item_a_id);

    // 4. StackAll in station hangar merges the 5-unit stack and 30-unit stack ( leaving new_item_id separate only after we re-split 15 )
    let stack_all_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "StackAll",
            &[PyRep::Integer(InventoryFlag::Hangar as i64)],
            &kw,
            None,
        )
        .await
        .expect("invbroker.StackAll");
    assert!(
        stack_all_rep
            .dict_get("mergedCount")
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            >= 2
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&item_a_id].quantity, 50);
    });

    // Split 15 again so we can trash the 15-unit stack
    let split_again = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "SplitStack",
            &[PyRep::Integer(item_a_id), PyRep::Integer(15)],
            &kw,
            None,
        )
        .await
        .expect("SplitStack before trash");
    let trash_target_id = split_again.dict_get("newItemID").unwrap().as_i64().unwrap();

    // 5. Trash items via canonical invbroker.TrashItems (feat:inventory.trash-items)
    let destroy_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "TrashItems",
            &[
                PyRep::List(vec![PyRep::Integer(trash_target_id)]),
                PyRep::Integer(60_003_760),
            ],
            &kw,
            None,
        )
        .await
        .expect("TrashItems");
    let PyRep::KeyVal(des_kv) = destroy_rep else {
        panic!("Expected KeyVal from TrashItems");
    };
    assert_eq!(des_kv.get("deleted"), Some(&PyRep::Bool(true)));
    assert_eq!(des_kv.get("count"), Some(&PyRep::Integer(1)));

    ctx.db.with_snapshot(|snap| {
        assert!(
            !snap.items.contains_key(&trash_target_id),
            "Trashed item must be removed from inventory"
        );
    });

    // 4. Verify trashing refusals: fitted items (CannotTrashFittedItem) and locked items (ItemLocked)
    let fitted_item_id = 999_888_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            fitted_item_id,
            eve_db::InventoryItemRecord {
                item_id: fitted_item_id,
                type_id: 3300,
                owner_id: 90_000_001,
                location_id: 100_000_001,
                flag: eve_db::InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let fitted_err = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "TrashItems",
            &[PyRep::List(vec![PyRep::Integer(fitted_item_id)])],
            &kw,
            None,
        )
        .await
        .expect_err("Trashing fitted item must fail");
    assert_eq!(fitted_err.code, "CannotTrashFittedItem");

    let contract_item_id = 999_888_002i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            contract_item_id,
            eve_db::InventoryItemRecord {
                item_id: contract_item_id,
                type_id: 34,
                owner_id: 90_000_001,
                location_id: 60_003_760,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.contracts.insert(
            77001,
            eve_db::ContractRecord {
                contract_id: 77001,
                issuer_id: 90_000_001,
                status: "Outstanding".into(),
                item_ids: vec![contract_item_id],
                ..Default::default()
            },
        );
    });
    let contract_err = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "TrashItems",
            &[PyRep::List(vec![PyRep::Integer(contract_item_id)])],
            &kw,
            None,
        )
        .await
        .expect_err("Trashing contract-locked item must fail");
    assert_eq!(contract_err.code, "ItemLocked");

    let industry_item_id = 999_888_003i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            industry_item_id,
            eve_db::InventoryItemRecord {
                item_id: industry_item_id,
                type_id: 687,
                owner_id: 90_000_001,
                location_id: 60_003_760,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.industry_jobs.insert(
            88001,
            eve_db::IndustryJobRecord {
                job_id: 88001,
                installer_id: 90_000_001,
                facility_id: 60_003_760,
                blueprint_item_id: industry_item_id,
                blueprint_type_id: 687,
                activity_id: 1,
                runs: 1,
                product_type_id: 587,
                status: "Active".into(),
                duration_seconds: 3600,
                start_unix: 1704067200,
            },
        );
    });
    let industry_err = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "TrashItems",
            &[PyRep::List(vec![PyRep::Integer(industry_item_id)])],
            &kw,
            None,
        )
        .await
        .expect_err("Trashing industry-locked item must fail");
    assert_eq!(industry_err.code, "ItemLocked");
}

#[tokio::test]
async fn test_inv3_dna_fitting_string_encoder_decoder() {
    // parity: feat:fitting.fittings-save, rpc:charFittingMgr.DeleteManyFittings, rpc:charFittingMgr.GetFittings, rpc:charFittingMgr.SaveFitting, rpc:charFittingMgr.SaveManyFittings, rpc:charFittingMgr.UpdateNameAndDescription, rpc:charUnboundMgr.SelectCharacterID, rpc:corpFittingMgr.DeleteFitting, rpc:corpFittingMgr.GetFittings, rpc:corpFittingMgr.SaveFitting
    eve_testkit::attest(&["feat:fitting.fittings-save"]);
    // 1. DNA format encode/decode unit checks: <ship_type_id>:<type_id_1>;<qty_1>:<type_id_2>;<qty_2>:...::
    let ship_type_id = 587; // Rifter
    let modules = vec![
        (3300, 2), // Small Projectile Turret x2
        (34, 100), // Tritanium x100
        (3300, 1), // Additional Small Projectile Turret x1 (should aggregate to 3)
    ];

    let dna = fitting_to_dna(ship_type_id, &modules);
    assert_eq!(dna, "587:3300;3:34;100::");

    let (decoded_ship, decoded_items) = dna_to_fitting(&dna).expect("decode dna");
    assert_eq!(decoded_ship, 587);
    assert_eq!(decoded_items, vec![(3300, 3), (34, 100)]);

    let re_encoded = fitting_to_dna(decoded_ship, &decoded_items);
    assert_eq!(re_encoded, dna);

    let saved_fit = SavedFittingRecord {
        fitting_id: 1,
        owner_id: 90_000_001,
        name: "Test Fit".into(),
        description: "DNA test fitting".into(),
        ship_type_id: 587,
        items: vec![
            (3300, InventoryFlag::HiSlot0, 3),
            (34, InventoryFlag::Cargo, 100),
        ],
    };
    let saved_dna = saved_fitting_to_dna(&saved_fit);
    assert_eq!(saved_dna, "587:3300;3:34;100::");

    let (s_id, items) = dna_to_fitting("587:3300::").expect("decode without count");
    assert_eq!(s_id, 587);
    assert_eq!(items, vec![(3300, 1)]);

    assert!(dna_to_fitting("").is_err(), "Empty string should error");
    assert!(
        dna_to_fitting("not_a_number:3300;1::").is_err(),
        "Invalid ship type ID should error"
    );

    // 2. End-to-end charFittingMgr & corpFittingMgr RPCs + SQLite persistence + max fitting limits
    let db_path =
        std::env::temp_dir().join(format!("evomu_fittings_save_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&db_path);
    let repo = std::sync::Arc::new(
        eve_db::WorldRepository::open_sqlite(&db_path).expect("WorldRepository::open_sqlite"),
    );
    repo.with_mut_snapshot(|snap| {
        *snap = eve_db::WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
    });
    let ctx = ServerContext::with_repository(repo);
    let kw = HashMap::new();
    let mut sess = SessionState::new(926);

    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    let fit_kv = PyRep::keyval([
        ("name", PyRep::String("V24 Rifter Brawl".into())),
        ("shipTypeID", PyRep::Integer(587)),
        ("description", PyRep::String("Autocannon scram web Rifter".into())),
        (
            "fitData",
            PyRep::List(vec![
                PyRep::Tuple(vec![
                    PyRep::Integer(2889),
                    PyRep::Integer(InventoryFlag::HiSlot0 as i64),
                    PyRep::Integer(1),
                ]),
                PyRep::Tuple(vec![
                    PyRep::Integer(438),
                    PyRep::Integer(InventoryFlag::MedSlot0 as i64),
                    PyRep::Integer(1),
                ]),
                PyRep::Tuple(vec![
                    PyRep::Integer(1236),
                    PyRep::Integer(InventoryFlag::LoSlot0 as i64),
                    PyRep::Integer(1),
                ]),
            ]),
        ),
    ]);

    let fid1 = ctx
        .dispatch_rpc(
            &mut sess,
            "charFittingMgr",
            None,
            "SaveFitting",
            &[PyRep::Integer(char_id), fit_kv.clone()],
            &kw,
            None,
        )
        .await
        .expect("charFittingMgr.SaveFitting")
        .as_i64()
        .unwrap();

    // SaveManyFittings (bulk import)
    let bulk_dict = PyRep::Dict(vec![
        (
            PyRep::Integer(1),
            PyRep::keyval([
                ("name", PyRep::String("Bulk Rifter 1".into())),
                ("shipTypeID", PyRep::Integer(587)),
                ("description", PyRep::String("Bulk 1".into())),
                ("fitData", PyRep::List(vec![])),
            ]),
        ),
        (
            PyRep::Integer(2),
            PyRep::keyval([
                ("name", PyRep::String("Bulk Rifter 2".into())),
                ("shipTypeID", PyRep::Integer(587)),
                ("description", PyRep::String("Bulk 2".into())),
                ("fitData", PyRep::List(vec![])),
            ]),
        ),
    ]);
    let saved_many = ctx
        .dispatch_rpc(
            &mut sess,
            "charFittingMgr",
            None,
            "SaveManyFittings",
            &[PyRep::Integer(char_id), bulk_dict],
            &kw,
            None,
        )
        .await
        .expect("charFittingMgr.SaveManyFittings");
    let PyRep::List(many_ids) = saved_many else {
        panic!("Expected List of fittingIDs from SaveManyFittings");
    };
    assert_eq!(many_ids.len(), 2);

    // UpdateNameAndDescription
    ctx.dispatch_rpc(
        &mut sess,
        "charFittingMgr",
        None,
        "UpdateNameAndDescription",
        &[
            PyRep::Integer(fid1),
            PyRep::Integer(char_id),
            PyRep::String("Updated Rifter Fit".into()),
            PyRep::String("Updated description".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("charFittingMgr.UpdateNameAndDescription");

    // GetFittings
    let fittings_rowset = ctx
        .dispatch_rpc(
            &mut sess,
            "charFittingMgr",
            None,
            "GetFittings",
            &[PyRep::Integer(char_id)],
            &kw,
            None,
        )
        .await
        .expect("charFittingMgr.GetFittings");
    assert_eq!(fittings_rowset.row_count(), Some(3));

    // Corporate fitting save & retrieve via corpFittingMgr
    let corp_id = sess.corporation_id.unwrap_or(1_000_009);
    let corp_fid = ctx
        .dispatch_rpc(
            &mut sess,
            "corpFittingMgr",
            None,
            "SaveFitting",
            &[PyRep::Integer(corp_id), fit_kv.clone()],
            &kw,
            None,
        )
        .await
        .expect("corpFittingMgr.SaveFitting")
        .as_i64()
        .unwrap();
    let corp_fittings = ctx
        .dispatch_rpc(
            &mut sess,
            "corpFittingMgr",
            None,
            "GetFittings",
            &[PyRep::Integer(corp_id)],
            &kw,
            None,
        )
        .await
        .expect("corpFittingMgr.GetFittings");
    assert_eq!(corp_fittings.row_count(), Some(1));

    // DeleteManyFittings & DeleteFitting
    ctx.dispatch_rpc(
        &mut sess,
        "charFittingMgr",
        None,
        "DeleteManyFittings",
        &[PyRep::Integer(char_id), PyRep::List(many_ids)],
        &kw,
        None,
    )
    .await
    .expect("charFittingMgr.DeleteManyFittings");
    ctx.dispatch_rpc(
        &mut sess,
        "corpFittingMgr",
        None,
        "DeleteFitting",
        &[PyRep::Integer(corp_id), PyRep::Integer(corp_fid)],
        &kw,
        None,
    )
    .await
    .expect("corpFittingMgr.DeleteFitting");

    // Enforce 250 personal fitting limit (OwnerMaxFittings)
    ctx.db.with_mut_snapshot(|snap| {
        for idx in 0..249 {
            snap.next_entity_id += 1;
            let id = snap.next_entity_id;
            snap.saved_fittings.insert(
                id,
                SavedFittingRecord {
                    fitting_id: id,
                    owner_id: char_id,
                    name: format!("Fit {idx}"),
                    ship_type_id: 587,
                    description: String::new(),
                    items: Vec::new(),
                },
            );
        }
    });
    let over_limit_err = ctx
        .dispatch_rpc(
            &mut sess,
            "charFittingMgr",
            None,
            "SaveFitting",
            &[PyRep::Integer(char_id), fit_kv],
            &kw,
            None,
        )
        .await;
    assert_eq!(over_limit_err.unwrap_err().code, "OwnerMaxFittings");

    // Verify SQLite persistence across reload
    let reloaded_db = eve_db::WorldRepository::new_empty_with_snapshot_path(Some(db_path.clone()))
        .expect("reopen SQLite");
    reloaded_db.with_snapshot(|snap| {
        let updated = snap.saved_fittings.get(&fid1).expect("fid1 persisted in SQLite");
        assert_eq!(updated.name, "Updated Rifter Fit");
        assert_eq!(updated.description, "Updated description");
        assert_eq!(updated.items.len(), 3);
    });
    let _ = std::fs::remove_file(&db_path);
}
