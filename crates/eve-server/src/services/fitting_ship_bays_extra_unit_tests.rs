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
