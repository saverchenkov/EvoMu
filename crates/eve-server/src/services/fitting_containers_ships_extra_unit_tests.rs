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

#[tokio::test]
async fn containers_ops_clear_charges_fit_fitting_alsc_retrieve_password_and_item_locking() {
    let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
    let mut sde = eve_sde::SdeRegistry::with_canonical_seed();
    // Ensure a blueprint (category 9), charge (category 8), module (category 7), container (group 340),
    // plastic wrap (3468), and Mobile Siphon (1247) / Depot (1246) / Beacon exist cleanly.
    let base_tpl = sde.types.get(&587).unwrap().clone();
    let ensure_type = |sde: &mut eve_sde::SdeRegistry, tid: u32, grp: u32, cat: u32, vol: f64, cap: f64, base_price: f64, effects: Vec<u16>| {
        if !sde.types.contains_key(&tid) {
            let mut t = base_tpl.clone();
            t.type_id = tid;
            t.group_id = grp;
            t.category_id = cat;
            t.volume = vol;
            t.capacity = cap;
            t.base_price = base_price;
            t.effect_ids = effects;
            sde.types.insert(tid, t);
        }
    };
    ensure_type(&mut sde, 3468, 346, 2, 1.0, 0.0, 0.0, vec![]);
    ensure_type(&mut sde, 995_001, 55, 7, 5.0, 0.0, 1_000.0, vec![12]);
    ensure_type(&mut sde, 995_002, 83, 8, 1.0, 0.0, 10.0, vec![]);
    ensure_type(&mut sde, 995_003, 105, 9, 0.01, 0.0, 50_000.0, vec![]);
    let cont_tid = sde.types.values().find(|t| t.group_id == 340 && t.volume > 0.0).map(|t| t.type_id).unwrap_or_else(|| {
        ensure_type(&mut sde, 995_004, 340, 2, 10.0, 100.0, 500.0, vec![]);
        995_004
    });

    let ctx = Arc::new(
        ServerContext::with_sde_and_repository(Arc::new(sde), db)
            .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED),
    );
    let char_id = 90_000_001_i64;
    let corp_id = 98_000_001_i64;
    let station_id = 60_003_760_i64;
    let sys_id = 30_000_142_u32;
    let ship_id = 100_000_001_i64;

    let mut sess = SessionState::new(91_601);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.station_id = Some(station_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    sess.corp_role = 0;
    ctx.sync_session(&mut sess);
    let kw = HashMap::new();

    // 1. ClearCharges validation & execution
    let mut bad_sess = sess.clone();
    bad_sess.ship_id = None;
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut bad_sess, None, "invbroker", "ClearCharges", &[], &kw)
            .await.unwrap().unwrap_err().code,
        "InvalidFittingTarget"
    );
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "ClearCharges", &[PyRep::Integer(9_999_999)], &kw)
            .await.unwrap().unwrap_err().code,
        "ShipNotFound"
    );

    let turret_id = 880_001_i64;
    let charge_slot_id = 880_002_i64;
    let cargo_fill_id = 880_003_i64;
    let corp_cont_id = 880_004_i64;
    let locked_corp_item = 880_005_i64;
    let job_bp_id = 880_006_i64;
    let escrow_item_id = 880_007_i64;
    let wrap_id = 880_008_i64;
    let wrap_child_id = 880_009_i64;
    let struct_id = 1_000_000_880_001_i64;
    let struct_bp_id = 880_010_i64;
    let hangar_mod_id = 880_011_i64;

    let ship_cap_m3 = ctx.db.with_snapshot(|snap| {
        let tid = snap.items[&ship_id].type_id;
        ctx.sde.types.get(&tid).map(|t| t.capacity).unwrap_or(100.0)
    });

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(turret_id, mk_item(turret_id, 995_001, char_id, ship_id, InventoryFlag::HiSlot0, 1, true));
        snap.items.insert(charge_slot_id, mk_item(charge_slot_id, 995_002, char_id, ship_id, InventoryFlag::HiSlot0, 5, false));
        // Overfill cargo so incoming 5.0 m3 exceeds capacity
        let fill_qty = (ship_cap_m3 / 1.0).ceil() as i32 + 10;
        snap.items.insert(cargo_fill_id, mk_item(cargo_fill_id, 995_002, char_id, ship_id, InventoryFlag::Cargo, fill_qty, false));

        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.flag = InventoryFlag::Locked;
        }
    });
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "ClearCharges", &[], &kw)
            .await.unwrap().unwrap_err().code,
        "ItemLocked"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().flag = InventoryFlag::Hangar;
        snap.module_active_states.insert(turret_id);
    });
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "ClearCharges", &[], &kw)
            .await.unwrap().unwrap_err().code,
        "ModuleIsActive"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.remove(&turret_id);
        snap.module_active_charges.insert(turret_id, 995_002);
    });
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "ClearCharges", &[], &kw)
            .await.unwrap().unwrap_err().code,
        "CapacityExceeded"
    );
    // Reduce cargo fill to 2 units so 5 incoming units merge cleanly
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&cargo_fill_id).unwrap().quantity = 2;
    });
    handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "ClearCharges", &[], &kw)
        .await.unwrap().unwrap();
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.module_active_charges.contains_key(&turret_id));
        let merged_total: i32 = snap.items.values()
            .filter(|i| i.location_id == ship_id && i.flag == InventoryFlag::Cargo && i.type_id == 995_002)
            .map(|i| i.quantity)
            .sum();
        assert_eq!(merged_total, 7);
    });

    // 2. FitFitting: Undocked, FittingNotFound, AccessDenied, SavedFitting, fitData invalid flag/qty & slot overflow
    let mut undocked = sess.clone();
    undocked.station_id = None;
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut undocked, None, "invbroker", "FitFitting", &[], &kw)
            .await.unwrap().unwrap_err().code,
        "NotDocked"
    );
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "FitFitting", &[PyRep::Integer(ship_id), PyRep::Integer(777_001)], &kw)
            .await.unwrap().unwrap_err().code,
        "FittingNotFound"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.saved_fittings.insert(777_002, eve_db::SavedFittingRecord {
            fitting_id: 777_002,
            owner_id: 90_999_999,
            name: "Foreign".into(),
            ship_type_id: 587,
            description: String::new(),
            items: vec![],
        });
        snap.saved_fittings.insert(777_003, eve_db::SavedFittingRecord {
            fitting_id: 777_003,
            owner_id: corp_id,
            name: "CorpFit".into(),
            ship_type_id: 0,
            description: String::new(),
            items: vec![(995_001, InventoryFlag::HiSlot1, 1)],
        });
        snap.items.insert(hangar_mod_id, mk_item(hangar_mod_id, 995_001, char_id, station_id, InventoryFlag::Hangar, 1, false));
    });
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "FitFitting", &[PyRep::Integer(ship_id), PyRep::Integer(777_002)], &kw)
            .await.unwrap().unwrap_err().code,
        "AccessDenied"
    );
    let fit_ok = handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "FitFitting", &[PyRep::Integer(ship_id), PyRep::Integer(777_003)], &kw)
        .await.unwrap().unwrap();
    assert!(matches!(fit_ok, PyRep::Dict(_) | PyRep::KeyVal(_) | PyRep::Tuple(_) | PyRep::List(_)));

    let mut bad_flag_kw = HashMap::new();
    bad_flag_kw.insert("fitData".into(), PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(995_001), PyRep::Integer(999), PyRep::Integer(1)])]));
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "FitFitting", &[PyRep::Integer(ship_id)], &bad_flag_kw)
            .await.unwrap().unwrap_err().code,
        "InvalidFittingFlag"
    );
    let mut bad_qty_kw = HashMap::new();
    bad_qty_kw.insert("fitData".into(), PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(995_001), PyRep::Integer(0)])]));
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "FitFitting", &[PyRep::Integer(ship_id)], &bad_qty_kw)
            .await.unwrap().unwrap_err().code,
        "InvalidQuantity"
    );
    let mut overflow_kw = HashMap::new();
    overflow_kw.insert("fitData".into(), PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(995_001), PyRep::Integer(20)])]));
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "FitFitting", &[PyRep::Integer(ship_id)], &overflow_kw)
            .await.unwrap().unwrap_err().message,
        "SlotIndexExceedsHullSlots"
    );

    // 3. GetAssetWorth, ListStations (blueprint_only & is_corp), Reprocess, RetrievePassword (roles, which=1/2/3), HasExistingPasswordSet
    let worth = handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "GetAssetWorth", &[], &kw).await.unwrap().unwrap();
    assert!(matches!(worth, PyRep::Tuple(ref v) if v.len() == 2 && matches!((&v[0], &v[1]), (PyRep::Real(a), PyRep::Real(p)) if *a >= 0.0 && *p >= 0.0)));

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(struct_id, eve_db::UpwellStructureRecord {
            structure_id: struct_id,
            type_id: 35832,
            owner_corp_id: corp_id,
            solar_system_id: sys_id,
            name: "Astra".into(),
            ..Default::default()
        });
        snap.items.insert(struct_bp_id, mk_item(struct_bp_id, 995_003, corp_id, struct_id, InventoryFlag::CorpSAG1, 1, false));
        snap.items.insert(corp_cont_id, mk_item(corp_cont_id, cont_tid, corp_id, station_id, InventoryFlag::CorpSAG1, 1, true));
        snap.items.insert(locked_corp_item, mk_item(locked_corp_item, 995_002, corp_id, station_id, InventoryFlag::Locked, 1, false));
        snap.items.insert(job_bp_id, mk_item(job_bp_id, 995_003, corp_id, station_id, InventoryFlag::Locked, 1, false));
        snap.industry_jobs.insert(44_001, eve_db::IndustryJobRecord {
            job_id: 44_001,
            installer_id: char_id,
            facility_id: station_id,
            blueprint_item_id: job_bp_id,
            blueprint_type_id: 995_003,
            status: "Active".into(),
            ..Default::default()
        });
        snap.contracts.insert(55_001, eve_db::ContractRecord { contract_id: 55_001, issuer_id: char_id, ..Default::default() });
        snap.items.insert(escrow_item_id, mk_item(escrow_item_id, 995_002, char_id, 55_001, InventoryFlag::Hangar, 1, false));
        snap.items.insert(wrap_id, mk_item(wrap_id, 3468, char_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(wrap_child_id, mk_item(wrap_child_id, 995_002, char_id, wrap_id, InventoryFlag::Hangar, 3, false));
    });
    let corp_bps = handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "ListStations", &[PyRep::Bool(true), PyRep::Bool(true)], &kw)
        .await.unwrap().unwrap();
    assert!(matches!(&corp_bps, PyRep::Rowset { rows, .. } if rows.iter().any(|row| row.first().and_then(|c| c.as_i64()) == Some(struct_id))));

    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut undocked, None, "invbroker", "Reprocess", &[PyRep::List(vec![])], &kw)
            .await.unwrap().unwrap_err().code,
        "NotDocked"
    );
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "Reprocess", &[PyRep::List(vec![])], &kw)
            .await.unwrap().unwrap(),
        PyRep::List(vec![])
    );

    // RetrievePassword + HasExistingPasswordSet
    write_container_ship_state(&ctx, |st| {
        st.set_container_password(corp_cont_id, 1, "genpw".into());
        st.set_container_password(corp_cont_id, 2, "cfgpw".into());
    });
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "HasExistingPasswordSet", &[PyRep::Integer(corp_cont_id), PyRep::Integer(2)], &kw)
            .await.unwrap().unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "RetrievePassword", &[PyRep::Integer(corp_cont_id), PyRep::Integer(3)], &kw)
            .await.unwrap().unwrap_err().code,
        "InvalidPasswordType"
    );
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            if corp.ceo_id == char_id {
                corp.ceo_id = 90_000_099;
            }
            corp.member_roles.remove(&char_id);
        }
        snap.corp_role_assignments.remove(&(corp_id, char_id));
    });
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "RetrievePassword", &[PyRep::Integer(corp_cont_id), PyRep::Integer(1)], &kw)
            .await.unwrap().unwrap_err().code,
        "CorpRoleRequired"
    );
    sess.corp_role = eve_db::corp_roles::ROLE_EQUIPMENT_CONFIG;
    ctx.sync_session(&mut sess);
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "RetrievePassword", &[PyRep::Integer(corp_cont_id), PyRep::Integer(2)], &kw)
            .await.unwrap().unwrap(),
        PyRep::String("cfgpw".into())
    );
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "RetrievePassword", &[PyRep::Integer(corp_cont_id), PyRep::Integer(1)], &kw)
            .await.unwrap().unwrap(),
        PyRep::String("genpw".into())
    );
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "RetrievePassword", &[PyRep::Integer(ship_id), PyRep::Integer(1)], &kw)
            .await.unwrap().unwrap_err().code,
        "ContainerDoesNotSupportPassword"
    );

    // 4. itemLocking.GetLockedItemLocations & GetItemsByLocation (vote-locked + escrow included, active job excluded)
    let locked_locs = handle_containers_ships_and_fitting(&ctx, &mut sess, None, "itemLocking", "GetLockedItemLocations", &[], &kw)
        .await.unwrap().unwrap();
    if let PyRep::List(v) = locked_locs {
        assert!(v.contains(&PyRep::Integer(station_id)));
        assert!(v.contains(&PyRep::Integer(55_001)));
    }
    let locked_by_loc = handle_containers_ships_and_fitting(&ctx, &mut sess, None, "itemLocking", "GetItemsByLocation", &[PyRep::Integer(station_id)], &kw)
        .await.unwrap().unwrap();
    if let PyRep::Dict(entries) = locked_by_loc {
        assert!(entries.iter().any(|(k, _)| *k == PyRep::Integer(locked_corp_item)));
        assert!(!entries.iter().any(|(k, _)| *k == PyRep::Integer(job_bp_id)));
    }

    // 5. BreakPlasticWrap validation + unwrap children + delete wrap
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "BreakPlasticWrap", &[PyRep::Integer(corp_cont_id)], &kw)
            .await.unwrap().unwrap_err().code,
        "InvalidPlasticWrap"
    );
    handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "BreakPlasticWrap", &[PyRep::Integer(wrap_id)], &kw)
        .await.unwrap().unwrap();
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&wrap_id));
        assert_eq!(snap.items[&wrap_child_id].location_id, station_id);
    });
}

#[tokio::test]
async fn pi_import_export_and_bay_move_boundary_mutants() {
    use crate::services::industry_pi::pi_helpers::{
        save_orbital_settings, save_pi_network, OrbitalCustomsSettings, PiColonyNetworkState,
        PiPinState,
    };

    let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
    let mut sde = eve_sde::SdeRegistry::with_canonical_seed();
    let base_tpl = sde.types.get(&587).unwrap().clone();
    let ensure_t = |sde: &mut eve_sde::SdeRegistry, tid: u32, grp: u32, cat: u32, vol: f64, cap: f64| {
        if !sde.types.contains_key(&tid) {
            let mut t = base_tpl.clone();
            t.type_id = tid;
            t.group_id = grp;
            t.category_id = cat;
            t.volume = vol;
            t.capacity = cap;
            sde.types.insert(tid, t);
        }
    };
    ensure_t(&mut sde, 2256, 1030, 41, 100.0, 10_000.0); // Launchpad pin
    ensure_t(&mut sde, 2257, 1029, 41, 100.0, 10_000.0); // Storage pin (non-launchpad)
    ensure_t(&mut sde, 996_001, 1042, 43, 0.38, 0.0);     // P1 Commodity
    ensure_t(&mut sde, 996_002, 513, 6, 10_000.0, 50_000.0); // Freighter
    ensure_t(&mut sde, 996_003, 1247, 2, 10.0, 100.0);    // Siphon
    ensure_t(&mut sde, 996_004, 12, 2, 10.0, 100.0);      // Generic container
    let mut sma_ship_t = base_tpl.clone();
    sma_ship_t.type_id = 996_005;
    sma_ship_t.group_id = 547;
    sma_ship_t.category_id = 6;
    sma_ship_t.volume = 100_000.0;
    sma_ship_t.default_attributes.insert(908, 250_000.0); // SMA cap
    sma_ship_t.default_attributes.insert(912, 50_000.0);  // Fleet Hangar cap
    sde.types.insert(996_005, sma_ship_t);

    let ctx = Arc::new(
        ServerContext::with_sde_and_repository(Arc::new(sde), db)
            .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED),
    );
    let char_id = 90_000_001_i64;
    let corp_id = 98_000_001_i64;
    let sys_id = 30_000_142_u32;
    let planet_id = 40_000_001_i64;
    let launchpad_pin_id = 7_000_001_i64;
    let storage_pin_id = 7_000_002_i64;
    let orbital_id = 1_000_000_700_001_i64;

    let mut sess = SessionState::new(91_602);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(sys_id);
    ctx.sync_session(&mut sess);
    let kw = HashMap::new();

    // 1. Additional validate_container_and_bay_move / validate_single_item_can_be_opened gates
    let freighter_id = 890_001_i64;
    let siphon_id = 890_002_i64;
    let unpackaged_cont_id = 890_003_i64;
    let assembled_cont_id = 890_004_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(freighter_id, mk_item(freighter_id, 996_002, char_id, 60_003_760, InventoryFlag::Hangar, 1, true));
        snap.items.insert(siphon_id, mk_item(siphon_id, 996_003, char_id, 60_003_760, InventoryFlag::Hangar, 1, true));
        snap.items.insert(unpackaged_cont_id, mk_item(unpackaged_cont_id, 996_004, char_id, 60_003_760, InventoryFlag::Hangar, 1, false));
        snap.items.insert(assembled_cont_id, mk_item(assembled_cont_id, 996_004, char_id, 60_003_760, InventoryFlag::Hangar, 1, true));
    });
    assert_eq!(validate_container_and_bay_move(&ctx, char_id, freighter_id, freighter_id, 5, &kw, None).unwrap_err().message, "CannotStoreShipInItself");
    for (flag_bad, code) in [(72, "ImpoundedItem"), (36, "AssetSafetyLocked"), (63, "ItemLocked")] {
        assert_eq!(validate_container_and_bay_move(&ctx, char_id, freighter_id, 60_003_760, flag_bad, &kw, None).unwrap_err().code, code);
    }
    assert_eq!(validate_container_and_bay_move(&ctx, char_id, assembled_cont_id, siphon_id, 5, &kw, None).unwrap_err().code, "CannotAddToSiphon");
    assert_eq!(validate_container_and_bay_move(&ctx, char_id, assembled_cont_id, freighter_id, 5, &kw, None).unwrap_err().code, "OnlyFreightContainersAllowedInFreighters");
    assert_eq!(validate_container_and_bay_move(&ctx, char_id, freighter_id, unpackaged_cont_id, 5, &kw, None).unwrap_err().code, "ContainerNotAssembled");
    assert_eq!(validate_container_and_bay_move(&ctx, char_id, assembled_cont_id, assembled_cont_id + 10, 5, &kw, None).is_ok(), true);

    assert_eq!(validate_single_item_can_be_opened(&ctx, char_id, 0).await.unwrap_err().code, "InvalidArgument");
    assert_eq!(validate_single_item_can_be_opened(&ctx, char_id, 9_999_998).await.unwrap_err().code, "ItemNotFound");
    assert_eq!(validate_single_item_can_be_opened(&ctx, char_id, unpackaged_cont_id).await.unwrap_err().code, "CantOpenPackagedItem");

    // 2. ImportExportWithPlanet validation + ByItemId + ByTypeId multi-stack + Export merge + Tax
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "ImportExportWithPlanet", &[PyRep::Integer(0)], &kw)
            .await.unwrap().unwrap_err().code,
        "PinNotFound"
    );
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "ImportExportWithPlanet", &[PyRep::Integer(launchpad_pin_id), PyRep::None, PyRep::None], &kw)
            .await.unwrap().unwrap_err().code,
        "InvalidTransferArguments"
    );

    let imp_by_item_id = 890_010_i64;
    let imp_stack_a = 890_011_i64;
    let imp_stack_b = 890_012_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(orbital_id, eve_db::UpwellStructureRecord {
            structure_id: orbital_id,
            type_id: 2233,
            owner_corp_id: corp_id,
            solar_system_id: sys_id,
            name: format!("Orbital:{planet_id}"),
            ..Default::default()
        });
        snap.planetary_colonies.insert((char_id, planet_id), eve_db::PlanetaryColonyRecord {
            planet_id,
            owner_id: char_id,
            solar_system_id: sys_id,
            command_center_level: 4,
            pins_count: 2,
            extracted_commodities: HashMap::new(),
            last_extracted_unix: 0,
        });
        let mut net = PiColonyNetworkState::default();
        net.pins.push(PiPinState {
            pin_id: launchpad_pin_id,
            type_id: 2256,
            contents: HashMap::from([(996_001, 50_u64)]),
            ..Default::default()
        });
        net.pins.push(PiPinState {
            pin_id: storage_pin_id,
            type_id: 2257,
            ..Default::default()
        });
        save_pi_network(snap, char_id, planet_id, &net);
        save_orbital_settings(snap, planet_id, &OrbitalCustomsSettings {
            planet_id,
            orbital_type_id: 2233,
            owner_corp_id: corp_id,
            corporation_tax_rate: 0.05,
            alliance_tax_rate: 0.05,
            standing_high_tax_rate: 0.05,
            standing_good_tax_rate: 0.05,
            standing_neutral_tax_rate: 0.05,
            standing_bad_tax_rate: 0.05,
            standing_horrible_tax_rate: 0.05,
            allow_alliance: true,
            allow_standings: true,
            minimum_standing: -10.0,
        });
        snap.items.insert(imp_by_item_id, mk_item(imp_by_item_id, 996_001, char_id, orbital_id, InventoryFlag::Hangar, 10, false));
        snap.items.insert(imp_stack_a, mk_item(imp_stack_a, 996_001, char_id, orbital_id, InventoryFlag::Hangar, 5, false));
        snap.items.insert(imp_stack_b, mk_item(imp_stack_b, 996_001, char_id, orbital_id, InventoryFlag::Hangar, 10, false));
        snap.characters.get_mut(&char_id).unwrap().balance = eve_db::IskCents(100_000_000_00);
    });

    // Container-in-container & SMA/FleetHangar validation arms
    assert_eq!(validate_container_and_bay_move(&ctx, char_id, siphon_id, assembled_cont_id, 5, &kw, None).unwrap_err().code, "CannotPutContainerInContainer");
    let no_bay_ship = 890_005_i64;
    let sma_ship = 890_006_i64;
    let pkg_ship = 890_007_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(no_bay_ship, mk_item(no_bay_ship, 587, char_id, 60_003_760, InventoryFlag::Hangar, 1, true));
        snap.items.insert(pkg_ship, mk_item(pkg_ship, 587, char_id, 60_003_760, InventoryFlag::Hangar, 1, false));
        snap.items.insert(sma_ship, mk_item(sma_ship, 996_005, 90_000_099, 60_003_760, InventoryFlag::Hangar, 1, true));
    });
    assert_eq!(validate_container_and_bay_move(&ctx, char_id, freighter_id, no_bay_ship, 90, &kw, None).unwrap_err().code, "NoShipMaintenanceBay");
    assert_eq!(validate_container_and_bay_move(&ctx, char_id, freighter_id, no_bay_ship, 155, &kw, None).unwrap_err().code, "NoFleetHangar");
    assert_eq!(validate_container_and_bay_move(&ctx, char_id, pkg_ship, sma_ship, 90, &kw, None).unwrap_err().code, "OnlyAssembledShipsInSMA");
    assert_eq!(validate_container_and_bay_move(&ctx, char_id, siphon_id, sma_ship, 90, &kw, None).unwrap_err().code, "CannotStoreCargoInSMA");
    assert_eq!(validate_container_and_bay_move(&ctx, char_id, freighter_id, sma_ship, 90, &kw, None).unwrap_err().code, "SMAAccessDenied");
    assert_eq!(validate_container_and_bay_move(&ctx, char_id, freighter_id, sma_ship, 155, &kw, None).unwrap_err().code, "FleetHangarAccessDenied");

    // Storage pin fails InvalidPinTypeForLaunch
    assert_eq!(
        handle_containers_ships_and_fitting(
            &ctx, &mut sess, None, "invbroker", "ImportExportWithPlanet",
            &[PyRep::Integer(orbital_id), PyRep::Integer(storage_pin_id), PyRep::Dict(vec![(PyRep::Integer(imp_by_item_id), PyRep::Integer(1))]), PyRep::None],
            &kw,
        ).await.unwrap().unwrap_err().code,
        "InvalidPinTypeForLaunch"
    );

    // Import 1 unit ByTypeId first to verify positive transfer, then 30_000-unit overflow -> LaunchpadCapacityExceeded
    assert_eq!(
        handle_containers_ships_and_fitting(
            &ctx, &mut sess, None, "invbroker", "ImportExportWithPlanet",
            &[PyRep::Integer(orbital_id), PyRep::Integer(launchpad_pin_id), PyRep::Dict(vec![(PyRep::Integer(996_001), PyRep::Integer(1))]), PyRep::None],
            &kw,
        ).await.unwrap().unwrap(),
        PyRep::None
    );
    let huge_stack = 890_099_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(huge_stack, mk_item(huge_stack, 996_001, char_id, orbital_id, InventoryFlag::Hangar, 30_000, false));
    });
    assert_eq!(
        handle_containers_ships_and_fitting(
            &ctx, &mut sess, None, "invbroker", "ImportExportWithPlanet",
            &[PyRep::Integer(orbital_id), PyRep::Integer(launchpad_pin_id), PyRep::Dict(vec![(PyRep::Integer(huge_stack), PyRep::Integer(30_000))]), PyRep::None],
            &kw,
        ).await.unwrap().unwrap_err().code,
        "LaunchpadCapacityExceeded"
    );
    ctx.db.with_mut_snapshot(|snap| { snap.items.remove(&huge_stack); });

    // Import 5 units ByItemId + export 20 units (merges into existing hangar stack!)
    let journal_before = ctx.db.with_snapshot(|s| s.wallet_journal.len());
    let corp_bal_before = ctx.db.with_snapshot(|s| s.corporations[&corp_id].division_balance(1).0);
    let total_before: i32 = ctx.db.with_snapshot(|snap| {
        snap.items.values()
            .filter(|i| i.owner_id == char_id && i.location_id == orbital_id && i.type_id == 996_001)
            .map(|i| i.quantity)
            .sum()
    });
    let pi_ok = handle_containers_ships_and_fitting(
        &ctx, &mut sess, None, "invbroker", "ImportExportWithPlanet",
        &[
            PyRep::Integer(orbital_id),
            PyRep::Integer(launchpad_pin_id),
            PyRep::Dict(vec![(PyRep::Integer(imp_by_item_id), PyRep::Integer(5))]),
            PyRep::Dict(vec![(PyRep::Integer(996_001), PyRep::Integer(20))]),
        ],
        &kw,
    ).await.unwrap().unwrap();
    assert_eq!(pi_ok, PyRep::None);

    ctx.db.with_snapshot(|snap| {
        let total_after: i32 = snap.items.values()
            .filter(|i| i.owner_id == char_id && i.location_id == orbital_id && i.type_id == 996_001)
            .map(|i| i.quantity)
            .sum();
        assert_eq!(total_after, total_before - 5 + 20);
        let net = crate::services::industry_pi::load_pi_network(snap, char_id, planet_id, 4);
        let pin = net.pins.iter().find(|p| p.pin_id == launchpad_pin_id).unwrap();
        assert_eq!(pin.contents.get(&996_001).copied(), Some(36)); // 50 + 1 + 5 - 20 = 36
        assert!(snap.wallet_journal.len() > journal_before);
        assert!(snap.corporations[&corp_id].division_balance(1).0 >= corp_bal_before);
    });

    // Multi-stack ByTypeId import consuming across remaining hangar stacks
    handle_containers_ships_and_fitting(
        &ctx, &mut sess, None, "invbroker", "ImportExportWithPlanet",
        &[PyRep::Integer(orbital_id), PyRep::Integer(launchpad_pin_id), PyRep::Dict(vec![(PyRep::Integer(996_001), PyRep::Integer(16))]), PyRep::None],
        &kw,
    ).await.unwrap().unwrap();
    ctx.db.with_snapshot(|snap| {
        let net = crate::services::industry_pi::load_pi_network(snap, char_id, planet_id, 4);
        let pin = net.pins.iter().find(|p| p.pin_id == launchpad_pin_id).unwrap();
        assert_eq!(pin.contents.get(&996_001).copied(), Some(52));
    });
}

#[tokio::test]
async fn invbroker_misc_crystal_damage_fit_fitting_edge_cases_and_pi_errors() {
    use crate::services::industry_pi::pi_helpers::{
        save_pi_network, PiColonyNetworkState, PiPinState,
    };

    let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
    let mut sde = eve_sde::SdeRegistry::with_canonical_seed();
    let base_tpl = sde.types.get(&587).unwrap().clone();
    let mut crystal_t = base_tpl.clone();
    crystal_t.type_id = 997_001;
    crystal_t.group_id = 86;
    crystal_t.category_id = 8;
    crystal_t.volume = 1.0;
    crystal_t.default_attributes.insert(786, 1.0); // crystalsGetDamaged
    sde.types.insert(997_001, crystal_t);

    let mut drone_t = base_tpl.clone();
    drone_t.type_id = 997_002;
    drone_t.group_id = 100;
    drone_t.category_id = 18;
    drone_t.volume = 5.0;
    sde.types.insert(997_002, drone_t);

    let mut charge_t = base_tpl.clone();
    charge_t.type_id = 997_003;
    charge_t.group_id = 83;
    charge_t.category_id = 8;
    charge_t.volume = 0.1;
    sde.types.insert(997_003, charge_t);

    let mut depot_t = base_tpl.clone();
    depot_t.type_id = 997_004;
    depot_t.group_id = 1246; // GROUP_MOBILE_DEPOT
    depot_t.category_id = 23;
    depot_t.volume = 50.0;
    sde.types.insert(997_004, depot_t);

    let mut pi_comm_t = base_tpl.clone();
    pi_comm_t.type_id = 997_005;
    pi_comm_t.group_id = 1042;
    pi_comm_t.category_id = 43;
    pi_comm_t.volume = 0.38;
    sde.types.insert(997_005, pi_comm_t);

    let mut cont_t = base_tpl.clone();
    cont_t.type_id = 997_006;
    cont_t.group_id = 340;
    cont_t.category_id = 2;
    cont_t.volume = 10.0;
    cont_t.capacity = 100.0;
    sde.types.insert(997_006, cont_t);
    if let Some(ship_t) = sde.types.get_mut(&587) {
        ship_t.default_attributes.insert(eve_sde::attr_ids::DRONE_CAPACITY, 50.0);
        ship_t.capacity = ship_t.capacity.max(100.0);
    }
    for (tid, eff) in [(997_010_u32, 13_u16), (997_011_u32, 2663_u16), (997_012_u32, 3772_u16)] {
        let mut mt = base_tpl.clone();
        mt.type_id = tid;
        mt.group_id = 55;
        mt.category_id = 7;
        mt.volume = 1.0;
        mt.effect_ids = vec![eff];
        sde.types.insert(tid, mt);
    }
    if let Some(ss) = sde.solar_systems.get_mut(&30_000_144) {
        ss.security_status = 0.9;
    }

    let ctx = Arc::new(
        ServerContext::with_sde_and_repository(Arc::new(sde), db)
            .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED),
    );
    let char_id = 90_000_001_i64;
    let corp_id = 98_000_001_i64;
    let station_id = 60_003_760_i64;
    let sys_id = 30_000_142_u32;
    let ship_id = 100_000_001_i64;

    let mut sess = SessionState::new(91_603);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.station_id = Some(station_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    ctx.sync_session(&mut sess);
    let kw = HashMap::new();

    let cry_dmg_id = 895_001_i64;
    let cry_hp_id = 895_002_i64;
    let other_depot_id = 895_003_i64;
    let pw_cont_id = 895_004_i64;
    let cont_child_id = 895_005_i64;
    let distant_cont_id = 895_006_i64;
    let wrong_cont_child = 895_007_i64;
    let courier_wrap_id = 895_008_i64;
    let plain_member_id = 90_000_098_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut plain_ch = snap.characters[&char_id].clone();
        plain_ch.character_id = plain_member_id;
        plain_ch.user_id = 90_000_098;
        plain_ch.corporation_id = corp_id;
        snap.characters.insert(plain_member_id, plain_ch);

        let mut c1 = mk_item(cry_dmg_id, 997_001, char_id, station_id, InventoryFlag::Hangar, 1, false);
        c1.dynamic_attributes.insert(eve_dogma::attr_ids::DAMAGE, 0.42);
        snap.items.insert(cry_dmg_id, c1);

        let mut c2 = mk_item(cry_hp_id, 997_001, char_id, station_id, InventoryFlag::Hangar, 1, false);
        c2.dynamic_attributes.insert(eve_dogma::attr_ids::HP_STRUCT, 0.75);
        snap.items.insert(cry_hp_id, c2);

        snap.items.insert(other_depot_id, mk_item(other_depot_id, 997_004, 90_000_099, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(pw_cont_id, mk_item(pw_cont_id, 997_006, corp_id, station_id, InventoryFlag::CorpSAG2, 1, true));
        snap.items.insert(cont_child_id, mk_item(cont_child_id, 997_003, corp_id, pw_cont_id, InventoryFlag::Hangar, 5, false));
        snap.items.insert(distant_cont_id, mk_item(distant_cont_id, 997_006, char_id, 60_003_761, InventoryFlag::Hangar, 1, true));
        snap.items.insert(wrong_cont_child, mk_item(wrong_cont_child, 997_003, char_id, station_id, InventoryFlag::Hangar, 1, false));
        snap.items.insert(courier_wrap_id, mk_item(courier_wrap_id, 3468, 90_000_099, station_id, InventoryFlag::Hangar, 1, true));

        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
        }
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.ceo_id = 90_000_099;
            corp.member_roles.remove(&char_id);
        }
        snap.corp_role_assignments.remove(&(corp_id, char_id));
        snap.corp_governance.remove(&corp_id);
    });
    write_container_ship_state(&ctx, |st| {
        st.set_container_password(pw_cont_id, 1, "secret".into());
    });

    // 1. GetDamageForCrystals reads DAMAGE and HP_STRUCT fallback
    let dmg_map = handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "GetDamageForCrystals", &[], &kw)
        .await.unwrap().unwrap();
    if let PyRep::Dict(entries) = dmg_map {
        assert!(entries.iter().any(|(k, v)| *k == PyRep::Integer(cry_dmg_id) && matches!(v, PyRep::Real(r) if (*r - 0.42).abs() < 1e-6)));
        assert!(entries.iter().any(|(k, v)| *k == PyRep::Integer(cry_hp_id) && matches!(v, PyRep::Real(r) if (*r - 0.25).abs() < 1e-6)));
    } else {
        panic!("expected Dict from GetDamageForCrystals");
    }

    // 2. validate_single_item_can_be_opened + validate_container_and_bay_move from passworded & corp container
    assert_eq!(validate_single_item_can_be_opened(&ctx, char_id, cry_dmg_id).await.unwrap_err().code, "CantOpenPackagedItem");
    assert_eq!(validate_single_item_can_be_opened(&ctx, char_id, other_depot_id).await.unwrap_err().code, "OwnershipViolation");
    assert_eq!(validate_single_item_can_be_opened(&ctx, char_id, pw_cont_id).await.unwrap_err().code, "ContainerIsLocked");

    assert_eq!(validate_container_and_bay_move(&ctx, plain_member_id, cont_child_id, station_id, 4, &kw, None).unwrap_err().code, "ALSCPasswordNeededToOpen");
    let mut pw_kw = HashMap::new();
    pw_kw.insert("password".into(), PyRep::String("secret".into()));
    assert_eq!(validate_container_and_bay_move(&ctx, plain_member_id, cont_child_id, station_id, 4, &pw_kw, None).unwrap_err().code, "CorpRoleDenied");

    // 3. ALSC Lock/Unlock + Audit Log validation (ColocationRequired, ALSCAccessDenied, ItemNotFound, ItemNotInContainer, Wrong/Right password)
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "ALSCLock", &[PyRep::Integer(distant_cont_id), PyRep::List(vec![PyRep::Integer(wrong_cont_child)])], &kw)
            .await.unwrap().unwrap_err().code,
        "ColocationRequired"
    );
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "ALSCLockItems", &[PyRep::Integer(pw_cont_id), PyRep::List(vec![PyRep::Integer(cont_child_id)])], &kw)
            .await.unwrap().unwrap_err().code,
        "ALSCAccessDenied"
    );
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.member_roles.insert(char_id, eve_db::corp_roles::ROLE_DIRECTOR);
        }
    });
    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    ctx.sync_session(&mut sess);
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "ALSCLockItems", &[PyRep::Integer(pw_cont_id), PyRep::List(vec![PyRep::Integer(9_999_999)])], &kw)
            .await.unwrap().unwrap_err().code,
        "ItemNotFound"
    );
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "ALSCLockItems", &[PyRep::Integer(pw_cont_id), PyRep::List(vec![PyRep::Integer(wrong_cont_child)])], &kw)
            .await.unwrap().unwrap_err().code,
        "ItemNotInContainer"
    );
    write_container_ship_state(&ctx, |st| { st.unlocked_containers.clear(); });
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "ALSCLockItems", &[PyRep::Integer(pw_cont_id), PyRep::List(vec![PyRep::Integer(cont_child_id)])], &kw)
            .await.unwrap().unwrap_err().code,
        "ALSCPasswordNeededToLock"
    );
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "ALSCLogGet", &[PyRep::Integer(pw_cont_id)], &kw)
            .await.unwrap().unwrap_err().code,
        "ALSCPasswordNeededToViewAuditLog"
    );
    handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "ALSCLogGet", &[PyRep::Integer(pw_cont_id), PyRep::String("secret".into())], &kw)
        .await.unwrap().unwrap();
    handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "ALSCUnlock", &[PyRep::Integer(pw_cont_id), PyRep::List(vec![PyRep::Integer(cont_child_id)])], &pw_kw)
        .await.unwrap().unwrap();
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "BreakPlasticWrap", &[PyRep::Integer(courier_wrap_id)], &kw)
            .await.unwrap().unwrap_err().code,
        "NotItemOwner"
    );

    // 4. FitFitting validation branches (3-item negative tid/flag/qty, 2-item negative tid/qty, missing type, Drone & Charge auto-routing)
    for (label, list, expected_code) in [
        ("neg tid3", vec![PyRep::Tuple(vec![PyRep::Integer(-1), PyRep::Integer(27), PyRep::Integer(1)])], "ItemNotFound"),
        ("neg flg3", vec![PyRep::Tuple(vec![PyRep::Integer(997_003), PyRep::Integer(-1), PyRep::Integer(1)])], "InvalidFittingFlag"),
        ("bad qty3", vec![PyRep::Tuple(vec![PyRep::Integer(997_003), PyRep::Integer(5), PyRep::Integer(0)])], "InvalidQuantity"),
        ("neg tid2", vec![PyRep::Tuple(vec![PyRep::Integer(-1), PyRep::Integer(1)])], "ItemNotFound"),
        ("bad qty2", vec![PyRep::Tuple(vec![PyRep::Integer(997_003), PyRep::Integer(0)])], "InvalidQuantity"),
        ("unk tid2", vec![PyRep::Tuple(vec![PyRep::Integer(999_999), PyRep::Integer(1)])], "ItemNotFound"),
    ] {
        let mut fkw = HashMap::new();
        fkw.insert("fitData".into(), PyRep::List(list));
        let err = handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "FitFitting", &[PyRep::Integer(ship_id)], &fkw)
            .await.unwrap().unwrap_err();
        assert_eq!(err.code, expected_code, "FitFitting case {label}");
    }
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(895_020, mk_item(895_020, 997_002, char_id, station_id, InventoryFlag::Hangar, 1, false));
        snap.items.insert(895_021, mk_item(895_021, 997_003, char_id, station_id, InventoryFlag::Hangar, 2, false));
    });
    let mut fit_auto_kw = HashMap::new();
    fit_auto_kw.insert("fitData".into(), PyRep::List(vec![
        PyRep::Tuple(vec![PyRep::Integer(997_002), PyRep::Integer(1)]),
        PyRep::Tuple(vec![PyRep::Integer(997_003), PyRep::Integer(2)]),
    ]));
    assert!(handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "FitFitting", &[PyRep::Integer(ship_id)], &fit_auto_kw)
        .await.unwrap().is_ok());

    // 4b. Extra coverage for Fitting (empty shipID, med/rig/sub/cargo 2-tuple overflow/cargo), ALSCAssemble, item-lock/capacity gates, and validate_ship_module_unfit_auth
    for tid in [997_010_i64, 997_011, 997_012] {
        let mut overflow_kw = HashMap::new();
        overflow_kw.insert("fitData".into(), PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(tid), PyRep::Integer(10)])]));
        assert_eq!(
            handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "FitFitting", &[PyRep::Integer(ship_id)], &overflow_kw)
                .await.unwrap().unwrap_err().message,
            "SlotIndexExceedsHullSlots"
        );
    }
    let mut no_ship_sess = sess.clone();
    no_ship_sess.ship_id = None;
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut no_ship_sess, None, "invbroker", "FitFitting", &[], &kw)
            .await.unwrap().unwrap_err().code,
        "InvalidFittingTarget"
    );
    let mut cargo_fit_kw = HashMap::new();
    cargo_fit_kw.insert("fitData".into(), PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(997_006), PyRep::Integer(1)])]));
    assert!(handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "FitFitting", &[PyRep::Integer(ship_id)], &cargo_fit_kw)
        .await.unwrap().is_ok());

    assert_eq!(validate_single_item_can_be_opened(&ctx, char_id, 997_003).await.unwrap_err().code, "ItemNotFound");
    let non_cont_id = 895_030_i64;
    let fat_item_id = 895_031_i64;
    let corp_stack_cont = 895_032_i64;
    let remote_corp_cont = 895_033_i64;
    let owned_other_ship = 895_034_i64;
    let remote_own_ship = 895_035_i64;
    let remote_mod = 895_036_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(non_cont_id, mk_item(non_cont_id, 997_001, char_id, station_id, InventoryFlag::Hangar, 1, false));
        snap.items.insert(fat_item_id, mk_item(fat_item_id, 997_001, char_id, station_id, InventoryFlag::Hangar, 150, false)); // 150 * 1 = 150 m3 > 100 m3
        snap.items.insert(corp_stack_cont, mk_item(corp_stack_cont, 997_006, corp_id, station_id, InventoryFlag::CorpSAG1, 2, false));
        snap.items.insert(remote_corp_cont, mk_item(remote_corp_cont, 997_006, corp_id, 60_003_761, InventoryFlag::CorpSAG1, 1, false));
        snap.items.insert(owned_other_ship, mk_item(owned_other_ship, 587, 90_000_099, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(remote_own_ship, mk_item(remote_own_ship, 587, char_id, 60_003_761, InventoryFlag::Hangar, 1, true));
        snap.items.insert(remote_mod, mk_item(remote_mod, 997_003, char_id, remote_own_ship, InventoryFlag::Cargo, 1, false));
    });
    assert_eq!(validate_single_item_can_be_opened(&ctx, char_id, non_cont_id).await.unwrap_err().code, "CantOpenPackagedItem");
    let non_cont_assembled = 895_037_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(non_cont_assembled, mk_item(non_cont_assembled, 997_003, char_id, station_id, InventoryFlag::Hangar, 1, true));
    });
    assert_eq!(validate_single_item_can_be_opened(&ctx, char_id, non_cont_assembled).await.unwrap_err().code, "NotAContainer");
    assert_eq!(validate_container_and_bay_move(&ctx, char_id, fat_item_id, pw_cont_id, 4, &pw_kw, None).unwrap_err().code, "CapacityExceeded");

    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "ValidateItemListCanBeOpened", &[], &kw)
            .await.unwrap().unwrap_err().code,
        "InvalidArgument"
    );
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "ValidateItemListCanBeOpened", &[PyRep::Integer(non_cont_assembled)], &kw)
            .await.unwrap().unwrap_err().code,
        "NotAContainer"
    );
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "AssembleCargoContainer", &[PyRep::Integer(9_999_999)], &kw)
            .await.unwrap().unwrap_err().code,
        "ItemNotFound"
    );
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "AssembleCargoContainer", &[PyRep::Integer(other_depot_id)], &kw)
            .await.unwrap().unwrap_err().code,
        "OwnershipViolation"
    );
    let mut no_role_sess = sess.clone();
    no_role_sess.corp_role = 0;
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut no_role_sess, None, "invbroker", "AssembleCargoContainer", &[PyRep::Integer(corp_stack_cont)], &kw)
            .await.unwrap().unwrap_err().code,
        "CorpRoleRequired"
    );
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "AssembleCargoContainer", &[PyRep::Integer(remote_corp_cont)], &kw)
            .await.unwrap().unwrap_err().code,
        "ColocationRequired"
    );
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "AssembleCargoContainer", &[PyRep::Integer(non_cont_assembled)], &kw)
            .await.unwrap().unwrap_err().code,
        "NotAContainer"
    );
    let asm_corp = handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "AssembleCargoContainer", &[PyRep::Integer(corp_stack_cont)], &kw)
        .await.unwrap().unwrap();
    assert_eq!(asm_corp.dict_get("singleton"), Some(&PyRep::Bool(true)));

    // validate_ship_module_unfit_auth edge cases
    let bad_ship_mod = mk_item(895_040, 997_003, char_id, 9_999_999, InventoryFlag::HiSlot0, 1, true);
    assert_eq!(validate_ship_module_unfit_auth(&ctx, &sess, char_id, &bad_ship_mod, station_id).await.unwrap_err().code, "ShipNotFound");
    let foreign_ship_mod = mk_item(895_041, 997_003, char_id, owned_other_ship, InventoryFlag::HiSlot0, 1, true);
    assert_eq!(validate_ship_module_unfit_auth(&ctx, &sess, char_id, &foreign_ship_mod, station_id).await.unwrap_err().code, "ShipOwnershipViolation");
    let rem_ship_mod = mk_item(895_042, 997_003, char_id, remote_own_ship, InventoryFlag::HiSlot0, 1, true);
    assert_eq!(validate_ship_module_unfit_auth(&ctx, &sess, char_id, &rem_ship_mod, station_id).await.unwrap_err().code, "InvalidFittingTarget");
    let ok_ship_mod = mk_item(895_043, 997_003, char_id, ship_id, InventoryFlag::HiSlot0, 1, true);
    assert_eq!(validate_ship_module_unfit_auth(&ctx, &sess, char_id, &ok_ship_mod, 60_003_761).await.unwrap_err().code, "InvalidItemLocation");
    let mut space_sess = sess.clone();
    space_sess.station_id = None;
    assert_eq!(validate_ship_module_unfit_auth(&ctx, &space_sess, char_id, &ok_ship_mod, station_id).await.unwrap_err().code, "InvalidFittingTarget");

    // 5. PI transfer validation & lookup error branches
    let planet_id = 40_000_002_i64;
    let pin_id = 7_100_001_i64;
    let other_sys_orb = 1_000_000_710_002_i64;
    let pi_remote_item = 895_010_i64;
    let pi_small_item = 895_011_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.planetary_colonies.insert((char_id, planet_id), eve_db::PlanetaryColonyRecord {
            planet_id,
            owner_id: char_id,
            solar_system_id: 30_000_144, // different system
            command_center_level: 3,
            pins_count: 1,
            extracted_commodities: HashMap::new(),
            last_extracted_unix: 0,
        });
        let mut net = PiColonyNetworkState::default();
        net.pins.push(PiPinState {
            pin_id, type_id: 2256, latitude: 0.0, longitude: 0.0,
            contents: HashMap::from([(997_005, 5)]),
            ..Default::default()
        });
        save_pi_network(snap, char_id, planet_id, &net);
        snap.structures.insert(other_sys_orb, eve_db::UpwellStructureRecord {
            structure_id: other_sys_orb,
            type_id: 2233,
            owner_corp_id: corp_id,
            solar_system_id: 30_000_144,
            name: "OrbitalOther".into(),
            ..Default::default()
        });
        snap.items.insert(pi_remote_item, mk_item(pi_remote_item, 997_005, char_id, station_id, InventoryFlag::Hangar, 5, false));
        snap.items.insert(pi_small_item, mk_item(pi_small_item, 997_005, char_id, ship_id, InventoryFlag::Cargo, 2, false));
    });
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "ImportExportWithPlanet", &[PyRep::Integer(other_sys_orb), PyRep::Integer(pin_id), PyRep::String("bad".into()), PyRep::None], &kw)
            .await.unwrap().unwrap_err().code,
        "InvalidTransferArguments"
    );
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "ImportExportWithPlanet", &[PyRep::Integer(other_sys_orb), PyRep::Integer(pin_id), PyRep::keyval([("bad", PyRep::Integer(1))]), PyRep::None], &kw)
            .await.unwrap().unwrap_err().code,
        "InvalidCommodity"
    );
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "ImportExportWithPlanet", &[PyRep::Integer(other_sys_orb), PyRep::Integer(pin_id), PyRep::keyval([("997005", PyRep::Integer(0))]), PyRep::None], &kw)
            .await.unwrap().unwrap_err().code,
        "InvalidCommodityQuantity"
    );
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "ImportExportWithPlanet", &[PyRep::Integer(other_sys_orb), PyRep::Integer(999_999), PyRep::Dict(vec![(PyRep::Integer(997_005), PyRep::Integer(1))]), PyRep::None], &kw)
            .await.unwrap().unwrap_err().code,
        "ColonyNotFound"
    );
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "ImportExportWithPlanet", &[PyRep::Integer(0), PyRep::Integer(999_999), PyRep::Dict(vec![(PyRep::Integer(997_005), PyRep::Integer(1))]), PyRep::None], &kw)
            .await.unwrap().unwrap_err().code,
        "ColonyNotFound"
    );
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "ImportExportWithPlanet", &[PyRep::Integer(other_sys_orb), PyRep::Integer(pin_id), PyRep::Dict(vec![(PyRep::Integer(997_005), PyRep::Integer(1))]), PyRep::None], &kw)
            .await.unwrap().unwrap_err().code,
        "ColonyNotInCurrentSystem"
    );
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = 30_000_144;
            ch.station_id = None;
        }
    });
    sess.station_id = None;
    sess.solar_system_id = Some(30_000_144);
    ctx.sync_session(&mut sess);
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "ImportExportWithPlanet", &[PyRep::Integer(other_sys_orb), PyRep::Integer(pin_id), PyRep::Dict(vec![(PyRep::Integer(pi_remote_item), PyRep::Integer(1))]), PyRep::None], &kw)
            .await.unwrap().unwrap_err().code,
        "ColocationRequired"
    );
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "ImportExportWithPlanet", &[PyRep::Integer(other_sys_orb), PyRep::Integer(pin_id), PyRep::Dict(vec![(PyRep::Integer(pi_small_item), PyRep::Integer(10))]), PyRep::None], &kw)
            .await.unwrap().unwrap_err().code,
        "InsufficientCommodityQuantity"
    );
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "ImportExportWithPlanet", &[PyRep::Integer(other_sys_orb), PyRep::Integer(pin_id), PyRep::Dict(vec![(PyRep::Integer(997_005), PyRep::Integer(10))]), PyRep::None], &kw)
            .await.unwrap().unwrap_err().code,
        "InsufficientCommodityQuantity"
    );
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "ImportExportWithPlanet", &[PyRep::Integer(other_sys_orb), PyRep::Integer(pin_id), PyRep::None, PyRep::Dict(vec![(PyRep::Integer(999_999), PyRep::Integer(1))])], &kw)
            .await.unwrap().unwrap_err().code,
        "InvalidCommodity"
    );
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "ImportExportWithPlanet", &[PyRep::Integer(other_sys_orb), PyRep::Integer(pin_id), PyRep::None, PyRep::Dict(vec![(PyRep::Integer(997_005), PyRep::Integer(50))])], &kw)
            .await.unwrap().unwrap_err().code,
        "InsufficientCommoditiesForRouting"
    );
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "ImportExportWithPlanet", &[PyRep::Integer(pin_id), PyRep::Dict(vec![(PyRep::Integer(1_000_000_999), PyRep::Integer(1))]), PyRep::None], &kw)
            .await.unwrap().unwrap_err().code,
        "ItemNotFound"
    );
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "ImportExportWithPlanet", &[PyRep::Integer(pin_id), PyRep::Dict(vec![(PyRep::Integer(997_006), PyRep::Integer(1))]), PyRep::None], &kw)
            .await.unwrap().unwrap_err().code,
        "ItemNotFound"
    );
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "ImportExportWithPlanet", &[PyRep::Integer(other_sys_orb), PyRep::Integer(pin_id), PyRep::None, PyRep::Dict(vec![(PyRep::Integer(997_005), PyRep::Integer(i64::from(i32::MAX) + 1))])], &kw)
            .await.unwrap().unwrap_err().code,
        "InvalidCommodityQuantity"
    );
    ctx.db.with_mut_snapshot(|snap| {
        let mut net = PiColonyNetworkState::default();
        net.pins.push(PiPinState {
            pin_id, type_id: 2256, latitude: 0.0, longitude: 0.0,
            contents: HashMap::from([(997_005, 120_000)]),
            ..Default::default()
        });
        save_pi_network(snap, char_id, planet_id, &net);
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = eve_db::IskCents(0);
        }
    });
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "ImportExportWithPlanet", &[PyRep::Integer(other_sys_orb), PyRep::Integer(pin_id), PyRep::None, PyRep::Dict(vec![(PyRep::Integer(997_005), PyRep::Integer(100_000))])], &kw)
            .await.unwrap().unwrap_err().code,
        "CustomsOfficeCapacityExceeded"
    );
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "ImportExportWithPlanet", &[PyRep::Integer(other_sys_orb), PyRep::Integer(pin_id), PyRep::None, PyRep::Dict(vec![(PyRep::Integer(997_005), PyRep::Integer(5_000))])], &kw)
            .await.unwrap().unwrap_err().code,
        "InsufficientFunds"
    );

    let bound_pw = BoundObjectEntry {
        bound_id: 2,
        session_id: sess.session_id,
        user_id: 1,
        character_id: char_id,
        service: "invbroker".into(),
        object_id: pw_cont_id,
        oid_str: "N=1:2".into(),
        ref_id: 2,
    };
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut sess, Some(&bound_pw), "invbroker", "RetrievePassword", &[PyRep::Integer(1)], &kw)
            .await.unwrap().unwrap(),
        PyRep::String("secret".into())
    );
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "RetrievePassword", &[PyRep::Integer(0), PyRep::Integer(1)], &kw)
            .await.unwrap().unwrap_err().code,
        "InvalidArguments"
    );
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "RetrievePassword", &[PyRep::Integer(9_999_999), PyRep::Integer(1)], &kw)
            .await.unwrap().unwrap_err().code,
        "ItemNotFound"
    );
    assert_eq!(
        handle_containers_ships_and_fitting(&ctx, &mut sess, None, "invbroker", "RetrievePassword", &[PyRep::Integer(other_depot_id), PyRep::Integer(1)], &kw)
            .await.unwrap().unwrap_err().code,
        "ItemNotFound"
    );

    ctx.db.with_snapshot(|snap| {
        assert_eq!(check_item_lock_impound_asset_safety_in_snap(snap, &mk_item(1, 997_003, char_id, station_id, InventoryFlag::Impounded, 1, false)).unwrap_err().code, "ImpoundedItem");
        assert_eq!(check_item_lock_impound_asset_safety_in_snap(snap, &mk_item(1, 60, char_id, station_id, InventoryFlag::Hangar, 1, false)).unwrap_err().code, "AssetSafetyLocked");
    });
}
