//! Combat, fighters, and weapon systems integration tests.

#![forbid(unsafe_code)]

use crate::common;

use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

#[tokio::test]
async fn test_phase_0_s2_fighter_mgr_unimplemented_rpc() {
    // parity: plan:S-2, rpc:charUnboundMgr.SelectCharacterID, rpc:fighterMgr.CmdActivateAbilitySlots
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(902);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");

    let sys = ctx.systems.get_or_spawn(30_000_142);
    let (_, ball_before) = sys
        .get_ball_snapshot(100_000_001)
        .await
        .expect("get_ball_snapshot")
        .expect("ship ball 100_000_001");
    assert_eq!(ball_before.id, 100_000_001);

    let s2_err = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "CmdActivateAbilitySlots",
            &[PyRep::Integer(100_000_001), PyRep::Integer(0)],
            &kw,
            None,
        )
        .await;
    assert!(matches!(
        s2_err.unwrap_err().code.as_str(),
        "UnimplementedRPC" | "FighterNotFound"
    ));

    let (_, ball_after) = sys
        .get_ball_snapshot(100_000_001)
        .await
        .expect("get_ball_snapshot")
        .expect("ship ball 100_000_001 must remain intact in ballpark");
    assert_eq!(ball_after.id, 100_000_001);
}
