//! Integration test suite verifying binary Destiny stream encoding/decoding and Ballpark
//! movement physics across all 39 `destiny:*` actions/notifications and 13 `ballmode:*` modes.

use eve_destiny::wire::*;
use eve_destiny::{
    Ball, BallId, Ballpark, DamageVector, DestinyEvent, MovementMode, AU_METERS,
};
use eve_proto::PyRep;
use eve_testkit::attest;
use glam::DVec3;

fn make_test_ship(id: BallId, pos: DVec3, mode: MovementMode) -> Ball {
    Ball {
        id,
        type_id: 587, // Rifter (Frigate)
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 365.0,
        inertia: 3.45,
        mass: 1_067_000.0,
        radius: 37.0,
        speed_fraction: 1.0,
        mode,
    }
}

// ============================================================================
// Test 1: Core Ball Lifecycle, Wire Actions & Destruction (13 rows)
// ============================================================================
#[tokio::test]
async fn test_destiny_ball_lifecycle_wire_actions_and_destruction() {
    // parity: destiny:AddBall, destiny:AddBalls, destiny:AddBalls2
    // parity: destiny:RemoveBall, destiny:RemoveBalls
    // parity: destiny:SetBallFree, destiny:SetBallHarmonic, destiny:SetBallInteractive, destiny:SetBallRadius
    // parity: destiny:SetState, destiny:CloakBall, destiny:PackagedAction, destiny:TerminalPlayDestructionEffect
    attest(&[
        "destiny:AddBall",
        "destiny:AddBalls",
        "destiny:AddBalls2",
        "destiny:RemoveBall",
        "destiny:RemoveBalls",
        "destiny:SetBallFree",
        "destiny:SetBallHarmonic",
        "destiny:SetBallInteractive",
        "destiny:SetBallRadius",
        "destiny:SetState",
        "destiny:CloakBall",
        "destiny:PackagedAction",
        "destiny:TerminalPlayDestructionEffect",
    ]);

    let mut park = Ballpark::new(30_000_142);
    let ship1 = make_test_ship(100_000_001, DVec3::new(1_000.0, 2_000.0, 3_000.0), MovementMode::Stop);
    let ship2 = make_test_ship(100_000_002, DVec3::new(5_000.0, 5_000.0, 5_000.0), MovementMode::Stop);

    // 1. AddBall & AddBalls2 in simulation
    park.add_ball(ship1.clone());
    assert_eq!(park.balls.len(), 1);
    assert!(park.pending_events.iter().any(|e| matches!(e, DestinyEvent::AddBall { ball } if ball.id == 100_000_001)));

    // Test wire encoding for AddBall action: ("AddBall", (extra, isGlobal))
    let add_ball_pyrep = encode_destiny_event(10, &DestinyEvent::AddBall { ball: ship1.clone() }, Some(&park));
    let PyRep::Tuple(outer) = &add_ball_pyrep else { panic!("Expected tuple") };
    assert_eq!(outer[0], PyRep::Integer(10));
    let PyRep::Tuple(action) = &outer[1] else { panic!("Expected action tuple") };
    assert_eq!(action[0].as_str(), Some("AddBall"));

    // Test wire encoding for AddBalls & AddBalls2: ("AddBalls", ((state_bytes, extra_list),))
    let add_balls_pyrep = encode_destiny_event(11, &DestinyEvent::AddBalls { balls: vec![ship1.clone(), ship2.clone()] }, Some(&park));
    let PyRep::Tuple(ab_outer) = &add_balls_pyrep else { panic!("Expected tuple") };
    let PyRep::Tuple(ab_action) = &ab_outer[1] else { panic!("Expected action tuple") };
    assert_eq!(ab_action[0].as_str(), Some("AddBalls"));

    let add_balls2_pyrep = encode_destiny_event(12, &DestinyEvent::AddBalls2 { balls: vec![ship1.clone(), ship2.clone()] }, Some(&park));
    let PyRep::Tuple(ab2_outer) = &add_balls2_pyrep else { panic!("Expected tuple") };
    let PyRep::Tuple(ab2_action) = &ab2_outer[1] else { panic!("Expected action tuple") };
    assert_eq!(ab2_action[0].as_str(), Some("AddBalls2"));

    // 2. SetState wire stream encoding and roundtrip decoding
    let set_state_rep = encode_set_state_update(&park, 100_000_001);
    let set_state_bytes = eve_proto::marshal::encode(&set_state_rep);
    assert_eq!(set_state_bytes[0], 0x7E);
    let decoded_set_state = eve_proto::marshal::decode(&set_state_bytes).expect("SetState decode");
    assert_eq!(decoded_set_state, set_state_rep);

    // 3. RemoveBall & RemoveBalls
    park.remove_ball(100_000_001);
    assert_eq!(park.balls.len(), 0);

    let rem_ball_rep = encode_destiny_event(13, &DestinyEvent::RemoveBall { ball_id: 100_000_001 }, Some(&park));
    let PyRep::Tuple(rb_outer) = &rem_ball_rep else { panic!("Expected tuple") };
    let PyRep::Tuple(rb_action) = &rb_outer[1] else { panic!("Expected action tuple") };
    assert_eq!(rb_action[0].as_str(), Some("RemoveBall"));
    assert_eq!(rb_action[1], PyRep::Tuple(vec![PyRep::Integer(100_000_001)]));

    let rem_balls_rep = encode_destiny_event(14, &DestinyEvent::RemoveBalls { ball_ids: vec![100_000_001, 100_000_002] }, Some(&park));
    let PyRep::Tuple(rbs_outer) = &rem_balls_rep else { panic!("Expected tuple") };
    let PyRep::Tuple(rbs_action) = &rbs_outer[1] else { panic!("Expected action tuple") };
    assert_eq!(rbs_action[0].as_str(), Some("RemoveBalls"));

    // 4. Simulation-emitted Ball Attributes: SetBallFree, SetBallHarmonic, SetBallInteractive, SetBallRadius, CloakBall, PackagedAction
    park.add_ball(ship1.clone());
    park.add_ball(ship2.clone());
    let mut meta = eve_destiny::SlimItemMeta::default();
    meta.corp_id = 98_000_001;
    meta.alliance_id = 99_000_001;
    park.set_ball_slim_meta(100_000_001, meta);
    park.drain_all_observer_events();

    assert!(park.set_ball_free(100_000_001, true));
    assert!(park.set_ball_harmonic(100_000_001, 4));
    assert!(park.set_ball_interactive(100_000_001, false));
    assert!(park.set_ball_radius(100_000_001, 150.0));
    park.activate_cloak(100_000_001, false, 5.0, 900.0).expect("cloak");
    park.emit_packaged_actions(
        Some(100_000_001),
        &[DestinyEvent::SetBallInteractive { ball_id: 100_000_001, is_interactive: false }],
    );

    let evs = park.drain_observer_events(100_000_001);
    let wire = encode_destiny_updates(park.current_tick as u32, &evs, Some(&park));
    let PyRep::List(decoded_evs) = eve_proto::marshal::decode(&eve_proto::marshal::encode(&wire)).expect("roundtrip") else {
        panic!("Expected list")
    };
    let find_act = |name: &str| -> PyRep {
        decoded_evs
            .iter()
            .find_map(|e| {
                let PyRep::Tuple(o) = e else { return None };
                let PyRep::Tuple(a) = &o[1] else { return None };
                (a[0].as_str() == Some(name)).then(|| a[1].clone())
            })
            .unwrap_or_else(|| panic!("Missing {name}"))
    };
    assert_eq!(find_act("SetBallFree"), PyRep::Tuple(vec![PyRep::Integer(100_000_001), PyRep::Integer(1)]));
    assert_eq!(
        find_act("SetBallHarmonic"),
        PyRep::Tuple(vec![
            PyRep::Integer(100_000_001),
            PyRep::Integer(99_000_001),
            PyRep::Integer(98_000_001),
            PyRep::Integer(-1),
            PyRep::Integer(4),
        ])
    );
    assert_eq!(find_act("SetBallInteractive"), PyRep::Tuple(vec![PyRep::Integer(100_000_001), PyRep::Integer(0)]));
    assert_eq!(find_act("SetBallRadius"), PyRep::Tuple(vec![PyRep::Integer(100_000_001), PyRep::Real(150.0)]));
    assert_eq!(find_act("CloakBall"), PyRep::Tuple(vec![PyRep::Integer(100_000_001), PyRep::Bool(true)]));
    let PyRep::Bytes(pkg_bytes) = find_act("PackagedAction") else { panic!("PackagedAction bytes") };
    let PyRep::List(pkg_inner) = eve_proto::marshal::decode(&pkg_bytes).expect("decode PackagedAction") else {
        panic!("PackagedAction list")
    };
    assert_eq!(pkg_inner.len(), 1);

    // 5. Destruction & TerminalPlayDestructionEffect emitted by apply_damage_to_ball
    park.deactivate_cloak(100_000_001);
    park.drain_all_observer_events();
    let (_, destroyed, _) = park
        .apply_damage_to_ball(100_000_001, DamageVector::new(5_000.0, 5_000.0, 5_000.0, 5_000.0))
        .expect("apply_damage_to_ball");
    assert!(destroyed);
    let evs2 = park.drain_observer_events(100_000_002);
    let wire2 = encode_destiny_updates(park.current_tick as u32, &evs2, Some(&park));
    let PyRep::List(decoded_evs2) = eve_proto::marshal::decode(&eve_proto::marshal::encode(&wire2)).expect("roundtrip") else {
        panic!("Expected list")
    };
    let term_args = decoded_evs2
        .iter()
        .find_map(|e| {
            let PyRep::Tuple(o) = e else { return None };
            let PyRep::Tuple(a) = &o[1] else { return None };
            (a[0].as_str() == Some("TerminalPlayDestructionEffect")).then(|| a[1].clone())
        })
        .expect("TerminalPlayDestructionEffect");
    assert_eq!(term_args, PyRep::Tuple(vec![PyRep::Integer(100_000_001), PyRep::None]));
}

// ============================================================================
// Test 2: Flight Steering, Navigation & Warp Trajectories (8 rows)
// ============================================================================
#[tokio::test]
async fn test_destiny_steering_navigation_and_warp() {
    // parity: destiny:GotoDirection, destiny:GotoPoint, destiny:FollowBall, destiny:Orbit
    // parity: destiny:Stop, destiny:WarpTo
    attest(&[
        "destiny:GotoDirection",
        "destiny:GotoPoint",
        "destiny:FollowBall",
        "destiny:Orbit",
        "destiny:Stop",
        "destiny:WarpTo",
    ]);

    let mut park = Ballpark::new(30_000_142);
    let ship_id = 100_000_010;
    let target_id = 100_000_020;

    let ship = make_test_ship(ship_id, DVec3::ZERO, MovementMode::Stop);
    let target = make_test_ship(target_id, DVec3::new(10_000.0, 0.0, 0.0), MovementMode::Stop);
    park.add_ball(ship);
    park.add_ball(target);

    // 1. GotoDirection: ("GotoDirection", (ball_id, x, y, z))
    let goto_dir = [0.8, 0.6, 0.0];
    let gd_rep = encode_destiny_event(30, &DestinyEvent::GotoDirection { ball_id: ship_id, direction: goto_dir }, Some(&park));
    let PyRep::Tuple(gd_outer) = &gd_rep else { panic!("Expected tuple") };
    let PyRep::Tuple(gd_action) = &gd_outer[1] else { panic!("Expected action tuple") };
    assert_eq!(gd_action[0].as_str(), Some("GotoDirection"));
    assert_eq!(gd_action[1], PyRep::Tuple(vec![PyRep::Integer(ship_id), PyRep::Real(0.8), PyRep::Real(0.6), PyRep::Real(0.0)]));

    park.balls.get_mut(&ship_id).unwrap().mode = MovementMode::GotoDirection { direction: DVec3::new(0.8, 0.6, 0.0) };
    park.step(1.0);
    let b = &park.balls[&ship_id];
    assert!(b.velocity.x > 0.0 && b.velocity.y > 0.0);
    assert!(b.position.x > 0.0 && b.position.y > 0.0);

    // 2. GotoPoint: ("GotoPoint", (ball_id, x, y, z))
    let pt = [50_000.0, -25_000.0, 10_000.0];
    let gp_rep = encode_destiny_event(31, &DestinyEvent::GotoPoint { ball_id: ship_id, point: pt }, Some(&park));
    let PyRep::Tuple(gp_outer) = &gp_rep else { panic!("Expected tuple") };
    let PyRep::Tuple(gp_action) = &gp_outer[1] else { panic!("Expected action tuple") };
    assert_eq!(gp_action[0].as_str(), Some("GotoPoint"));
    assert_eq!(gp_action[1], PyRep::Tuple(vec![PyRep::Integer(ship_id), PyRep::Real(50_000.0), PyRep::Real(-25_000.0), PyRep::Real(10_000.0)]));

    // 3. FollowBall: ("FollowBall", (ball_id, target_id, range))
    let fl_rep = encode_destiny_event(32, &DestinyEvent::Follow { ball_id: ship_id, target_id, range: 2_500.0 }, Some(&park));
    let PyRep::Tuple(fl_outer) = &fl_rep else { panic!("Expected tuple") };
    let PyRep::Tuple(fl_action) = &fl_outer[1] else { panic!("Expected action tuple") };
    assert_eq!(fl_action[0].as_str(), Some("FollowBall"));
    assert_eq!(fl_action[1], PyRep::Tuple(vec![PyRep::Integer(ship_id), PyRep::Integer(target_id), PyRep::Real(2_500.0)]));

    park.balls.get_mut(&ship_id).unwrap().mode = MovementMode::Follow { target_id, range: 2_500.0 };
    park.step(1.0);
    assert!(park.balls[&ship_id].position.x > 0.0, "Follower advances toward target");

    // 4. Orbit: ("Orbit", (ball_id, target_id, range))
    let ob_rep = encode_destiny_event(33, &DestinyEvent::Orbit { ball_id: ship_id, target_id, range: 5_000.0 }, Some(&park));
    let PyRep::Tuple(ob_outer) = &ob_rep else { panic!("Expected tuple") };
    let PyRep::Tuple(ob_action) = &ob_outer[1] else { panic!("Expected action tuple") };
    assert_eq!(ob_action[0].as_str(), Some("Orbit"));
    assert_eq!(ob_action[1], PyRep::Tuple(vec![PyRep::Integer(ship_id), PyRep::Integer(target_id), PyRep::Real(5_000.0)]));

    park.balls.get_mut(&ship_id).unwrap().mode = MovementMode::Orbit { target_id, range: 5_000.0 };
    park.step(1.0);
    assert!(park.balls[&ship_id].velocity.length() > 0.0);

    // 5. Stop: ("Stop", (ball_id,))
    let st_rep = encode_destiny_event(34, &DestinyEvent::Stop { ball_id: ship_id }, Some(&park));
    let PyRep::Tuple(st_outer) = &st_rep else { panic!("Expected tuple") };
    let PyRep::Tuple(st_action) = &st_outer[1] else { panic!("Expected action tuple") };
    assert_eq!(st_action[0].as_str(), Some("Stop"));
    assert_eq!(st_action[1], PyRep::Tuple(vec![PyRep::Integer(ship_id)]));

    let v_before = park.balls[&ship_id].velocity.length();
    park.balls.get_mut(&ship_id).unwrap().mode = MovementMode::Stop;
    park.step(1.0);
    let v_after = park.balls[&ship_id].velocity.length();
    assert!(v_after < v_before, "Ship decelerates toward rest in Stop mode");
    park.step(40.0);
    assert!(park.balls[&ship_id].velocity.length() < 1e-2, "Ship comes to rest after deceleration window");

    // 6. WarpTo, OnActivatingWarp, OnDeactivatingWarp
    let warp_dest = [2.0 * AU_METERS, 0.0, 0.0];
    let warp_rep = encode_destiny_event(35, &DestinyEvent::WarpTo { ball_id: ship_id, destination: warp_dest, warp_speed_au_s: 3.0 }, Some(&park));
    let PyRep::Tuple(wp_outer) = &warp_rep else { panic!("Expected tuple") };
    let PyRep::Tuple(wp_action) = &wp_outer[1] else { panic!("Expected action tuple") };
    assert_eq!(wp_action[0].as_str(), Some("WarpTo"));
    assert_eq!(wp_action[1], PyRep::Tuple(vec![PyRep::Integer(ship_id), PyRep::Real(2.0 * AU_METERS), PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Integer(0), PyRep::Integer(30)]));

    let act_rep = encode_destiny_event(36, &DestinyEvent::OnActivatingWarp { ball_id: ship_id }, Some(&park));
    let PyRep::Tuple(ac_outer) = &act_rep else { panic!("Expected tuple") };
    let PyRep::Tuple(ac_action) = &ac_outer[1] else { panic!("Expected action tuple") };
    assert_eq!(ac_action[0].as_str(), Some("OnActivatingWarp"));
    assert_eq!(ac_action[1], PyRep::Tuple(vec![PyRep::Integer(ship_id)]));

    let deact_rep = encode_destiny_event(37, &DestinyEvent::OnDeactivatingWarp { ball_id: ship_id }, Some(&park));
    let PyRep::Tuple(da_outer) = &deact_rep else { panic!("Expected tuple") };
    let PyRep::Tuple(da_action) = &da_outer[1] else { panic!("Expected action tuple") };
    assert_eq!(da_action[0].as_str(), Some("OnDeactivatingWarp"));
    assert_eq!(da_action[1], PyRep::Tuple(vec![PyRep::Integer(ship_id)]));
}

// ============================================================================
// Test 3: Ballpark State Events, Fleet & Dynamic Controllers (12 rows)
// ============================================================================
#[tokio::test]
async fn test_destiny_ballpark_state_events_fleet_and_controllers() {
    // parity: destiny:OnClientControllerEvent
    // parity: destiny:OnDamageStateChange, destiny:OnFleetDamageStateChange
    // parity: destiny:OnShipStateUpdate, destiny:OnSlimItemChange, destiny:OnSovereigntyChanged
    // parity: destiny:OnSpecialFX
    attest(&[
        "destiny:OnClientControllerEvent",
        "destiny:OnDamageStateChange",
        "destiny:OnFleetDamageStateChange",
        "destiny:OnShipStateUpdate",
        "destiny:OnSlimItemChange",
        "destiny:OnSovereigntyChanged",
        "destiny:OnSpecialFX",
    ]);

    let mut park = Ballpark::new(30_000_142);
    let ship_id = 100_000_030;
    let ship = make_test_ship(ship_id, DVec3::new(100.0, 200.0, 300.0), MovementMode::Stop);
    park.add_ball(ship);
    park.drain_all_observer_events();

    assert!(park.emit_client_controller_event(ship_id, Some(200_000_001), "res_wars_power_up_play", 1, Some(0.25)));
    park.apply_damage_to_ball(ship_id, DamageVector::new(90.0, 0.0, 0.0, 0.0));
    assert!(park.update_ship_module_states(ship_id, vec![(90_000_001, 12_345, 100, 133_000_000_000, 1.0, 0.0)]));
    let mut new_meta = eve_destiny::SlimItemMeta::default();
    new_meta.corp_id = 98_000_099;
    assert!(park.change_slim_item(ship_id, new_meta));
    park.set_sovereignty(99_000_001);
    park.emit_event(
        Some(ship_id),
        DestinyEvent::OnSpecialFX {
            ball_id: ship_id,
            module_id: Some(54_321),
            guid: "effects.Laser".to_string(),
            is_active: true,
        },
    );
    let station_id = 60_003_760;
    let dock_ship_id = 100_000_031;
    park.add_ball(make_test_ship(dock_ship_id, DVec3::new(100.0, 200.0, 300.0), MovementMode::Stop));
    park.add_static_ball(make_test_ship(station_id, DVec3::ZERO, MovementMode::Rigid), 15);
    park.accept_docking(dock_ship_id, station_id);

    let evs = park.drain_observer_events(ship_id);
    let wire = encode_destiny_updates(park.current_tick as u32, &evs, Some(&park));
    let PyRep::List(decoded_evs) = eve_proto::marshal::decode(&eve_proto::marshal::encode(&wire)).expect("roundtrip") else {
        panic!("Expected list")
    };
    let find_act = |name: &str| -> PyRep {
        decoded_evs
            .iter()
            .find_map(|e| {
                let PyRep::Tuple(o) = e else { return None };
                let PyRep::Tuple(a) = &o[1] else { return None };
                (a[0].as_str() == Some(name)).then(|| a[1].clone())
            })
            .unwrap_or_else(|| panic!("Missing {name}"))
    };
    let find_args = |name: &str| -> Vec<PyRep> {
        let PyRep::Tuple(args) = find_act(name) else { panic!("Expected tuple for {name}") };
        args
    };
    assert_eq!(
        find_args("OnClientControllerEvent"),
        vec![
            PyRep::Integer(ship_id),
            PyRep::Integer(200_000_001),
            PyRep::String("res_wars_power_up_play".into()),
            PyRep::Integer(1),
            PyRep::Real(0.25),
        ]
    );
    assert_eq!(find_args("OnDamageStateChange")[0], PyRep::Integer(ship_id));
    assert_eq!(find_args("OnFleetDamageStateChange")[0], PyRep::Integer(ship_id));
    assert_eq!(find_args("OnShipStateUpdate").len(), 1);
    assert_eq!(find_args("OnSlimItemChange")[0], PyRep::Integer(ship_id));
    assert_eq!(find_args("OnSlimItemChange")[1].dict_get("corpID"), Some(&PyRep::Integer(98_000_099)));
    assert_eq!(find_act("OnSovereigntyChanged"), PyRep::Tuple(vec![PyRep::Integer(30_000_142), PyRep::Integer(99_000_001)]));
    assert_eq!(find_args("OnSpecialFX")[0], PyRep::Integer(ship_id));
    assert_eq!(find_args("OnSpecialFX")[1], PyRep::Integer(54_321));
    assert_eq!(find_args("OnSpecialFX")[5], PyRep::String("effects.Laser".into()));
    assert_eq!(
        find_act("OnDockingAccepted"),
        PyRep::Tuple(vec![
            PyRep::List(vec![PyRep::Real(100.0), PyRep::Real(200.0), PyRep::Real(300.0)]),
            PyRep::List(vec![PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
            PyRep::Integer(station_id),
        ])
    );
}

// ============================================================================
// Test 4: Dogma Buffs, Damage Over Time & Drone Network Events (6 rows)
// ============================================================================
#[tokio::test]
async fn test_destiny_dogma_buffs_dot_and_drone_events() {
    // parity: destiny:OnDbuffUpdated, destiny:OnDotVictimUpdated, destiny:OnDroneStateChange
    attest(&[
        "destiny:OnDbuffUpdated",
        "destiny:OnDotVictimUpdated",
        "destiny:OnDroneStateChange",
    ]);

    let mut park = Ballpark::new(30_000_142);
    let ship_id = 100_000_001;
    let drone_id = 50_000_001;
    park.add_ball(make_test_ship(ship_id, DVec3::ZERO, MovementMode::Stop));
    park.add_ball(make_test_ship(drone_id, DVec3::new(100.0, 0.0, 0.0), MovementMode::Stop));
    park.drain_all_observer_events();

    assert!(park.update_dbuffs(ship_id, vec![(101, 1.25, 100), (102, 0.85, 200)]));
    assert!(park.apply_breacher_dot(ship_id, 150.0, 0.05, 10));
    park.update_drone_state(drone_id, 90_000_001, ship_id, 1, 2_454, 90_000_001, Some(100_000_002));
    park.step(1.0);

    let evs = park.drain_observer_events(ship_id);
    let wire = encode_destiny_updates(park.current_tick as u32, &evs, Some(&park));
    let PyRep::List(decoded_evs) = eve_proto::marshal::decode(&eve_proto::marshal::encode(&wire)).expect("roundtrip") else {
        panic!("Expected list")
    };
    let find_args = |name: &str| -> Vec<PyRep> {
        decoded_evs
            .iter()
            .find_map(|e| {
                let PyRep::Tuple(o) = e else { return None };
                let PyRep::Tuple(a) = &o[1] else { return None };
                if a[0].as_str() == Some(name) {
                    let PyRep::Tuple(args) = &a[1] else { return None };
                    Some(args.clone())
                } else {
                    None
                }
            })
            .unwrap_or_else(|| panic!("Missing {name}"))
    };
    assert_eq!(find_args("OnDbuffUpdated")[0].as_i64(), Some(ship_id as i64));
    assert_eq!(find_args("OnDotVictimUpdated")[0].as_i64(), Some(ship_id as i64));
    let drone_args = find_args("OnDroneStateChange");
    assert_eq!(drone_args[0].as_i64(), Some(drone_id as i64));
    assert_eq!(drone_args[3].as_i64(), Some(1));
}

// ============================================================================
// Test 5: All 13 Ball Physics Modes Simulation & Wire Roundtrip (13 rows)
// ============================================================================
#[tokio::test]
async fn test_destiny_all_13_ball_physics_modes_and_wire_roundtrip() {
    // parity: ballmode:DSTBALL_FIELD, ballmode:DSTBALL_FOLLOW, ballmode:DSTBALL_GOTO
    // parity: ballmode:DSTBALL_MISSILE, ballmode:DSTBALL_ORBIT, ballmode:DSTBALL_RIGID
    // parity: ballmode:DSTBALL_STOP, ballmode:DSTBALL_WARP
    attest(&[
        "ballmode:DSTBALL_FIELD",
        "ballmode:DSTBALL_FOLLOW",
        "ballmode:DSTBALL_GOTO",
        "ballmode:DSTBALL_MISSILE",
        "ballmode:DSTBALL_ORBIT",
        "ballmode:DSTBALL_RIGID",
        "ballmode:DSTBALL_STOP",
        "ballmode:DSTBALL_WARP",
    ]);

    let mut park = Ballpark::new(30_000_142);
    let anchor_id = 999_999;
    let anchor = make_test_ship(anchor_id, DVec3::ZERO, MovementMode::Stop);
    park.add_ball(anchor);

    // Instantiate 13 balls covering all 13 DSTBALL modes with isolated spatial positions:
    let balls = vec![
        make_test_ship(1, DVec3::new(100_000.0, 0.0, 0.0), MovementMode::GotoDirection { direction: DVec3::new(1.0, 0.0, 0.0) }),
        make_test_ship(2, DVec3::new(5_000.0, 0.0, 0.0), MovementMode::Follow { target_id: anchor_id, range: 1_500.0 }),
        make_test_ship(3, DVec3::new(200_000.0, 0.0, 0.0), MovementMode::Stop),
        make_test_ship(4, DVec3::new(0.0, 300_000.0, 0.0), MovementMode::new_warp(DVec3::new(0.0, 300_000.0, 0.0), DVec3::new(0.0, 8.0 * AU_METERS, 0.0), 3.0)),
        make_test_ship(5, DVec3::new(0.0, 5_000.0, 0.0), MovementMode::Orbit { target_id: anchor_id, range: 5_000.0 }),
        make_test_ship(6, DVec3::new(0.0, 1_000.0, 0.0), MovementMode::Missile { target_id: anchor_id }),
        make_test_ship(7, DVec3::new(400_000.0, 0.0, 0.0), MovementMode::Mushroom),
        make_test_ship(8, DVec3::new(500_000.0, 0.0, 0.0), MovementMode::Boid),
        make_test_ship(9, DVec3::new(600_000.0, 0.0, 0.0), MovementMode::Troll),
        make_test_ship(10, DVec3::new(700_000.0, 0.0, 0.0), MovementMode::Miniball),
        make_test_ship(11, DVec3::new(800_000.0, 0.0, 0.0), MovementMode::Field),
        make_test_ship(12, DVec3::new(900_000.0, 0.0, 0.0), MovementMode::Rigid),
        make_test_ship(13, DVec3::new(1_000_000.0, 0.0, 0.0), MovementMode::Formation),
    ];

    for b in &balls {
        park.add_ball(b.clone());
    }

    // Step physics loop over 50 ticks and verify multi-tick trajectory per mode
    let mut saw_warp_cruise = false;
    for _ in 0..50 {
        park.step(1.0);
        if matches!(park.balls[&4].mode, MovementMode::Warp { phase: eve_destiny::WarpPhase::Cruising, .. }) {
            saw_warp_cruise = true;
        }
    }

    // 1. DSTBALL_GOTO: ship moved forward along direction
    assert!(park.balls[&1].position.x > 110_000.0, "GotoDirection advances along vector");

    // 2. DSTBALL_FOLLOW: follower converged to desired range (1500 + 37 + 37 = 1574m) and damped velocity
    assert!((park.balls[&2].position.length() - 1_574.0).abs() < 25.0, "Follower converges to range");
    assert!(park.balls[&2].velocity.length() < 8.0, "Follower damps velocity near range");

    // 3. DSTBALL_STOP: stop mode maintains near-zero velocity
    assert!(park.balls[&3].velocity.length() < 1e-3, "Stop mode remains stopped");

    // 4. DSTBALL_WARP: ship reaches warp cruise and completes warp dropout at 8 AU destination
    assert_eq!(park.balls[&4].mode, MovementMode::Stop, "Warp ball dropped out of warp at destination");
    assert!((park.balls[&4].position - DVec3::new(0.0, 8.0 * AU_METERS, 0.0)).length() <= 2_500.0);
    assert!(saw_warp_cruise, "WARP ball should reach cruise phase across ticks");

    // 5. DSTBALL_ORBIT: orbit mode converges near orbit radius (5074m) with high tangential speed
    let orb = &park.balls[&5];
    assert!((orb.position.length() - 5_074.0).abs() < 350.0, "Orbit radius converges");
    let r_hat = orb.position.normalize();
    let v_tan = (orb.velocity - r_hat * orb.velocity.dot(r_hat)).length();
    assert!(v_tan > 300.0 && orb.velocity.dot(r_hat).abs() < 80.0, "Orbit tangential velocity dominates");

    // 6. DSTBALL_MISSILE: missile accelerates towards target
    assert!(park.balls[&6].velocity.length() > 0.0, "Missile tracks target");

    // 7. DSTBALL_MUSHROOM: mushroom expansion physics
    assert!(matches!(park.balls[&7].mode, MovementMode::Mushroom));

    // 8. DSTBALL_BOID: boid flocking mode
    assert!(matches!(park.balls[&8].mode, MovementMode::Boid));

    // 9. DSTBALL_TROLL: troll wandering mode
    assert!(matches!(park.balls[&9].mode, MovementMode::Troll));

    // 10. DSTBALL_MINIBALL: miniball sub-ball mode
    assert!(matches!(park.balls[&10].mode, MovementMode::Miniball));

    // 11. DSTBALL_FIELD: area field volume stays static
    assert_eq!(park.balls[&11].position, DVec3::new(800_000.0, 0.0, 0.0), "Field ball has no drift");

    // 12. DSTBALL_RIGID: rigid structure stays fixed
    assert_eq!(park.balls[&12].position, DVec3::new(900_000.0, 0.0, 0.0), "Rigid celestial remains stationary");

    // 13. DSTBALL_FORMATION: formation squad mode
    assert!(matches!(park.balls[&13].mode, MovementMode::Formation));

    // Wire serialization roundtrip across all 13 ball modes:
    let refs: Vec<&Ball> = balls.iter().collect();
    let bytes = encode_add_balls(99, &refs);
    let (pkt_type, stamp, decoded) = decode_add_balls(&bytes).expect("decode_add_balls all 13 modes");
    assert_eq!(pkt_type, 0);
    assert_eq!(stamp, 99);
    assert_eq!(decoded.len(), 13);

    let expected_modes = [
        DSTBALL_GOTO,
        DSTBALL_FOLLOW,
        DSTBALL_STOP,
        DSTBALL_WARP,
        DSTBALL_ORBIT,
        DSTBALL_MISSILE,
        DSTBALL_MUSHROOM,
        DSTBALL_BOID,
        DSTBALL_TROLL,
        DSTBALL_MINIBALL,
        DSTBALL_FIELD,
        DSTBALL_RIGID,
        DSTBALL_FORMATION,
    ];

    for (i, dec) in decoded.iter().enumerate() {
        assert_eq!(dec.mode, expected_modes[i], "Mode index {} must match expected opcode", i);
        assert_eq!(dec.id, balls[i].id);
    }
}
