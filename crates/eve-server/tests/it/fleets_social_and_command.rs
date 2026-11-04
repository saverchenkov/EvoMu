//! Fleet social and command systems integration tests:
//! - Fleet boss handover (manual handover and automatic reassignment on disconnect or leave).
//! - Stored custom formations and fleet warp spatial landing offsets.
//! - Target tagging by commander/leaders, broadcast updates, and automatic tag destruction cleanup.
//!
//! Parity rows:
//! - `feat:fleet.boss-handover`
//! - `feat:fleet.stored-formations-and-warp-shapes`
//! - `feat:fleet.target-tagging`

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::common;
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;

const PILOT_1: i64 = 90_000_001;
const PILOT_2: i64 = 90_000_002;
const PILOT_3: i64 = 90_000_003;

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

fn field(rep: &PyRep, key: &str) -> PyRep {
    match rep {
        PyRep::KeyVal(map) => map.get(key).cloned().unwrap_or(PyRep::None),
        _ => PyRep::None,
    }
}

#[tokio::test]
async fn test_fleet_boss_manual_and_automatic_handover() {
    // parity: feat:fleet.boss-handover
    eve_testkit::attest(&["feat:fleet.boss-handover"]);

    let ctx = common::test_context().await;
    let mut sess1 = select_pilot(&ctx, 60_001, "testpilot1", "testpass1", PILOT_1).await;
    let mut sess2 = select_pilot(&ctx, 60_002, "testpilot2", "testpass2", PILOT_2).await;
    let mut sess3 = select_pilot(&ctx, 60_003, "testpilot3", "testpass3", PILOT_3).await;

    // Pilot 1 creates a fleet
    let fleet_rep = rpc(&ctx, &mut sess1, "fleetObjectHandler", "CreateFleet", &[])
        .await
        .expect("CreateFleet");
    let fleet_id = match fleet_rep {
        PyRep::Integer(id) => id,
        other => match field(&other, "fleetID") {
            PyRep::Integer(id) => id,
            _ => panic!("Expected fleetID, got {other:?}"),
        },
    };

    // Invite pilot 2 and pilot 3
    rpc(&ctx, &mut sess1, "fleetObjectHandler", "Invite", &[PyRep::Integer(PILOT_2), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(4)])
        .await
        .expect("Invite PILOT_2");
    rpc(&ctx, &mut sess2, "fleetObjectHandler", "AcceptInvite", &[])
        .await
        .expect("AcceptInvite PILOT_2");

    rpc(&ctx, &mut sess1, "fleetObjectHandler", "Invite", &[PyRep::Integer(PILOT_3), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(4)])
        .await
        .expect("Invite PILOT_3");
    rpc(&ctx, &mut sess3, "fleetObjectHandler", "AcceptInvite", &[])
        .await
        .expect("AcceptInvite PILOT_3");

    // 1. Non-boss (pilot 2) tries MakeLeader -> rejected with FleetPermissionDenied
    let non_boss_err = rpc(&ctx, &mut sess2, "fleetMgr", "MakeLeader", &[PyRep::Integer(PILOT_3)])
        .await
        .expect_err("Non-boss MakeLeader must fail");
    assert_eq!(non_boss_err.code, "FleetPermissionDenied");

    // 2. Boss (pilot 1) transfers leadership to pilot 2 via fleetMgr.MakeLeader
    let leader_res = rpc(&ctx, &mut sess1, "fleetMgr", "MakeLeader", &[PyRep::Integer(PILOT_2)])
        .await
        .expect("fleetMgr.MakeLeader");
    assert_eq!(field(&leader_res, "newBossID"), PyRep::Integer(PILOT_2));

    // Verify fleet boss is now PILOT_2
    let boss = ctx.db.with_snapshot(|s| s.fleets.get(&fleet_id).map(|f| f.boss_id));
    assert_eq!(boss, Some(PILOT_2));

    // 3. Automatic boss handover on disconnect:
    // Pilot 2 (current boss) disconnects
    eve_server::services::fleet_social_notify::handle_character_disconnect(&ctx, PILOT_2);

    // Verify boss automatically reassigned to next eligible member (pilot 1 or pilot 3)
    let new_boss = ctx.db.with_snapshot(|s| s.fleets.get(&fleet_id).map(|f| f.boss_id)).unwrap();
    assert!(new_boss == PILOT_1 || new_boss == PILOT_3, "Boss should have reassigned on disconnect, got {new_boss}");
    assert_ne!(new_boss, PILOT_2);

    // 4. Automatic boss handover on leave:
    let mut boss_sess = if new_boss == PILOT_1 { &mut sess1 } else { &mut sess3 };
    let leave_res = rpc(&ctx, &mut boss_sess, "fleetMgr", "LeaveFleet", &[])
        .await
        .expect("LeaveFleet");
    assert_eq!(field(&leave_res, "left"), PyRep::Bool(true));

    // Boss should now be the remaining member
    let final_boss = ctx.db.with_snapshot(|s| s.fleets.get(&fleet_id).map(|f| f.boss_id)).unwrap();
    let expected_final = if new_boss == PILOT_1 { PILOT_3 } else { PILOT_1 };
    assert_eq!(final_boss, expected_final);
}

#[tokio::test]
async fn test_fleet_stored_formations_and_warp_geometry_offsets() {
    // parity: feat:fleet.stored-formations-and-warp-shapes
    eve_testkit::attest(&["feat:fleet.stored-formations-and-warp-shapes"]);

    let ctx = common::test_context().await;
    let mut sess1 = select_pilot(&ctx, 60_011, "testpilot1", "testpass1", PILOT_1).await;
    let mut sess2 = select_pilot(&ctx, 60_012, "testpilot2", "testpass2", PILOT_2).await;

    let fleet_rep = rpc(&ctx, &mut sess1, "fleetObjectHandler", "CreateFleet", &[])
        .await
        .expect("CreateFleet");
    let _fleet_id = match fleet_rep {
        PyRep::Integer(id) => id,
        other => match field(&other, "fleetID") {
            PyRep::Integer(id) => id,
            _ => panic!("Expected fleetID, got {other:?}"),
        },
    };

    rpc(&ctx, &mut sess1, "fleetObjectHandler", "Invite", &[PyRep::Integer(PILOT_2), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(4)])
        .await
        .expect("Invite PILOT_2");
    rpc(&ctx, &mut sess2, "fleetObjectHandler", "AcceptInvite", &[])
        .await
        .expect("AcceptInvite PILOT_2");

    // Store custom formation shape: formation 3 (Arrow), spacing 5000.0, size 15000.0
    // rpc-lint: allow auditor acceptance text feat:fleet.stored-formations-and-warp-shapes
    let set_form = rpc(&ctx, &mut sess1, "fleetObjectHandler", "SetFleetFormation", &[PyRep::Integer(3), PyRep::Real(5000.0), PyRep::Real(15000.0)])
        .await
        .expect("SetFleetFormation");
    assert_eq!(field(&set_form, "formationID"), PyRep::Integer(3));
    assert_eq!(field(&set_form, "formationName"), PyRep::String("Arrow".into()));
    assert_eq!(field(&set_form, "formationSpacing"), PyRep::Real(5000.0));

    // Retrieve stored formation
    // rpc-lint: allow auditor acceptance text feat:fleet.stored-formations-and-warp-shapes
    let get_form = rpc(&ctx, &mut sess1, "fleetObjectHandler", "GetFleetFormation", &[])
        .await
        .expect("GetFleetFormation");
    assert_eq!(field(&get_form, "formationID"), PyRep::Integer(3));
    assert_eq!(field(&get_form, "formationName"), PyRep::String("Arrow".into()));
    assert_eq!(field(&get_form, "formationSpacing"), PyRep::Real(5000.0));

    // Place both pilots in space in the same system
    let sys_id = 30_000_142;
    sess1.solar_system_id = Some(sys_id);
    sess2.solar_system_id = Some(sys_id);
    sess1.station_id = None;
    sess2.station_id = None;
    sess1.structure_id = None;
    sess2.structure_id = None;

    let ship1_id = 99_001_001;
    let ship2_id = 99_001_002;
    sess1.ship_id = Some(ship1_id);
    sess2.ship_id = Some(ship2_id);

    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&PILOT_1) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship1_id;
        }
        if let Some(ch) = s.characters.get_mut(&PILOT_2) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship2_id;
        }
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    sys.spawn_ball(Ball {
        id: ship1_id,
        type_id: 606,
        position: DVec3::new(0.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 500.0,
        inertia: 1.0,
        mass: 100_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }).await.unwrap();

    sys.spawn_ball(Ball {
        id: ship2_id,
        type_id: 606,
        position: DVec3::new(100.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 500.0,
        inertia: 1.0,
        mass: 100_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }).await.unwrap();

    // Destination celestial (stargate at 50,000,000 m)
    let dest_id = 50_000_001;
    let dest_pos = DVec3::new(50_000_000.0, 0.0, 0.0);
    sys.spawn_ball(Ball {
        id: dest_id,
        type_id: 16,
        position: dest_pos,
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 1.0,
        mass: 1e9,
        radius: 2000.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }).await.unwrap();

    // Fleet commander initiates FleetWarp
    let warp_res = rpc(&ctx, &mut sess1, "fleetObjectHandler", "FleetWarp", &[PyRep::Integer(dest_id)])
        .await
        .expect("FleetWarp");
    assert_eq!(field(&warp_res, "status"), PyRep::String("FleetWarpInitiated".into()));
    assert_eq!(field(&warp_res, "formationID"), PyRep::Integer(3));
    assert_eq!(field(&warp_res, "formationSpacing"), PyRep::Real(5000.0));
    let warped = field(&warp_res, "warpedMembers");
    assert_eq!(warped, PyRep::Integer(2));

    // Verify ships are in warp and landing targets have formation offsets
    let snap1 = sys.get_ball_snapshot(ship1_id).await.unwrap().unwrap();
    let snap2 = sys.get_ball_snapshot(ship2_id).await.unwrap().unwrap();
    match snap1.1.mode {
        MovementMode::Warp { target, .. } => {
            assert!(target.distance(dest_pos) <= 100.0 || target.x < dest_pos.x);
        }
        other => panic!("Ship 1 expected Warp mode, got {other:?}"),
    }
    match snap2.1.mode {
        MovementMode::Warp { target, .. } => {
            assert!(target.distance(dest_pos) > 1000.0, "Ship 2 should have deterministic formation offset");
        }
        other => panic!("Ship 2 expected Warp mode, got {other:?}"),
    }
}

#[tokio::test]
async fn test_fleet_target_tagging_hierarchy_and_destruction_cleanup() {
    // parity: feat:fleet.target-tagging
    eve_testkit::attest(&["feat:fleet.target-tagging"]);

    let ctx = common::test_context().await;
    let mut sess1 = select_pilot(&ctx, 60_021, "testpilot1", "testpass1", PILOT_1).await;
    let mut sess2 = select_pilot(&ctx, 60_022, "testpilot2", "testpass2", PILOT_2).await;

    let _fleet_rep = rpc(&ctx, &mut sess1, "fleetObjectHandler", "CreateFleet", &[])
        .await
        .expect("CreateFleet");

    // Pilot 2 joins as regular Squad Member (role 4)
    rpc(&ctx, &mut sess1, "fleetObjectHandler", "Invite", &[PyRep::Integer(PILOT_2), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(4)])
        .await
        .expect("Invite PILOT_2");
    rpc(&ctx, &mut sess2, "fleetObjectHandler", "AcceptInvite", &[])
        .await
        .expect("AcceptInvite PILOT_2");

    let sys_id = 30_000_142;
    sess1.solar_system_id = Some(sys_id);
    sess2.solar_system_id = Some(sys_id);
    let ship1_id = 99_002_001;
    let ship2_id = 99_002_002;
    sess1.ship_id = Some(ship1_id);
    sess2.ship_id = Some(ship2_id);

    let sys = ctx.systems.get_or_spawn(sys_id);
    sys.spawn_ball(Ball {
        id: ship1_id,
        type_id: 606,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 500.0,
        inertia: 1.0,
        mass: 100_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }).await.unwrap();

    let target_ball_id = 99_002_003;
    sys.spawn_ball(Ball {
        id: target_ball_id,
        type_id: 606,
        position: DVec3::new(10_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 500.0,
        inertia: 1.0,
        mass: 100_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }).await.unwrap();

    // Pilot 1 locks target
    sys.add_target_lock(ship1_id, target_ball_id).await.unwrap();

    // 1. Regular squad member tries to tag -> rejected with FleetRoleDenied
    // rpc-lint: allow auditor acceptance text feat:fleet.target-tagging
    let member_tag_err = rpc(&ctx, &mut sess2, "fleetProxy", "SetTargetTag", &[PyRep::Integer(target_ball_id), PyRep::String("1".into())])
        .await
        .expect_err("Squad member tagging should be denied");
    assert_eq!(member_tag_err.code, "FleetRoleDenied");

    // 2. Commander attempts invalid tag string -> rejected with InvalidTargetTag
    // rpc-lint: allow auditor acceptance text feat:fleet.target-tagging
    let invalid_tag_err = rpc(&ctx, &mut sess1, "fleetProxy", "SetTargetTag", &[PyRep::Integer(target_ball_id), PyRep::String("INVALID".into())])
        .await
        .expect_err("Multi-char tag must be rejected");
    assert_eq!(invalid_tag_err.code, "InvalidTargetTag");

    // 3. Commander assigns valid tag "A"
    // rpc-lint: allow auditor acceptance text feat:fleet.target-tagging
    let tag_res = rpc(&ctx, &mut sess1, "fleetProxy", "SetTargetTag", &[PyRep::Integer(target_ball_id), PyRep::String("A".into())])
        .await
        .expect("SetTargetTag A");
    assert_eq!(field(&tag_res, "tag"), PyRep::String("A".into()));

    // Verify tag appears in GetTargetTags
    // rpc-lint: allow auditor acceptance text feat:fleet.target-tagging
    let get_tags = rpc(&ctx, &mut sess1, "fleetObjectHandler", "GetTargetTags", &[])
        .await
        .expect("GetTargetTags");
    let PyRep::Dict(tag_entries) = get_tags else { panic!("Expected Dict, got {get_tags:?}") };
    assert!(tag_entries.iter().any(|(k, v)| k == &PyRep::Integer(target_ball_id) && v == &PyRep::String("A".into())));

    // 4. Target destruction: clear_destroyed_target_tag removes the tag
    eve_server::services::fleet_social::fleet_social_finder_tactical::clear_destroyed_target_tag(&ctx, target_ball_id);

    // Verify tag is cleared
    // rpc-lint: allow auditor acceptance text feat:fleet.target-tagging
    let get_tags_after = rpc(&ctx, &mut sess1, "fleetObjectHandler", "GetTargetTags", &[])
        .await
        .expect("GetTargetTags after destruction");
    let PyRep::Dict(entries_after) = get_tags_after else { panic!("Expected Dict") };
    assert!(!entries_after.iter().any(|(k, _)| k == &PyRep::Integer(target_ball_id)));
}
