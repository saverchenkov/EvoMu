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
    corp_roles, InventoryFlag, InventoryItemRecord, PersistedSpaceObjectMeta, VirtualServerClock,
    WorldRepository,
};
use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn custom_inv_type(
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

fn make_ctx() -> ServerContext {
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

fn put_item(
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

async fn spawn_ball_at(
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

    // Host SMA password denies access without password, then allows with password + ROLE_EQUIPMENT_CONFIG
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
async fn ship_jettison_and_ship_drop_validation_and_phase_anchor_branches() {
    let ctx = make_ctx();
    let char_id = 90_000_001;
    let corp_id = 98_000_001;
    let low_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status > 0.0 && s.security_status < 0.45 && s.solar_system_id < 31_000_000)
        .map(|s| s.solar_system_id)
        .unwrap();
    let jita_sys = 30_000_142;
    let ship_id = 1_200_001;
    let kw = HashMap::new();

    spawn_ball_at(&ctx, low_sys, ship_id, 998_001, char_id, DVec3::new(500_000.0, 0.0, 0.0)).await;
    put_item(&ctx, ship_id, 998_001, char_id, low_sys as i64, InventoryFlag::None, 1, true);

    let (sma_ship_id, implant_id, cargo_ship_id, cont_id, huge_ore_id) =
        (1_200_010, 1_200_011, 1_200_012, 1_200_013, 1_200_014);
    let cont_tid = ctx.sde.types.values().find(|t| t.group_id == 340).unwrap().type_id;
    put_item(&ctx, sma_ship_id, 587, char_id, ship_id, InventoryFlag::ShipHangar, 1, true);
    put_item(&ctx, implant_id, 34, char_id, ship_id, InventoryFlag::Implant, 1, false);
    put_item(&ctx, cargo_ship_id, 587, char_id, ship_id, InventoryFlag::Cargo, 1, false);
    put_item(&ctx, cont_id, cont_tid, char_id, ship_id, InventoryFlag::Cargo, 1, true);
    put_item(&ctx, huge_ore_id, 1230, char_id, ship_id, InventoryFlag::Cargo, 10_000_000, false);

    let mut sess = SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        solar_system_id: Some(low_sys),
        ship_id: Some(ship_id),
        ..Default::default()
    };

    assert_eq!(
        ship_jettison(&ctx, &mut sess, &[], &kw).await.unwrap_err().code,
        "InvalidArgument"
    );
    for (id, expected_code) in [
        (999_999_999, "ItemNotFound"),
        (sma_ship_id, "CannotJettisonStoredShip"),
        (implant_id, "CannotJettisonItem"),
        (cargo_ship_id, "CannotJettisonShip"),
        (cont_id, "CannotPutContainerInContainer"),
        (huge_ore_id, "CapacityExceeded"),
    ] {
        let mut kw_id = HashMap::new();
        kw_id.insert("itemID".into(), PyRep::Integer(id));
        assert_eq!(
            ship_jettison(&ctx, &mut sess, &[], &kw_id).await.unwrap_err().code,
            expected_code
        );
    }

    // ship_drop guards: docked, foreign owner, fitted, ShipHangar, Zarzakh, security band, Phase Anchor
    let mut docked = sess.clone();
    docked.station_id = Some(60_003_760);
    assert_eq!(
        ship_drop(&ctx, &mut docked, &[PyRep::Integer(cont_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "CannotDropWhileDocked"
    );
    assert_eq!(
        ship_drop(
            &ctx,
            &mut sess,
            &[PyRep::Integer(cont_id), PyRep::Integer(99_888_777)],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "UserError"
    );
    assert_eq!(
        ship_drop(&ctx, &mut sess, &[PyRep::Integer(sma_ship_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "CannotDropStoredShip"
    );

    let (sec_depot_id, phase_anchor_id) = (1_200_020, 1_200_021);
    put_item(&ctx, sec_depot_id, 998_004, char_id, ship_id, InventoryFlag::Cargo, 1, true);
    put_item(
        &ctx,
        phase_anchor_id,
        crate::services::mining_phased_autotarget::TYPE_MOBILE_PHASE_ANCHOR,
        char_id,
        ship_id,
        InventoryFlag::Cargo,
        1,
        true,
    );

    // Zarzakh (30_100_000) and High-sec (30_000_142) drop guards
    let mut zar_sess = sess.clone();
    zar_sess.solar_system_id = Some(30_100_000);
    assert_eq!(
        ship_drop(&ctx, &mut zar_sess, &[PyRep::Integer(sec_depot_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "AnchoringDisallowedInZarzakh"
    );

    let mut hs_sess = sess.clone();
    hs_sess.solar_system_id = Some(jita_sys);
    assert_eq!(
        ship_drop(&ctx, &mut hs_sess, &[PyRep::Integer(phase_anchor_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "CannotDeployInHighSec"
    );
    assert_eq!(
        ship_drop(&ctx, &mut hs_sess, &[PyRep::Integer(sec_depot_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "UserError"
    );

    // In low-sec, full-stack drop of Mobile Phase Anchor succeeds
    let drop_ok = ship_drop(&ctx, &mut sess, &[PyRep::Integer(phase_anchor_id)], &kw)
        .await
        .expect("drop phase anchor in low-sec");
    assert!(matches!(drop_ok, PyRep::Dict(_)));
}

#[tokio::test]
async fn ship_scoop_and_store_feb_fleet_hangar_infra_depot_and_cargo_holds() {
    let ctx = make_ctx();
    let char_id = 90_000_001;
    let corp_id = 98_000_001;
    let station_id = 60_003_760;
    let sys_id = 30_000_142;
    let kw = HashMap::new();

    let (pod_id, feb_host_id, frig1_id, frig2_id, hauler_id) =
        (1_300_001, 1_300_002, 1_300_003, 1_300_004, 1_300_005);
    put_item(&ctx, pod_id, 670, char_id, station_id, InventoryFlag::Hangar, 1, true);
    put_item(&ctx, feb_host_id, 998_002, char_id, station_id, InventoryFlag::Hangar, 1, true);
    put_item(&ctx, frig1_id, 998_003, char_id, station_id, InventoryFlag::Hangar, 1, true);
    put_item(&ctx, frig2_id, 998_003, char_id, station_id, InventoryFlag::Hangar, 1, true);
    put_item(&ctx, hauler_id, 648, char_id, station_id, InventoryFlag::Hangar, 1, true);

    let mut docked_sess = SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        station_id: Some(station_id),
        solar_system_id: Some(sys_id),
        ship_id: Some(pod_id),
        ..Default::default()
    };

    // Docked StoreVessel into FEB-only host: non-frigate rejected, first frigate stored in FrigateEscapeBay, second exceeds capacity
    assert_eq!(
        ship_scoop_and_store(
            &ctx,
            &mut docked_sess,
            "StoreVessel",
            &[PyRep::Integer(hauler_id), PyRep::Integer(feb_host_id)],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "InvalidFrigateGroup"
    );
    let feb_ok = ship_scoop_and_store(
        &ctx,
        &mut docked_sess,
        "StoreVessel",
        &[PyRep::Integer(frig1_id), PyRep::Integer(feb_host_id)],
        &kw,
    )
    .await
    .expect("store frigate in FEB");
    assert_eq!(
        feb_ok.dict_get("flagID"),
        Some(&PyRep::Integer(InventoryFlag::FrigateEscapeBay as i64))
    );
    assert_eq!(
        ship_scoop_and_store(
            &ctx,
            &mut docked_sess,
            "StoreVessel",
            &[PyRep::Integer(frig2_id), PyRep::Integer(feb_host_id)],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "CapacityExceeded"
    );

    // Scoop while docked -> MustBeInSpace
    assert_eq!(
        ship_scoop_and_store(&ctx, &mut docked_sess, "Scoop", &[PyRep::Integer(frig2_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "MustBeInSpace"
    );

    // Move to space with MultiBay Capital (998_001)
    let (cap_id, active_frig_id, unpiloted_frig_id, depot_id, ore_can_id, anchored_id) =
        (1_300_010, 1_300_011, 1_300_012, 1_300_013, 1_300_014, 1_300_015);
    let sec_cont_tid = ctx.sde.types.values().find(|t| t.group_id == 340).unwrap().type_id;
    put_item(&ctx, cap_id, 998_001, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    put_item(&ctx, active_frig_id, 998_003, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    put_item(&ctx, unpiloted_frig_id, 998_003, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    put_item(&ctx, depot_id, 33_474, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    put_item(&ctx, ore_can_id, sec_cont_tid, 0, sys_id as i64, InventoryFlag::None, 1, true);
    put_item(&ctx, anchored_id, 33_474, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    ctx.db.with_mut_snapshot(|snap| {
        snap.sp6_lifecycle.space_object_meta.insert(
            anchored_id,
            PersistedSpaceObjectMeta {
                anchor_completed: true,
                ..Default::default()
            },
        );
    });

    for (id, tid) in [
        (cap_id, 998_001),
        (active_frig_id, 998_003),
        (unpiloted_frig_id, 998_003),
        (depot_id, 33_474),
        (ore_can_id, sec_cont_tid),
        (anchored_id, 33_474),
    ] {
        spawn_ball_at(&ctx, sys_id, id, tid, char_id, DVec3::new(100.0, 0.0, 0.0)).await;
    }

    // 1. In-space StoreVessel on unpiloted_frig_id while piloting active_frig_id
    let mut space_sess = SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        solar_system_id: Some(sys_id),
        ship_id: Some(active_frig_id),
        ..Default::default()
    };
    let unpiloted_res = ship_scoop_and_store(
        &ctx,
        &mut space_sess,
        "StoreVessel",
        &[PyRep::Integer(unpiloted_frig_id), PyRep::Integer(cap_id)],
        &kw,
    )
    .await
    .expect("store unpiloted ship in host bay");
    assert_eq!(unpiloted_res.dict_get("storedShipID"), Some(&PyRep::Integer(unpiloted_frig_id)));

    // 2. Pilot cap_id and test ScoopToMobileDepotHold, ScoopToInfrastructureHold, ScoopToFleetHangar, and Scoop
    space_sess.ship_id = Some(cap_id);
    assert_eq!(
        ship_scoop_and_store(&ctx, &mut space_sess, "Scoop", &[PyRep::Integer(anchored_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "CannotScoopAnchored"
    );

    let depot_scoop = ship_scoop_and_store(&ctx, &mut space_sess, "ScoopToMobileDepotHold", &[PyRep::Integer(depot_id)], &kw)
        .await
        .expect("scoop mobile depot to mobile depot hold");
    assert_eq!(depot_scoop.dict_get("flagID"), Some(&PyRep::Integer(179)));

    // Put depot back in space and scoop to InfrastructureHold (185)
    put_item(&ctx, depot_id, 33_474, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    spawn_ball_at(&ctx, sys_id, depot_id, 33_474, char_id, DVec3::new(100.0, 0.0, 0.0)).await;
    let infra_scoop = ship_scoop_and_store(&ctx, &mut space_sess, "ScoopToInfrastructureHold", &[PyRep::Integer(depot_id)], &kw)
        .await
        .expect("scoop deployable to infrastructure hold");
    assert_eq!(infra_scoop.dict_get("flagID"), Some(&PyRep::Integer(185)));

    // Scoop abandoned container to FleetHangar (155) -> transfers ownership to char_id
    let fh_scoop = ship_scoop_and_store(&ctx, &mut space_sess, "ScoopToFleetHangar", &[PyRep::Integer(ore_can_id)], &kw)
        .await
        .expect("scoop container to fleet hangar");
    assert_eq!(fh_scoop.dict_get("flagID"), Some(&PyRep::Integer(155)));
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&ore_can_id].owner_id), char_id);

    // Put container back in space and Scoop to standard Cargo (5)
    put_item(&ctx, ore_can_id, sec_cont_tid, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    spawn_ball_at(&ctx, sys_id, ore_can_id, sec_cont_tid, char_id, DVec3::new(100.0, 0.0, 0.0)).await;
    let cargo_scoop = ship_scoop_and_store(&ctx, &mut space_sess, "Scoop", &[PyRep::Integer(ore_can_id)], &kw)
        .await
        .expect("scoop container to cargo");
    assert_eq!(cargo_scoop.dict_get("flagID"), Some(&PyRep::Integer(5)));
}
