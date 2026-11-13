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
