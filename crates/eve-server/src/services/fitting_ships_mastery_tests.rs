//! Unit tests for `fitting_ships_mastery` (`LeaveShip`, `CreateNewbieShip`, `GetRequiredSkillLevels`, `GetCurrCharMasteryLevel`, `GetCertificatesForShipByMasteryLevel`, `GetSkillsRequiredForMasteryLevel`, `GetItemKillCountPlayer`, `SetShipKillCounter`, `GetDamagedModulesAndRepairCost`, `GetShipFittingInfo`, `TrashItems`, `GetDirtTimestamp`, `ResetDirtTimestamp`).

use super::*;
use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

#[tokio::test]
async fn ships_mastery_leave_ship_corvette_mastery_killmarks_repair_simulation_trash_and_dirt() {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let char_id = 96_400_001i64;
    let station_id = 60_003_760i64;
    let ship_id = 96_400_100i64;
    let trash_item_id = 96_400_200i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = 1;
        ch.name = "MasteryPilot".into();
        ch.corporation_id = 1_000_009;
        ch.race_id = 2; // Minmatar -> Reaper
        ch.station_id = Some(station_id);
        ch.solar_system_id = 30_000_142;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);

        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587, // Rifter
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                dynamic_attributes: HashMap::from([(attr_ids::DAMAGE, 15.0)]),
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            trash_item_id,
            InventoryItemRecord {
                item_id: trash_item_id,
                type_id: 34, // Tritanium
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mut session = SessionState::default();
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    session.corporation_id = Some(1_000_009);
    session.station_id = Some(station_id);
    session.solar_system_id = Some(30_000_142);
    session.ship_id = Some(ship_id);
    session.role = u64::MAX; // GM role for SetShipKillCounter

    // 1. GetRequiredSkillLevels, GetCurrCharMasteryLevel, GetCertificatesForShipByMasteryLevel, GetSkillsRequiredForMasteryLevel
    let req_skills = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut session,
        None,
        "dogmaIM",
        "GetRequiredSkillLevels",
        &[PyRep::Integer(587)],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("GetRequiredSkillLevels ok");
    assert_ne!(req_skills, PyRep::None);

    let mastery_lvl = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut session,
        None,
        "ship",
        "GetCurrCharMasteryLevel",
        &[PyRep::Integer(587)],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("GetCurrCharMasteryLevel ok");
    assert_eq!(mastery_lvl, PyRep::Integer(0));

    let certs_by_lvl = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut session,
        None,
        "dogmaIM",
        "GetCertificatesForShipByMasteryLevel",
        &[PyRep::Integer(587), PyRep::Integer(1)],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("GetCertificatesForShipByMasteryLevel ok");
    assert_ne!(certs_by_lvl, PyRep::None);

    let skills_by_lvl = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut session,
        None,
        "ship",
        "GetSkillsRequiredForMasteryLevel",
        &[PyRep::Integer(587), PyRep::Integer(1)],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("GetSkillsRequiredForMasteryLevel ok");
    assert_ne!(skills_by_lvl, PyRep::None);

    // 2. Kill counter & Repair cost & Dirt timestamp & GetShipFittingInfo
    let set_kills = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut session,
        None,
        "shipKillCounter",
        "SetShipKillCounter",
        &[PyRep::Integer(ship_id), PyRep::Integer(7)],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("SetShipKillCounter ok");
    assert_eq!(set_kills, PyRep::Integer(7));

    let get_kills = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut session,
        None,
        "shipKillCounter",
        "GetItemKillCountPlayer",
        &[PyRep::Integer(ship_id)],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("GetItemKillCountPlayer ok");
    assert_eq!(get_kills, PyRep::Integer(7));

    let repair_info = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut session,
        None,
        "repairSvc",
        "GetDamagedModulesAndRepairCost",
        &[PyRep::Integer(ship_id)],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("GetDamagedModulesAndRepairCost ok");
    assert_ne!(repair_info, PyRep::None);

    let sim_info = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut session,
        None,
        "ship",
        "GetShipFittingInfo",
        &[
            PyRep::Integer(587),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(484),
                PyRep::Integer(27),
            ])]),
        ],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("GetShipFittingInfo ok");
    assert_ne!(sim_info, PyRep::None);

    let dirt_ts = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut session,
        None,
        "ship",
        "ResetDirtTimestamp",
        &[PyRep::Integer(ship_id)],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("ResetDirtTimestamp ok");
    let got_dirt = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut session,
        None,
        "ship",
        "GetDirtTimestamp",
        &[PyRep::Integer(ship_id)],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("GetDirtTimestamp ok");
    assert_eq!(dirt_ts, got_dirt);

    // 3. TrashItems
    let _ = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut session,
        None,
        "invbroker",
        "TrashItems",
        &[PyRep::List(vec![PyRep::Integer(trash_item_id)])],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("TrashItems ok");
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&trash_item_id)));

    // 4. LeaveShip -> Capsule -> remove old ship -> CreateNewbieShip
    let pod_res = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut session,
        None,
        "ship",
        "LeaveShip",
        &[],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("LeaveShip ok");
    assert!(matches!(pod_res, PyRep::Integer(id) if id > 0));

    // Remove the Rifter from station so CreateNewbieShip issues a free corvette
    ctx.db.with_mut_snapshot(|s| {
        s.items.remove(&ship_id);
    });
    let corvette_res = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut session,
        None,
        "dogmaIM",
        "CreateNewbieShip",
        &[],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("CreateNewbieShip ok");
    assert_ne!(corvette_res, PyRep::None);

    // Issue #603: CreateNewbieShip while docked in a regular non-corvette ship leaves it in hangar,
    // issues a Corvette, and emits OnItemChange + OnSessionChanged; calling again inside Corvette fails.
    let rifter2_id = 96_400_300i64;
    ctx.db.with_mut_snapshot(|s| {
        s.items.retain(|_, i| !(i.owner_id == char_id && i.location_id == station_id && i.type_id != 587));
        s.items.insert(
            rifter2_id,
            InventoryItemRecord {
                item_id: rifter2_id,
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
        if let Some(ch) = s.characters.get_mut(&char_id) {
            ch.active_ship_id = rifter2_id;
        }
        s.item_custom_names.remove(&(-8_800_000 - char_id));
    });
    session.ship_id = Some(rifter2_id);
    ctx.db
        .acquire_online_lock(1, session.session_id.max(1), Some(char_id), true, ctx.now_unix())
        .expect("register online session for CreateNewbieShip notif");
    let _ = ctx.drain_notifications_for_char(char_id);
    let board_corv = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut session,
        None,
        "dogmaIM",
        "CreateNewbieShip",
        &[PyRep::Integer(rifter2_id), PyRep::Integer(station_id)],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("CreateNewbieShip from non-capsule ship must succeed");
    let new_corv_id = board_corv.dict_get("shipID").and_then(PyRep::as_i64).unwrap();
    assert_eq!(session.ship_id, Some(new_corv_id));
    assert!(ctx.db.with_snapshot(|s| {
        s.items.get(&rifter2_id).is_some_and(|i| i.location_id == station_id && i.flag == InventoryFlag::Hangar)
    }));
    let notifs = ctx.drain_notifications_for_char(char_id);
    assert!(notifs.iter().any(|n| n.event == "OnItemChange"));
    assert!(notifs.iter().any(|n| n.event == "OnSessionChanged"));

    ctx.db.with_mut_snapshot(|s| {
        s.item_custom_names.remove(&(-8_800_000 - char_id));
    });
    let err_already_corv = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut session,
        None,
        "dogmaIM",
        "CreateNewbieShip",
        &[],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect_err("CreateNewbieShip while already in corvette must fail");
    assert_eq!(err_already_corv.code, "AlreadyInNewbieShip");
}

#[tokio::test]
async fn eject_in_space_sets_capsule_name_and_queues_set_state() {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let char_id = 96_500_201_i64;
    let sys_id = 30_000_142_u32;
    let ship_id = 96_500_202_i64;
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|s| {
        let mut ch = s.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = 1;
        ch.name = "EjectPilot".into();
        ch.corporation_id = 98_000_001;
        ch.station_id = None;
        ch.structure_id = None;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        s.characters.insert(char_id, ch);
        s.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    sys.try_spawn_ball_with_meta(
        eve_destiny::Ball {
            id: ship_id,
            type_id: 587,
            position: glam::DVec3::new(10_000.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.0,
            mass: 1_100_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        },
        eve_destiny::CombatState::default(),
        ctx.build_ship_slim_meta(char_id, 587),
    );

    let mut sess = SessionState::new(9_729);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(98_000_001);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    let _ = handle_ships_mastery_and_fitting_ext(&ctx, &mut sess, None, "ship", "Eject", &[], &kw)
        .await
        .unwrap()
        .expect("ship.Eject in space");

    let cap_id = sess.ship_id.expect("new capsule id");
    assert_ne!(cap_id, ship_id);

    let cap_slim = sys
        .get_ball_slim_item(cap_id)
        .await
        .unwrap()
        .expect("capsule slim item");
    assert!(
        !cap_slim
            .dict_get("name")
            .and_then(PyRep::as_str)
            .unwrap_or("")
            .is_empty(),
        "ejected capsule must carry non-empty name (#728/#729)"
    );

    let (_tick, events, _rep) = sys.drain_observer_updates(cap_id).await.unwrap();
    assert!(
        events.iter().any(|e| matches!(
            e,
            eve_destiny::DestinyEvent::SetState { observer_ball_id } if *observer_ball_id == cap_id
        )),
        "ship.Eject must queue SetState for new capsule (#729)"
    );
}
