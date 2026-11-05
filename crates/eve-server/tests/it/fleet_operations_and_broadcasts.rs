//! End-to-end integration tests for Fleet parity features:
//! - `feat:fleet.boss-handover`
//! - `feat:fleet.stored-formations-and-warp-shapes`
//! - `feat:fleet.target-tagging`

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::common;
use eve_destiny::{DVec3, TargetLock};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;

const PILOT_1: i64 = 90_000_001;
const PILOT_2: i64 = 90_000_002;
const JITA_SYS: u32 = 30_000_142;

async fn select_pilot(
    ctx: &ServerContext,
    session_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");
    ctx.advance_secs(12);
    sess
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
        .await
}

async fn form_fleet_with_two(ctx: &ServerContext, s1: &mut SessionState, s2: &mut SessionState, p2: i64) -> i64 {
    let init = rpc(ctx, s1, "fleetObjectHandler", "CreateFleet", &[]).await.expect("CreateFleet");
    let fleet_id = init.as_i64().expect("fleetID");
    s1.fleet_id = Some(fleet_id);
    rpc(ctx, s1, "fleetObjectHandler", "Invite", &[PyRep::Integer(p2), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(4)])
        .await
        .expect("Invite p2");
    rpc(ctx, s2, "fleetObjectHandler", "AcceptInvite", &[PyRep::Integer(fleet_id), PyRep::Integer(587)])
        .await
        .expect("AcceptInvite p2");
    s2.fleet_id = Some(fleet_id);
    fleet_id
}

#[tokio::test]
async fn test_fleet_boss_handover_on_leave_and_disconnect() {
    // parity: feat:fleet.boss-handover
    let ctx = ServerContext::new();
    let p3: i64 = 90_000_003;
    ctx.db.provision_gm_account("pilot3", "pilot3-secret", p3, "PilotThree");
    let mut s1 = select_pilot(&ctx, 1, "demo", "demo-secret", PILOT_1).await;
    let mut s2 = select_pilot(&ctx, 2, "other", "other-secret", PILOT_2).await;
    let mut s3 = select_pilot(&ctx, 3, "pilot3", "pilot3-secret", p3).await;

    let fleet_id = form_fleet_with_two(&ctx, &mut s1, &mut s2, PILOT_2).await;
    rpc(&ctx, &mut s1, "fleetObjectHandler", "Invite", &[PyRep::Integer(p3), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(4)])
        .await
        .expect("Invite p3");
    rpc(&ctx, &mut s3, "fleetObjectHandler", "AcceptInvite", &[PyRep::Integer(fleet_id), PyRep::Integer(587)])
        .await
        .expect("AcceptInvite p3");
    s3.fleet_id = Some(fleet_id);

    ctx.drain_notifications_for_char(PILOT_2);
    ctx.drain_notifications_for_char(p3);

    // 1. Boss (PILOT_1) leaves fleet -> boss passes automatically to PILOT_2 and all members are notified
    rpc(&ctx, &mut s1, "fleetObjectHandler", "LeaveFleet", &[]).await.expect("Boss LeaveFleet");
    assert_eq!(ctx.db.with_snapshot(|snap| snap.fleets.get(&fleet_id).map(|f| f.boss_id)), Some(PILOT_2));

    let n2 = ctx.drain_notifications_for_char(PILOT_2);
    let n3 = ctx.drain_notifications_for_char(p3);
    assert!(n2.iter().any(|n| n.event == "OnFleetLeave"));
    assert!(n2.iter().any(|n| n.event == "OnFleetMemberChanged"));
    assert!(n3.iter().any(|n| n.event == "OnFleetLeave"));
    assert!(n3.iter().any(|n| n.event == "OnFleetMemberChanged"));

    // 2. New boss (PILOT_2) disconnects -> after grace expiry, boss passes to p3 and p3 is notified
    eve_server::services::fleet_social::fleet_social_notify::handle_fleet_member_disconnect(&ctx, &mut s2);
    ctx.advance_virtual_clock(125);
    eve_server::services::fleet_social::fleet_social_notify::tick_fleet_lifecycle(&ctx);

    assert_eq!(ctx.db.with_snapshot(|snap| snap.fleets.get(&fleet_id).map(|f| f.boss_id)), Some(p3));
    let n3_dc = ctx.drain_notifications_for_char(p3);
    assert!(n3_dc.iter().any(|n| n.event == "OnFleetLeave"));
    assert!(n3_dc.iter().any(|n| n.event == "OnFleetMemberChanged"));
}

#[tokio::test]
async fn test_fleet_stored_formations_and_warp_shapes() {
    // parity: feat:fleet.stored-formations-and-warp-shapes
    let ctx = ServerContext::new();
    let mut s1 = select_pilot(&ctx, 11, "demo", "demo-secret", PILOT_1).await;
    let mut s2 = select_pilot(&ctx, 12, "other", "other-secret", PILOT_2).await;

    let sys = ctx.systems.get_or_spawn(JITA_SYS);
    let ship1 = 200_880_001_i64;
    let ship2 = 200_880_002_i64;
    for (sid, x) in [(ship1, 10_000.0), (ship2, 20_000.0)] {
        let _ = sys.spawn_ball(eve_destiny::Ball {
            id: sid,
            type_id: 587,
            position: DVec3::new(x, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 2.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        }).await;
    }
    s1.ship_id = Some(ship1);
    s2.ship_id = Some(ship2);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&PILOT_1) {
            c.active_ship_id = ship1;
            c.station_id = None;
        }
        if let Some(c) = snap.characters.get_mut(&PILOT_2) {
            c.active_ship_id = ship2;
            c.station_id = None;
        }
    });

    form_fleet_with_two(&ctx, &mut s1, &mut s2, PILOT_2).await;
    rpc(&ctx, &mut s2, "fleetObjectHandler", "SetTakesFleetWarp", &[PyRep::Bool(true)])
        .await
        .expect("SetTakesFleetWarp");

    let set_res = rpc(
        &ctx,
        &mut s1,
        "fleetObjectHandler",
        "SetFleetFormationSettings",
        &[PyRep::Integer(3), PyRep::Real(5_000.0), PyRep::Real(10_000.0)],
    )
    .await
    .expect("SetFleetFormationSettings");
    assert_eq!(set_res.dict_get("formationName"), Some(&PyRep::String("Arrow".into())));
    let get_res = rpc(&ctx, &mut s1, "fleetObjectHandler", "GetFormationSettings", &[]).await.expect("GetFormationSettings");
    assert_eq!(get_res.dict_get("formationID"), Some(&PyRep::Integer(3)));

    let dest = DVec3::new(1_000_000_000.0, 0.0, 0.0);
    rpc(
        &ctx,
        &mut s1,
        "fleetObjectHandler",
        "FleetWarp",
        &[PyRep::Real(dest.x), PyRep::Real(dest.y), PyRep::Real(dest.z), PyRep::Real(0.0)],
    )
    .await
    .expect("FleetWarp");

    for _ in 0..100 {
        let _ = sys.step_manual(1.0).await;
    }
    let (_, b1) = sys.get_ball_snapshot(ship1).await.unwrap().unwrap();
    let (_, b2) = sys.get_ball_snapshot(ship2).await.unwrap().unwrap();
    let sep = b1.position.distance(b2.position);
    assert!(
        (sep - 5_000.0 * 2.0_f64.sqrt()).abs() < 250.0,
        "Arrow formation must land members at deterministic 5km*sqrt(2) offset, got sep={sep}"
    );
}

#[tokio::test]
async fn test_fleet_target_tagging_validation_lock_and_destruction() {
    // parity: feat:fleet.target-tagging
    let ctx = ServerContext::new();
    let mut s1 = select_pilot(&ctx, 21, "demo", "demo-secret", PILOT_1).await;
    let mut s2 = select_pilot(&ctx, 22, "other", "other-secret", PILOT_2).await;

    rpc(&ctx, &mut s1, "stationSvc", "Undock", &[]).await.expect("Undock s1");
    rpc(&ctx, &mut s2, "stationSvc", "Undock", &[]).await.expect("Undock s2");
    let ship1 = s1.ship_id.expect("ship1");
    let ship2 = s2.ship_id.expect("ship2");

    form_fleet_with_two(&ctx, &mut s1, &mut s2, PILOT_2).await;

    let err_unlocked = rpc(&ctx, &mut s1, "beyonce", "CmdFleetTagTarget", &[PyRep::Integer(ship2), PyRep::String("A".into())])
        .await
        .expect_err("Unlocked target must fail");
    assert_eq!(err_unlocked.code, "TargetNotLocked");

    let err_invalid = rpc(&ctx, &mut s1, "beyonce", "CmdFleetTagTarget", &[PyRep::Integer(ship2), PyRep::String("AA".into())])
        .await
        .expect_err("Invalid tag must fail");
    assert_eq!(err_invalid.code, "InvalidTargetTag");

    let sys = ctx.systems.get_or_spawn(JITA_SYS);
    sys.add_target_lock(
        ship1,
        TargetLock { target_id: ship2, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) },
    )
    .await
    .unwrap();
    ctx.drain_notifications_for_char(PILOT_2);

    let tag_res = rpc(&ctx, &mut s1, "beyonce", "CmdFleetTagTarget", &[PyRep::Integer(ship2), PyRep::String("A".into())])
        .await
        .expect("CmdFleetTagTarget A");
    assert_eq!(tag_res, PyRep::None);
    assert!(ctx.drain_notifications_for_char(PILOT_2).iter().any(|n| n.event == "OnFleetStateChange"));

    eve_server::services::fleet_social::fleet_social_finder_tactical::clear_destroyed_target_tag(&ctx, ship2);
    let fs_after = rpc(&ctx, &mut s1, "beyonce", "GetFleetState", &[]).await.expect("GetFleetState");
    assert_eq!(fs_after.dict_get("targetTags"), Some(&PyRep::Dict(Vec::new())));
}

