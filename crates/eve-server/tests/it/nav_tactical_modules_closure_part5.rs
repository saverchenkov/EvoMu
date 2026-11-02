//! Negative integration tests for `R-INT-01` (Interdiction Sphere Launcher activation
//! rejections when docked, not in space, cloaked, tethered, or in active warp) and `R-MJD-02`
//! (`warpVectorMgr.UseVector` rejection during MJD spool + `Ballpark::micro_jump` /
//! `Ballpark::micro_jump_field` rejection during active warp).

use crate::esi_market_industry_pi::make_structure;
use crate::modules_advanced_tactical::*;
use eve_db::InventoryFlag;
use eve_destiny::{Ball, Ballpark, DVec3, MovementMode, WarpPhase};
use eve_proto::PyRep;
use std::sync::Arc;

#[tokio::test]
async fn test_interdiction_sphere_launcher_negative_states() {
    let ctx = Arc::new(build_ctx());
    let jita_sys: u32 = 30_004_759;
    let char_id: i64 = 94_201;
    let ship_id: i64 = 84_201;
    let launcher_id: i64 = 74_201;
    let probe_id: i64 = 74_202;

    // Sabre (22456, group 541 Interdictor) + Interdiction Sphere Launcher I (22782) + Warp Disrupt Probe (22778)
    let mut sess = seed_pilot(&ctx, char_id, ship_id, 22_456, jita_sys, false);
    give_item(&ctx, launcher_id, 22_782, char_id, ship_id, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, probe_id, 22_778, char_id, ship_id, InventoryFlag::HiSlot0, 5);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(launcher_id, true);
    });
    ctx.invalidate_ship_fitting(ship_id);

    let sys = ctx.systems.get_or_spawn(jita_sys);

    // 1. Docked -> ShipDocked (even if ball is present)
    let mut cs_init = ctx.build_ship_combat_state(char_id, ship_id, 22_456);
    cs_init.capacitor = 100_000.0;
    cs_init.max_capacitor = 100_000.0;
    sys.spawn_ball_with_meta(
        Ball {
            id: ship_id,
            type_id: 22_456,
            position: DVec3::new(50_000_000_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_500_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        cs_init,
        ctx.build_ship_slim_meta(char_id, 22_456),
    )
    .await
    .unwrap();
    sess.station_id = Some(60_003_760);
    ctx.db.with_mut_snapshot(|s| {
        if let Some(c) = s.characters.get_mut(&char_id) {
            c.station_id = Some(60_003_760);
        }
    });
    let err_docked = rpc(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(launcher_id)])
        .await
        .unwrap_err();
    assert_eq!(err_docked.code, "ShipDocked");
    sess.station_id = None;
    ctx.db.with_mut_snapshot(|s| {
        if let Some(c) = s.characters.get_mut(&char_id) {
            c.station_id = None;
        }
    });

    // 2. Not in space -> ShipNotInSpace
    let _ = sys.remove_ball(ship_id).await;
    let fit = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap();
    let err_no_ball = eve_server::services::tactical_module_mechanics::activate_interdiction_sphere_launcher(
        &ctx, &sys, jita_sys, char_id, ship_id, 22_456, launcher_id,
        fit.modules.get(&launcher_id).unwrap(), ctx.sde.types.get(&22_782).unwrap(), "online", None,
    )
    .await
    .unwrap_err();
    assert_eq!(err_no_ball.code, "ShipNotInSpace");

    // 3. Cloaked -> CannotActivateCloaked
    let mut cs_cloaked = ctx.build_ship_combat_state(char_id, ship_id, 22_456);
    cs_cloaked.capacitor = 100_000.0;
    cs_cloaked.max_capacitor = 100_000.0;
    cs_cloaked.is_cloaked = true;
    sys.spawn_ball_with_meta(
        Ball {
            id: ship_id,
            type_id: 22_456,
            position: DVec3::new(50_000_000_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_500_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        cs_cloaked,
        ctx.build_ship_slim_meta(char_id, 22_456),
    )
    .await
    .unwrap();
    let err_cloaked = rpc(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(launcher_id)])
        .await
        .unwrap_err();
    assert_eq!(err_cloaked.code, "CannotActivateCloaked");

    // 4. Tethered -> CantActivateWhileTethered
    let _ = sys.remove_ball(ship_id).await;
    let mut cs_tethered = ctx.build_ship_combat_state(char_id, ship_id, 22_456);
    cs_tethered.capacitor = 100_000.0;
    cs_tethered.max_capacitor = 100_000.0;
    cs_tethered.is_tethered = true;
    sys.spawn_ball_with_meta(
        Ball {
            id: ship_id,
            type_id: 22_456,
            position: DVec3::new(50_000_000_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_500_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        cs_tethered,
        ctx.build_ship_slim_meta(char_id, 22_456),
    )
    .await
    .unwrap();
    let err_tethered = rpc(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(launcher_id)])
        .await
        .unwrap_err();
    assert_eq!(err_tethered.code, "CantActivateWhileTethered");

    // 5. Active non-Aligning Warp -> CantActivateInWarp
    let _ = sys.remove_ball(ship_id).await;
    let mut cs_warping = ctx.build_ship_combat_state(char_id, ship_id, 22_456);
    cs_warping.capacitor = 100_000.0;
    cs_warping.max_capacitor = 100_000.0;
    sys.spawn_ball_with_meta(
        Ball {
            id: ship_id,
            type_id: 22_456,
            position: DVec3::new(50_000_000_000.0, 0.0, 0.0),
            velocity: DVec3::new(1_000_000.0, 0.0, 0.0),
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_500_000.0,
            radius: 50.0,
            speed_fraction: 1.0,
            mode: MovementMode::Warp {
                origin: DVec3::new(50_000_000_000.0, 0.0, 0.0),
                destination: DVec3::new(100_000_000_000.0, 0.0, 0.0),
                warp_speed_au_s: 3.0,
                total_distance: 50_000_000_000.0,
                elapsed_warp_time: 2.0,
                phase: WarpPhase::Cruising,
            },
        },
        cs_warping,
        ctx.build_ship_slim_meta(char_id, 22_456),
    )
    .await
    .unwrap();
    let err_warping = rpc(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(launcher_id)])
        .await
        .unwrap_err();
    assert_eq!(err_warping.code, "CantActivateInWarp");
}

#[tokio::test]
async fn test_warp_vector_rejected_during_mjd_spool() {
    let ctx = Arc::new(build_ctx());
    let jita_sys: u32 = 30_004_759;
    let char_id: i64 = 94_202;
    let ship_id: i64 = 84_202;
    let mjd_id: i64 = 74_211;
    let vector_id: i64 = 74_212;

    // Scorpion (640, Battleship) + Large Micro Jump Drive (4383) + Jovian Symbolic Navigator (77825)
    let mut sess = seed_pilot(&ctx, char_id, ship_id, 640, jita_sys, false);
    give_item(&ctx, mjd_id, 4_383, char_id, ship_id, InventoryFlag::MedSlot0, 1);
    give_item(&ctx, vector_id, 77_825, char_id, ship_id, InventoryFlag::Cargo, 2);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(mjd_id, true);
    });
    ctx.invalidate_ship_fitting(ship_id);

    let sys = ctx.systems.get_or_spawn(jita_sys);
    let mut cs = ctx.build_ship_combat_state(char_id, ship_id, 640);
    cs.capacitor = 100_000.0;
    cs.max_capacitor = 100_000.0;
    sys.spawn_ball_with_meta(
        Ball {
            id: ship_id,
            type_id: 640,
            position: DVec3::new(100_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 150.0,
            inertia: 1.0,
            mass: 100_000_000.0,
            radius: 250.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        cs,
        ctx.build_ship_slim_meta(char_id, 640),
    )
    .await
    .unwrap();

    // Start MJD spool
    rpc(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(mjd_id)])
        .await
        .expect("Activate MJD spool");

    // warpVectorMgr.UseVector must fail during MJD spool
    let err_spool = rpc(&ctx, &mut sess, "warpVectorMgr", "UseVector", vec![PyRep::Integer(vector_id)])
        .await
        .unwrap_err();
    assert!(
        err_spool.code.contains("Mjd") || err_spool.message.to_lowercase().contains("micro jump"),
        "Expected MJD spool rejection, got: {err_spool:?}"
    );

    // Defense-in-depth: Ballpark::micro_jump and micro_jump_field reject active non-Aligning Warp
    let mut bp = Ballpark::new(jita_sys);
    bp.add_ball(Ball {
        id: ship_id,
        type_id: 640,
        position: DVec3::ZERO,
        velocity: DVec3::new(1_000_000.0, 0.0, 0.0),
        max_velocity: 150.0,
        inertia: 1.0,
        mass: 100_000_000.0,
        radius: 250.0,
        speed_fraction: 1.0,
        mode: MovementMode::Warp {
            origin: DVec3::ZERO,
            destination: DVec3::new(100_000_000_000.0, 0.0, 0.0),
            warp_speed_au_s: 3.0,
            total_distance: 100_000_000_000.0,
            elapsed_warp_time: 2.0,
            phase: WarpPhase::Cruising,
        },
    });
    assert!(bp.micro_jump(ship_id, 100_000.0).unwrap_err().contains("CantActivateInWarp"));
    assert!(bp.micro_jump_field(ship_id, 100_000.0, 6_000.0, 25).unwrap_err().contains("CantActivateInWarp"));
}

#[tokio::test]
async fn test_warp_bubble_warp_entry_only_and_shuttle_emergency_exemptions() {
    let mut bp = Ballpark::new(30_000_142);
    let origin = DVec3::ZERO;
    let dest = DVec3::new(100_000_000.0, 0.0, 0.0);
    let bubble_center = DVec3::new(100_000_000.0, 0.0, 0.0);

    // (a) Bubble present at set_mode that despawns during Aligning must NOT trap the ship
    bp.add_ball(Ball {
        id: 1001, type_id: 587, position: origin, velocity: DVec3::ZERO,
        max_velocity: 365.0, inertia: 3.45, mass: 1_067_000.0, radius: 37.0,
        speed_fraction: 0.0, mode: MovementMode::Stop,
    });
    let bid: i64 = 5001;
    bp.warp_bubble_mgr.add_bubble(eve_destiny::WarpBubble::new(bid, bubble_center, 20_000.0));
    bp.set_mode(1001, MovementMode::new_warp(origin, dest, 3.0)).unwrap();
    if let MovementMode::Warp { destination, .. } = bp.balls[&1001].mode {
        assert_eq!(destination, dest, "set_mode must not prematurely mutate warp destination");
    } else {
        panic!("Expected Warp mode");
    }
    bp.warp_bubble_mgr.remove_bubble(bid);
    for _ in 0..10 {
        bp.step(1.0);
    }
    if let MovementMode::Warp { destination, .. } = bp.balls[&1001].mode {
        assert_eq!(destination, dest, "Despawned bubble during alignment must not trap ship");
    }

    // (b) is_emergency_warping disconnect warp toward a bubbled destination is exempt
    bp.warp_bubble_mgr.add_bubble(eve_destiny::WarpBubble::new(5002, bubble_center, 20_000.0));
    let mut cs_emerg = eve_destiny::CombatState::default();
    cs_emerg.is_emergency_warping = true;
    bp.combat_states.insert(1002, cs_emerg);
    bp.add_ball(Ball {
        id: 1002, type_id: 587, position: origin, velocity: DVec3::ZERO,
        max_velocity: 365.0, inertia: 1.0, mass: 1_067_000.0, radius: 37.0,
        speed_fraction: 0.0, mode: MovementMode::new_warp(origin, dest, 3.0),
    });
    for _ in 0..5 {
        bp.step(1.0);
    }
    if let MovementMode::Warp { destination, .. } = bp.balls[&1002].mode {
        assert_eq!(destination, dest, "Emergency disconnect warp must be exempt from warp bubbles");
    }

    // (c) Newly spawned group 31 shuttle warping toward a bubbled destination is exempt
    bp.ball_group_ids.insert(1003, 31);
    bp.add_ball(Ball {
        id: 1003, type_id: 672, position: origin, velocity: DVec3::ZERO,
        max_velocity: 500.0, inertia: 1.0, mass: 1_000_000.0, radius: 25.0,
        speed_fraction: 0.0, mode: MovementMode::new_warp(origin, dest, 5.0),
    });
    for _ in 0..5 {
        bp.step(1.0);
    }
    if let MovementMode::Warp { destination, .. } = bp.balls[&1003].mode {
        assert_eq!(destination, dest, "Group 31 shuttle must be exempt from warp bubbles");
    }
}

#[tokio::test]
async fn test_mid_cycle_overload_and_burnout_ship_fitting_sync() {
    let ctx = Arc::new(build_ctx());
    let jita_sys: u32 = 30_004_759;
    let char_id: i64 = 95_011;
    let ship_id: i64 = 96_011;
    let mwd_id: i64 = 97_011;
    let mut sess = seed_pilot(&ctx, char_id, ship_id, 587, jita_sys, false);
    give_item(&ctx, mwd_id, 439, char_id, ship_id, InventoryFlag::MedSlot0, 1);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(mwd_id, true);
    });
    ctx.invalidate_ship_fitting(ship_id);
    let sys = ctx.systems.get_or_spawn(jita_sys);
    let mut cs = ctx.build_ship_combat_state(char_id, ship_id, 587);
    cs.capacitor = 100_000.0;
    cs.max_capacitor = 100_000.0;
    sys.spawn_ball_with_meta(
        Ball {
            id: ship_id, type_id: 587, position: DVec3::new(50_000_000_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO, max_velocity: 365.0, inertia: 1.0, mass: 1_067_000.0,
            radius: 37.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        },
        cs,
        ctx.build_ship_slim_meta(char_id, 587),
    ).await.unwrap();

    // Activate MWD (non-overloaded)
    rpc(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(mwd_id), PyRep::String("online".into())])
        .await
        .expect("Activate MWD");
    let fit_before = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap();
    assert_eq!(fit_before.modules[&mwd_id].state, eve_dogma::ModuleState::Active);

    // Mid-cycle Overload must defer ShipFittingEngine state until cycle boundary
    rpc(&ctx, &mut sess, "dogmaIM", "Overload", vec![PyRep::Integer(mwd_id)])
        .await
        .expect("Overload mid-cycle");
    let fit_mid = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap();
    assert_eq!(
        fit_mid.modules[&mwd_id].state,
        eve_dogma::ModuleState::Active,
        "Mid-cycle Overload must not immediately mutate cached ShipFittingEngine state"
    );

    // Advance past 10s MWD cycle boundary -> SyncOverloadState updates ShipFittingEngine to Overloaded
    for _ in 0..11 {
        sys.step_manual(1.0).await.unwrap();
    }
    let fit_after = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap();
    assert_eq!(
        fit_after.modules[&mwd_id].state,
        eve_dogma::ModuleState::Overloaded,
        "Cycle boundary must transition cached ShipFittingEngine state to Overloaded"
    );
}

#[tokio::test]
async fn test_warp_to_within_min_range_sub_150km_rejected() {
    let ctx = Arc::new(build_ctx());
    let jita_sys: u32 = 30_004_759;
    let char_id: i64 = 95_012;
    let ship_id: i64 = 96_012;
    let target_id: i64 = 1_000_000_098_012;
    let mut sess = seed_pilot(&ctx, char_id, ship_id, 587, jita_sys, false);
    let sys = ctx.systems.get_or_spawn(jita_sys);
    let origin = DVec3::new(50_000_000_000.0, 0.0, 0.0);
    let target_pos = DVec3::new(50_000_250_000.0, 0.0, 0.0); // 250 km away (Astrahus radius 47 km -> 203 km landing dist)

    let mut cs = ctx.build_ship_combat_state(char_id, ship_id, 587);
    cs.capacitor = 100_000.0;
    cs.max_capacitor = 100_000.0;
    sys.spawn_ball_with_meta(
        Ball {
            id: ship_id, type_id: 587, position: origin,
            velocity: DVec3::ZERO, max_velocity: 365.0, inertia: 1.0, mass: 1_067_000.0,
            radius: 37.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        },
        cs,
        ctx.build_ship_slim_meta(char_id, 587),
    ).await.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        let mut st = make_structure(target_id, 35_832, 98_000_001, jita_sys, "Target Astrahus");
        st.position = target_pos.to_array();
        snap.structures.insert(target_id, st);
    });

    // "Warp to Within 100 km" on a 180 km target -> 80 km effective warp < 150 km -> must reject
    let err = rpc(
        &ctx,
        &mut sess,
        "beyonce",
        "CmdWarpToStuff",
        vec![
            PyRep::String("item".into()),
            PyRep::Integer(target_id),
            PyRep::Real(100_000.0),
        ],
    )
    .await
    .expect_err("Warp to Within 100 km on 180 km target must be rejected");
    assert_eq!(err.code, "WarpDistanceTooShort");

    // "Warp to Within 20 km" on a 180 km target -> 160 km effective warp >= 150 km -> must succeed
    rpc(
        &ctx,
        &mut sess,
        "beyonce",
        "CmdWarpToStuff",
        vec![
            PyRep::String("item".into()),
            PyRep::Integer(target_id),
            PyRep::Real(20_000.0),
        ],
    )
    .await
    .expect("Warp to Within 20 km on 180 km target must succeed");
}

#[tokio::test]
async fn test_structure_docking_advances_waypoints_and_uses_sde_radius_fallback() {
    let ctx = Arc::new(build_ctx());
    let jita_sys: u32 = 30_004_759;
    let char_id: i64 = 95_013;
    let ship_id: i64 = 96_013;
    let structure_id: i64 = 1_000_000_099_013;
    let astrahus_type_id: u32 = 35_832;
    let sde_radius = ctx
        .sde
        .types
        .get(&astrahus_type_id)
        .map(|t| t.radius)
        .filter(|&r| r > 0.0)
        .expect("Astrahus SDE radius must be > 0");

    let mut sess = seed_pilot(&ctx, char_id, ship_id, 587, jita_sys, false);
    let corp_id = sess.corporation_id.unwrap_or(1_000_009);
    let structure_pos = DVec3::new(50_000_000_000.0, 0.0, 0.0);
    ctx.db.with_mut_snapshot(|snap| {
        let mut st = make_structure(structure_id, astrahus_type_id, corp_id, jita_sys, "Test Astrahus");
        st.position = structure_pos.to_array();
        snap.structures.insert(structure_id, st);
    });

    // Set waypoints [structure_id, 30_004_759]
    rpc(
        &ctx,
        &mut sess,
        "charMgr",
        "SaveCharacterSetting",
        vec![
            PyRep::String("waypoints".into()),
            PyRep::List(vec![
                PyRep::Integer(structure_id),
                PyRep::Integer(jita_sys as i64),
            ]),
        ],
    )
    .await
    .expect("SaveCharacterSetting waypoints");

    // Spawn ship at sde_radius + 2_000 m (outside 2_500 m if radius were 0.0, inside sde_radius + 2_500 m)
    // Intentionally do NOT spawn a Ballpark ball for structure_id to test SDE radius fallback.
    let sys = ctx.systems.get_or_spawn(jita_sys);
    let cs = ctx.build_ship_combat_state(char_id, ship_id, 587);
    sys.spawn_ball_with_meta(
        Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::new(structure_pos.x + sde_radius + 2_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 365.0,
            inertia: 1.0,
            mass: 1_067_000.0,
            radius: 37.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        cs,
        ctx.build_ship_slim_meta(char_id, 587),
    )
    .await
    .unwrap();

    rpc(
        &ctx,
        &mut sess,
        "structureDocking",
        "Dock",
        vec![PyRep::Integer(structure_id)],
    )
    .await
    .expect("structureDocking.Dock must succeed using SDE radius fallback");

    let settings = rpc(&ctx, &mut sess, "charMgr", "GetCharacterSettings", vec![])
        .await
        .expect("GetCharacterSettings after structureDocking.Dock");
    assert_eq!(
        settings.dict_get("waypoints"),
        Some(&PyRep::List(vec![PyRep::Integer(jita_sys as i64)])),
        "structureDocking.Dock must advance waypoints on arrival"
    );
}

#[tokio::test]
async fn test_system_actor_post_entry_bubble_does_not_yank_active_warp() {
    let ctx = Arc::new(build_ctx());
    let jita_sys: u32 = 30_000_142;
    let char_id: i64 = 900_991;
    let ship_id: i64 = 100_991;
    let sys = ctx.systems.get_or_spawn(jita_sys);

    let origin = DVec3::ZERO;
    let dest = DVec3::new(149_597_870_700.0, 0.0, 0.0); // 1 AU
    let cs = ctx.build_ship_combat_state(char_id, ship_id, 587);
    sys.spawn_ball_with_meta(
        Ball {
            id: ship_id,
            type_id: 587,
            position: origin,
            velocity: DVec3::ZERO,
            max_velocity: 365.0,
            inertia: 1.0,
            mass: 1_067_000.0,
            radius: 37.0,
            speed_fraction: 0.0,
            mode: MovementMode::new_warp(origin, dest, 3.0),
        },
        cs,
        ctx.build_ship_slim_meta(char_id, 587),
    )
    .await
    .unwrap();

    // Step until ship transitions past WarpPhase::Aligning into active warp
    for _ in 0..5 {
        sys.step_manual(1.0).await.unwrap();
    }
    let (_, mid_ball) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(
        matches!(mid_ball.mode, MovementMode::Warp { phase, .. } if phase != eve_destiny::WarpPhase::Aligning),
        "Ship should have exited Aligning into active warp, got {:?}",
        mid_ball.mode
    );

    // Launch a warp disruption bubble at the destination AFTER warp entry
    sys.add_warp_bubble(88_001, dest, 20_000.0).await.unwrap();

    // Step through remaining warp ticks; SystemActor must NOT yank ship out of active warp
    for _ in 0..45 {
        sys.step_manual(1.0).await.unwrap();
    }
    let (_, final_ball) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(
        final_ball.position.distance(dest) <= 3_000.0,
        "Post-entry bubble must not yank active warp; ship landed at {:?} (dist {:.1} m from dest)",
        final_ball.position,
        final_ball.position.distance(dest)
    );
}

#[tokio::test]
async fn test_warp_vector_rejects_mid_warp_and_abyssal_and_cmd_dock_checks_upwell_tackle() {
    let ctx = Arc::new(build_ctx());
    let jita_sys: u32 = 30_000_142;
    let char_id: i64 = 900_992;
    let ship_id: i64 = 100_992;
    let mut sess = seed_pilot(&ctx, char_id, ship_id, 587, jita_sys, false);
    let corp_id = sess.corporation_id.unwrap_or(1_000_009);
    let sys = ctx.systems.get_or_spawn(jita_sys);
    let cs = ctx.build_ship_combat_state(char_id, ship_id, 587);
    sys.spawn_ball_with_meta(
        Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 365.0,
            inertia: 1.0,
            mass: 1_067_000.0,
            radius: 37.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        cs,
        ctx.build_ship_slim_meta(char_id, 587),
    )
    .await
    .unwrap();

    // Seed a Warp Vector Data item (77825, group 4599) in ship cargo
    let vec_item_id: i64 = 777_825;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            vec_item_id,
            eve_db::InventoryItemRecord {
                item_id: vec_item_id,
                type_id: 77825,
                owner_id: char_id,
                location_id: ship_id,
                flag: eve_db::InventoryFlag::Cargo,
                quantity: 2,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    // 1. Mid-warp trajectory redirection via warpVectorMgr.UseVector must be rejected
    sys.set_movement_mode(
        ship_id,
        MovementMode::Warp {
            origin: DVec3::ZERO,
            destination: DVec3::new(10_000_000.0, 0.0, 0.0),
            warp_speed_au_s: 3.0,
            total_distance: 10_000_000.0,
            elapsed_warp_time: 1.0,
            phase: eve_destiny::WarpPhase::Cruising,
        },
    )
    .await
    .unwrap();
    let err_mid = rpc(&ctx, &mut sess, "warpVectorMgr", "UseVector", vec![PyRep::Integer(vec_item_id)])
        .await
        .unwrap_err();
    assert_eq!(err_mid.code, "ShipAlreadyInWarp");

    // 2. Abyssal Deadspace (32_000_001) must reject warpVectorMgr.UseVector
    sys.set_movement_mode(ship_id, MovementMode::Stop).await.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.solar_system_id = 32_000_001;
        }
    });
    sess.solar_system_id = Some(32_000_001);
    let err_abyss = rpc(&ctx, &mut sess, "warpVectorMgr", "UseVector", vec![PyRep::Integer(vec_item_id)])
        .await
        .unwrap_err();
    assert_eq!(err_abyss.code, "CannotWarpInAbyssalSpace");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.solar_system_id = jita_sys;
        }
    });
    sess.solar_system_id = Some(jita_sys);

    // 3. beyonce.CmdDock must reject scrambled/bubbled ships at Upwell structures
    let structure_id: i64 = 1_000_000_999_002;
    let structure_pos = DVec3::new(50_000_000.0, 0.0, 0.0);
    ctx.db.with_mut_snapshot(|snap| {
        let mut st = make_structure(structure_id, 35832, corp_id, jita_sys, "Test Citadel");
        st.position = structure_pos.to_array();
        snap.structures.insert(structure_id, st);
    });
    sys.set_movement_mode(ship_id, MovementMode::Stop).await.unwrap();
    let _ = sys.set_ball_position(ship_id, structure_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
    sys.add_warp_bubble(88_002, structure_pos, 20_000.0).await.unwrap();

    let err_dock = rpc(&ctx, &mut sess, "beyonce", "CmdDock", vec![PyRep::Integer(structure_id)])
        .await
        .unwrap_err();
    assert_eq!(err_dock.code, "ShipWarpScrambled");
}
