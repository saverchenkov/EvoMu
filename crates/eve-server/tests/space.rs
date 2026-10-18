//! Space physics, cyno jumps, scans, and world fixtures integration tests.

#![forbid(unsafe_code)]

use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

#[tokio::test]
async fn test_phase_0_s3_cyno_jump_unimplemented_rpc() {
    // parity: plan:S-3
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(903);
    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[
            PyRep::String("demo".into()),
            PyRep::String("demo-secret".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Login");
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

    let s3_err = ctx
        .dispatch_rpc(
            &mut sess,
            "structureCynoBeaconMgr",
            None,
            "CmdJumpToStructureBeacon",
            &[
                PyRep::Integer(30_002_187),
                PyRep::Integer(1_040_000_000_001),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(s3_err.unwrap_err().code, "UnimplementedRPC");
    assert_eq!(sess.solar_system_id, Some(30_000_142));
    assert_eq!(
        ctx.db
            .get_character(90_000_001)
            .ok()
            .map(|c| (c.solar_system_id, c.station_id)),
        Some((30_000_142, Some(60_003_760)))
    );
}

#[tokio::test]
async fn test_phase_0_s5_no_unconditional_demo_spawns_on_empty_world() {
    // parity: plan:S-5
    use eve_db::{IskCents, WorldRepository};

    let empty_repo = WorldRepository::new_empty();
    empty_repo.with_snapshot(|snap| {
        assert!(snap.market_orders.is_empty());
        assert!(snap.system_states.is_empty());
    });

    let ctx = ServerContext::new();
    assert!(
        ctx.living_world.list_convoys().is_empty(),
        "S-5: Fresh ServerContext must not spawn demo convoys 800_000_001..003 by default"
    );
    ctx.db.with_snapshot(|snap| {
        for (sys_id, st) in &snap.system_states {
            assert_eq!(
                st.ess_main_bank,
                IskCents(0),
                "S-5: System {sys_id} ess_main_bank must start at 0"
            );
            assert_eq!(
                st.ess_reserve_bank,
                IskCents(0),
                "S-5: System {sys_id} ess_reserve_bank must start at 0"
            );
            assert_eq!(
                st.wormhole_destination_system_id, None,
                "S-5: System {sys_id} wormhole destination must be None without fixture seed"
            );
            assert_eq!(
                st.signature_position, None,
                "S-5: System {sys_id} signature_position must be None without fixture seed"
            );
            assert_eq!(
                st.fw_complex_position, None,
                "S-5: System {sys_id} fw_complex_position must be None without fixture seed"
            );
        }
    });
    let sys = ctx.systems.get_or_spawn(30_000_142);
    let balls = sys.list_balls().await.expect("list_balls");
    assert!(
        balls.is_empty(),
        "S-5: Fresh SystemActor ballpark must contain 0 hardcoded NPC balls"
    );
}

#[tokio::test]
async fn test_phase_0_s6_request_scans_unimplemented_rpc() {
    // parity: plan:S-6
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(906);
    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[
            PyRep::String("demo".into()),
            PyRep::String("demo-secret".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Login");
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

    let s6_err = ctx
        .dispatch_rpc(&mut sess, "scanMgr", None, "RequestScans", &[], &kw, None)
        .await;
    assert_eq!(s6_err.unwrap_err().code, "UnimplementedRPC");

    let sys = ctx.systems.get_or_spawn(30_000_142);
    let balls = sys.list_balls().await.expect("list_balls");
    assert!(
        !balls.iter().any(|b| b.id == 750_000_001),
        "S-6: RequestScans must not spawn hardcoded wormhole ball 750_000_001"
    );
}
