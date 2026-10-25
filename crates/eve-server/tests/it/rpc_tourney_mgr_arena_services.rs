//! Integration tests for tourneyMgr tournament arena setup, pairings, and teleportation.

use crate::common;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;

async fn select_pilot(
    ctx: &ServerContext,
    sid: u64,
    user: &str,
    pass: &str,
    char_id: i64,
) -> SessionState {
    let mut s = SessionState::new(sid);
    common::login_pilot(ctx, &mut s, user, pass).await;
    ctx.dispatch_rpc(
        &mut s,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");
    s
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

#[tokio::test]
async fn test_tourney_setup_pairings_and_teleportation() {
    // parity: rpc:tourneyMgr.GetPotentialMatches
    // parity: rpc:tourneyMgr.GetTourneySetup
    // parity: rpc:tourneyMgr.TeleportToArena
    // parity: rpc:tourneyMgr.TeleportToStation
    eve_testkit::attest(&[
        "rpc:tourneyMgr.GetPotentialMatches",
        "rpc:tourneyMgr.GetTourneySetup",
        "rpc:tourneyMgr.TeleportToArena",
        "rpc:tourneyMgr.TeleportToStation",
    ]);

    let ctx = ServerContext::new();
    let mut sess = select_pilot(&ctx, 80_101, "demo", "demo-secret", P1).await;
    sess.role = eve_machonet::service_roles::ROLE_GMH;

    // 1. GetTourneySetup: returns 3-tuple (tourneys, systems, rules)
    let setup_res = rpc(&ctx, &mut sess, "tourneyMgr", "GetTourneySetup", &[])
        .await
        .expect("GetTourneySetup");
    let PyRep::Tuple(parts) = setup_res else {
        panic!("GetTourneySetup must return 3-tuple, got {setup_res:?}");
    };
    assert_eq!(parts.len(), 3, "Setup tuple must have 3 elements");
    let PyRep::Dict(tourneys) = &parts[0] else {
        panic!("Element 0 must be Dict of tournaments");
    };
    assert!(!tourneys.is_empty(), "Tournaments dict should not be empty");
    let PyRep::Dict(systems) = &parts[1] else {
        panic!("Element 1 must be Dict of systems");
    };
    assert!(!systems.is_empty(), "Systems dict should not be empty");
    for (sys_k, sys_v) in systems {
        assert!(matches!(sys_k, PyRep::Integer(_)), "System key must be systemID integer");
        let PyRep::Tuple(sys_pair) = sys_v else {
            panic!("System value must be 2-tuple (systemName, systemObj), got {sys_v:?}");
        };
        assert_eq!(sys_pair.len(), 2, "System value tuple must have 2 elements");
        assert!(matches!(&sys_pair[0], PyRep::String(_)), "systemName must be String");
        assert!(matches!(&sys_pair[1], PyRep::Dict(_)), "systemObj must be Dict");
    }
    let PyRep::List(rules) = &parts[2] else {
        panic!("Element 2 must be List of rulesets");
    };
    assert!(!rules.is_empty(), "Rules list should not be empty");

    // 2. GetPotentialMatches before creating match: returns List
    let matches_res = rpc(&ctx, &mut sess, "tourneyMgr", "GetPotentialMatches", &[PyRep::Integer(1)])
        .await
        .expect("GetPotentialMatches");
    let PyRep::List(match_list) = &matches_res else {
        panic!("GetPotentialMatches must return List, got {matches_res:?}");
    };
    let initial_count = match_list.len();

    // Create a match
    let match_id = 7001;
    rpc(
        &ctx,
        &mut sess,
        "tourneyMgr",
        "CreateMatch",
        &[
            PyRep::Integer(1), // tourneyID
            PyRep::Tuple(vec![
                PyRep::Integer(match_id),
                PyRep::Integer(98_000_001),
                PyRep::Integer(98_000_002),
                PyRep::Integer(P1),
                PyRep::Integer(P2),
            ]),
            PyRep::Integer(100),
            PyRep::Integer(600),
            PyRep::Integer(0),
            PyRep::Integer(0),
        ],
    )
    .await
    .expect("CreateMatch");

    // 3. GetPotentialMatches after creating match: includes newly created match
    let matches_after = rpc(&ctx, &mut sess, "tourneyMgr", "GetPotentialMatches", &[PyRep::Integer(1)])
        .await
        .expect("GetPotentialMatches after create");
    let PyRep::List(match_list_after) = matches_after else {
        panic!("GetPotentialMatches must return List");
    };
    assert_eq!(match_list_after.len(), initial_count + 1);
    let found = match_list_after.iter().any(|m| match m {
        PyRep::Tuple(fields) => fields.first().and_then(|f| f.as_i64()) == Some(match_id),
        _ => false,
    });
    assert!(found, "Created match must be found in potential matches list");

    // 4. TeleportToArena: teleports referee/GM into arena space
    let bound_oid = 99_001;
    ctx.bound_objects.write().unwrap().insert(
        bound_oid,
        eve_server::router::BoundObjectEntry {
            bound_id: bound_oid,
            session_id: sess.session_id,
            user_id: sess.user_id.unwrap_or(0),
            character_id: P1,
            service: "beyonce".to_string(),
            object_id: 30_000_144,
        },
    );

    let p1_ship = ctx.db.with_snapshot(|s| s.characters.get(&P1).unwrap().active_ship_id);
    let old_sys = 30_000_144u32;
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&P1) {
            ch.solar_system_id = old_sys;
            ch.station_id = None;
        }
    });
    sess.solar_system_id = Some(old_sys);
    sess.station_id = None;
    let old_sys_handle = ctx.systems.get_or_spawn(old_sys);
    old_sys_handle
        .spawn_ball(eve_destiny::Ball {
            id: p1_ship,
            type_id: 606,
            position: glam::DVec3::ZERO,
            velocity: glam::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 2.5,
            mass: 1_200_000.0,
            radius: 80.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await
        .unwrap();
    assert!(old_sys_handle.get_ball_snapshot(p1_ship).await.unwrap().is_some());

    let tele_res = rpc(&ctx, &mut sess, "tourneyMgr", "TeleportToArena", &[PyRep::Integer(match_id)])
        .await
        .expect("TeleportToArena");
    assert_eq!(tele_res, PyRep::Bool(true));
    assert!(sess.station_id.is_none(), "Station ID should be None in space");
    assert_eq!(sess.solar_system_id, Some(30_000_142));
    assert!(old_sys_handle.get_ball_snapshot(p1_ship).await.unwrap().is_none(), "Ship ball should be removed from old_sys");
    assert_eq!(
        ctx.bound_objects.read().unwrap().get(&bound_oid).unwrap().object_id,
        30_000_142,
        "beyonce bound object_id must be updated to arena_sys"
    );

    // 5. TeleportToStation: returns referee/GM to home station
    let _ = ctx.drain_notifications_for_char(P1);
    let tele_home_res = rpc(&ctx, &mut sess, "tourneyMgr", "TeleportToStation", &[PyRep::Integer(match_id)])
        .await
        .expect("TeleportToStation");
    assert_eq!(tele_home_res, PyRep::Bool(true));
    assert!(sess.station_id.is_some(), "Character should be docked in station");
    let home_sys = sess.solar_system_id.unwrap() as i64;
    assert_eq!(
        ctx.bound_objects.read().unwrap().get(&bound_oid).unwrap().object_id,
        home_sys,
        "beyonce bound object_id must be updated to home_sys"
    );
    let notifs = ctx.drain_notifications_for_char(P1);
    let session_change = notifs.iter().find(|n| n.event == "OnSessionChanged").expect("OnSessionChanged emitted");
    assert_eq!(session_change.args.len(), 3, "OnSessionChanged must have 3 args");
    let sess_rep = &session_change.args[1];
    assert_eq!(sess_rep.dict_get("solarsystemid"), Some(&PyRep::None), "Docked session solarsystemid must be None");
    assert_eq!(sess_rep.dict_get("solarsystemid2"), Some(&PyRep::Integer(home_sys)), "Docked session solarsystemid2 must be home_sys");
}
