//! Deep branch and mutation-killing tests for `services::fitting_containers_ships`
//! covering packaged ship/container volumes, in-space Mobile Depot & SMA fitting range,
//! SMA/Fleet Hangar/ALSC move validation, ALSC password/role checks, and module unfit auth.

use super::fitting_containers_ships::*;
use crate::router::{BoundObjectEntry, ServerContext};
use eve_db::{FleetRecord, InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

fn make_ctx_and_session() -> (Arc<ServerContext>, SessionState) {
    let ctx = Arc::new(ServerContext::with_repository(Arc::new(
        eve_db::WorldRepository::with_demo_seed(),
    )));
    let mut sess = SessionState::new(91_501);
    sess.user_id = Some(1);
    sess.character_id = Some(90_000_001);
    sess.corporation_id = Some(98_000_001);
    sess.station_id = Some(60_003_760);
    sess.solar_system_id = Some(30_000_142);
    sess.ship_id = Some(100_000_001);
    ctx.sync_session(&mut sess);
    (ctx, sess)
}

fn mk_item(id: i64, tid: u32, owner: i64, loc: i64, flag: InventoryFlag, qty: i32, sing: bool) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id: id,
        type_id: tid,
        owner_id: owner,
        location_id: loc,
        flag,
        quantity: qty,
        singleton: sing,
        version: 1,
        ..Default::default()
    }
}

#[test]
fn packaged_hull_and_container_volumes_and_corp_log_target_resolution() {
    let (ctx, _) = make_ctx_and_session();
    let find_type_in_group = |grp: u32| -> u32 {
        ctx.sde
            .types
            .values()
            .find(|t| t.group_id == grp && t.category_id == 6)
            .map(|t| t.type_id)
            .unwrap()
    };

    for (grp, expected_vol) in [
        (31, 500.0),      // Shuttle
        (463, 3_750.0),   // Mining Barge
        (420, 5_000.0),   // Destroyer
        (26, 10_000.0),   // Cruiser
        (419, 15_000.0),  // Battlecruiser
        (28, 20_000.0),   // Industrial
        (27, 50_000.0),   // Battleship
    ] {
        let item = mk_item(1, find_type_in_group(grp), 90_000_001, 60_003_760, InventoryFlag::Hangar, 1, false);
        assert_eq!(
            effective_item_unit_volume(&ctx.sde, &item),
            expected_vol,
            "group {grp} packaged volume mismatch"
        );
    }

    // Capital ship (group 513 Freighter, volume > 50_000) -> clamped 5% volume
    let cap_item = mk_item(2, find_type_in_group(513), 90_000_001, 60_003_760, InventoryFlag::Hangar, 1, false);
    let cap_vol = effective_item_unit_volume(&ctx.sde, &cap_item);
    assert!((50_000.0..=1_000_000.0).contains(&cap_vol));

    // Packaged container volume = (t.volume * 0.1).min(5_000.0)
    let cont_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 340 && t.volume > 0.0)
        .map(|t| t.type_id)
        .unwrap();
    let pkg_cont = mk_item(3, cont_tid, 90_000_001, 60_003_760, InventoryFlag::Hangar, 1, false);
    let expected_c_vol = (ctx.sde.types.get(&cont_tid).unwrap().volume * 0.1).min(5_000.0);
    assert!((effective_item_unit_volume(&ctx.sde, &pkg_cont) - expected_c_vol).abs() < 1e-6);

    // resolve_corp_container_log_target across CorpSAG flags and parent-owned corp container
    let corp_id = 98_000_001;
    ctx.db.with_mut_snapshot(|snap| {
        for (idx, (flag, expected_str)) in [
            (InventoryFlag::CorpSAG1, "CorpSAG1"),
            (InventoryFlag::CorpSAG2, "CorpSAG2"),
            (InventoryFlag::CorpSAG3, "CorpSAG3"),
            (InventoryFlag::CorpSAG4, "CorpSAG4"),
            (InventoryFlag::CorpSAG5, "CorpSAG5"),
            (InventoryFlag::CorpSAG6, "CorpSAG6"),
            (InventoryFlag::CorpSAG7, "CorpSAG7"),
            (InventoryFlag::CorpDeliveries, "CorpDeliveries"),
            (InventoryFlag::Locked, "Locked"),
            (InventoryFlag::Unlocked, "Unlocked"),
            (InventoryFlag::Hangar, "Hangar"),
        ]
        .into_iter()
        .enumerate()
        {
            let cid = 890_000 + idx as i64;
            snap.items.insert(cid, mk_item(cid, cont_tid, corp_id, 60_003_760, flag, 1, true));
            let (_, _, _, flag_name) =
                resolve_corp_container_log_target(snap, cid).expect("corp log target");
            assert_eq!(flag_name, expected_str);
        }

        // Character-owned container inside corp parent item with flag CorpSAG1 (116)
        snap.items.insert(890_050, mk_item(890_050, 27, corp_id, 60_003_760, InventoryFlag::Hangar, 1, true));
        snap.items.insert(890_051, mk_item(890_051, cont_tid, 90_000_001, 890_050, InventoryFlag::CorpSAG1, 1, true));
        assert_eq!(
            resolve_corp_container_log_target(snap, 890_051).unwrap().0,
            corp_id
        );
    });
}

#[test]
fn ship_bay_access_fleet_corp_and_password_unlock_branches() {
    let (ctx, _) = make_ctx_and_session();
    let host_owner = 90_000_001;
    let fleet_mate = 90_000_002;
    let host_ship = 100_000_001;

    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            501,
            FleetRecord {
                fleet_id: 501,
                boss_id: host_owner,
                members: vec![host_owner, fleet_mate],
                ..Default::default()
            },
        );
    });

    write_container_ship_state(&ctx, |st| {
        st.ship_configs.insert(
            host_ship,
            ShipAccessConfig {
                allow_fleet_smb: true,
                allow_corp_smb: false,
                allow_fleet_hangar: true,
                allow_corp_hangar: false,
                password: Some("secret123".into()),
            },
        );
    });

    ctx.db.with_snapshot(|snap| {
        assert!(!can_character_access_ship_bay_with_password_snap(
            snap, fleet_mate, host_ship, host_owner, true, Some("wrong")
        ));
        assert!(can_character_access_ship_bay_with_password_snap(
            snap, fleet_mate, host_ship, host_owner, true, Some("secret123")
        ));
        assert!(can_character_access_ship_bay_with_password_snap(
            snap, fleet_mate, host_ship, host_owner, false, Some("secret123")
        ));
    });

    write_container_ship_state(&ctx, |st| {
        st.unlocked_containers.insert((fleet_mate, host_ship));
    });
    ctx.db.with_snapshot(|snap| {
        assert!(can_character_access_ship_bay_snap(
            snap, fleet_mate, host_ship, host_owner, true
        ));
    });
}

#[tokio::test]
async fn validate_container_and_bay_move_sma_fleet_hangar_and_container_errors() {
    let (ctx, _) = make_ctx_and_session();
    let char_id = 90_000_001;
    let kw = HashMap::new();

    let carrier_tid = ctx
        .sde
        .types
        .values()
        .find(|t| {
            t.category_id == 6
                && t.default_attributes.get(&908).copied().unwrap_or(0.0) > 0.0
                && t.default_attributes.get(&912).copied().unwrap_or(0.0) > 0.0
        })
        .map(|t| t.type_id)
        .unwrap();
    let find_grp = |grp: u32| ctx.sde.types.values().find(|t| t.group_id == grp).unwrap().type_id;
    let freighter_tid = find_grp(513);
    let station_cont_tid = find_grp(448);
    let siphon_tid = find_grp(1247);
    let depot_tid = find_grp(1246);
    let secure_cont_tid = find_grp(340);

    let (carrier_id, freighter_id, rifter_ship_id) = (891_001, 891_002, 100_000_001);
    let (small_ship_id, ore_id, locked_item_id) = (891_003, 891_004, 891_005);
    let (st_cont_id, siphon_id, other_depot_id, pkg_cont_id, asm_cont_id) =
        (891_006, 891_007, 891_008, 891_009, 891_010);

    ctx.db.with_mut_snapshot(|snap| {
        for (id, tid, owner, flag, qty, sing) in [
            (carrier_id, carrier_tid, char_id, InventoryFlag::Hangar, 1, true),
            (freighter_id, freighter_tid, char_id, InventoryFlag::Hangar, 1, true),
            (small_ship_id, 587, char_id, InventoryFlag::Hangar, 1, false),
            (ore_id, 34, char_id, InventoryFlag::Hangar, 100, false),
            (locked_item_id, 34, char_id, InventoryFlag::Locked, 1, false),
            (st_cont_id, station_cont_tid, char_id, InventoryFlag::Hangar, 1, true),
            (siphon_id, siphon_tid, char_id, InventoryFlag::Hangar, 1, true),
            (other_depot_id, depot_tid, 90_000_002, InventoryFlag::Hangar, 1, true),
            (pkg_cont_id, secure_cont_tid, char_id, InventoryFlag::Hangar, 1, false),
            (asm_cont_id, secure_cont_tid, char_id, InventoryFlag::Hangar, 1, true),
        ] {
            snap.items.insert(id, mk_item(id, tid, owner, 60_003_760, flag, qty, sing));
        }
    });

    for (item, dest, flag, expected_code) in [
        (locked_item_id, carrier_id, 4, "ItemLocked"),
        (st_cont_id, carrier_id, 4, "CapacityExceeded"),
        (asm_cont_id, freighter_id, 4, "OnlyFreightContainersAllowedInFreighters"),
        (ore_id, siphon_id, 4, "CannotAddToSiphon"),
        (ore_id, other_depot_id, 4, "OwnershipViolation"),
        (ore_id, pkg_cont_id, 4, "ContainerNotAssembled"),
        (pkg_cont_id, asm_cont_id, 4, "CannotPutContainerInContainer"),
        (small_ship_id, rifter_ship_id, 90, "NoShipMaintenanceBay"),
        (ore_id, carrier_id, 90, "CannotStoreCargoInSMA"),
        (small_ship_id, carrier_id, 90, "OnlyAssembledShipsInSMA"),
        (ore_id, rifter_ship_id, 155, "NoFleetHangar"),
    ] {
        assert_eq!(
            validate_container_and_bay_move(&ctx, char_id, item, dest, flag, &kw, None)
                .unwrap_err()
                .code,
            expected_code
        );
    }

    // Password-protected destination container failure then success
    write_container_ship_state(&ctx, |st| {
        st.set_container_password(asm_cont_id, 1, "contpw".into());
    });
    assert_eq!(
        validate_container_and_bay_move(&ctx, char_id, ore_id, asm_cont_id, 4, &kw, None)
            .unwrap_err()
            .code,
        "ALSCPasswordNeededToOpen"
    );
    let mut pw_kw = HashMap::new();
    pw_kw.insert("password".into(), PyRep::String("contpw".into()));
    assert!(validate_container_and_bay_move(&ctx, char_id, ore_id, asm_cont_id, 4, &pw_kw, None).is_ok());

    // Assemble small_ship_id and put ore inside its cargo -> ShipHasNonChargeCargo
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&small_ship_id).unwrap().singleton = true;
        snap.items.get_mut(&ore_id).unwrap().location_id = small_ship_id;
        snap.items.get_mut(&ore_id).unwrap().flag = InventoryFlag::Cargo;
    });
    assert_eq!(
        validate_container_and_bay_move(&ctx, char_id, small_ship_id, carrier_id, 90, &kw, None)
            .unwrap_err()
            .code,
        "ShipHasNonChargeCargo"
    );

    // Move ore back to station; now assembled empty ship stores in SMA (90) and ore stores in Fleet Hangar (155)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ore_id).unwrap().location_id = 60_003_760;
        snap.items.get_mut(&ore_id).unwrap().flag = InventoryFlag::Hangar;
    });
    assert!(validate_container_and_bay_move(&ctx, char_id, small_ship_id, carrier_id, 90, &kw, None).is_ok());
    assert!(validate_container_and_bay_move(&ctx, char_id, ore_id, carrier_id, 155, &kw, None).is_ok());
}

#[tokio::test]
async fn in_space_fitting_service_and_validate_ship_module_unfit_auth_branches() {
    let (ctx, mut sess) = make_ctx_and_session();
    let (char_id, ship_id, sys_id, mod_id, depot_id) = (90_000_001, 100_000_001, 30_000_142, 892_001, 892_002);
    let depot_tid = ctx.sde.types.values().find(|t| t.group_id == 1246).unwrap().type_id;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(mod_id, mk_item(mod_id, 438, char_id, ship_id, InventoryFlag::MedSlot0, 1, true));
    });

    // Wrong owner -> ItemOwnershipViolation; Locked -> ItemLocked
    let mut mod_rec = ctx.db.with_snapshot(|s| s.items.get(&mod_id).cloned().unwrap());
    mod_rec.owner_id = 90_000_002;
    assert_eq!(
        validate_ship_module_unfit_auth(&ctx, &sess, char_id, &mod_rec, 60_003_760).await.unwrap_err().code,
        "ItemOwnershipViolation"
    );
    mod_rec.owner_id = char_id;
    mod_rec.flag = InventoryFlag::Locked;
    assert_eq!(
        validate_ship_module_unfit_auth(&ctx, &sess, char_id, &mod_rec, 60_003_760).await.unwrap_err().code,
        "ItemLocked"
    );
    mod_rec.flag = InventoryFlag::MedSlot0;

    // Docked at station 60_003_760 -> valid unfit to station hangar; invalid destination -> InvalidItemLocation
    assert!(validate_ship_module_unfit_auth(&ctx, &sess, char_id, &mod_rec, 60_003_760).await.is_ok());
    assert_eq!(
        validate_ship_module_unfit_auth(&ctx, &sess, char_id, &mod_rec, 60_003_761).await.unwrap_err().code,
        "InvalidItemLocation"
    );

    // Undock into space (sys_id): without Mobile Depot -> CannotUnfitInSpace
    sess.station_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.solar_system_id = sys_id;
        }
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.location_id = sys_id as i64;
        }
    });
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
            SlimItemMeta {
                group_id: 25,
                category_id: 6,
                owner_id: char_id,
                char_id: Some(char_id),
                ..Default::default()
            },
        )
        .await;

    assert!(!is_near_in_space_fitting_service(&ctx, sys_id, ship_id).await);
    assert_eq!(
        validate_ship_module_unfit_auth(&ctx, &sess, char_id, &mod_rec, ship_id).await.unwrap_err().code,
        "CannotUnfitInSpace"
    );

    // Place owner's Mobile Depot within 1,000m -> is_near_in_space_fitting_service == true & unfit succeeds!
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(depot_id, mk_item(depot_id, depot_tid, char_id, sys_id as i64, InventoryFlag::Hangar, 1, true));
        snap.item_positions.insert(depot_id, [1_000.0, 0.0, 0.0]);
    });
    assert!(is_near_in_space_fitting_service(&ctx, sys_id, ship_id).await);
    assert!(validate_ship_module_unfit_auth(&ctx, &sess, char_id, &mod_rec, ship_id).await.is_ok());

    // Active weapons timer blocks in-space unfit -> CannotRefitWithWeaponsTimer
    let now_secs = ctx.now_unix_secs();
    ctx.crimewatch
        .write()
        .unwrap()
        .states
        .entry(char_id)
        .or_default()
        .weapon_timer_until_unix = now_secs + 60;
    assert_eq!(
        validate_ship_module_unfit_auth(&ctx, &sess, char_id, &mod_rec, ship_id).await.unwrap_err().code,
        "CannotRefitWithWeaponsTimer"
    );
}

#[tokio::test]
async fn alsc_lock_unlock_log_assemble_and_decode_item_branches() {
    let (ctx, mut sess) = make_ctx_and_session();
    let char_id = 90_000_001;
    let kw = HashMap::new();
    let secure_cont_tid = ctx.sde.types.values().find(|t| t.group_id == 340).unwrap().type_id;

    let (cont_id, outside_item_id, inside_item_id, bpc_item_id) = (893_001, 893_002, 893_003, 893_004);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(cont_id, mk_item(cont_id, secure_cont_tid, char_id, 60_003_760, InventoryFlag::Hangar, 1, false));
        snap.items.insert(outside_item_id, mk_item(outside_item_id, 34, char_id, 60_003_760, InventoryFlag::Hangar, 10, false));
        snap.items.insert(inside_item_id, mk_item(inside_item_id, 34, char_id, cont_id, InventoryFlag::Unlocked, 10, false));
        let mut bpc = mk_item(bpc_item_id, 587, char_id, 60_003_760, InventoryFlag::Hangar, 1, true);
        bpc.is_blueprint_copy = true;
        bpc.source_mutaplasmid_id = Some(47_740);
        bpc.dynamic_attributes.insert(40, 12.5);
        snap.items.insert(bpc_item_id, bpc);
        snap.item_custom_names.insert(bpc_item_id, "Abyssal Rifter".into());
    });

    // AssembleCargoContainer error branches & single-item in-place assembly
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "AssembleCargoContainer", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        handle_containers_ships_and_fitting(
            &ctx,
            &mut sess,
            None,
            "invbroker",
            "AssembleCargoContainer",
            &[PyRep::Integer(outside_item_id)],
            &kw
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "NotAContainer"
    );
    let asm_single = handle_containers_ships_and_fitting(
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
    .unwrap();
    assert_eq!(asm_single.dict_get("itemID"), Some(&PyRep::Integer(cont_id)));

    // ALSC lock/unlock & ALSCLogGet password and item location errors
    assert_eq!(
        handle_containers_ships_and_fitting(
            &ctx,
            &mut sess,
            None,
            "invbroker",
            "ALSCLock",
            &[PyRep::Integer(cont_id), PyRep::Integer(outside_item_id)],
            &kw
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "ItemNotInContainer"
    );
    write_container_ship_state(&ctx, |st| {
        st.set_container_password(cont_id, 1, "alscpw".into());
    });
    assert_eq!(
        handle_containers_ships_and_fitting(
            &ctx,
            &mut sess,
            None,
            "invbroker",
            "ALSCLock",
            &[PyRep::Integer(cont_id), PyRep::Integer(inside_item_id)],
            &kw
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "ALSCPasswordNeededToLock"
    );
    assert_eq!(
        handle_containers_ships_and_fitting(
            &ctx,
            &mut sess,
            None,
            "invbroker",
            "ALSCLogGet",
            &[PyRep::Integer(cont_id)],
            &kw
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "ALSCPasswordNeededToViewAuditLog"
    );

    // encodedItems.DecodeItem with Tuple and missing args
    let dec = handle_containers_ships_and_fitting(
        &ctx,
        &mut sess,
        None,
        "encodedItems",
        "DecodeItem",
        &[PyRep::Tuple(vec![PyRep::Integer(bpc_item_id), PyRep::Integer(587)])],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(dec.dict_get("customName"), Some(&PyRep::String("Abyssal Rifter".into())));
    assert_eq!(dec.dict_get("isBlueprintCopy"), Some(&PyRep::Bool(true)));
    assert_eq!(dec.dict_get("sourceMutaplasmidID"), Some(&PyRep::Integer(47_740)));
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut sess, None, "encodedItems", "DecodeItem", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    let _ = BoundObjectEntry {
        bound_id: 1,
        session_id: sess.session_id,
        user_id: 1,
        character_id: char_id,
        service: "invbroker".into(),
        object_id: cont_id,
        oid_str: "N=1:1".into(),
        ref_id: 1,
    };
}
