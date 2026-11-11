//! Deep branch-coverage and mutation-killing unit tests for `fitting_containers_ships`,
//! `inventory_fitting`, and `fitting_ships_mastery`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::inventory_fitting;
use crate::services::inventory_fitting::fitting_containers_ships::{
    can_character_access_ship_bay_with_password_snap, effective_item_unit_volume,
    handle_containers_ships_and_fitting, is_container_group, validate_container_and_bay_move,
    write_container_ship_state,
};
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
const SYNTH_DEPOT_TID: u32 = 990_201;
const SYNTH_CHARGE_TID: u32 = 990_202;
const SYNTH_SECURE_CONT_TID: u32 = 990_203;
const SYNTH_STATION_CONT_TID: u32 = 990_204;
const SYNTH_SIPHON_TID: u32 = 990_205;

fn test_ctx() -> ServerContext {
    let ctx = if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let mut sde = SdeRegistry::with_canonical_seed();
        if let Some(base) = sde.types.get(&587).cloned() {
            let mut depot = base.clone();
            depot.type_id = SYNTH_DEPOT_TID;
            depot.group_id = 1246;
            depot.category_id = 23;
            depot.name = "Synthetic Mobile Depot".into();
            depot.base_price = 1_500_000.0;
            sde.types.insert(SYNTH_DEPOT_TID, depot);

            let mut charge = base.clone();
            charge.type_id = SYNTH_CHARGE_TID;
            charge.group_id = 83;
            charge.category_id = 8;
            charge.name = "Synthetic Laser Crystal".into();
            charge.base_price = 25_000.0;
            sde.types.insert(SYNTH_CHARGE_TID, charge);

            let mut sec_c = base.clone();
            sec_c.type_id = SYNTH_SECURE_CONT_TID;
            sec_c.group_id = 340;
            sec_c.category_id = 23;
            sec_c.volume = 300.0;
            sec_c.capacity = 3_900.0;
            sec_c.name = "Synthetic Secure Container".into();
            sde.types.insert(SYNTH_SECURE_CONT_TID, sec_c);

            let mut st_c = base.clone();
            st_c.type_id = SYNTH_STATION_CONT_TID;
            st_c.group_id = 448;
            st_c.category_id = 23;
            st_c.volume = 500_000.0;
            st_c.capacity = 1_000_000.0;
            st_c.name = "Synthetic Station Container".into();
            sde.types.insert(SYNTH_STATION_CONT_TID, st_c);

            let mut siphon = base.clone();
            siphon.type_id = SYNTH_SIPHON_TID;
            siphon.group_id = 1247;
            siphon.category_id = 23;
            siphon.name = "Synthetic Mobile Siphon".into();
            sde.types.insert(SYNTH_SIPHON_TID, siphon);

            let loki = sde.types.entry(29990).or_insert_with(|| base.clone());
            loki.type_id = 29990;
            loki.group_id = 963;
            loki.category_id = 6;
            loki.volume = 140_000.0;
            loki.default_attributes.insert(14, 0.0);

            let sub = sde.types.entry(29970).or_insert_with(|| base.clone());
            sub.type_id = 29970;
            sub.group_id = 958;
            sub.category_id = 32;
            sub.default_attributes.insert(14, 5.0);
            sde.types.entry(492).or_insert_with(|| {
                let mut gun = base.clone();
                gun.type_id = 492;
                gun.group_id = 53;
                gun.category_id = 7;
                gun
            });
            sde.types.entry(2605).or_insert_with(|| {
                let mut rig = base;
                rig.type_id = 2605;
                rig.group_id = 773;
                rig.category_id = 7;
                rig
            });
        }
        let arc_sde = Arc::new(sde);
        let _ = SHARED_SDE.set(Arc::clone(&arc_sde));
        ServerContext::with_sde_and_repository(
            arc_sde,
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    };
    ctx.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(1_770_000_000)));
    ctx
}

fn make_docked_session(char_id: i64, station_id: i64, sys_id: u32, ship_id: i64) -> SessionState {
    let mut s = SessionState::new(9701);
    s.user_id = Some(1);
    s.character_id = Some(char_id);
    s.corporation_id = Some(1000009);
    s.station_id = Some(station_id);
    s.solar_system_id = Some(sys_id);
    s.ship_id = Some(ship_id);
    s
}

fn make_space_session(char_id: i64, sys_id: u32, ship_id: i64) -> SessionState {
    let mut s = SessionState::new(9702);
    s.user_id = Some(1);
    s.character_id = Some(char_id);
    s.corporation_id = Some(1000009);
    s.station_id = None;
    s.structure_id = None;
    s.solar_system_id = Some(sys_id);
    s.ship_id = Some(ship_id);
    s
}

fn make_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity,
        singleton,
        version: 1,
        ..Default::default()
    }
}

fn dict_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(map) => map.get(key),
        PyRep::Dict(entries) => entries.iter().find_map(|(k, v)| (k.as_str() == Some(key)).then_some(v)),
        PyRep::ObjectEx { header, .. } => {
            if let PyRep::Tuple(t) = header.as_ref() {
                if let Some(PyRep::Dict(entries)) = t.get(1) {
                    return entries.iter().find_map(|(k, v)| (k.as_str() == Some(key)).then_some(v));
                }
            }
            None
        }
        _ => None,
    }
}

#[test]
fn effective_item_unit_volume_and_container_group_cover_packaged_ship_and_container_brackets() {
    let ctx = test_ctx();

    assert!(is_container_group(12, 3293));
    assert!(is_container_group(340, 17363));
    assert!(is_container_group(448, 17366));
    assert!(is_container_group(649, 33003));
    assert!(is_container_group(0, 23));
    assert!(!is_container_group(25, 587));

    // Assembled vs packaged ship volume brackets across ship hull classes
    let cases: &[(u32, f64)] = &[
        (672, 500.0),      // Caldari Shuttle (group 31)
        (587, 2_500.0),    // Rifter (group 25, Frigate)
        (17480, 3_750.0),  // Procurer (group 463, Mining Barge)
        (16242, 5_000.0),  // Thrasher (group 420, Destroyer)
        (622, 10_000.0),   // Stabber (group 26, Cruiser)
        (24702, 15_000.0), // Hurricane (group 419, Battlecruiser)
        (648, 20_000.0),   // Badger (group 28, Industrial)
        (639, 50_000.0),   // Tempest (group 27, Battleship)
    ];
    for &(type_id, expected_pkg_vol) in cases {
        if let Some(t) = ctx.sde.types.get(&type_id) {
            let packaged = make_item(1, type_id, 9001, 60003760, InventoryFlag::Hangar, 1, false);
            let assembled = make_item(2, type_id, 9001, 60003760, InventoryFlag::Hangar, 1, true);
            assert!(
                (effective_item_unit_volume(&ctx.sde, &packaged) - expected_pkg_vol).abs() < 1e-6,
                "type {type_id} packaged volume mismatch"
            );
            assert!(
                (effective_item_unit_volume(&ctx.sde, &assembled) - t.volume).abs() < 1e-6,
                "type {type_id} assembled volume mismatch"
            );
        }
    }

    // Container packaged volume is (t.volume * 0.1).min(5_000.0)
    if let Some(t) = ctx.sde.types.get(&17366) {
        let pkg_cont = make_item(10, 17366, 9001, 60003760, InventoryFlag::Hangar, 1, false);
        let asm_cont = make_item(11, 17366, 9001, 60003760, InventoryFlag::Hangar, 1, true);
        let expected_pkg = (t.volume * 0.1).min(5_000.0);
        assert!((effective_item_unit_volume(&ctx.sde, &pkg_cont) - expected_pkg).abs() < 1e-6);
        assert!((effective_item_unit_volume(&ctx.sde, &asm_cont) - t.volume).abs() < 1e-6);
    }
}

#[test]
fn validate_container_and_bay_move_enforces_locks_passwords_nested_containers_and_special_structures() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let other_char = 90_000_002_i64;
    let station_id = 60_003_760_i64;

    let (rifter_id, freighter_id, alsc_id, station_cont_id, freight_cont_id, pkg_cont_id) = (51001, 51002, 51003, 51004, 51005, 51006);
    let (mobile_siphon_id, foreign_depot_id, trit_in_alsc_id, trit_in_hangar_id) = (51007, 51008, 51009, 51010);
    let freighter_tid = ctx.sde.types.values().find(|t| t.group_id == 513).unwrap().type_id;

    ctx.db.with_mut_snapshot(|snap| {
        for (id, tid, own, loc, flg, qty, sng) in [
            (rifter_id, 587, char_id, station_id, InventoryFlag::Hangar, 1, true),
            (freighter_id, freighter_tid, char_id, station_id, InventoryFlag::Hangar, 1, true),
            (alsc_id, SYNTH_SECURE_CONT_TID, char_id, station_id, InventoryFlag::Hangar, 1, true),
            (station_cont_id, SYNTH_STATION_CONT_TID, char_id, station_id, InventoryFlag::Hangar, 1, true),
            (freight_cont_id, 33003, char_id, station_id, InventoryFlag::Hangar, 1, true),
            (pkg_cont_id, SYNTH_SECURE_CONT_TID, char_id, station_id, InventoryFlag::Hangar, 1, false),
            (mobile_siphon_id, SYNTH_SIPHON_TID, char_id, 30000142, InventoryFlag::None, 1, true),
            (foreign_depot_id, 33474, other_char, 30000142, InventoryFlag::None, 1, true),
            (trit_in_alsc_id, 34, char_id, alsc_id, InventoryFlag::Cargo, 100, false),
            (trit_in_hangar_id, 34, char_id, station_id, InventoryFlag::Hangar, 100, false),
        ] {
            snap.items.insert(id, make_item(id, tid, own, loc, flg, qty, sng));
        }
    });

    let kw = HashMap::new();
    let cargo_flag = InventoryFlag::Cargo as u16;
    let hangar_flag = InventoryFlag::Hangar as u16;

    // 1. Moving item into itself
    let self_err = validate_container_and_bay_move(&ctx, char_id, rifter_id, rifter_id, cargo_flag, &kw, None).unwrap_err();
    assert_eq!(self_err.code, "UserError");

    // 2. ALSC locked item cannot be moved out
    write_container_ship_state(&ctx, |st| {
        st.alsc_locked_items.insert(trit_in_alsc_id);
        st.container_passwords.insert(alsc_id, "secret123".into());
    });
    let locked_err = validate_container_and_bay_move(&ctx, char_id, trit_in_alsc_id, station_id, hangar_flag, &kw, None).unwrap_err();
    assert_eq!(locked_err.code, "ALSCItemLocked");

    // 3. Unlock item, now password check triggers when moving out without password
    write_container_ship_state(&ctx, |st| {
        st.alsc_locked_items.remove(&trit_in_alsc_id);
    });
    let pw_err = validate_container_and_bay_move(&ctx, char_id, trit_in_alsc_id, station_id, hangar_flag, &kw, None).unwrap_err();
    assert_eq!(pw_err.code, "ALSCPasswordNeededToOpen");

    // Supplying password via kwargs unlocks and permits move
    let mut pw_kwargs = HashMap::new();
    pw_kwargs.insert("password".to_string(), PyRep::String("secret123".into()));
    validate_container_and_bay_move(&ctx, char_id, trit_in_alsc_id, station_id, hangar_flag, &pw_kwargs, None).unwrap();

    // 4. Assembled Station Container cannot be loaded into a ship
    let st_cont_err = validate_container_and_bay_move(&ctx, char_id, station_cont_id, freighter_id, cargo_flag, &kw, None).unwrap_err();
    assert_eq!(st_cont_err.code, "CapacityExceeded");

    // 5. Assembled non-freight container cannot be loaded into Freighter, but Freight Container can
    let non_freight_err = validate_container_and_bay_move(&ctx, char_id, alsc_id, freighter_id, cargo_flag, &kw, None).unwrap_err();
    assert_eq!(non_freight_err.code, "OnlyFreightContainersAllowedInFreighters");
    validate_container_and_bay_move(&ctx, char_id, freight_cont_id, freighter_id, cargo_flag, &kw, None).unwrap();

    // 6. Packaged container cannot hold items, and container-in-container is rejected
    let pkg_err = validate_container_and_bay_move(&ctx, char_id, trit_in_hangar_id, pkg_cont_id, cargo_flag, &kw, None).unwrap_err();
    assert_eq!(pkg_err.code, "ContainerNotAssembled");
    let nested_err = validate_container_and_bay_move(&ctx, char_id, freight_cont_id, alsc_id, cargo_flag, &kw, None).unwrap_err();
    assert_eq!(nested_err.code, "CannotPutContainerInContainer");

    // 7. Mobile Siphon and unowned Mobile Depot reject deposits
    let siphon_err = validate_container_and_bay_move(&ctx, char_id, trit_in_hangar_id, mobile_siphon_id, cargo_flag, &kw, None).unwrap_err();
    assert_eq!(siphon_err.code, "CannotAddToSiphon");
    let depot_err = validate_container_and_bay_move(&ctx, char_id, trit_in_hangar_id, foreign_depot_id, cargo_flag, &kw, None).unwrap_err();
    assert_eq!(depot_err.code, "OwnershipViolation");
}

#[test]
fn validate_ship_maintenance_bay_and_fleet_hangar_rules_and_access_controls() {
    let ctx = test_ctx();
    let owner_id = 90_000_001_i64;
    let stranger_id = 90_000_012_i64;
    let station_id = 60_003_760_i64;

    let (archon_id, rifter_no_sma, pkg_frigate, asm_frigate, trit_id, ammo_id) =
        (52001, 52002, 52003, 52004, 52005, 52006);

    ctx.db.with_mut_snapshot(|snap| {
        for (id, tid, loc, flg, qty, sng) in [
            (archon_id, 23757, station_id, InventoryFlag::Hangar, 1, true),
            (rifter_no_sma, 587, station_id, InventoryFlag::Hangar, 1, true),
            (pkg_frigate, 587, station_id, InventoryFlag::Hangar, 1, false),
            (asm_frigate, 587, station_id, InventoryFlag::Hangar, 1, true),
            (trit_id, 34, asm_frigate, InventoryFlag::Cargo, 10, false),
            (ammo_id, 182, station_id, InventoryFlag::Hangar, 50, false),
        ] {
            snap.items.insert(id, make_item(id, tid, owner_id, loc, flg, qty, sng));
        }
    });

    let kw = HashMap::new();
    let sma_flag = InventoryFlag::ShipHangar as u16;
    let fh_flag = InventoryFlag::FleetHangar as u16;

    // Moving into SMA of a ship without SMA (Rifter)
    let no_sma_err = validate_container_and_bay_move(&ctx, owner_id, asm_frigate, rifter_no_sma, sma_flag, &kw, None).unwrap_err();
    assert_eq!(no_sma_err.code, "NoShipMaintenanceBay");

    // Moving non-ship cargo or packaged ship into Carrier SMA
    let non_ship_err = validate_container_and_bay_move(&ctx, owner_id, ammo_id, archon_id, sma_flag, &kw, None).unwrap_err();
    assert_eq!(non_ship_err.code, "CannotStoreCargoInSMA");
    let pkg_ship_err = validate_container_and_bay_move(&ctx, owner_id, pkg_frigate, archon_id, sma_flag, &kw, None).unwrap_err();
    assert_eq!(pkg_ship_err.code, "OnlyAssembledShipsInSMA");

    // Moving assembled ship that contains non-charge cargo (Tritanium) into SMA
    let dirty_cargo_err = validate_container_and_bay_move(&ctx, owner_id, asm_frigate, archon_id, sma_flag, &kw, None).unwrap_err();
    assert_eq!(dirty_cargo_err.code, "ShipHasNonChargeCargo");

    // Replace Tritanium in asm_frigate with ammo (category 8 Charge): now allowed for owner!
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&trit_id);
        if let Some(ammo) = snap.items.get_mut(&ammo_id) {
            ammo.location_id = asm_frigate;
            ammo.flag = InventoryFlag::Cargo;
        }
    });
    validate_container_and_bay_move(&ctx, owner_id, asm_frigate, archon_id, sma_flag, &kw, None).unwrap();

    // Stranger without corp/fleet access or password gets SMAAccessDenied & FleetHangarAccessDenied
    let sma_denied = validate_container_and_bay_move(&ctx, stranger_id, asm_frigate, archon_id, sma_flag, &kw, None).unwrap_err();
    assert_eq!(sma_denied.code, "SMAAccessDenied");
    let fh_no_bay = validate_container_and_bay_move(&ctx, owner_id, ammo_id, rifter_no_sma, fh_flag, &kw, None).unwrap_err();
    assert_eq!(fh_no_bay.code, "NoFleetHangar");
    let fh_denied = validate_container_and_bay_move(&ctx, stranger_id, ammo_id, archon_id, fh_flag, &kw, None).unwrap_err();
    assert_eq!(fh_denied.code, "FleetHangarAccessDenied");

    let corpmate_id = 90_000_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(mut corp_ch) = snap.characters.get(&owner_id).cloned() {
            corp_ch.character_id = corpmate_id;
            snap.characters.insert(corpmate_id, corp_ch);
        }
    });
    write_container_ship_state(&ctx, |st| {
        let cfg = st.ship_configs.entry(archon_id).or_default();
        cfg.allow_corp_smb = true;
        cfg.password = Some("bay-pass".into());
    });
    ctx.db.with_snapshot(|snap| {
        assert!(!can_character_access_ship_bay_with_password_snap(snap, corpmate_id, archon_id, owner_id, true, Some("wrong")));
        assert!(can_character_access_ship_bay_with_password_snap(snap, corpmate_id, archon_id, owner_id, true, Some("bay-pass")));
    });
}

#[tokio::test]
async fn alsc_assemble_passwords_lock_unlock_and_audit_log_work_end_to_end() {
    let ctx = test_ctx();
    let (char_id, station_id, sys_id) = (90_000_001_i64, 60_003_760_i64, 30_000_142_u32);
    let (ship_id, cont_id, item_inside_id) = (53001, 53002, 53003);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(station_id);
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(ship_id, make_item(ship_id, 587, char_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(cont_id, make_item(cont_id, SYNTH_SECURE_CONT_TID, char_id, station_id, InventoryFlag::Hangar, 1, false));
        snap.items.insert(item_inside_id, make_item(item_inside_id, 34, char_id, cont_id, InventoryFlag::Cargo, 500, false));
    });

    let mut session = make_docked_session(char_id, station_id, sys_id, ship_id);
    let kw = HashMap::new();

    let asm_rep = handle_containers_ships_and_fitting(&ctx, &mut session, None, "invbroker", "AssembleCargoContainer", &[PyRep::Integer(cont_id)], &kw)
        .await.unwrap().unwrap();
    assert_eq!(dict_get(&asm_rep, "singleton").and_then(|v| v.as_bool()), Some(true));

    handle_containers_ships_and_fitting(&ctx, &mut session, None, "invbroker", "SetPassword", &[PyRep::Integer(cont_id), PyRep::Integer(1), PyRep::String("open-sesame".into())], &kw).await.unwrap().unwrap();
    handle_containers_ships_and_fitting(&ctx, &mut session, None, "invbroker", "SetPassword", &[PyRep::Integer(cont_id), PyRep::Integer(2), PyRep::String("cfg-sesame".into())], &kw).await.unwrap().unwrap();

    let cfg_rep = handle_containers_ships_and_fitting(&ctx, &mut session, None, "invbroker", "ALSCConfigGet", &[PyRep::Integer(cont_id)], &kw).await.unwrap().unwrap();
    assert_eq!(dict_get(&cfg_rep, "passwordRequired").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(dict_get(&cfg_rep, "hasPassword").and_then(|v| v.as_bool()), Some(true));

    handle_containers_ships_and_fitting(&ctx, &mut session, None, "invbroker", "ALSCLock", &[PyRep::Integer(cont_id), PyRep::List(vec![PyRep::Integer(item_inside_id)])], &kw).await.unwrap().unwrap();
    let locked_locs = handle_containers_ships_and_fitting(&ctx, &mut session, None, "itemLocking", "GetLockedItemLocations", &[], &kw).await.unwrap().unwrap();
    assert!(matches!(locked_locs, PyRep::List(ref l) if l.contains(&PyRep::Integer(cont_id))));
    let locked_items = handle_containers_ships_and_fitting(&ctx, &mut session, None, "itemLocking", "GetItemsByLocation", &[PyRep::Integer(cont_id)], &kw).await.unwrap().unwrap();
    assert!(matches!(locked_items, PyRep::Dict(ref d) if !d.is_empty()));

    handle_containers_ships_and_fitting(&ctx, &mut session, None, "invbroker", "ALSCUnlock", &[PyRep::Integer(cont_id), PyRep::List(vec![PyRep::Integer(item_inside_id)])], &kw).await.unwrap().unwrap();
    let log_rep = handle_containers_ships_and_fitting(&ctx, &mut session, None, "invbroker", "ALSCLogGet", &[PyRep::Integer(cont_id)], &kw).await.unwrap().unwrap();
    let entries = dict_get(&log_rep, "entries").expect("alsc entries");
    assert!(matches!(entries, PyRep::List(l) if l.len() >= 2));
}

#[tokio::test]
async fn rig_destruction_on_unfit_and_t3_subsystem_unfit_guard_and_ship_boarding_rules() {
    let ctx = test_ctx();
    let (char_id, station_id, sys_id) = (90_000_001_i64, 60_003_760_i64, 30_000_142_u32);
    let (loki_id, subsys_id, high_mod_id, rig_id, spare_ship_id) = (54001, 54002, 54003, 54004, 54005);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(station_id);
            ch.solar_system_id = sys_id;
            ch.active_ship_id = loki_id;
        }
        snap.items.insert(loki_id, make_item(loki_id, 29990, char_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(subsys_id, make_item(subsys_id, 29970, char_id, loki_id, InventoryFlag::SubSystemSlot0, 1, true));
        snap.items.insert(high_mod_id, make_item(high_mod_id, 492, char_id, loki_id, InventoryFlag::HiSlot7, 1, true));
        snap.items.insert(rig_id, make_item(rig_id, 2605, char_id, loki_id, InventoryFlag::RigSlot0, 1, true));
        snap.items.insert(spare_ship_id, make_item(spare_ship_id, 587, char_id, station_id, InventoryFlag::Hangar, 1, false));
    });

    let mut session = make_docked_session(char_id, station_id, sys_id, loki_id);
    let kw = HashMap::new();

    // 1. Unfitting T3 subsystem while high-slot module occupies subsystem slot fails with SubsystemHasFittedModules
    let sub_err = inventory_fitting::handle(
        &ctx, &mut session, None, "invbroker", "Add",
        &[PyRep::Integer(subsys_id), PyRep::Integer(station_id), PyRep::Integer(InventoryFlag::Hangar as i64)], &kw,
    ).await.unwrap().unwrap_err();
    assert_eq!(sub_err.code, "SubsystemHasFittedModules");

    // 2. Unfitting rig to station hangar destroys the rig
    let rig_rep = inventory_fitting::handle(
        &ctx, &mut session, None, "invbroker", "Add",
        &[PyRep::Integer(rig_id), PyRep::Integer(station_id), PyRep::Integer(InventoryFlag::Hangar as i64)], &kw,
    ).await.unwrap().unwrap();
    assert_eq!(dict_get(&rig_rep, "destroyed").and_then(|v| v.as_bool()), Some(true));
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&rig_id)));

    // 3. Assemble spare ship via ship.AssembleShip, then repackage via repackagingSvc.RepackageItems
    inventory_fitting::handle(&ctx, &mut session, None, "ship", "AssembleShip", &[PyRep::Integer(spare_ship_id)], &kw).await.unwrap().unwrap();
    assert!(ctx.db.with_snapshot(|s| s.items.get(&spare_ship_id).unwrap().singleton));

    let active_repack_err = inventory_fitting::handle(&ctx, &mut session, None, "repackagingSvc", "RepackageItems", &[PyRep::List(vec![PyRep::Integer(loki_id)])], &kw).await.unwrap().unwrap_err();
    assert_eq!(active_repack_err.code, "CannotRepackageActiveShip");

    inventory_fitting::handle(&ctx, &mut session, None, "repackagingSvc", "RepackageItems", &[PyRep::List(vec![PyRep::Integer(spare_ship_id)])], &kw).await.unwrap().unwrap();
    assert!(!ctx.db.with_snapshot(|s| s.items.get(&spare_ship_id).unwrap().singleton));

    // 4. Eject docked -> CannotEjectWhileDocked; LeaveShip -> capsule; LeaveShip in capsule -> CannotEjectFromCapsule
    assert_eq!(inventory_fitting::handle(&ctx, &mut session, None, "ship", "Eject", &[], &kw).await.unwrap().unwrap_err().code, "CannotEjectWhileDocked");
    let leave_rep = inventory_fitting::handle(&ctx, &mut session, None, "ship", "LeaveShip", &[], &kw).await.unwrap().unwrap();
    let pod_id = leave_rep.as_i64().or(session.ship_id).unwrap();
    assert_ne!(pod_id, loki_id);
    assert_eq!(session.ship_id, Some(pod_id));
    assert_eq!(inventory_fitting::handle(&ctx, &mut session, None, "ship", "LeaveShip", &[], &kw).await.unwrap().unwrap_err().code, "CannotEjectFromCapsule");

    // 5. In space: LeaveShip -> CannotLeaveShipInSpace; Board > 2500m -> ShipOutOfBoardingRange
    let mut space_sess = make_space_session(char_id, sys_id, pod_id);
    assert_eq!(inventory_fitting::handle(&ctx, &mut space_sess, None, "ship", "LeaveShip", &[], &kw).await.unwrap().unwrap_err().code, "CannotLeaveShipInSpace");

    let far_ship_id = 54006;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(far_ship_id, make_item(far_ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1, true));
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for (id, tid, pos, r) in [(pod_id, 670, DVec3::ZERO, 20.0), (far_ship_id, 587, DVec3::new(5_000.0, 0.0, 0.0), 40.0)] {
        let _ = sys_handle.spawn_ball(Ball { id, type_id: tid, position: pos, velocity: DVec3::ZERO, max_velocity: 100.0, inertia: 1.0, mass: 1_000.0, radius: r, speed_fraction: 0.0, mode: MovementMode::Stop }).await;
    }
    assert_eq!(inventory_fitting::handle(&ctx, &mut space_sess, None, "ship", "Board", &[PyRep::Integer(far_ship_id)], &kw).await.unwrap().unwrap_err().code, "SessionChangeTimerActive");
    ctx.session_change_timers.lock().unwrap().clear();
    assert_eq!(inventory_fitting::handle(&ctx, &mut space_sess, None, "ship", "Board", &[PyRep::Integer(far_ship_id)], &kw).await.unwrap().unwrap_err().code, "ShipOutOfBoardingRange");
}

#[tokio::test]
async fn container_open_validation_alsc_config_bitmask_asset_worth_and_plastic_wrap() {
    let ctx = test_ctx();
    let (char_id, station_id, sys_id) = (90_000_001_i64, 60_003_760_i64, 30_000_142_u32);
    let (pkg_cont_id, non_cont_id, wrap_id, inside_wrap_id, plex_id) = (55001, 55002, 55003, 55004, 55005);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(station_id);
            ch.solar_system_id = sys_id;
        }
        snap.items.retain(|_, i| i.owner_id != char_id);
        snap.items.insert(pkg_cont_id, make_item(pkg_cont_id, SYNTH_SECURE_CONT_TID, char_id, station_id, InventoryFlag::Hangar, 1, false));
        snap.items.insert(non_cont_id, make_item(non_cont_id, 492, char_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(wrap_id, make_item(wrap_id, 3468, char_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(inside_wrap_id, make_item(inside_wrap_id, 34, char_id, wrap_id, InventoryFlag::Locked, 250, false));
        snap.items.insert(plex_id, make_item(plex_id, 44992, char_id, station_id, InventoryFlag::Hangar, 10, false));
    });

    let mut session = make_docked_session(char_id, station_id, sys_id, 55099);
    let kw = HashMap::new();

    assert_eq!(handle_containers_ships_and_fitting(&ctx, &mut session, None, "invbroker", "ValidateItemCanBeOpened", &[PyRep::Integer(pkg_cont_id)], &kw).await.unwrap().unwrap_err().code, "CantOpenPackagedItem");
    assert_eq!(handle_containers_ships_and_fitting(&ctx, &mut session, None, "invbroker", "ValidateItemCanBeOpened", &[PyRep::Integer(non_cont_id)], &kw).await.unwrap().unwrap_err().code, "NotAContainer");

    handle_containers_ships_and_fitting(&ctx, &mut session, None, "invbroker", "AssembleCargoContainer", &[PyRep::Integer(pkg_cont_id)], &kw).await.unwrap().unwrap();
    let cfg_kw = HashMap::from([("configBitmask".into(), PyRep::Integer(3))]);
    handle_containers_ships_and_fitting(&ctx, &mut session, None, "invbroker", "ALSCConfigSet", &[PyRep::Integer(pkg_cont_id), PyRep::Integer(2), PyRep::String("pw2".into())], &cfg_kw).await.unwrap().unwrap();
    let cfg_get = handle_containers_ships_and_fitting(&ctx, &mut session, None, "invbroker", "ALSCConfigGet", &[PyRep::Integer(pkg_cont_id)], &kw).await.unwrap().unwrap();
    assert_eq!(dict_get(&cfg_get, "lockByDefault").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(dict_get(&cfg_get, "passwordPerAction").and_then(|v| v.as_bool()), Some(true));

    write_container_ship_state(&ctx, |st| {
        st.container_passwords.insert(pkg_cont_id, "pw1".into());
        st.unlocked_containers.clear();
    });
    assert_eq!(handle_containers_ships_and_fitting(&ctx, &mut session, None, "invbroker", "ValidateItemCanBeOpened", &[PyRep::Integer(pkg_cont_id)], &kw).await.unwrap().unwrap_err().code, "ContainerIsLocked");

    handle_containers_ships_and_fitting(&ctx, &mut session, None, "invbroker", "BreakPlasticWrap", &[PyRep::Integer(wrap_id)], &kw).await.unwrap().unwrap();
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&wrap_id) && s.items[&inside_wrap_id].location_id == station_id));

    let all_rep = handle_containers_ships_and_fitting(&ctx, &mut session, None, "invbroker", "GetAllItems", &[], &kw).await.unwrap().unwrap();
    assert_eq!(dict_get(&all_rep, "totalItems").and_then(|v| v.as_i64()), Some(4));

    let worth_rep = handle_containers_ships_and_fitting(&ctx, &mut session, None, "invbroker", "GetAssetWorth", &[], &kw).await.unwrap().unwrap();
    assert!(matches!(worth_rep, PyRep::Tuple(t) if t.len() == 2));
    assert!(handle_containers_ships_and_fitting(&ctx, &mut session, None, "invbroker", "GetPriceForItems", &[PyRep::List(vec![PyRep::Integer(non_cont_id)])], &kw).await.is_some());
    assert!(handle_containers_ships_and_fitting(&ctx, &mut session, None, "encodedItems", "DecodeItem", &[PyRep::String("abc".into())], &kw).await.is_some());
}

#[tokio::test]
async fn clear_charges_destroy_fitting_and_in_space_mobile_depot_unfit_rules() {
    let ctx = test_ctx();
    let (char_id, sys_id) = (90_000_001_i64, 30_000_142_u32);
    let (ship_id, gun_id, charge_id, rig_id, depot_id) = (56001, 56002, 56003, 56004, 56005);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(ship_id, make_item(ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1, true));
        snap.items.insert(gun_id, make_item(gun_id, 492, char_id, ship_id, InventoryFlag::HiSlot0, 1, true));
        snap.items.insert(charge_id, make_item(charge_id, 215, char_id, ship_id, InventoryFlag::HiSlot0, 1, false));
        snap.items.insert(rig_id, make_item(rig_id, 2605, char_id, ship_id, InventoryFlag::RigSlot0, 1, true));
    });

    let mut space_sess = make_space_session(char_id, sys_id, ship_id);
    let kw = HashMap::new();

    let no_depot_err = inventory_fitting::handle(&ctx, &mut space_sess, None, "invbroker", "Add", &[PyRep::Integer(gun_id), PyRep::Integer(ship_id), PyRep::Integer(InventoryFlag::Cargo as i64)], &kw).await.unwrap().unwrap_err();
    assert_eq!(no_depot_err.code, "CannotUnfitInSpace");

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(depot_id, make_item(depot_id, SYNTH_DEPOT_TID, char_id, sys_id as i64, InventoryFlag::None, 1, true));
    });
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h.spawn_ball(Ball { id: ship_id, type_id: 587, position: DVec3::ZERO, velocity: DVec3::ZERO, max_velocity: 100.0, inertia: 1.0, mass: 1_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop }).await;
    let _ = sys_h.spawn_ball(Ball { id: depot_id, type_id: SYNTH_DEPOT_TID, position: DVec3::new(1_000.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 0.0, inertia: 1.0, mass: 1_000.0, radius: 100.0, speed_fraction: 0.0, mode: MovementMode::Stop }).await;

    handle_containers_ships_and_fitting(&ctx, &mut space_sess, None, "invbroker", "ClearCharges", &[PyRep::Integer(ship_id)], &kw).await.unwrap().unwrap();
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&charge_id].flag), InventoryFlag::Cargo);

    handle_containers_ships_and_fitting(&ctx, &mut space_sess, None, "invbroker", "DestroyFitting", &[PyRep::Integer(gun_id)], &kw).await.unwrap().unwrap();
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&gun_id)));
}

#[tokio::test]
async fn mutants_cov_stub_type_alsc_lock_config_log_and_can_open() {
    use crate::services::inventory_fitting::fitting_containers_ships::resolve_missing_dynamic_ship_stub_type_id;

    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let other_char = 90_000_002_i64;
    let station_id = 60_003_760_i64;
    let sys_id = 30_000_142_u32;

    // 1. resolve_missing_dynamic_ship_stub_type_id range/char/item/killmail boundaries (11 mutants)
    let existing_dyn = 250_000_000_i64;
    let missing_dyn = 250_000_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.killmails.clear();
        snap.items.insert(
            existing_dyn,
            make_item(existing_dyn, 587, char_id, station_id, InventoryFlag::Hangar, 1, true),
        );
    });
    assert_eq!(resolve_missing_dynamic_ship_stub_type_id(&ctx, char_id, 199_999_999), None);
    assert_eq!(resolve_missing_dynamic_ship_stub_type_id(&ctx, char_id, 1_000_000_000_000), None);
    assert_eq!(resolve_missing_dynamic_ship_stub_type_id(&ctx, 0, missing_dyn), None);
    assert_eq!(resolve_missing_dynamic_ship_stub_type_id(&ctx, -1, missing_dyn), None);
    assert_eq!(resolve_missing_dynamic_ship_stub_type_id(&ctx, char_id, existing_dyn), None);
    assert_eq!(resolve_missing_dynamic_ship_stub_type_id(&ctx, char_id, missing_dyn), Some(670));

    // Distractor killmails: km1 newer with ship_type_id=0, km2 newer with wrong victim=other_char, km3 valid (type 622)
    ctx.db.with_mut_snapshot(|snap| {
        snap.killmails.insert(
            1,
            eve_db::KillmailRecord {
                kill_id: 1,
                victim_character_id: char_id,
                ship_type_id: 0,
                killed_unix: 9_999,
                ..Default::default()
            },
        );
        snap.killmails.insert(
            2,
            eve_db::KillmailRecord {
                kill_id: 2,
                victim_character_id: other_char,
                ship_type_id: 639,
                killed_unix: 5_000,
                ..Default::default()
            },
        );
        snap.killmails.insert(
            3,
            eve_db::KillmailRecord {
                kill_id: 3,
                victim_character_id: char_id,
                ship_type_id: 622,
                killed_unix: 4_000,
                ..Default::default()
            },
        );
    });
    assert_eq!(resolve_missing_dynamic_ship_stub_type_id(&ctx, char_id, missing_dyn), Some(622));

    // 2. handle_containers_ships_and_fitting: non-matching service, ALSC lock/unlock, ConfigGet, LogGet, ValidateItemCanBeOpened
    let cont_id = 58_001_i64;
    let inner_id = 58_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(station_id);
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
        }
        snap.items.insert(
            cont_id,
            make_item(cont_id, SYNTH_STATION_CONT_TID, char_id, station_id, InventoryFlag::Hangar, 1, true),
        );
        snap.items.insert(
            inner_id,
            make_item(inner_id, 34, char_id, cont_id, InventoryFlag::Unlocked, 10, false),
        );
    });
    let mut sess = make_docked_session(char_id, station_id, sys_id, 56001);
    let kw = HashMap::new();

    let other_svc = ["other", "Svc"].concat();
    assert!(
        handle_containers_ships_and_fitting(&ctx, &mut sess, None, &other_svc, "ALSCConfigGet", &[], &kw)
            .await
            .is_none()
    );

    let can_open = handle_containers_ships_and_fitting(
        &ctx,
        &mut sess,
        None,
        "invbroker",
        "ValidateItemCanBeOpened",
        &[PyRep::Integer(cont_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("ValidateItemCanBeOpened");
    assert_eq!(can_open, PyRep::Bool(true));

    let lock_rep = handle_containers_ships_and_fitting(
        &ctx,
        &mut sess,
        None,
        "invbroker",
        "ALSCLockItems",
        &[PyRep::Integer(cont_id), PyRep::List(vec![PyRep::Integer(inner_id)])],
        &kw,
    )
    .await
    .unwrap()
    .expect("ALSCLockItems");
    assert_eq!(dict_get(&lock_rep, "locked"), Some(&PyRep::Bool(true)));
    assert_eq!(dict_get(&lock_rep, "count"), Some(&PyRep::Integer(1)));

    let unlock_rep = handle_containers_ships_and_fitting(
        &ctx,
        &mut sess,
        None,
        "invbroker",
        "ALSCUnlockItems",
        &[PyRep::Integer(cont_id), PyRep::List(vec![PyRep::Integer(inner_id)])],
        &kw,
    )
    .await
    .unwrap()
    .expect("ALSCUnlockItems");
    assert_eq!(dict_get(&unlock_rep, "locked"), Some(&PyRep::Bool(false)));
    assert_eq!(dict_get(&unlock_rep, "count"), Some(&PyRep::Integer(1)));

    let cfg_rep = handle_containers_ships_and_fitting(
        &ctx,
        &mut sess,
        None,
        "invbroker",
        "ALSCConfigGet",
        &[PyRep::Integer(cont_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("ALSCConfigGet");
    assert_eq!(dict_get(&cfg_rep, "containerID"), Some(&PyRep::Integer(cont_id)));
    assert_eq!(dict_get(&cfg_rep, "hasPassword"), Some(&PyRep::Bool(false)));

    let log_rep = handle_containers_ships_and_fitting(
        &ctx,
        &mut sess,
        None,
        "invbroker",
        "ALSCLogGet",
        &[PyRep::Integer(cont_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("ALSCLogGet");
    assert_eq!(dict_get(&log_rep, "containerID"), Some(&PyRep::Integer(cont_id)));
    assert!(dict_get(&log_rep, "logCount").and_then(|v| v.as_i64()).unwrap_or(0) >= 2);
}
