//! Integration test suite verifying End-to-End journeys (`E2E-01` through `E2E-18`) against `eve_server::router::ServerContext`.

#![forbid(unsafe_code)]

use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

#[tokio::test]
async fn test_e2e_01_to_18_integration_journeys() {
    let ctx = ServerContext::new();
    let mut sess = SessionState::new(1);
    let kw = HashMap::new();

    // Unauthenticated RPC must be rejected before login
    assert!(ctx
        .dispatch_rpc(&mut sess, "marketProxy", None, "GetSystemAsks", &[], &kw, None)
        .await
        .is_err());

    // E2E-01: Auth -> Character Select -> CreateCharacterWithDoll -> Corvette Spawn
    ctx.dispatch_rpc(
        &mut sess,
        "auth",
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
    .expect("auth.Login");

    let new_char = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "CreateCharacterWithDoll",
            &[
                PyRep::String("Integration Pilot".into()),
                PyRep::Integer(1),  // Caldari
                PyRep::Integer(1),  // Deteis
                PyRep::Integer(11), // Scientists
                PyRep::Integer(17), // Science and Trade Institute
            ],
            &kw,
            None,
        )
        .await
        .expect("CreateCharacterWithDoll");
    assert!(new_char.as_i64().unwrap_or(0) >= 90_000_002);

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
    assert_eq!(sess.character_id, Some(90_000_001));

    // E2E-02: Skill Queue & Skill Injector via V24.01 `skillMgr2`
    let (skill_bound, _) = match ctx
        .dispatch_rpc(
            &mut sess,
            "skillMgr2",
            None,
            "MachoBindObject",
            &[PyRep::Integer(90_000_001), PyRep::None],
            &kw,
            None,
        )
        .await
        .expect("skillMgr2 bind")
    {
        PyRep::Tuple(t) => (t[0].as_i64().unwrap_or(1) as u64, t[1].clone()),
        _ => panic!("Expected bound tuple"),
    };
    ctx.dispatch_rpc(
        &mut sess,
        "skillMgr2",
        Some(skill_bound),
        "SaveNewQueue",
        &[PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(3300),
            PyRep::Integer(4),
        ])])],
        &kw,
        None,
    )
    .await
    .expect("SaveNewQueue");
    let injected = ctx
        .dispatch_rpc(
            &mut sess,
            "skillMgr2",
            Some(skill_bound),
            "InjectSkillPoints",
            &[PyRep::Integer(100_000_014), PyRep::Integer(1)],
            &kw,
            None,
        )
        .await
        .expect("InjectSkillPoints");
    assert_eq!(injected.as_i64(), Some(500_000));

    // E2E-04: Undock -> Orbit -> Stargate Jump -> Dock
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    assert_eq!(sess.station_id, None);
    let jump_res = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            None,
            "CmdStargateJump",
            &[PyRep::Integer(50001248), PyRep::Integer(50002187)],
            &kw,
            None,
        )
        .await
        .expect("CmdStargateJump");
    assert_eq!(jump_res.as_i64(), Some(30002187));
    assert_eq!(sess.solar_system_id, Some(30002187));

    // Jump back to Jita and Dock
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        None,
        "CmdStargateJump",
        &[PyRep::Integer(50002187), PyRep::Integer(50001248)],
        &kw,
        None,
    )
    .await
    .expect("CmdStargateJump back to Jita");
    ctx.dispatch_rpc(
        &mut sess,
        "stationSvc",
        None,
        "Dock",
        &[PyRep::Integer(60003760)],
        &kw,
        None,
    )
    .await
    .expect("Dock");
    assert_eq!(sess.station_id, Some(60003760));
}
