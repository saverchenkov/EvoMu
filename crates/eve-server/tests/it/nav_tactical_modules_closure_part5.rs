//! Negative integration tests for `R-INT-01` (Interdiction Sphere Launcher activation
//! rejections when docked, not in space, cloaked, tethered, or in active warp) and `R-MJD-02`
//! (`warpVectorMgr.UseVector` rejection during MJD spool + `Ballpark::micro_jump` /
//! `Ballpark::micro_jump_field` rejection during active warp).

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
    let mod_inst = fit.modules.get(&launcher_id).unwrap();
    let mod_type = ctx.sde.types.get(&22_782).unwrap();
    let err_no_ball = eve_server::services::tactical_module_mechanics::activate_interdiction_sphere_launcher(
        &ctx,
        &sys,
        jita_sys,
        char_id,
        ship_id,
        22_456,
        launcher_id,
        mod_inst,
        mod_type,
        "online",
        None,
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
