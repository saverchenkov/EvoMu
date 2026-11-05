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
