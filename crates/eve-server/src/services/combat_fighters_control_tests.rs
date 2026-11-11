use super::combat_fighters_tests::{make_fighter_ctx_and_session, make_fighter_item};
use super::*;
use crate::router::context::ActiveFighterSquadron;
use eve_destiny::{Ball, CombatState, DamageVector, MovementMode};
use glam::DVec3;
use std::collections::HashMap;

#[tokio::test]
async fn fighter_movement_commands_cover_mwd_mjd_orbit_follow_stop_goto_and_errors() {
    let (ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let kwargs = HashMap::new();
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    // Carrier + target + Light fighter (type 23057, no MJD) + Heavy fighter (type 23061, has MJD)
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 23_915,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 120.0,
                inertia: 0.5,
                mass: 1_000_000_000.0,
                radius: 500.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await
        .unwrap();

    let light_ball = 410_001i64;
    let heavy_ball = 410_002i64;
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: light_ball,
                type_id: 23_057,
                position: DVec3::new(1_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 2_000.0,
                inertia: 0.5,
                mass: 25_000.0,
                radius: 25.0,
                speed_fraction: 1.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await
        .unwrap();
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: heavy_ball,
                type_id: 23_061,
                position: DVec3::new(2_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 1_500.0,
                inertia: 0.5,
                mass: 50_000.0,
                radius: 35.0,
                speed_fraction: 1.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await
        .unwrap();

    ctx.active_fighters.write().unwrap().insert(
        light_ball,
        ActiveFighterSquadron {
            ball_id: light_ball,
            item_id: 510_001,
            type_id: 23_057,
            owner_char_id: char_id,
            parent_ship_id: ship_id,
            tube_index: 0,
            squadron_size: 9,
            target_id: None,
            is_abandoned: false,
            mwd_active: false,
        },
    );
    ctx.active_fighters.write().unwrap().insert(
        heavy_ball,
        ActiveFighterSquadron {
            ball_id: heavy_ball,
            item_id: 510_002,
            type_id: 23_061,
            owner_char_id: char_id,
            parent_ship_id: ship_id,
            tube_index: 1,
            squadron_size: 6,
            target_id: None,
            is_abandoned: false,
            mwd_active: false,
        },
    );

    // 1. MWD on light fighter multiplies velocity by (1 + 400/100) = 5x
    let mwd_res = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "ExecuteMovementCommandOnFighters",
        &[PyRep::Integer(light_ball), PyRep::String("MWD".into())],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(mwd_res, PyRep::Bool(true));
    assert!(ctx.active_fighters.read().unwrap()[&light_ball].mwd_active);
    let (_, b_after_mwd) = sys_handle.get_ball_snapshot(light_ball).await.unwrap().unwrap();
    assert!((b_after_mwd.max_velocity - 10_000.0).abs() < 1e-3);

    // 2. MJD on light fighter lacks ability -> FighterLacksMjdAbility
    let err_no_mjd = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "ExecuteMovementCommandOnFighters",
        &[PyRep::Integer(light_ball), PyRep::String("MJD".into())],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_no_mjd.code, "FighterLacksMjdAbility");

    // 3. MJD on heavy fighter succeeds and puts slot 1 on cooldown; immediate second call hits AbilityOnCooldown
    let mjd_ok = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "ExecuteMovementCommandOnFighters",
        &[PyRep::Integer(heavy_ball), PyRep::String("MJD".into())],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(mjd_ok, PyRep::Bool(true));
    let mjd_cd = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "ExecuteMovementCommandOnFighters",
        &[PyRep::Integer(heavy_ball), PyRep::String("MJD".into())],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(mjd_cd.code, "AbilityOnCooldown");

    // 4. ORBIT, FOLLOW, GOTO_POINT, STOP
    for args in [
        vec![
            PyRep::List(vec![PyRep::Integer(light_ball)]),
            PyRep::String("ORBIT".into()),
            PyRep::Integer(ship_id),
            PyRep::Real(2_500.0),
        ],
        vec![
            PyRep::List(vec![PyRep::Integer(light_ball)]),
            PyRep::String("FOLLOW".into()),
            PyRep::Integer(ship_id),
            PyRep::Real(1_500.0),
        ],
        vec![
            PyRep::List(vec![PyRep::Integer(light_ball)]),
            PyRep::String("GOTO_POINT".into()),
            PyRep::Real(10_000.0),
            PyRep::Real(2_000.0),
            PyRep::Real(-3_000.0),
        ],
        vec![
            PyRep::List(vec![PyRep::Integer(light_ball)]),
            PyRep::String("STOP".into()),
        ],
    ] {
        let r = handle_fighters(
            &ctx,
            &mut session,
            "fighterMgr",
            "ExecuteMovementCommandOnFighters",
            &args,
            &kwargs,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(r, PyRep::Bool(true));
    }
}

#[tokio::test]
async fn abandon_scoop_and_squadron_total_wipeout_cover_ownership_range_and_capacity() {
    let (ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let kwargs = HashMap::new();
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 23_915,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 120.0,
                inertia: 0.5,
                mass: 1_000_000_000.0,
                radius: 500.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await
        .unwrap();

    let fighter_ball = 420_001i64;
    let fighter_item = 520_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            fighter_item,
            make_fighter_item(fighter_item, 23_057, char_id, sys_id as i64, InventoryFlag::None, 5),
        );
    });
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: fighter_ball,
                type_id: 23_057,
                position: DVec3::new(10_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 2_000.0,
                inertia: 0.5,
                mass: 25_000.0,
                radius: 25.0,
                speed_fraction: 1.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await
        .unwrap();
    ctx.active_fighters.write().unwrap().insert(
        fighter_ball,
        ActiveFighterSquadron {
            ball_id: fighter_ball,
            item_id: fighter_item,
            type_id: 23_057,
            owner_char_id: char_id,
            parent_ship_id: ship_id,
            tube_index: 0,
            squadron_size: 5,
            target_id: None,
            is_abandoned: false,
            mwd_active: false,
        },
    );

    // 1. Scooping before abandoning fails with FighterNotAbandoned
    let not_aband = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "CmdScoopAbandonedFighterFromSpace",
        &[PyRep::Integer(fighter_ball)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(not_aband.code, "FighterNotAbandoned");

    // 2. Abandon fighter squadron -> returns [fighter_ball] and stops ball
    let aband_res = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "CmdAbandonFighter",
        &[PyRep::Integer(fighter_ball)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(aband_res, PyRep::List(vec![PyRep::Integer(fighter_ball)]));
    assert!(ctx.active_fighters.read().unwrap()[&fighter_ball].is_abandoned);

    // 3. Scooping at 10km distance (> 2500m) fails with FighterOutOfRange
    let out_rng = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "CmdScoopAbandonedFighterFromSpace",
        &[PyRep::Integer(fighter_ball)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(out_rng.code, "FighterOutOfRange");

    // 4. Move ship within 1,500m of fighter and scoop into bay -> returns Bool(true) and removes ball
    assert!(
        sys_handle
            .set_ball_position(ship_id, DVec3::new(9_000.0, 0.0, 0.0))
            .await
    );
    let scoop_ok = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "CmdScoopAbandonedFighterFromSpace",
        &[PyRep::Integer(fighter_ball)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(scoop_ok, PyRep::Bool(true));
    assert!(!ctx.active_fighters.read().unwrap().contains_key(&fighter_ball));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&fighter_item].location_id, ship_id);
        assert_eq!(snap.items[&fighter_item].flag, InventoryFlag::FighterBay);
    });

    // 5. Total squadron wipeout deletes item from DB and removes ball
    let wipe_ball = 420_002i64;
    let wipe_item = 520_002i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            wipe_item,
            make_fighter_item(wipe_item, 23_057, char_id, sys_id as i64, InventoryFlag::None, 3),
        );
    });
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: wipe_ball,
                type_id: 23_057,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 2_000.0,
                inertia: 0.5,
                mass: 25_000.0,
                radius: 25.0,
                speed_fraction: 1.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await
        .unwrap();
    ctx.active_fighters.write().unwrap().insert(
        wipe_ball,
        ActiveFighterSquadron {
            ball_id: wipe_ball,
            item_id: wipe_item,
            type_id: 23_057,
            owner_char_id: char_id,
            parent_ship_id: ship_id,
            tube_index: 1,
            squadron_size: 3,
            target_id: None,
            is_abandoned: false,
            mwd_active: false,
        },
    );
    let (lost, rem, wiped) = apply_damage_to_fighter_squadron(
        &ctx,
        sys_id,
        wipe_ball,
        DamageVector::new(10_000.0, 0.0, 0.0, 0.0),
    )
    .await;
    assert_eq!(lost, 3);
    assert_eq!(rem, 0);
    assert!(wiped);
    assert!(!ctx.active_fighters.read().unwrap().contains_key(&wipe_ball));
    assert!(ctx.db.get_item(wipe_item).is_none());
}

#[tokio::test]
async fn load_unload_get_fighters_for_ship_and_validation_errors() {
    let (ctx, mut session, char_id, ship_id, _sys_id) = make_fighter_ctx_and_session();
    let kw = std::collections::HashMap::new();

    let fid1 = 910_001i64;
    let fid2 = 910_002i64;
    let fid3 = 910_003i64;
    let f_heavy = 910_004i64;
    let f_other = 910_005i64;
    let f_cargo = 910_006i64;
    let f_non_fighter = 910_007i64;

    ctx.db.with_mut_snapshot(|snap| {
        for id in [fid1, fid2, fid3] {
            snap.items.insert(
                id,
                make_fighter_item(id, 23_057, char_id, ship_id, eve_db::InventoryFlag::FighterBay, 9),
            );
        }
        snap.items.insert(
            f_heavy,
            make_fighter_item(f_heavy, 23_061, char_id, ship_id, eve_db::InventoryFlag::FighterBay, 6),
        );
        snap.items.insert(
            f_other,
            make_fighter_item(f_other, 23_057, char_id + 77, ship_id, eve_db::InventoryFlag::FighterBay, 9),
        );
        snap.items.insert(
            f_cargo,
            make_fighter_item(f_cargo, 23_057, char_id, ship_id, eve_db::InventoryFlag::Cargo, 9),
        );
        snap.items.insert(
            f_non_fighter,
            make_fighter_item(f_non_fighter, 587, char_id, ship_id, eve_db::InventoryFlag::FighterBay, 1),
        );
    });

    // GetFightersForShip denies another shipID and returns empty tuple lists initially.
    let deny_err = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "GetFightersForShip",
        &[PyRep::Integer(ship_id + 1)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(deny_err.code, "AccessDenied");

    let empty_state = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "GetFightersForShip",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .expect("GetFightersForShip empty");
    let PyRep::Tuple(parts) = empty_state else {
        panic!("expected tuple from GetFightersForShip");
    };
    assert_eq!(parts.len(), 3);
    assert!(matches!(&parts[0], PyRep::List(v) if v.is_empty()));
    assert!(matches!(&parts[1], PyRep::List(v) if v.is_empty()));
    assert!(matches!(&parts[2], PyRep::Dict(v) if v.is_empty()));

    // LoadFightersToTube validation branches.
    let bad_tube = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "LoadFightersToTube",
        &[PyRep::Integer(fid1), PyRep::Integer(999)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(bad_tube.code, "InvalidFighterTube");

    let missing_item = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "LoadFightersToTube",
        &[PyRep::Integer(9_999_888), PyRep::Integer(0)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(missing_item.code, "ItemNotFound");

    let wrong_owner = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "LoadFightersToTube",
        &[PyRep::Integer(f_other), PyRep::Integer(0)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(wrong_owner.code, "OwnershipViolation");

    let wrong_bay = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "LoadFightersToTube",
        &[PyRep::Integer(f_cargo), PyRep::Integer(0)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(wrong_bay.code, "ItemNotInFighterBay");

    let non_fighter = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "LoadFightersToTube",
        &[PyRep::Integer(f_non_fighter), PyRep::Integer(0)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(non_fighter.code, "ItemNotAFighter");

    // Reversed (tube_idx, fighter_id) argument order succeeds.
    session.drain_notifications();
    handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "LoadFightersToTube",
        &[PyRep::Integer(0), PyRep::Integer(fid1)],
        &kw,
    )
    .await
    .unwrap()
    .expect("reversed LoadFightersToTube");
    assert_eq!(
        ctx.fighter_tube_loads.read().unwrap().get(&(ship_id, 0)).copied(),
        Some(fid1)
    );
    assert_eq!(
        ctx.db.get_item(fid1).unwrap().flag,
        eve_db::InventoryFlag::FighterBay
    );
    assert!(session.drain_notifications().iter().any(|(n, payload)| {
        n == "OnFighterTubeContentUpdate"
            && payload == &vec![PyRep::Integer(159), PyRep::Integer(fid1), PyRep::Integer(23_057), PyRep::Integer(9)]
    }));

    // Same fighter cannot be loaded into another tube, nor another into an occupied tube.
    let dup_fighter = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "LoadFightersToTube",
        &[PyRep::Integer(fid1), PyRep::Integer(1)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(dup_fighter.code, "FighterAlreadyLoaded");

    // Load 2nd light fighter into tube 1; 3rd light and heavy both hit Chimera class limits (light=2, heavy=0).
    handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "LoadFightersToTube",
        &[PyRep::Integer(fid2), PyRep::Integer(1)],
        &kw,
    )
    .await
    .unwrap()
    .expect("second light load");

    let light_cap_err = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "LoadFightersToTube",
        &[PyRep::Integer(fid3), PyRep::Integer(2)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(light_cap_err.code, "CannotLoadFighterTubeNoTubesAvailableForClass");

    let heavy_cap_err = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "LoadFightersToTube",
        &[PyRep::Integer(f_heavy), PyRep::Integer(2)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(heavy_cap_err.code, "CannotLoadFighterTubeNoTubesExistForClass");

    // GetFightersForShip reflects the 2 loaded tubes before launch.
    let loaded_state = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "GetFightersForShip",
        &[PyRep::Integer(ship_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::Tuple(parts_loaded) = loaded_state else {
        panic!("expected tuple");
    };
    assert_eq!(parts_loaded.len(), 3);
    assert!(matches!(&parts_loaded[0], PyRep::List(v) if v.len() == 2));
    assert!(matches!(&parts_loaded[1], PyRep::List(v) if v.is_empty()));

    // UnloadTubeToFighterBay invalid, empty, and loaded branches.
    let unload_bad = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "UnloadTubeToFighterBay",
        &[PyRep::Integer(999)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(unload_bad.code, "InvalidFighterTube");

    let unload_flag_err = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "UnloadTubeToFighterBay",
        &[PyRep::Integer(161)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(unload_flag_err.code, "InvalidFighterTube");

    let unload_empty = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "UnloadTubeToFighterBay",
        &[PyRep::Integer(2)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(unload_empty, PyRep::Bool(false));

    session.drain_notifications();
    handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "UnloadTubeToFighterBay",
        &[PyRep::Integer(0)],
        &kw,
    )
    .await
    .unwrap()
    .expect("unload tube 0");
    assert!(!ctx.fighter_tube_loads.read().unwrap().contains_key(&(ship_id, 0)));
    assert_eq!(ctx.db.get_item(fid1).unwrap().flag, eve_db::InventoryFlag::FighterBay);
    assert!(session.drain_notifications().iter().any(|(n, p)| {
        n == "OnFighterTubeContentEmpty" && p == &vec![PyRep::Integer(159)]
    }));
}

#[tokio::test]
async fn launch_preconditions_recall_by_tube_rearm_and_launched_unload_guards() {
    let (ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let kw = std::collections::HashMap::new();
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    if sys_handle.get_ball_snapshot(ship_id).await.ok().flatten().is_none() {
        sys_handle
            .spawn_ball(eve_destiny::Ball {
                id: ship_id,
                type_id: 23_915,
                position: glam::DVec3::ZERO,
                velocity: glam::DVec3::ZERO,
                max_velocity: 100.0,
                inertia: 1.0,
                mass: 1_000_000_000.0,
                radius: 1_000.0,
                speed_fraction: 1.0,
                mode: MovementMode::Stop,
            })
            .await
            .unwrap();
    }

    let fid = 910_101i64;
    let reserve_fid = 910_102i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            fid,
            make_fighter_item(fid, 23_057, char_id, ship_id, eve_db::InventoryFlag::FighterBay, 9),
        );
        snap.items.insert(
            reserve_fid,
            make_fighter_item(reserve_fid, 23_057, char_id, ship_id, eve_db::InventoryFlag::FighterBay, 5),
        );
    });

    // Launching with nothing loaded fails with EmptyFighterTube; invalid tube flag fails InvalidFighterTube.
    let bad_launch = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "LaunchFightersFromTubes",
        &[PyRep::List(vec![PyRep::Integer(169)])],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(bad_launch.code, "InvalidFighterTube");

    let empty_launch = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "LaunchFightersFromTubes",
        &[PyRep::List(vec![PyRep::Integer(159)])],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(empty_launch.code, "EmptyFighterTube");

    handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "LoadFightersToTube",
        &[PyRep::Integer(fid), PyRep::Integer(0)],
        &kw,
    )
    .await
    .unwrap()
    .expect("load tube 0");

    // Launching while cloaked or warping is blocked cleanly.
    if sys_handle.get_combat_state(ship_id).await.ok().flatten().is_some() {
        sys_handle.try_recompute_combat_attributes(ship_id, |cs| cs.is_cloaked = true);
        let cloak_err = handle_fighters(
            &ctx,
            &mut session,
            "fighterMgr",
            "LaunchFightersFromTubes",
            &[PyRep::List(vec![PyRep::Integer(159)])],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(cloak_err.code, "CannotOpenFighterTubesWhileCloaked");
        sys_handle.try_recompute_combat_attributes(ship_id, |cs| cs.is_cloaked = false);
    }

    sys_handle
        .set_movement_mode(
            ship_id,
            MovementMode::Warp {
                origin: glam::DVec3::ZERO,
                destination: glam::DVec3::new(1_000_000.0, 0.0, 0.0),
                warp_speed_au_s: 3.0,
                total_distance: 1_000_000.0,
                phase: eve_destiny::WarpPhase::Accelerating,
                elapsed_warp_time: 0.0,
            },
        )
        .await
        .unwrap();
    let warp_err = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "LaunchFightersFromTubes",
        &[PyRep::List(vec![PyRep::Integer(159)])],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(warp_err.code, "CannotOpenFighterTubesWhileWarping");
    sys_handle
        .spawn_ball(eve_destiny::Ball {
            id: ship_id,
            type_id: 23_915,
            position: glam::DVec3::ZERO,
            velocity: glam::DVec3::ZERO,
            max_velocity: 100.0,
            inertia: 1.0,
            mass: 1_000_000_000.0,
            radius: 1_000.0,
            speed_fraction: 1.0,
            mode: MovementMode::Stop,
        })
        .await
        .unwrap();

    // Launch tube 159 cleanly.
    session.drain_notifications();
    handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "LaunchFightersFromTubes",
        &[PyRep::List(vec![PyRep::Integer(159)])],
        &kw,
    )
    .await
    .unwrap()
    .expect("launch tube 159");

    let launch_notifs = session.drain_notifications();
    assert!(launch_notifs.iter().any(|(n, _)| n == "OnFighterAddedToController"));
    assert!(launch_notifs.iter().any(|(n, p)| {
        n == "OnFighterTubeTaskStatus" && p.get(1) == Some(&PyRep::String("INSPACE".into()))
    }));

    // Unloading or re-launching a launched tube returns FighterAlreadyLaunched.
    let unload_launched = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "UnloadTubeToFighterBay",
        &[PyRep::Integer(0)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(unload_launched.code, "FighterAlreadyLaunched");

    let relaunch_err = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "LaunchFightersFromTubes",
        &[PyRep::List(vec![PyRep::Integer(159)])],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(relaunch_err.code, "FighterAlreadyLaunched");

    let space_state = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "GetFightersForShip",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::Tuple(parts_space) = space_state else {
        panic!("expected tuple");
    };
    assert!(matches!(&parts_space[1], PyRep::List(v) if v.len() == 1));

    // Damage squadron partially (still 9), then kill 2 units leaving 900.0 accum so recall has >1s repair/rearm.
    let fighter_ball = ctx
        .active_fighters
        .read()
        .unwrap()
        .values()
        .find(|f| f.parent_ship_id == ship_id && f.tube_index == 0)
        .unwrap()
        .ball_id;

    let (l0, r0, w0) = apply_damage_to_fighter_squadron(
        &ctx,
        sys_id,
        fighter_ball,
        DamageVector::new(400.0, 0.0, 0.0, 0.0),
    )
    .await;
    assert_eq!((l0, r0, w0), (0, 9, false));
    assert!((with_combat_runtime(&ctx, |rt| rt.fighter_unit_damage_accum[&fighter_ball]) - 400.0).abs() < 1e-6);

    let (l1, r1, w1) = apply_damage_to_fighter_squadron(
        &ctx,
        sys_id,
        fighter_ball,
        DamageVector::new(2_500.0, 0.0, 0.0, 0.0),
    )
    .await;
    assert_eq!((l1, r1, w1), (2, 7, false));
    assert!((with_combat_runtime(&ctx, |rt| rt.fighter_unit_damage_accum[&fighter_ball]) - 900.0).abs() < 1e-6);

    // Recall by tube flag [159]: repairs, tops up from reserve stack (5 -> 3), sets REFUELING + rearm lock.
    session.drain_notifications();
    handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "RecallFightersToTubes",
        &[PyRep::List(vec![PyRep::Integer(159)])],
        &kw,
    )
    .await
    .unwrap()
    .expect("recall by tube flag");
    assert!(!ctx.active_fighters.read().unwrap().contains_key(&fighter_ball));
    let reserve_after = ctx.db.get_item(reserve_fid).map(|i| i.quantity).unwrap_or(0);
    assert_eq!(reserve_after, 3);
    assert_eq!(ctx.db.get_item(fid).unwrap().quantity, 9);

    let recall_notifs = session.drain_notifications();
    assert!(recall_notifs.iter().any(|(n, _)| n == "OnFighterRemovedFromController"));
    assert!(recall_notifs.iter().any(|(n, p)| {
        n == "OnFighterTubeTaskStatus" && p.get(1) == Some(&PyRep::String("REFUELING".into()))
    }));

    let rearm_err = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "LaunchFightersFromTubes",
        &[PyRep::List(vec![PyRep::Integer(159)])],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(rearm_err.code, "TubeRearming");
}

#[tokio::test]
async fn movement_approach_rpc_args_errors_deactivate_point_ability_and_missing_damage() {
    let (ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let kw = std::collections::HashMap::new();
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    if sys_handle.get_ball_snapshot(ship_id).await.ok().flatten().is_none() {
        sys_handle
            .spawn_ball(eve_destiny::Ball {
                id: ship_id,
                type_id: 23_915,
                position: glam::DVec3::ZERO,
                velocity: glam::DVec3::ZERO,
                max_velocity: 100.0,
                inertia: 1.0,
                mass: 1_000_000_000.0,
                radius: 1_000.0,
                speed_fraction: 1.0,
                mode: MovementMode::Stop,
            })
            .await
            .unwrap();
    }
    let target_id = 910_299i64;
    sys_handle
        .spawn_ball(eve_destiny::Ball {
            id: target_id,
            type_id: 587,
            position: glam::DVec3::new(5_000.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 1.0,
            mode: MovementMode::Stop,
        })
        .await
        .unwrap();

    // Missing squadron returns (0, 0, false).
    assert_eq!(
        apply_damage_to_fighter_squadron(&ctx, sys_id, 9_888_777, DamageVector::new(100.0, 0.0, 0.0, 0.0)).await,
        (0, 0, false)
    );

    let fid = 910_201i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            fid,
            make_fighter_item(fid, 23_057, char_id, ship_id, eve_db::InventoryFlag::FighterBay, 9),
        );
    });
    handle_fighters(&ctx, &mut session, "fighterMgr", "LoadFightersToTube", &[PyRep::Integer(fid), PyRep::Integer(0)], &kw)
        .await
        .unwrap()
        .unwrap();
    handle_fighters(&ctx, &mut session, "fighterMgr", "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(159)])], &kw)
        .await
        .unwrap()
        .unwrap();
    let fball = ctx.active_fighters.read().unwrap().values().find(|f| f.item_id == fid).unwrap().ball_id;

    // Movement validation errors (empty IDs, missing fighter, negative range, zero target, NaN vec3).
    assert_eq!(
        handle_fighters(&ctx, &mut session, "fighterMgr", "ExecuteMovementCommandOnFighters", &[PyRep::List(vec![]), PyRep::String("STOP".into())], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        handle_fighters(&ctx, &mut session, "fighterMgr", "ExecuteMovementCommandOnFighters", &[PyRep::Integer(9_999_111), PyRep::String("STOP".into())], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "FighterNotFound"
    );
    assert_eq!(
        handle_fighters(
            &ctx,
            &mut session,
            "fighterMgr",
            "ExecuteMovementCommandOnFighters",
            &[PyRep::Integer(fball), PyRep::String("ORBIT".into()), PyRep::Integer(target_id), PyRep::Real(-50.0)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidArgument"
    );
    assert_eq!(
        handle_fighters(
            &ctx,
            &mut session,
            "fighterMgr",
            "ExecuteMovementCommandOnFighters",
            &[PyRep::Integer(fball), PyRep::String("APPROACH".into()), PyRep::Integer(0)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidArgument"
    );
    assert_eq!(
        handle_fighters(
            &ctx,
            &mut session,
            "fighterMgr",
            "ExecuteMovementCommandOnFighters",
            &[
                PyRep::Integer(fball),
                PyRep::String("GOTO_POINT".into()),
                PyRep::Tuple(vec![PyRep::Real(f64::NAN), PyRep::Real(0.0), PyRep::Real(0.0)]),
            ],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidArgument"
    );

    // APPROACH sets MovementMode::Follow with exact range.
    let approach_rep = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "ExecuteMovementCommandOnFighters",
        &[PyRep::Integer(fball), PyRep::String("APPROACH".into()), PyRep::Integer(target_id), PyRep::Real(750.0)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(approach_rep, PyRep::Bool(true));
    let (_, ball_snap) = sys_handle.get_ball_snapshot(fball).await.unwrap().unwrap();
    assert!(matches!(ball_snap.mode, MovementMode::Follow { target_id: tid, range } if tid == target_id && (range - 750.0).abs() < 1e-6));

    // Activate untargeted MWD slot 1, then CmdDeactivateAbilitySlots + error branches.
    handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "CmdActivateAbilitySlots",
        &[PyRep::List(vec![PyRep::Integer(fball)]), PyRep::Integer(1)],
        &kw,
    )
    .await
    .unwrap()
    .expect("activate fighter MWD slot 1");

    assert_eq!(
        handle_fighters(
            &ctx,
            &mut session,
            "fighterMgr",
            "CmdDeactivateAbilitySlots",
            &[PyRep::Integer(fball), PyRep::Integer(9)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidFighterAbilitySlot"
    );
    assert_eq!(
        handle_fighters(
            &ctx,
            &mut session,
            "fighterMgr",
            "CmdDeactivateAbilitySlots",
            &[PyRep::Integer(9_999_222), PyRep::Integer(1)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "FighterNotFound"
    );

    session.drain_notifications();
    let deact_rep = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "CmdDeactivateAbilitySlots",
        &[PyRep::Integer(fball), PyRep::Integer(1)],
        &kw,
    )
    .await
    .unwrap()
    .expect("CmdDeactivateAbilitySlots slot 1");
    assert_eq!(deact_rep.dict_get("deactivated"), Some(&PyRep::Bool(true)));
    assert_eq!(deact_rep.dict_get("success"), Some(&PyRep::Bool(true)));
    assert!(!ctx.active_fighters.read().unwrap().get(&fball).unwrap().mwd_active);
    assert!(session.drain_notifications().iter().any(|(n, p)| {
        n == "OnFighterAbilitySlotDeactivated"
            && p == &vec![PyRep::Integer(fball), PyRep::Integer(1), PyRep::None]
    }));

    // ActivateAbilitySlotsAtPoint guard branches.
    assert_eq!(
        handle_fighters(
            &ctx,
            &mut session,
            "fighterMgr",
            "ActivateAbilitySlotsAtPoint",
            &[PyRep::List(vec![]), PyRep::Integer(0), PyRep::Tuple(vec![PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(0.0)])],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "FighterNotFound"
    );
    assert_eq!(
        handle_fighters(
            &ctx,
            &mut session,
            "fighterMgr",
            "ActivateAbilitySlotsAtPoint",
            &[PyRep::Integer(fball), PyRep::Integer(0), PyRep::Tuple(vec![PyRep::Real(f64::INFINITY), PyRep::Real(0.0), PyRep::Real(0.0)])],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidTargetPoint"
    );
    assert_eq!(
        handle_fighters(
            &ctx,
            &mut session,
            "fighterMgr",
            "ActivateAbilitySlotsAtPoint",
            &[PyRep::Integer(fball), PyRep::Integer(0), PyRep::Tuple(vec![PyRep::Real(1_000.0), PyRep::Real(0.0), PyRep::Real(0.0)])],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidTargetMode"
    );
}

