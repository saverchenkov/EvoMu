//! Edge-case, ACL, boundary, and error-path tests for `fitting_ships_mastery`.

use super::*;
use crate::services::fitting_containers_ships::{AlscLogEntry, ContainerShipRuntimeState};
use crate::services::space_destiny::SessionChangeTimerState;
use eve_db::{
    InventoryFlag, InventoryItemRecord, OwnerNoteRecord, StarbaseDetailSettings,
    UpwellStructureRecord, WorldRepository,
};
use eve_destiny::{Ball, DVec3, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use std::collections::HashMap;
use std::sync::Arc;

fn docked_session(char_id: i64, corp_id: i64, station_id: i64, sys_id: u32, ship_id: i64) -> SessionState {
    let mut s = SessionState::default();
    s.user_id = Some(1);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.station_id = Some(station_id);
    s.solar_system_id = Some(sys_id);
    s.ship_id = Some(ship_id);
    s
}

fn space_session(char_id: i64, corp_id: i64, sys_id: u32, ship_id: Option<i64>) -> SessionState {
    let mut s = SessionState::default();
    s.user_id = Some(1);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = Some(sys_id);
    s.ship_id = ship_id;
    s
}

fn make_ball(id: i64, type_id: u32, pos: DVec3) -> Ball {
    Ball {
        id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ships_mastery_leave_eject_board_error_paths_and_existing_capsule_reuse() {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let char_id = 96_510_001i64;
    let other_char = 96_510_002i64;
    let corp_id = 1_000_009i64;
    let station_id = 60_003_760i64;
    let hisec_sys = 30_000_142u32;
    let lowsec_sys = 30_002_718u32;
    let ship_id = 96_510_100i64;
    let docked_cap_id = 96_510_101i64;
    let structure_id = 1_000_000_510_001i64;
    let tritanium_id = 96_510_102i64;
    let unowned_ship_id = 96_510_103i64;
    let unpackaged_ship_id = 96_510_104i64;
    let space_cap_id = 96_510_105i64;
    let far_ship_id = 96_510_106i64;
    let near_ship_id = 96_510_107i64;
    let tower_id = 96_510_108i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = 1;
        ch.name = "EdgePilot".into();
        ch.corporation_id = corp_id;
        ch.race_id = 2;
        ch.station_id = Some(station_id);
        ch.solar_system_id = hisec_sys;
        ch.active_ship_id = docked_cap_id;
        snap.characters.insert(char_id, ch);

        snap.items.insert(
            docked_cap_id,
            InventoryItemRecord {
                item_id: docked_cap_id,
                type_id: 670,
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
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 588, // Reaper (Corvette, zero skill prereqs)
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
            tritanium_id,
            InventoryItemRecord {
                item_id: tritanium_id,
                type_id: 34,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 10,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            unowned_ship_id,
            InventoryItemRecord {
                item_id: unowned_ship_id,
                type_id: 587,
                owner_id: other_char,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            unpackaged_ship_id,
            InventoryItemRecord {
                item_id: unpackaged_ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.structures.insert(
            structure_id,
            UpwellStructureRecord {
                structure_id,
                type_id: 35_832,
                owner_corp_id: corp_id,
                solar_system_id: hisec_sys,
                name: "GuardFort".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 10_000.0,
                armor_hp: 10_000.0,
                hull_hp: 10_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });

    // 1. LeaveShip while active ship is a Capsule -> CannotEjectFromCapsule
    let mut sess = docked_session(char_id, corp_id, station_id, hisec_sys, docked_cap_id);
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut sess, None, "ship", "LeaveShip", &[], &HashMap::new())
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CannotEjectFromCapsule"
    );

    // 2. LeaveShip from non-capsule reuses existing unboarded capsule in hangar
    sess.ship_id = Some(ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().active_ship_id = ship_id;
    });
    let reused_cap = handle_ships_mastery_and_fitting_ext(&ctx, &mut sess, None, "ship", "LeaveShip", &[], &HashMap::new())
        .await
        .unwrap()
        .unwrap()
        .as_i64()
        .unwrap();
    assert_eq!(reused_cap, docked_cap_id);
    assert_eq!(sess.ship_id, Some(docked_cap_id));

    // 3. SessionChangeTimerActive blocks LeaveShip and Board
    {
        let now_u = ctx.now_unix_secs();
        ctx.session_change_timers.lock().unwrap().insert(
            char_id,
            SessionChangeTimerState {
                expires_unix: now_u + 30,
                system_id: hisec_sys,
                started_tick: 0,
                reason: "test".into(),
                location_change_invul_until: 0,
            },
        );
    }
    sess.ship_id = Some(ship_id);
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut sess, None, "ship", "LeaveShip", &[], &HashMap::new())
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "SessionChangeTimerActive"
    );
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut sess, None, "ship", "Board", &[PyRep::Integer(docked_cap_id)], &HashMap::new())
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "SessionChangeTimerActive"
    );
    ctx.session_change_timers.lock().unwrap().clear();

    // 4. Board error cases docked: empty args (no ship_id), ControllingStructure, OwnershipViolation, ShipNotBoardable, NotAShip
    let mut no_ship_sess = docked_session(char_id, corp_id, station_id, hisec_sys, 0);
    no_ship_sess.ship_id = None;
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut no_ship_sess, None, "ship", "Board", &[], &HashMap::new())
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ShipNotFound"
    );

    let mut ctrl_sess = docked_session(char_id, corp_id, station_id, hisec_sys, structure_id);
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut ctrl_sess, None, "ship", "Board", &[PyRep::Integer(ship_id)], &HashMap::new())
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ControllingStructure"
    );
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut sess, None, "ship", "Board", &[PyRep::Integer(unowned_ship_id)], &HashMap::new())
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "OwnershipViolation"
    );
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut sess, None, "ship", "Board", &[PyRep::Integer(unpackaged_ship_id)], &HashMap::new())
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ShipNotBoardable"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&tritanium_id).unwrap().singleton = true;
    });
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut sess, None, "ship", "Board", &[PyRep::Integer(tritanium_id)], &HashMap::new())
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NotAShip"
    );

    // Boarding 588 without Minmatar Frigate skill returns ShipHasSkillPrerequisites
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut sess, None, "ship", "Board", &[PyRep::Integer(ship_id)], &HashMap::new())
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ShipHasSkillPrerequisites"
    );

    // Board into already-active capsule (no skill prereqs) produces no OnSessionChanged notification
    ctx.drain_notifications_for_char(char_id);
    sess.ship_id = Some(docked_cap_id);
    let same_res = handle_ships_mastery_and_fitting_ext(&ctx, &mut sess, None, "ship", "Board", &[PyRep::Integer(docked_cap_id)], &HashMap::new())
        .await
        .unwrap()
        .unwrap();
    let PyRep::Tuple(same_tup) = same_res else { panic!("expected Tuple"); };
    assert_eq!(same_tup.len(), 4);
    let notes = ctx.drain_notifications_for_char(char_id);
    assert!(!notes.iter().any(|n| n.event == "OnSessionChanged"));

    // 5. Eject in lowsec while criminal is allowed (only high-sec >= 0.45 blocks criminal eject)
    let lowsec_ship = 96_510_110i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().station_id = None;
        snap.characters.get_mut(&char_id).unwrap().solar_system_id = lowsec_sys;
        snap.items.insert(
            lowsec_ship,
            InventoryItemRecord {
                item_id: lowsec_ship,
                type_id: 587,
                owner_id: char_id,
                location_id: lowsec_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    ctx.session_change_timers.lock().unwrap().clear();
    {
        let now_u = ctx.now_unix_secs();
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.states.entry(char_id).or_default().criminal_flag_until_unix = now_u + 120;
    }
    let mut ls_sess = space_session(char_id, corp_id, lowsec_sys, Some(lowsec_ship));
    let ej_ok = handle_ships_mastery_and_fitting_ext(&ctx, &mut ls_sess, None, "ship", "Eject", &[], &HashMap::new())
        .await
        .unwrap()
        .expect("criminal eject should succeed in lowsec/nullsec");
    let PyRep::KeyVal(pairs) = ej_ok else { panic!("expected KeyVal"); };
    assert!(pairs.iter().any(|(k, _)| k == "capsuleID"));
    ctx.crimewatch.write().unwrap().states.clear();
    ctx.session_change_timers.lock().unwrap().clear();

    // 6. In-space Board guards: NoSolarSystem, NoActiveShip, weapons timer, hisec criminal,
    // ShipNotInSpace, ShipOutOfBoardingRange, ForcefieldAccessDenied
    let mut no_sys_sess = SessionState::default();
    no_sys_sess.user_id = Some(1);
    no_sys_sess.character_id = Some(char_id);
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut no_sys_sess, None, "ship", "Board", &[PyRep::Integer(ship_id)], &HashMap::new())
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NoSolarSystem"
    );

    let mut sp_no_ship = space_session(char_id, corp_id, hisec_sys, None);
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut sp_no_ship, None, "ship", "Board", &[PyRep::Integer(ship_id)], &HashMap::new())
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NoActiveShip"
    );

    ctx.db.with_mut_snapshot(|snap| {
        for (id, tid) in [(space_cap_id, 670), (far_ship_id, 587), (near_ship_id, 587)] {
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: hisec_sys as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        snap.items.insert(
            tower_id,
            InventoryItemRecord {
                item_id: tower_id,
                type_id: 12_235,
                owner_id: other_char,
                location_id: hisec_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.item_positions.insert(tower_id, [0.0, 0.0, 0.0]);
        snap.module_online_states.insert(tower_id, true);
        let fuel = 96_510_199i64;
        snap.items.insert(
            fuel,
            InventoryItemRecord {
                item_id: fuel,
                type_id: 4051,
                owner_id: other_char,
                location_id: tower_id,
                flag: InventoryFlag::SpecializedFuelBay,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.starbase_settings.insert(
            tower_id,
            StarbaseDetailSettings {
                password: Some("secret".into()),
                allow_corporation_members: false,
                allow_alliance_members: false,
                ..Default::default()
            },
        );
    });

    let sys = ctx.systems.get_or_spawn(hisec_sys);
    let _ = sys.spawn_ball(make_ball(space_cap_id, 670, DVec3::new(0.0, 0.0, 0.0))).await;
    let _ = sys.spawn_ball(make_ball(far_ship_id, 587, DVec3::new(5_000.0, 0.0, 0.0))).await;
    let _ = sys.spawn_ball(make_ball(near_ship_id, 587, DVec3::new(1_000.0, 0.0, 0.0))).await;

    let mut sp_sess = space_session(char_id, corp_id, hisec_sys, Some(space_cap_id));

    // Weapons timer blocks in-space board
    {
        let now_u = ctx.now_unix_secs();
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.states.entry(char_id).or_default().weapon_timer_until_unix = now_u + 60;
    }
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut sp_sess, None, "ship", "Board", &[PyRep::Integer(near_ship_id)], &HashMap::new())
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CannotStoreShipActiveWeaponsTimer"
    );
    ctx.crimewatch.write().unwrap().states.clear();

    // Criminal in hisec blocks in-space board
    {
        let now_u = ctx.now_unix_secs();
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.states.entry(char_id).or_default().criminal_flag_until_unix = now_u + 60;
    }
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut sp_sess, None, "ship", "Board", &[PyRep::Integer(near_ship_id)], &HashMap::new())
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CannotBoardShipInHighSecAsCriminal"
    );
    ctx.crimewatch.write().unwrap().states.clear();

    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut sp_sess, None, "ship", "Board", &[PyRep::Integer(99_999_999)], &HashMap::new())
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ShipNotInSpace"
    );
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut sp_sess, None, "ship", "Board", &[PyRep::Integer(far_ship_id)], &HashMap::new())
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ShipOutOfBoardingRange"
    );
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut sp_sess, None, "ship", "Board", &[PyRep::Integer(near_ship_id)], &HashMap::new())
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ForcefieldAccessDenied"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ships_mastery_launch_corvette_trash_mastery_killcounter_dirt_edge_paths() {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let char_id = 96_520_001i64;
    let other_char = 96_520_002i64;
    let corp_id = 1_000_009i64;
    let station_id = 60_003_760i64;
    let other_station_id = 60_003_761i64;
    let sys_id = 30_000_142u32;
    let ship_id = 96_520_100i64;
    let container_id = 96_520_101i64;
    let inside_ship_id = 96_520_102i64;
    let tritanium_in_can = 96_520_103i64;
    let audit_container = 96_520_105i64;
    let distant_item = 96_520_106i64;
    let trash_a = 96_520_107i64;
    let trash_b = 96_520_108i64;
    let no_dirt_ship = 96_520_109i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = 1;
        ch.name = "EdgeMastery".into();
        ch.corporation_id = corp_id;
        ch.race_id = 2;
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
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                dynamic_attributes: HashMap::from([(1969, 1_700_000_000.0)]),
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            container_id,
            InventoryItemRecord {
                item_id: container_id,
                type_id: 3468,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.item_positions.insert(container_id, [10_000.0, 0.0, 0.0]);
        snap.items.insert(
            inside_ship_id,
            InventoryItemRecord {
                item_id: inside_ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: container_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            tritanium_in_can,
            InventoryItemRecord {
                item_id: tritanium_in_can,
                type_id: 34,
                owner_id: char_id,
                location_id: container_id,
                flag: InventoryFlag::Cargo,
                quantity: 5,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            audit_container,
            InventoryItemRecord {
                item_id: audit_container,
                type_id: 3468,
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
            distant_item,
            InventoryItemRecord {
                item_id: distant_item,
                type_id: 34,
                owner_id: char_id,
                location_id: other_station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        for id in [trash_a, trash_b] {
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: 34,
                    owner_id: char_id,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 5,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        snap.items.insert(
            no_dirt_ship,
            InventoryItemRecord {
                item_id: no_dirt_ship,
                type_id: 596,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        let mut cs_state = ContainerShipRuntimeState::default();
        cs_state.alsc_logs.insert(
            audit_container,
            vec![AlscLogEntry {
                timestamp_unix: ctx.now_unix(),
                actor_id: char_id,
                action: "Lock".into(),
                item_id: audit_container,
                item_type_id: 3468,
                quantity: 1,
            }],
        );
        snap.owner_notes.insert(
            -8_400_000_000_000,
            OwnerNoteRecord {
                note_id: -8_400_000_000_000,
                character_id: -84,
                item_id: 0,
                label: "container_ship".into(),
                note: serde_json::to_string(&cs_state).unwrap(),
                timestamp_unix: 0,
            },
        );
    });

    // 1. LaunchFromContainer error paths & item_positions fallback
    let mut docked = docked_session(char_id, corp_id, station_id, sys_id, ship_id);
    docked.solar_system_id = None;
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut docked, None, "ship", "LaunchFromContainer", &[PyRep::Integer(inside_ship_id)], &HashMap::new())
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NotInSpace"
    );

    let mut sp_no_ship = space_session(char_id, corp_id, sys_id, None);
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut sp_no_ship, None, "ship", "LaunchFromContainer", &[PyRep::Integer(inside_ship_id)], &HashMap::new())
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NoActiveShip"
    );

    let mut sp = space_session(char_id, corp_id, sys_id, Some(ship_id));
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut sp, None, "ship", "LaunchFromContainer", &[PyRep::Integer(inside_ship_id)], &HashMap::new())
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ObjectNotInSpace"
    );

    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys.spawn_ball(make_ball(ship_id, 587, DVec3::new(0.0, 0.0, 0.0))).await;

    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut sp, None, "ship", "LaunchFromContainer", &[PyRep::Integer(88_888_888)], &HashMap::new())
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ItemNotFound"
    );
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut sp, None, "ship", "LaunchFromContainer", &[PyRep::Integer(tritanium_in_can)], &HashMap::new())
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidLaunchItem"
    );
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut sp, None, "ship", "LaunchFromContainer", &[PyRep::Integer(inside_ship_id)], &HashMap::new())
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "OutOfRange"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&container_id).unwrap().location_id = station_id;
    });
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut sp, None, "ship", "LaunchFromContainer", &[PyRep::Integer(inside_ship_id)], &HashMap::new())
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "LaunchAccessDenied"
    );

    // Use a fresh container inserted after get_or_spawn(sys_id) so no ballpark ball exists,
    // exercising the snap.item_positions fallback path directly.
    let fallback_can_id = 96_520_191i64;
    let fallback_inside_ship_id = 96_520_192i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            fallback_can_id,
            InventoryItemRecord {
                item_id: fallback_can_id,
                type_id: 3468,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.item_positions.insert(fallback_can_id, [1_200.0, 0.0, 0.0]);
        snap.items.insert(
            fallback_inside_ship_id,
            InventoryItemRecord {
                item_id: fallback_inside_ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: fallback_can_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let launched = handle_ships_mastery_and_fitting_ext(&ctx, &mut sp, None, "ship", "LaunchFromContainer", &[PyRep::Integer(fallback_inside_ship_id)], &HashMap::new())
        .await
        .unwrap()
        .expect("LaunchFromContainer via item_positions fallback should succeed");
    assert!(matches!(launched, PyRep::KeyVal(_)));

    // 2. CreateNewbieShip error paths
    let mut sp_corv = space_session(char_id, corp_id, sys_id, Some(ship_id));
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut sp_corv, None, "dogmaIM", "CreateNewbieShip", &[], &HashMap::new())
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NotDocked"
    );

    let mut dk_corv = docked_session(char_id, corp_id, station_id, sys_id, ship_id);
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut dk_corv, None, "dogmaIM", "CreateNewbieShip", &[], &HashMap::new())
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "AlreadyInNewbieShip"
    );

    let dock_pod = 96_520_110i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            dock_pod,
            InventoryItemRecord {
                item_id: dock_pod,
                type_id: 670,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.characters.get_mut(&char_id).unwrap().active_ship_id = dock_pod;
        snap.characters.get_mut(&char_id).unwrap().race_id = 999;
    });
    dk_corv.ship_id = Some(dock_pod);
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut dk_corv, None, "dogmaIM", "CreateNewbieShip", &[], &HashMap::new())
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "AlreadyHaveShipInStation"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&no_dirt_ship);
        snap.items.remove(&audit_container);
    });
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut dk_corv, None, "dogmaIM", "CreateNewbieShip", &[], &HashMap::new())
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NoRacialCorvette"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().race_id = 2;
        snap.item_custom_names
            .insert(-8_800_000 - char_id, ctx.now_unix_secs().to_string());
    });
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut dk_corv, None, "dogmaIM", "CreateNewbieShip", &[], &HashMap::new())
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NewbieShipCooldown"
    );

    // 3. Mastery / Skill / Certificate / Simulation kwargs / KillCounter errors
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut dk_corv, None, "dogmaIM", "GetRequiredSkillLevels", &[PyRep::Integer(99_999_999)], &HashMap::new())
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "TypeNotFound"
    );
    assert!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut dk_corv, None, "dogmaIM", "GetRequiredSkillLevels", &[PyRep::Integer(dock_pod)], &HashMap::new())
            .await
            .unwrap()
            .is_ok()
    );
    let l0 = handle_ships_mastery_and_fitting_ext(&ctx, &mut dk_corv, None, "dogmaIM", "GetSkillsRequiredForMasteryLevel", &[PyRep::Integer(587), PyRep::Integer(0)], &HashMap::new())
        .await
        .unwrap()
        .unwrap();
    let l1 = handle_ships_mastery_and_fitting_ext(&ctx, &mut dk_corv, None, "dogmaIM", "GetSkillsRequiredForMasteryLevel", &[PyRep::Integer(587), PyRep::Integer(1)], &HashMap::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(l0, l1);
    let c9 = handle_ships_mastery_and_fitting_ext(&ctx, &mut dk_corv, None, "dogmaIM", "GetCertificatesForShipByMasteryLevel", &[PyRep::Integer(587), PyRep::Integer(9)], &HashMap::new())
        .await
        .unwrap()
        .unwrap();
    let c5 = handle_ships_mastery_and_fitting_ext(&ctx, &mut dk_corv, None, "dogmaIM", "GetCertificatesForShipByMasteryLevel", &[PyRep::Integer(587), PyRep::Integer(5)], &HashMap::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(c9, c5);

    let mut sim_kw = HashMap::new();
    sim_kw.insert("shipTypeID".into(), PyRep::Integer(587));
    sim_kw.insert("fitData".into(), PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(2048), PyRep::Integer(11)])]));
    let sim_res = handle_ships_mastery_and_fitting_ext(&ctx, &mut dk_corv, None, "ship", "GetShipFittingInfo", &[], &sim_kw)
        .await
        .unwrap()
        .unwrap();
    let PyRep::KeyVal(sim_kv) = sim_res else { panic!("expected KeyVal"); };
    assert!(sim_kv.iter().any(|(k, v)| k == "shipTypeID" && v == &PyRep::Integer(587)));

    dk_corv.role = u64::MAX;
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut dk_corv, None, "shipKillCounter", "SetShipKillCounter", &[PyRep::Integer(88_888_888), PyRep::Integer(3)], &HashMap::new())
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ItemNotFound"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            no_dirt_ship,
            InventoryItemRecord {
                item_id: no_dirt_ship,
                type_id: 587,
                owner_id: other_char,
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
        handle_ships_mastery_and_fitting_ext(&ctx, &mut dk_corv, None, "shipKillCounter", "SetShipKillCounter", &[PyRep::Integer(no_dirt_ship), PyRep::Integer(3)], &HashMap::new())
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ShipOwnershipViolation"
    );
    let clamped = handle_ships_mastery_and_fitting_ext(&ctx, &mut dk_corv, None, "shipKillCounter", "SetShipKillCounter", &[PyRep::Integer(dock_pod), PyRep::Integer(-5)], &HashMap::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(clamped, PyRep::Integer(0));

    // 4. TrashItems edge paths: ContainerHasActiveAuditLog, expected_station mismatch, out-of-range docked, multi-item list
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            audit_container,
            InventoryItemRecord {
                item_id: audit_container,
                type_id: 3468,
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
        handle_ships_mastery_and_fitting_ext(&ctx, &mut dk_corv, None, "invbroker", "TrashItems", &[PyRep::Integer(audit_container)], &HashMap::new())
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ContainerHasActiveAuditLog"
    );
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut dk_corv, None, "invbroker", "TrashItems", &[PyRep::Integer(trash_a), PyRep::Integer(other_station_id)], &HashMap::new())
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "DeleteFailed"
    );
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut dk_corv, None, "invbroker", "TrashItems", &[PyRep::Integer(distant_item)], &HashMap::new())
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "DeleteFailed"
    );
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut sp, None, "invbroker", "TrashItems", &[PyRep::Integer(inside_ship_id)], &HashMap::new())
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "DeleteFailed"
    );
    let multi = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut dk_corv,
        None,
        "invbroker",
        "TrashItems",
        &[PyRep::List(vec![PyRep::Integer(trash_a), PyRep::Integer(trash_b)])],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(mk) = multi else { panic!("expected KeyVal"); };
    assert!(mk.iter().any(|(k, v)| k == "count" && v == &PyRep::Integer(2)));

    // 5. GetDirtTimestamp & ResetDirtTimestamp error & fallback paths + unknown dispatch
    let mut no_ship_dirt = dk_corv.clone();
    no_ship_dirt.ship_id = None;
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut no_ship_dirt, None, "ship", "GetDirtTimestamp", &[], &HashMap::new())
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidArguments"
    );
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut dk_corv, None, "ship", "GetDirtTimestamp", &[PyRep::Integer(88_888_888)], &HashMap::new())
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ItemNotFound"
    );
    let dirt_fallback = handle_ships_mastery_and_fitting_ext(&ctx, &mut dk_corv, None, "ship", "GetDirtTimestamp", &[PyRep::Integer(dock_pod)], &HashMap::new())
        .await
        .unwrap()
        .unwrap()
        .as_i64()
        .unwrap();
    assert!(dirt_fallback > 130_000_000_000_000_000);

    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut no_ship_dirt, None, "ship", "ResetDirtTimestamp", &[], &HashMap::new())
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidArguments"
    );
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut dk_corv, None, "ship", "ResetDirtTimestamp", &[PyRep::Integer(88_888_888)], &HashMap::new())
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ItemNotFound"
    );
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut dk_corv, None, "ship", "ResetDirtTimestamp", &[PyRep::Integer(no_dirt_ship)], &HashMap::new())
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ShipOwnershipViolation"
    );
    let other_svc = "other";
    assert!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut dk_corv, None, other_svc, "GetDirtTimestamp", &[], &HashMap::new())
            .await
            .is_none()
    );
}
