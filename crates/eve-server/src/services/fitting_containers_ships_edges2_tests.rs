//! Additional edge-case and branch coverage tests for `services::fitting_containers_ships`.

use super::*;
use eve_db::{CorporationRecord, InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| {
        if std::env::var_os("EVOMU_SDE_PATH").is_some() {
            Arc::new(ServerContext::new().sde.as_ref().clone())
        } else {
            Arc::new(SdeRegistry::with_canonical_seed())
        }
    }))
}

fn make_ctx() -> ServerContext {
    ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    )
    .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
}

fn make_session(char_id: i64, corp_id: i64, station_id: Option<i64>, sys_id: u32, ship_id: i64) -> SessionState {
    let mut s = SessionState::new(9820);
    s.user_id = Some(1);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.station_id = station_id;
    s.solar_system_id = Some(sys_id);
    s.ship_id = Some(ship_id);
    s.corp_role = u64::MAX;
    s
}

#[test]
fn container_runtime_state_volume_bay_access_logs_and_item_locks() {
    let ctx = make_ctx();
    let char_id = 96_600_001_i64;
    let other_char = 96_600_002_i64;
    let corp_id = 98_910_i64;
    let station_id = 60_003_760_i64;

    // 1. ContainerShipRuntimeState password set/get/clear & persistence
    let mut st = ContainerShipRuntimeState::default();
    st.set_container_password(100, 1, "sec1".into());
    st.set_container_password(100, 2, "sec2".into());
    assert_eq!(st.get_container_password(100, 1), Some("sec1"));
    assert_eq!(st.get_container_password(100, 2), Some("sec2"));
    st.set_container_password(100, 1, "".into());
    assert_eq!(st.get_container_password(100, 1), None);
    save_container_ship_state(&ctx, &st);
    let loaded = read_container_ship_state(&ctx);
    assert_eq!(loaded.get_container_password(100, 2), Some("sec2"));

    // 2. is_container_group & effective_item_unit_volume
    assert!(is_container_group(448, 34)); // Audit Log Secure Container group
    assert!(is_container_group(999_999, 23)); // type_id 23 fallback

    let unk_vol = effective_item_unit_volume(
        &ctx.sde,
        &InventoryItemRecord {
            type_id: 999_999_999,
            singleton: false,
            ..Default::default()
        },
    );
    assert!(unk_vol >= 0.0);

    let assembled_rifter = effective_item_unit_volume(
        &ctx.sde,
        &InventoryItemRecord {
            type_id: 587,
            singleton: true,
            ..Default::default()
        },
    );
    let packaged_rifter = effective_item_unit_volume(
        &ctx.sde,
        &InventoryItemRecord {
            type_id: 587,
            singleton: false,
            ..Default::default()
        },
    );
    assert!(packaged_rifter <= assembled_rifter);

    // 3. Ship bay access (owner, corp, password-locked), corp container log targets, lock/impound/asset safety chain
    let ship_id = 96_600_100_i64;
    let corp_cont = 96_600_101_i64;
    let office_id = 96_600_102_i64;
    let alsc_id = 96_600_103_i64;
    let locked_parent = 96_600_104_i64;
    let child_in_locked = 96_600_105_i64;
    let impounded = 96_600_106_i64;
    let asset_safety = 96_600_107_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default(); __corp.corporation_id = corp_id; __corp.name = "ContCorp".into(); __corp.ticker = "CNT".into(); __corp.ceo_id = char_id; __corp.members = HashSet::from([char_id, other_char]); __corp.ensure_wallet_divisions(); __corp },
        );
        let mut ch = snap.characters.values().next().cloned().expect("char");
        ch.character_id = char_id;
        ch.corporation_id = corp_id;
        snap.characters.insert(char_id, ch.clone());
        ch.character_id = other_char;
        ch.corporation_id = corp_id;
        snap.characters.insert(other_char, ch);

        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 28606, // Orca
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            office_id,
            InventoryItemRecord {
                item_id: office_id,
                type_id: 27, // Office folder
                owner_id: corp_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            corp_cont,
            InventoryItemRecord {
                item_id: corp_cont,
                type_id: 17366, // Station Container
                owner_id: corp_id,
                location_id: office_id,
                flag: InventoryFlag::CorpSAG2,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            alsc_id,
            InventoryItemRecord {
                item_id: alsc_id,
                type_id: 17366,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            locked_parent,
            InventoryItemRecord {
                item_id: locked_parent,
                type_id: 23,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Locked,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            child_in_locked,
            InventoryItemRecord {
                item_id: child_in_locked,
                type_id: 34,
                owner_id: char_id,
                location_id: locked_parent,
                flag: InventoryFlag::Hangar,
                quantity: 10,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            impounded,
            InventoryItemRecord {
                item_id: impounded,
                type_id: 34,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Impounded,
                quantity: 10,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            asset_safety,
            InventoryItemRecord {
                item_id: asset_safety,
                type_id: 60, // AssetSafetyWrap
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 10,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );

        let mut rt = ContainerShipRuntimeState::default();
        rt.ship_configs.insert(
            ship_id,
            ShipAccessConfig {
                allow_corp_smb: true,
                allow_corp_hangar: true,
                password: Some("fleetpw".into()),
                ..Default::default()
            },
        );
        rt.container_configs.insert(
            alsc_id,
            ContainerConfig {
                lock_by_default: true,
                ..Default::default()
            },
        );
        save_container_ship_state_snap(snap, 1000, &rt);

        // Corp member with wrong password vs right password
        assert!(!can_character_access_ship_bay_with_password_snap(
            snap, other_char, ship_id, char_id, false, Some("wrong"),
        ));
        assert!(can_character_access_ship_bay_with_password_snap(
            snap, other_char, ship_id, char_id, false, Some("fleetpw"),
        ));

        // Corp container log target
        assert!(resolve_corp_container_log_target(snap, corp_cont).is_some());
        assert!(resolve_corp_container_log_target(snap, alsc_id).is_none());

        // Lock/impound/asset safety ancestor chain
        assert_eq!(
            check_item_lock_impound_asset_safety_in_snap(snap, &snap.items[&child_in_locked]).unwrap_err().code,
            "ItemLocked"
        );
        assert_eq!(
            check_item_lock_impound_asset_safety_in_snap(snap, &snap.items[&impounded]).unwrap_err().code,
            "ImpoundedItem"
        );
        assert_eq!(
            check_item_lock_impound_asset_safety_in_snap(snap, &snap.items[&asset_safety]).unwrap_err().code,
            "AssetSafetyLocked"
        );
    });

    // ALSC & corp logs + active audit log <21d vs >=21d
    record_container_add_log(&ctx, char_id, 96_600_901, 34, 100, alsc_id);
    record_container_take_log(&ctx, char_id, 96_600_901, 34, 25, alsc_id);
    record_container_rename_log(&ctx, char_id, alsc_id, 17366);
    let now = ctx.now_unix() as u64;
    ctx.db.with_snapshot(|snap| {
        assert!(container_has_active_audit_log_snap(snap, alsc_id, now));
        assert!(!container_has_active_audit_log_snap(snap, alsc_id, now + 22 * 86_400));
    });
}

#[test]
fn validate_container_and_bay_move_rejects_invalid_moves_and_enforces_bays() {
    let ctx = make_ctx();
    let char_id = 96_600_003_i64;
    let other_char = 96_600_004_i64;
    let station_id = 60_003_760_i64;
    let cont_a = 96_600_200_i64;
    let cont_b = 96_600_201_i64;
    let unass_cont = 96_600_202_i64;
    let locked_item = 96_600_203_i64;
    let rifter_normal = 96_600_204_i64;
    let rifter_packaged = 96_600_205_i64;
    let rifter_cargo = 96_600_206_i64;
    let orca_ship = 96_600_207_i64;
    let big_trit = 96_600_208_i64;
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("char");
        ch.character_id = char_id;
        snap.characters.insert(char_id, ch);

        for (id, tid, sing, loc, flag, qty) in [
            (cont_a, 23, true, station_id, InventoryFlag::Hangar, 1),
            (cont_b, 23, true, station_id, InventoryFlag::Hangar, 1),
            (unass_cont, 23, false, station_id, InventoryFlag::Hangar, 1),
            (locked_item, 34, false, cont_a, InventoryFlag::Hangar, 10),
            (rifter_normal, 587, true, station_id, InventoryFlag::Hangar, 1),
            (rifter_packaged, 587, false, station_id, InventoryFlag::Hangar, 1),
            (rifter_cargo, 34, false, rifter_normal, InventoryFlag::Cargo, 5),
            (orca_ship, 28606, true, station_id, InventoryFlag::Hangar, 1),
            (big_trit, 34, false, station_id, InventoryFlag::Hangar, 50_000_000),
        ] {
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: loc,
                    flag,
                    quantity: qty,
                    singleton: sing,
                    version: 1,
                    ..Default::default()
                },
            );
        }

        let mut rt = ContainerShipRuntimeState::default();
        rt.alsc_locked_items.insert(locked_item);
        rt.set_container_password(cont_b, 1, "pw123".into());
        save_container_ship_state_snap(snap, 1000, &rt);
    });

    // Self store
    assert_eq!(
        validate_container_and_bay_move(&ctx, char_id, cont_a, cont_a, 4, &kw, None)
            .unwrap_err()
            .code,
        "UserError"
    );
    // Impound flag rejected
    assert_eq!(
        validate_container_and_bay_move(&ctx, char_id, cont_a, station_id, 72, &kw, None)
            .unwrap_err()
            .code,
        "ImpoundedItem"
    );
    // ALSC locked item cannot move
    assert_eq!(
        validate_container_and_bay_move(&ctx, char_id, locked_item, station_id, 4, &kw, None)
            .unwrap_err()
            .code,
        "ALSCItemLocked"
    );
    // Container-in-container rejected
    assert_eq!(
        validate_container_and_bay_move(&ctx, char_id, cont_a, cont_b, 4, &kw, None)
            .unwrap_err()
            .code,
        "CannotPutContainerInContainer"
    );
    // Unassembled container cannot hold items
    assert_eq!(
        validate_container_and_bay_move(&ctx, char_id, big_trit, unass_cont, 4, &kw, Some(1))
            .unwrap_err()
            .code,
        "ContainerNotAssembled"
    );
    // Password-protected container rejects missing/wrong password, accepts matching password
    assert_eq!(
        validate_container_and_bay_move(&ctx, char_id, big_trit, cont_b, 4, &kw, Some(1))
            .unwrap_err()
            .code,
        "ALSCPasswordNeededToOpen"
    );
    let mut pw_kw = HashMap::new();
    pw_kw.insert("password".into(), PyRep::String("pw123".into()));
    assert!(validate_container_and_bay_move(&ctx, char_id, big_trit, cont_b, 4, &pw_kw, Some(1)).is_ok());

    // SMA checks: Rifter has NoShipMaintenanceBay; Orca rejects non-ship cargo, packaged ship, ship with cargo, and foreign access
    assert_eq!(
        validate_container_and_bay_move(&ctx, char_id, rifter_packaged, rifter_normal, 90, &kw, None)
            .unwrap_err()
            .code,
        "NoShipMaintenanceBay"
    );
    assert_eq!(
        validate_container_and_bay_move(&ctx, char_id, big_trit, orca_ship, 90, &kw, Some(1))
            .unwrap_err()
            .code,
        "CannotStoreCargoInSMA"
    );
    assert_eq!(
        validate_container_and_bay_move(&ctx, char_id, rifter_packaged, orca_ship, 90, &kw, None)
            .unwrap_err()
            .code,
        "OnlyAssembledShipsInSMA"
    );
    assert_eq!(
        validate_container_and_bay_move(&ctx, char_id, rifter_normal, orca_ship, 90, &kw, None)
            .unwrap_err()
            .code,
        "ShipHasNonChargeCargo"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&rifter_cargo);
    });
    assert_eq!(
        validate_container_and_bay_move(&ctx, other_char, rifter_normal, orca_ship, 90, &kw, None)
            .unwrap_err()
            .code,
        "SMAAccessDenied"
    );

    // Fleet hangar checks: Rifter has NoFleetHangar; foreign char -> FleetHangarAccessDenied
    assert_eq!(
        validate_container_and_bay_move(&ctx, char_id, big_trit, rifter_normal, 155, &kw, Some(1))
            .unwrap_err()
            .code,
        "NoFleetHangar"
    );
    assert_eq!(
        validate_container_and_bay_move(&ctx, other_char, big_trit, orca_ship, 155, &kw, Some(1))
            .unwrap_err()
            .code,
        "FleetHangarAccessDenied"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn container_rpcs_validate_open_alsc_crystals_plastic_wrap_and_unfit_auth() {
    let ctx = make_ctx();
    let char_id = 96_600_005_i64;
    let corp_id = 98_911_i64;
    let station_id = 60_003_760_i64;
    let sys_id = 30_000_142_u32;
    let ship_id = 96_600_300_i64;
    let stack_cont_id = 96_600_301_i64;
    let alsc_id = 96_600_302_i64;
    let trit_inside = 96_600_303_i64;
    let crystal_id = 96_600_304_i64;
    let wrap_id = 96_600_305_i64;
    let wrap_child = 96_600_306_i64;
    let mod_id = 96_600_307_i64;
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("char");
        ch.character_id = char_id;
        ch.corporation_id = corp_id;
        ch.station_id = Some(station_id);
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);

        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            stack_cont_id,
            InventoryItemRecord {
                item_id: stack_cont_id,
                type_id: 23,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 3,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            alsc_id,
            InventoryItemRecord {
                item_id: alsc_id,
                type_id: 17366,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            trit_inside,
            InventoryItemRecord {
                item_id: trit_inside,
                type_id: 34,
                owner_id: char_id,
                location_id: alsc_id,
                flag: InventoryFlag::Hangar,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            crystal_id,
            InventoryItemRecord {
                item_id: crystal_id,
                type_id: 12814, // Frequency crystal or generic item with DAMAGE
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                dynamic_attributes: HashMap::from([(attr_ids::DAMAGE, 0.25)]),
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            wrap_id,
            InventoryItemRecord {
                item_id: wrap_id,
                type_id: 3468, // Plastic Wrap
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            wrap_child,
            InventoryItemRecord {
                item_id: wrap_child,
                type_id: 34,
                owner_id: char_id,
                location_id: wrap_id,
                flag: InventoryFlag::Hangar,
                quantity: 50,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            mod_id,
            InventoryItemRecord {
                item_id: mod_id,
                type_id: 485,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot1,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mut sess = make_session(char_id, corp_id, Some(station_id), sys_id, ship_id);

    // ValidateItemCanBeOpened / ValidateItemListCanBeOpened: unpackaged, not a container, valid
    assert_eq!(
        handle_containers_ships_and_fitting(
            &ctx,
            &mut sess,
            None,
            "invbroker",
            "ValidateItemCanBeOpened",
            &[PyRep::Integer(stack_cont_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CantOpenPackagedItem"
    );
    let non_cont_singleton = 96_290_999_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            non_cont_singleton,
            InventoryItemRecord {
                item_id: non_cont_singleton,
                type_id: 34,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        handle_containers_ships_and_fitting(
            &ctx,
            &mut sess,
            None,
            "invbroker",
            "ValidateItemListCanBeOpened",
            &[PyRep::List(vec![PyRep::Integer(non_cont_singleton)])],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "NotAContainer"
    );
    assert!(
        handle_containers_ships_and_fitting(
            &ctx,
            &mut sess,
            None,
            "invbroker",
            "ValidateItemCanBeOpened",
            &[PyRep::Integer(alsc_id)],
            &kw,
        )
        .await
        .unwrap()
        .is_ok()
    );

    // AssembleCargoContainer splits stacked container into singleton
    let assembled = handle_containers_ships_and_fitting(
        &ctx,
        &mut sess,
        None,
        "invbroker",
        "AssembleCargoContainer",
        &[PyRep::Integer(stack_cont_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_ne!(assembled, PyRep::None);

    // ALSC SetPassword / ConfigGet / Lock / Unlock / LogGet
    let _ = handle_containers_ships_and_fitting(
        &ctx,
        &mut sess,
        None,
        "invbroker",
        "SetPassword",
        &[PyRep::Integer(alsc_id), PyRep::Integer(1), PyRep::String("open".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let cfg = handle_containers_ships_and_fitting(
        &ctx,
        &mut sess,
        None,
        "invbroker",
        "ALSCConfigGet",
        &[PyRep::Integer(alsc_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_ne!(cfg, PyRep::None);
    let _ = handle_containers_ships_and_fitting(
        &ctx,
        &mut sess,
        None,
        "invbroker",
        "ALSCLock",
        &[PyRep::Integer(alsc_id), PyRep::List(vec![PyRep::Integer(trit_inside)]), PyRep::String("open".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let _ = handle_containers_ships_and_fitting(
        &ctx,
        &mut sess,
        None,
        "invbroker",
        "ALSCUnlock",
        &[PyRep::Integer(alsc_id), PyRep::List(vec![PyRep::Integer(trit_inside)]), PyRep::String("open".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();

    // GetDamageForCrystals, GetAllItems, IsMyItemAtDockableLocation
    let dmg = handle_containers_ships_and_fitting(
        &ctx,
        &mut sess,
        None,
        "invbroker",
        "GetDamageForCrystals",
        &[PyRep::List(vec![PyRep::Integer(crystal_id)])],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(dmg, PyRep::Dict(_)));

    let all_items = handle_containers_ships_and_fitting(
        &ctx,
        &mut sess,
        None,
        "invbroker",
        "GetAllItems",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_ne!(all_items, PyRep::None);

    let at_dock = handle_containers_ships_and_fitting(
        &ctx,
        &mut sess,
        None,
        "invbroker",
        "IsMyItemAtDockableLocation",
        &[PyRep::Integer(alsc_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(at_dock, PyRep::Bool(true));

    // BreakPlasticWrap unwraps child and removes wrap
    let _ = handle_containers_ships_and_fitting(
        &ctx,
        &mut sess,
        None,
        "invbroker",
        "BreakPlasticWrap",
        &[PyRep::Integer(wrap_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&wrap_id));
        assert_eq!(snap.items[&wrap_child].location_id, station_id);
    });

    // validate_ship_module_unfit_auth in space with weapons timer vs no mobile depot
    let src_mod = ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().location_id = sys_id as i64;
        snap.items[&mod_id].clone()
    });
    let mut space_sess = make_session(char_id, corp_id, None, sys_id, ship_id);
    {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        cw.states.entry(char_id).or_default().weapon_timer_until_unix = ctx.now_unix_secs() + 120;
    }
    assert_eq!(
        validate_ship_module_unfit_auth(&ctx, &space_sess, char_id, &src_mod, ship_id)
            .await
            .unwrap_err()
            .code,
        "CannotRefitWithWeaponsTimer"
    );
    {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        cw.states.remove(&char_id);
    }
    assert_eq!(
        validate_ship_module_unfit_auth(&ctx, &space_sess, char_id, &src_mod, ship_id)
            .await
            .unwrap_err()
            .code,
        "CannotUnfitInSpace"
    );
    let _ = &mut space_sess;
}
