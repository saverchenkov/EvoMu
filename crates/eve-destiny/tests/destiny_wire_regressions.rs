//! Regression tests for Destiny action-tuple encoding and Ballpark movement physics.
//!
//! These tests carry no parity tags. Most encode hand-built `DestinyEvent`s and check the
//! action tuple instead of a simulation-emitted, client-decoder golden layout, so they do not meet
//! the ledger acceptance text for `destiny:*` / `ballmode:*` rows (review 20261020-192642, R-P1-2/3).

use eve_destiny::wire::*;
use eve_destiny::{
    Ball, BallId, Ballpark, CombatState, DamageVector, DestinyEvent, MovementMode, AU_METERS,
};
use eve_proto::PyRep;
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

    // 4. Ball Attributes: SetBallFree, SetBallHarmonic, SetBallInteractive, SetBallRadius
    let free_rep = encode_destiny_event(15, &DestinyEvent::SetBallFree { ball_id: 100_000_001, is_free: true }, Some(&park));
    let PyRep::Tuple(fr_outer) = &free_rep else { panic!("Expected tuple") };
    let PyRep::Tuple(fr_action) = &fr_outer[1] else { panic!("Expected action tuple") };
    assert_eq!(fr_action[0].as_str(), Some("SetBallFree"));
    assert_eq!(fr_action[1], PyRep::Tuple(vec![PyRep::Integer(100_000_001), PyRep::Integer(1)]));

    let harm_rep = encode_destiny_event(16, &DestinyEvent::SetBallHarmonic { ball_id: 100_000_001, harmonic: 4 }, Some(&park));
    let PyRep::Tuple(hm_outer) = &harm_rep else { panic!("Expected tuple") };
    let PyRep::Tuple(hm_action) = &hm_outer[1] else { panic!("Expected action tuple") };
    assert_eq!(hm_action[0].as_str(), Some("SetBallHarmonic"));
    assert_eq!(hm_action[1], PyRep::Tuple(vec![PyRep::Integer(100_000_001), PyRep::Integer(4)]));

    let inter_rep = encode_destiny_event(17, &DestinyEvent::SetBallInteractive { ball_id: 100_000_001, is_interactive: false }, Some(&park));
    let PyRep::Tuple(ir_outer) = &inter_rep else { panic!("Expected tuple") };
    let PyRep::Tuple(ir_action) = &ir_outer[1] else { panic!("Expected action tuple") };
    assert_eq!(ir_action[0].as_str(), Some("SetBallInteractive"));
    assert_eq!(ir_action[1], PyRep::Tuple(vec![PyRep::Integer(100_000_001), PyRep::Integer(0)]));

    let rad_rep = encode_destiny_event(18, &DestinyEvent::SetBallRadius { ball_id: 100_000_001, radius: 150.0 }, Some(&park));
    let PyRep::Tuple(rd_outer) = &rad_rep else { panic!("Expected tuple") };
    let PyRep::Tuple(rd_action) = &rd_outer[1] else { panic!("Expected action tuple") };
    assert_eq!(rd_action[0].as_str(), Some("SetBallRadius"));
    assert_eq!(rd_action[1], PyRep::Tuple(vec![PyRep::Integer(100_000_001), PyRep::Real(150.0)]));

    // 5. CloakBall: ("CloakBall", (ball_id, cloaked))
    let cloak_rep = encode_destiny_event(19, &DestinyEvent::CloakBall { ball_id: 100_000_001, cloaked: 1 }, Some(&park));
    let PyRep::Tuple(ck_outer) = &cloak_rep else { panic!("Expected tuple") };
    let PyRep::Tuple(ck_action) = &ck_outer[1] else { panic!("Expected action tuple") };
    assert_eq!(ck_action[0].as_str(), Some("CloakBall"));
    assert_eq!(ck_action[1], PyRep::Tuple(vec![PyRep::Integer(100_000_001), PyRep::Bool(true)]));

    // 6. PackagedAction: ("PackagedAction", bytes)
    let pkg_bytes = vec![1, 2, 3, 4, 5];
    let pkg_rep = encode_destiny_event(20, &DestinyEvent::PackagedAction { actions_bytes: pkg_bytes.clone() }, Some(&park));
    let PyRep::Tuple(pk_outer) = &pkg_rep else { panic!("Expected tuple") };
    let PyRep::Tuple(pk_action) = &pk_outer[1] else { panic!("Expected action tuple") };
    assert_eq!(pk_action[0].as_str(), Some("PackagedAction"));
    assert_eq!(pk_action[1], PyRep::Bytes(pkg_bytes));

    // 7. Destruction & TerminalPlayDestructionEffect
    let mut combat = CombatState::new();
    combat.shield_hp = 10.0;
    combat.armor_hp = 10.0;
    combat.structure_hp = 10.0;
    let (_, destroyed) = combat.apply_damage(DamageVector::new(500.0, 500.0, 500.0, 500.0));
    assert!(destroyed);

    let term_rep = encode_destiny_event(21, &DestinyEvent::TerminalExplosion { ball_id: 100_000_001, destruction_effect_id: None }, Some(&park));
    let PyRep::Tuple(tm_outer) = &term_rep else { panic!("Expected tuple") };
    let PyRep::Tuple(tm_action) = &tm_outer[1] else { panic!("Expected action tuple") };
    assert_eq!(tm_action[0].as_str(), Some("TerminalPlayDestructionEffect"));
    assert_eq!(tm_action[1], PyRep::Tuple(vec![PyRep::Integer(100_000_001), PyRep::None]));
}

// ============================================================================
// Test 2: Flight Steering, Navigation & Warp Trajectories (8 rows)
// ============================================================================
#[tokio::test]
async fn test_destiny_steering_navigation_and_warp() {
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
    let mut park = Ballpark::new(30_000_142);
    let ship_id = 100_000_030;
    let ship = make_test_ship(ship_id, DVec3::ZERO, MovementMode::Stop);
    park.add_ball(ship);

    // 1. OnClientControllerEvent & SetControllerVariableOnBall
    let cce_rep = encode_destiny_event(40, &DestinyEvent::OnClientControllerEvent {
        ball_id: ship_id,
        target_id: Some(200_000_001),
        event_name: "res_wars_power_up_play".to_string(),
        value: 1,
        delay: Some(0.25),
    }, Some(&park));
    let PyRep::Tuple(cce_outer) = &cce_rep else { panic!("Expected tuple") };
    let PyRep::Tuple(cce_action) = &cce_outer[1] else { panic!("Expected action tuple") };
    assert_eq!(cce_action[0].as_str(), Some("OnClientControllerEvent"));

    let scv_rep = encode_destiny_event(41, &DestinyEvent::SetControllerVariableOnBall {
        ball_id: ship_id,
        var_name: "emanationLock".to_string(),
        value: 1,
    }, Some(&park));
    let PyRep::Tuple(scv_outer) = &scv_rep else { panic!("Expected tuple") };
    let PyRep::Tuple(scv_action) = &scv_outer[1] else { panic!("Expected action tuple") };
    assert_eq!(scv_action[0].as_str(), Some("SetControllerVariableOnBall"));
    assert_eq!(scv_action[1], PyRep::Tuple(vec![PyRep::Integer(ship_id), PyRep::String("emanationLock".into()), PyRep::Integer(1)]));

    // 2. OnDamageStateChange & OnFleetDamageStateChange
    let dsc_rep = encode_destiny_event(42, &DestinyEvent::OnDamageStateChange {
        ball_id: ship_id,
        shield_ratio: 0.75,
        armor_ratio: 1.0,
        structure_ratio: 1.0,
    }, Some(&park));
    let PyRep::Tuple(dsc_outer) = &dsc_rep else { panic!("Expected tuple") };
    let PyRep::Tuple(dsc_action) = &dsc_outer[1] else { panic!("Expected action tuple") };
    assert_eq!(dsc_action[0].as_str(), Some("OnDamageStateChange"));

    let fdsc_rep = encode_destiny_event(43, &DestinyEvent::OnFleetDamageStateChange {
        ball_id: ship_id,
        shield_ratio: 0.50,
        armor_ratio: 0.90,
        structure_ratio: 1.0,
    }, Some(&park));
    let PyRep::Tuple(fdsc_outer) = &fdsc_rep else { panic!("Expected tuple") };
    let PyRep::Tuple(fdsc_action) = &fdsc_outer[1] else { panic!("Expected action tuple") };
    assert_eq!(fdsc_action[0].as_str(), Some("OnFleetDamageStateChange"));

    // 3. OnFleetStateChange
    let fsc_rep = encode_destiny_event(44, &DestinyEvent::OnFleetStateChange {
        fleet_id: 99_000_100,
        member_id: ship_id,
        fleet_state: 2,
    }, Some(&park));
    let PyRep::Tuple(fsc_outer) = &fsc_rep else { panic!("Expected tuple") };
    let PyRep::Tuple(fsc_action) = &fsc_outer[1] else { panic!("Expected action tuple") };
    assert_eq!(fsc_action[0].as_str(), Some("OnFleetStateChange"));
    assert_eq!(fsc_action[1], PyRep::Tuple(vec![PyRep::Integer(99_000_100), PyRep::Integer(ship_id), PyRep::Integer(2)]));

    // 4. OnShipStateUpdate
    let ssu_rep = encode_destiny_event(45, &DestinyEvent::OnShipStateUpdate {
        module_states: vec![(90_000_001, 12_345, 100, 133_000_000_000, 1.0, 0.0)],
    }, Some(&park));
    let PyRep::Tuple(ssu_outer) = &ssu_rep else { panic!("Expected tuple") };
    let PyRep::Tuple(ssu_action) = &ssu_outer[1] else { panic!("Expected action tuple") };
    assert_eq!(ssu_action[0].as_str(), Some("OnShipStateUpdate"));

    // 5. OnSlimItemChange
    let sic_rep = encode_destiny_event(46, &DestinyEvent::OnSlimItemChange { ball_id: ship_id }, Some(&park));
    let PyRep::Tuple(sic_outer) = &sic_rep else { panic!("Expected tuple") };
    let PyRep::Tuple(sic_action) = &sic_outer[1] else { panic!("Expected action tuple") };
    assert_eq!(sic_action[0].as_str(), Some("OnSlimItemChange"));

    // 6. OnSovereigntyChanged
    let sov_rep = encode_destiny_event(47, &DestinyEvent::OnSovereigntyChanged {
        solar_system_id: 30_000_142,
        alliance_id: 99_000_001,
    }, Some(&park));
    let PyRep::Tuple(sov_outer) = &sov_rep else { panic!("Expected tuple") };
    let PyRep::Tuple(sov_action) = &sov_outer[1] else { panic!("Expected action tuple") };
    assert_eq!(sov_action[0].as_str(), Some("OnSovereigntyChanged"));
    assert_eq!(sov_action[1], PyRep::Tuple(vec![PyRep::Integer(30_000_142), PyRep::Integer(99_000_001)]));

    // 7. OnSpecialFX
    let fx_rep = encode_destiny_event(48, &DestinyEvent::OnSpecialFX {
        ball_id: ship_id,
        module_id: Some(54_321),
        guid: "effects.Laser".to_string(),
        is_active: true,
    }, Some(&park));
    let PyRep::Tuple(fx_outer) = &fx_rep else { panic!("Expected tuple") };
    let PyRep::Tuple(fx_action) = &fx_outer[1] else { panic!("Expected action tuple") };
    assert_eq!(fx_action[0].as_str(), Some("OnSpecialFX"));

    // 8. OnAudioActivated
    let audio_rep = encode_destiny_event(49, &DestinyEvent::OnAudioActivated, Some(&park));
    let PyRep::Tuple(au_outer) = &audio_rep else { panic!("Expected tuple") };
    let PyRep::Tuple(au_action) = &au_outer[1] else { panic!("Expected action tuple") };
    assert_eq!(au_action[0].as_str(), Some("OnAudioActivated"));
    assert_eq!(au_action[1], PyRep::Tuple(Vec::new()));

    // 9. OnDockingAccepted
    let dock_rep = encode_destiny_event(50, &DestinyEvent::OnDockingAccepted {
        start_pos: [100.0, 200.0, 300.0],
        end_pos: [0.0, 0.0, 0.0],
        station_id: 60_003_760,
    }, Some(&park));
    let PyRep::Tuple(dk_outer) = &dock_rep else { panic!("Expected tuple") };
    let PyRep::Tuple(dk_action) = &dk_outer[1] else { panic!("Expected action tuple") };
    assert_eq!(dk_action[0].as_str(), Some("OnDockingAccepted"));

    // 10. ScatterEwars
    let ewar_data = vec![0xDE, 0xAD, 0xBE, 0xEF];
    let ew_rep = encode_destiny_event(51, &DestinyEvent::ScatterEwars {
        ball_id: ship_id,
        effect_states: ewar_data.clone(),
    }, Some(&park));
    let PyRep::Tuple(ew_outer) = &ew_rep else { panic!("Expected tuple") };
    let PyRep::Tuple(ew_action) = &ew_outer[1] else { panic!("Expected action tuple") };
    assert_eq!(ew_action[0].as_str(), Some("ScatterEwars"));
    assert_eq!(ew_action[1], PyRep::Tuple(vec![PyRep::Integer(ship_id), PyRep::Bytes(ewar_data)]));
}

// ============================================================================
// Test 4: Dogma Buffs, Damage Over Time & Drone Network Events (6 rows)
// ============================================================================
#[tokio::test]
async fn test_destiny_dogma_buffs_dot_and_drone_events() {
    let park = Ballpark::new(30_000_142);
    let drone_id = 50_000_001;

    // 1. OnDbuffUpdated: ("OnDbuffUpdated", (ball_id, [(id, (val, exp))]))
    let dbuffs = vec![(101, 1.25, 100), (102, 0.85, 200)];
    let db_rep = encode_destiny_event(60, &DestinyEvent::OnDbuffUpdated { ball_id: 100_000_001, dbuffs }, Some(&park));
    let PyRep::Tuple(db_outer) = &db_rep else { panic!("Expected tuple") };
    let PyRep::Tuple(db_action) = &db_outer[1] else { panic!("Expected action tuple") };
    assert_eq!(db_action[0].as_str(), Some("OnDbuffUpdated"));

    // 2. OnDotVictimUpdated: ("OnDotVictimUpdated", (ball_id, [apps]))
    let dot_rep = encode_destiny_event(61, &DestinyEvent::OnDotVictimUpdated {
        ball_id: 100_000_001,
        flat_dps: 150.0,
        pct_dps: 0.05,
        remaining_ticks: 10,
    }, Some(&park));
    let PyRep::Tuple(dot_outer) = &dot_rep else { panic!("Expected tuple") };
    let PyRep::Tuple(dot_action) = &dot_outer[1] else { panic!("Expected action tuple") };
    assert_eq!(dot_action[0].as_str(), Some("OnDotVictimUpdated"));

    // 3. OnDroneStateChange: ("OnDroneStateChange", (drone_id, owner, controller, state, type_id, ctrl_owner, target))
    let dsc_rep = encode_destiny_event(62, &DestinyEvent::OnDroneStateChange {
        drone_id,
        owner_id: 90_000_001,
        controller_id: 100_000_001,
        activity_state: 1, // Combat
        type_id: 282,      // Hobgoblin I
        controller_owner_id: 90_000_001,
        target_id: Some(100_000_002),
    }, Some(&park));
    let PyRep::Tuple(dsc_outer) = &dsc_rep else { panic!("Expected tuple") };
    let PyRep::Tuple(dsc_action) = &dsc_outer[1] else { panic!("Expected action tuple") };
    assert_eq!(dsc_action[0].as_str(), Some("OnDroneStateChange"));

    // 4. OnDroneStateChange2: ("OnDroneStateChange2", (drone_id, owner, controller, state, type_id, ctrl_owner, target))
    let dsc2_rep = encode_destiny_event(63, &DestinyEvent::OnDroneStateChange2 {
        drone_id,
        owner_id: 90_000_001,
        controller_id: 100_000_001,
        activity_state: 2, // Orbiting
        type_id: 282,
        controller_owner_id: 90_000_001,
        target_id: None,
    }, Some(&park));
    let PyRep::Tuple(dsc2_outer) = &dsc2_rep else { panic!("Expected tuple") };
    let PyRep::Tuple(dsc2_action) = &dsc2_outer[1] else { panic!("Expected action tuple") };
    assert_eq!(dsc2_action[0].as_str(), Some("OnDroneStateChange2"));

    // 5. OnDroneActivityChange: ("OnDroneActivityChange", (drone_id, activity_state))
    let dac_rep = encode_destiny_event(64, &DestinyEvent::OnDroneActivityChange {
        drone_id,
        activity_state: 3, // Returning
    }, Some(&park));
    let PyRep::Tuple(dac_outer) = &dac_rep else { panic!("Expected tuple") };
    let PyRep::Tuple(dac_action) = &dac_outer[1] else { panic!("Expected action tuple") };
    assert_eq!(dac_action[0].as_str(), Some("OnDroneActivityChange"));
    assert_eq!(dac_action[1], PyRep::Tuple(vec![PyRep::Integer(drone_id), PyRep::Integer(3)]));

    // 6. OnDroneControlLost: ("OnDroneControlLost", (drone_id,))
    let dcl_rep = encode_destiny_event(65, &DestinyEvent::OnDroneControlLost { drone_id }, Some(&park));
    let PyRep::Tuple(dcl_outer) = &dcl_rep else { panic!("Expected tuple") };
    let PyRep::Tuple(dcl_action) = &dcl_outer[1] else { panic!("Expected action tuple") };
    assert_eq!(dcl_action[0].as_str(), Some("OnDroneControlLost"));
    assert_eq!(dcl_action[1], PyRep::Tuple(vec![PyRep::Integer(drone_id)]));
}

// ============================================================================
// Test 5: All 13 Ball Physics Modes Simulation & Wire Roundtrip (13 rows)
// ============================================================================
#[tokio::test]
async fn test_destiny_all_13_ball_physics_modes_and_wire_roundtrip() {
    let mut park = Ballpark::new(30_000_142);
    let anchor_id = 999_999;
    let anchor = make_test_ship(anchor_id, DVec3::ZERO, MovementMode::Stop);
    park.add_ball(anchor);

    // Instantiate 13 balls covering all 13 DSTBALL modes with isolated spatial positions:
    let balls = vec![
        make_test_ship(1, DVec3::new(100_000.0, 0.0, 0.0), MovementMode::GotoDirection { direction: DVec3::new(1.0, 0.0, 0.0) }),
        make_test_ship(2, DVec3::new(5_000.0, 0.0, 0.0), MovementMode::Follow { target_id: anchor_id, range: 1_500.0 }),
        make_test_ship(3, DVec3::new(200_000.0, 0.0, 0.0), MovementMode::Stop),
        make_test_ship(4, DVec3::new(0.0, 300_000.0, 0.0), MovementMode::new_warp(DVec3::new(0.0, 300_000.0, 0.0), DVec3::new(0.0, 5.0 * AU_METERS, 0.0), 3.0)),
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

    // Step physics loop dt = 1.0s and verify physics behavior per mode
    park.step(1.0);

    // 1. DSTBALL_GOTO: ship moved forward along direction
    assert!(park.balls[&1].position.x > 100_000.0, "GotoDirection advances along vector");

    // 2. DSTBALL_FOLLOW: follower advanced towards anchor
    assert!(park.balls[&2].position.x < 5_000.0, "Follower closes distance to anchor");

    // 3. DSTBALL_STOP: stop mode maintains near-zero velocity
    assert!(park.balls[&3].velocity.length() < 1e-3, "Stop mode remains stopped");

    // 4. DSTBALL_WARP: ship progresses in warp
    assert!(matches!(park.balls[&4].mode, MovementMode::Warp { .. }), "Warp ball is in warp trajectory");

    // 5. DSTBALL_ORBIT: orbit mode maintains orbital speed around anchor
    assert!(park.balls[&5].velocity.length() > 0.0, "Orbit mode has active tangential velocity");

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
