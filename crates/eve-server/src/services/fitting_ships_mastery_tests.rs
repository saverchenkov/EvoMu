//! Unit tests for `fitting_ships_mastery` (`LeaveShip`, `CreateNewbieShip`, `GetRequiredSkillLevels`, `GetCurrCharMasteryLevel`, `GetCertificatesForShipByMasteryLevel`, `GetSkillsRequiredForMasteryLevel`, `GetItemKillCountPlayer`, `SetShipKillCounter`, `GetDamagedModulesAndRepairCost`, `GetShipFittingInfo`, `TrashItems`, `GetDirtTimestamp`, `ResetDirtTimestamp`).

use super::*;
use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

#[cfg(test)]
#[path = "fitting_ships_mastery_cov_edges_tests.rs"]
mod fitting_ships_mastery_cov_edges_tests;

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| {
        let mut sde = SdeRegistry::with_canonical_seed();
        for (aid, name) in [
            (306u16, "maxVelocityModifier"),
            (560, "cloakingTargetingDelay"),
            (852, "siegeModeWarpStatus"),
            (1035, "covertOpsStealthBomberTargettingDelay"),
            (2343, "disallowTethering"),
            (2354, "disallowDocking"),
            (2453, "disallowDriveJumping"),
        ] {
            sde.attributes.entry(aid).or_insert_with(|| eve_sde::DogmaAttributeType {
                attribute_id: aid,
                name: name.into(),
                default_value: 0.0,
                high_is_good: true,
                stackable: false,
                published: true,
            });
        }
        let mut bastion = sde
            .types
            .values()
            .next()
            .expect("canonical seed has types")
            .clone();
        bastion.type_id = 33_400;
        bastion.group_id = 515;
        bastion.category_id = 7;
        bastion.name = "Bastion Module I".into();
        bastion.default_attributes = HashMap::from([(852, 1.0)]);
        sde.types.entry(33_400).or_insert(bastion);
        Arc::new(sde)
    }))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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

    // Remove the Rifter from station and grant Spaceship Command I (3327) so CreateNewbieShip
    // issues a free corvette (#1433).
    ctx.db.with_mut_snapshot(|s| {
        s.items.remove(&ship_id);
        s.skills.entry(char_id).or_default().insert(
            3327,
            eve_db::TrainedSkillRecord {
                type_id: 3327,
                level: 1,
                skill_points: 250.0,
            },
        );
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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
        !events.iter().any(|e| matches!(e, eve_destiny::DestinyEvent::SetState { .. })),
        "ship.Eject must NOT queue SetState for new capsule (#992); got {events:?}"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            eve_destiny::DestinyEvent::AddBalls2 { balls }
                if balls.iter().any(|b| b.id == cap_id)
        )),
        "ship.Eject must queue incremental AddBalls2 for new capsule (#992); got {events:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn in_space_eject_and_ship_to_ship_board_lifecycle_and_guards() {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let char_id = 96_500_301_i64;
    let other_char_id = 96_500_302_i64;
    let sys_id = 30_000_142_u32;
    let ship_a = 96_500_310_i64;
    let ship_b = 96_500_311_i64;
    let ship_manned = 96_500_312_i64;
    let mod_a = 96_500_320_i64;
    let bastion_mod_id = 96_500_321_i64;
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|s| {
        let seed_ch = s.characters.values().next().cloned().expect("seed char");
        let mut ch = seed_ch.clone();
        ch.character_id = char_id;
        ch.user_id = 1;
        ch.name = "BoardPilot".into();
        ch.corporation_id = 98_000_001;
        ch.station_id = None;
        ch.structure_id = None;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_a;
        ch.clone_grade = 2; // Omega
        s.characters.insert(char_id, ch);
        let mut pilot_skills = s.skills.values().next().cloned().unwrap_or_default();
        if let Some(t) = ctx.sde.types.get(&587) {
            for &aid in &[182_u16, 183, 184] {
                if let Some(&req_sid) = t.default_attributes.get(&aid) {
                    pilot_skills.insert(
                        req_sid as u32,
                        eve_db::TrainedSkillRecord {
                            type_id: req_sid as u32,
                            level: 5,
                            skill_points: 256_000.0,
                        },
                    );
                }
            }
        }
        s.skills.insert(char_id, pilot_skills);

        let mut ch2 = seed_ch;
        ch2.character_id = other_char_id;
        ch2.user_id = 2;
        ch2.name = "OtherPilot".into();
        ch2.corporation_id = 98_000_001;
        ch2.station_id = None;
        ch2.structure_id = None;
        ch2.solar_system_id = sys_id;
        ch2.active_ship_id = ship_manned;
        s.characters.insert(other_char_id, ch2);

        for sid in [ship_a, ship_b, ship_manned] {
            s.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
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
        }
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    for (sid, pos, vel, mode, pilot_opt) in [
        (
            ship_a,
            glam::DVec3::new(10_000.0, 0.0, 0.0),
            glam::DVec3::new(300.0, 0.0, 0.0),
            eve_destiny::MovementMode::GotoDirection {
                direction: glam::DVec3::X,
            },
            Some(char_id),
        ),
        (
            ship_b,
            glam::DVec3::new(10_600.0, 0.0, 0.0),
            glam::DVec3::ZERO,
            eve_destiny::MovementMode::Stop,
            None,
        ),
        (
            ship_manned,
            glam::DVec3::new(10_800.0, 0.0, 0.0),
            glam::DVec3::ZERO,
            eve_destiny::MovementMode::Stop,
            Some(other_char_id),
        ),
    ] {
        let mut meta = ctx.build_ship_slim_meta(pilot_opt.unwrap_or(char_id), 587);
        meta.char_id = pilot_opt;
        sys.try_spawn_ball_with_meta(
            eve_destiny::Ball {
                id: sid,
                type_id: 587,
                position: pos,
                velocity: vel,
                max_velocity: 350.0,
                inertia: 2.0,
                mass: 1_100_000.0,
                radius: 40.0,
                speed_fraction: if vel.length_squared() > 0.0 { 1.0 } else { 0.0 },
                mode,
            },
            eve_destiny::CombatState::default(),
            meta,
        );
    }

    // Seed active module cycle and target lock on ship_a before Eject.
    let _ = sys
        .add_module_cycle(crate::system_actor::ActiveModuleCycle {
            ship_id: ship_a,
            module_id: mod_a,
            target_id: Some(ship_b),
            effect_name: "afterburner".into(),
            duration_s: 10.0,
            elapsed_s: 0.0,
            cap_need_per_cycle: 0.0,
            ..Default::default()
        })
        .await;
    {
        let mut bp = sys.ballpark.write().unwrap_or_else(|e| e.into_inner());
        bp.register_observer(ship_a);
        bp.register_observer(ship_manned);
        bp.target_locks.entry(ship_a).or_default().push(eve_destiny::TargetLock {
            target_id: ship_b,
            lock_time_ms: 2_000.0,
            remaining_lock_ms: 0.0,
            locked: true,
            max_range_m: None,
        });
    }

    let mut sess = SessionState::new(9_730);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(98_000_001);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_a);

    // 1. Negative guard: cannot Eject while immobilized (e.g. Siege / Bastion).
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(
            bastion_mod_id,
            InventoryItemRecord {
                item_id: bastion_mod_id,
                type_id: 33_400,
                owner_id: char_id,
                location_id: ship_a,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        s.module_active_states.insert(bastion_mod_id);
    });
    let immob_err = handle_ships_mastery_and_fitting_ext(&ctx, &mut sess, None, "ship", "Eject", &[], &kw)
        .await
        .unwrap()
        .expect_err("Eject must reject immobilized ship");
    assert_eq!(immob_err.code, "CantJumpWhileModuleActive");
    ctx.db.with_mut_snapshot(|s| {
        s.module_active_states.remove(&bastion_mod_id);
        s.items.remove(&bastion_mod_id);
    });

    // 2. Negative guard: cannot Eject while in active warp.
    let _ = sys
        .set_movement_mode(
            ship_a,
            eve_destiny::MovementMode::Warp {
                origin: glam::DVec3::new(10_000.0, 0.0, 0.0),
                destination: glam::DVec3::new(2_000_000.0, 0.0, 0.0),
                warp_speed_au_s: 3.0,
                total_distance: 1_990_000.0,
                phase: eve_destiny::WarpPhase::Cruising,
                elapsed_warp_time: 5.0,
            },
        )
        .await;
    let warp_eject_err = handle_ships_mastery_and_fitting_ext(&ctx, &mut sess, None, "ship", "Eject", &[], &kw)
        .await
        .unwrap()
        .expect_err("Eject must reject active warp");
    assert_eq!(warp_eject_err.code, "ShipInWarp");
    let _ = sys.force_stop_warp(ship_a);
    let _ = sys
        .set_movement_mode(
            ship_a,
            eve_destiny::MovementMode::GotoDirection {
                direction: glam::DVec3::X,
            },
        )
        .await
        .expect("restore GotoDirection after force_stop_warp");

    // 3. Successful in-space Eject: stops ship_a, initializes capsule HP, updates ball_interactive_overrides,
    //    clears cycles/locks, and persists both positions to snap.item_positions.
    let _ = handle_ships_mastery_and_fitting_ext(&ctx, &mut sess, None, "ship", "Eject", &[], &kw)
        .await
        .unwrap()
        .expect("Eject in space");
    let cap_id = sess.ship_id.expect("capsule id");
    let (_, old_ball_after_eject) = sys.get_ball_snapshot(ship_a).await.unwrap().unwrap();
    assert!(
        matches!(old_ball_after_eject.mode, eve_destiny::MovementMode::Stop),
        "ejected ship must be stopped, got {:?}",
        old_ball_after_eject.mode
    );
    let cap_cs = sys.get_combat_state(cap_id).await.unwrap().unwrap();
    assert!(cap_cs.max_shield_hp > 0.0 && cap_cs.max_armor_hp > 0.0 && cap_cs.max_structure_hp > 0.0);
    {
        let bp = sys.ballpark.read().unwrap_or_else(|e| e.into_inner());
        assert_eq!(bp.ball_interactive_overrides.get(&ship_a), Some(&false));
        assert_eq!(bp.ball_slim_meta.get(&ship_a).and_then(|m| m.char_id), None);
        assert!(bp.target_locks.get(&ship_a).is_none_or(|v| v.is_empty()));
        assert!(
            bp.observer_events.get(&ship_a).is_none_or(|v| v.is_empty()),
            "HF1005-02: Eject must not strand observer_events under vacated ship_a: {:?}",
            bp.observer_events.get(&ship_a)
        );
    }
    let (_t_ej, cap_events, _) = sys.drain_observer_updates(cap_id).await.unwrap();
    assert!(
        !cap_events.iter().any(|e| matches!(e, eve_destiny::DestinyEvent::SetState { .. })),
        "ship.Eject must NOT emit SetState (#992); got {cap_events:?}"
    );
    assert!(
        cap_events.iter().any(|e| matches!(e, eve_destiny::DestinyEvent::Stop { ball_id } if *ball_id == ship_a)),
        "HF1005-02: pre-handoff Stop(ship_a) must carry into capsule drain; got {cap_events:?}"
    );
    assert!(
        cap_events.iter().any(|e| matches!(
            e,
            eve_destiny::DestinyEvent::OnSpecialFX { ball_id, guid, is_active: false, .. }
                if *ball_id == ship_a && !guid.is_empty()
        )),
        "HF1005-02: pre-handoff module deactivation OnSpecialFX must carry into capsule drain; got {cap_events:?}"
    );
    let add_cap_count = cap_events
        .iter()
        .filter(|e| matches!(
            e,
            eve_destiny::DestinyEvent::AddBalls2 { balls }
                if balls.iter().any(|b| b.id == cap_id)
        ))
        .count();
    assert_eq!(
        add_cap_count, 1,
        "HF1005-02: AddBalls2 for cap_id must be emitted exactly once; got {cap_events:?}"
    );
    assert!(sys.actor_state.read().unwrap().active_module_cycles.iter().all(|c| c.ship_id != ship_a));
    ctx.db.with_snapshot(|s| {
        assert!(s.item_positions.contains_key(&ship_a));
        assert!(s.item_positions.contains_key(&cap_id));
    });

    // 4. Board ship_a from capsule (removes capsule item & ball, restores interactive=true).
    ctx.advance_secs(12);
    let _ = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut sess,
        None,
        "ship",
        "Board",
        &[PyRep::Integer(ship_a)],
        &kw,
    )
    .await
    .unwrap()
    .expect("Board ship_a from capsule");
    assert_eq!(sess.ship_id, Some(ship_a));
    assert!(sys.get_ball_snapshot(cap_id).await.unwrap().is_none());
    {
        let bp = sys.ballpark.read().unwrap_or_else(|e| e.into_inner());
        assert_eq!(bp.ball_interactive_overrides.get(&ship_a), Some(&true));
        assert_eq!(bp.ball_slim_meta.get(&ship_a).and_then(|m| m.char_id), Some(char_id));
    }

    // 5. Negative guard: cannot Board another pilot's manned ship.
    ctx.advance_secs(12);
    let manned_err = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut sess,
        None,
        "ship",
        "Board",
        &[PyRep::Integer(ship_manned)],
        &kw,
    )
    .await
    .unwrap()
    .expect_err("Cannot board manned ship");
    assert_eq!(manned_err.code, "ShipNotBoardable");

    // 6. Ship-to-ship Board: moving ship_a -> unmanned ship_b stops ship_a, sets interactive(ship_a, false),
    //    sets ship_a slim char_id=None, sets interactive(ship_b, true), sets ship_b slim char_id=Some(char_id).
    let _ = sys
        .set_movement_mode(
            ship_a,
            eve_destiny::MovementMode::GotoDirection {
                direction: glam::DVec3::Y,
            },
        )
        .await;
    let _ = sys.drain_observer_updates(ship_manned).await;
    let _ = sys.drain_observer_updates(ship_a).await;
    let _ = sys.drain_observer_updates(ship_b).await;
    ctx.advance_secs(12);
    let _ = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut sess,
        None,
        "ship",
        "Board",
        &[PyRep::Integer(ship_b)],
        &kw,
    )
    .await
    .unwrap()
    .expect("Ship-to-ship Board");
    assert_eq!(sess.ship_id, Some(ship_b));
    let (_, ship_a_after_board) = sys.get_ball_snapshot(ship_a).await.unwrap().unwrap();
    assert!(matches!(ship_a_after_board.mode, eve_destiny::MovementMode::Stop));
    {
        let bp = sys.ballpark.read().unwrap_or_else(|e| e.into_inner());
        assert_eq!(bp.ball_interactive_overrides.get(&ship_a), Some(&false));
        assert_eq!(bp.ball_slim_meta.get(&ship_a).and_then(|m| m.char_id), None);
        assert_eq!(bp.ball_interactive_overrides.get(&ship_b), Some(&true));
        assert_eq!(bp.ball_slim_meta.get(&ship_b).and_then(|m| m.char_id), Some(char_id));
        assert!(
            bp.observer_events.get(&ship_a).is_none_or(|v| v.is_empty()),
            "HF1005-02: ship-to-ship Board must not strand observer_events under vacated ship_a: {:?}",
            bp.observer_events.get(&ship_a)
        );
    }
    let (_tb, pilot_board_events, _) = sys.drain_observer_updates(ship_b).await.unwrap();
    assert!(
        !pilot_board_events.iter().any(|e| matches!(e, eve_destiny::DestinyEvent::SetState { .. })),
        "ship-to-ship Board must NOT emit SetState (#992); got {pilot_board_events:?}"
    );
    assert!(
        pilot_board_events.iter().any(|e| matches!(e, eve_destiny::DestinyEvent::Stop { ball_id } if *ball_id == ship_a)),
        "HF1005-02: pre-handoff Stop(ship_a) must carry into ship_b drain; got {pilot_board_events:?}"
    );
    let (_t, obs_events, _) = sys.drain_observer_updates(ship_manned).await.unwrap();
    assert!(obs_events.iter().any(|e| matches!(
        e,
        eve_destiny::DestinyEvent::SetBallInteractive { ball_id, is_interactive: false } if *ball_id == ship_a
    )));
    assert!(obs_events.iter().any(|e| matches!(
        e,
        eve_destiny::DestinyEvent::SetBallInteractive { ball_id, is_interactive: true } if *ball_id == ship_b
    )));
}

#[tokio::test]
async fn ship_board_uses_surface_distance_for_large_titan_hull() {
    let mut ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let sde_mut = Arc::make_mut(&mut ctx.sde);
    let titan_tid = 11_567_u32;
    let mut titan_t = sde_mut.types.get(&587).expect("seed type").clone();
    titan_t.type_id = titan_tid;
    titan_t.group_id = 30;
    titan_t.category_id = 6;
    titan_t.radius = 6_800.0;
    titan_t.mass = 2_400_000_000.0;
    titan_t.name = "Avatar".into();
    titan_t.default_attributes.insert(4, 2_400_000_000.0);
    sde_mut.types.insert(titan_tid, titan_t);

    let sys_id = 30_000_142_u32;
    let char_id = 90_000_001_i64;
    let cap_id = 96_600_100_i64;
    let titan_id = 96_600_101_i64;
    let st_pos = DVec3::new(100_000.0, 0.0, 0.0);

    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = cap_id;
        }
        s.items.insert(
            cap_id,
            InventoryItemRecord {
                item_id: cap_id,
                type_id: 670,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        s.items.insert(
            titan_id,
            InventoryItemRecord {
                item_id: titan_id,
                type_id: titan_tid,
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
    sys.try_spawn_ball_with_combat(
        eve_destiny::Ball {
            id: cap_id,
            type_id: 670,
            position: st_pos,
            velocity: DVec3::ZERO,
            max_velocity: 335.0,
            inertia: 0.25,
            mass: 16_000.0,
            radius: 15.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        },
        eve_destiny::CombatState::default(),
    );
    sys.try_spawn_ball_with_combat(
        eve_destiny::Ball {
            id: titan_id,
            type_id: titan_tid,
            position: st_pos + DVec3::new(9_350.0, 0.0, 0.0), // surface dist = 9350 - 15 - 6800 = 2535 m > 2500 m
            velocity: DVec3::ZERO,
            max_velocity: 60.0,
            inertia: 0.5,
            mass: 2_400_000_000.0,
            radius: 6_800.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        },
        eve_destiny::CombatState::default(),
    );

    let mut sess = SessionState::new(9_830);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(cap_id);
    let kw = HashMap::new();

    // Surface distance 2535 m (> 2500 m) still rejects with surface distance reported.
    let too_far = handle_ships_mastery_and_fitting_ext(&ctx, &mut sess, None, "ship", "Board", &[PyRep::Integer(titan_id)], &kw)
        .await
        .unwrap()
        .expect_err("surface distance 2535 m must be out of boarding range");
    assert_eq!(too_far.code, "ShipOutOfBoardingRange");
    assert!(too_far.message.contains("2535.0"), "msg={}", too_far.message);

    // Move Titan so center-to-center is 7,000 m (> 2500 m) but surface distance is 185 m (<= 2500 m).
    sys.try_set_ball_position(titan_id, st_pos + DVec3::new(7_000.0, 0.0, 0.0));
    let boarded = handle_ships_mastery_and_fitting_ext(&ctx, &mut sess, None, "ship", "Board", &[PyRep::Integer(titan_id)], &kw)
        .await
        .unwrap()
        .expect("boarding Titan at 185 m surface distance must succeed (#1004)");
    assert_eq!(sess.ship_id, Some(titan_id));
    assert!(
        matches!(boarded, PyRep::Tuple(ref t) if t.len() == 4),
        "expected 4-tuple ship_state from ship.Board, got {boarded:?}"
    );
}

#[tokio::test]
async fn eject_in_space_missing_hull_ball_rejects_without_mutating_db_or_spawning_origin_pod() {
    use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::{
        clone_char_with, test_ctx_with_clock,
    };

    let ctx = test_ctx_with_clock(1_800_000_000);
    let sys_id = 30_000_142_u32;
    let char_id = 9_850_001_i64;
    let corp_id = 98_600_850_i64;
    let ship_id = 9_850_002_i64;
    let ship_type_id = ctx
        .sde
        .types
        .values()
        .find(|t| t.category_id == 6 && t.group_id == 25)
        .or_else(|| ctx.sde.types.values().find(|t| t.category_id == 6 && t.type_id != 670))
        .map(|t| t.type_id)
        .unwrap_or(587);

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = clone_char_with(snap, char_id, "Missing Ball Eject", corp_id, sys_id, ship_id);
        ch.station_id = None;
        ch.structure_id = None;
        snap.characters.insert(char_id, ch);
        snap.items.insert(
            ship_id,
            eve_db::InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_type_id,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    assert!(
        sys.get_ball_snapshot(ship_id).await.unwrap().is_none(),
        "precondition: ship ball is absent from ballpark"
    );
    let (pre_items, pre_next_id) = ctx
        .db
        .with_snapshot(|snap| (snap.items.len(), snap.next_entity_id));

    let mut sess = SessionState::new(9_850);
    sess.user_id = Some(char_id as u64);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    let kw = HashMap::new();

    let err = handle_ships_mastery_and_fitting_ext(&ctx, &mut sess, None, "ship", "Eject", &[], &kw)
        .await
        .expect("handled")
        .expect_err("ship.Eject must reject when active hull ball is missing from system actor");
    assert_eq!(err.code, "ShipNotInSpace");

    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items.len(), pre_items, "missing ball Eject must not create capsule");
        assert_eq!(
            snap.next_entity_id, pre_next_id,
            "missing ball Eject must not allocate entity IDs"
        );
        assert_eq!(
            snap.characters[&char_id].active_ship_id, ship_id,
            "missing ball Eject must keep original active_ship_id"
        );
        let ship = &snap.items[&ship_id];
        assert_eq!(ship.owner_id, char_id);
        assert_eq!(ship.location_id, i64::from(sys_id));
        assert!(
            !snap
                .items
                .values()
                .any(|it| it.owner_id == char_id && it.type_id == 670),
            "no capsule item may be minted on rejected Eject"
        );
    });
    assert_eq!(sess.ship_id, Some(ship_id));
    assert!(
        !sys.ballpark
            .write()
            .unwrap()
            .balls
            .values()
            .any(|b| b.type_id == 670),
        "no capsule ball may be spawned near star origin (150, 0, 0)"
    );
}

#[tokio::test]
async fn active_cloak_and_invalid_args_reject_eject_cleanly_while_gate_cloak_allows() {
    use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::{
        clone_char_with, test_ctx_with_clock,
    };

    let ctx = test_ctx_with_clock(1_800_000_000);
    let sys_id = 30_000_142_u32;
    let char_id = 9_860_001_i64;
    let corp_id = 98_600_860_i64;
    let ship_id = 9_860_002_i64;
    let ship_type_id = ctx
        .sde
        .types
        .values()
        .find(|t| t.category_id == 6 && t.group_id == 25)
        .or_else(|| ctx.sde.types.values().find(|t| t.category_id == 6 && t.type_id != 670))
        .map(|t| t.type_id)
        .unwrap_or(587);

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = clone_char_with(snap, char_id, "Cloak Eject", corp_id, sys_id, ship_id);
        ch.station_id = None;
        ch.structure_id = None;
        snap.characters.insert(char_id, ch);
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_type_id,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    sys.spawn_ball_with_combat(
        eve_destiny::Ball {
            id: ship_id,
            type_id: ship_type_id,
            position: glam::DVec3::new(15_000.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 320.0,
            inertia: 2.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        },
        eve_destiny::CombatState::default(),
    )
    .await
    .expect("spawn test hull");

    let mut sess = SessionState::new(9_860);
    sess.user_id = Some(char_id as u64);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    let kw = HashMap::new();
    let expected_ship_cloaked_ce =
        BridgeRpcError::from_user_key(eve_proto::UserErrorKey::ShipCloaked).client_error;

    // 1. Active module cloak (non-covert and covert) must reject ship.Eject cleanly without mutating DB or ballpark.
    for is_covert_cloak in [false, true] {
        sys.set_combat_state(
            ship_id,
            eve_destiny::CombatState {
                is_cloaked: true,
                gate_cloak_remaining_ticks: 0,
                is_covert_cloak,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let epoch_0 = ctx.db.state_epoch();
        let tx_0 = ctx.db.tx_commit_count();
        let err = handle_ships_mastery_and_fitting_ext(&ctx, &mut sess, None, "ship", "Eject", &[], &kw)
            .await
            .expect("handled")
            .expect_err("cloaked ship.Eject must fail");
        assert_eq!(err.code, "ShipCloaked");
        assert_eq!(err.client_error, expected_ship_cloaked_ce);
        assert_eq!(sess.ship_id, Some(ship_id));
        assert_eq!(ctx.db.state_epoch(), epoch_0);
        assert_eq!(ctx.db.tx_commit_count(), tx_0);
        let cs = sys.get_combat_state(ship_id).await.unwrap().unwrap();
        assert!(cs.is_cloaked);
        assert_eq!(cs.gate_cloak_remaining_ticks, 0);
    }

    // 1b. Unsynced active cloak module in `snap.module_active_states` (`cs.is_cloaked == false`)
    //     must ALSO reject ship.Eject cleanly with UserErrorKey::ShipCloaked and zero mutations.
    sys.set_combat_state(ship_id, eve_destiny::CombatState::default())
        .await
        .unwrap();
    let cloak_mod_id = 9_860_003_i64;
    let cloak_tid = ctx
        .sde
        .types
        .iter()
        .find(|(_, t)| t.group_id == 330)
        .map(|(&tid, _)| tid)
        .unwrap_or(11_577);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            cloak_mod_id,
            InventoryItemRecord {
                item_id: cloak_mod_id,
                type_id: cloak_tid,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_active_states.insert(cloak_mod_id);
    });
    let epoch_unsynced = ctx.db.state_epoch();
    let tx_unsynced = ctx.db.tx_commit_count();
    let unsynced_err = handle_ships_mastery_and_fitting_ext(&ctx, &mut sess, None, "ship", "Eject", &[], &kw)
        .await
        .expect("handled")
        .expect_err("unsynced active cloak module must reject ship.Eject");
    assert_eq!(unsynced_err.code, "ShipCloaked");
    assert_eq!(unsynced_err.client_error, expected_ship_cloaked_ce);
    assert_eq!(sess.ship_id, Some(ship_id));
    assert_eq!(ctx.db.state_epoch(), epoch_unsynced);
    assert_eq!(ctx.db.tx_commit_count(), tx_unsynced);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.remove(&cloak_mod_id);
        snap.items.remove(&cloak_mod_id);
    });

    // 2. Invalid / mismatched optional pickid (and [PyRep::None] + kwargs["shipID"]) must reject before any mutation.
    sys.set_combat_state(ship_id, eve_destiny::CombatState::default())
        .await
        .unwrap();
    let epoch_0 = ctx.db.state_epoch();
    let tx_0 = ctx.db.tx_commit_count();
    let bad_pick = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut sess,
        None,
        "ship",
        "Eject",
        &[PyRep::Integer(ship_id + 1)],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("mismatched Eject pickid must reject");
    assert_eq!(bad_pick.code, "InvalidShip");
    assert_eq!(ctx.db.state_epoch(), epoch_0);
    assert_eq!(ctx.db.tx_commit_count(), tx_0);
    let mut kw_ship = HashMap::new();
    kw_ship.insert("shipID".to_string(), PyRep::Integer(ship_id + 2));
    let bad_none_ship = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut sess,
        None,
        "ship",
        "Eject",
        &[PyRep::None],
        &kw_ship,
    )
    .await
    .expect("handled")
    .expect_err("[None] + mismatched kwargs[shipID] must reject");
    assert_eq!(bad_none_ship.code, "InvalidShip");
    assert_eq!(ctx.db.state_epoch(), epoch_0);
    assert_eq!(ctx.db.tx_commit_count(), tx_0);

    // 3. DB-docked character (even if session.station_id was stale None) must reject CannotEjectWhileDocked.
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().station_id = Some(60_003_760);
    });
    let epoch_docked = ctx.db.state_epoch();
    let tx_docked = ctx.db.tx_commit_count();
    let dock_err = handle_ships_mastery_and_fitting_ext(&ctx, &mut sess, None, "ship", "Eject", &[], &kw)
        .await
        .expect("handled")
        .expect_err("DB-docked Eject must fail");
    assert_eq!(dock_err.code, "CannotEjectWhileDocked");
    assert_eq!(ctx.db.state_epoch(), epoch_docked);
    assert_eq!(ctx.db.tx_commit_count(), tx_docked);

    // 3b. DB-resolved solar system security check: criminal in highsec rejects even if session.solar_system_id is None.
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().station_id = None;
    });
    {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        cw.get_or_create(char_id).criminal_flag_until_unix = ctx.now_unix_secs() + 60;
    }
    sess.solar_system_id = None;
    let crim_err = handle_ships_mastery_and_fitting_ext(&ctx, &mut sess, None, "ship", "Eject", &[], &kw)
        .await
        .expect("handled")
        .expect_err("highsec criminal must fail via DB-resolved sys_id");
    assert_eq!(crim_err.code, "CannotEjectInvolvedInCrime");
    {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        cw.get_or_create(char_id).criminal_flag_until_unix = 0;
    }
    sess.solar_system_id = Some(sys_id);

    // 4. Undock in DB, set gate cloak (`gate_cloak_remaining_ticks > 0`), pass matching shipID arg -> Eject succeeds.
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().station_id = None;
    });
    sys.set_combat_state(
        ship_id,
        eve_destiny::CombatState {
            is_cloaked: true,
            gate_cloak_remaining_ticks: 10,
            is_covert_cloak: false,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let ok = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut sess,
        None,
        "ship",
        "Eject",
        &[PyRep::Integer(ship_id)],
        &kw,
    )
    .await
    .expect("handled")
    .expect("gate-cloaked Eject with matching shipID must succeed");
    assert_eq!(ok.dict_get("abandonedShipID").and_then(PyRep::as_i64), Some(ship_id));
    assert_ne!(sess.ship_id, Some(ship_id));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn v24_create_newbie_ship_boards_existing_station_corvette_and_emits_typed_errors() {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let char_id = 96_501_411_i64;
    let station_id = 60_003_760_i64;
    let freighter_id = 96_501_420_i64;
    let locked_ibis_id = 96_501_421_i64;
    let non_racial_velator_id = 96_501_422_i64;
    let ibis_a_id = 96_501_423_i64;
    let ibis_b_id = 96_501_424_i64;
    let capsule_id = 96_501_425_i64;
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|s| {
        let mut ch = s.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = 1;
        ch.race_id = 1; // Caldari -> racial corvette Ibis (601)
        ch.name = "CorvetteBoarder".into();
        ch.corporation_id = 98_000_001;
        ch.solar_system_id = 30_000_142;
        ch.station_id = Some(station_id);
        ch.structure_id = None;
        ch.active_ship_id = freighter_id;
        s.characters.insert(char_id, ch);

        // Grant Spaceship Command (3327) + Caldari/Gallente Frigate (3330/3328) so
        // `find_boardable_corvette_in_hangar` skill checks pass for boardable test corvettes (#1417).
        let char_skills = s.skills.entry(char_id).or_default();
        for skill_id in [3327_u32, 3328_u32, 3330_u32] {
            char_skills.insert(
                skill_id,
                eve_db::TrainedSkillRecord {
                    type_id: skill_id,
                    level: 1,
                    skill_points: 250.0,
                },
            );
        }

        for (item_id, type_id, flag) in [
            (freighter_id, 587_u32, InventoryFlag::Hangar),
            (locked_ibis_id, 601_u32, InventoryFlag::Locked),
            (non_racial_velator_id, 606_u32, InventoryFlag::Hangar),
            (ibis_a_id, 601_u32, InventoryFlag::Hangar),
            (ibis_b_id, 601_u32, InventoryFlag::Hangar),
            (capsule_id, 670_u32, InventoryFlag::Hangar),
        ] {
            s.items.insert(
                item_id,
                InventoryItemRecord {
                    item_id,
                    type_id,
                    owner_id: char_id,
                    location_id: station_id,
                    flag,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });

    let items_before = ctx.db.with_snapshot(|s| s.items.len());
    let mut sess = SessionState::new(9_411);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(98_000_001);
    sess.solar_system_id = Some(30_000_142);
    sess.station_id = Some(station_id);
    sess.ship_id = Some(freighter_id);
    ctx.db
        .acquire_online_lock(1, sess.session_id, Some(char_id), true, ctx.now_unix())
        .expect("register online session");
    let _ = ctx.drain_notifications_for_char(char_id);

    // 1. Live V24 2-arg CreateNewbieShip(shipID, stationID) while docked in non-capsule ship
    //    must board the deterministic existing racial corvette in Hangar without spawning duplicates (#1411).
    let res = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "CreateNewbieShip",
        &[PyRep::Integer(freighter_id), PyRep::Integer(station_id)],
        &kw,
    )
    .await
    .expect("CreateNewbieShip handled")
    .expect("CreateNewbieShip must board existing Hangar corvette");

    assert_eq!(res.dict_get("shipID").and_then(PyRep::as_i64), Some(ibis_a_id));
    assert_eq!(res.dict_get("typeID").and_then(PyRep::as_i64), Some(601));
    assert_eq!(res.dict_get("raceID").and_then(PyRep::as_i64), Some(1));
    assert_eq!(sess.ship_id, Some(ibis_a_id));
    assert_eq!(
        ctx.db.with_snapshot(|s| (
            s.items.len(),
            s.characters.get(&char_id).map(|c| c.active_ship_id),
            s.items.get(&freighter_id).map(|i| (i.location_id, i.flag)),
            s.item_custom_names.contains_key(&(-8_800_000 - char_id)),
        )),
        (
            items_before,
            Some(ibis_a_id),
            Some((station_id, InventoryFlag::Hangar)),
            false,
        )
    );

    let notifs = ctx.drain_notifications_for_char(char_id);
    assert!(notifs.iter().any(|n| n.event == "OnItemChange"));
    assert!(notifs.iter().any(|n| n.event == "OnSessionChanged"));
    assert!(
        !notifs.iter().any(|n| n.event == "OnPodOrCorvetteAssembly"),
        "boarding existing corvette must not emit OnPodOrCorvetteAssembly"
    );

    // 2. Calling again while already in a corvette returns typed AlreadyInNewbieShip UserErrorKey,
    //    even when an active `claim_key` cooldown is set (HF1411-P1-01).
    let claim_key = -8_800_000 - char_id;
    let active_cd_ts = ctx.now_unix_secs().to_string();
    ctx.db.with_mut_snapshot(|s| {
        s.item_custom_names.insert(claim_key, active_cd_ts.clone());
    });
    let already_err = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "CreateNewbieShip",
        &[PyRep::Integer(ibis_a_id), PyRep::Integer(station_id)],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("must reject when already in corvette even with active cooldown");
    assert_eq!(already_err.code, "AlreadyInNewbieShip");
    assert!(matches!(
        already_err.to_client_error("dogmaIM", "CreateNewbieShip"),
        eve_proto::ClientError::User {
            key: eve_proto::UserErrorKey::AlreadyInNewbieShip,
            ..
        }
    ));

    // 2b. Active `claim_key` cooldown must NOT block V24 Hangar corvette boarding (HF1411-P1-01).
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&char_id).unwrap().active_ship_id = freighter_id;
    });
    sess.ship_id = Some(freighter_id);
    let board_during_cd = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "CreateNewbieShip",
        &[PyRep::Integer(freighter_id), PyRep::Integer(station_id)],
        &kw,
    )
    .await
    .expect("handled")
    .expect("active cooldown must not block boarding existing Hangar corvette");
    assert_eq!(
        board_during_cd.dict_get("shipID").and_then(PyRep::as_i64),
        Some(ibis_a_id)
    );
    assert_eq!(sess.ship_id, Some(ibis_a_id));
    assert_eq!(
        ctx.db.with_snapshot(|s| s.item_custom_names.get(&claim_key).cloned()),
        Some(active_cd_ts)
    );

    // 3. Legacy 0-arg call from capsule still preserves AlreadyHaveShipInStation, while V24 2-arg
    //    call from capsule falls back to boarding any remaining Hangar corvette (even non-racial Velator),
    //    and once no Hangar corvette remains, fresh spawn is blocked by active `NewbieShipCooldown`.
    ctx.db.with_mut_snapshot(|s| {
        s.items.remove(&ibis_a_id);
        s.items.remove(&ibis_b_id);
        s.characters.get_mut(&char_id).unwrap().active_ship_id = capsule_id;
    });
    sess.ship_id = Some(capsule_id);

    let legacy_err = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "CreateNewbieShip",
        &[],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("legacy 0-arg call from capsule must preserve AlreadyHaveShipInStation");
    assert_eq!(legacy_err.code, "AlreadyHaveShipInStation");

    let capsule_2arg = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "CreateNewbieShip",
        &[PyRep::Integer(capsule_id), PyRep::Integer(station_id)],
        &kw,
    )
    .await
    .expect("handled")
    .expect("V24 2-arg call from capsule must board remaining non-racial Hangar corvette");
    assert_eq!(
        capsule_2arg.dict_get("shipID").and_then(PyRep::as_i64),
        Some(non_racial_velator_id)
    );
    assert_eq!(
        capsule_2arg.dict_get("typeID").and_then(PyRep::as_i64),
        Some(606)
    );
    assert_eq!(sess.ship_id, Some(non_racial_velator_id));

    // 3b. With no eligible Hangar corvettes remaining and active `claim_key`, fresh spawn hits `NewbieShipCooldown`.
    ctx.db.with_mut_snapshot(|s| {
        s.items.remove(&non_racial_velator_id);
        s.characters.get_mut(&char_id).unwrap().active_ship_id = freighter_id;
    });
    sess.ship_id = Some(freighter_id);
    let spawn_cd_err = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "CreateNewbieShip",
        &[PyRep::Integer(freighter_id), PyRep::Integer(station_id)],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("fresh spawn with active claim_key must hit NewbieShipCooldown");
    assert_eq!(spawn_cd_err.code, "NewbieShipCooldown");
    assert_eq!(sess.ship_id, Some(freighter_id));

    // 4. Undocked returns NotDocked backed by typed UserErrorKey::MustBeDocked.
    sess.station_id = None;
    sess.structure_id = None;
    let undocked_err = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "CreateNewbieShip",
        &[PyRep::Integer(freighter_id), PyRep::Integer(station_id)],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("undocked must fail");
    assert_eq!(undocked_err.code, "NotDocked");
    assert!(matches!(
        undocked_err.to_client_error("dogmaIM", "CreateNewbieShip"),
        eve_proto::ClientError::User {
            key: eve_proto::UserErrorKey::MustBeDocked,
            ..
        }
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_newbie_ship_guards_structure_control_saved_ship_and_corvette_skill_prereqs() {
    let mut sde = (*shared_sde()).clone();
    // Require Caldari Frigate (3330) on Ibis (601) and Gallente Frigate (3328) on Velator (606)
    // to exercise cross-race / special-edition Corvette prerequisite filtering (#1417 / HF1411-P1-03).
    if let Some(ibis) = sde.types.get_mut(&601) {
        ibis.default_attributes.insert(182, 3327.0);
        ibis.default_attributes.insert(277, 1.0);
        ibis.default_attributes.insert(183, 3330.0);
        ibis.default_attributes.insert(278, 1.0);
    }
    if let Some(velator) = sde.types.get_mut(&606) {
        velator.default_attributes.insert(182, 3327.0);
        velator.default_attributes.insert(277, 1.0);
        velator.default_attributes.insert(183, 3328.0);
        velator.default_attributes.insert(278, 1.0);
    }
    let ctx = ServerContext::with_sde_and_repository(
        Arc::new(sde),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let char_id = 96_501_451_i64;
    let other_gunner_id = 96_501_452_i64;
    let corp_id = 98_000_001_i64;
    let sys_id = 30_000_142_u32;
    let structure_id = 1_000_000_514_001_i64;
    let other_structure_id = 1_000_000_514_002_i64;
    let freighter_id = 96_501_460_i64;
    let saved_ibis_id = 96_501_461_i64;
    let boardable_ibis_id = 96_501_462_i64;
    let untrained_velator_id = 96_501_463_i64;
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|s| {
        let mut ch = s.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = 1;
        ch.race_id = 1; // Caldari -> Ibis (601)
        ch.name = "StructureGuardPilot".into();
        ch.corporation_id = corp_id;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = Some(structure_id);
        ch.active_ship_id = structure_id;
        s.characters.insert(char_id, ch);

        // Pilot has Spaceship Command (3327) + Caldari Frigate (3330), NOT Gallente Frigate (3328).
        let skills = s.skills.entry(char_id).or_default();
        for skill_id in [3327_u32, 3330_u32] {
            skills.insert(
                skill_id,
                eve_db::TrainedSkillRecord {
                    type_id: skill_id,
                    level: 1,
                    skill_points: 250.0,
                },
            );
        }

        for sid in [structure_id, other_structure_id] {
            s.structures.insert(
                sid,
                eve_db::UpwellStructureRecord {
                    structure_id: sid,
                    type_id: 35_832,
                    owner_corp_id: corp_id,
                    solar_system_id: sys_id,
                    name: format!("TestFort-{sid}"),
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
        }

        for (item_id, type_id) in [
            (freighter_id, 587_u32),
            (saved_ibis_id, 601_u32),
            (boardable_ibis_id, 601_u32),
            (untrained_velator_id, 606_u32),
        ] {
            s.items.insert(
                item_id,
                InventoryItemRecord {
                    item_id,
                    type_id,
                    owner_id: char_id,
                    location_id: structure_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }

        let mut ext =
            crate::services::station_structure::structure_upwell_ops::load_ext_state(s);
        ext.active_pilot_by_structure.insert(structure_id, char_id);
        ext.previous_ship_by_structure
            .insert(structure_id, freighter_id);
        ext.active_pilot_by_structure
            .insert(other_structure_id, other_gunner_id);
        ext.previous_ship_by_structure
            .insert(other_structure_id, saved_ibis_id);
        crate::services::station_structure::structure_upwell_ops::save_ext_state(s, &ext);
    });

    let mut sess = SessionState::new(9_417);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(sys_id);
    sess.structure_id = Some(structure_id);
    sess.ship_id = Some(structure_id);

    // 1. Controlling a docked Upwell structure rejects CreateNewbieShip with ControllingStructure
    //    (both 2-arg V24 call and 0-arg call) without orphaning active_pilot_by_structure (HF1411-P1-02 / HF1417-P1-02).
    let ctrl_err = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "CreateNewbieShip",
        &[PyRep::Integer(structure_id), PyRep::Integer(structure_id)],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("controlling structure must reject 2-arg CreateNewbieShip");
    assert_eq!(ctrl_err.code, "ControllingStructure");
    assert_eq!(sess.ship_id, Some(structure_id));
    let ctrl_0arg_err = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "CreateNewbieShip",
        &[],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("controlling structure must reject 0-arg CreateNewbieShip");
    assert_eq!(ctrl_0arg_err.code, "ControllingStructure");
    assert_eq!(sess.ship_id, Some(structure_id));
    ctx.db.with_snapshot(|s| {
        assert_eq!(
            s.characters.get(&char_id).map(|c| c.active_ship_id),
            Some(structure_id)
        );
        let ext = crate::services::station_structure::structure_upwell_ops::load_ext_state(s);
        assert_eq!(
            ext.active_pilot_by_structure.get(&structure_id),
            Some(&char_id)
        );
    });

    // 1b. Even if active_ship_id / session.ship_id were desynced to a regular ship, membership in
    //     active_pilot_by_structure still rejects with ControllingStructure.
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&char_id).unwrap().active_ship_id = freighter_id;
    });
    sess.ship_id = Some(freighter_id);
    let ctrl_ext_err = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "CreateNewbieShip",
        &[PyRep::Integer(freighter_id), PyRep::Integer(structure_id)],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("active_pilot_by_structure entry must still reject with ControllingStructure");
    assert_eq!(ctrl_ext_err.code, "ControllingStructure");

    // 2. After releasing structure control, `find_boardable_corvette_in_hangar` must skip
    //    `saved_ibis_id` (`is_structure_controller_saved_ship`) even though its item_id is lower
    //    than `boardable_ibis_id` (HF1411-P1-02).
    ctx.db.with_mut_snapshot(|s| {
        let mut ext =
            crate::services::station_structure::structure_upwell_ops::load_ext_state(s);
        ext.active_pilot_by_structure.remove(&structure_id);
        ext.previous_ship_by_structure.remove(&structure_id);
        crate::services::station_structure::structure_upwell_ops::save_ext_state(s, &ext);
    });
    let skipped_saved = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "CreateNewbieShip",
        &[PyRep::Integer(freighter_id), PyRep::Integer(structure_id)],
        &kw,
    )
    .await
    .expect("handled")
    .expect("must skip structure-saved corvette and board next eligible Hangar corvette");
    assert_eq!(
        skipped_saved.dict_get("shipID").and_then(PyRep::as_i64),
        Some(boardable_ibis_id)
    );
    assert_eq!(sess.ship_id, Some(boardable_ibis_id));

    // 3. With only the untrained cross-race Velator (606, requiring Gallente Frigate 3328) left
    //    in Hangar alongside `saved_ibis_id`, `find_boardable_corvette_in_hangar` must skip the
    //    untrained Velator (`HF1411-P1-03`) and spawn a fresh racial Ibis (601) instead.
    ctx.db.with_mut_snapshot(|s| {
        s.items.remove(&boardable_ibis_id);
        s.characters.get_mut(&char_id).unwrap().active_ship_id = freighter_id;
    });
    sess.ship_id = Some(freighter_id);
    let spawned_instead = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "CreateNewbieShip",
        &[PyRep::Integer(freighter_id), PyRep::Integer(structure_id)],
        &kw,
    )
    .await
    .expect("handled")
    .expect("must skip untrained cross-race Velator and spawn racial Ibis");
    let spawned_id = spawned_instead
        .dict_get("shipID")
        .and_then(PyRep::as_i64)
        .expect("spawned corvette shipID");
    assert_ne!(spawned_id, untrained_velator_id);
    assert_ne!(spawned_id, saved_ibis_id);
    assert_eq!(
        spawned_instead.dict_get("typeID").and_then(PyRep::as_i64),
        Some(601)
    );
    assert_eq!(sess.ship_id, Some(spawned_id));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_newbie_ship_spawn_enforces_racial_corvette_skill_prerequisites() {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let char_id = 96_501_501_i64;
    let station_id = 60_003_760_i64;
    let sys_id = 30_000_142_u32;
    let rifter_id = 96_501_510_i64;
    let hangar_ibis_id = 96_501_511_i64;
    let capsule_id = 96_501_512_i64;
    let claim_key = -8_800_000_i64 - char_id;
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|s| {
        let mut ch = s.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = 1;
        ch.race_id = 1; // Caldari -> Ibis (601, requires Spaceship Command I / 3327)
        ch.name = "UntrainedCorvettePilot".into();
        ch.corporation_id = 98_000_001;
        ch.solar_system_id = sys_id;
        ch.station_id = Some(station_id);
        ch.structure_id = None;
        ch.active_ship_id = rifter_id;
        s.characters.insert(char_id, ch);
        // Deliberately leave `s.skills` empty for `char_id` (missing Spaceship Command I / 3327).
        s.skills.remove(&char_id);

        for (item_id, type_id) in [
            (rifter_id, 587_u32),
            (hangar_ibis_id, 601_u32),
            (capsule_id, 670_u32),
        ] {
            s.items.insert(
                item_id,
                InventoryItemRecord {
                    item_id,
                    type_id,
                    owner_id: char_id,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });

    let mut sess = SessionState::new(9_433);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(98_000_001);
    sess.solar_system_id = Some(sys_id);
    sess.station_id = Some(station_id);
    sess.ship_id = Some(rifter_id);

    // 1. V24 2-arg call by an untrained pilot with an existing Hangar corvette:
    //    `find_boardable_corvette_in_hangar` skips `hangar_ibis_id` AND the spawn fallback must
    //    also reject with `ShipHasSkillPrerequisites` instead of leaving `rifter_id` or spawning
    //    a duplicate corvette (+ Civilian Miner + Tritanium) (#1433 / HF1417-P1-01).
    let items_before = ctx.db.with_snapshot(|s| s.items.len());
    let err_2arg = handle_ships_mastery_and_fitting_ext(
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
    .expect_err("untrained pilot must not spawn duplicate racial corvette on 2-arg call");
    assert_eq!(err_2arg.code, "ShipHasSkillPrerequisites");
    assert_eq!(sess.ship_id, Some(rifter_id));
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.items.len(), items_before);
        assert_eq!(
            s.characters.get(&char_id).map(|c| c.active_ship_id),
            Some(rifter_id)
        );
        assert!(!s.item_custom_names.contains_key(&claim_key));
    });

    // 2. Legacy 0-arg call from a capsule with no corvette in station must likewise reject with
    //    `ShipHasSkillPrerequisites` without spawning or writing a cooldown timestamp.
    ctx.db.with_mut_snapshot(|s| {
        s.items.remove(&hangar_ibis_id);
        s.characters.get_mut(&char_id).unwrap().active_ship_id = capsule_id;
    });
    sess.ship_id = Some(capsule_id);
    let items_before_0arg = ctx.db.with_snapshot(|s| s.items.len());
    let err_0arg = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "CreateNewbieShip",
        &[],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("untrained pilot in capsule must hit ShipHasSkillPrerequisites on 0-arg spawn");
    assert_eq!(err_0arg.code, "ShipHasSkillPrerequisites");
    assert_eq!(sess.ship_id, Some(capsule_id));
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.items.len(), items_before_0arg);
        assert_eq!(
            s.characters.get(&char_id).map(|c| c.active_ship_id),
            Some(capsule_id)
        );
        assert!(!s.item_custom_names.contains_key(&claim_key));
    });

    // 3. Once Spaceship Command I (3327) is trained, spawning the racial Ibis (601) succeeds.
    ctx.db.with_mut_snapshot(|s| {
        s.skills.entry(char_id).or_default().insert(
            3327,
            eve_db::TrainedSkillRecord {
                type_id: 3327,
                level: 1,
                skill_points: 250.0,
            },
        );
    });
    let spawned = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "CreateNewbieShip",
        &[],
        &kw,
    )
    .await
    .expect("handled")
    .expect("trained pilot with Spaceship Command I must spawn racial corvette");
    let spawned_id = spawned
        .dict_get("shipID")
        .and_then(PyRep::as_i64)
        .expect("spawned corvette shipID");
    assert_eq!(spawned.dict_get("typeID").and_then(PyRep::as_i64), Some(601));
    assert_eq!(sess.ship_id, Some(spawned_id));
}

#[tokio::test]
async fn leave_ship_enforces_ownership_for_active_and_explicit_ships() {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let char_a = 90_000_001i64;
    let char_b = 90_000_002i64;
    let station_id = 60_003_760i64;
    let sys_id = 30_000_142u32;
    let own_ship_id = 955_100_001i64;
    let other_ship_id = 955_100_002i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_a) {
            ch.station_id = Some(station_id);
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = other_ship_id;
        }
        snap.items.insert(
            own_ship_id,
            InventoryItemRecord {
                item_id: own_ship_id,
                type_id: 587,
                owner_id: char_a,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            other_ship_id,
            InventoryItemRecord {
                item_id: other_ship_id,
                type_id: 587,
                owner_id: char_b,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let kw = HashMap::new();
    let mut sess = SessionState::new(1);
    sess.user_id = Some(1);
    sess.character_id = Some(char_a);
    sess.station_id = Some(station_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(other_ship_id);

    // 1. Active ship owned by another pilot -> OwnershipViolation
    let err_active = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut sess,
        None,
        "ship",
        "LeaveShip",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_active.code, "OwnershipViolation");
    assert_eq!(sess.ship_id, Some(other_ship_id));

    // 2. Active ship owned by caller, but explicit arg names another pilot's ship -> OwnershipViolation
    sess.ship_id = Some(own_ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_a) {
            ch.active_ship_id = own_ship_id;
        }
    });
    let err_explicit = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut sess,
        None,
        "ship",
        "LeaveShip",
        &[PyRep::Integer(other_ship_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_explicit.code, "OwnershipViolation");
    assert_eq!(sess.ship_id, Some(own_ship_id));

    // 3. Explicit active ship owned by caller succeeds
    let ok_leave = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut sess,
        None,
        "ship",
        "LeaveShip",
        &[PyRep::Integer(own_ship_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("LeaveShip with owned explicit active ship succeeds");
    let capsule_id = ok_leave.as_i64().expect("returns capsule ID");
    assert_ne!(capsule_id, own_ship_id);
    assert_eq!(sess.ship_id, Some(capsule_id));
}

