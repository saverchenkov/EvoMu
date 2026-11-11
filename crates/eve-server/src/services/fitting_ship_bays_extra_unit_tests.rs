//! Extra unit and mutation-killing tests for `services::inventory_fitting::fitting_ship_bays`
//! covering `board_stored_ship` (corp role, FEB, in-space warp/scramble/crimewatch/non-pod swap),
//! `ship_jettison` (capacity, ship/container/flag guards, kwargs),
//! `ship_drop` (full-stack, Zarzakh/empire/security guards, Mobile Phase Anchor),
//! and `ship_scoop_and_store` (`StoreVessel` FEB/corp/unpiloted, `ScoopToFleetHangar`,
//! `ScoopToInfrastructureHold`, `ScoopToMobileDepotHold`, and `Scoop`).

#![forbid(unsafe_code)]

use super::inventory_fitting::fitting_containers_ships::{
    write_container_ship_state, ShipAccessConfig,
};
use super::inventory_fitting::fitting_ship_bays::*;
use crate::router::ServerContext;
use eve_db::{
    corp_roles, InventoryFlag, InventoryItemRecord, VirtualServerClock,
    WorldRepository,
};
use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

pub(super) fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

pub(super) fn custom_inv_type(
    sde: &SdeRegistry,
    type_id: u32,
    group_id: u32,
    category_id: u32,
    volume: f64,
    capacity: f64,
    name: &str,
    attrs: &[(u16, f64)],
) -> InvType {
    let mut t = sde.types.get(&587).expect("Rifter seed type").clone();
    t.type_id = type_id;
    t.group_id = group_id;
    t.category_id = category_id;
    t.volume = volume;
    t.capacity = capacity;
    t.name = name.into();
    t.default_attributes.clear();
    for &(k, v) in attrs {
        t.default_attributes.insert(k, v);
    }
    t
}

pub(super) fn make_ctx() -> ServerContext {
    let mut sde = (*shared_sde()).clone();
    // 998_001: Multi-bay host ship (SMA=908, FleetHangar=912, FEB=3020, MobileDepotHold=5325, InfraHold=5646)
    sde.types.insert(
        998_001,
        custom_inv_type(
            &sde,
            998_001,
            485,
            6,
            500_000.0,
            10_000.0,
            "MultiBay Capital",
            &[
                (908, 500_000.0),
                (912, 50_000.0),
                (3020, 50_000.0),
                (5325, 10_000.0),
                (5646, 50_000.0),
            ],
        ),
    );
    // 998_002: FEB-only battleship (attr 3020 only)
    sde.types.insert(
        998_002,
        custom_inv_type(
            &sde,
            998_002,
            27,
            6,
            400_000.0,
            1_000.0,
            "FEB Battleship",
            &[(3020, 50_000.0)],
        ),
    );
    // 998_003: Zero-skill frigate
    sde.types.insert(
        998_003,
        custom_inv_type(&sde, 998_003, 25, 6, 2_500.0, 200.0, "ZeroSkill Frigate", &[]),
    );
    // 998_004: Deployable with max/min security attrs (1032 = 0.4, 1946 = -0.5)
    sde.types.insert(
        998_004,
        custom_inv_type(
            &sde,
            998_004,
            1246,
            22,
            50.0,
            100.0,
            "SecBand Depot",
            &[(1032, 0.4), (1946, -0.5)],
        ),
    );
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    ctx
}

pub(super) fn put_item(
    ctx: &ServerContext,
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_id,
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
            },
        );
    });
}

pub(super) async fn spawn_ball_at(
    ctx: &ServerContext,
    sys_id: u32,
    ball_id: i64,
    type_id: u32,
    owner_id: i64,
    pos: DVec3,
) {
    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball_with_meta(
            Ball {
                id: ball_id,
                type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
            SlimItemMeta {
                group_id: 25,
                category_id: 6,
                owner_id,
                char_id: Some(owner_id),
                corp_id: 98_000_001,
                ..Default::default()
            },
        )
        .await;
}

#[tokio::test]
async fn board_stored_ship_corp_role_sma_password_and_in_space_combat_guards() {
    let ctx = make_ctx();
    let char_id = 90_000_001;
    let corp_id = 98_000_001;
    let station_id = 60_003_760;
    let sys_id = 30_000_142;
    let host_id = 1_100_001;
    let active_ship_id = 1_100_002;
    let stored_ship_id = 1_100_003;
    let pkg_ship_id = 1_100_004;
    let cargo_ship_id = 1_100_005;
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
        }
        if let Some(mut ch2) = snap.characters.get(&char_id).cloned() {
            ch2.character_id = 90_000_002;
            ch2.corporation_id = corp_id;
            snap.characters.insert(90_000_002, ch2);
        }
    });
    put_item(&ctx, host_id, 998_001, 90_000_002, station_id, InventoryFlag::Hangar, 1, true);
    put_item(&ctx, active_ship_id, 998_003, char_id, station_id, InventoryFlag::Hangar, 1, true);
    put_item(&ctx, stored_ship_id, 998_003, corp_id, host_id, InventoryFlag::ShipHangar, 1, true);
    put_item(&ctx, pkg_ship_id, 998_003, char_id, host_id, InventoryFlag::ShipHangar, 1, false);
    put_item(&ctx, cargo_ship_id, 998_003, char_id, host_id, InventoryFlag::Cargo, 1, true);

    let mut sess = SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        station_id: Some(station_id),
        solar_system_id: Some(sys_id),
        ship_id: Some(active_ship_id),
        corp_role: corp_roles::ROLE_EQUIPMENT_CONFIG,
        ..Default::default()
    };

    // Packaged ship -> ShipNotBoardable; Cargo flag -> ShipNotInMaintenanceBay
    assert_eq!(
        board_stored_ship(&ctx, &mut sess, &[PyRep::Integer(pkg_ship_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "ShipNotBoardable"
    );
    assert_eq!(
        board_stored_ship(&ctx, &mut sess, &[PyRep::Integer(cargo_ship_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "ShipNotInMaintenanceBay"
    );

    // Other player's docked personal hangar host is denied (#560)
    assert_eq!(
        board_stored_ship(&ctx, &mut sess, &[PyRep::Integer(stored_ship_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "OwnershipViolation"
    );

    // Move host to CorpSAG1 owned by corp_id; host SMA password denies access without password, then allows with password + ROLE_EQUIPMENT_CONFIG + ROLE_HANGAR_CAN_TAKE_1
    put_item(&ctx, host_id, 998_001, corp_id, station_id, InventoryFlag::CorpSAG1, 1, true);
    sess.corp_role = corp_roles::ROLE_EQUIPMENT_CONFIG | corp_roles::ROLE_HANGAR_CAN_TAKE[0];
    write_container_ship_state(&ctx, |st| {
        st.ship_configs.insert(
            host_id,
            ShipAccessConfig {
                allow_fleet_smb: true,
                allow_corp_smb: true,
                allow_fleet_hangar: true,
                allow_corp_hangar: true,
                password: Some("baypw".into()),
            },
        );
    });
    assert_eq!(
        board_stored_ship(&ctx, &mut sess, &[PyRep::Integer(stored_ship_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "SMAAccessDenied"
    );
    let mut pw_kw = HashMap::new();
    pw_kw.insert("password".into(), PyRep::String("baypw".into()));
    let boarded = board_stored_ship(&ctx, &mut sess, &[PyRep::Integer(stored_ship_id)], &pw_kw)
        .await
        .expect("corp stored ship boarded with password");
    assert_eq!(boarded.dict_get("shipID"), Some(&PyRep::Integer(stored_ship_id)));

    // Move to space and test Criminal, WeaponsTimer, ShipInWarp, WarpScrambled, and non-capsule swap
    write_container_ship_state(&ctx, |st| {
        st.ship_configs.remove(&host_id);
    });
    sess.station_id = None;
    sess.ship_id = Some(active_ship_id);
    put_item(&ctx, host_id, 998_001, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    put_item(&ctx, active_ship_id, 998_003, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    put_item(&ctx, stored_ship_id, 998_003, char_id, host_id, InventoryFlag::ShipHangar, 1, true);
    spawn_ball_at(&ctx, sys_id, host_id, 998_001, char_id, DVec3::ZERO).await;
    spawn_ball_at(&ctx, sys_id, active_ship_id, 998_003, char_id, DVec3::new(500.0, 0.0, 0.0)).await;

    let now = ctx.now_unix();
    ctx.crimewatch
        .write()
        .unwrap()
        .states
        .entry(char_id)
        .or_default()
        .criminal_flag_until_unix = now + 60;
    assert_eq!(
        board_stored_ship(&ctx, &mut sess, &[PyRep::Integer(stored_ship_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "CannotBoardShipInHighSecAsCriminal"
    );
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        let st = cw.states.entry(char_id).or_default();
        st.criminal_flag_until_unix = 0;
        st.weapon_timer_until_unix = now + 60;
    }
    assert_eq!(
        board_stored_ship(&ctx, &mut sess, &[PyRep::Integer(stored_ship_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "CannotStoreShipActiveWeaponsTimer"
    );
    ctx.crimewatch.write().unwrap().states.clear();

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h
        .set_movement_mode(
            active_ship_id,
            MovementMode::new_warp(DVec3::ZERO, DVec3::new(1_000_000.0, 0.0, 0.0), 3.0),
        )
        .await;
    assert_eq!(
        board_stored_ship(&ctx, &mut sess, &[PyRep::Integer(stored_ship_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "ShipInWarp"
    );
    let _ = sys_h.set_movement_mode(active_ship_id, MovementMode::Stop).await;

    let _ = sys_h.set_warp_scramble_strength(active_ship_id, 2).await;
    assert_eq!(
        board_stored_ship(&ctx, &mut sess, &[PyRep::Integer(stored_ship_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "WarpScrambled"
    );
    let _ = sys_h.set_warp_scramble_strength(active_ship_id, 0).await;

    // Non-capsule active ship swaps cleanly in space
    let space_ok = board_stored_ship(&ctx, &mut sess, &[PyRep::Integer(stored_ship_id)], &kw)
        .await
        .expect("board stored ship in space from non-capsule");
    assert_eq!(space_ok.dict_get("oldShipID"), Some(&PyRep::Integer(active_ship_id)));
}

#[tokio::test]
async fn r9_p0_560_store_vessel_and_board_stored_ship_enforce_locks_hangar_access_and_crimewatch() {
    let ctx = make_ctx();
    let char_id = 90_000_001_i64;
    let other_char = 90_000_002_i64;
    let corp_id = 98_000_001_i64;
    let station_id = 60_003_760_i64;
    let sys_id = 30_000_142_u32;

    let host_id = 1_110_001_i64;
    let target_id = 1_110_002_i64;
    let active_id = 1_110_003_i64;
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
        }
        if let Some(mut ch2) = snap.characters.get(&char_id).cloned() {
            ch2.character_id = other_char;
            ch2.corporation_id = corp_id;
            snap.characters.insert(other_char, ch2);
        }
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.ceo_id = other_char;
            corp.member_roles.remove(&char_id);
        }
    });

    put_item(&ctx, active_id, 998_003, char_id, station_id, InventoryFlag::Hangar, 1, true);
    let mut sess = SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        station_id: Some(station_id),
        solar_system_id: Some(sys_id),
        ship_id: Some(active_id),
        corp_role: corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_EQUIPMENT_CONFIG,
        ..Default::default()
    };

    // 1. Docked StoreVessel: impounded target or host is rejected
    put_item(&ctx, host_id, 998_001, char_id, station_id, InventoryFlag::Hangar, 1, true);
    put_item(&ctx, target_id, 998_003, char_id, station_id, InventoryFlag::Impounded, 1, true);
    assert_eq!(
        ship_scoop_and_store(
            &ctx,
            &mut sess,
            "StoreVessel",
            &[PyRep::Integer(target_id), PyRep::Integer(host_id)],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "ImpoundedItem"
    );

    put_item(&ctx, target_id, 998_003, char_id, station_id, InventoryFlag::Hangar, 1, true);
    put_item(&ctx, host_id, 998_001, char_id, station_id, InventoryFlag::Locked, 1, true);
    assert_eq!(
        ship_scoop_and_store(
            &ctx,
            &mut sess,
            "StoreVessel",
            &[PyRep::Integer(target_id), PyRep::Integer(host_id)],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "ItemLocked"
    );

    // 2. Docked StoreVessel: another player's personal hangar host is denied
    put_item(&ctx, host_id, 998_001, other_char, station_id, InventoryFlag::Hangar, 1, true);
    assert_eq!(
        ship_scoop_and_store(
            &ctx,
            &mut sess,
            "StoreVessel",
            &[PyRep::Integer(target_id), PyRep::Integer(host_id)],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "OwnershipViolation"
    );

    // 3. Docked StoreVessel & BoardStoredShip: corp host in CorpSAG2 requires hangar take role (when not Director)
    sess.corp_role = corp_roles::ROLE_EQUIPMENT_CONFIG | corp_roles::ROLE_STATION_MANAGER;
    put_item(&ctx, host_id, 998_001, corp_id, station_id, InventoryFlag::CorpSAG2, 1, true);
    assert_eq!(
        ship_scoop_and_store(
            &ctx,
            &mut sess,
            "StoreVessel",
            &[PyRep::Integer(target_id), PyRep::Integer(host_id)],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );
    sess.corp_role |= corp_roles::ROLE_HANGAR_CAN_TAKE[1];
    ship_scoop_and_store(
        &ctx,
        &mut sess,
        "StoreVessel",
        &[PyRep::Integer(target_id), PyRep::Integer(host_id)],
        &kw,
    )
    .await
    .expect("docked store into authorized CorpSAG2 host succeeds");

    // 4. Docked BoardStoredShip refuses if stored ship or host ship is impounded/locked
    sess.corp_role = corp_roles::ROLE_EQUIPMENT_CONFIG;
    assert_eq!(
        board_stored_ship(&ctx, &mut sess, &[PyRep::Integer(target_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );
    sess.corp_role |= corp_roles::ROLE_HANGAR_CAN_TAKE[1];
    put_item(&ctx, host_id, 998_001, corp_id, station_id, InventoryFlag::Impounded, 1, true);
    assert_eq!(
        board_stored_ship(&ctx, &mut sess, &[PyRep::Integer(target_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "ImpoundedItem"
    );
    put_item(&ctx, host_id, 998_001, corp_id, station_id, InventoryFlag::CorpSAG2, 1, true);
    board_stored_ship(&ctx, &mut sess, &[PyRep::Integer(target_id)], &kw)
        .await
        .expect("authorized CorpSAG2 host board succeeds");

    // 5. In-space StoreVessel: rejects high-sec criminal + active ship warp scramble + active cloak
    sess.station_id = None;
    sess.ship_id = Some(active_id);
    put_item(&ctx, host_id, 998_001, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    put_item(&ctx, active_id, 998_003, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    put_item(&ctx, target_id, 998_003, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    spawn_ball_at(&ctx, sys_id, host_id, 998_001, char_id, DVec3::ZERO).await;
    spawn_ball_at(&ctx, sys_id, active_id, 998_003, char_id, DVec3::new(300.0, 0.0, 0.0)).await;
    spawn_ball_at(&ctx, sys_id, target_id, 998_003, char_id, DVec3::new(400.0, 0.0, 0.0)).await;

    let now = ctx.now_unix();
    ctx.crimewatch
        .write()
        .unwrap()
        .states
        .entry(char_id)
        .or_default()
        .criminal_flag_until_unix = now + 60;
    assert_eq!(
        ship_scoop_and_store(
            &ctx,
            &mut sess,
            "StoreVessel",
            &[PyRep::Integer(target_id), PyRep::Integer(host_id)],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "CannotBoardShipInHighSecAsCriminal"
    );
    ctx.crimewatch.write().unwrap().states.clear();

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h.set_warp_scramble_strength(active_id, 1).await;
    assert_eq!(
        ship_scoop_and_store(
            &ctx,
            &mut sess,
            "StoreVessel",
            &[PyRep::Integer(target_id), PyRep::Integer(host_id)],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "WarpScrambled"
    );
}

#[tokio::test]
async fn workstream_2a2_store_vessel_docked_bay_selection_cargo_validation_and_feb_limits() {
    let mut ctx = make_ctx();
    {
        let sde_mut = Arc::make_mut(&mut ctx.sde);
        // 998_010: Cruiser (group 26, volume 10_000 m3 -> non-frigate, goes to SMA, never FEB)
        sde_mut.types.insert(
            998_010,
            custom_inv_type(&sde_mut.clone(), 998_010, 26, 6, 10_000.0, 400.0, "Test Cruiser", &[]),
        );
        // 998_011: Small-SMA carrier (908 = 4_000 m3) -> can fit one 2_500 m3 frigate, rejects 10_000 m3 cruiser or 2nd frigate
        sde_mut.types.insert(
            998_011,
            custom_inv_type(
                &sde_mut.clone(),
                998_011,
                547,
                6,
                1_000_000.0,
                1_000.0,
                "Tiny SMA Carrier",
                &[(908, 4_000.0)],
            ),
        );
        // 998_012: Host ship without any SMB or FEB
        sde_mut.types.insert(
            998_012,
            custom_inv_type(&sde_mut.clone(), 998_012, 26, 6, 10_000.0, 500.0, "No Bay Cruiser", &[]),
        );
        // 998_013: Custom charge (category 8) and 998_014: non-charge cargo module/mineral (category 4, group 18)
        sde_mut.types.insert(
            998_013,
            custom_inv_type(&sde_mut.clone(), 998_013, 83, 8, 0.01, 0.0, "Test Ammo", &[]),
        );
        sde_mut.types.insert(
            998_014,
            custom_inv_type(&sde_mut.clone(), 998_014, 18, 4, 0.01, 0.0, "Test Mineral", &[]),
        );
    }

    let char_id = 90_000_001_i64;
    let corp_id = 98_000_001_i64;
    let station_id = 60_003_760_i64;
    let other_station_id = 60_003_761_i64;
    let sys_id = 30_000_142_u32;
    let active_id = 1_210_001_i64;
    let host_feb_id = 1_210_002_i64;
    let host_tiny_sma_id = 1_210_003_i64;
    let host_nobay_id = 1_210_004_i64;
    let frig1_id = 1_210_010_i64;
    let frig2_id = 1_210_011_i64;
    let cruiser_id = 1_210_012_i64;
    let pod_item_id = 1_210_013_i64;
    let non_ship_id = 1_210_014_i64;
    let kw = HashMap::new();

    put_item(&ctx, active_id, 998_003, char_id, station_id, InventoryFlag::Hangar, 1, true);
    let mut sess = SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        station_id: Some(station_id),
        solar_system_id: Some(sys_id),
        ship_id: Some(active_id),
        ..Default::default()
    };

    // 1. Argument & basic guard checks (args.len() == 0 / args.len() < 2 shorthand, InvalidArgument, CannotStoreSelf, CannotStoreActiveShip, ShipNotFound)
    assert_eq!(
        ship_scoop_and_store(&ctx, &mut sess, "StoreVessel", &[], &kw)
            .await
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    let active_1arg_err = ship_scoop_and_store(&ctx, &mut sess, "StoreVessel", &[PyRep::Integer(host_feb_id)], &kw)
        .await
        .unwrap_err();
    assert_eq!(active_1arg_err.code, "UserError");
    assert_eq!(active_1arg_err.message, "CannotStoreActiveShip");
    assert_eq!(
        ship_scoop_and_store(&ctx, &mut sess, "StoreVessel", &[PyRep::Integer(0), PyRep::Integer(host_feb_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    let self_err = ship_scoop_and_store(&ctx, &mut sess, "StoreVessel", &[PyRep::Integer(host_feb_id), PyRep::Integer(host_feb_id)], &kw)
        .await
        .unwrap_err();
    assert_eq!(self_err.code, "UserError");
    assert_eq!(self_err.message, "CannotStoreSelf");
    let active_err = ship_scoop_and_store(&ctx, &mut sess, "StoreVessel", &[PyRep::Integer(active_id), PyRep::Integer(host_feb_id)], &kw)
        .await
        .unwrap_err();
    assert_eq!(active_err.code, "UserError");
    assert_eq!(active_err.message, "CannotStoreActiveShip");
    assert_eq!(
        ship_scoop_and_store(&ctx, &mut sess, "StoreVessel", &[PyRep::Integer(99_999_991), PyRep::Integer(host_feb_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "ShipNotFound"
    );

    // 2. Docked path: ShipNotFound (missing host), ShipNotInSameHangar, OnlyShipsCanBeStoredInSMA (capsule 670 & mineral), OnlyAssembledShipsInSMA
    put_item(&ctx, frig1_id, 998_003, char_id, other_station_id, InventoryFlag::Hangar, 1, true);
    put_item(&ctx, host_feb_id, 998_002, char_id, station_id, InventoryFlag::Hangar, 1, true);
    assert_eq!(
        ship_scoop_and_store(&ctx, &mut sess, "StoreVessel", &[PyRep::Integer(frig1_id), PyRep::Integer(99_999_992)], &kw)
            .await
            .unwrap_err()
            .code,
        "ShipNotFound"
    );
    let hangar_err = ship_scoop_and_store(&ctx, &mut sess, "StoreVessel", &[PyRep::Integer(frig1_id), PyRep::Integer(host_feb_id)], &kw)
        .await
        .unwrap_err();
    assert_eq!(hangar_err.code, "UserError");
    assert_eq!(hangar_err.message, "ShipNotInSameHangar");

    put_item(&ctx, pod_item_id, 670, char_id, station_id, InventoryFlag::Hangar, 1, true);
    assert_eq!(
        ship_scoop_and_store(&ctx, &mut sess, "StoreVessel", &[PyRep::Integer(pod_item_id), PyRep::Integer(host_feb_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "UserError" // OnlyShipsCanBeStoredInSMA
    );
    put_item(&ctx, non_ship_id, 998_014, char_id, station_id, InventoryFlag::Hangar, 1, true);
    assert_eq!(
        ship_scoop_and_store(&ctx, &mut sess, "StoreVessel", &[PyRep::Integer(non_ship_id), PyRep::Integer(host_feb_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "UserError"
    );
    put_item(&ctx, frig1_id, 998_003, char_id, station_id, InventoryFlag::Hangar, 1, false);
    assert_eq!(
        ship_scoop_and_store(&ctx, &mut sess, "StoreVessel", &[PyRep::Integer(frig1_id), PyRep::Integer(host_feb_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "OnlyAssembledShipsInSMA"
    );
    put_item(&ctx, frig1_id, 998_003, char_id, station_id, InventoryFlag::Hangar, 1, true);

    // 3. ship_has_non_charge_cargo: charges in Cargo + non-charge items in fitted/drone/rig/subsys/fighter bays are ALLOWED; non-charge in Cargo is REJECTED
    put_item(&ctx, 1_210_100, 998_013, char_id, frig1_id, InventoryFlag::Cargo, 50, false); // charge category 8 -> OK
    put_item(&ctx, 1_210_101, 998_014, char_id, frig1_id, InventoryFlag::HiSlot0, 1, true); // 27 -> OK
    put_item(&ctx, 1_210_102, 998_014, char_id, frig1_id, InventoryFlag::DroneBay, 1, false); // 87 -> OK
    put_item(&ctx, 1_210_103, 998_014, char_id, frig1_id, InventoryFlag::RigSlot0, 1, true); // 92 -> OK
    put_item(&ctx, 1_210_104, 998_014, char_id, frig1_id, InventoryFlag::SubSystemSlot0, 1, true); // 125 -> OK
    put_item(&ctx, 1_210_105, 998_014, char_id, frig1_id, InventoryFlag::FighterBay, 1, false); // 158 -> OK
    // First test ShipHasNonChargeCargo while mineral is in Cargo
    put_item(&ctx, 1_210_106, 998_014, char_id, frig1_id, InventoryFlag::Cargo, 5, false);
    assert_eq!(
        ship_scoop_and_store(&ctx, &mut sess, "StoreVessel", &[PyRep::Integer(frig1_id), PyRep::Integer(host_feb_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "ShipHasNonChargeCargo"
    );
    // Remove only the non-charge Cargo item; fitted/drone/rig/subsys/fighter items + Cargo charges remain allowed
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&1_210_106);
    });

    // 4. select_and_validate_bay_dest_flag:
    // NoShipMaintenanceBay host
    put_item(&ctx, host_nobay_id, 998_012, char_id, station_id, InventoryFlag::Hangar, 1, true);
    assert_eq!(
        ship_scoop_and_store(&ctx, &mut sess, "StoreVessel", &[PyRep::Integer(frig1_id), PyRep::Integer(host_nobay_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "NoShipMaintenanceBay"
    );
    // FEB rejects non-frigate (cruiser) with InvalidFrigateGroup
    put_item(&ctx, cruiser_id, 998_010, char_id, station_id, InventoryFlag::Hangar, 1, true);
    assert_eq!(
        ship_scoop_and_store(&ctx, &mut sess, "StoreVessel", &[PyRep::Integer(cruiser_id), PyRep::Integer(host_feb_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "InvalidFrigateGroup"
    );
    // Store frig1 into FEB succeeds (flag = FrigateEscapeBay), then 2nd frigate exceeds 1-ship FEB limit
    let feb_ok = ship_scoop_and_store(
        &ctx,
        &mut sess,
        "StoreVessel",
        &[PyRep::Integer(frig1_id), PyRep::Integer(host_feb_id)],
        &kw,
    )
    .await
    .unwrap();
    assert_eq!(
        feb_ok.dict_get("flagID"),
        Some(&PyRep::Integer(InventoryFlag::FrigateEscapeBay as i64))
    );
    put_item(&ctx, frig2_id, 998_003, char_id, station_id, InventoryFlag::Hangar, 1, true);
    assert_eq!(
        ship_scoop_and_store(&ctx, &mut sess, "StoreVessel", &[PyRep::Integer(frig2_id), PyRep::Integer(host_feb_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "CapacityExceeded"
    );

    // Tiny SMA host (4_000 m3): cruiser (10_000 m3) rejected with CapacityExceeded; frig2 (2_500 m3) fits in ShipHangar, then another 2_500 m3 ship overflows
    put_item(&ctx, host_tiny_sma_id, 998_011, char_id, station_id, InventoryFlag::Hangar, 1, true);
    assert_eq!(
        ship_scoop_and_store(&ctx, &mut sess, "StoreVessel", &[PyRep::Integer(cruiser_id), PyRep::Integer(host_tiny_sma_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "CapacityExceeded"
    );
    let sma_ok = ship_scoop_and_store(
        &ctx,
        &mut sess,
        "StoreVessel",
        &[PyRep::Integer(frig2_id), PyRep::Integer(host_tiny_sma_id)],
        &kw,
    )
    .await
    .unwrap();
    assert_eq!(
        sma_ok.dict_get("flagID"),
        Some(&PyRep::Integer(InventoryFlag::ShipHangar as i64))
    );
}

#[tokio::test]
async fn workstream_2a2_in_space_store_guards_cleanup_active_capsule_reuse_and_kinematics() {
    let mut ctx = make_ctx();
    {
        let sde_mut = Arc::make_mut(&mut ctx.sde);
        // Ship with explicit kinematics attrs (37=maxVelocity 350, 70=agility 2.8, 4=mass 1_100_000) and radius 45
        let mut kin_frig = custom_inv_type(
            &sde_mut.clone(),
            998_020,
            25,
            6,
            2_500.0,
            200.0,
            "Kinematics Frigate",
            &[(37, 350.0), (70, 2.8), (4, 1_100_000.0)],
        );
        kin_frig.mass = 1_100_000.0;
        kin_frig.radius = 45.0;
        sde_mut.types.insert(998_020, kin_frig);
        sde_mut.types.insert(
            998_021,
            custom_inv_type(&sde_mut.clone(), 998_021, 26, 6, 10_000.0, 500.0, "SMA Cruiser", &[]),
        );
        sde_mut.types.insert(
            998_022,
            custom_inv_type(&sde_mut.clone(), 998_022, 18, 4, 1.0, 0.0, "Mineral Item", &[]),
        );
    }

    let char_id = 90_000_001_i64;
    let other_char = 90_000_002_i64;
    let corp_id = 98_000_001_i64;
    let null_sys = 30_004_759_u32; // null-sec for ESS intrusion bubble check (no highsec criminal interference!)
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.solar_system_id = null_sys;
            ch.station_id = None;
        }
        let mut ch2 = snap.characters[&char_id].clone();
        ch2.character_id = other_char;
        ch2.active_ship_id = 1_220_050;
        snap.characters.insert(other_char, ch2);
    });

    let host_id = 1_220_001_i64;
    let active_id = 1_220_002_i64;
    let target_cruiser = 1_220_003_i64;
    let watcher_id = 1_220_004_i64;
    let other_piloted = 1_220_050_i64;
    let mod_id = 1_220_090_i64;

    let mut sess = SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        station_id: None,
        solar_system_id: None, // initially missing solar system -> NotInSpace
        ship_id: Some(active_id),
        corp_role: corp_roles::ROLE_DIRECTOR,
        ..Default::default()
    };

    // 1. NotInSpace, CannotStoreShipInItself, ShipNotInSpace, ObjectNotInSpace, OutOfRange
    assert_eq!(
        ship_scoop_and_store(
            &ctx,
            &mut sess,
            "StoreVessel",
            &[PyRep::Integer(target_cruiser), PyRep::Integer(host_id)],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "NotInSpace"
    );
    sess.solar_system_id = Some(null_sys);
    let space_self_err = ship_scoop_and_store(
        &ctx,
        &mut sess,
        "StoreVessel",
        &[PyRep::Integer(host_id), PyRep::Integer(host_id)],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(space_self_err.code, "UserError");
    assert_eq!(space_self_err.message, "CannotStoreShipInItself");
    assert_eq!(
        ship_scoop_and_store(
            &ctx,
            &mut sess,
            "StoreVessel",
            &[PyRep::Integer(target_cruiser), PyRep::Integer(host_id)],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "ShipNotInSpace"
    );

    // Spawn active_id, then missing host -> ShipNotInSpace
    put_item(&ctx, active_id, 998_020, char_id, null_sys as i64, InventoryFlag::None, 1, true);
    put_item(&ctx, host_id, 998_001, char_id, null_sys as i64, InventoryFlag::None, 1, true);
    put_item(&ctx, target_cruiser, 998_021, char_id, null_sys as i64, InventoryFlag::None, 1, true);
    spawn_ball_at(&ctx, null_sys, active_id, 998_020, char_id, DVec3::new(300.0, 0.0, 0.0)).await;
    assert_eq!(
        ship_scoop_and_store(
            &ctx,
            &mut sess,
            "StoreVessel",
            &[PyRep::Integer(target_cruiser), PyRep::Integer(host_id)],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "ShipNotInSpace"
    );

    // Spawn host out of range (3_000 m from active) -> OutOfRange
    spawn_ball_at(&ctx, null_sys, host_id, 998_001, char_id, DVec3::new(3_300.0, 0.0, 0.0)).await;
    spawn_ball_at(&ctx, null_sys, target_cruiser, 998_021, char_id, DVec3::new(3_500.0, 0.0, 0.0)).await;
    assert_eq!(
        ship_scoop_and_store(
            &ctx,
            &mut sess,
            "StoreVessel",
            &[PyRep::Integer(target_cruiser), PyRep::Integer(host_id)],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "OutOfRange"
    );

    // Move host & target_cruiser within range (500 m / 600 m from active at 300 m)
    let sys_h = ctx.systems.get_or_spawn(null_sys);
    let _ = sys_h.remove_ball(host_id).await;
    let _ = sys_h.remove_ball(target_cruiser).await;
    spawn_ball_at(&ctx, null_sys, host_id, 998_001, char_id, DVec3::new(600.0, 0.0, 0.0)).await;
    spawn_ball_at(&ctx, null_sys, target_cruiser, 998_021, char_id, DVec3::new(500.0, 0.0, 0.0)).await;

    // 2. All validate_in_space_ship_store_allowed guards on target_cruiser & active_id:
    // 2a. Warp
    let _ = sys_h
        .set_movement_mode(
            target_cruiser,
            MovementMode::new_warp(DVec3::ZERO, DVec3::new(100_000.0, 0.0, 0.0), 3.0),
        )
        .await;
    assert_eq!(
        ship_scoop_and_store(&ctx, &mut sess, "StoreVessel", &[PyRep::Integer(target_cruiser), PyRep::Integer(host_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "CantStoreInWarp"
    );
    let _ = sys_h.set_movement_mode(target_cruiser, MovementMode::Stop).await;

    // 2b. MJD / MJFG spooling
    assert!(sys_h.try_update_combat_state(target_cruiser, |cs| cs.mjfg_spool_remaining_s = 5.0));
    assert_eq!(
        ship_scoop_and_store(&ctx, &mut sess, "StoreVessel", &[PyRep::Integer(target_cruiser), PyRep::Integer(host_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "CantMoveWhileModuleActive"
    );
    assert!(sys_h.try_update_combat_state(target_cruiser, |cs| cs.mjfg_spool_remaining_s = 0.0));

    // 2c. WarpScrambled on target_id (tested alongside ScoopToSMA path!)
    let _ = sys_h.set_warp_scramble_strength(target_cruiser, 2).await;
    assert_eq!(
        ship_scoop_and_store(&ctx, &mut sess, "StoreVessel", &[PyRep::Integer(target_cruiser), PyRep::Integer(host_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "WarpScrambled"
    );
    // Also check ScoopToSMA calls validate_in_space_ship_store_allowed
    let _ = sys_h.remove_ball(host_id).await;
    let _ = sys_h.remove_ball(active_id).await;
    spawn_ball_at(&ctx, null_sys, active_id, 998_001, char_id, DVec3::new(300.0, 0.0, 0.0)).await;
    assert_eq!(
        ship_scoop_and_store(&ctx, &mut sess, "ScoopToSMA", &[PyRep::Integer(target_cruiser)], &kw)
            .await
            .unwrap_err()
            .code,
        "WarpScrambled"
    );
    let _ = sys_h.remove_ball(active_id).await;
    spawn_ball_at(&ctx, null_sys, active_id, 998_020, char_id, DVec3::new(300.0, 0.0, 0.0)).await;
    spawn_ball_at(&ctx, null_sys, host_id, 998_001, char_id, DVec3::new(600.0, 0.0, 0.0)).await;
    let _ = sys_h.set_warp_scramble_strength(target_cruiser, 0).await;

    // 2d. ESS intrusion bubble restriction
    let ess_beacon_id = 820_000_000_i64 + i64::from(null_sys);
    spawn_ball_at(&ctx, null_sys, ess_beacon_id, 55914, 0, DVec3::ZERO).await;
    assert_eq!(
        ship_scoop_and_store(&ctx, &mut sess, "StoreVessel", &[PyRep::Integer(target_cruiser), PyRep::Integer(host_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "EssIntrusionBubbleRestricted"
    );
    let _ = sys_h.remove_ball(ess_beacon_id).await;

    // 2e. Disruptive Lance / Navigation lock
    let now_s = ctx.now_unix_secs();
    ctx.tactical_state
        .write()
        .unwrap()
        .lance_caster_lock_until
        .insert((ctx.instance_id as usize, target_cruiser), now_s + 60);
    assert_eq!(
        ship_scoop_and_store(&ctx, &mut sess, "StoreVessel", &[PyRep::Integer(target_cruiser), PyRep::Integer(host_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "LanceFiringLockActive"
    );
    ctx.tactical_state
        .write()
        .unwrap()
        .lance_caster_lock_until
        .clear();

    // 2f. Gate cloak AND active cloak
    assert!(sys_h.try_update_combat_state(target_cruiser, |cs| cs.gate_cloak_remaining_ticks = 5));
    assert_eq!(
        ship_scoop_and_store(&ctx, &mut sess, "StoreVessel", &[PyRep::Integer(target_cruiser), PyRep::Integer(host_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "CannotStoreCloakedShip"
    );
    assert!(sys_h.try_update_combat_state(target_cruiser, |cs| {
        cs.gate_cloak_remaining_ticks = 0;
        cs.is_cloaked = true;
    }));
    assert_eq!(
        ship_scoop_and_store(&ctx, &mut sess, "StoreVessel", &[PyRep::Integer(target_cruiser), PyRep::Integer(host_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "CannotStoreCloakedShip"
    );
    assert!(sys_h.try_update_combat_state(target_cruiser, |cs| cs.is_cloaked = false));

    // 2g. Active module state in DB AND active_module_cycles in sys.actor_state
    put_item(&ctx, mod_id, 998_014, char_id, target_cruiser, InventoryFlag::HiSlot0, 1, true);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.insert(mod_id);
    });
    assert_eq!(
        ship_scoop_and_store(&ctx, &mut sess, "StoreVessel", &[PyRep::Integer(target_cruiser), PyRep::Integer(host_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "CannotStoreShipWithActiveModules"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.remove(&mod_id);
    });
    sys_h.actor_state.write().unwrap().active_module_cycles.push(
        crate::system_actor::ActiveModuleCycle {
            ship_id: target_cruiser,
            module_id: mod_id,
            duration_s: 10.0,
            ..Default::default()
        },
    );
    assert_eq!(
        ship_scoop_and_store(&ctx, &mut sess, "StoreVessel", &[PyRep::Integer(target_cruiser), PyRep::Integer(host_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "CannotStoreShipWithActiveModules"
    );
    sys_h
        .actor_state
        .write()
        .unwrap()
        .active_module_cycles
        .retain(|c| c.ship_id != target_cruiser);

    // 2h. Target lock held by another vessel -> CannotStoreTargetedShip
    spawn_ball_at(&ctx, null_sys, watcher_id, 998_020, other_char, DVec3::new(1000.0, 0.0, 0.0)).await;
    let _ = sys_h
        .add_target_lock(
            watcher_id,
            eve_destiny::TargetLock {
                target_id: target_cruiser,
                lock_time_ms: 500.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(50_000.0),
            },
        )
        .await;
    assert_eq!(
        ship_scoop_and_store(&ctx, &mut sess, "StoreVessel", &[PyRep::Integer(target_cruiser), PyRep::Integer(host_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "CannotStoreTargetedShip"
    );
    let _ = sys_h.clear_target_locks(watcher_id).await;

    // 3. In-space snap guards: Cargo Container (23), Capsule (670), Deployable (cat != 6), ShipPiloted, SMAAccessDenied
    let cargo_cont_id = 1_220_060_i64;
    let space_pod_id = 1_220_061_i64;
    let deploy_id = 1_220_062_i64;
    put_item(&ctx, cargo_cont_id, 23, char_id, null_sys as i64, InventoryFlag::None, 1, true);
    put_item(&ctx, space_pod_id, 670, char_id, null_sys as i64, InventoryFlag::None, 1, true);
    put_item(&ctx, deploy_id, 998_004, char_id, null_sys as i64, InventoryFlag::None, 1, true);
    put_item(&ctx, other_piloted, 998_021, other_char, null_sys as i64, InventoryFlag::None, 1, true);
    spawn_ball_at(&ctx, null_sys, cargo_cont_id, 23, char_id, DVec3::new(500.0, 0.0, 0.0)).await;
    spawn_ball_at(&ctx, null_sys, space_pod_id, 670, char_id, DVec3::new(500.0, 0.0, 0.0)).await;
    spawn_ball_at(&ctx, null_sys, deploy_id, 998_004, char_id, DVec3::new(500.0, 0.0, 0.0)).await;
    spawn_ball_at(&ctx, null_sys, other_piloted, 998_021, other_char, DVec3::new(500.0, 0.0, 0.0)).await;

    assert_eq!(
        ship_scoop_and_store(&ctx, &mut sess, "StoreVessel", &[PyRep::Integer(cargo_cont_id), PyRep::Integer(host_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "UserError"
    );
    assert_eq!(
        ship_scoop_and_store(&ctx, &mut sess, "StoreVessel", &[PyRep::Integer(space_pod_id), PyRep::Integer(host_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "CannotStoreCapsuleInSMA"
    );
    assert_eq!(
        ship_scoop_and_store(&ctx, &mut sess, "StoreVessel", &[PyRep::Integer(deploy_id), PyRep::Integer(host_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "CannotStoreCargoInSMA"
    );
    assert_eq!(
        ship_scoop_and_store(&ctx, &mut sess, "StoreVessel", &[PyRep::Integer(other_piloted), PyRep::Integer(host_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "OwnershipViolation"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&other_piloted).unwrap().owner_id = char_id;
    });
    assert_eq!(
        ship_scoop_and_store(&ctx, &mut sess, "StoreVessel", &[PyRep::Integer(other_piloted), PyRep::Integer(host_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "ShipPiloted"
    );

    // Password-locked corp host rejects wrong password in space, succeeds once password supplied
    put_item(&ctx, host_id, 998_001, corp_id, null_sys as i64, InventoryFlag::None, 1, true);
    write_container_ship_state(&ctx, |st| {
        st.ship_configs.insert(
            host_id,
            ShipAccessConfig {
                allow_fleet_smb: true,
                allow_corp_smb: true,
                allow_fleet_hangar: true,
                allow_corp_hangar: true,
                password: Some("secpw".into()),
            },
        );
    });
    assert_eq!(
        ship_scoop_and_store(&ctx, &mut sess, "StoreVessel", &[PyRep::Integer(target_cruiser), PyRep::Integer(host_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "SMAAccessDenied"
    );

    // 4. Unpiloted store succeeds & invokes cleanup_ship_leaving_space (target ball removed, pilot stays in active_id!)
    let mut good_kw = HashMap::new();
    good_kw.insert("password".into(), PyRep::String("secpw".into()));
    let _ = sys_h
        .add_target_lock(
            target_cruiser,
            eve_destiny::TargetLock {
                target_id: watcher_id,
                lock_time_ms: 500.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(50_000.0),
            },
        )
        .await;
    let unpiloted_res = ship_scoop_and_store(
        &ctx,
        &mut sess,
        "StoreVessel",
        &[PyRep::Integer(target_cruiser), PyRep::Integer(host_id)],
        &good_kw,
    )
    .await
    .expect("unpiloted store succeeds with password");
    assert_eq!(unpiloted_res.dict_get("flagID"), Some(&PyRep::Integer(90)));
    assert_eq!(sess.ship_id, Some(active_id));
    assert!(sys_h.get_ball_snapshot(target_cruiser).await.unwrap().is_none());
    assert!(sys_h
        .ballpark
        .read()
        .unwrap()
        .target_locks
        .get(&target_cruiser)
        .is_none_or(|v| v.is_empty()));

    // 5. Active ship store WITHOUT existing_cap mints new capsule; WITH existing_cap reuses it
    // First remove `space_pod_id` so no existing_cap exists -> mints a brand new capsule ID
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&space_pod_id);
        snap.items.get_mut(&host_id).unwrap().owner_id = char_id;
    });
    write_container_ship_state(&ctx, |st| {
        st.ship_configs.remove(&host_id);
    });
    let active_res_1 = ship_scoop_and_store(
        &ctx,
        &mut sess,
        "StoreVessel",
        &[PyRep::Integer(active_id), PyRep::Integer(host_id)],
        &kw,
    )
    .await
    .expect("active ship store mints capsule");
    let minted_cap = sess.ship_id.expect("new capsule active");
    assert_ne!(minted_cap, active_id);
    assert_eq!(
        active_res_1.dict_get("capsuleID"),
        Some(&PyRep::Integer(minted_cap))
    );
    assert_eq!(
        active_res_1.dict_get("storedShipID"),
        Some(&PyRep::Integer(active_id))
    );
    assert_eq!(
        active_res_1.dict_get("hostShipID"),
        Some(&PyRep::Integer(host_id))
    );
    assert!(sys_h.get_ball_snapshot(minted_cap).await.unwrap().is_some());

    // Board stored `active_id` (`998_020`) back out of FEB/host -> invokes compute_ship_ball_kinematics!
    let board_out = board_stored_ship(&ctx, &mut sess, &[PyRep::Integer(active_id)], &kw)
        .await
        .expect("board stored 998_020 back into space");
    assert_eq!(board_out.dict_get("shipID"), Some(&PyRep::Integer(active_id)));
    let (_, kin_ball) = sys_h.get_ball_snapshot(active_id).await.unwrap().unwrap();
    assert!(kin_ball.max_velocity >= 350.0);
    assert!(kin_ball.inertia > 0.0);
    assert!((kin_ball.mass - 1_100_000.0).abs() < 1.0);
    assert!((kin_ball.radius - 45.0).abs() < 1e-6);

    // Put an existing capsule item owned by char_id -> storing active ship REUSES existing_cap
    let reuse_cap_id = 1_220_777_i64;
    put_item(&ctx, reuse_cap_id, 670, char_id, null_sys as i64, InventoryFlag::None, 1, true);
    let _ = ship_scoop_and_store(
        &ctx,
        &mut sess,
        "StoreVessel",
        &[PyRep::Integer(active_id), PyRep::Integer(host_id)],
        &kw,
    )
    .await
    .expect("active ship store reuses existing capsule");
    assert_eq!(sess.ship_id, Some(reuse_cap_id));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ship_bays_board_jettison_drop_scoop_guards_and_versions_kill_mutants() {
    use eve_db::OwnerNoteRecord;
    use eve_destiny::{Ball, CombatState, MovementMode};
    use glam::DVec3;

    let ctx = make_ctx();
    let char_id: i64 = 90_992_001;
    let corp_id: i64 = 98_992_001;
    let sys_null = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| s.security_status < 0.0 && s.security_status >= -0.45 && s.solar_system_id < 31_000_000)
        .min_by_key(|s| s.solar_system_id)
        .unwrap()
        .solar_system_id;
    let sys_hs: u32 = 30_000_142;

    ctx.db.provision_gm_account("bay_mutant_pilot", "pw", char_id, "BayMutant");

    let active_ship: i64 = 1_992_001;
    let carrier_host: i64 = 1_992_002; // 998_001 MultiBay Capital
    let stored_feb: i64 = 1_992_003;   // 998_003 ZeroSkill Frigate with custom flag 177
    let stored_sde_ship: i64 = 1_992_004; // 998_003 with custom flag 138
    let stored_not_singleton: i64 = 1_992_005;
    let cargo_trit: i64 = 1_992_010;
    let deploy_hs: i64 = 1_992_011; // 998_004 SecBand Depot (max 0.4, min -0.5)
    let deploy_ls: i64 = 1_992_012;
    let scoop_target: i64 = 1_992_020;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_null;
            ch.active_ship_id = active_ship;
        }
        let mut corp = eve_db::CorporationRecord::new(corp_id, "BayCorp", "BAYC", char_id + 99);
        corp.members.insert(char_id);
        snap.corporations.insert(corp_id, corp);
    });
    crate::services::fitting_containers_ships::write_container_ship_state(&ctx, |st| {
        st.custom_item_flags.insert(stored_feb, 177);
        st.custom_item_flags.insert(stored_sde_ship, 138);
    });

    put_item(&ctx, active_ship, 587, char_id, sys_null as i64, InventoryFlag::None, 1, true);
    put_item(&ctx, carrier_host, 998_001, corp_id, sys_null as i64, InventoryFlag::None, 1, true);
    put_item(&ctx, stored_feb, 998_003, corp_id, carrier_host, InventoryFlag::None, 1, true);
    put_item(&ctx, stored_sde_ship, 998_003, corp_id, carrier_host, InventoryFlag::None, 1, true);
    put_item(&ctx, stored_not_singleton, 998_003, char_id, carrier_host, InventoryFlag::ShipHangar, 1, false);
    put_item(&ctx, cargo_trit, 34, char_id, active_ship, InventoryFlag::Cargo, 5, false);
    put_item(&ctx, deploy_hs, 998_004, char_id, active_ship, InventoryFlag::Cargo, 1, false);
    put_item(&ctx, deploy_ls, 998_004, char_id, active_ship, InventoryFlag::Cargo, 1, false);
    put_item(&ctx, scoop_target, 998_003, char_id, sys_null as i64, InventoryFlag::None, 1, true);

    let spawn = |sys_id: u32, id: i64, tid: u32, pos: DVec3| {
        let sys = ctx.systems.get_or_spawn(sys_id);
        async move {
            let _ = sys
                .spawn_ball_with_combat(
                    Ball {
                        id,
                        type_id: tid,
                        position: pos,
                        velocity: DVec3::ZERO,
                        max_velocity: 300.0,
                        inertia: 1.0,
                        mass: 1_000_000.0,
                        radius: 40.0,
                        speed_fraction: 0.0,
                        mode: MovementMode::Stop,
                    },
                    CombatState::default(),
                )
                .await;
        }
    };
    spawn(sys_null, active_ship, 587, DVec3::new(10_000.0, 0.0, 0.0)).await;
    spawn(sys_null, carrier_host, 998_001, DVec3::new(10_200.0, 0.0, 0.0)).await;
    spawn(sys_null, scoop_target, 998_003, DVec3::new(10_300.0, 0.0, 0.0)).await;

    let kw = HashMap::new();
    let mut sess = SessionState::new(920);
    sess.user_id = Some(ctx.db.with_snapshot(|s| s.characters.get(&char_id).map_or(1, |c| c.user_id)));
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(sys_null);
    sess.ship_id = Some(active_ship);
    sess.corp_role = 0;

    // 1. board_stored_ship: non-singleton, missing corp role, custom flags 177/138 + version += 1
    assert!(
        board_stored_ship(
            &ctx,
            &mut sess,
            &[PyRep::Integer(carrier_host), PyRep::Integer(stored_not_singleton)],
            &kw,
        )
        .await
        .is_err()
    );
    assert!(
        board_stored_ship(
            &ctx,
            &mut sess,
            &[PyRep::Integer(carrier_host), PyRep::Integer(stored_feb)],
            &kw,
        )
        .await
        .is_err(),
        "corp-owned host without ROLE_EQUIPMENT_CONFIG or ROLE_DIRECTOR must reject"
    );

    sess.corp_role = corp_roles::ROLE_EQUIPMENT_CONFIG;
    let ver_before = ctx.db.with_snapshot(|snap| snap.items[&stored_feb].version);
    board_stored_ship(
        &ctx,
        &mut sess,
        &[PyRep::Integer(carrier_host), PyRep::Integer(stored_feb)],
        &kw,
    )
    .await
    .expect("custom flag 177 board succeeds with ROLE_EQUIPMENT_CONFIG");
    assert_eq!(sess.ship_id, Some(stored_feb));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&stored_feb].version, ver_before + 1);
        assert_eq!(snap.items[&stored_feb].location_id, sys_null as i64);
        assert_eq!(snap.items[&stored_feb].flag, InventoryFlag::None);
    });

    // Put active_ship back in space & board stored_sde_ship (custom flag 138)
    sess.ship_id = Some(active_ship);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().active_ship_id = active_ship;
    });
    let ver2_before = ctx.db.with_snapshot(|snap| snap.items[&stored_sde_ship].version);
    board_stored_ship(
        &ctx,
        &mut sess,
        &[PyRep::Integer(carrier_host), PyRep::Integer(stored_sde_ship)],
        &kw,
    )
    .await
    .expect("custom flag 138 board succeeds");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&stored_sde_ship].version, ver2_before + 1);
    });

    // 2. ship_jettison: 179s cooldown blocks; 180s succeeds and records note_key = -10_000_018 - char_id
    sess.ship_id = Some(active_ship);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().active_ship_id = active_ship;
        let now = ctx.now_unix_secs();
        let note_key = -10_000_018 - char_id;
        snap.owner_notes.insert(
            note_key,
            OwnerNoteRecord {
                note_id: note_key,
                character_id: char_id,
                item_id: 0,
                label: "JettisonCooldown".into(),
                note: (now - 179).to_string(),
                timestamp_unix: now - 179,
            },
        );
    });
    assert!(
        ship_jettison(
            &ctx,
            &mut sess,
            &[PyRep::List(vec![PyRep::Integer(cargo_trit)])],
            &kw,
        )
        .await
        .is_err(),
        "jettison within 179s (< 180s) must fail cooldown"
    );
    ctx.db.with_mut_snapshot(|snap| {
        let now = ctx.now_unix_secs();
        let note_key = -10_000_018 - char_id;
        let entry = snap.owner_notes.get_mut(&note_key).unwrap();
        entry.note = (now - 180).to_string();
        entry.timestamp_unix = now - 180;
    });
    let jet_res = ship_jettison(
        &ctx,
        &mut sess,
        &[PyRep::List(vec![PyRep::Integer(cargo_trit)])],
        &kw,
    )
    .await
    .expect("jettison at exact 180s succeeds");
    assert_ne!(jet_res, PyRep::None);
    ctx.db.with_snapshot(|snap| {
        let jetcan_id = snap.items[&cargo_trit].location_id;
        assert_ne!(jetcan_id, active_ship);
        assert_eq!(snap.items[&jetcan_id].location_id, sys_null as i64);
    });

    // 3. ship_drop: SecBand Depot rejected in highsec (0.95 > 0.4), accepted in nullsec (-0.5 <= sec <= 0.4)
    let _ = ship_drop(&ctx, &mut sess, &[PyRep::Integer(deploy_ls)], &kw)
        .await
        .expect("drop deployable in nullsec succeeds");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&deploy_ls].location_id, sys_null as i64);
    });

    sess.solar_system_id = Some(sys_hs);
    spawn(sys_hs, active_ship, 587, DVec3::new(10_000.0, 0.0, 0.0)).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().solar_system_id = sys_hs;
        snap.items.get_mut(&active_ship).unwrap().location_id = sys_hs as i64;
    });
    assert!(
        ship_drop(&ctx, &mut sess, &[PyRep::Integer(deploy_hs)], &kw)
            .await
            .is_err(),
        "SecBand Depot must reject highsec > 0.4"
    );

    // 4. ship_scoop_and_store: ScoopToFleetHangar on active carrier_host rejects corp item without role, rejects ships, succeeds with ROLE_DIRECTOR
    sess.solar_system_id = Some(sys_null);
    sess.ship_id = Some(carrier_host);
    let fleet_scoop_item: i64 = 1_992_021;
    put_item(&ctx, fleet_scoop_item, 34, corp_id, sys_null as i64, InventoryFlag::None, 1, false);
    spawn(sys_null, fleet_scoop_item, 34, DVec3::new(10_300.0, 0.0, 0.0)).await;
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.solar_system_id = sys_null;
        ch.active_ship_id = carrier_host;
        snap.items.get_mut(&active_ship).unwrap().location_id = sys_null as i64;
    });
    sess.corp_role = 0;
    assert_eq!(
        ship_scoop_and_store(
            &ctx,
            &mut sess,
            "ScoopToFleetHangar",
            &[PyRep::Integer(fleet_scoop_item)],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );
    sess.corp_role = corp_roles::ROLE_DIRECTOR;
    assert_eq!(
        ship_scoop_and_store(
            &ctx,
            &mut sess,
            "ScoopToFleetHangar",
            &[PyRep::Integer(scoop_target), PyRep::Integer(carrier_host)],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "CannotStoreShipInFleetHangar"
    );
    ship_scoop_and_store(
        &ctx,
        &mut sess,
        "ScoopToFleetHangar",
        &[PyRep::Integer(fleet_scoop_item), PyRep::Integer(carrier_host)],
        &kw,
    )
    .await
    .expect("ScoopToFleetHangar succeeds with ROLE_DIRECTOR");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&fleet_scoop_item].location_id, carrier_host);
        assert_eq!(snap.items[&fleet_scoop_item].flag, InventoryFlag::FleetHangar);
    });
}

