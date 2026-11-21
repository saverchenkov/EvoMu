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
    ctx.db.with_mut_snapshot(|snap| {
        assert_eq!(container_password_snap(snap, 5001, 1), None);
        assert_eq!(container_password_snap(snap, 5001, 2), None);

        set_container_password_snap(snap, 5001, 1, "open_pw", 0).unwrap();
        set_container_password_snap(snap, 5001, 2, "cfg_pw", 0).unwrap();
        assert_eq!(container_password_snap(snap, 5001, 1).as_deref(), Some("open_pw"));
        assert_eq!(container_password_snap(snap, 5001, 2).as_deref(), Some("cfg_pw"));

        set_container_password_snap(snap, 5001, 1, "", 0).unwrap();
        assert_eq!(container_password_snap(snap, 5001, 1), None);
        assert_eq!(container_password_snap(snap, 5001, 2).as_deref(), Some("cfg_pw"));
    });
    ctx.db.with_snapshot(|snap| {
        assert_eq!(container_password_snap(snap, 5001, 2).as_deref(), Some("cfg_pw"));
    });

    ctx.db.with_mut_snapshot(|snap| {
        set_alsc_item_locked_snap(snap, 777, true, 0).unwrap();
    });
    assert!(ctx.db.with_snapshot(|snap| snap.is_item_in_alsc_lock(777)));
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
    ctx.db.with_mut_snapshot(|snap| {
        set_ship_access_config_snap(
            snap,
            host_ship_id,
            &ShipAccessConfig {
                allow_fleet_smb: false,
                allow_corp_smb: false,
                allow_fleet_hangar: false,
                allow_corp_hangar: false,
                password: Some("baypw".into()),
            },
            0,
        )
        .unwrap();
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
    ctx.db.with_mut_snapshot(|snap| {
        set_container_config_snap(
            snap,
            cont_id,
            &ContainerConfig {
                lock_by_default: true,
                password_per_action: false,
                config_bitmask: 3,
            },
            0,
        )
        .unwrap();
    });

    record_container_add_log(&ctx, owner_id, 880_002, 34, 100, cont_id);
    record_container_take_log(&ctx, owner_id, 880_002, 34, 50, cont_id);
    record_container_rename_log(&ctx, owner_id, cont_id, alsc_tid);

    let now_secs = ctx.now_unix_secs();
    ctx.db.with_snapshot(|snap| {
        assert!(container_has_active_audit_log_snap(snap, cont_id, now_secs));
        assert!(snap.is_item_in_alsc_lock(880_002));
        assert!(container_alsc_logs_snap(snap, cont_id).len() >= 3);
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
    ctx.db.with_mut_snapshot(|snap| {
        set_alsc_item_locked_snap(snap, 880_010, true, 0).unwrap();
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
    let PyRep::List(entries) = log_res else {
        panic!("expected List from ALSCLogGet");
    };
    assert!(entries.len() >= 2);
    assert!(entries[0].dict_get("actionID").and_then(PyRep::as_i64).is_some());
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
    let secure_cont_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 340)
        .expect("secure container type")
        .type_id;
    let planet_id = 40_000_006i64;
    let poco_id = 880_200i64;
    let spaceport_pin_id = 880_201i64;
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
        s.items.insert(
            880_103,
            InventoryItemRecord {
                item_id: 880_103,
                type_id: secure_cont_tid,
                owner_id: char_id,
                location_id: wrap_loc,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        s.planetary_colonies.insert(
            (char_id, planet_id),
            eve_db::PlanetaryColonyRecord {
                planet_id,
                owner_id: char_id,
                solar_system_id: 30_000_142,
                command_center_level: 1,
                pins_count: 1,
                extracted_commodities: HashMap::new(),
                last_extracted_unix: 1_775_400_000,
            },
        );
        let net = crate::services::industry_pi::PiColonyNetworkState {
            pins: vec![crate::services::industry_pi::PiPinState {
                pin_id: spaceport_pin_id,
                type_id: 2256,
                latitude: 0.1,
                longitude: 0.1,
                contents: HashMap::from([(2268u32, 10)]),
                ..Default::default()
            }],
            cpu_max: 5_000.0,
            pg_max: 9_000.0,
            ..Default::default()
        };
        crate::services::industry_pi::save_pi_network(s, char_id, planet_id, &net);
        s.structures.insert(
            poco_id,
            eve_db::UpwellStructureRecord {
                structure_id: poco_id,
                type_id: 2233,
                owner_corp_id: sess.corporation_id.unwrap_or(1_000_009),
                solar_system_id: 30_000_142,
                name: format!("Orbital:{planet_id}"),
                state: "ShieldVulnerable".into(),
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
        ("invbroker", "HasExistingPasswordSet", vec![PyRep::Integer(880_103), PyRep::Integer(1)]),
        (
            "invbroker",
            "ImportExportWithPlanet",
            vec![
                PyRep::Integer(poco_id),
                PyRep::Integer(spaceport_pin_id),
                PyRep::Dict(Vec::new()),
                PyRep::Dict(vec![(PyRep::Integer(2268), PyRep::Integer(2))]),
            ],
        ),
        ("invbroker", "ListIncludingContainers", vec![PyRep::Integer(60_003_760)]),
        ("invbroker", "ListStationItems", vec![PyRep::Integer(60_003_760)]),
        ("invbroker", "ListStations", vec![]),
        ("invbroker", "Reprocess", vec![]),
        ("invbroker", "RetrievePassword", vec![PyRep::Integer(880_103), PyRep::Integer(1)]),
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

#[tokio::test]
async fn invbroker_empty_station_hangar_no_fallback_and_fit_reprocess_real() {
    let (ctx, mut sess) = make_ctx_and_session();
    let kw = HashMap::new();
    let char_id = sess.character_id.unwrap();
    let _ = ctx.db.acquire_online_lock(1, sess.session_id, Some(char_id), true, ctx.now_unix());
    let ship_id = sess.ship_id.unwrap();
    let station_id = sess.station_id.unwrap();
    let empty_station_id = 60_003_761_i64;

    // #537: Querying an empty station must return 0 rows, not fallback to universe items
    for method in ["ListStationItems", "ListIncludingContainers"] {
        let rep = handle_containers_ships_and_fitting(
            &ctx,
            &mut sess,
            None,
            "invbroker",
            method,
            &[PyRep::Integer(empty_station_id)],
            &kw,
        )
        .await
        .expect("handled")
        .expect("empty station list ok");
        let rows = match rep {
            PyRep::ObjectEx { ref list, .. } => list.len(),
            _ => panic!("expected CRowset ObjectEx, got {rep:?}"),
        };
        assert_eq!(rows, 0, "{method} leaked universe items into empty station");
    }

    // #582: invbroker.FitFitting fits real items, emits OnItemChange, and only reports truly missing items
    let mut lo_mods = ctx
        .sde
        .types
        .values()
        .filter(|t| {
            t.published
                && t.category_id == 7
                && t.effect_ids.contains(&11)
                && !t.default_attributes.contains_key(&182)
                && !t.default_attributes.contains_key(&1544)
                && !t.default_attributes.contains_key(&1548)
                && !t.default_attributes.contains_key(&2431)
                && t.volume <= 10.0
        })
        .map(|t| t.type_id)
        .collect::<Vec<_>>();
    lo_mods.sort_unstable();
    let hi_mod_tid = lo_mods[0];
    let missing_mod_tid = lo_mods[1];
    let mod_item_id = 880_901_i64;
    let ore_item_id = 880_902_i64;
    let mod_item_id_2 = 880_903_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.location_id = station_id;
        }
        snap.items.insert(
            mod_item_id,
            InventoryItemRecord {
                item_id: mod_item_id,
                type_id: hi_mod_tid,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            mod_item_id_2,
            InventoryItemRecord {
                item_id: mod_item_id_2,
                type_id: hi_mod_tid,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            ore_item_id,
            InventoryItemRecord {
                item_id: ore_item_id,
                type_id: 1230, // Veldspar
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1_000,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let _ = ctx.drain_notifications_for_char(char_id);
    let fit_partial = handle_containers_ships_and_fitting(
        &ctx,
        &mut sess,
        None,
        "invbroker",
        "FitFitting",
        &[
            PyRep::Integer(ship_id),
            PyRep::List(vec![
                PyRep::Tuple(vec![PyRep::Integer(hi_mod_tid as i64), PyRep::Integer(1)]),
                PyRep::Tuple(vec![PyRep::Integer(missing_mod_tid as i64), PyRep::Integer(1)]),
            ]),
        ],
        &kw,
    )
    .await
    .expect("handled")
    .expect("FitFitting ok");
    assert_eq!(
        fit_partial,
        PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(missing_mod_tid as i64),
            PyRep::Integer(1),
        ])]),
        "FitFitting must return only genuinely missing items"
    );
    let notifs_after = ctx.drain_notifications_for_char(char_id);
    assert!(
        notifs_after
            .iter()
            .any(|n| n.event == "OnItemChange"),
        "FitFitting must emit OnItemChange when items move onto ship"
    );
    ctx.db.with_snapshot(|snap| {
        let on_ship_count = [mod_item_id, mod_item_id_2]
            .into_iter()
            .filter(|id| snap.items[id].location_id == ship_id)
            .count();
        assert_eq!(on_ship_count, 1, "one module must have moved onto the ship");
    });

    let fit_full = handle_containers_ships_and_fitting(
        &ctx,
        &mut sess,
        None,
        "invbroker",
        "FitFitting",
        &[
            PyRep::Integer(ship_id),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(hi_mod_tid as i64),
                PyRep::Integer(1),
            ])]),
        ],
        &kw,
    )
    .await
    .expect("handled")
    .expect("full FitFitting ok");
    assert_eq!(fit_full, PyRep::List(Vec::new()));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&mod_item_id].location_id, ship_id);
        assert_eq!(snap.items[&mod_item_id_2].location_id, ship_id);
    });

    // #582: invbroker.Reprocess delegates to real reprocessing instead of no-op
    let reproc = handle_containers_ships_and_fitting(
        &ctx,
        &mut sess,
        None,
        "invbroker",
        "Reprocess",
        &[PyRep::List(vec![PyRep::Integer(ore_item_id)])],
        &kw,
    )
    .await
    .expect("handled")
    .expect("invbroker.Reprocess real ok");
    assert_ne!(reproc, PyRep::List(Vec::new()));
    ctx.db.with_snapshot(|snap| {
        assert!(
            !snap.items.contains_key(&ore_item_id),
            "Veldspar stack should be consumed by invbroker.Reprocess"
        );
        let tritanium_qty: i64 = snap
            .items
            .values()
            .filter(|it| it.owner_id == char_id && it.location_id == station_id && it.type_id == 34)
            .map(|it| i64::from(it.quantity))
            .sum();
        assert!(tritanium_qty > 0, "Tritanium minerals must be yielded in hangar");
    });
}

#[tokio::test]
async fn alsc_config_log_and_password_check_reject_outsiders_non_containers_and_unprivileged_corp_members() {
    let (ctx, mut owner_sess) = make_ctx_and_session();
    let owner_id = owner_sess.character_id.unwrap();
    let corp_id = 98_000_001_i64;
    let roleless_corp_mate_id = 90_000_011_i64;
    let outsider_id = 90_000_002_i64;
    let alsc_type_id = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 448)
        .map(|t| t.type_id)
        .expect("SDE must contain group 448 ALSC");
    let freight_type_id = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 649)
        .map(|t| t.type_id)
        .expect("SDE must contain group 649 Freight Container");
    let sentry_type_id = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 449)
        .map(|t| t.type_id)
        .expect("SDE must contain group 449 Mobile Hybrid Sentry");
    let personal_alsc = 990_501_i64;
    let corp_alsc = 990_502_i64;
    let trit_id = 990_503_i64;
    let freight_cont = 990_504_i64;
    let sentry_id = 990_505_i64;
    let station_id = 60_003_760_i64;

    ctx.db.with_mut_snapshot(|snap| {
        for (iid, tid, oid) in [
            (personal_alsc, alsc_type_id, owner_id),
            (corp_alsc, alsc_type_id, corp_id),
            (trit_id, 34, owner_id),
            (freight_cont, freight_type_id, owner_id),
            (sentry_id, sentry_type_id, owner_id),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: oid,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: iid != trit_id,
                    ..Default::default()
                },
            );
        }
        if let Some(ch) = snap.characters.get_mut(&roleless_corp_mate_id) {
            ch.corporation_id = corp_id;
        }
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.member_roles.insert(roleless_corp_mate_id, 0);
        }
    });

    let mut roleless_sess = owner_sess.clone();
    roleless_sess.character_id = Some(roleless_corp_mate_id);
    roleless_sess.corporation_id = Some(corp_id);
    roleless_sess.corp_role = 0;

    let mut outsider_sess = owner_sess.clone();
    outsider_sess.character_id = Some(outsider_id);
    outsider_sess.corporation_id = Some(98_000_099);
    outsider_sess.corp_role = 0;

    let mut director_sess = owner_sess.clone();
    director_sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    let kw = HashMap::new();
    let alsc_denied = "ALSCAccessDenied";

    // Non-container Tritanium must reject even for owner across all three endpoints.
    for method in ["ALSCConfigGet", "ALSCLogGet", "HasExistingPasswordSet"] {
        let err = handle_containers_ships_and_fitting(
            &ctx,
            &mut owner_sess,
            None,
            "invbroker",
            method,
            &[PyRep::Integer(trit_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(err.code, alsc_denied, "{method} must reject non-container");

        let err_out = handle_containers_ships_and_fitting(
            &ctx,
            &mut outsider_sess,
            None,
            "invbroker",
            method,
            &[PyRep::Integer(personal_alsc)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(err_out.code, alsc_denied, "{method} must reject outsider");

        let err_corp = handle_containers_ships_and_fitting(
            &ctx,
            &mut roleless_sess,
            None,
            "invbroker",
            method,
            &[PyRep::Integer(corp_alsc)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(err_corp.code, alsc_denied, "{method} must reject roleless corp member");
    }

    // Owner on personal ALSC & Director on corp ALSC succeed.
    assert!(handle_containers_ships_and_fitting(
        &ctx,
        &mut owner_sess,
        None,
        "invbroker",
        "ALSCConfigGet",
        &[PyRep::Integer(personal_alsc)],
        &kw,
    )
    .await
    .unwrap()
    .is_ok());
    assert!(handle_containers_ships_and_fitting(
        &ctx,
        &mut director_sess,
        None,
        "invbroker",
        "HasExistingPasswordSet",
        &[PyRep::Integer(corp_alsc), PyRep::Integer(1)],
        &kw,
    )
    .await
    .unwrap()
    .is_ok());

    // Group 649 (Freight Container) is accepted and group 449 (Mobile Hybrid Sentry) is rejected.
    for method in ["ALSCConfigGet", "ALSCLogGet", "HasExistingPasswordSet"] {
        assert!(
            handle_containers_ships_and_fitting(
                &ctx,
                &mut owner_sess,
                None,
                "invbroker",
                method,
                &[PyRep::Integer(freight_cont)],
                &kw,
            )
            .await
            .unwrap()
            .is_ok(),
            "{method} must accept group 649 freight container"
        );
        let sentry_err = handle_containers_ships_and_fitting(
            &ctx,
            &mut owner_sess,
            None,
            "invbroker",
            method,
            &[PyRep::Integer(sentry_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(sentry_err.code, alsc_denied, "{method} must reject group 449 sentry");
    }
}

fn extract_bound_id_from_rep(rep: &PyRep) -> u64 {
    let target = match rep {
        PyRep::Tuple(items) if !items.is_empty() => &items[0],
        other => other,
    };
    if let Some(id) = target.as_i64().and_then(|v| u64::try_from(v).ok()) {
        return id;
    }
    let PyRep::SubStruct(sub) = target else {
        panic!("expected SubStruct or integer bound id, got {rep:?}");
    };
    let PyRep::SubStream(inner) = sub.as_ref() else {
        panic!("expected SubStream inside SubStruct, got {sub:?}");
    };
    let PyRep::Tuple(parts) = inner.as_ref() else {
        panic!("expected Tuple inside SubStream, got {inner:?}");
    };
    let oid = match parts.first() {
        Some(PyRep::Bytes(bytes)) => std::str::from_utf8(bytes).expect("utf-8 oid"),
        Some(PyRep::String(s)) => s.as_str(),
        other => panic!("expected oid Bytes/String in bound SubStream, got {other:?}"),
    };
    oid.rsplit(':')
        .next()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or_else(|| panic!("invalid oid string {oid:?}"))
}

#[tokio::test]
async fn has_existing_password_set_bound_moniker_and_slot_validation() {
    let (ctx, mut owner_sess) = make_ctx_and_session();
    let owner_id = owner_sess.character_id.unwrap();
    let station_id = owner_sess.station_id.unwrap();
    let alsc_id = 880_210_i64;
    let outsider_id = 90_000_211_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            alsc_id,
            InventoryItemRecord {
                item_id: alsc_id,
                type_id: 17366,
                owner_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        let mut outsider = snap.characters[&owner_id].clone();
        outsider.character_id = outsider_id;
        outsider.user_id = outsider_id as u64;
        outsider.corporation_id = 98_000_211;
        outsider.name = "Outsider Pilot".into();
        snap.characters.insert(outsider_id, outsider);
    });

    let kw = std::collections::HashMap::new();
    let trace = Some("machoNet:26000");

    // V24 client flow: MachoBindObject(station_id) -> GetInventoryFromId(alsc_id, 0) -> HasExistingPasswordSet(which)
    let station_bind_rep = ctx
        .dispatch_rpc(
            &mut owner_sess,
            "invbroker",
            None,
            "MachoBindObject",
            &[PyRep::Integer(station_id), PyRep::None],
            &kw,
            trace,
        )
        .await
        .expect("MachoBindObject(station_id) must succeed");
    let station_bid = extract_bound_id_from_rep(&station_bind_rep);

    let cont_bind_rep = ctx
        .dispatch_rpc(
            &mut owner_sess,
            "invbroker",
            Some(station_bid),
            "GetInventoryFromId",
            &[PyRep::Integer(alsc_id), PyRep::Integer(0)],
            &kw,
            trace,
        )
        .await
        .expect("GetInventoryFromId(alsc_id, 0) must succeed");
    let cont_bid = extract_bound_id_from_rep(&cont_bind_rep);

    // 1. Neither password slot set -> both 1 (General) and 2 (Config) return false.
    for slot in [1_i64, 2_i64] {
        let rep = ctx
            .dispatch_rpc(
                &mut owner_sess,
                "invbroker",
                Some(cont_bid),
                "HasExistingPasswordSet",
                &[PyRep::Integer(slot)],
                &kw,
                trace,
            )
            .await
            .expect("bound HasExistingPasswordSet must succeed");
        assert_eq!(rep, PyRep::Bool(false), "slot {slot} must be false before SetPassword");
    }

    // 2. Only config password (which=2) set: slot 1 stays false, slot 2 returns true across bound/unbound/kwargs.
    ctx.db.with_mut_snapshot(|snap| {
        set_container_password_snap(snap, alsc_id, 2, "cfg-secret", 0).unwrap();
    });
    assert_eq!(
        ctx.dispatch_rpc(
            &mut owner_sess,
            "invbroker",
            Some(cont_bid),
            "HasExistingPasswordSet",
            &[PyRep::Integer(1)],
            &kw,
            trace,
        )
        .await
        .unwrap(),
        PyRep::Bool(false)
    );
    assert_eq!(
        ctx.dispatch_rpc(
            &mut owner_sess,
            "invbroker",
            Some(cont_bid),
            "HasExistingPasswordSet",
            &[PyRep::Integer(2)],
            &kw,
            trace,
        )
        .await
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        ctx.dispatch_rpc(
            &mut owner_sess,
            "invbroker",
            None,
            "HasExistingPasswordSet",
            &[PyRep::Integer(alsc_id), PyRep::Integer(2)],
            &kw,
            trace,
        )
        .await
        .unwrap(),
        PyRep::Bool(true)
    );
    let mut kw_id = std::collections::HashMap::new();
    kw_id.insert("containerID".to_string(), PyRep::Integer(alsc_id));
    kw_id.insert("which".to_string(), PyRep::Integer(2));
    assert_eq!(
        ctx.dispatch_rpc(
            &mut owner_sess,
            "invbroker",
            None,
            "HasExistingPasswordSet",
            &[],
            &kw_id,
            trace,
        )
        .await
        .unwrap(),
        PyRep::Bool(true)
    );

    // 3. Both slots set then slot 1 cleared: slot 1 returns false again while slot 2 remains true.
    ctx.db.with_mut_snapshot(|snap| {
        set_container_password_snap(snap, alsc_id, 1, "gen-secret", 0).unwrap();
    });
    assert_eq!(
        ctx.dispatch_rpc(
            &mut owner_sess,
            "invbroker",
            Some(cont_bid),
            "HasExistingPasswordSet",
            &[PyRep::Integer(1)],
            &kw,
            trace,
        )
        .await
        .unwrap(),
        PyRep::Bool(true)
    );
    ctx.db.with_mut_snapshot(|snap| {
        set_container_password_snap(snap, alsc_id, 1, "", 0).unwrap();
    });
    assert_eq!(
        ctx.dispatch_rpc(
            &mut owner_sess,
            "invbroker",
            Some(cont_bid),
            "HasExistingPasswordSet",
            &[PyRep::Integer(1)],
            &kw,
            trace,
        )
        .await
        .unwrap(),
        PyRep::Bool(false)
    );
    assert_eq!(
        ctx.dispatch_rpc(
            &mut owner_sess,
            "invbroker",
            Some(cont_bid),
            "HasExistingPasswordSet",
            &[PyRep::Integer(2)],
            &kw,
            trace,
        )
        .await
        .unwrap(),
        PyRep::Bool(true)
    );

    // 4. Invalid `which` values (including small integers 3/99 and strings) must fail with InvalidPasswordType.
    for bad_which in [
        PyRep::Integer(0),
        PyRep::Integer(3),
        PyRep::Integer(99),
        PyRep::Integer(-1),
        PyRep::String("general".into()),
    ] {
        let err = ctx
            .dispatch_rpc(
                &mut owner_sess,
                "invbroker",
                Some(cont_bid),
                "HasExistingPasswordSet",
                &[bad_which.clone()],
                &kw,
                trace,
            )
            .await
            .expect_err("invalid which must be rejected");
        assert_eq!(err.code, "InvalidPasswordType", "bad which {bad_which:?}");
    }

    // 5. Station-bound HasExistingPasswordSet(1) resolves container_id=station_id and fails ContainerNotFound.
    let station_err = ctx
        .dispatch_rpc(
            &mut owner_sess,
            "invbroker",
            Some(station_bid),
            "HasExistingPasswordSet",
            &[PyRep::Integer(1)],
            &kw,
            trace,
        )
        .await
        .expect_err("station-bound HasExistingPasswordSet(1) must reject");
    assert_eq!(station_err.code, "ContainerNotFound");

    // 6. Outsider cannot probe HasExistingPasswordSet on another pilot's container.
    let mut outsider_sess = SessionState {
        session_id: 211,
        user_id: Some(outsider_id as u64),
        character_id: Some(outsider_id),
        corporation_id: Some(98_000_211),
        station_id: Some(station_id),
        ..owner_sess.clone()
    };
    let denied = ctx
        .dispatch_rpc(
            &mut outsider_sess,
            "invbroker",
            None,
            "HasExistingPasswordSet",
            &[PyRep::Integer(alsc_id), PyRep::Integer(2)],
            &kw,
            trace,
        )
        .await
        .expect_err("outsider HasExistingPasswordSet must be denied");
    assert_eq!(denied.code, "ALSCAccessDenied");
}

#[tokio::test]
async fn alsc_log_get_rejects_same_corp_and_npc_corp_non_owner_without_mutating_logs() {
    let (ctx, mut owner_sess) = make_ctx_and_session();
    let owner_id = owner_sess.character_id.unwrap();
    let player_corp = owner_sess.corporation_id.unwrap();
    let station_id = owner_sess.station_id.unwrap();
    let npc_corp = 1_000_009_i64;

    let personal_alsc = 880_220_i64;
    let corp_alsc = 880_221_i64;
    let npc_owner_id = 90_000_222_i64;
    let npc_peer_id = 90_000_223_i64;
    let npc_personal_alsc = 880_224_i64;
    let mate_dir_id = 90_000_225_i64;
    let mate_plain_id = 90_000_226_i64;
    let gm_id = 90_000_227_i64;

    ctx.db.with_mut_snapshot(|snap| {
        for (cid, item_owner) in [
            (personal_alsc, owner_id),
            (corp_alsc, player_corp),
            (npc_personal_alsc, npc_owner_id),
        ] {
            snap.items.insert(
                cid,
                InventoryItemRecord {
                    item_id: cid,
                    type_id: 17366,
                    owner_id: item_owner,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
        let template = snap.characters[&owner_id].clone();
        for (cid, corp) in [
            (npc_owner_id, npc_corp),
            (npc_peer_id, npc_corp),
            (mate_dir_id, player_corp),
            (mate_plain_id, player_corp),
            (gm_id, 98_000_299),
        ] {
            let mut rec = template.clone();
            rec.character_id = cid;
            rec.user_id = cid as u64;
            rec.corporation_id = corp;
            rec.name = format!("Pilot {cid}");
            snap.characters.insert(cid, rec);
        }
        if let Some(corp) = snap.corporations.get_mut(&player_corp) {
            corp.members.insert(mate_dir_id);
            corp.members.insert(mate_plain_id);
            corp.member_roles.insert(
                mate_dir_id,
                eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_EQUIPMENT_CONFIG,
            );
        }
    });

    ctx.db.with_mut_snapshot(|snap| {
        set_container_password_snap(snap, personal_alsc, 1, "p-pw", 0).unwrap();
        set_container_password_snap(snap, npc_personal_alsc, 1, "npc-pw", 0).unwrap();
        set_container_password_snap(snap, corp_alsc, 1, "c-pw", 0).unwrap();
    });

    let kw = std::collections::HashMap::new();
    let mut mate_dir_sess = SessionState {
        session_id: 225,
        user_id: Some(mate_dir_id as u64),
        character_id: Some(mate_dir_id),
        corporation_id: Some(player_corp),
        corp_role: eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_EQUIPMENT_CONFIG,
        ..owner_sess.clone()
    };
    let mut mate_plain_sess = SessionState {
        session_id: 226,
        user_id: Some(mate_plain_id as u64),
        character_id: Some(mate_plain_id),
        corporation_id: Some(player_corp),
        corp_role: 0,
        ..owner_sess.clone()
    };
    let mut npc_peer_sess = SessionState {
        session_id: 223,
        user_id: Some(npc_peer_id as u64),
        character_id: Some(npc_peer_id),
        corporation_id: Some(npc_corp),
        corp_role: 0,
        ..owner_sess.clone()
    };
    let mut gm_sess = SessionState {
        session_id: 227,
        user_id: Some(gm_id as u64),
        character_id: Some(gm_id),
        corporation_id: Some(98_000_299),
        corp_role: 0,
        role: eve_machonet::service_roles::ROLE_GM_DEFAULT,
        ..owner_sess.clone()
    };

    // 1. Same-player-corp colleague (even Director + EquipmentConfig) CANNOT read personal ALSC audit logs
    //    and CANNOT inject ALSCActionPasswordFailure entries.
    for args in [
        vec![PyRep::Integer(personal_alsc)],
        vec![PyRep::Integer(personal_alsc), PyRep::String("wrong".into())],
        vec![PyRep::Integer(personal_alsc), PyRep::String("p-pw".into())],
    ] {
        let err = handle_containers_ships_and_fitting(
            &ctx,
            &mut mate_dir_sess,
            None,
            "invbroker",
            "ALSCLogGet",
            &args,
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(err.code, "ALSCAccessDenied");
    }

    // 2. Same-NPC-corp non-owner CANNOT read or pollute another pilot's personal ALSC audit logs.
    let npc_unbound_err = handle_containers_ships_and_fitting(
        &ctx,
        &mut npc_peer_sess,
        None,
        "invbroker",
        "ALSCLogGet",
        &[PyRep::Integer(npc_personal_alsc)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(npc_unbound_err.code, "ALSCAccessDenied");

    let npc_bound_id = ctx
        .bind_verified_sub_object(&npc_peer_sess, "invbroker", npc_personal_alsc)
        .expect("bind verified sub object");
    let npc_bound_entry = ctx.verify_bound_object(&npc_peer_sess, npc_bound_id).unwrap();
    let npc_bound_err = handle_containers_ships_and_fitting(
        &ctx,
        &mut npc_peer_sess,
        Some(&npc_bound_entry),
        "invbroker",
        "ALSCLogGet",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(npc_bound_err.code, "ALSCAccessDenied");

    // Verify neither personal ALSC accumulated any ALSCActionPasswordFailure entry.
    let now = ctx.now_unix();
    ctx.db.with_snapshot(|snap| {
        assert!(
            container_alsc_logs_snap(snap, personal_alsc).is_empty(),
            "denied same-corp ALSCLogGet must not mutate personal ALSC audit log"
        );
        assert!(
            container_alsc_logs_snap(snap, npc_personal_alsc).is_empty(),
            "denied NPC-corp ALSCLogGet must not mutate personal ALSC audit log"
        );
        assert!(!container_has_active_audit_log_snap(snap, personal_alsc, now));
        assert!(!container_has_active_audit_log_snap(snap, npc_personal_alsc, now));
    });

    // 3. Personal owner and GM succeed via atomic entity_kv tx (recording failure then success).
    let wrong_pw_err = handle_containers_ships_and_fitting(
        &ctx,
        &mut owner_sess,
        None,
        "invbroker",
        "ALSCLogGet",
        &[PyRep::Integer(personal_alsc), PyRep::String("wrong".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(wrong_pw_err.code, "ALSCPasswordNeededToViewAuditLog");
    let p_logs = ctx
        .db
        .with_snapshot(|snap| container_alsc_logs_snap(snap, personal_alsc));
    assert_eq!(p_logs.len(), 1);
    assert_eq!(p_logs[0].action, "ALSCActionPasswordFailure");
    assert_eq!(p_logs[0].actor_id, owner_id);

    assert!(
        handle_containers_ships_and_fitting(
            &ctx,
            &mut gm_sess,
            None,
            "invbroker",
            "ALSCLogGet",
            &[PyRep::Integer(personal_alsc), PyRep::String("p-pw".into())],
            &kw,
        )
        .await
        .unwrap()
        .is_ok(),
        "GM with valid config password must be able to read personal ALSC log"
    );

    // 4. Corp-owned ALSC still rejects unprivileged corp mate without log mutation and allows Director+Config.
    let corp_plain_err = handle_containers_ships_and_fitting(
        &ctx,
        &mut mate_plain_sess,
        None,
        "invbroker",
        "ALSCLogGet",
        &[PyRep::Integer(corp_alsc)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(corp_plain_err.code, "ALSCAccessDenied");
    assert!(ctx
        .db
        .with_snapshot(|snap| container_alsc_logs_snap(snap, corp_alsc))
        .is_empty());

    let corp_dir_wrong = handle_containers_ships_and_fitting(
        &ctx,
        &mut mate_dir_sess,
        None,
        "invbroker",
        "ALSCLogGet",
        &[PyRep::Integer(corp_alsc), PyRep::String("bad".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(corp_dir_wrong.code, "ALSCPasswordNeededToViewAuditLog");
    assert_eq!(
        ctx.db
            .with_snapshot(|snap| container_alsc_logs_snap(snap, corp_alsc))[0]
            .action,
        "ALSCActionPasswordFailure"
    );

    assert!(
        handle_containers_ships_and_fitting(
            &ctx,
            &mut mate_dir_sess,
            None,
            "invbroker",
            "ALSCLogGet",
            &[PyRep::Integer(corp_alsc), PyRep::String("c-pw".into())],
            &kw,
        )
        .await
        .unwrap()
        .is_ok(),
        "corp Director + EquipmentConfig with valid config password must read corp ALSC log"
    );
}
