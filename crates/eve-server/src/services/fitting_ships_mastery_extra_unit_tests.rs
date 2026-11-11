//! Extra unit and mutation-killing tests for `services::inventory_fitting::fitting_ships_mastery`
//! covering in-space `Eject` and `Board` (range, crimewatch timers, capsule cleanup, structure control),
//! `LaunchFromContainer`, `CreateNewbieShip` guards, `GetRequiredSkillLevels`, mastery/certificate errors,
//! `SetShipKillCounter` role & ownership checks, `TrashItems` (fitted, locked, asset safety, non-empty,
//! and in-space empty jetcan despawn), `GetDirtTimestamp` / `ResetDirtTimestamp`, and `SafeLogoff`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::inventory_fitting::fitting_ships_mastery::handle_ships_mastery_and_fitting_ext;
use crate::services::space_destiny::clear_session_change_timer;
use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord, WorldRepository};
use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

#[cfg(test)]
#[path = "fitting_containers_trash_and_kills_unit_tests.rs"]
mod fitting_containers_trash_and_kills_unit_tests;

pub(super) fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

pub(super) fn make_ctx() -> ServerContext {
    ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    )
}

pub(super) fn test_ctx() -> ServerContext {
    make_ctx()
}

pub(super) fn make_session(
    char_id: i64,
    station_id: Option<i64>,
    sys_id: u32,
    ship_id: i64,
) -> SessionState {
    let mut s = SessionState::new(801);
    s.user_id = Some(1);
    s.character_id = Some(char_id);
    s.corporation_id = Some(98_000_001);
    s.solar_system_id = Some(sys_id);
    s.station_id = station_id;
    s.ship_id = Some(ship_id);
    s
}

pub(super) fn make_sess(
    char_id: i64,
    sys_id: Option<u32>,
    station_id: Option<i64>,
    ship_id: Option<i64>,
) -> SessionState {
    let mut s = SessionState::new(802);
    s.user_id = Some(1);
    s.character_id = Some(char_id);
    s.corporation_id = Some(98_000_001);
    s.solar_system_id = sys_id;
    s.station_id = station_id;
    s.ship_id = ship_id;
    s
}

pub(super) fn make_char(
    character_id: i64,
    name: &str,
    corporation_id: i64,
    solar_system_id: u32,
    active_ship_id: i64,
) -> eve_db::CharacterRecord {
    eve_db::CharacterRecord {
        activity_status: None,
        character_id,
        user_id: 1,
        name: name.into(),
        corporation_id,
        alliance_id: None,
        war_faction_id: None,
        station_id: None,
        structure_id: None,
        solar_system_id,
        constellation_id: 20_000_020,
        region_id: 10_000_002,
        active_ship_id,
        balance: eve_db::IskCents(0),
        plex_balance: 0,
        loyalty_points: HashMap::new(),
        standings: HashMap::new(),
        security_status: 0.0,
        clone_grade: 2,
        home_station_id: 60_003_760,
        race_id: 1,
        bloodline_id: 1,
        ancestry_id: 1,
        school_id: 11,
        neural: eve_db::NeuralAttributes::default(),
        skill_points: 0.0,
        free_skill_points: 0.0,
        last_clone_jump_unix: 0,
        jump_fatigue_minutes: 0.0,
        jump_cooldown_until_unix: 0,
        jump_fatigue_until_unix: 0,
        description: String::new(),
        title: String::new(),
        gender: 1,
        create_date_unix: 0,
        fleet_id: None,
        autopilot_waypoints: Vec::new(),
        version: 1,
        system_epoch: 1,
    }
}

#[tokio::test]
async fn eject_and_board_in_space_and_docked_guards_and_capsule_transitions() {
    let ctx = make_ctx();
    let char_id = 96_500_001_i64;
    let other_char = 96_500_002_i64;
    let sys_id = 30_000_142_u32;
    let station_id = 60_003_760_i64;
    let ship1_id = 96_500_101_i64;
    let ship2_id = 96_500_102_i64;
    let not_ship_id = 96_500_103_i64;
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = 1;
        ch.corporation_id = 1_000_009;
        ch.station_id = Some(station_id);
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship1_id;
        snap.characters.insert(char_id, ch);

        snap.items.insert(
            ship1_id,
            InventoryItemRecord {
                item_id: ship1_id,
                type_id: 587, // Rifter
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
            ship2_id,
            InventoryItemRecord {
                item_id: ship2_id,
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
            not_ship_id,
            InventoryItemRecord {
                item_id: not_ship_id,
                type_id: 34, // Tritanium (not category 6)
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

    let mut sess = SessionState::new(801);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(1_000_009);
    sess.station_id = Some(station_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = None;

    // 1. NoActiveShip on Eject / LeaveShip
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut sess, None, "ship", "LeaveShip", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NoActiveShip"
    );

    sess.ship_id = Some(ship1_id);
    // 2. CannotEjectWhileDocked when docked
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut sess, None, "ship", "Eject", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CannotEjectWhileDocked"
    );

    // 3. Docked Board: NotAShip, OwnershipViolation, ShipNotBoardable, and valid docked switch
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(
            &ctx,
            &mut sess,
            None,
            "ship",
            "Board",
            &[PyRep::Integer(not_ship_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "NotAShip"
    );

    ctx.db.with_mut_snapshot(|s| {
        s.items.get_mut(&ship2_id).unwrap().owner_id = other_char;
    });
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(
            &ctx,
            &mut sess,
            None,
            "ship",
            "Board",
            &[PyRep::Integer(ship2_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "OwnershipViolation"
    );

    ctx.db.with_mut_snapshot(|s| {
        let it = s.items.get_mut(&ship2_id).unwrap();
        it.owner_id = char_id;
        it.singleton = false;
    });
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(
            &ctx,
            &mut sess,
            None,
            "ship",
            "Board",
            &[PyRep::Integer(ship2_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "ShipNotBoardable"
    );

    ctx.db.with_mut_snapshot(|s| {
        s.items.get_mut(&ship2_id).unwrap().singleton = true;
    });
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(
            &ctx,
            &mut sess,
            None,
            "ship",
            "Board",
            &[PyRep::Integer(ship2_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "ShipHasSkillPrerequisites"
    );
    ctx.db.with_mut_snapshot(|s| {
        let mut demo_skills = s.skills.values().next().cloned().unwrap_or_default();
        for (&attr_id, &val) in ctx
            .sde
            .get_type(587)
            .into_iter()
            .flat_map(|t| t.default_attributes.iter())
        {
            if matches!(attr_id, 182 | 183 | 184 | 1285 | 1289 | 1290) && val > 0.0 {
                demo_skills.insert(
                    val as u32,
                    TrainedSkillRecord {
                        type_id: val as u32,
                        level: 5,
                        skill_points: 256_000.0,
                    },
                );
            }
        }
        s.skills.insert(char_id, demo_skills);
    });
    let _ = ctx.db.acquire_online_lock(ctx.db.with_snapshot(|s| s.characters.get(&char_id).map_or(1, |c| c.user_id)), 1, Some(char_id), false, ctx.now_unix());
    let docked_board = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut sess,
        None,
        "ship",
        "Board",
        &[PyRep::Integer(ship2_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::Tuple(ship_state) = docked_board else {
        panic!("Expected 4-tuple shipState from ship.Board, got {docked_board:?}");
    };
    assert_eq!(ship_state.len(), 4);
    assert!(ship_state.iter().all(|part| matches!(part, PyRep::Dict(_))));
    assert_eq!(sess.ship_id, Some(ship2_id));
    let board_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        board_notifs.iter().any(|n| n.event == "ProcessGodmaPrimeLocation"
            && n.args.first() == Some(&PyRep::Integer(ship2_id))),
        "ship.Board must emit ProcessGodmaPrimeLocation for newly boarded ship"
    );
    clear_session_change_timer(&ctx, char_id);

    // 4. Move to space and test LeaveShip error, weapon/criminal timers on Eject, and valid Eject -> Board in space
    sess.station_id = None;
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&char_id).unwrap().station_id = None;
        let it = s.items.get_mut(&ship2_id).unwrap();
        it.location_id = sys_id as i64;
        it.flag = InventoryFlag::None;
    });
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h
        .spawn_ball_with_meta(
            Ball {
                id: ship2_id,
                type_id: 587,
                position: DVec3::new(1_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
            SlimItemMeta::default(),
        )
        .await;

    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut sess, None, "ship", "LeaveShip", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CannotLeaveShipInSpace"
    );

    // Active weapon timer blocks Eject
    let now_u = ctx.now_unix_secs();
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.states.entry(char_id).or_default().weapon_timer_until_unix = now_u + 60;
    }
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut sess, None, "ship", "Eject", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CannotEjectActiveWeaponsTimer"
    );

    // Criminal timer in high-sec blocks Eject
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        let st = cw.states.entry(char_id).or_default();
        st.weapon_timer_until_unix = 0;
        st.criminal_flag_until_unix = now_u + 60;
    }
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut sess, None, "ship", "Eject", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CannotEjectInvolvedInCrime"
    );
    {
        ctx.crimewatch.write().unwrap().states.clear();
    }

    // Valid in-space Eject spawns capsule and switches session.ship_id to capsule
    let eject_res =
        handle_ships_mastery_and_fitting_ext(&ctx, &mut sess, None, "ship", "Eject", &[], &kw)
            .await
            .unwrap()
            .unwrap();
    let capsule_id = eject_res
        .dict_get("capsuleID")
        .and_then(PyRep::as_i64)
        .unwrap();
    assert_eq!(
        eject_res.dict_get("abandonedShipID").and_then(PyRep::as_i64),
        Some(ship2_id)
    );
    assert_eq!(sess.ship_id, Some(capsule_id));

    // Cannot eject from capsule
    clear_session_change_timer(&ctx, char_id);
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut sess, None, "ship", "Eject", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CannotEjectFromCapsule"
    );

    // In-space Board back into ship2_id from capsule removes capsule and restores ship2_id
    let board_space = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut sess,
        None,
        "ship",
        "Board",
        &[PyRep::Integer(ship2_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::Tuple(space_ship_state) = board_space else {
        panic!("Expected 4-tuple shipState from in-space ship.Board, got {board_space:?}");
    };
    assert_eq!(space_ship_state.len(), 4);
    assert!(space_ship_state.iter().all(|part| matches!(part, PyRep::Dict(_))));
    assert_eq!(sess.ship_id, Some(ship2_id));
    let space_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        space_notifs.iter().any(|n| n.event == "OnSessionChanged"),
        "OnSessionChanged expected on in-space ship.Board"
    );
    assert!(
        space_notifs.iter().any(|n| n.event == "ProcessGodmaPrimeLocation"
            && n.args.first() == Some(&PyRep::Integer(ship2_id))),
        "ProcessGodmaPrimeLocation expected on in-space ship.Board"
    );
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&capsule_id)));
}

#[tokio::test]
async fn create_newbie_ship_from_active_ship_with_args_spawns_starter_fit_and_notifications() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let station_id = 60_003_760_i64;
    let rifter_id = ctx.db.with_mut_snapshot(|s| {
        s.items.retain(|_, i| !(i.owner_id == char_id && matches!(i.type_id, 588 | 596 | 601 | 606)));
        let ch = s.characters.get_mut(&char_id).unwrap();
        ch.station_id = Some(station_id);
        ch.solar_system_id = 30_000_142;
        ch.race_id = 1; // Caldari -> Ibis (601)
        s.next_entity_id += 1;
        let rid = s.next_entity_id;
        s.items.insert(
            rid,
            InventoryItemRecord {
                item_id: rid,
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
        ch.active_ship_id = rid;
        rid
    });
    let mut sess = make_session(char_id, Some(station_id), 30_000_142, rifter_id);
    let uid = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
    let _ = ctx.db.acquire_online_lock(uid, 1, Some(char_id), false, ctx.now_unix());
    let _ = ctx.drain_notifications_for_char(char_id);
    let kw = HashMap::new();

    // V24 client sends CreateNewbieShip(shipID, locationID) directly from Rifter
    let res = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "CreateNewbieShip",
        &[PyRep::Integer(rifter_id), PyRep::Integer(station_id)],
        &kw,
    )
    .await
    .expect("handled")
    .expect("CreateNewbieShip ok from active Rifter");
    let new_id = res.dict_get("shipID").and_then(PyRep::as_i64).unwrap();
    assert_eq!(res.dict_get("typeID").and_then(PyRep::as_i64), Some(601));
    assert_eq!(sess.ship_id, Some(new_id));

    // Civilian Miner (3651) in HiSlot0 and 1x Tritanium (34) in Cargo
    ctx.db.with_snapshot(|s| {
        assert!(s.items.values().any(|i| i.location_id == new_id && i.type_id == 3651 && i.flag == InventoryFlag::HiSlot0 && i.singleton));
        assert!(s.items.values().any(|i| i.location_id == new_id && i.type_id == 34 && i.flag == InventoryFlag::Cargo && i.quantity == 1));
    });
    let notifs = ctx.drain_notifications_for_char(char_id);
    assert!(notifs.iter().any(|n| n.event == "OnItemChange"));
    assert!(notifs.iter().any(|n| n.event == "OnPodOrCorvetteAssembly"));
    assert!(notifs.iter().any(|n| n.event == "OnSessionChanged"));
}

#[tokio::test]
async fn ship_board_returns_4_tuple_ship_state_and_primes_fitted_modules() {
    let ctx = test_ctx();
    let char_id = 91_201_i64;
    let station_id = 60_003_760_i64;
    let sys_id = 30_000_142_u32;
    let old_ship = 91_202_i64;
    let venture_id = 91_203_i64;
    let miner1_id = 91_204_i64;
    let miner2_id = 91_205_i64;
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|s| {
        let mut ch = make_char(char_id, "BoardMiner", 98_000_001, sys_id, old_ship);
        ch.station_id = Some(station_id);
        let mut demo_skills = s.skills.values().next().cloned().unwrap_or_default();
        for (&attr_id, &val) in ctx
            .sde
            .get_type(32880)
            .into_iter()
            .flat_map(|t| t.default_attributes.iter())
        {
            if matches!(attr_id, 182 | 183 | 184 | 1285 | 1289 | 1290) && val > 0.0 {
                demo_skills.insert(
                    val as u32,
                    TrainedSkillRecord {
                        type_id: val as u32,
                        level: 5,
                        skill_points: 256_000.0,
                    },
                );
            }
        }
        s.skills.insert(char_id, demo_skills);
        s.characters.insert(char_id, ch);
        for (id, tid, loc, flag) in [
            (old_ship, 587_u32, station_id, InventoryFlag::Hangar),
            (venture_id, 32880_u32, station_id, InventoryFlag::Hangar),
            (miner1_id, 483_u32, venture_id, InventoryFlag::HiSlot0),
            (miner2_id, 483_u32, venture_id, InventoryFlag::HiSlot1),
        ] {
            s.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: loc,
                    flag,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        s.weapon_banks.insert(venture_id, HashMap::from([(miner1_id, vec![miner1_id, miner2_id])]));
    });

    let mut sess = make_sess(char_id, Some(sys_id), Some(station_id), Some(old_ship));
    let _ = ctx.db.acquire_online_lock(ctx.db.with_snapshot(|s| s.characters.get(&char_id).map_or(1, |c| c.user_id)), 1, Some(char_id), false, ctx.now_unix());
    let _ = ctx.drain_notifications_for_char(char_id);
    let rep = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut sess,
        None,
        "ship",
        "Board",
        &[PyRep::Integer(venture_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("ship.Board venture");

    let PyRep::Tuple(ship_state) = rep else {
        panic!("ship.Board must return 4-tuple shipState, got {rep:?}");
    };
    assert_eq!(ship_state.len(), 4);
    assert!(matches!(ship_state[0], PyRep::Dict(_)));
    assert!(matches!(ship_state[1], PyRep::Dict(_)));
    let PyRep::Dict(wb_entries) = &ship_state[2] else {
        panic!("wbData must be Dict");
    };
    assert_eq!(wb_entries.len(), 1);
    assert_eq!(wb_entries[0].0, PyRep::Integer(miner1_id));
    assert!(matches!(ship_state[3], PyRep::Dict(_)));

    let notifs = ctx.drain_notifications_for_char(char_id);
    let prime = notifs
        .iter()
        .find(|n| n.event == "ProcessGodmaPrimeLocation")
        .expect("ProcessGodmaPrimeLocation emitted on ship.Board");
    assert_eq!(prime.args.len(), 2);
    assert_eq!(prime.args[0], PyRep::Integer(venture_id));
    let PyRep::Dict(prime_items) = &prime.args[1] else {
        panic!("ProcessGodmaPrimeLocation data must be Dict");
    };
    for expected_id in [venture_id, miner1_id, miner2_id] {
        assert!(
            prime_items.iter().any(|(k, _)| *k == PyRep::Integer(expected_id)),
            "ProcessGodmaPrimeLocation must prime item {expected_id}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ships_mastery_board_eject_launch_killmark_repair_kill_all_mutants() {
    use crate::system_actor::ActiveModuleCycle;
    use eve_db::{ContractRecord, InventoryFlag, InventoryItemRecord, WorldRepository};
    use eve_destiny::{Ball, CombatState, MovementMode, WarpPhase};
    use eve_sde::attr_ids;
    use glam::DVec3;

    let ctx = make_ctx();
    let sys_id: u32 = 30_000_142;
    let station_id: i64 = 60_003_760;
    let char_id: i64 = 90_991_001;
    let other_char_id: i64 = 90_991_002;
    let corp_id: i64 = 98_991_001;
    ctx.db.provision_gm_account("m_pilot1", "pw", char_id, "MPilot1");
    ctx.db.provision_gm_account("m_pilot2", "pw", other_char_id, "MPilot2");

    let ship_a: i64 = 1_991_001; // Rifter 587
    let ship_b: i64 = 1_991_002; // Rifter 587 unmanned
    let ship_c: i64 = 1_991_003; // Unrelated Rifter 587 with active cycle
    let ship_unowned: i64 = 1_991_004;
    let cap_id: i64 = 1_991_005;
    let launch_ship_1: i64 = 1_991_010;
    let launch_ship_2: i64 = 1_991_011;
    let mod_dmg_id: i64 = 1_991_020;
    let mod_ok_id: i64 = 1_991_021;

    ctx.db.with_mut_snapshot(|snap| {
        for (cid, active_s) in [(char_id, ship_a), (other_char_id, ship_c)] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.corporation_id = corp_id;
                ch.station_id = None;
                ch.structure_id = None;
                ch.solar_system_id = sys_id;
                ch.active_ship_id = active_s;
            }
        }
        for (iid, owner, tid, loc, flag, singleton, dmg, armor_dmg) in [
            (ship_a, char_id, 587_u32, sys_id as i64, InventoryFlag::None, true, 120.0, 180.0),
            (ship_b, char_id, 587, sys_id as i64, InventoryFlag::None, true, 0.0, 0.0),
            (ship_c, char_id, 587, sys_id as i64, InventoryFlag::None, true, 0.0, 0.0),
            (ship_unowned, 0_i64, 587, sys_id as i64, InventoryFlag::None, true, 0.0, 0.0),
            (cap_id, char_id, 670, sys_id as i64, InventoryFlag::None, true, 0.0, 0.0),
            (launch_ship_1, char_id, 587, ship_b, InventoryFlag::Cargo, true, 0.0, 0.0),
            (launch_ship_2, char_id, 587, ship_b, InventoryFlag::Cargo, true, 0.0, 0.0),
            (mod_dmg_id, char_id, 484, ship_a, InventoryFlag::LoSlot0, true, 40.0, 0.0),
            (mod_ok_id, char_id, 484, ship_a, InventoryFlag::LoSlot1, true, 0.0, 0.0),
        ] {
            let mut dyn_attrs = HashMap::new();
            if dmg > 0.0 {
                dyn_attrs.insert(attr_ids::DAMAGE, dmg);
            }
            if armor_dmg > 0.0 {
                dyn_attrs.insert(attr_ids::ARMOR_DAMAGE, armor_dmg);
            }
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: owner,
                    location_id: loc,
                    flag,
                    quantity: 1,
                    singleton,
                    dynamic_attributes: dyn_attrs,
                    ..Default::default()
                },
            );
            if loc == sys_id as i64 {
                snap.item_positions.insert(iid, [10_000.0, 0.0, 0.0]);
            }
        }
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    for (bid, pos, mode) in [
        (ship_a, DVec3::new(10_000.0, 0.0, 0.0), MovementMode::Stop),
        (ship_b, DVec3::new(10_500.0, 0.0, 0.0), MovementMode::Stop),
        (ship_c, DVec3::new(12_000.0, 0.0, 0.0), MovementMode::Stop),
        (ship_unowned, DVec3::new(11_000.0, 0.0, 0.0), MovementMode::Stop),
    ] {
        let _ = sys
            .spawn_ball_with_combat(
                Ball {
                    id: bid,
                    type_id: 587,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 300.0,
                    inertia: 1.0,
                    mass: 1_000_000.0,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode,
                },
                CombatState::default(),
            )
            .await;
    }

    // Active cycles on ship_a and unrelated ship_c
    let _ = sys
        .add_module_cycle(ActiveModuleCycle {
            ship_id: ship_a,
            module_id: 77_001,
            module_type_id: 484,
            duration_s: 10.0,
            repeat: 1,
            ..Default::default()
        })
        .await;
    let _ = sys
        .add_module_cycle(ActiveModuleCycle {
            ship_id: ship_c,
            module_id: 77_002,
            module_type_id: 484,
            duration_s: 10.0,
            repeat: 1,
            ..Default::default()
        })
        .await;

    let mut sp_sess = SessionState::new(901);
    sp_sess.user_id = Some(ctx.db.with_snapshot(|s| s.characters.get(&char_id).map_or(1, |c| c.user_id)));
    sp_sess.character_id = Some(char_id);
    sp_sess.corporation_id = Some(corp_id);
    sp_sess.solar_system_id = Some(sys_id);
    sp_sess.ship_id = Some(ship_a);

    // 1. Dispatch boundary: unrelated service returns None; BoardStoredShip covered
    assert!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut sp_sess, None, "marketProxy", "GetMarketGroups", &[], &HashMap::new())
            .await
            .is_none()
    );
    assert!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut sp_sess, None, "ship", "BoardStoredShip", &[], &HashMap::new())
            .await
            .is_some()
    );

    // 2. Board ShipInWarp both ways + ShipNotBoardable clauses + non-capsule swap preserving ship_c cycles + vacated slim meta
    {
        let mut bp = sys.ballpark.write().unwrap();
        bp.balls.get_mut(&ship_a).unwrap().mode = MovementMode::Warp {
            origin: DVec3::ZERO,
            destination: DVec3::new(1e8, 0.0, 0.0),
            warp_speed_au_s: 3.0,
            total_distance: 1e8,
            phase: WarpPhase::Cruising,
            elapsed_warp_time: 1.0,
        };
    }
    let err_warp_src = handle_ships_mastery_and_fitting_ext(
        &ctx, &mut sp_sess, None, "ship", "Board", &[PyRep::Integer(ship_b)], &HashMap::new(),
    )
    .await.unwrap().unwrap_err();
    assert_eq!(err_warp_src.code, "ShipInWarp");
    {
        let mut bp = sys.ballpark.write().unwrap();
        bp.balls.get_mut(&ship_a).unwrap().mode = MovementMode::Stop;
        bp.balls.get_mut(&ship_b).unwrap().mode = MovementMode::Warp {
            origin: DVec3::ZERO,
            destination: DVec3::new(1e8, 0.0, 0.0),
            warp_speed_au_s: 3.0,
            total_distance: 1e8,
            phase: WarpPhase::Cruising,
            elapsed_warp_time: 1.0,
        };
    }
    let err_warp_tgt = handle_ships_mastery_and_fitting_ext(
        &ctx, &mut sp_sess, None, "ship", "Board", &[PyRep::Integer(ship_b)], &HashMap::new(),
    )
    .await.unwrap().unwrap_err();
    assert_eq!(err_warp_tgt.code, "ShipInWarp");
    {
        let mut bp = sys.ballpark.write().unwrap();
        bp.balls.get_mut(&ship_b).unwrap().mode = MovementMode::Stop;
    }

    // ShipNotBoardable clauses: piloted by other_char, non-singleton, contract-locked
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(
            &ctx, &mut sp_sess, None, "ship", "Board", &[PyRep::Integer(ship_c)], &HashMap::new(),
        ).await.unwrap().unwrap_err().code,
        "ShipNotBoardable"
    );
    ctx.db.with_mut_snapshot(|snap| { snap.items.get_mut(&ship_b).unwrap().singleton = false; });
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(
            &ctx, &mut sp_sess, None, "ship", "Board", &[PyRep::Integer(ship_b)], &HashMap::new(),
        ).await.unwrap().unwrap_err().code,
        "ShipNotBoardable"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_b).unwrap().singleton = true;
        snap.contracts.insert(88_001, ContractRecord {
            contract_id: 88_001,
            status: "Outstanding".into(),
            item_ids: vec![ship_b],
            ..Default::default()
        });
    });
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(
            &ctx, &mut sp_sess, None, "ship", "Board", &[PyRep::Integer(ship_b)], &HashMap::new(),
        ).await.unwrap().unwrap_err().code,
        "ShipNotBoardable"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.contracts.clear();
        let skills = snap.skills.entry(char_id).or_default();
        for (skill_tid, lvl) in ctx.sde.required_skills_for_type(587) {
            skills.insert(
                skill_tid,
                TrainedSkillRecord {
                    type_id: skill_tid,
                    level: lvl,
                    skill_points: 256_000.0,
                },
            );
        }
    });

    // Valid non-capsule Board(ship_a -> ship_b): stops ship_a cycle, keeps ship_c cycle, vacates ship_a slim meta with corp_id
    handle_ships_mastery_and_fitting_ext(
        &ctx, &mut sp_sess, None, "ship", "Board", &[PyRep::Integer(ship_b)], &HashMap::new(),
    )
    .await.unwrap().expect("Board ship_b ok");
    assert_eq!(sp_sess.ship_id, Some(ship_b));
    assert!(!sys.has_active_module_cycle_sync(ship_a, 77_001));
    assert!(sys.has_active_module_cycle_sync(ship_c, 77_002));
    {
        let bp = sys.ballpark.read().unwrap();
        let m = bp.ball_slim_meta.get(&ship_a).expect("vacated ship_a slim meta");
        assert_eq!(m.char_id, None);
        assert_eq!(m.corp_id, corp_id);
        let mb = bp.ball_slim_meta.get(&ship_b).expect("boarded ship_b slim meta");
        assert_eq!(mb.char_id, Some(char_id));
    }

    // Clear active session-change cooldown from the prior Board(ship_b) so ownership guard is reached
    ctx.session_change_timers.lock().unwrap().remove(&char_id);
    // Board ship_unowned (owner_id == 0) is rejected with OwnershipViolation
    let err = handle_ships_mastery_and_fitting_ext(
        &ctx, &mut sp_sess, None, "ship", "Board", &[PyRep::Integer(ship_unowned)], &HashMap::new(),
    )
    .await.unwrap().expect_err("Board ship_unowned rejected");
    assert_eq!(err.code, "OwnershipViolation");

    // 3. LaunchFromContainer 1-arg & 2-arg arms ("ship" and "eject")
    assert!(
        handle_ships_mastery_and_fitting_ext(
            &ctx, &mut sp_sess, None, "ship", "LaunchFromContainer",
            &[PyRep::Integer(launch_ship_1)], &HashMap::new(),
        ).await.unwrap().is_ok()
    );
    assert!(
        handle_ships_mastery_and_fitting_ext(
            &ctx, &mut sp_sess, None, "eject", "LaunchShipOrContainerFromWreckOrContainer",
            &[PyRep::Integer(ship_b), PyRep::Integer(launch_ship_2)], &HashMap::new(),
        ).await.unwrap().is_ok()
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&launch_ship_1].location_id, sys_id as i64);
        assert_eq!(snap.items[&launch_ship_2].location_id, sys_id as i64);
    });

    // 4. shipKillCounter & CreateNewbieShip (NotDocked vs docked) & GetDamagedModulesAndRepairCost (hull + armor + module)
    assert!(
        handle_ships_mastery_and_fitting_ext(
            &ctx, &mut sp_sess, None, "dogmaIM", "CreateNewbieShip", &[], &HashMap::new(),
        ).await.unwrap().is_err()
    );
    let prev_role = sp_sess.role;
    sp_sess.role = u64::MAX;
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(
            &ctx, &mut sp_sess, None, "shipKillCounter", "SetShipKillCounter",
            &[PyRep::Integer(ship_b), PyRep::Integer(5)], &HashMap::new(),
        ).await.unwrap().unwrap(),
        PyRep::Integer(5)
    );
    sp_sess.role = prev_role;
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(
            &ctx, &mut sp_sess, None, "shipKillCounter", "GetItemKillCountPlayer",
            &[PyRep::Integer(ship_b)], &HashMap::new(),
        ).await.unwrap().unwrap(),
        PyRep::Integer(5)
    );

    // Docked repair quote on ship_a (hull 120 + armor 180 + mod_dmg 40 = 340 HP -> 170,000 ISK)
    let mut dock_sess = SessionState::new(902);
    dock_sess.user_id = sp_sess.user_id;
    dock_sess.character_id = Some(char_id);
    dock_sess.corporation_id = Some(corp_id);
    dock_sess.station_id = Some(station_id);
    dock_sess.solar_system_id = Some(sys_id);
    dock_sess.ship_id = Some(ship_a);
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.station_id = Some(station_id);
        ch.active_ship_id = ship_a;
        snap.items.get_mut(&ship_a).unwrap().location_id = station_id;
    });
    let quote = handle_ships_mastery_and_fitting_ext(
        &ctx, &mut dock_sess, None, "repairSvc", "GetDamagedModulesAndRepairCost",
        &[PyRep::Integer(ship_a)], &HashMap::new(),
    ).await.unwrap().expect("GetDamagedModulesAndRepairCost ok");
    assert_eq!(quote.dict_get("shipID").and_then(|v| v.as_i64()), Some(ship_a));
    assert_eq!(quote.dict_get("totalDamagedHP").and_then(|v| v.as_f64()), Some(340.0));
    assert_eq!(quote.dict_get("repairCostISK").and_then(|v| v.as_f64()), Some(170_000.0));

    // 5. Eject with synthetic SDE missing type 670 -> exercises fallback CombatState 17 fields & > 0.0 bounds
    let mut no_cap_sde = (*shared_sde()).clone();
    no_cap_sde.types.remove(&670);
    let ctx_nocap = ServerContext::with_sde_and_repository(
        Arc::new(no_cap_sde),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    ctx_nocap.db.provision_gm_account("eject_nocap", "pw", char_id, "EjectNoCap");
    ctx_nocap.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_b;
        }
        snap.items.insert(
            ship_b,
            InventoryItemRecord {
                item_id: ship_b,
                type_id: 587,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    let sys_nc = ctx_nocap.systems.get_or_spawn(sys_id);
    let _ = sys_nc
        .spawn_ball_with_combat(
            Ball {
                id: ship_b,
                type_id: 587,
                position: DVec3::new(20_000.0, 0.0, 0.0),
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
    let mut nc_sess = SessionState::new(903);
    nc_sess.user_id = Some(ctx_nocap.db.with_snapshot(|s| s.characters[&char_id].user_id));
    nc_sess.character_id = Some(char_id);
    nc_sess.corporation_id = Some(corp_id);
    nc_sess.solar_system_id = Some(sys_id);
    nc_sess.ship_id = Some(ship_b);
    let _ = handle_ships_mastery_and_fitting_ext(
        &ctx_nocap, &mut nc_sess, None, "ship", "Eject", &[], &HashMap::new(),
    ).await.unwrap().expect("Eject without type 670 ok");
    let new_cap_id = nc_sess.ship_id.expect("capsule id");
    assert_ne!(new_cap_id, ship_b);
    let (_, cap_ball) = sys_nc.get_ball_snapshot(new_cap_id).await.unwrap().unwrap();
    let cap_cs = sys_nc.get_combat_state(new_cap_id).await.unwrap().unwrap();
    assert!(cap_ball.max_velocity > 0.0 && cap_ball.inertia > 0.0 && cap_ball.mass > 0.0 && cap_ball.radius > 0.0);
    assert!(cap_cs.shield_hp > 0.0 && cap_cs.max_shield_hp == cap_cs.shield_hp);
    assert!(cap_cs.armor_hp > 0.0 && cap_cs.max_armor_hp == cap_cs.armor_hp);
    assert!(cap_cs.structure_hp > 0.0 && cap_cs.max_structure_hp == cap_cs.structure_hp);
    assert!(cap_cs.capacitor > 0.0 && cap_cs.max_capacitor == cap_cs.capacitor);
    assert!(cap_cs.shield_recharge_time_ms > 0.0 && cap_cs.capacitor_recharge_time_ms > 0.0);
    assert!(cap_cs.signature_radius > 0.0);
    assert!(cap_cs.shield_resonance.iter().all(|&r| r > 0.0 && r <= 1.0));
    assert!(cap_cs.armor_resonance.iter().all(|&r| r > 0.0 && r <= 1.0));
    assert!(cap_cs.structure_resonance.iter().all(|&r| r > 0.0 && r <= 1.0));
}

