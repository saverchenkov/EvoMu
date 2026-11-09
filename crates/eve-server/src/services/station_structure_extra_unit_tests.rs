//! Extra unit and mutation-killing tests for `services::station_structure`
//! covering station/structure lookups, undock/dock validation & Upwell structure docking,
//! onlineModules undock flag, crimewatch/warp/scramble/perimeter dock guards,
//! and `posMgr.AnchorStructure` restriction & bubble/container anchoring paths.

#![forbid(unsafe_code)]

use super::station_structure::*;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::{
    clone_char_with, test_ctx_with_clock,
};
use eve_db::{CorporationRecord, InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_destiny::MovementMode;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use glam::DVec3;
use std::collections::HashMap;

pub(super) fn make_sess(
    char_id: i64,
    corp_id: i64,
    sys_id: Option<u32>,
    station_id: Option<i64>,
    structure_id: Option<i64>,
    ship_id: Option<i64>,
) -> SessionState {
    let mut s = SessionState::new(1);
    s.user_id = Some(char_id as u64);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = sys_id;
    s.station_id = station_id;
    s.structure_id = structure_id;
    s.ship_id = ship_id;
    s
}

#[tokio::test]
async fn station_lookup_guests_and_standing_error_branches() {
    let ctx = test_ctx_with_clock(1_800_000_000);
    let char_id = 920_101_i64;
    let corp_id = 98_200_101_i64;
    let jita_sys = 30_000_142_u32;
    let st_id = 1_000_000_920_101_i64;
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        let ch = clone_char_with(snap, char_id, "LookupPilot", corp_id, jita_sys, 920_102);
        snap.characters.insert(char_id, ch);
        snap.structures.insert(
            st_id,
            UpwellStructureRecord {
                structure_id: st_id,
                type_id: 35_832,
                owner_corp_id: corp_id,
                solar_system_id: jita_sys,
                name: "Test Astrahus".into(),
                position: [50_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 1.0,
                armor_hp: 1.0,
                hull_hp: 1.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });

    let mut sess = make_sess(char_id, corp_id, None, None, None, None);

    // GetSolarSystem: no arg & no session solar_system_id -> NoSolarSystem
    assert_eq!(
        handle(&ctx, &mut sess, "stationSvc", "GetSolarSystem", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NoSolarSystem"
    );

    // GetSolarSystem: unknown solar system ID -> SolarSystemNotFound
    assert_eq!(
        handle(&ctx, &mut sess, "station", "GetSolarSystem", &[PyRep::Integer(39_999_999)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "SolarSystemNotFound"
    );

    // GetSolarSystem: Upwell structure ID >= 60_000_000 resolves to solarSystemID integer
    assert_eq!(
        handle(&ctx, &mut sess, "stationSvc", "GetSolarSystem", &[PyRep::Integer(st_id)], &kw)
            .await
            .unwrap()
            .unwrap(),
        PyRep::Integer(jita_sys as i64)
    );

    // GetSolarSystem: fallback to session.solar_system_id
    sess.solar_system_id = Some(jita_sys);
    assert!(handle(&ctx, &mut sess, "station", "GetSolarSystem", &[], &kw)
        .await
        .unwrap()
        .is_ok());

    // GetStation & GetStationItemBits: MissingStationId and StationNotFound
    assert_eq!(
        handle(&ctx, &mut sess, "stationSvc", "GetStation", &[], &kw).await.unwrap().unwrap_err().code,
        "MissingStationId"
    );
    assert_eq!(
        handle(&ctx, &mut sess, "stationSvc", "GetStationItemBits", &[], &kw).await.unwrap().unwrap_err().code,
        "MissingStationId"
    );
    assert_eq!(
        handle(&ctx, &mut sess, "station", "GetStation", &[PyRep::Integer(69_999_999)], &kw).await.unwrap().unwrap_err().code,
        "StationNotFound"
    );
    assert_eq!(
        handle(&ctx, &mut sess, "station", "GetStationItemBits", &[PyRep::Integer(69_999_999)], &kw).await.unwrap().unwrap_err().code,
        "StationNotFound"
    );

    // station.GetGuests when not docked -> NotDockedInStation
    assert_eq!(
        handle(&ctx, &mut sess, "station", "GetGuests", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NotDockedInStation"
    );

    // DoStandingCheckForStationService when not docked -> NotDocked
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "corpStationMgr",
            "DoStandingCheckForStationService",
            &[PyRep::Integer(5)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "NotDocked"
    );
}

#[tokio::test]
async fn undock_and_dock_upwell_structure_online_modules_and_guard_branches() {
    let ctx = test_ctx_with_clock(1_800_000_000);
    let char_id = 920_201_i64;
    let corp_id = 98_200_201_i64;
    let jita_sys = 30_000_142_u32;
    let jita_station = 60_003_760_i64;
    let ship_id = 920_202_i64;
    let mod_id = 920_203_i64;
    let st_id = 1_000_000_920_201_i64;
    let now = ctx.now_unix();
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = clone_char_with(snap, char_id, "StructurePilot", corp_id, jita_sys, ship_id);
        ch.station_id = None;
        ch.structure_id = Some(st_id);
        snap.characters.insert(char_id, ch);
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                ..Default::default()
            },
        );
        snap.structures.insert(
            st_id,
            UpwellStructureRecord {
                structure_id: st_id,
                type_id: 35_832,
                owner_corp_id: corp_id,
                solar_system_id: jita_sys,
                name: "Home Citadel".into(),
                position: [50_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                shield_hp: 100_000.0,
                armor_hp: 100_000.0,
                hull_hp: 100_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: st_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            mod_id,
            InventoryItemRecord {
                item_id: mod_id,
                type_id: 438,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mut sess = make_sess(char_id, corp_id, Some(jita_sys), None, Some(st_id), Some(ship_id));

    // 1. Podding session-change timer blocks Undock -> SessionChangeInProgress
    crate::services::space_destiny::record_session_change_timer(
        &ctx, char_id, now, jita_sys, 1, "podding",
    );
    assert_eq!(
        handle(&ctx, &mut sess, "structureDocking", "Undock", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "SessionChangeInProgress"
    );
    crate::services::space_destiny::clear_session_change_timer(&ctx, char_id);

    // 2. Undock from Upwell structure with onlineModules=true onlines fitted module
    let mut online_kw = HashMap::new();
    online_kw.insert("onlineModules".into(), PyRep::Bool(true));
    let undock_res = handle(&ctx, &mut sess, "structureDocking", "Undock", &[], &online_kw)
        .await
        .unwrap()
        .expect("undock from structure");
    assert_eq!(undock_res.dict_get("shipID"), Some(&PyRep::Integer(ship_id)));
    assert_eq!(sess.structure_id, None);
    assert!(ctx.db.with_snapshot(|s| s.module_online_states.get(&mod_id).copied().unwrap_or(false)));

    // Calling Undock again while already in space -> UndockFailed
    assert_eq!(
        handle(&ctx, &mut sess, "ship", "Undock", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "UndockFailed"
    );

    // 3. Dock validation guards
    let mut already_docked = sess.clone();
    already_docked.station_id = Some(jita_station);
    assert_eq!(
        handle(&ctx, &mut already_docked, "stationSvc", "Dock", &[PyRep::Integer(jita_station)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "AlreadyDocked"
    );

    let mut no_ship = sess.clone();
    no_ship.ship_id = None;
    assert_eq!(
        handle(&ctx, &mut no_ship, "stationSvc", "Dock", &[PyRep::Integer(jita_station)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NoActiveShip"
    );

    assert_eq!(
        handle(&ctx, &mut sess, "stationSvc", "Dock", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "MissingLocationId"
    );

    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "stationSvc",
            "Dock",
            &[PyRep::Integer(jita_station), PyRep::Integer(ship_id + 99)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidShipId"
    );

    let mut no_sys = sess.clone();
    no_sys.solar_system_id = None;
    assert_eq!(
        handle(&ctx, &mut no_sys, "stationSvc", "Dock", &[PyRep::Integer(jita_station)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NoSolarSystem"
    );

    // Weapons timer & Criminal timer in high-sec
    ctx.crimewatch
        .write()
        .unwrap()
        .states
        .entry(char_id)
        .or_default()
        .weapon_timer_until_unix = now + 60;
    assert_eq!(
        handle(&ctx, &mut sess, "stationSvc", "Dock", &[PyRep::Integer(jita_station)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "WeaponsTimerActive"
    );
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        let st = cw.states.entry(char_id).or_default();
        st.weapon_timer_until_unix = 0;
        st.criminal_flag_until_unix = now + 60;
    }
    assert_eq!(
        handle(&ctx, &mut sess, "stationSvc", "Dock", &[PyRep::Integer(jita_station)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CantDockGlobalCriminalFlag"
    );
    ctx.crimewatch.write().unwrap().states.clear();

    // Station in wrong solar system -> DockTargetWrongSystem
    let wrong_station = ctx
        .sde
        .stations
        .values()
        .find(|s| s.solar_system_id != jita_sys)
        .unwrap()
        .station_id;
    assert_eq!(
        handle(&ctx, &mut sess, "stationSvc", "Dock", &[PyRep::Integer(wrong_station)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "DockTargetWrongSystem"
    );

    // Ship in Warp -> ShipInWarp
    let sys_h = ctx.systems.get_or_spawn(jita_sys);
    let _ = sys_h
        .set_movement_mode(
            ship_id,
            MovementMode::new_warp(DVec3::ZERO, DVec3::new(1_000_000.0, 0.0, 0.0), 3.0),
        )
        .await;
    assert_eq!(
        handle(&ctx, &mut sess, "structureDocking", "Dock", &[PyRep::Integer(st_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ShipInWarp"
    );
    let _ = sys_h.set_movement_mode(ship_id, MovementMode::Stop).await;

    // Warp scrambled at Upwell structure -> ShipWarpScrambled
    let _ = sys_h.set_warp_scramble_strength(ship_id, 1).await;
    assert_eq!(
        handle(&ctx, &mut sess, "structureDocking", "Dock", &[PyRep::Integer(st_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ShipWarpScrambled"
    );
    let _ = sys_h.set_warp_scramble_strength(ship_id, 0).await;

    // Outside docking perimeter (including 10 km with non-empty session.address) -> OutsideDockingPerimeter
    sess.address = "tcp:127.0.0.1:44444".to_string();
    let (_, st_ball) = sys_h.get_ball_snapshot(st_id).await.unwrap().unwrap();
    let _ = sys_h
        .set_ball_position(ship_id, st_ball.position + DVec3::new(st_ball.radius + 10_000.0, 0.0, 0.0))
        .await;
    assert_eq!(
        handle(&ctx, &mut sess, "structureDocking", "Dock", &[PyRep::Integer(st_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "OutsideDockingPerimeter"
    );

    // Move within docking perimeter of Upwell structure & mark mod_id active -> docks & deactivates mod_id
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.insert(mod_id);
    });
    let _ = sys_h
        .set_ball_position(ship_id, DVec3::new(50_500.0, 0.0, 0.0))
        .await;
    let dock_st_res = handle(&ctx, &mut sess, "structureDocking", "Dock", &[PyRep::Integer(st_id)], &kw)
        .await
        .unwrap()
        .expect("dock at Upwell structure");
    assert_eq!(dock_st_res.dict_get("isStructure"), Some(&PyRep::Bool(true)));
    assert_eq!(sess.structure_id, Some(st_id));
    assert!(!ctx.db.with_snapshot(|s| s.module_active_states.contains(&mod_id)));
}

#[tokio::test]
async fn station_dock_at_warp_destination_succeeds_while_mid_warp_still_rejects() {
    let ctx = test_ctx_with_clock(1_800_000_000);
    let char_id = 920_301_i64;
    let corp_id = 98_200_301_i64;
    let jita_sys = 30_000_142_u32;
    let jita_station = 60_003_760_i64;
    let ship_id = 920_302_i64;
    let kw = HashMap::new();

    let st_pos = ctx.sde.stations.get(&jita_station).unwrap().position;
    let st_radius = ctx
        .sde
        .types
        .get(&ctx.sde.stations.get(&jita_station).unwrap().station_type_id)
        .map(|t| t.radius)
        .unwrap_or(0.0);

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = clone_char_with(snap, char_id, "WarpDockPilot", corp_id, jita_sys, ship_id);
        ch.station_id = None;
        ch.structure_id = None;
        snap.characters.insert(char_id, ch);
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: jita_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let sys_h = ctx.systems.get_or_spawn(jita_sys);
    let eff_radius = sys_h
        .get_ball_snapshot(jita_station)
        .await
        .unwrap()
        .map(|(_, b)| b.radius)
        .filter(|&r| r > 0.0)
        .unwrap_or(st_radius);
    let origin = st_pos + DVec3::new(1_000_000_000.0, 0.0, 0.0);
    let landing = st_pos + DVec3::new(eff_radius, 0.0, 0.0);
    // 1. Decelerating with > 1.0s remaining (5,000m from landing at 15.0 AU/s) -> ShipInWarp even with non-empty session.address
    let mut warp_mode = MovementMode::new_warp(origin, landing, 15.0);
    if let MovementMode::Warp { ref mut phase, .. } = warp_mode {
        *phase = eve_destiny::WarpPhase::Decelerating;
    }
    sys_h
        .spawn_ball_with_meta(
            eve_destiny::Ball {
                id: ship_id,
                type_id: 587,
                position: landing + DVec3::new(5_000.0, 0.0, 0.0),
                velocity: DVec3::new(-25_000.0, 0.0, 0.0),
                max_velocity: 350.0,
                inertia: 2.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: warp_mode,
            },
            eve_destiny::CombatState::default(),
            eve_destiny::SlimItemMeta::default(),
        )
        .await
        .unwrap();
    ctx.active_warps.lock().unwrap().insert(ship_id, char_id);

    let mut sess = make_sess(char_id, corp_id, Some(jita_sys), None, None, Some(ship_id));
    sess.address = "tcp:127.0.0.1:44446".to_string();
    assert_eq!(
        handle(&ctx, &mut sess, "stationSvc", "Dock", &[PyRep::Integer(jita_station)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ShipInWarp"
    );
    assert_eq!(sess.station_id, None);
    assert!(ctx.active_warps.lock().unwrap().contains_key(&ship_id));

    // 2. Decelerating with <= 1.0s remaining (2,800m from landing at 15.0 AU/s) -> succeeds and terminates warp
    assert!(sys_h.set_ball_position(ship_id, landing + DVec3::new(2_800.0, 0.0, 0.0)).await);
    let dock_res = handle(&ctx, &mut sess, "stationSvc", "Dock", &[PyRep::Integer(jita_station)], &kw)
        .await
        .unwrap()
        .expect("stationSvc.Dock should succeed within <= 1.0s warp deceleration to station");
    assert_eq!(dock_res.dict_get("locationID"), Some(&PyRep::Integer(jita_station)));
    assert_eq!(sess.station_id, Some(jita_station));
    assert!(!ctx.active_warps.lock().unwrap().contains_key(&ship_id));
}

#[tokio::test]
async fn station_dock_invalidates_space_bound_objects_and_emits_ship_item_change() {
    let ctx = test_ctx_with_clock(1_800_000_000);
    let char_id = 90_000_703_i64;
    let corp_id = 98_000_001_i64;
    let ship_id = 100_000_703_i64;
    let jita_sys = 30_000_142_u32;
    let jita_station = 60_003_760_i64;
    let kw = HashMap::new();

    let (st_pos, st_radius) = ctx
        .sde
        .stations
        .get(&jita_station)
        .map(|s| (DVec3::new(s.position.x, s.position.y, s.position.z), 5_000.0))
        .unwrap_or((DVec3::ZERO, 5_000.0));

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = clone_char_with(snap, char_id, "DockInvalidator", corp_id, jita_sys, ship_id);
        ch.user_id = 1;
        ch.station_id = None;
        ch.structure_id = None;
        snap.characters.insert(char_id, ch);
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: jita_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let sys_h = ctx.systems.get_or_spawn(jita_sys);
    sys_h
        .spawn_ball_with_meta(
            eve_destiny::Ball {
                id: ship_id,
                type_id: 587,
                position: st_pos + DVec3::new(st_radius, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 2.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            eve_destiny::CombatState::default(),
            eve_destiny::SlimItemMeta::default(),
        )
        .await
        .unwrap();

    ctx.db
        .acquire_online_lock(1, 9_703, Some(char_id), true, ctx.now_unix())
        .unwrap();
    let mut sess = make_sess(char_id, corp_id, Some(jita_sys), None, None, Some(ship_id));
    sess.user_id = Some(1);
    let beyonce_oid = 70_300_001_u64;
    ctx.bound_objects.write().unwrap().insert(
        beyonce_oid,
        crate::router::context::BoundObjectEntry {
            bound_id: beyonce_oid,
            session_id: sess.session_id,
            user_id: char_id as u64,
            character_id: char_id,
            service: "beyonce".into(),
            object_id: jita_sys as i64,
            oid_str: format!("N=1:{jita_sys}"),
            ref_id: 1,
        },
    );
    let inv_space_oid = 70_300_002_u64;
    ctx.bound_objects.write().unwrap().insert(
        inv_space_oid,
        crate::router::context::BoundObjectEntry {
            bound_id: inv_space_oid,
            session_id: sess.session_id,
            user_id: char_id as u64,
            character_id: char_id,
            service: "invbroker".into(),
            object_id: jita_sys as i64,
            oid_str: format!("N=1:{jita_sys}"),
            ref_id: 2,
        },
    );
    assert!(ctx.bound_objects.read().unwrap().contains_key(&beyonce_oid));
    assert!(ctx.bound_objects.read().unwrap().contains_key(&inv_space_oid));

    let _ = handle(&ctx, &mut sess, "stationSvc", "Dock", &[PyRep::Integer(jita_station)], &kw)
        .await
        .unwrap()
        .expect("Dock must succeed");

    assert!(
        !ctx.bound_objects.read().unwrap().contains_key(&beyonce_oid),
        "stationSvc.Dock must invalidate space-bound beyonce monikers"
    );
    assert!(
        !ctx.bound_objects.read().unwrap().contains_key(&inv_space_oid),
        "stationSvc.Dock must invalidate space-bound invbroker monikers"
    );
    let notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        notifs.iter().any(|n| {
            n.event == "OnItemChange"
                && n.args.len() == 2
                && n.args[0].dbrow_get("itemID") == Some(&PyRep::Integer(ship_id))
                && n.args[0].dbrow_get("locationID") == Some(&PyRep::Integer(jita_station))
                && n.args[0].dbrow_get("flagID") == Some(&PyRep::Integer(InventoryFlag::Hangar as i64))
        }),
        "Dock must emit OnItemChange moving the active ship into station hangar: {notifs:?}"
    );
}
