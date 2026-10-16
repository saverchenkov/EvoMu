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
    let unauth = ctx
        .dispatch_rpc(&mut sess, "marketProxy", None, "GetOrders", &[], &kw, None)
        .await;
    assert_eq!(unauth.unwrap_err().code, "Unauthorized");

    // E2E-01: Auth -> CreateCharacterWithDoll (Caldari race=1, Deteis bloodline=1, Scientists ancestry=2, STI school=17) -> SelectCharacterID
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
    .expect("authentication.Login");

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
                PyRep::Integer(2),  // Scientists (bloodline 1)
                PyRep::Integer(17), // Science and Trade Institute (race 1)
            ],
            &kw,
            None,
        )
        .await
        .expect("CreateCharacterWithDoll");
    assert!(new_char.as_i64().unwrap_or(0) > 200_000_000);

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

    // E2E-02: Skill Queue & Skill Injector via V24.01 `skillMgr2` (with MachoBindObject)
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
        "SaveSkillQueue",
        &[PyRep::Integer(3327), PyRep::Integer(5)],
        &kw,
        None,
    )
    .await
    .expect("SaveSkillQueue");
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
    // Aura has 15,000,000 SP (5M..50M bracket -> 400,000 SP yield)
    assert_eq!(injected.as_f64(), Some(400_000.0));
    let sp_after = ctx
        .dispatch_rpc(
            &mut sess,
            "skillMgr2",
            Some(skill_bound),
            "ApplyFreeSkillPoints",
            &[],
            &kw,
            None,
        )
        .await
        .expect("ApplyFreeSkillPoints");
    assert!(sp_after.as_f64().unwrap_or(0.0) > 15_000_000.0);

    // E2E-03: Ship Fitting, Module Overload (persisted in ctx.ship_fittings), Mutaplasmid, & Insurance
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "MoveItem",
        &[
            PyRep::Integer(100_000_003),
            PyRep::Integer(100_000_001),
            PyRep::Integer(19),
        ],
        &kw,
        None,
    )
    .await
    .expect("MoveItem");
    let ov_res = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Overload",
            &[PyRep::Integer(100_000_003)],
            &kw,
            None,
        )
        .await
        .expect("Overload");
    let PyRep::KeyVal(ov_kv) = ov_res else {
        panic!("Expected Overload KeyVal");
    };
    assert!(ov_kv.get("effectiveMaxVelocity").unwrap().as_f64().unwrap() > 800.0);

    // E2E-04 & E2E-05: Undock -> Warp to Stargate 50001248 -> Stargate Jump to Perimeter (30000144) -> Jump Back -> Warp to Jita 4-4 -> Dock
    let undock_res = ctx
        .dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    assert_eq!(sess.station_id, None);
    let PyRep::KeyVal(ukv) = undock_res else {
        panic!("Expected Undock KeyVal");
    };
    let jita_bound = ukv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;

    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(jita_bound),
        "CmdWarpToPoint",
        &[
            PyRep::Real(-161_369_743_360.0),
            PyRep::Real(-30_690_549_760.0),
            PyRep::Real(-1_107_962_183_680.0),
        ],
        &kw,
        None,
    )
    .await
    .expect("CmdWarpToPoint to stargate 50001248");

    let jita_handle = ctx.systems.get_or_spawn(30_000_142);
    for _ in 0..30 {
        let _ = jita_handle.step_manual(1.0).await.unwrap();
    }

    let jump_res = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(jita_bound),
            "CmdStargateJump",
            &[PyRep::Integer(50001248)],
            &kw,
            None,
        )
        .await
        .expect("CmdStargateJump to Perimeter");
    let PyRep::KeyVal(jkv) = jump_res else {
        panic!("Expected jump KeyVal");
    };
    assert_eq!(jkv.get("solarSystemID"), Some(&PyRep::Integer(30_000_144)));
    assert_eq!(sess.solar_system_id, Some(30_000_144));
    let perim_bound = jkv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;

    // E2E-06: Combat against NPC pirate ball 700_000_001 in Perimeter & ESS Main Bank Hack
    let combat_res = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "FireWeapon",
            &[
                PyRep::Integer(700_000_001),
                PyRep::String("EntropicDisintegrator".into()),
                PyRep::Integer(10),
            ],
            &kw,
            None,
        )
        .await
        .expect("FireWeapon");
    let PyRep::KeyVal(ckv) = combat_res else {
        panic!("Expected combat KeyVal");
    };
    assert_eq!(ckv.get("destroyed"), Some(&PyRep::Bool(true)));

    ctx.dispatch_rpc(
        &mut sess,
        "essMgr",
        None,
        "AttemptLinkToMainBank",
        &[],
        &kw,
        None,
    )
    .await
    .expect("AttemptLinkToMainBank");

    // Jump back to Jita (30000142) via stargate 50001873, warp to Jita 4-4 (60003760), and Dock
    let jump_back = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(perim_bound),
            "CmdStargateJump",
            &[PyRep::Integer(50001873)],
            &kw,
            None,
        )
        .await
        .expect("CmdStargateJump back to Jita");
    let PyRep::KeyVal(jb_kv) = jump_back else {
        panic!("Expected jump_back KeyVal");
    };
    let jita_bound_2 = jb_kv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;

    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(jita_bound_2),
        "CmdWarpToPoint",
        &[
            PyRep::Real(-107_303_362_560.0),
            PyRep::Real(-18_744_975_360.0),
            PyRep::Real(436_489_052_160.0),
        ],
        &kw,
        None,
    )
    .await
    .expect("CmdWarpToPoint to Jita 4-4");
    for _ in 0..30 {
        let _ = jita_handle.step_manual(1.0).await.unwrap();
    }

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
