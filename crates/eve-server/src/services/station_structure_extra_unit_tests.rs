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
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use glam::DVec3;
use std::collections::HashMap;

fn make_sess(
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

    // Outside docking perimeter -> OutsideDockingPerimeter
    let _ = sys_h
        .set_ball_position(ship_id, DVec3::new(500_000.0, 0.0, 0.0))
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
async fn pos_mgr_anchor_structure_restriction_and_warp_disruptor_container_paths() {
    let ctx = test_ctx_with_clock(1_800_000_000);
    let char_id = 920_301_i64;
    let corp_id = 98_200_301_i64;
    let ship_id = 920_302_i64;
    let null_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status <= -0.5 && s.solar_system_id < 31_000_000)
        .map(|s| s.solar_system_id)
        .unwrap();
    let jita_sys = 30_000_142_u32;
    let kw = HashMap::new();

    let find_grp = |grp: u32| ctx.sde.types.values().find(|t| t.group_id == grp).unwrap().type_id;
    let tower_tid = find_grp(365);
    let freight_cont_tid = find_grp(649);
    let bubble_tid = find_grp(361);
    let sec_cont_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 340 && t.capacity > 0.0)
        .unwrap()
        .type_id;

    let (tower_id, freight_id, trit_id, foreign_id, bubble_id, sec_cont_id) =
        (920_310, 920_311, 920_312, 920_313, 920_314, 920_315);

    ctx.db.with_mut_snapshot(|snap| {
        let ch = clone_char_with(snap, char_id, "AnchorPilot", corp_id, null_sys, ship_id);
        snap.characters.insert(char_id, ch);
        for (id, tid, owner, loc) in [
            (tower_id, tower_tid, char_id, ship_id),
            (freight_id, freight_cont_tid, char_id, ship_id),
            (trit_id, 34, char_id, ship_id),
            (foreign_id, 33_474, 999_999, ship_id),
            (bubble_id, bubble_tid, char_id, ship_id),
            (sec_cont_id, sec_cont_tid, char_id, ship_id),
        ] {
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: tid,
                    owner_id: owner,
                    location_id: loc,
                    flag: InventoryFlag::Cargo,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        // Grant Omega clone grade and anchoring skills so skill checks pass
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 2;
        }
        for &(skill_attr, _) in &[
            (182u16, 277u16),
            (183, 278),
            (184, 279),
            (1285, 1286),
            (1289, 1290),
            (1291, 1292),
        ] {
            for tid in [bubble_tid, sec_cont_tid] {
                if let Some(&sk) = ctx.sde.types.get(&tid).and_then(|t| t.default_attributes.get(&skill_attr)) {
                    let rec = eve_db::TrainedSkillRecord {
                        type_id: sk as u32,
                        level: 5,
                        skill_points: 256_000.0,
                    };
                    snap.skills.entry(char_id).or_default().insert(sk as u32, rec);
                }
            }
        }
    });

    let mut sess = make_sess(char_id, corp_id, Some(null_sys), None, None, Some(ship_id));

    // Missing argument -> MissingArgument
    assert_eq!(
        handle(&ctx, &mut sess, "posMgr", "AnchorStructure", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "MissingArgument"
    );

    // Control tower, freight container, non-anchorable item, foreign item
    for (id, expected_code) in [
        (tower_id, "StarbaseRemoved"),
        (freight_id, "CannotAnchorFreightContainer"),
        (trit_id, "ItemNotAnchorable"),
        (foreign_id, "NotItemOwner"),
    ] {
        assert_eq!(
            handle(&ctx, &mut sess, "posMgr", "AnchorStructure", &[PyRep::Integer(id)], &kw)
                .await
                .unwrap()
                .unwrap_err()
                .code,
            expected_code
        );
    }

    // Thera (31_000_005) & Abyssal (32_000_001) & Zarzakh (30_100_000) restrictions
    let mut thera_sess = sess.clone();
    thera_sess.solar_system_id = Some(31_000_005);
    assert_eq!(
        handle(&ctx, &mut thera_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(bubble_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "AnchoringDisallowedInTheraOrShattered"
    );

    let mut abyss_sess = sess.clone();
    abyss_sess.solar_system_id = Some(32_000_001);
    assert_eq!(
        handle(&ctx, &mut abyss_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(bubble_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CannotDeployInAbyssalSpace"
    );

    let mut zarzakh_sess = sess.clone();
    zarzakh_sess.solar_system_id = Some(30_100_000);
    assert!(handle(&ctx, &mut zarzakh_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(bubble_id)], &kw)
        .await
        .unwrap()
        .is_err());

    // High-sec restrictions for Mobile Warp Disruptor (361) and Secure Container (340 in >= 0.8)
    let mut hs_sess = sess.clone();
    hs_sess.solar_system_id = Some(jita_sys);
    assert_eq!(
        handle(&ctx, &mut hs_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(bubble_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CantAnchorInEmpire"
    );
    assert_eq!(
        handle(&ctx, &mut hs_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(sec_cont_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CantAnchorInHighSec08"
    );

    // Spawn ship in null_sys and anchor both Mobile Warp Disruptor (with explicit coords) and Secure Container
    let sys_h = ctx.systems.get_or_spawn(null_sys);
    let _ = sys_h
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::new(300_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    let bubble_rep = handle(
        &ctx,
        &mut sess,
        "posMgr",
        "AnchorStructure",
        &[
            PyRep::Integer(bubble_id),
            PyRep::Tuple(vec![PyRep::Real(300_500.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
        ],
        &kw,
    )
    .await
    .unwrap()
    .expect("anchor mobile warp disruptor in nullsec");
    assert!(bubble_rep.dict_get("bubbleRadius").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0);

    // Anchoring already-anchoring bubble again -> StructureAlreadyAnchored
    assert_eq!(
        handle(&ctx, &mut sess, "posMgr", "AnchorStructure", &[PyRep::Integer(bubble_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "StructureAlreadyAnchored"
    );

    let cont_rep = handle(&ctx, &mut sess, "posMgr", "AnchorStructure", &[PyRep::Integer(sec_cont_id)], &kw)
        .await
        .unwrap()
        .expect("anchor secure container in nullsec");
    assert_eq!(cont_rep.dict_get("itemID"), Some(&PyRep::Integer(sec_cont_id)));
}
