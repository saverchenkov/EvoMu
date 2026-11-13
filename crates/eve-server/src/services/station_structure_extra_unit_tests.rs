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
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp },
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
    let landing = st_pos + DVec3::new(eff_radius + 2_300.0, 0.0, 0.0);
    // 1. Decelerating with > 4.0s remaining (1,000,000m from landing at 15.0 AU/s) -> ShipInWarp even with non-empty session.address
    let mut warp_mode = MovementMode::new_warp(origin, landing, 15.0);
    if let MovementMode::Warp { ref mut phase, .. } = warp_mode {
        *phase = eve_destiny::WarpPhase::Decelerating;
    }
    sys_h
        .spawn_ball_with_meta(
            eve_destiny::Ball {
                id: ship_id,
                type_id: 587,
                position: landing + DVec3::new(1_000_000.0, 0.0, 0.0),
                velocity: DVec3::new(-2_000_000.0, 0.0, 0.0),
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

    // 2. Decelerating with ~2.8s remaining at 5.0 AU/s (#1001 client/server decel lead, outside 2,500m perimeter) -> succeeds and terminates warp
    let mut lead_warp = MovementMode::new_warp(origin, landing, 5.0);
    if let MovementMode::Warp { ref mut phase, .. } = lead_warp {
        *phase = eve_destiny::WarpPhase::Decelerating;
    }
    {
        let mut bp = sys_h.ballpark.write().unwrap();
        let ball = bp.balls.get_mut(&ship_id).expect("ship ball");
        ball.position = landing + DVec3::new(6_069.0, 0.0, 0.0);
        ball.mode = lead_warp;
    }
    let dock_res = handle(&ctx, &mut sess, "stationSvc", "Dock", &[PyRep::Integer(jita_station)], &kw)
        .await
        .unwrap()
        .expect("stationSvc.Dock should succeed within <= 4.0s warp deceleration to station (#1001)");
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

#[tokio::test]
async fn undock_fulcrum_inertia_structure_notice_and_anchor_structure_all_gates_slim_meta() {
    use crate::router::ServerContext;
    use eve_db::{ContractRecord, WorldRepository};
    use eve_destiny::Ball;
    use eve_sde::SdeRegistry;
    use std::sync::Arc;

    let kw = HashMap::new();
    let jita_station = 60_003_760_i64;
    let fulcrum_station = 60_015_187_i64;
    let zarzakh_sys = 30_100_000_u32;
    let jita_sys = 30_000_142_u32;
    let st_zarzakh = 1_000_000_999_001_i64;
    let corp_id = 98_299_001_i64;
    let ally_id = 99_299_001_i64;

    // Build ServerContext with custom named & unnamed Secure Cargo Containers (category 22, group 340)
    let mut sde = SdeRegistry::with_canonical_seed();
    let cont_named_tid = 929_001_u32;
    let cont_unnamed_tid = 929_002_u32;
    let base_cont = sde.types.get(&587).cloned().expect("seed frigate");
    let mut named_cont = base_cont.clone();
    named_cont.type_id = cont_named_tid;
    named_cont.group_id = 340;
    named_cont.category_id = 22;
    named_cont.name = "Custom Anchor Container".into();
    named_cont.capacity = 1_000.0;
    named_cont.mass = 50_000.0;
    named_cont.radius = 50.0;
    named_cont.default_attributes = HashMap::from([(553, 5_000.0)]);
    let mut unnamed_cont = named_cont.clone();
    unnamed_cont.type_id = cont_unnamed_tid;
    unnamed_cont.category_id = 2;
    unnamed_cont.effect_ids.push(649);
    unnamed_cont.name = String::new();
    sde.types.insert(cont_named_tid, named_cont);
    sde.types.insert(cont_unnamed_tid, unnamed_cont);

    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db);
    ctx.set_clock(Arc::new(eve_db::clock::VirtualServerClock::with_fixed_base(1_800_100_000)));

    let null_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| {
            s.solar_system_id >= 30_000_000
                && s.solar_system_id < 31_000_000
                && s.security_status <= 0.0
                && s.solar_system_id != zarzakh_sys
        })
        .map(|s| s.solar_system_id)
        .unwrap_or(30_004_759);

    let neutral_pilot = 929_101_i64;
    let pirate_fulcrum = 929_102_i64;
    let pirate_zarzakh_st = 929_103_i64;
    let jita_pilot = 929_104_i64;
    let ship_neutral = 929_201_i64;
    let ship_pirate_f = 929_202_i64;
    let ship_pirate_z = 929_203_i64;
    let ship_jita = 929_204_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(corp_id, {
            let mut c = CorporationRecord::default();
            c.corporation_id = corp_id;
            c.alliance_id = Some(ally_id);
            c
        });
        snap.structures.insert(
            st_zarzakh,
            UpwellStructureRecord {
                structure_id: st_zarzakh,
                type_id: 35_832,
                owner_corp_id: corp_id,
                solar_system_id: zarzakh_sys,
                name: "Zarzakh Citadel".into(),
                position: [150_000.0, 0.0, 0.0],
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

        for (cid, uid, name, sys_id, stn, struc, ship_id, war_faction) in [
            (neutral_pilot, 101_u64, "NeutralFulcrum", zarzakh_sys, Some(fulcrum_station), None, ship_neutral, None),
            (pirate_fulcrum, 102, "PirateFulcrum", zarzakh_sys, Some(fulcrum_station), None, ship_pirate_f, Some(500_010)),
            (pirate_zarzakh_st, 103, "PirateZarzakhSt", zarzakh_sys, None, Some(st_zarzakh), ship_pirate_z, Some(500_010)),
            (jita_pilot, 104, "JitaNpcUndock", jita_sys, Some(jita_station), None, ship_jita, None),
        ] {
            let uname = format!("u_{uid}");
            snap.accounts.insert(
                uname.clone(),
                eve_db::AccountRecord {
                    user_id: uid,
                    username: uname,
                    password_token: "test".into(),
                    role: eve_db::service_roles::ROLE_PLAYER,
                    banned_until_unix: None,
                    ban_reason: None,
                },
            );
            let mut ch = clone_char_with(snap, cid, name, corp_id, sys_id, ship_id);
            ch.user_id = uid;
            ch.station_id = stn;
            ch.structure_id = struc;
            ch.alliance_id = Some(ally_id);
            ch.war_faction_id = war_faction;
            snap.characters.insert(cid, ch);
            snap.items.insert(
                ship_id,
                InventoryItemRecord {
                    item_id: ship_id,
                    type_id: 587,
                    owner_id: cid,
                    location_id: stn.or(struc).unwrap(),
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });

    for (uid, cid) in [
        (101_u64, neutral_pilot),
        (102, pirate_fulcrum),
        (103, pirate_zarzakh_st),
        (104, jita_pilot),
    ] {
        ctx.db
            .acquire_online_lock(uid, uid, Some(cid), true, ctx.now_unix())
            .expect("online lock");
    }

    // 1. Fulcrum Undock: neutral mult==1.0 leaves base_inertia/external_inertia_multiplier None;
    //    pirate at Fulcrum AND pirate at non-Fulcrum Zarzakh Upwell structure apply 0.80 multiplier;
    //    Upwell structure (>= 1_000_000_000_000) emits OnCharacterLeftStructure, NPC station does not.
    let mut s_neu = make_sess(neutral_pilot, corp_id, Some(zarzakh_sys), Some(fulcrum_station), None, Some(ship_neutral));
    s_neu.user_id = Some(101);
    handle(&ctx, &mut s_neu, "stationSvc", "Undock", &[], &kw).await.unwrap().expect("neutral undock");
    let z_sys_h = ctx.systems.get_or_spawn(zarzakh_sys);
    let (_, neu_ball) = z_sys_h.get_ball_snapshot(ship_neutral).await.unwrap().unwrap();
    let neu_cs = z_sys_h.get_combat_state(ship_neutral).await.unwrap().unwrap();
    assert_eq!(neu_cs.base_inertia, Some(neu_ball.inertia));
    assert_eq!(neu_cs.external_inertia_multiplier, None);

    let mut s_pir_f = make_sess(pirate_fulcrum, corp_id, Some(zarzakh_sys), Some(fulcrum_station), None, Some(ship_pirate_f));
    s_pir_f.user_id = Some(102);
    handle(&ctx, &mut s_pir_f, "stationSvc", "Undock", &[], &kw).await.unwrap().expect("pirate fulcrum undock");
    let (_, pir_f_ball) = z_sys_h.get_ball_snapshot(ship_pirate_f).await.unwrap().unwrap();
    let pir_f_cs = z_sys_h.get_combat_state(ship_pirate_f).await.unwrap().unwrap();
    assert!(pir_f_cs.base_inertia.is_some());
    assert_eq!(pir_f_cs.external_inertia_multiplier, Some(0.80));
    assert!((pir_f_ball.inertia - neu_ball.inertia * 0.80).abs() < 1e-6);

    let _ = ctx.drain_notifications_for_char(pirate_zarzakh_st);
    let mut s_pir_z = make_sess(pirate_zarzakh_st, corp_id, Some(zarzakh_sys), None, Some(st_zarzakh), Some(ship_pirate_z));
    s_pir_z.user_id = Some(103);
    handle(&ctx, &mut s_pir_z, "stationSvc", "Undock", &[], &kw).await.unwrap().expect("pirate zarzakh upwell undock");
    let (_, pir_z_ball) = z_sys_h.get_ball_snapshot(ship_pirate_z).await.unwrap().unwrap();
    let pir_z_cs = z_sys_h.get_combat_state(ship_pirate_z).await.unwrap().unwrap();
    assert!(pir_z_cs.base_inertia.is_some());
    assert_eq!(pir_z_cs.external_inertia_multiplier, Some(0.80));
    assert!((pir_z_ball.inertia - neu_ball.inertia * 0.80).abs() < 1e-6);
    let upwell_notifs = ctx.drain_notifications_for_char(pirate_zarzakh_st);
    assert!(
        upwell_notifs.iter().any(|n| n.event == "OnCharacterLeftStructure" && n.args == vec![PyRep::Integer(pirate_zarzakh_st)]),
        "Upwell undock must emit OnCharacterLeftStructure: {upwell_notifs:?}"
    );

    let _ = ctx.drain_notifications_for_char(jita_pilot);
    let mut s_jita = make_sess(jita_pilot, corp_id, Some(jita_sys), Some(jita_station), None, Some(ship_jita));
    s_jita.user_id = Some(104);
    handle(&ctx, &mut s_jita, "stationSvc", "Undock", &[], &kw).await.unwrap().expect("jita npc undock");
    let jita_notifs = ctx.drain_notifications_for_char(jita_pilot);
    assert!(
        !jita_notifs.iter().any(|n| n.event == "OnCharacterLeftStructure"),
        "NPC station undock must NOT emit OnCharacterLeftStructure: {jita_notifs:?}"
    );

    // 2. posMgr.AnchorStructure DB transaction gates & SlimItemMeta fields
    let anchor_pilot = 929_301_i64;
    let anchor_ship = 929_302_i64;
    let cont_cargo = 929_401_i64;
    let cont_flag_locked = 929_402_i64;
    let cont_space = 929_403_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = clone_char_with(snap, anchor_pilot, "AnchorTester", corp_id, null_sys, anchor_ship);
        ch.station_id = None;
        ch.structure_id = None;
        ch.alliance_id = Some(ally_id);
        snap.characters.insert(anchor_pilot, ch);
        for (item_id, tid, loc, flag) in [
            (anchor_ship, 587_u32, null_sys as i64, InventoryFlag::None),
            (cont_cargo, cont_named_tid, anchor_ship, InventoryFlag::Cargo),
            (cont_flag_locked, cont_named_tid, anchor_ship, InventoryFlag::Locked),
            (cont_space, cont_unnamed_tid, null_sys as i64, InventoryFlag::None),
        ] {
            snap.items.insert(
                item_id,
                InventoryItemRecord {
                    item_id,
                    type_id: tid,
                    owner_id: anchor_pilot,
                    location_id: loc,
                    flag,
                    quantity: 1,
                    singleton: false,
                    version: 7,
                    ..Default::default()
                },
            );
        }
        snap.contracts.insert(
            929_501,
            ContractRecord {
                contract_id: 929_501,
                issuer_id: anchor_pilot,
                contract_type: "ItemExchange".into(),
                status: "Outstanding".into(),
                item_ids: vec![cont_cargo],
                ..Default::default()
            },
        );
    });

    let null_h = ctx.systems.get_or_spawn(null_sys);
    null_h
        .spawn_ball(Ball {
            id: anchor_ship,
            type_id: 587,
            position: DVec3::new(100_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await
        .unwrap();

    let mut a_sess = make_sess(anchor_pilot, corp_id, Some(null_sys), None, None, Some(anchor_ship));
    a_sess.alliance_id = Some(ally_id);

    // Every DB character mismatch branch inside AnchorStructure tx -> ShipNotInSpace
    for (stn, struc, sys, sh) in [
        (Some(jita_station), None, null_sys, anchor_ship),
        (None, Some(st_zarzakh), null_sys, anchor_ship),
        (None, None, jita_sys, anchor_ship),
        (None, None, null_sys, anchor_ship + 1),
    ] {
        ctx.db.with_mut_snapshot(|snap| {
            let ch = snap.characters.get_mut(&anchor_pilot).unwrap();
            ch.station_id = stn;
            ch.structure_id = struc;
            ch.solar_system_id = sys;
            ch.active_ship_id = sh;
        });
        assert_eq!(
            handle(&ctx, &mut a_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(cont_cargo)], &kw)
                .await
                .unwrap()
                .unwrap_err()
                .code,
            "ShipNotInSpace"
        );
    }
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&anchor_pilot).unwrap();
        ch.station_id = None;
        ch.structure_id = None;
        ch.solar_system_id = null_sys;
        ch.active_ship_id = anchor_ship;
    });

    // Locked by contract AND locked by InventoryFlag::Locked -> ItemLocked
    assert_eq!(
        handle(&ctx, &mut a_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(cont_cargo)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ItemLocked"
    );
    assert_eq!(
        handle(&ctx, &mut a_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(cont_flag_locked)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ItemLocked"
    );

    // Unlock contract -> anchor named container from ship cargo: version bumps 7 -> 8 & SlimItemMeta populates
    ctx.db.with_mut_snapshot(|snap| {
        snap.contracts.clear();
    });
    handle(&ctx, &mut a_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(cont_cargo)], &kw)
        .await
        .unwrap()
        .expect("anchor named container from cargo");
    ctx.db.with_snapshot(|snap| {
        let it = &snap.items[&cont_cargo];
        assert_eq!(it.version, 8);
        assert_eq!(it.location_id, null_sys as i64);
        assert_eq!(it.flag, InventoryFlag::Hangar);
        assert!(it.singleton);
    });
    let slim_named = null_h.get_ball_slim_item(cont_cargo).await.unwrap().unwrap();
    assert_eq!(slim_named.dict_get("groupID"), Some(&PyRep::Integer(340)));
    assert_eq!(slim_named.dict_get("categoryID"), Some(&PyRep::Integer(22)));
    assert_eq!(slim_named.dict_get("ownerID"), Some(&PyRep::Integer(anchor_pilot)));
    assert_eq!(slim_named.dict_get("charID"), Some(&PyRep::None));
    assert_eq!(slim_named.dict_get("corpID"), Some(&PyRep::Integer(corp_id)));
    assert_eq!(slim_named.dict_get("allianceID"), Some(&PyRep::Integer(ally_id)));
    assert_eq!(slim_named.dict_get("securityStatus"), Some(&PyRep::Real(0.0)));
    assert_eq!(slim_named.dict_get("name"), Some(&PyRep::String("Custom Anchor Container".into())));

    // Anchor unnamed container already in space with session corp/alliance = None -> corpID/allianceID None, fallback name
    a_sess.corporation_id = None;
    a_sess.alliance_id = None;
    handle(&ctx, &mut a_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(cont_space)], &kw)
        .await
        .unwrap()
        .expect("anchor unnamed container in space");
    let slim_unnamed = null_h.get_ball_slim_item(cont_space).await.unwrap().unwrap();
    assert_eq!(slim_unnamed.dict_get("corpID"), Some(&PyRep::None));
    assert_eq!(slim_unnamed.dict_get("allianceID"), Some(&PyRep::None));
    assert_eq!(
        slim_unnamed.dict_get("name"),
        Some(&PyRep::String(format!("Item {cont_space}")))
    );
}
