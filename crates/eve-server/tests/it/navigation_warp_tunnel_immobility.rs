//! Adversarial integration tests for warp-tunnel navigation immutability, pre-warp alignment
//! cancellation, warp disruption bubble capture integrity, and sub-warp velocity clamping.

#![forbid(unsafe_code)]

use crate::common;
use eve_destiny::{Ball, DestinyEvent, MovementMode, WarpBubble, WarpPhase, AU_METERS};
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

async fn spawn_beacon(
    handle: &eve_server::system_actor::SystemHandle,
    beacon_id: i64,
    pos: DVec3,
) {
    handle
        .spawn_ball(Ball {
            id: beacon_id,
            type_id: 15,
            position: pos,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1.0e12,
            radius: 1_000.0,
            speed_fraction: 0.0,
            mode: MovementMode::Rigid,
        })
        .await
        .expect("spawn rigid beacon");
}

#[tokio::test]
async fn aligning_warp_cancellation_cleans_active_warps_and_restores_massive_state() {
    // parity: feat:navigation.align-to, feat:navigation.stop-ship, feat:navigation.goto-point, destiny:GotoPoint, rpc:beyonce.CmdWarpToStuff, rpc:beyonce.CmdStop, rpc:beyonce.CmdSetSpeedFraction
    eve_testkit::attest(&[
        "feat:navigation.align-to",
        "feat:navigation.stop-ship",
        "feat:navigation.goto-point",
        "destiny:GotoPoint",
    ]);

    let ctx = Arc::new(ServerContext::new());
    common::pin_clock(&ctx);
    let kw = HashMap::new();
    let sys_id = 30_000_142_u32;
    let pilot = common::gm_pilot_in_space(&ctx, 7101, 91_810_001, 587, sys_id).await;
    let mut sess = pilot.sess.clone();
    let ship_id = pilot.ship_id;
    let beyonce_id = pilot.beyonce;
    let handle = ctx.systems.get_or_spawn(sys_id);

    // Spawn an observer ball on the same grid to inspect emitted SetBallMassive events,
    // and a distant beacon 5 AU away along +Y (orthogonal to initial velocity so ship stays Aligning on tick 1).
    let (_, ship_ball0) = handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let observer_id = 88_910_001_i64;
    handle
        .spawn_ball(Ball {
            id: observer_id,
            type_id: 587,
            position: ship_ball0.position + DVec3::new(1_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.5,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await
        .unwrap();
    let dest_beacon_id = 88_910_002_i64;
    let dest_pos = ship_ball0.position + DVec3::new(0.0, 5.0 * AU_METERS, 0.0);
    spawn_beacon(&handle, dest_beacon_id, dest_pos).await;

    // 1. Initiate warp and verify ship enters WarpPhase::Aligning and registers in ctx.active_warps.
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_id),
        "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(dest_beacon_id)],
        &kw,
        None,
    )
    .await
    .expect("CmdWarpToStuff");
    assert!(
        ctx.active_warps.lock().unwrap().contains_key(&ship_id),
        "Active warp registry must track aligning ship"
    );

    // Step 0.5s so the ship is mid-alignment (< 75% max speed) and still in WarpPhase::Aligning.
    handle.step_manual(0.5).await.expect("step 0.5s align");
    let (_, aligning_ball) = handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let v_align = aligning_ball.velocity.length();
    assert!(v_align > 0.0 && v_align < 0.75 * aligning_ball.max_velocity);
    assert!(
        matches!(
            aligning_ball.mode,
            MovementMode::Warp {
                phase: WarpPhase::Aligning,
                ..
            }
        ),
        "Expected WarpPhase::Aligning, got {:?}",
        aligning_ball.mode
    );
    {
        let mut bp = handle.ballpark.write().unwrap();
        let _ = bp.drain_observer_events(observer_id);
    }

    // 2. Cancel pre-warp alignment via CmdStop -> succeeds, removes active_warps entry, restores mass, stays on origin grid.
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_id), "CmdStop", &[], &kw, None)
        .await
        .expect("CmdStop must cancel WarpPhase::Aligning");
    assert!(
        !ctx.active_warps.lock().unwrap().contains_key(&ship_id),
        "Cancelling WarpPhase::Aligning via CmdStop must remove ship from ctx.active_warps"
    );
    let obs_events = {
        let mut bp = handle.ballpark.write().unwrap();
        bp.drain_observer_events(observer_id)
    };
    assert!(
        obs_events.iter().any(|ev| matches!(
            ev,
            DestinyEvent::SetBallMassive {
                ball_id,
                is_massive: true
            } if *ball_id == ship_id
        )),
        "Cancelling Aligning warp must restore SetBallMassive(true)"
    );

    // Step 10s to confirm ship never enters warp after cancellation and stays sub-warp on origin grid.
    for _ in 0..10 {
        handle.step_manual(1.0).await.unwrap();
    }
    let (_, stopped_ball) = handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert_eq!(stopped_ball.mode, MovementMode::Stop);
    assert!(stopped_ball.velocity.length() < v_align * 0.25);
    assert!((stopped_ball.position - ship_ball0.position).length() < 5_000.0);

    // 3. Re-initiate warp and verify CmdGotoPoint and CmdSetSpeedFraction(0.5) also cleanly cancel Aligning warp.
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_id),
        "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(dest_beacon_id)],
        &kw,
        None,
    )
    .await
    .unwrap();
    assert!(ctx.active_warps.lock().unwrap().contains_key(&ship_id));
    {
        let mut bp = handle.ballpark.write().unwrap();
        let _ = bp.drain_observer_events(observer_id);
    }
    let goto_target = DVec3::new(
        ship_ball0.position.x + 10_000.0,
        ship_ball0.position.y,
        ship_ball0.position.z,
    );
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_id),
        "CmdGotoPoint",
        &[
            PyRep::Real(goto_target.x),
            PyRep::Real(goto_target.y),
            PyRep::Real(goto_target.z),
        ],
        &kw,
        None,
    )
    .await
    .expect("CmdGotoPoint must cancel Aligning warp");
    assert!(!ctx.active_warps.lock().unwrap().contains_key(&ship_id));
    let goto_events = {
        let mut bp = handle.ballpark.write().unwrap();
        bp.drain_observer_events(observer_id)
    };
    assert!(
        goto_events.iter().any(|ev| matches!(
            ev,
            DestinyEvent::GotoPoint { ball_id, .. } if *ball_id == ship_id
        )),
        "CmdGotoPoint must emit DestinyEvent::GotoPoint"
    );
    let (_, goto_ball) = handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(matches!(goto_ball.mode, MovementMode::GotoDirection { .. }));

    ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_id), "CmdStop", &[], &kw, None)
        .await
        .unwrap();
    for _ in 0..10 {
        handle.step_manual(1.0).await.unwrap();
    }
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_id),
        "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(dest_beacon_id)],
        &kw,
        None,
    )
    .await
    .unwrap();
    assert!(ctx.active_warps.lock().unwrap().contains_key(&ship_id));
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_id),
        "CmdSetSpeedFraction",
        &[PyRep::Real(0.25)],
        &kw,
        None,
    )
    .await
    .expect("CmdSetSpeedFraction(<0.75) must cancel Aligning warp");
    assert!(!ctx.active_warps.lock().unwrap().contains_key(&ship_id));

    // 4. Verify align formula: align_time = ln(4)*inertia*mass/1e6 enters warp at >= 75% velocity.
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_id), "CmdStop", &[], &kw, None)
        .await
        .unwrap();
    for _ in 0..15 {
        handle.step_manual(1.0).await.unwrap();
    }
    let (_, rest_ball) = handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let align_secs = 4.0_f64.ln() * rest_ball.inertia * rest_ball.mass / 1.0e6;
    let align_ticks = align_secs.ceil().max(1.0) as usize;
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_id),
        "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(dest_beacon_id)],
        &kw,
        None,
    )
    .await
    .unwrap();
    for _ in 0..align_ticks {
        handle.step_manual(1.0).await.unwrap();
    }
    let (_, aligned_ball) = handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(
        aligned_ball.velocity.length() >= 0.75 * aligned_ball.max_velocity - 0.5,
        "Ship must reach >= 75% max velocity by ceil(ln(4)*inertia*mass/1e6)={align_ticks}s"
    );
    handle.step_manual(1.0).await.unwrap();
    let (_, warped_ball) = handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(
        matches!(
            warped_ball.mode,
            MovementMode::Warp {
                phase: WarpPhase::Accelerating | WarpPhase::Cruising,
                ..
            }
        ),
        "Ship must enter warp after reaching 75% velocity, got {:?}",
        warped_ball.mode
    );
}

#[tokio::test]
async fn active_warp_tunnel_rejects_all_subwarp_rpcs_and_prevents_slingshot_past_bubble() {
    // parity: feat:modules.interdiction-sphere, feat:navigation.goto-point, destiny:GotoPoint, rpc:beyonce.CmdWarpToStuff, rpc:beyonce.CmdStop, rpc:beyonce.CmdGotoDirection, rpc:beyonce.CmdSteerDirection, rpc:beyonce.CmdSetSpeedFraction, rpc:beyonce.CmdOrbit, rpc:beyonce.CmdFollowBall, rpc:beyonce.CmdAlignTo
    eve_testkit::attest(&[
        "feat:modules.interdiction-sphere",
        "feat:navigation.goto-point",
        "destiny:GotoPoint",
    ]);

    let ctx = Arc::new(ServerContext::new());
    common::pin_clock(&ctx);
    let kw = HashMap::new();
    // Use null-sec system (FD-MLJ: 30_003_307) with a warp disruption bubble on the destination beacon.
    let sys_id = 30_003_307_u32;
    let pilot = common::gm_pilot_in_space(&ctx, 7201, 91_810_002, 587, sys_id).await;
    let mut sess = pilot.sess.clone();
    let ship_id = pilot.ship_id;
    let beyonce_id = pilot.beyonce;
    let handle = ctx.systems.get_or_spawn(sys_id);

    let origin = DVec3::ZERO;
    handle.set_ball_position(ship_id, origin).await;

    let origin_target_id = 88_920_001_i64;
    let dest_gate_id = 88_920_002_i64;
    let bubble_id = 88_920_003_i64;
    let gate_pos = DVec3::new(15.0 * AU_METERS, 0.0, 0.0);
    let bubble_radius = 20_000.0;
    spawn_beacon(&handle, origin_target_id, DVec3::new(2_500.0, 0.0, 0.0)).await;
    spawn_beacon(&handle, dest_gate_id, gate_pos).await;

    // Anchor a 20 km warp disruption bubble centered on the destination gate so an un-nullified
    // warp MUST be caught at gate_pos.x - 20_000 m and never bypass it via mid-warp steering.
    {
        let mut bp = handle.ballpark.write().unwrap();
        bp.warp_bubble_mgr
            .add_bubble(WarpBubble::new(bubble_id, gate_pos, bubble_radius));
    }

    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_id),
        "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(dest_gate_id)],
        &kw,
        None,
    )
    .await
    .expect("CmdWarpToStuff toward bubbled gate");

    // Step until ship enters WarpPhase::Accelerating, then test rejection; step further into WarpPhase::Cruising at multi-AU/s!
    let mut saw_accelerating = false;
    let mut saw_cruising = false;
    for _ in 0..40 {
        handle.step_manual(0.5).await.unwrap();
        let (_, b) = handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
        let MovementMode::Warp { phase, .. } = b.mode else {
            break;
        };
        if phase == WarpPhase::Accelerating && !saw_accelerating {
            saw_accelerating = true;
            let stop_err = ctx
                .dispatch_rpc(&mut sess, "beyonce", Some(beyonce_id), "CmdStop", &[], &kw, None)
                .await
                .expect_err("CmdStop must fail in WarpPhase::Accelerating");
            assert_eq!(stop_err.code, "ShipAlreadyInWarp");
        }
        if phase == WarpPhase::Cruising && b.velocity.length() > 1.0 * AU_METERS {
            saw_cruising = true;
            break;
        }
    }
    assert!(saw_cruising, "Ship must reach multi-AU/s WarpPhase::Cruising");

    // Issue EVERY sub-warp navigation RPC while cruising at multi-AU/s -> ALL must reject with ShipAlreadyInWarp!
    let expected_code = "ShipAlreadyInWarp";
    for (rpc_name, res) in [
        (
            "CmdStop",
            ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_id), "CmdStop", &[], &kw, None).await,
        ),
        (
            "CmdGotoDirection",
            ctx.dispatch_rpc(
                &mut sess,
                "beyonce",
                Some(beyonce_id),
                "CmdGotoDirection",
                &[PyRep::Real(0.0), PyRep::Real(1.0), PyRep::Real(0.0)],
                &kw,
                None,
            )
            .await,
        ),
        (
            "CmdSteerDirection",
            ctx.dispatch_rpc(
                &mut sess,
                "beyonce",
                Some(beyonce_id),
                "CmdSteerDirection",
                &[PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(1.0)],
                &kw,
                None,
            )
            .await,
        ),
        (
            "CmdGotoPoint",
            ctx.dispatch_rpc(
                &mut sess,
                "beyonce",
                Some(beyonce_id),
                "CmdGotoPoint",
                &[PyRep::Real(0.0), PyRep::Real(1.0e8), PyRep::Real(0.0)],
                &kw,
                None,
            )
            .await,
        ),
        (
            "CmdSetSpeedFraction(0.1)",
            ctx.dispatch_rpc(
                &mut sess,
                "beyonce",
                Some(beyonce_id),
                "CmdSetSpeedFraction",
                &[PyRep::Real(0.1)],
                &kw,
                None,
            )
            .await,
        ),
        (
            "CmdSetSpeedFraction(0.0)",
            ctx.dispatch_rpc(
                &mut sess,
                "beyonce",
                Some(beyonce_id),
                "CmdSetSpeedFraction",
                &[PyRep::Real(0.0)],
                &kw,
                None,
            )
            .await,
        ),
        (
            "CmdOrbit",
            ctx.dispatch_rpc(
                &mut sess,
                "beyonce",
                Some(beyonce_id),
                "CmdOrbit",
                &[PyRep::Integer(origin_target_id), PyRep::Real(2_500.0)],
                &kw,
                None,
            )
            .await,
        ),
        (
            "CmdFollowBall",
            ctx.dispatch_rpc(
                &mut sess,
                "beyonce",
                Some(beyonce_id),
                "CmdFollowBall",
                &[PyRep::Integer(origin_target_id), PyRep::Real(1_000.0)],
                &kw,
                None,
            )
            .await,
        ),
        (
            "CmdAlignTo",
            ctx.dispatch_rpc(
                &mut sess,
                "beyonce",
                Some(beyonce_id),
                "CmdAlignTo",
                &[PyRep::Integer(origin_target_id)],
                &kw,
                None,
            )
            .await,
        ),
    ] {
        let err = res.expect_err(&format!("{rpc_name} must fail mid-warp"));
        assert_eq!(err.code, expected_code, "{rpc_name} got {err:?}");
    }

    // Verify ship is STILL in MovementMode::Warp (never dropped into sub-warp at multi-AU/s)
    // and step until warp completes at the bubble edge!
    let (_, still_warping) = handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(
        matches!(still_warping.mode, MovementMode::Warp { .. }),
        "Ship must remain in MovementMode::Warp after rejected mid-warp steering RPCs"
    );

    for _ in 0..80 {
        handle.step_manual(1.0).await.unwrap();
        let (_, b) = handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
        if b.mode == MovementMode::Stop {
            break;
        }
    }

    let (_, landed) = handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert_eq!(landed.mode, MovementMode::Stop, "Ship must complete warp to Stop");
    assert!(
        landed.velocity.length() <= landed.max_velocity + 1e-6,
        "Landed ship velocity ({} m/s) must never exceed sub-warp max_velocity ({} m/s)",
        landed.velocity.length(),
        landed.max_velocity
    );
    let expected_catch_x = gate_pos.x - bubble_radius;
    assert!(
        (landed.position.x - expected_catch_x).abs() < 2_500.0 && landed.position.y.abs() < 100.0,
        "Ship must land caught at destination bubble edge ({expected_catch_x}), got {:?}",
        landed.position
    );
}
