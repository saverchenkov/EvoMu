//! Integration tests for Destiny Binary Stream Completeness (`SP-6`, `Z-3`)
//! and Continuous Warp Physics curves, visual handoff, bubble interception, and scrambling.

#![forbid(unsafe_code)]

// parity: feat:navigation.warp-speed-formula

use eve_destiny::wire::{
    decode_add_balls, encode_add_balls, DST_BALL_FORMATION, DST_BALL_MISSILE, DST_BALL_MUSHROOM,
    DST_BALL_TROLL,
};
use eve_destiny::{
    Ball, Ballpark, DestinyEvent, MovementMode, WarpPhase, AU_METERS,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

fn sample_test_ball(id: i64, mode: MovementMode) -> Ball {
    Ball {
        id,
        type_id: 587,
        position: DVec3::new(1000.0, 2000.0, 3000.0),
        velocity: DVec3::new(10.0, 20.0, 30.0),
        max_velocity: 365.0,
        inertia: 3.45,
        mass: 1_067_000.0,
        radius: 37.0,
        speed_fraction: 1.0,
        mode,
    }
}

// Wire-layout regression only (AddBalls encode/decode); the simulation is never stepped, so this
// test does not close the ballmode rows (review 20261020-194722, R2-P1-1).
#[test]
fn test_destiny_binary_mode_missile_mushroom_troll_formation() {
    let mut park = Ballpark::new(30_000_142);

    // 1. Missile mode: MISSILE_Struct (48 bytes payload: target ID, speed, explosion params, flight time)
    // Total mode sector: 1 byte formationID + 48 bytes payload = 49 bytes. Total ball = 148 bytes.
    let missile_ball = sample_test_ball(101, MovementMode::Missile { target_id: 9001 });

    // 2. Mushroom mode: MUSHROOM_Struct (24 bytes payload)
    // Total mode sector: 1 byte formationID + 24 bytes payload = 25 bytes. Total ball = 124 bytes.
    let mushroom_ball = sample_test_ball(102, MovementMode::Mushroom);

    // 3. Troll mode: TROLL_Struct (4 bytes payload)
    // Total mode sector: 1 byte formationID + 4 bytes payload = 5 bytes. Total ball = 104 bytes.
    let troll_ball = sample_test_ball(103, MovementMode::Troll);

    // 4. Formation mode: FORMATION_Struct (16 bytes payload: formation ID, layout parameters)
    // Total mode sector: 1 byte formationID + 16 bytes payload = 17 bytes. Total ball = 116 bytes.
    let formation_ball = sample_test_ball(104, MovementMode::Formation);

    park.add_ball(missile_ball.clone());
    park.add_ball(mushroom_ball.clone());
    park.add_ball(troll_ball.clone());
    park.add_ball(formation_ball.clone());

    let balls = [&missile_ball, &mushroom_ball, &troll_ball, &formation_ball];
    let stamp = 42u32;
    let encoded = encode_add_balls(stamp, &balls);

    // Verify 5-byte header: packet_type = 0, stamp = 42
    assert_eq!(encoded[0], 0);
    assert_eq!(u32::from_le_bytes(encoded[1..5].try_into().unwrap()), 42);

    // Total length: 5 header + 148 (Missile) + 124 (Mushroom) + 104 (Troll) + 116 (Formation) = 497 bytes
    assert_eq!(encoded.len(), 5 + 148 + 124 + 104 + 116);

    // Verify slice bounds for each mode matching DestinyStructs.h exactly
    let missile_slice = &encoded[5..5 + 148];
    assert_eq!(missile_slice[8], DST_BALL_MISSILE);
    // Payload targetID at offset 100..108 (after 38 header + 25 mass + 36 data + 1 formationID = 100)
    assert_eq!(
        i64::from_le_bytes(missile_slice[100..108].try_into().unwrap()),
        9001
    );

    let mushroom_slice = &encoded[5 + 148..5 + 148 + 124];
    assert_eq!(mushroom_slice[8], DST_BALL_MUSHROOM);
    assert_eq!(mushroom_slice.len(), 124);

    let troll_slice = &encoded[5 + 148 + 124..5 + 148 + 124 + 104];
    assert_eq!(troll_slice[8], DST_BALL_TROLL);
    assert_eq!(troll_slice.len(), 104);

    let form_slice = &encoded[5 + 148 + 124 + 104..];
    assert_eq!(form_slice[8], DST_BALL_FORMATION);
    assert_eq!(form_slice.len(), 116);

    // Decode and verify all fields roundtrip
    let (pkt_type, dec_stamp, decoded) = decode_add_balls(&encoded).expect("decode_add_balls");
    assert_eq!(pkt_type, 0);
    assert_eq!(dec_stamp, 42);
    assert_eq!(decoded.len(), 4);

    // Missile
    let dec_missile = &decoded[0];
    assert_eq!(dec_missile.id, 101);
    assert_eq!(dec_missile.mode, DST_BALL_MISSILE);
    assert_eq!(dec_missile.target_id, Some(9001));
    assert_eq!(dec_missile.owner_id, Some(1));
    assert_eq!(dec_missile.follow_range, Some(0.0));
    assert_eq!(dec_missile.effect_stamp, Some(42));
    assert_eq!(dec_missile.direction, Some(DVec3::new(10.0, 20.0, 30.0)));

    // Mushroom
    let dec_mushroom = &decoded[1];
    assert_eq!(dec_mushroom.id, 102);
    assert_eq!(dec_mushroom.mode, DST_BALL_MUSHROOM);
    assert_eq!(dec_mushroom.follow_range, Some(37.0));
    assert_eq!(dec_mushroom.owner_id, Some(1));
    assert_eq!(dec_mushroom.effect_stamp, Some(42));

    // Troll
    let dec_troll = &decoded[2];
    assert_eq!(dec_troll.id, 103);
    assert_eq!(dec_troll.mode, DST_BALL_TROLL);
    assert_eq!(dec_troll.effect_stamp, Some(42));

    // Formation
    let dec_form = &decoded[3];
    assert_eq!(dec_form.id, 104);
    assert_eq!(dec_form.mode, DST_BALL_FORMATION);
    assert_eq!(dec_form.target_id, Some(0));
    assert_eq!(dec_form.follow_range, Some(0.0));
    assert_eq!(dec_form.effect_stamp, Some(42));
}

// parity: feat:navigation.warp-speed-formula
#[test]
fn test_warp_continuous_physics_curves_and_visual_handoff() {
    let mut park = Ballpark::new(30_000_142);
    let warp_speed_au = 3.0;
    let origin = DVec3::ZERO;
    let distance = 20.0 * AU_METERS; // 20 AU long warp
    let destination = DVec3::new(distance, 0.0, 0.0);

    let ship = Ball {
        id: 2001,
        type_id: 587,
        position: origin,
        velocity: DVec3::ZERO,
        max_velocity: 365.0,
        inertia: 3.45,
        mass: 1_067_000.0,
        radius: 37.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    park.add_ball(ship);

    // Initiate warp
    park.set_mode(
        2001,
        MovementMode::new_warp(origin, destination, warp_speed_au),
    )
    .expect("set warp mode");

    // Check tunnel visual handoff events emitted on warp initiation
    let init_events = park.drain_all_observer_events();
    let has_warp_to = init_events
        .values()
        .flatten()
        .any(|e| matches!(e, DestinyEvent::WarpTo { ball_id: 2001, .. }));
    assert!(has_warp_to, "Must emit DestinyEvent::WarpTo");

    let has_act_warp = park.pending_events.iter().any(|e| {
        matches!(e, DestinyEvent::OnActivatingWarp { ball_id: 2001 })
    });
    assert!(has_act_warp, "Must emit DestinyEvent::OnActivatingWarp");

    let has_sfx_start = park.pending_events.iter().any(|e| {
        matches!(
            e,
            DestinyEvent::OnSpecialFX {
                ball_id: 2001,
                guid,
                is_active: true,
                ..
            } if guid == "effects.Warping"
        )
    });
    assert!(has_sfx_start, "Must emit OnSpecialFX for WarpDrive activation");

    // Clear pending events for step monitoring
    park.pending_events.clear();

    // Step physics and monitor continuous acceleration, cruise, deceleration
    let mut saw_acceleration = false;
    let mut saw_cruise = false;
    let mut saw_deceleration = false;
    let mut prev_velocity = 0.0f64;
    let mut max_speed = 0.0f64;

    for tick in 1..=80 {
        let events = park.step(1.0);
        let ball = park.balls.get(&2001).unwrap();
        let speed = ball.velocity.length();
        max_speed = max_speed.max(speed);

        match &ball.mode {
            MovementMode::Warp { phase, .. } => match phase {
                WarpPhase::Aligning => {}
                WarpPhase::Accelerating => {
                    saw_acceleration = true;
                    if prev_velocity > 0.0 && speed > 1000.0 {
                        // In exponential acceleration, speed must strictly increase
                        assert!(
                            speed >= prev_velocity,
                            "Speed must monotonically increase during acceleration (tick {tick}: {speed} >= {prev_velocity})"
                        );
                    }
                }
                WarpPhase::Cruising => {
                    saw_cruise = true;
                    let cruise_expected = warp_speed_au * AU_METERS;
                    let diff_ratio = (speed - cruise_expected).abs() / cruise_expected;
                    assert!(
                        diff_ratio < 0.05,
                        "Cruising speed must match warp_speed_au_s * AU_METERS: got {speed}, expected {cruise_expected}"
                    );
                }
                WarpPhase::Decelerating => {
                    saw_deceleration = true;
                }
            },
            MovementMode::Stop => {
                // Landed at destination
                let dist_from_dest = (ball.position - destination).length();
                assert!(
                    dist_from_dest <= 2_500.0,
                    "Ship overshot destination: remaining distance {dist_from_dest} m (must be <= 2,500m)"
                );

                // Verify visual handoff on deactivating warp
                let has_deact = events
                    .iter()
                    .any(|e| matches!(e, DestinyEvent::OnDeactivatingWarp { ball_id: 2001 }));
                assert!(has_deact, "Must emit OnDeactivatingWarp on dropping out of warp");

                let has_sfx_end = events.iter().any(|e| {
                    matches!(
                        e,
                        DestinyEvent::OnSpecialFX {
                            ball_id: 2001,
                            guid,
                            is_active: false,
                            ..
                        } if guid == "effects.Warping"
                    )
                });
                assert!(has_sfx_end, "Must emit OnSpecialFX with is_active = false on warp exit");
                break;
            }
            _ => {}
        }
        prev_velocity = speed;
    }

    assert!(saw_acceleration, "Warp must exhibit continuous exponential acceleration phase");
    assert!(saw_cruise, "Long warp must exhibit cruise phase at ship warp speed");
    assert!(saw_deceleration, "Warp must exhibit deceleration phase without overshoot");
    assert!(max_speed > 1.0e9, "Warp must reach relativistic AU/s velocities");

    // Verify exact EVE warp acceleration, cruise, and deceleration duration formulas across ship classes:
    // - Stiletto (11198, Interceptor, 8.0 AU/s): k_a = 8.0, k_d = min(8.0 / 3.0, 2.0) = 2.0 (capped at 2.0!)
    // - Rifter (587, Frigate, 5.0 AU/s): k_a = 5.0, k_d = 5.0 / 3.0
    // - Caracal (621, Cruiser, 4.0 AU/s): k_a = 4.0, k_d = 4.0 / 3.0
    // - Raven (638, Battleship, 3.0 AU/s): k_a = 3.0, k_d = 1.0
    // - Charon (20185, Freighter, 1.5 AU/s): k_a = 1.5, k_d = 0.5
    let class_cases = [
        ("Stiletto", 11198u32, 8.0f64, 2.0f64),
        ("Rifter", 587, 5.0f64, 5.0 / 3.0),
        ("Caracal", 621, 4.0f64, 4.0 / 3.0),
        ("Raven", 638, 3.0f64, 1.0f64),
        ("Charon", 20185, 1.5f64, 0.5f64),
    ];
    let mut prev_total_time = 0.0f64;
    for (name, ship_type_id, k, expected_kd) in class_cases {
        let kd = (k / 3.0).min(2.0);
        assert!(
            (kd - expected_kd).abs() < 1e-9,
            "{name} deceleration constant k_d mismatch: got {kd}, expected {expected_kd}"
        );
        let mut class_park = Ballpark::new(30_000_142);
        let test_ball = Ball {
            id: 2100 + ship_type_id as i64,
            type_id: ship_type_id,
            position: origin,
            velocity: DVec3::new(300.0, 0.0, 0.0), // already aligned
            max_velocity: 300.0,
            inertia: 2.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 1.0,
            mode: MovementMode::Stop,
        };
        let bid = test_ball.id;
        class_park.add_ball(test_ball);
        class_park
            .set_mode(bid, MovementMode::new_warp(origin, destination, k))
            .expect("set warp");

        let mut total_t = 0.0f64;
        let mut decel_t = 0.0f64;
        for _ in 0..1000 {
            class_park.step(0.1);
            total_t += 0.1;
            let b = class_park.balls.get(&bid).unwrap();
            match b.mode {
                MovementMode::Warp {
                    phase: WarpPhase::Decelerating,
                    ..
                } => {
                    decel_t += 0.1;
                }
                MovementMode::Stop => break,
                _ => {}
            }
        }
        assert!(
            decel_t > 5.0,
            "{name} must spend significant time in exponential deceleration ({decel_t:.1}s)"
        );
        if prev_total_time > 0.0 {
            assert!(
                total_t > prev_total_time,
                "Slower ship class {name} ({k} AU/s) must take strictly longer to warp 20 AU than faster class ({total_t:.1}s vs {prev_total_time:.1}s)"
            );
        }
        prev_total_time = total_t;
    }
}

// parity: feat:navigation.warp-speed-formula
#[tokio::test]
async fn test_server_cmd_warp_to_stuff_uses_sde_ship_warp_speed() {
    // parity: rpc:authentication.Login, rpc:beyonce.CmdWarpToStuff, rpc:charUnboundMgr.SelectCharacterID, rpc:stationSvc.Undock
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(9900);

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

    let undock = ctx
        .dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock in Jita");
    let PyRep::KeyVal(undock_kv) = &undock else { panic!() };
    let beyonce_id = undock_kv["beyonceBoundID"].as_i64().unwrap() as u64;
    let ship_id = undock_kv["shipID"].as_i64().unwrap();
    let jita_handle = ctx.systems.get_or_spawn(30_000_142);
    let dest_gate_id = 50_001_249i64;

    // Test Stiletto (11198 -> 8.0 AU/s), Rifter (587 -> 5.0 AU/s), Raven (638 -> 3.0 AU/s), Charon (20185 -> 1.5 AU/s)
    for (ship_type_id, expected_au_s) in [(11198u32, 8.0f64), (587, 5.0), (638, 3.0), (20185, 1.5)] {
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ship_item) = snap.items.get_mut(&ship_id) {
                ship_item.type_id = ship_type_id;
            }
        });
        ctx.invalidate_ship_fitting(ship_id);
        let (_, mut ball) = jita_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
        ball.type_id = ship_type_id;
        ball.position = DVec3::ZERO;
        ball.velocity = DVec3::ZERO;
        ball.mode = MovementMode::Stop;
        jita_handle.spawn_ball(ball).await.unwrap();

        ctx.dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(beyonce_id),
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(dest_gate_id)],
            &kw,
            None,
        )
        .await
        .expect("CmdWarpToStuff");

        let (_, active_ball) = jita_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
        let MovementMode::Warp { warp_speed_au_s, .. } = active_ball.mode else {
            panic!("Expected ship {ship_type_id} in Warp mode");
        };
        assert!(
            (warp_speed_au_s - expected_au_s).abs() < 1e-4,
            "Ship type {ship_type_id} must warp at SDE warp speed {expected_au_s} AU/s, got {warp_speed_au_s}"
        );
    }
}

fn seed_fitting_item(
    ctx: &ServerContext,
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: eve_db::InventoryFlag,
    quantity: i32,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_id,
            eve_db::InventoryItemRecord {
                item_id,
                type_id,
                owner_id,
                location_id,
                flag,
                quantity,
                singleton: quantity == 1,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
}

fn grant_fitting_skills(ctx: &ServerContext, char_id: i64) {
    ctx.db.with_mut_snapshot(|snap| {
        let entry = snap.skills.entry(char_id).or_default();
        for &skill_id in &[
            3402u32, // Science
            3412,    // Astrometrics
            3413,    // Power Grid Management
            3420,    // Tactical Shield Manipulation
            3426,    // CPU Management
            3429,    // Target Management
            3430,    // Advanced Target Management
            3435,    // Propulsion Jamming
            3449,    // Navigation
            3450,    // Afterburner
            3454,    // High Speed Maneuvering
            3455,    // Warp Drive Operation
            11446,   // Graviton Physics
            28609,   // Heavy Interdiction Cruisers
        ] {
            entry.insert(
                skill_id,
                eve_db::TrainedSkillRecord {
                    type_id: skill_id,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }
    });
}

// parity: feat:modules.warp-disruptor
#[tokio::test]
async fn test_warp_scrambler_disruptor_blocking() {
    // parity: rpc:authentication.Login, rpc:beyonce.CmdWarpToStuff, rpc:charUnboundMgr.SelectCharacterID, rpc:dogmaIM.Activate, rpc:dogmaIM.AddTarget, rpc:dogmaIM.Deactivate, rpc:stationSvc.Undock
    let ctx = ServerContext::new();
    let kw = HashMap::new();

    // Pilot 1 (Victim in Rifter 100_000_001)
    let mut sess_victim = SessionState::new(9901);
    ctx.dispatch_rpc(
        &mut sess_victim,
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
    .expect("Login victim");
    ctx.dispatch_rpc(
        &mut sess_victim,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID victim");

    // Pilot 2 (Tackler in Rifter)
    let mut sess_tackler = SessionState::new(9902);
    ctx.dispatch_rpc(
        &mut sess_tackler,
        "authentication",
        None,
        "Login",
        &[
            PyRep::String("other".into()),
            PyRep::String("other-secret".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Login tackler");
    ctx.dispatch_rpc(
        &mut sess_tackler,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_002)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID tackler");

    let victim_char_id = 90_000_001i64;
    let victim_ship_id = sess_victim.ship_id.expect("victim ship_id");
    let tackler_char_id = 90_000_002i64;
    let tackler_ship_id = sess_tackler.ship_id.expect("tackler ship_id");

    // Clear any pre-seeded modules on both ships so slots and CPU/PG are clean
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, i| {
            !(i.location_id == victim_ship_id || i.location_id == tackler_ship_id)
        });
    });

    grant_fitting_skills(&ctx, victim_char_id);
    grant_fitting_skills(&ctx, tackler_char_id);

    // Fit 5MN Microwarpdrive I (type 434) and Warp Core Stabilizer I (type 10998) on Victim
    let victim_mwd_id = 880_001i64;
    let victim_wcs_id = 880_002i64;
    seed_fitting_item(
        &ctx,
        victim_mwd_id,
        434,
        victim_char_id,
        victim_ship_id,
        eve_db::InventoryFlag::MedSlot0,
        1,
    );
    seed_fitting_item(
        &ctx,
        victim_wcs_id,
        10998,
        victim_char_id,
        victim_ship_id,
        eve_db::InventoryFlag::LoSlot0,
        1,
    );
    ctx.invalidate_ship_fitting(victim_ship_id);

    // Fit Warp Disruptor I (type 3242, range 20km, strength 1) and Warp Scrambler I (type 447, range 7.5km, strength 2, blocks MWD/MJD) on Tackler
    let disruptor_id = 880_003i64;
    let scrambler_id = 880_004i64;
    seed_fitting_item(
        &ctx,
        disruptor_id,
        3242,
        tackler_char_id,
        tackler_ship_id,
        eve_db::InventoryFlag::MedSlot0,
        1,
    );
    seed_fitting_item(
        &ctx,
        scrambler_id,
        447,
        tackler_char_id,
        tackler_ship_id,
        eve_db::InventoryFlag::MedSlot1,
        1,
    );
    ctx.invalidate_ship_fitting(tackler_ship_id);

    // Both pilots undock into Jita (30000142)
    let undock_v = ctx
        .dispatch_rpc(&mut sess_victim, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock victim");
    let PyRep::KeyVal(uv_kv) = &undock_v else { panic!() };
    let beyonce_victim = uv_kv["beyonceBoundID"].as_i64().unwrap() as u64;

    let _undock_t = ctx
        .dispatch_rpc(&mut sess_tackler, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock tackler");

    let jita_handle = ctx.systems.get_or_spawn(30_000_142);
    let dest_x = 50_001_249i64; // Perimeter stargate

    // Position victim at (0, 0, 0) and tackler at (22,000, 0, 0) -> within targeting range (27.5km) but out of 20km disruptor range
    let _ = jita_handle
        .set_ball_position(victim_ship_id, DVec3::ZERO)
        .await;
    let _ = jita_handle
        .set_ball_position(tackler_ship_id, DVec3::new(22_000.0, 0.0, 0.0))
        .await;

    // Lock victim via dogmaIM.AddTarget and step 10s so lock completes
    ctx.dispatch_rpc(
        &mut sess_tackler,
        "dogmaIM",
        None,
        "AddTarget",
        &[PyRep::Integer(victim_ship_id)],
        &kw,
        None,
    )
    .await
    .expect("AddTarget at 22km");
    jita_handle.step_manual(10.0).await.unwrap();

    // 1. Out of range (> 20km): activating Warp Disruptor I fails with TargetOutOfRange
    let oor_err = ctx
        .dispatch_rpc(
            &mut sess_tackler,
            "dogmaIM",
            None,
            "Activate",
            &[
                PyRep::Integer(disruptor_id),
                PyRep::String("warpScramble".into()),
                PyRep::Integer(victim_ship_id),
                PyRep::Integer(1),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("Warp Disruptor I must reject target beyond 20km");
    assert_eq!(oor_err.code, "TargetOutOfRange");

    // 2. Move tackler to 15km (within 20km Warp Disruptor I range, outside 7.5km Warp Scrambler I range)
    let _ = jita_handle
        .set_ball_position(tackler_ship_id, DVec3::new(15_000.0, 0.0, 0.0))
        .await;

    // Activate 5MN Microwarpdrive I on victim before being tackled
    ctx.dispatch_rpc(
        &mut sess_victim,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(victim_mwd_id),
            PyRep::String("moduleBonusMicrowarpdrive".into()),
            PyRep::None,
            PyRep::Integer(1),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate MWD on victim");

    // Activate Warp Disruptor I (strength 1) on tackler -> succeeds at 15km
    ctx.dispatch_rpc(
        &mut sess_tackler,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(disruptor_id),
            PyRep::String("warpScramble".into()),
            PyRep::Integer(victim_ship_id),
            PyRep::Integer(1),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate Warp Disruptor I at 15km");

    let cs_disrupted = jita_handle.get_combat_state(victim_ship_id).await.unwrap().unwrap();
    assert_eq!(cs_disrupted.warp_scramble_strength, 1);
    assert!(
        !cs_disrupted.is_mwd_mjd_disabled,
        "Long point (Warp Disruptor I) must NOT disable MWD/MJD"
    );

    // Victim warp attempt fails with WarpScrambled (strength 1 >= core strength 1)
    let scrambled_err = ctx
        .dispatch_rpc(
            &mut sess_victim,
            "beyonce",
            Some(beyonce_victim),
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(dest_x)],
            &kw,
            None,
        )
        .await
        .expect_err("Disrupted ship cannot warp");
    assert_eq!(scrambled_err.code, "WarpScrambled");

    // Warp Scrambler I at 15km fails with TargetOutOfRange (7.5km maxRange)
    let scram_oor = ctx
        .dispatch_rpc(
            &mut sess_tackler,
            "dogmaIM",
            None,
            "Activate",
            &[
                PyRep::Integer(scrambler_id),
                PyRep::String("warpScrambleBlockMWDWithNPCEffect".into()),
                PyRep::Integer(victim_ship_id),
                PyRep::Integer(1),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("Warp Scrambler I must reject target at 15km > 7.5km");
    assert_eq!(scram_oor.code, "TargetOutOfRange");

    // 3. Victim activates Warp Core Stabilizer I (type 10998, +2 warp core strength -> total 3)
    ctx.dispatch_rpc(
        &mut sess_victim,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(victim_wcs_id),
            PyRep::String("warpCoreFitted".into()),
            PyRep::None,
            PyRep::Integer(1),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate Warp Core Stabilizer I");

    let cs_stabilized = jita_handle.get_combat_state(victim_ship_id).await.unwrap().unwrap();
    assert_eq!(
        cs_stabilized.warp_core_strength, 3,
        "Warp Core Stabilizer I must add +2 warp core strength (total 3)"
    );

    // Victim can now warp through the 1-point Warp Disruptor I!
    let stabilized_ok = ctx
        .dispatch_rpc(
            &mut sess_victim,
            "beyonce",
            Some(beyonce_victim),
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(dest_x)],
            &kw,
            None,
        )
        .await;
    assert!(
        stabilized_ok.is_ok(),
        "Ship with 3 warp core strength can warp through 1 disruptor strength"
    );

    // Stop victim ship and move tackler within 5km (inside 7.5km Warp Scrambler I range)
    let _ = jita_handle.set_movement_mode(victim_ship_id, MovementMode::Stop).await;
    let _ = jita_handle
        .set_ball_position(tackler_ship_id, DVec3::new(5_000.0, 0.0, 0.0))
        .await;

    // 4. Tackler activates Warp Scrambler I (strength 2) alongside Warp Disruptor I (strength 1) -> total 3
    ctx.dispatch_rpc(
        &mut sess_tackler,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(scrambler_id),
            PyRep::String("warpScrambleBlockMWDWithNPCEffect".into()),
            PyRep::Integer(victim_ship_id),
            PyRep::Integer(1),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate Warp Scrambler I at 5km");

    let cs_scrammed = jita_handle.get_combat_state(victim_ship_id).await.unwrap().unwrap();
    assert_eq!(
        cs_scrammed.warp_scramble_strength, 3,
        "Combined Warp Disruptor I (1) + Warp Scrambler I (2) must apply 3 scramble strength"
    );
    assert!(
        cs_scrammed.is_mwd_mjd_disabled,
        "Warp Scrambler I must disable MWD and MJD on target"
    );

    // Reactivating MWD while scrambled must fail with WarpScrambled
    let mwd_blocked = ctx
        .dispatch_rpc(
            &mut sess_victim,
            "dogmaIM",
            None,
            "Activate",
            &[
                PyRep::Integer(victim_mwd_id),
                PyRep::String("moduleBonusMicrowarpdrive".into()),
                PyRep::None,
                PyRep::Integer(1),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("MWD activation must be blocked by Warp Scrambler I");
    assert_eq!(mwd_blocked.code, "WarpScrambled");

    // Victim warp fails with WarpScrambled (scramble 3 >= core strength 3)
    let heavy_scrambled_err = ctx
        .dispatch_rpc(
            &mut sess_victim,
            "beyonce",
            Some(beyonce_victim),
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(dest_x)],
            &kw,
            None,
        )
        .await
        .expect_err("3 scramble points must block ship with 3 warp core strength");
    assert_eq!(heavy_scrambled_err.code, "WarpScrambled");

    // 5. Deactivate Warp Scrambler I via dogmaIM.Deactivate -> scramble strength drops to 1 and MWD/MJD block clears
    ctx.dispatch_rpc(
        &mut sess_tackler,
        "dogmaIM",
        None,
        "Deactivate",
        &[
            PyRep::Integer(scrambler_id),
            PyRep::String("warpScrambleBlockMWDWithNPCEffect".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Deactivate Warp Scrambler I");

    let cs_after_deact = jita_handle.get_combat_state(victim_ship_id).await.unwrap().unwrap();
    assert_eq!(cs_after_deact.warp_scramble_strength, 1);
    assert!(!cs_after_deact.is_mwd_mjd_disabled);
}

// parity: feat:modules.warp-disruption-field
#[tokio::test]
async fn test_hic_warp_disruption_field_generator_bubble_and_focused_script() {
    // parity: rpc:authentication.Login, rpc:beyonce.CmdWarpToStuff, rpc:charUnboundMgr.SelectCharacterID, rpc:dogmaIM.Activate, rpc:dogmaIM.AddTarget, rpc:dogmaIM.Deactivate, rpc:dogmaIM.LoadAmmo, rpc:invbroker.Add, rpc:stationSvc.Undock
    let ctx = ServerContext::new();
    let kw = HashMap::new();

    // Pilot 1 (HIC pilot)
    let mut sess_hic = SessionState::new(9903);
    ctx.dispatch_rpc(
        &mut sess_hic,
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
    .expect("Login HIC");
    ctx.dispatch_rpc(
        &mut sess_hic,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID HIC");

    // Pilot 2 (Victim pilot)
    let mut sess_victim = SessionState::new(9904);
    ctx.dispatch_rpc(
        &mut sess_victim,
        "authentication",
        None,
        "Login",
        &[
            PyRep::String("other".into()),
            PyRep::String("other-secret".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Login victim");
    ctx.dispatch_rpc(
        &mut sess_victim,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_002)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID victim");

    let hic_char_id = 90_000_001i64;
    let hic_ship_id = sess_hic.ship_id.expect("hic ship_id");
    let victim_char_id = 90_000_002i64;
    let victim_ship_id = sess_victim.ship_id.expect("victim ship_id");
    let station_id = 60_003_760i64;

    // Clear any pre-seeded modules on both ships so slots are clean
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, i| {
            !(i.location_id == hic_ship_id || i.location_id == victim_ship_id)
        });
    });

    grant_fitting_skills(&ctx, hic_char_id);
    grant_fitting_skills(&ctx, victim_char_id);

    // Seed Warp Disruption Field Generator I (type 28654, group 899, bubble radius 16,000m) in station hangar first
    let wdfg_id = 880_010i64;
    let script_id = 880_011i64; // Focused Warp Scrambling Script (type 45010)
    seed_fitting_item(
        &ctx,
        wdfg_id,
        28654,
        hic_char_id,
        station_id,
        eve_db::InventoryFlag::Hangar,
        1,
    );

    // 1. Hull restriction check: Rifter (587, group 25) cannot fit Warp Disruption Field Generator I (canFitShipGroup01 = 894)
    let non_hic_fit_err = ctx
        .dispatch_rpc(
            &mut sess_hic,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(wdfg_id),
                PyRep::Integer(hic_ship_id),
                PyRep::Integer(eve_db::InventoryFlag::HiSlot0 as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("Non-HIC hull must not fit WDFG");
    assert_eq!(non_hic_fit_err.code, "FittingValidationFailed");
    assert!(
        non_hic_fit_err.message.contains("894"),
        "Fitting error must cite Heavy Interdiction Cruiser group 894: {}",
        non_hic_fit_err.message
    );

    // 2. Transform Pilot 1's ship into an Onyx Heavy Interdiction Cruiser (type 11995, group 894) and fit WDFG
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship_item) = snap.items.get_mut(&hic_ship_id) {
            ship_item.type_id = 11995;
        }
    });
    ctx.invalidate_ship_fitting(hic_ship_id);

    ctx.dispatch_rpc(
        &mut sess_hic,
        "invbroker",
        None,
        "Add",
        &[
            PyRep::Integer(wdfg_id),
            PyRep::Integer(hic_ship_id),
            PyRep::Integer(eve_db::InventoryFlag::HiSlot0 as i64),
        ],
        &kw,
        None,
    )
    .await
    .expect("Onyx (group 894) must fit WDFG I");

    seed_fitting_item(
        &ctx,
        script_id,
        45010,
        hic_char_id,
        hic_ship_id,
        eve_db::InventoryFlag::Cargo,
        5,
    );

    // Fit Warp Core Stabilizer I (type 10998, maxGroupFitted = 1) on Victim via invbroker.Add
    seed_fitting_item(
        &ctx,
        880_012,
        10998,
        victim_char_id,
        station_id,
        eve_db::InventoryFlag::Hangar,
        1,
    );
    ctx.dispatch_rpc(
        &mut sess_victim,
        "invbroker",
        None,
        "Add",
        &[
            PyRep::Integer(880_012),
            PyRep::Integer(victim_ship_id),
            PyRep::Integer(eve_db::InventoryFlag::LoSlot0 as i64),
        ],
        &kw,
        None,
    )
    .await
    .expect("Fit WCS on victim");
    ctx.invalidate_ship_fitting(hic_ship_id);
    ctx.invalidate_ship_fitting(victim_ship_id);

    // Undock both ships
    ctx.dispatch_rpc(&mut sess_hic, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock HIC");
    let undock_v = ctx
        .dispatch_rpc(&mut sess_victim, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock victim");
    let PyRep::KeyVal(uv_kv) = &undock_v else { panic!() };
    let beyonce_victim = uv_kv["beyonceBoundID"].as_i64().unwrap() as u64;
    let jita_handle = ctx.systems.get_or_spawn(30_000_142);
    let dest_x = 50_001_249i64;

    let (_, mut hic_ball) = jita_handle.get_ball_snapshot(hic_ship_id).await.unwrap().unwrap();
    hic_ball.type_id = 11995;
    hic_ball.position = DVec3::ZERO;
    hic_ball.max_velocity = 185.0;
    hic_ball.mass = 12_600_000.0;
    jita_handle.spawn_ball(hic_ball).await.unwrap();
    let base_cs = ctx.build_ship_combat_state(hic_char_id, hic_ship_id, 11995);
    let base_sig = base_cs.signature_radius;
    jita_handle.set_combat_state(hic_ship_id, base_cs).await.unwrap();

    // Place victim at (10,000m, 0, 0) -> inside the 16,000m unscripted WDFG bubble
    let _ = jita_handle
        .set_ball_position(victim_ship_id, DVec3::new(10_000.0, 0.0, 0.0))
        .await;

    // 3. Activate unscripted WDFG I on Onyx -> spawns 16,000m bubble and applies self-penalties
    let act_unscripted = ctx
        .dispatch_rpc(
            &mut sess_hic,
            "dogmaIM",
            None,
            "Activate",
            &[
                PyRep::Integer(wdfg_id),
                PyRep::String("warpDisruptionSphere".into()),
                PyRep::None,
                PyRep::Integer(1),
            ],
            &kw,
            None,
        )
        .await
        .expect("Activate unscripted WDFG on Onyx");
    let PyRep::KeyVal(act_kv) = &act_unscripted else { panic!() };
    assert_eq!(act_kv["scripted"], PyRep::Bool(false));
    assert_eq!(act_kv["bubbleRadius"], PyRep::Real(16_000.0));

    // Verify HIC self-penalties (velocity -60%, mass +80M kg, sig radius +50%, disallow_assistance = true)
    let (_, hic_ball_active) = jita_handle.get_ball_snapshot(hic_ship_id).await.unwrap().unwrap();
    assert!(
        (hic_ball_active.max_velocity - 185.0 * 0.40).abs() < 1.0,
        "Unscripted WDFG must reduce HIC max velocity by 60%"
    );
    assert!(
        (hic_ball_active.mass - (12_600_000.0 + 80_000_000.0)).abs() < 100.0,
        "Unscripted WDFG must add 80,000,000 kg mass penalty"
    );
    let hic_cs_active = jita_handle.get_combat_state(hic_ship_id).await.unwrap().unwrap();
    assert!(
        (hic_cs_active.signature_radius - base_sig * 1.50).abs() < 1.0,
        "Unscripted WDFG must increase HIC signature radius by 50%"
    );
    assert!(
        hic_cs_active.disallow_assistance,
        "Unscripted WDFG must set disallow_assistance = true"
    );

    // Victim at 10km (inside 16km bubble) cannot warp even with Warp Core Stabilizers!
    ctx.dispatch_rpc(
        &mut sess_victim,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(880_012),
            PyRep::String("warpCoreFitted".into()),
            PyRep::None,
            PyRep::Integer(1),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate WCS 1");
    let bubble_warp_err = ctx
        .dispatch_rpc(
            &mut sess_victim,
            "beyonce",
            Some(beyonce_victim),
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(dest_x)],
            &kw,
            None,
        )
        .await
        .expect_err("Ship inside HIC bubble cannot warp regardless of WCS");
    assert_eq!(bubble_warp_err.code, "WarpScrambled");

    // 4. Deactivate unscripted WDFG -> bubble removed and HIC stats restored
    ctx.dispatch_rpc(
        &mut sess_hic,
        "dogmaIM",
        None,
        "Deactivate",
        &[
            PyRep::Integer(wdfg_id),
            PyRep::String("warpDisruptionSphere".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Deactivate unscripted WDFG");

    let (_, hic_ball_restored) = jita_handle.get_ball_snapshot(hic_ship_id).await.unwrap().unwrap();
    assert!((hic_ball_restored.max_velocity - 185.0).abs() < 1.0);
    assert!((hic_ball_restored.mass - 12_600_000.0).abs() < 100.0);
    let hic_cs_restored = jita_handle.get_combat_state(hic_ship_id).await.unwrap().unwrap();
    assert!(!hic_cs_restored.disallow_assistance);

    // 5. Load Focused Warp Scrambling Script (type 45010) into WDFG via dogmaIM.LoadAmmo
    ctx.dispatch_rpc(
        &mut sess_hic,
        "dogmaIM",
        None,
        "LoadAmmo",
        &[
            PyRep::Integer(hic_ship_id),
            PyRep::List(vec![PyRep::Integer(wdfg_id)]),
            PyRep::List(vec![PyRep::Integer(script_id)]),
        ],
        &kw,
        None,
    )
    .await
    .expect("Load Focused Warp Scrambling Script into WDFG");

    // Activating scripted WDFG without a target fails with TargetRequired
    let no_tgt_err = ctx
        .dispatch_rpc(
            &mut sess_hic,
            "dogmaIM",
            None,
            "Activate",
            &[
                PyRep::Integer(wdfg_id),
                PyRep::String("warpDisruptionSphere".into()),
                PyRep::None,
                PyRep::Integer(1),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("Scripted WDFG requires a target");
    assert_eq!(no_tgt_err.code, "TargetRequired");

    // Lock victim before activating scripted WDFG
    ctx.dispatch_rpc(
        &mut sess_hic,
        "dogmaIM",
        None,
        "AddTarget",
        &[PyRep::Integer(victim_ship_id)],
        &kw,
        None,
    )
    .await
    .expect("HIC AddTarget victim");
    jita_handle.step_manual(10.0).await.unwrap();

    // Activate scripted WDFG on victim -> applies infinite point (100 strength) + disables MWD/MJD, no bubble
    let act_scripted = ctx
        .dispatch_rpc(
            &mut sess_hic,
            "dogmaIM",
            None,
            "Activate",
            &[
                PyRep::Integer(wdfg_id),
                PyRep::String("warpDisruptionSphere".into()),
                PyRep::Integer(victim_ship_id),
                PyRep::Integer(1),
            ],
            &kw,
            None,
        )
        .await
        .expect("Activate scripted WDFG on victim");
    let PyRep::KeyVal(scr_kv) = &act_scripted else { panic!() };
    assert_eq!(scr_kv["scripted"], PyRep::Bool(true));
    assert_eq!(scr_kv["bubbleRadius"], PyRep::None);
    assert_eq!(scr_kv["warpScrambleStrength"], PyRep::Integer(100));

    let victim_cs_inf = jita_handle.get_combat_state(victim_ship_id).await.unwrap().unwrap();
    assert_eq!(
        victim_cs_inf.warp_scramble_strength, 100,
        "Focused script must apply 100 warp scramble strength (infinite point)"
    );
    assert!(
        victim_cs_inf.is_mwd_mjd_disabled,
        "Focused script must disable target MWD and MJD"
    );

    // Victim cannot warp despite Warp Core Stabilizers
    let inf_warp_err = ctx
        .dispatch_rpc(
            &mut sess_victim,
            "beyonce",
            Some(beyonce_victim),
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(dest_x)],
            &kw,
            None,
        )
        .await
        .expect_err("Focused WDFG infinite point must prevent warp regardless of WCS");
    assert_eq!(inf_warp_err.code, "WarpScrambled");
}

// parity: feat:modules.warp-disruption-field
#[test]
fn test_warp_bubble_interception_stop_and_drag() {
    let mut park = Ballpark::new(30_000_142);
    let ship_id = 3001;
    let origin = DVec3::ZERO;
    let target_destination = DVec3::new(10_000_000.0, 0.0, 0.0);

    let stop_bubble_id = 7001;
    let stop_bubble_pos = DVec3::new(5_000_000.0, 0.0, 0.0);
    let bubble_radius = 20_000.0;
    park.add_warp_bubble(stop_bubble_id, stop_bubble_pos, bubble_radius);
    park.add_ball(Ball {
        id: ship_id,
        type_id: 587,
        position: origin,
        velocity: DVec3::ZERO,
        max_velocity: 365.0,
        inertia: 3.45,
        mass: 1_067_000.0,
        radius: 37.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });

    park.set_mode(ship_id, MovementMode::new_warp(origin, target_destination, 3.0))
        .expect("set warp mode");
    let MovementMode::Warp { destination, .. } = park.balls[&ship_id].mode else {
        panic!("Ship must be in Warp mode");
    };
    assert_eq!(destination.x, 4_980_000.0, "Stop bubble must pull warp destination to entry perimeter");

    for _ in 0..40 {
        park.step(1.0);
    }
    let landed = &park.balls[&ship_id];
    assert_eq!(landed.mode, MovementMode::Stop);
    assert!((landed.position - DVec3::new(4_980_000.0, 0.0, 0.0)).length() <= 2_500.0);

    park.remove_warp_bubble(stop_bubble_id);
    let drag_bubble_id = 7002;
    let drag_bubble_pos = DVec3::new(10_100_000.0, 0.0, 0.0);
    park.add_warp_bubble(drag_bubble_id, drag_bubble_pos, bubble_radius);

    park.set_ball_position(ship_id, origin);
    park.set_mode(ship_id, MovementMode::new_warp(origin, target_destination, 3.0))
        .expect("set warp with drag bubble");
    let MovementMode::Warp { destination: drag_dest, .. } = park.balls[&ship_id].mode else {
        panic!("Ship must be in Warp mode");
    };
    assert_eq!(drag_dest.x, 10_080_000.0, "Drag bubble must drag warp landing point to perimeter");

    for _ in 0..40 {
        park.step(1.0);
    }
    let landed_drag = &park.balls[&ship_id];
    assert_eq!(landed_drag.mode, MovementMode::Stop);
    assert!((landed_drag.position - DVec3::new(10_080_000.0, 0.0, 0.0)).length() <= 2_500.0);

    park.set_ball_position(ship_id, drag_bubble_pos);
    let inside_err = park.set_mode(ship_id, MovementMode::new_warp(drag_bubble_pos, origin, 3.0));
    assert!(inside_err.unwrap_err().contains("WarpScrambled"));

    park.remove_warp_bubble(drag_bubble_id);
    park.set_ball_position(ship_id, origin);
    park.set_mode(ship_id, MovementMode::new_warp(origin, target_destination, 3.0))
        .expect("clean warp");
    for _ in 0..5 {
        park.step(1.0);
    }
    let mid_flight_pos = park.balls[&ship_id].position;
    park.add_warp_bubble(7003, mid_flight_pos, 25_000.0);
    let evs = park.step(1.0);
    assert_eq!(park.balls[&ship_id].mode, MovementMode::Stop);
    assert!(
        evs.iter()
            .any(|e| matches!(e, DestinyEvent::OnDeactivatingWarp { ball_id: 3001 })),
        "Must emit OnDeactivatingWarp when stopped by in-flight bubble"
    );
}
