//! Semantic unit tests split from `combat_fighters_extra_unit_tests`.

use super::combat_fighters_extra_unit_tests::*;
use crate::router::context::ActiveFighterSquadron;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item,
};
use crate::services::combat_crimewatch::combat_fighters::*;
use crate::services::combat_crimewatch::with_mut_combat_runtime;
use eve_db::InventoryFlag;
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_proto::PyRep;
use glam::DVec3;
use std::collections::HashMap;

#[tokio::test]
async fn launch_and_unload_tube_edge_cases_warp_tether_rearm_and_empty() {
    let (ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let kw = HashMap::new();

    // 1. UnloadTubeToFighterBay invalid tube and empty tube returning Bool(false)
    let err_neg = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "UnloadTubeToFighterBay",
        &[PyRep::Integer(-1)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_neg.code, "InvalidFighterTube");

    let empty_unload = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "UnloadTubeToFighterBay",
        &[PyRep::Integer(0)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(empty_unload, PyRep::Bool(false));

    // 2. LaunchFightersFromTubes when ship ball is not in space -> ShipNotInSpace
    let err_not_in_space = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "LaunchFightersFromTubes",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_not_in_space.code, "ShipNotInSpace");

    // 3. Ship in Warp -> CannotOpenFighterTubesWhileWarping
    let warp_mode = MovementMode::new_warp(DVec3::ZERO, DVec3::new(100_000.0, 0.0, 0.0), 3.0);
    spawn_carrier_ball(&ctx, sys_id, ship_id, 23_915, DVec3::ZERO, warp_mode, CombatState::default()).await;
    let err_warp = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "LaunchFightersFromTubes",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_warp.code, "CannotOpenFighterTubesWhileWarping");

    // 4. Stop ship, test EmptyFighterTube when no tubes loaded
    spawn_carrier_ball(&ctx, sys_id, ship_id, 23_915, DVec3::ZERO, MovementMode::Stop, CombatState::default()).await;
    let err_empty = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "LaunchFightersFromTubes",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_empty.code, "EmptyFighterTube");

    // 5. Load tube 0, set rearm timer -> TubeRearming; clear rearm timer and launch with no args -> returns List
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            602_001,
            make_fighter_item(602_001, 23_057, char_id, ship_id, InventoryFlag::FighterBay, 9),
        );
    });
    ctx.fighter_tube_loads.write().unwrap().insert((ship_id, 0), 602_001);
    with_mut_combat_runtime(&ctx, |rt| {
        rt.fighter_tube_rearm_until.insert((ship_id, 0), ctx.now_unix() + 60);
    });
    let err_rearm = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "LaunchFightersFromTubes",
        &[PyRep::Integer(0)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_rearm.code, "TubeRearming");

    with_mut_combat_runtime(&ctx, |rt| {
        rt.fighter_tube_rearm_until.remove(&(ship_id, 0));
    });
    let launched = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "LaunchFightersFromTubes",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::List(ball_list) = launched else { panic!("expected list of launched ball IDs") };
    assert_eq!(ball_list.len(), 1);

    // 6. Calling LaunchFightersFromTubes again with no args while fighter is active -> FighterAlreadyLaunched
    let err_already = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "LaunchFightersFromTubes",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_already.code, "FighterAlreadyLaunched");

    // 7. UnloadTubeToFighterBay while tube 0 is deployed in space -> FighterAlreadyLaunched
    let err_unload_active = handle_fighters(
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
    assert_eq!(err_unload_active.code, "FighterAlreadyLaunched");
}

#[tokio::test]
async fn scoop_abandoned_fighter_corpmate_access_docked_and_bay_capacity_checks() {
    let (ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let kw = HashMap::new();
    spawn_carrier_ball(&ctx, sys_id, ship_id, 23_915, DVec3::ZERO, MovementMode::Stop, CombatState::default()).await;

    // 1. Docked check -> ShipDocked
    let mut docked_sess = session.clone();
    docked_sess.station_id = Some(60_003_760);
    let err_docked = handle_fighters(
        &ctx,
        &mut docked_sess,
        "fighterMgr",
        "CmdScoopAbandonedFighterFromSpace",
        &[PyRep::Integer(440_001)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_docked.code, "ShipDocked");

    // 2. Invalid fighter ID (<= 0) -> InvalidFighterID
    let err_id = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "CmdScoopAbandonedFighterFromSpace",
        &[PyRep::Integer(0)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_id.code, "InvalidFighterID");

    // 3. Abandoned fighter owned by foreign corporation character -> NotFighterOwner
    let f_ball = 440_001i64;
    let f_item = 540_001i64;
    let other_char = 90_099_001i64;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: f_ball,
                type_id: 23_057,
                position: DVec3::new(500.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 2_000.0,
                inertia: 0.5,
                mass: 25_000.0,
                radius: 25.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await
        .unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters[&char_id].clone();
        ch.character_id = other_char;
        ch.corporation_id = 98_999_999; // different corp
        snap.characters.insert(other_char, ch);
        snap.items.insert(
            f_item,
            make_fighter_item(f_item, 23_057, other_char, sys_id as i64, InventoryFlag::None, 9),
        );
    });
    ctx.active_fighters.write().unwrap().insert(
        f_ball,
        ActiveFighterSquadron {
            ball_id: f_ball,
            item_id: f_item,
            type_id: 23_057,
            owner_char_id: other_char,
            parent_ship_id: 0,
            tube_index: 0,
            squadron_size: 9,
            target_id: None,
            is_abandoned: true,
            mwd_active: false,
        },
    );
    let err_foreign = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "CmdScoopAbandonedFighterFromSpace",
        &[PyRep::Integer(f_ball)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_foreign.code, "NotFighterOwner");

    // 4. Put other_char in same corp, but fill carrier's fighter bay (capacity 50_000m3) with 45 units (45_000m3)
    // so scooping 9 units (9_000m3) exceeds 50_000m3 -> FighterBayFull!
    ctx.db.with_mut_snapshot(|snap| {
        let caller_corp = snap.characters[&char_id].corporation_id;
        snap.characters.get_mut(&other_char).unwrap().corporation_id = caller_corp;
        snap.items.insert(
            540_002,
            make_fighter_item(540_002, 23_057, char_id, ship_id, InventoryFlag::FighterBay, 45),
        );
    });
    let err_full = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "CmdScoopAbandonedFighterFromSpace",
        &[PyRep::Integer(f_ball)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_full.code, "FighterBayFull");

    // 5. Reduce fighter bay usage and scoop corpmate's abandoned fighter -> succeeds and transfers ownership to char_id
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&540_002);
    });
    let ok_scoop = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "CmdScoopAbandonedFighterFromSpace",
        &[PyRep::Integer(f_ball)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(ok_scoop, PyRep::Bool(true));
    assert_eq!(ctx.db.get_item(f_item).unwrap().owner_id, char_id);
}
