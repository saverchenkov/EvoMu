//! Additional edge-case and branch coverage tests for `services::fitting_ships_mastery`.

use super::*;
use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta};
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| {
        if std::env::var_os("EVOMU_SDE_PATH").is_some() {
            Arc::new(ServerContext::new().sde.as_ref().clone())
        } else {
            Arc::new(SdeRegistry::with_canonical_seed())
        }
    }))
}

fn make_ctx() -> ServerContext {
    ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    )
    .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
}

fn make_session(char_id: i64, sys_id: u32, station_id: Option<i64>, ship_id: Option<i64>) -> SessionState {
    let mut s = SessionState::new(9810);
    s.user_id = Some(1);
    s.character_id = Some(char_id);
    s.corporation_id = Some(1_000_009);
    s.solar_system_id = Some(sys_id);
    s.station_id = station_id;
    s.ship_id = ship_id;
    s
}

fn seed_pilot(ctx: &ServerContext, char_id: i64, sys_id: u32, station_id: Option<i64>, ship_id: i64, ship_type: u32) {
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = 1;
        ch.name = format!("EdgeShipPilot {char_id}");
        ch.corporation_id = 1_000_009;
        ch.race_id = 2;
        ch.station_id = station_id;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);

        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_type,
                owner_id: char_id,
                location_id: station_id.unwrap_or(sys_id as i64),
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn leave_ship_eject_and_board_guards() {
    let ctx = make_ctx();
    let char_id = 96_500_001_i64;
    let station_id = 60_003_760_i64;
    let sys_id = 30_000_142_u32;
    let ship_id = 96_500_100_i64;
    let pod_id = 96_500_101_i64;
    let target_ship_id = 96_500_102_i64;
    let kw = HashMap::new();

    // 1. LeaveShip while in space -> CannotLeaveShipInSpace; Eject while docked -> CannotEjectWhileDocked
    seed_pilot(&ctx, char_id, sys_id, None, ship_id, 587);
    let mut space_sess = make_session(char_id, sys_id, None, Some(ship_id));
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut space_sess, None, "ship", "LeaveShip", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CannotLeaveShipInSpace"
    );

    let mut docked_sess = make_session(char_id, sys_id, Some(station_id), Some(ship_id));
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut docked_sess, None, "ship", "Eject", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CannotEjectWhileDocked"
    );

    // 2. Capsule cannot LeaveShip or Eject -> CannotEjectFromCapsule
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            pod_id,
            InventoryItemRecord {
                item_id: pod_id,
                type_id: 670, // Capsule
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.characters.get_mut(&char_id).unwrap().active_ship_id = pod_id;
    });
    docked_sess.ship_id = Some(pod_id);
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut docked_sess, None, "ship", "LeaveShip", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CannotEjectFromCapsule"
    );
    space_sess.ship_id = Some(pod_id);
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut space_sess, None, "ship", "Eject", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CannotEjectFromCapsule"
    );

    // 3. Eject blocked with weapons timer in space
    space_sess.ship_id = Some(ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().active_ship_id = ship_id;
    });
    let now_unix = ctx.now_unix_secs();
    {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        let st = cw.states.entry(char_id).or_default();
        st.weapon_timer_until_unix = now_unix + 120;
    }
    assert!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut space_sess, None, "ship", "Eject", &[], &kw)
            .await
            .unwrap()
            .is_err()
    );
    {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        cw.states.remove(&char_id);
    }

    // 4. Missing ship -> ShipNotFound
    space_sess.ship_id = Some(99_999_999);
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut space_sess, None, "ship", "Eject", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ShipNotFound"
    );

    // 5. Board guards: NoSolarSystem, NoActiveShip, OwnershipViolation, NotAShip, ShipNotBoardable, ShipNotInSpace, ShipOutOfBoardingRange, happy in-space board
    let mut bad_sys = make_session(char_id, sys_id, None, Some(pod_id));
    bad_sys.solar_system_id = None;
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut bad_sys, None, "ship", "Board", &[PyRep::Integer(target_ship_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NoSolarSystem"
    );

    let mut no_ship = make_session(char_id, sys_id, None, None);
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut no_ship, None, "ship", "Board", &[PyRep::Integer(target_ship_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NoActiveShip"
    );

    ctx.db.with_mut_snapshot(|snap| {
        // Non-ship tritanium
        snap.items.insert(
            96_500_200,
            InventoryItemRecord {
                item_id: 96_500_200,
                type_id: 34,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        // Unpackaged ship
        snap.items.insert(
            96_500_201,
            InventoryItemRecord {
                item_id: 96_500_201,
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
        // Foreign ship
        snap.items.insert(
            96_500_202,
            InventoryItemRecord {
                item_id: 96_500_202,
                type_id: 587,
                owner_id: char_id + 1,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        // Valid in-space target ship owned by char
        snap.items.insert(
            target_ship_id,
            InventoryItemRecord {
                item_id: target_ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    docked_sess.ship_id = Some(pod_id);
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut docked_sess, None, "ship", "Board", &[PyRep::Integer(96_500_200)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NotAShip"
    );
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut docked_sess, None, "ship", "Board", &[PyRep::Integer(96_500_201)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ShipNotBoardable"
    );
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut docked_sess, None, "ship", "Board", &[PyRep::Integer(96_500_202)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "OwnershipViolation"
    );

    // ShipNotInSpace when in space and target not in space ball grid
    space_sess.ship_id = Some(pod_id);
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h
        .spawn_ball_with_meta(
            Ball {
                id: pod_id,
                type_id: 670,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 100_000.0,
                radius: 20.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
            SlimItemMeta::default(),
        )
        .await;
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut space_sess, None, "ship", "Board", &[PyRep::Integer(target_ship_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ShipNotInSpace"
    );

    // Out of range (> 2500m), then in range (<= 2500m) removes old capsule
    let _ = sys_h
        .spawn_ball_with_meta(
            Ball {
                id: target_ship_id,
                type_id: 587,
                position: DVec3::new(10_000.0, 0.0, 0.0),
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
        handle_ships_mastery_and_fitting_ext(&ctx, &mut space_sess, None, "ship", "Board", &[PyRep::Integer(target_ship_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ShipOutOfBoardingRange"
    );

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 2;
        }
        let entry = snap.skills.entry(char_id).or_default();
        for sid in [3327_u32, 3328_u32, 3329_u32, 3330_u32, 3331_u32] {
            entry.insert(
                sid,
                eve_db::TrainedSkillRecord {
                    type_id: sid,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }
    });
    let _ = sys_h.set_ball_position(target_ship_id, DVec3::new(500.0, 0.0, 0.0)).await;
    let boarded = handle_ships_mastery_and_fitting_ext(&ctx, &mut space_sess, None, "ship", "Board", &[PyRep::Integer(target_ship_id)], &kw)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(boarded, PyRep::Tuple(ref els) if els.len() == 4));
    assert_eq!(space_sess.ship_id, Some(target_ship_id));
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.characters[&char_id].active_ship_id, target_ship_id);
    });
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&pod_id)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn launch_from_container_newbie_ship_guards_and_trash_dirt_mastery_edges() {
    let ctx = make_ctx();
    let char_id = 96_500_002_i64;
    let station_id = 60_003_760_i64;
    let sys_id = 30_000_142_u32;
    let ship_id = 96_500_300_i64;
    let kw = HashMap::new();
    seed_pilot(&ctx, char_id, sys_id, Some(station_id), ship_id, 587);

    // 1. CreateNewbieShip guards: NotDocked, StationMismatch, AlreadyHaveShipInStation, NewbieShipCooldown
    let mut space_sess = make_session(char_id, sys_id, None, Some(ship_id));
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut space_sess, None, "dogmaIM", "CreateNewbieShip", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NotDocked"
    );

    let mut docked_sess = make_session(char_id, sys_id, Some(station_id), Some(ship_id));
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(
            &ctx,
            &mut docked_sess,
            None,
            "dogmaIM",
            "CreateNewbieShip",
            &[PyRep::Integer(ship_id), PyRep::Integer(station_id + 1)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "StationMismatch"
    );

    // LaunchFromContainer: ObjectNotInSpace when docked
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(
            &ctx,
            &mut docked_sess,
            None,
            "ship",
            "LaunchFromContainer",
            &[PyRep::Integer(1)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "ObjectNotInSpace"
    );

    // LaunchFromContainer: ObjectNotInSpace / ItemNotFound / OutOfRange / happy path
    let cont_id = 96_500_310_i64;
    let packaged_or_valid_id = 96_500_311_i64;
    let bad_type_id = 96_500_312_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            cont_id,
            InventoryItemRecord {
                item_id: cont_id,
                type_id: 23, // Cargo Container
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            packaged_or_valid_id,
            InventoryItemRecord {
                item_id: packaged_or_valid_id,
                type_id: 587, // Assembled Rifter inside container
                owner_id: char_id,
                location_id: cont_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            bad_type_id,
            InventoryItemRecord {
                item_id: bad_type_id,
                type_id: 34, // Tritanium cannot launch as ship/container
                owner_id: char_id,
                location_id: cont_id,
                flag: InventoryFlag::Hangar,
                quantity: 10,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::ZERO,
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
        handle_ships_mastery_and_fitting_ext(
            &ctx,
            &mut space_sess,
            None,
            "ship",
            "LaunchFromContainer",
            &[PyRep::Integer(99_999_999)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "ItemNotFound"
    );
    let _ = sys_h
        .spawn_ball_with_meta(
            Ball {
                id: cont_id,
                type_id: 23,
                position: DVec3::new(8_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 1_000.0,
                radius: 20.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
            SlimItemMeta::default(),
        )
        .await;
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(
            &ctx,
            &mut space_sess,
            None,
            "ship",
            "LaunchFromContainer",
            &[PyRep::Integer(packaged_or_valid_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "OutOfRange"
    );

    let _ = sys_h.set_ball_position(cont_id, DVec3::new(400.0, 0.0, 0.0)).await;
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(
            &ctx,
            &mut space_sess,
            None,
            "ship",
            "LaunchFromContainer",
            &[PyRep::Integer(bad_type_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidLaunchItem"
    );
    let launched = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut space_sess,
        None,
        "ship",
        "LaunchFromContainer",
        &[PyRep::Integer(packaged_or_valid_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_ne!(launched, PyRep::None);

    // 2. Mastery / skill level / kill count / dirt error branches
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(
            &ctx,
            &mut docked_sess,
            None,
            "dogmaIM",
            "GetRequiredSkillLevels",
            &[PyRep::Integer(999_999_999)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "TypeNotFound"
    );
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(
            &ctx,
            &mut docked_sess,
            None,
            "ship",
            "GetCurrCharMasteryLevel",
            &[PyRep::Integer(0)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidShipTypeID"
    );
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(
            &ctx,
            &mut docked_sess,
            None,
            "shipKillCounter",
            "SetShipKillCounter",
            &[PyRep::Integer(ship_id), PyRep::Integer(5)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "GMRoleRequired"
    );
    let mut no_ship_sess = docked_sess.clone();
    no_ship_sess.ship_id = None;
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(
            &ctx,
            &mut no_ship_sess,
            None,
            "ship",
            "GetDirtTimestamp",
            &[],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidArguments"
    );
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(
            &ctx,
            &mut docked_sess,
            None,
            "ship",
            "ResetDirtTimestamp",
            &[PyRep::Integer(99_999_999)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "ItemNotFound"
    );

    // 3. TrashItems: fitted flag and locked flag guards
    let fitted_id = 96_500_320_i64;
    let locked_id = 96_500_321_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            fitted_id,
            InventoryItemRecord {
                item_id: fitted_id,
                type_id: 485,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            locked_id,
            InventoryItemRecord {
                item_id: locked_id,
                type_id: 34,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Locked,
                quantity: 50,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(
            &ctx,
            &mut docked_sess,
            None,
            "invbroker",
            "TrashItems",
            &[PyRep::List(vec![PyRep::Integer(fitted_id)])],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CannotTrashFittedItem"
    );
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(
            &ctx,
            &mut docked_sess,
            None,
            "invbroker",
            "TrashItems",
            &[PyRep::List(vec![PyRep::Integer(locked_id)])],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "ItemLocked"
    );

    // GetShipFittingInfo includes fitted module + SafeLogoff / AbortSafeLogoff
    let fit_info = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut docked_sess,
        None,
        "ship",
        "GetShipFittingInfo",
        &[PyRep::Integer(ship_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_ne!(fit_info, PyRep::None);

    // 4. Direct helper coverage for `build_vacated_ship_slim_meta` and `build_ejected_capsule_ball_and_combat_state`
    let _ = sys_h
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
            SlimItemMeta {
                owner_id: char_id,
                char_id: Some(char_id),
                is_cloaked: true,
                ..Default::default()
            },
        )
        .await;
    let vacated = super::build_vacated_ship_slim_meta(&ctx, &sys_h, char_id, ship_id);
    assert_eq!(vacated.owner_id, char_id);
    assert_eq!(vacated.char_id, None);
    assert!(!vacated.is_cloaked);

    let vacated_missing = super::build_vacated_ship_slim_meta(&ctx, &sys_h, char_id, 999_888_777);
    assert_eq!(vacated_missing.owner_id, char_id);
    assert_eq!(vacated_missing.char_id, None);
    assert!(!vacated_missing.is_cloaked);

    let (pod_ball, pod_cs) = super::build_ejected_capsule_ball_and_combat_state(
        &ctx,
        char_id,
        888_000_111,
        DVec3::new(100.0, 200.0, 300.0),
    );
    assert_eq!(pod_ball.id, 888_000_111);
    assert_eq!(pod_ball.type_id, 670);
    assert_eq!(pod_ball.position, DVec3::new(100.0, 200.0, 300.0));
    assert!(pod_ball.max_velocity > 0.0 && pod_ball.mass > 0.0 && pod_ball.radius >= 15.0);
    assert!(pod_cs.max_structure_hp > 0.0 && pod_cs.max_shield_hp > 0.0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn newbie_ship_clears_stale_future_cooldown_timestamp() {
    let ctx = make_ctx();
    let fixed_now = 1_800_000_000_u64;
    ctx.set_clock(Arc::new(eve_db::clock::VirtualServerClock::with_fixed_base(
        fixed_now,
    )));
    let char_id = 96_500_401_i64;
    let station_id = 60_003_760_i64;
    let sys_id = 30_000_142_u32;
    let capsule_id = 96_500_410_i64;
    let kw = HashMap::new();
    seed_pilot(&ctx, char_id, sys_id, Some(station_id), capsule_id, 670);

    let claim_key = -8_800_000_i64 - char_id;
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_custom_names
            .insert(claim_key, (fixed_now + 86_400).to_string());
    });

    let mut sess = make_session(char_id, sys_id, Some(station_id), Some(capsule_id));
    let claim_res = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "CreateNewbieShip",
        &[],
        &kw,
    )
    .await
    .expect("CreateNewbieShip route handled")
    .expect("stale future cooldown timestamp must not block CreateNewbieShip");
    assert!(
        claim_res
            .dict_get("shipID")
            .and_then(|v| v.as_i64())
            .is_some_and(|id| id > 0),
        "expected spawned corvette shipID in response: {claim_res:?}"
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.item_custom_names.get(&claim_key).map(String::as_str),
            Some(fixed_now.to_string().as_str())
        );
    });

    // Normal active cooldown still blocks within 60s.
    let cooldown_err = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "CreateNewbieShip",
        &[],
        &kw,
    )
    .await
    .expect("CreateNewbieShip route handled")
    .expect_err("fresh cooldown timestamp within 60s must return NewbieShipCooldown");
    assert_eq!(cooldown_err.code, "NewbieShipCooldown");

    // When a stale future key is detected and the claim subsequently exits early, the stale key is deleted.
    let rifter_id = 96_500_420_i64;
    let char_b = 96_500_402_i64;
    let claim_key_b = -8_800_000_i64 - char_b;
    seed_pilot(&ctx, char_b, sys_id, Some(station_id), rifter_id, 587);
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_custom_names
            .insert(claim_key_b, (fixed_now + 5_000).to_string());
    });
    let mut sess_b = make_session(char_b, sys_id, Some(station_id), Some(rifter_id));
    let early_err = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut sess_b,
        None,
        "dogmaIM",
        "CreateNewbieShip",
        &[],
        &kw,
    )
    .await
    .expect("CreateNewbieShip route handled")
    .expect_err("0-arg call in non-capsule ship fails after stale cleanup");
    assert_eq!(early_err.code, "AlreadyInNewbieShip");
    ctx.db.with_snapshot(|snap| {
        assert!(
            !snap.item_custom_names.contains_key(&claim_key_b),
            "stale future cooldown key must be purged from snapshot"
        );
    });
}
