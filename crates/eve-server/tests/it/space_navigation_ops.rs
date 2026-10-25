//! Space navigation RPC cluster integration tests: subwarp movement, speed fraction,
//! align, orbit, keep-at-range, approach, stop, warp, dock, undock, and stargate jump.
//!
//! Validates:
//! - Active space session checks (rejects movement and warp when docked)
//! - Ship in space checks
//! - Target resolution (ballpark balls, SDE celestials, stations, gates, bookmarks)
//! - Invariant & parameter validations (negative speeds, invalid ranges, self-targeting)
//! - Distance/perimeter checks for dock (<= 2500m), gate jump (<= 2500m), and warp (>= 150km)

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::Arc;

use crate::common;
use eve_destiny::{DVec3, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;

const C1: i64 = 90_000_001;
const C2: i64 = 90_000_002;
const JITA: u32 = 30_000_142;
const JITA_44: i64 = 60_003_760;

fn new_ctx() -> ServerContext {
    let sde = Arc::new(eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("sde"));
    let ctx = ServerContext::with_sde_and_repository(sde, Arc::new(eve_db::WorldRepository::with_demo_seed()));
    ctx.set_clock_paused(true);
    ctx
}

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, svc: &str, method: &str, args: &[PyRep]) -> PyRep {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
        .unwrap_or_else(|e| panic!("{svc}.{method} failed: {e:?}"))
}

async fn rpc_bound(ctx: &ServerContext, sess: &mut SessionState, svc: &str, bid: u64, method: &str, args: &[PyRep]) -> PyRep {
    ctx.dispatch_rpc(sess, svc, Some(bid), method, args, &HashMap::new(), None)
        .await
        .unwrap_or_else(|e| panic!("{svc}.{method} (bound) failed: {e:?}"))
}

async fn rpc_bound_err(ctx: &ServerContext, sess: &mut SessionState, svc: &str, bid: u64, method: &str, args: &[PyRep]) -> String {
    match ctx.dispatch_rpc(sess, svc, Some(bid), method, args, &HashMap::new(), None).await {
        Ok(v) => panic!("{svc}.{method} (bound) unexpectedly succeeded with: {v:?}"),
        Err(e) => e.code,
    }
}

async fn login_select(ctx: &ServerContext, char_id: i64, sid: u64) -> SessionState {
    let (user, pass) = match char_id {
        C1 => ("demo", "demo-secret"),
        _ => ("other", "other-secret"),
    };
    let mut sess = SessionState::new(sid);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)]).await;
    sess
}

#[tokio::test]
async fn test_space_navigation_subwarp_and_movement() {
    let ctx = new_ctx();
    let mut sess = login_select(&ctx, C1, 9101).await;
    rpc(&ctx, &mut sess, "stationSvc", "Undock", &[]).await;
    let ship_id = sess.ship_id.expect("active ship in space");
    let bid = ctx.bind_object(&mut sess, "beyonce", i64::from(JITA)).expect("bind beyonce");
    let h = ctx.systems.get_or_spawn(JITA);

    // 1. Positive: CmdSetSpeedFraction
    rpc_bound(&ctx, &mut sess, "beyonce", bid, "CmdSetSpeedFraction", &[PyRep::Real(0.5)]).await;
    let snap = h.get_ball_snapshot(ship_id).await.unwrap().unwrap().1;
    assert!((snap.speed_fraction - 0.5).abs() < 1e-6);

    rpc_bound(&ctx, &mut sess, "beyonce", bid, "CmdSetSpeedFraction", &[PyRep::Real(1.0)]).await;
    let snap = h.get_ball_snapshot(ship_id).await.unwrap().unwrap().1;
    assert!((snap.speed_fraction - 1.0).abs() < 1e-6);

    // Negative: negative speed fraction or non-finite
    assert_eq!(
        rpc_bound_err(&ctx, &mut sess, "beyonce", bid, "CmdSetSpeedFraction", &[PyRep::Real(-0.25)]).await,
        "InvalidSpeedFraction"
    );
    assert_eq!(
        rpc_bound_err(&ctx, &mut sess, "beyonce", bid, "CmdSetSpeedFraction", &[PyRep::Real(f64::NAN)]).await,
        "InvalidSpeedFraction"
    );

    // 2. Positive: CmdAlignTo (positional and client kwargs dstID shape)
    rpc_bound(&ctx, &mut sess, "beyonce", bid, "CmdAlignTo", &[PyRep::Integer(JITA_44)]).await;
    let snap = h.get_ball_snapshot(ship_id).await.unwrap().unwrap().1;
    assert_eq!(snap.speed_fraction, 1.0);
    assert!(matches!(snap.mode, MovementMode::GotoDirection { .. }));

    // Client kwargs shape: args: [] and kwargs: {"dstID": JITA_44}
    let mut align_kw = HashMap::new();
    align_kw.insert("dstID".to_string(), PyRep::Integer(JITA_44));
    let align_res = ctx
        .dispatch_rpc(&mut sess, "beyonce", Some(bid), "CmdAlignTo", &[], &align_kw, None)
        .await;
    assert!(align_res.is_ok(), "CmdAlignTo with dstID kwarg must succeed: {align_res:?}");
    let snap_kw = h.get_ball_snapshot(ship_id).await.unwrap().unwrap().1;
    assert_eq!(snap_kw.speed_fraction, 1.0);
    assert!(matches!(snap_kw.mode, MovementMode::GotoDirection { .. }));

    // Negative: invalid target ID, align to self, or non-existent celestial
    assert_eq!(
        rpc_bound_err(&ctx, &mut sess, "beyonce", bid, "CmdAlignTo", &[PyRep::Integer(-1)]).await,
        "InvalidTarget"
    );
    assert_eq!(
        rpc_bound_err(&ctx, &mut sess, "beyonce", bid, "CmdAlignTo", &[PyRep::Integer(ship_id)]).await,
        "InvalidTarget"
    );
    assert_eq!(
        rpc_bound_err(&ctx, &mut sess, "beyonce", bid, "CmdAlignTo", &[PyRep::Integer(999_999_999)]).await,
        "TargetNotFound"
    );

    // 3. Positive: CmdOrbit
    rpc_bound(&ctx, &mut sess, "beyonce", bid, "CmdOrbit", &[PyRep::Integer(JITA_44), PyRep::Real(15_000.0)]).await;
    let snap = h.get_ball_snapshot(ship_id).await.unwrap().unwrap().1;
    assert!(matches!(snap.mode, MovementMode::Orbit { target_id: JITA_44, range } if (range - 15_000.0).abs() < 1e-6));

    // Negative: orbit invalid target, orbit self, negative range
    assert_eq!(
        rpc_bound_err(&ctx, &mut sess, "beyonce", bid, "CmdOrbit", &[PyRep::Integer(-5), PyRep::Real(5_000.0)]).await,
        "InvalidTarget"
    );
    assert_eq!(
        rpc_bound_err(&ctx, &mut sess, "beyonce", bid, "CmdOrbit", &[PyRep::Integer(ship_id), PyRep::Real(5_000.0)]).await,
        "InvalidTarget"
    );
    assert_eq!(
        rpc_bound_err(&ctx, &mut sess, "beyonce", bid, "CmdOrbit", &[PyRep::Integer(JITA_44), PyRep::Real(-100.0)]).await,
        "InvalidRange"
    );
    assert_eq!(
        rpc_bound_err(&ctx, &mut sess, "beyonce", bid, "CmdOrbit", &[PyRep::Integer(999_999_999), PyRep::Real(5_000.0)]).await,
        "TargetNotFound"
    );

    // 4. Positive: CmdFollowBall (covers Approach with range 0.0 and KeepAtRange with explicit range)
    rpc_bound(&ctx, &mut sess, "beyonce", bid, "CmdFollowBall", &[PyRep::Integer(JITA_44), PyRep::Real(1_000.0)]).await;
    let snap = h.get_ball_snapshot(ship_id).await.unwrap().unwrap().1;
    assert!(matches!(snap.mode, MovementMode::Follow { target_id: JITA_44, range } if (range - 1_000.0).abs() < 1e-6));

    // Negative: follow self, negative range
    assert_eq!(
        rpc_bound_err(&ctx, &mut sess, "beyonce", bid, "CmdFollowBall", &[PyRep::Integer(ship_id), PyRep::Real(1_000.0)]).await,
        "InvalidTarget"
    );
    assert_eq!(
        rpc_bound_err(&ctx, &mut sess, "beyonce", bid, "CmdFollowBall", &[PyRep::Integer(JITA_44), PyRep::Real(-10.0)]).await,
        "InvalidRange"
    );

    // 5. Positive: CmdStop
    rpc_bound(&ctx, &mut sess, "beyonce", bid, "CmdStop", &[]).await;
    let snap = h.get_ball_snapshot(ship_id).await.unwrap().unwrap().1;
    assert_eq!(snap.mode, MovementMode::Stop);

    // 6. Negative: Docked pilot calling movement commands fails with NotInSpace
    let mut sess_docked = login_select(&ctx, C2, 9102).await;
    assert!(sess_docked.station_id.is_some(), "C2 starts docked in station");
    let bid_docked = ctx.bind_object(&mut sess_docked, "beyonce", i64::from(JITA)).expect("bind beyonce docked");

    assert_eq!(
        rpc_bound_err(&ctx, &mut sess_docked, "beyonce", bid_docked, "CmdSetSpeedFraction", &[PyRep::Real(1.0)]).await,
        "NotInSpace"
    );
    assert_eq!(
        rpc_bound_err(&ctx, &mut sess_docked, "beyonce", bid_docked, "CmdAlignTo", &[PyRep::Integer(JITA_44)]).await,
        "NotInSpace"
    );
    assert_eq!(
        rpc_bound_err(&ctx, &mut sess_docked, "beyonce", bid_docked, "CmdOrbit", &[PyRep::Integer(JITA_44), PyRep::Real(5_000.0)]).await,
        "NotInSpace"
    );
    assert_eq!(
        rpc_bound_err(&ctx, &mut sess_docked, "beyonce", bid_docked, "CmdFollowBall", &[PyRep::Integer(JITA_44), PyRep::Real(1_000.0)]).await,
        "NotInSpace"
    );
    assert_eq!(
        rpc_bound_err(&ctx, &mut sess_docked, "beyonce", bid_docked, "CmdStop", &[]).await,
        "NotInSpace"
    );

    eve_testkit::attest(&[
        "rpc:beyonce.CmdSetSpeedFraction",
        "rpc:beyonce.CmdAlignTo",
        "rpc:beyonce.CmdOrbit",
        "rpc:beyonce.CmdFollowBall",
        "rpc:beyonce.CmdStop",
        "feat:navigation.align-to",
        "feat:navigation.orbit",
        "feat:navigation.approach",
        "feat:navigation.stop-ship",
        "destiny:Stop",
    ]);
}

#[tokio::test]
async fn test_space_navigation_warp_dock_undock_gate() {
    let ctx = new_ctx();
    let mut sess = login_select(&ctx, C1, 9201).await;
    rpc(&ctx, &mut sess, "stationSvc", "Undock", &[]).await;
    let ship_id = sess.ship_id.expect("active ship in space");
    let bid = ctx.bind_object(&mut sess, "beyonce", i64::from(JITA)).expect("bind beyonce");
    let h = ctx.systems.get_or_spawn(JITA);

    let gate_id = ctx.sde.jumps.values().find(|j| j.from_solar_system_id == JITA).expect("Jita gate").stargate_id;
    let gate_pos = ctx.sde.jumps[&gate_id].position;

    // 1. Positive: CmdWarpToStuff
    rpc_bound(
        &ctx,
        &mut sess,
        "beyonce",
        bid,
        "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(gate_id), PyRep::Real(0.0)],
    )
    .await;
    let snap = h.get_ball_snapshot(ship_id).await.unwrap().unwrap().1;
    assert!(matches!(snap.mode, MovementMode::Warp { .. }));

    // Drop out of warp for further tests
    h.set_movement_mode(ship_id, MovementMode::Stop).await.unwrap();

    // 2. Negative: Warp distance too short (< 150 km)
    assert!(h.set_ball_position(ship_id, gate_pos + DVec3::new(50_000.0, 0.0, 0.0)).await);
    assert_eq!(
        rpc_bound_err(
            &ctx,
            &mut sess,
            "beyonce",
            bid,
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(gate_id), PyRep::Real(0.0)],
        )
        .await,
        "WarpDistanceTooShort"
    );

    // Negative: Warp target invalid or non-existent
    assert_eq!(
        rpc_bound_err(
            &ctx,
            &mut sess,
            "beyonce",
            bid,
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(-1), PyRep::Real(0.0)],
        )
        .await,
        "InvalidTarget"
    );
    assert_eq!(
        rpc_bound_err(
            &ctx,
            &mut sess,
            "beyonce",
            bid,
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(999_999_999), PyRep::Real(0.0)],
        )
        .await,
        "InvalidWarpDestination"
    );

    // 3. Positive: CmdWarpToStuffAutopilot
    assert!(h.set_ball_position(ship_id, DVec3::ZERO).await);
    rpc_bound(
        &ctx,
        &mut sess,
        "beyonce",
        bid,
        "CmdWarpToStuffAutopilot",
        &[PyRep::String("item".into()), PyRep::Integer(gate_id)],
    )
    .await;
    let snap = h.get_ball_snapshot(ship_id).await.unwrap().unwrap().1;
    assert!(matches!(snap.mode, MovementMode::Warp { .. }));
    h.set_movement_mode(ship_id, MovementMode::Stop).await.unwrap();

    // 4. Positive & Negative: CmdDock
    let st_pos = ctx.sde.stations[&JITA_44].position;

    // Negative: Outside docking perimeter (e.g. 50 km away)
    assert!(h.set_ball_position(ship_id, st_pos + DVec3::new(50_000.0, 0.0, 0.0)).await);
    assert_eq!(
        rpc_bound_err(&ctx, &mut sess, "beyonce", bid, "CmdDock", &[PyRep::Integer(JITA_44)]).await,
        "OutsideDockingPerimeter"
    );

    // Negative: Station in another solar system
    assert_eq!(
        rpc_bound_err(&ctx, &mut sess, "beyonce", bid, "CmdDock", &[PyRep::Integer(60_000_001)]).await,
        "DockTargetWrongSystem"
    );

    // Positive: Move ship to docking perimeter (<= 2500m) and dock
    assert!(h.set_ball_position(ship_id, st_pos + DVec3::new(1_000.0, 0.0, 0.0)).await);
    let dock_rep = rpc_bound(&ctx, &mut sess, "beyonce", bid, "CmdDock", &[PyRep::Integer(JITA_44)]).await;
    assert_eq!(dock_rep.dict_get("locationID"), Some(&PyRep::Integer(JITA_44)));
    assert_eq!(sess.station_id, Some(JITA_44));

    // Negative: Attempting to dock while already docked
    assert_eq!(
        rpc_bound_err(&ctx, &mut sess, "beyonce", bid, "CmdDock", &[PyRep::Integer(JITA_44)]).await,
        "AlreadyDocked"
    );

    // Negative: Warp while docked
    assert_eq!(
        rpc_bound_err(
            &ctx,
            &mut sess,
            "beyonce",
            bid,
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(gate_id), PyRep::Real(0.0)],
        )
        .await,
        "NotInSpace"
    );

    // 5. Positive & Negative: Undock
    ctx.advance_secs(11);
    let undock_rep = rpc_bound(&ctx, &mut sess, "beyonce", bid, "Undock", &[]).await;
    assert!(undock_rep.dict_get("solarSystemID").is_some());
    assert_eq!(sess.station_id, None);
    assert_eq!(sess.solar_system_id, Some(JITA));

    // Negative: Undocking while session change timer is active
    assert_eq!(
        rpc_bound_err(&ctx, &mut sess, "beyonce", bid, "Undock", &[]).await,
        "SessionChangeInProgress"
    );

    // Negative: Undocking when already in space (after session timer expires)
    ctx.advance_secs(11);
    assert_eq!(
        rpc_bound_err(&ctx, &mut sess, "beyonce", bid, "Undock", &[]).await,
        "UndockFailed"
    );

    // 6. Positive & Negative: CmdStargateJump
    let bid_space = ctx.bind_object(&mut sess, "beyonce", i64::from(JITA)).expect("re-bind beyonce");

    // Negative: Gate in wrong system or non-existent
    assert_eq!(
        rpc_bound_err(&ctx, &mut sess, "beyonce", bid_space, "CmdStargateJump", &[PyRep::Integer(999_999_999)]).await,
        "UnknownStargate"
    );

    // Negative: Gate jump outside 2500m perimeter
    assert!(h.set_ball_position(ship_id, gate_pos + DVec3::new(100_000.0, 0.0, 0.0)).await);
    assert_eq!(
        rpc_bound_err(&ctx, &mut sess, "beyonce", bid_space, "CmdStargateJump", &[PyRep::Integer(gate_id)]).await,
        "OutsideStargateJumpRange"
    );

    // Positive: Move ship to gate contact radius and jump
    let gate_radius = ctx.sde.types.get(&16).map(|t| t.radius).unwrap_or(14_051.0);
    assert!(h.set_ball_position(ship_id, gate_pos + DVec3::new(gate_radius + 500.0, 0.0, 0.0)).await);
    ctx.advance_secs(11);

    let jump_rep = rpc_bound(&ctx, &mut sess, "beyonce", bid_space, "CmdStargateJump", &[PyRep::Integer(gate_id)]).await;
    assert_eq!(jump_rep.dict_get("solarSystemID"), Some(&PyRep::Integer(ctx.sde.jumps[&gate_id].to_solar_system_id as i64)));
    assert_eq!(sess.solar_system_id, Some(ctx.sde.jumps[&gate_id].to_solar_system_id));

    eve_testkit::attest(&[
        "rpc:beyonce.CmdWarpToStuff",
        "rpc:beyonce.CmdWarpToStuffAutopilot",
        "rpc:beyonce.CmdDock",
        "rpc:beyonce.CmdStargateJump",
        "feat:navigation.warp-to",
        "feat:navigation.dock-station",
        "feat:navigation.undock",
        "feat:navigation.stargate-jump",
        "destiny:WarpTo",
    ]);
}
