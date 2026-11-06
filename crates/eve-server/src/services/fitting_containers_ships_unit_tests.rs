//! Unit and mutation-killing tests for `services::fitting_containers_ships`
//! (containers, ALSC locks/logs, ship bays, volume calculations, item locking, and encoded items).

use super::fitting_containers_ships::*;
use crate::router::{BoundObjectEntry, ServerContext};
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use std::collections::HashMap;
use std::sync::Arc;

fn make_ctx_and_session() -> (Arc<ServerContext>, SessionState) {
    let ctx = Arc::new(ServerContext::with_repository(Arc::new(
        eve_db::WorldRepository::with_demo_seed(),
    )));
    let mut sess = SessionState::new(91_001);
    sess.user_id = Some(1);
    sess.character_id = Some(90_000_001);
    sess.corporation_id = Some(98_000_001);
    sess.station_id = Some(60_003_760);
    sess.solar_system_id = Some(30_000_142);
    sess.ship_id = Some(100_000_001);
    ctx.sync_session(&mut sess);
    (ctx, sess)
}

#[test]
fn container_ship_runtime_state_passwords_and_persistence() {
    let (ctx, _) = make_ctx_and_session();
    let mut st = ContainerShipRuntimeState::default();
    assert_eq!(st.get_container_password(5001, 1), None);
    assert_eq!(st.get_container_password(5001, 2), None);

    st.set_container_password(5001, 1, "open_pw".into());
    st.set_container_password(5001, 2, "cfg_pw".into());
    assert_eq!(st.get_container_password(5001, 1), Some("open_pw"));
    assert_eq!(st.get_container_password(5001, 2), Some("cfg_pw"));

    st.set_container_password(5001, 1, String::new());
    assert_eq!(st.get_container_password(5001, 1), None);
    assert_eq!(st.get_container_password(5001, 2), Some("cfg_pw"));

    save_container_ship_state(&ctx, &st);
    let loaded = read_container_ship_state(&ctx);
    assert_eq!(loaded.get_container_password(5001, 2), Some("cfg_pw"));

    write_container_ship_state(&ctx, |mut_st| {
        mut_st.alsc_locked_items.insert(777);
    });
    assert!(read_container_ship_state(&ctx).alsc_locked_items.contains(&777));
}

#[test]
fn is_container_group_and_effective_item_unit_volume_across_hull_classes() {
    let (ctx, _) = make_ctx_and_session();
    assert!(is_container_group(0, 23)); // Jetcan type 23
    for grp in [12, 340, 448, 649, 1247, 1246, 4137] {
        assert!(is_container_group(grp, 100), "group {grp} must be container");
    }
    assert!(!is_container_group(25, 587));

    // Singleton vs packaged Rifter (587, group 25 frigate -> packaged 2500.0)
    let mut rifter = InventoryItemRecord {
        item_id: 1,
        type_id: 587,
        owner_id: 90_000_001,
        location_id: 60_003_760,
        flag: InventoryFlag::Hangar,
        quantity: 1,
        singleton: true,
        version: 1,
        ..Default::default()
    };
    let sde_vol = ctx.sde.types.get(&587).unwrap().volume;
    assert_eq!(effective_item_unit_volume(&ctx.sde, &rifter), sde_vol);
    rifter.singleton = false;
    assert_eq!(effective_item_unit_volume(&ctx.sde, &rifter), 2_500.0);

    // Capsule (670, group 29 -> packaged 500.0)
    let capsule = InventoryItemRecord {
        type_id: 670,
        singleton: false,
        ..rifter.clone()
    };
    assert_eq!(effective_item_unit_volume(&ctx.sde, &capsule), 500.0);

    // Unknown type returns 0.0
    let unknown = InventoryItemRecord {
        type_id: 999_999_999,
        singleton: true,
        ..rifter
    };
    assert_eq!(effective_item_unit_volume(&ctx.sde, &unknown), 0.0);
}

#[test]
fn ship_bay_access_and_alsc_audit_logging() {
    let (ctx, _) = make_ctx_and_session();
    let owner_id = 90_000_001;
    let other_id = 90_000_002;
    let host_ship_id = 100_000_001;

    ctx.db.with_snapshot(|snap| {
        assert!(can_character_access_ship_bay_snap(
            snap, owner_id, host_ship_id, owner_id, true
        ));
    });

    // Disallow corp & fleet on ship config -> denied for non-owner
    write_container_ship_state(&ctx, |st| {
        st.ship_configs.insert(
            host_ship_id,
            ShipAccessConfig {
                allow_fleet_smb: false,
                allow_corp_smb: false,
                allow_fleet_hangar: false,
                allow_corp_hangar: false,
                password: Some("baypw".into()),
            },
        );
    });
    ctx.db.with_snapshot(|snap| {
        assert!(!can_character_access_ship_bay_with_password_snap(
            snap,
            other_id,
            host_ship_id,
            owner_id,
            true,
            Some("baypw")
        ));
    });

    // Seed an ALSC container (type 17366 = Station Warehouse Container / Secure Container in group 340 or 448)
    let alsc_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 340)
        .map(|t| t.type_id)
        .unwrap_or(17366);
    let cont_id = 880_001;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            cont_id,
            InventoryItemRecord {
                item_id: cont_id,
                type_id: alsc_tid,
                owner_id: 98_000_001,
                location_id: 60_003_760,
                flag: InventoryFlag::CorpSAG1,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    write_container_ship_state(&ctx, |st| {
        st.container_configs.insert(
            cont_id,
            ContainerConfig {
                lock_by_default: true,
                password_per_action: false,
                config_bitmask: 3,
            },
        );
    });

    record_container_add_log(&ctx, owner_id, 880_002, 34, 100, cont_id);
    record_container_take_log(&ctx, owner_id, 880_002, 34, 50, cont_id);
    record_container_rename_log(&ctx, owner_id, cont_id, alsc_tid);

    let now_secs = ctx.now_unix_secs();
    ctx.db.with_snapshot(|snap| {
        assert!(container_has_active_audit_log_snap(snap, cont_id, now_secs));
        let st = read_container_ship_state_snap(snap);
        assert!(st.alsc_locked_items.contains(&880_002));
        assert!(st.alsc_logs.get(&cont_id).unwrap().len() >= 3);
    });
}

#[tokio::test]
async fn container_and_bay_move_validation_and_rpc_handlers() {
    let (ctx, mut sess) = make_ctx_and_session();
    let kw = HashMap::new();

    // 1. Self-move rejected
    let self_err = validate_container_and_bay_move(&ctx, 90_000_001, 100, 100, 4, &kw, None)
        .expect_err("self move must fail");
    assert_eq!(self_err.message, "CannotStoreShipInItself");

    // 2. ALSC locked item rejected
    write_container_ship_state(&ctx, |st| {
        st.alsc_locked_items.insert(880_010);
    });
    let lock_err = validate_container_and_bay_move(&ctx, 90_000_001, 880_010, 60_003_760, 4, &kw, None)
        .expect_err("locked item move must fail");
    assert_eq!(lock_err.code, "ALSCItemLocked");

    // 3. Seed packaged cargo container (group 12 or 340) and assemble via AssembleCargoContainer
    let cont_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 340)
        .map(|t| t.type_id)
        .unwrap_or(17366);
    let cont_id = 880_100;
    let tritanium_id = 880_101;
    let wrap_id = 880_102;
    let wrapped_child_id = 880_103;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            cont_id,
            InventoryItemRecord {
                item_id: cont_id,
                type_id: cont_tid,
                owner_id: 90_000_001,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 2,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            tritanium_id,
            InventoryItemRecord {
                item_id: tritanium_id,
                type_id: 34,
                owner_id: 90_000_001,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 500,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            wrap_id,
            InventoryItemRecord {
                item_id: wrap_id,
                type_id: 3468,
                owner_id: 90_000_001,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            wrapped_child_id,
            InventoryItemRecord {
                item_id: wrapped_child_id,
                type_id: 34,
                owner_id: 90_000_001,
                location_id: wrap_id,
                flag: InventoryFlag::Locked,
                quantity: 25,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    // Opening packaged container fails
    let open_pkg = handle_containers_ships_and_fitting(
        &ctx,
        &mut sess,
        None,
        "invbroker",
        "ValidateItemCanBeOpened",
        &[PyRep::Integer(cont_id)],
        &kw,
    )
    .await
    .unwrap();
    assert_eq!(open_pkg.unwrap_err().code, "CantOpenPackagedItem");

    // AssembleCargoContainer splits stack of 2 and returns new singleton container
    let asm_rep = handle_containers_ships_and_fitting(
        &ctx,
        &mut sess,
        None,
        "invbroker",
        "AssembleCargoContainer",
        &[PyRep::Integer(cont_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("AssembleCargoContainer");
    let new_cid = asm_rep.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
    assert_ne!(new_cid, cont_id);

    // ValidateItemCanBeOpened and ValidateItemListCanBeOpened succeed on assembled container
    let open_ok = handle_containers_ships_and_fitting(
        &ctx,
        &mut sess,
        None,
        "invbroker",
        "ValidateItemListCanBeOpened",
        &[PyRep::List(vec![PyRep::Integer(new_cid)])],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(open_ok, PyRep::Bool(true));

    // Move tritanium into assembled container and test ALSCLockItems / ALSCUnlockItems / ALSCLogGet / ALSCConfigGet
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&tritanium_id) {
            it.location_id = new_cid;
        }
    });
    let cfg_get = handle_containers_ships_and_fitting(
        &ctx,
        &mut sess,
        None,
        "invbroker",
        "ALSCConfigGet",
        &[PyRep::Integer(new_cid)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(cfg_get.dict_get("containerID"), Some(&PyRep::Integer(new_cid)));

    let lock_res = handle_containers_ships_and_fitting(
        &ctx,
        &mut sess,
        None,
        "invbroker",
        "ALSCLockItems",
        &[
            PyRep::Integer(new_cid),
            PyRep::List(vec![PyRep::Integer(tritanium_id)]),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(lock_res.dict_get("locked"), Some(&PyRep::Bool(true)));

    // itemLocking.GetLockedItemLocations and GetItemsByLocation
    let locked_locs = handle_containers_ships_and_fitting(
        &ctx,
        &mut sess,
        None,
        "itemLocking",
        "GetLockedItemLocations",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    if let PyRep::List(locs) = locked_locs {
        assert!(locs.contains(&PyRep::Integer(new_cid)));
    } else {
        panic!("expected list");
    }

    let locked_by_loc = handle_containers_ships_and_fitting(
        &ctx,
        &mut sess,
        None,
        "itemLocking",
        "GetItemsByLocation",
        &[PyRep::Integer(new_cid)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(locked_by_loc, PyRep::Dict(_)));

    let unlock_res = handle_containers_ships_and_fitting(
        &ctx,
        &mut sess,
        None,
        "invbroker",
        "ALSCUnlockItems",
        &[
            PyRep::Integer(new_cid),
            PyRep::List(vec![PyRep::Integer(tritanium_id)]),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(unlock_res.dict_get("locked"), Some(&PyRep::Bool(false)));

    let log_res = handle_containers_ships_and_fitting(
        &ctx,
        &mut sess,
        None,
        "invbroker",
        "ALSCLogGet",
        &[PyRep::Integer(new_cid)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(log_res.dict_get("logCount").and_then(|v| v.as_i64()).unwrap_or(0) >= 2);
}

#[tokio::test]
async fn invbroker_misc_and_encoded_items_decode_item() {
    let (ctx, mut sess) = make_ctx_and_session();
    let kw = HashMap::new();
    let bound = BoundObjectEntry {
        bound_id: 1,
        session_id: sess.session_id,
        user_id: sess.user_id.unwrap_or(1),
        character_id: sess.character_id.unwrap_or(140_000_001) as i64,
        service: "invbroker".into(),
        object_id: 100_000_001,
        oid_str: "N=1:1".into(),
        ref_id: 1,
    };
    let char_id = sess.character_id.unwrap_or(90_000_001);
    let wrap_loc = sess.station_id.unwrap_or(60_003_760);
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(
            880_102,
            InventoryItemRecord {
                item_id: 880_102,
                type_id: 3468,
                owner_id: char_id,
                location_id: wrap_loc,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    for (svc, method, args) in [
        ("invbroker", "GetDamageForCrystals", vec![]),
        ("invbroker", "GetAllItems", vec![PyRep::String("rifter".into())]),
        ("invbroker", "IsMyItemAtDockableLocation", vec![PyRep::Integer(100_000_001)]),
        ("invbroker", "BreakPlasticWrap", vec![PyRep::Integer(880_102)]),
        ("invbroker", "ClearCharges", vec![]),
        ("invbroker", "DestroyFitting", vec![PyRep::Integer(999_999)]),
        ("invbroker", "FitFitting", vec![PyRep::Integer(100_000_001)]),
        ("invbroker", "GetAssetWorth", vec![]),
        ("invbroker", "GetPriceForItems", vec![PyRep::List(vec![PyRep::Integer(100_000_001)])]),
        ("invbroker", "HasExistingPasswordSet", vec![PyRep::Integer(880_100), PyRep::Integer(1)]),
        ("invbroker", "ImportExportWithPlanet", vec![]),
        ("invbroker", "ListIncludingContainers", vec![PyRep::Integer(60_003_760)]),
        ("invbroker", "ListStationItems", vec![PyRep::Integer(60_003_760)]),
        ("invbroker", "ListStations", vec![]),
        ("invbroker", "Reprocess", vec![]),
        ("invbroker", "RetrievePassword", vec![]),
        ("encodedItems", "DecodeItem", vec![PyRep::String("100000001:587".into())]),
    ] {
        let res = handle_containers_ships_and_fitting(
            &ctx,
            &mut sess,
            Some(&bound),
            svc,
            method,
            &args,
            &kw,
        )
        .await
        .expect("handler matched");
        assert!(res.is_ok(), "{svc}.{method} failed: {:?}", res);
    }
}
