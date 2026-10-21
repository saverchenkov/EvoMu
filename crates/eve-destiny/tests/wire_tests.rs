use eve_destiny::wire::*;
use eve_destiny::{
    Ball, BallId, Ballpark, DestinyEvent, MovementMode, SlimItemMeta, AU_METERS,
};
use eve_proto::PyRep;
use glam::DVec3;

fn sample_ball(id: BallId, mode: MovementMode) -> Ball {
    Ball {
        id,
        type_id: 587,
        position: DVec3::new(1_000.0 * (id as f64), -2_500.0, 500.0),
        velocity: DVec3::new(120.0, -30.0, 10.0),
        max_velocity: 365.0,
        inertia: 3.45,
        mass: 1_067_000.0,
        radius: 37.0,
        speed_fraction: 0.85,
        mode,
    }
}

#[test]
// parity: plan:P-7, feat:navigation.destiny-updates, destiny:AddBalls2
fn test_binary_add_balls_roundtrip_all_13_modes() {
    let balls = vec![
        sample_ball(1001, MovementMode::GotoDirection { direction: DVec3::new(0.6, 0.8, 0.0) }),
        sample_ball(1002, MovementMode::Follow { target_id: 40_009_076, range: 2_500.0 }),
        sample_ball(1003, MovementMode::Stop),
        sample_ball(1004, MovementMode::new_warp(DVec3::ZERO, DVec3::new(2.0 * AU_METERS, 1.0e8, -5.0e7), 3.0)),
        sample_ball(1005, MovementMode::Orbit { target_id: 60_003_760, range: 5_000.0 }),
        sample_ball(1006, MovementMode::Missile { target_id: 1001 }),
        sample_ball(1007, MovementMode::Mushroom),
        sample_ball(1008, MovementMode::Boid),
        sample_ball(1009, MovementMode::Troll),
        sample_ball(1010, MovementMode::Miniball),
        sample_ball(1011, MovementMode::Field),
        sample_ball(1012, MovementMode::Rigid),
        sample_ball(1013, MovementMode::Formation),
    ];
    let refs: Vec<&Ball> = balls.iter().collect();
    let bytes = encode_add_balls(42, &refs);
    let (pkt_type, stamp, decoded) = decode_add_balls(&bytes).expect("decode_add_balls must succeed");
    assert_eq!((pkt_type, stamp, decoded.len()), (0, 42, 13));

    let expected_modes = [
        DSTBALL_GOTO, DSTBALL_FOLLOW, DSTBALL_STOP, DSTBALL_WARP, DSTBALL_ORBIT,
        DSTBALL_MISSILE, DSTBALL_MUSHROOM, DSTBALL_BOID, DSTBALL_TROLL, DSTBALL_MINIBALL,
        DSTBALL_FIELD, DSTBALL_RIGID, DSTBALL_FORMATION,
    ];
    let expected_ball_sizes = [124, 112, 100, 148, 112, 148, 124, 100, 104, 100, 64, 39, 116];
    for (i, dec) in decoded.iter().enumerate() {
        assert_eq!((dec.id, dec.mode), (balls[i].id, expected_modes[i]));
        assert!((dec.position - balls[i].position).length() < 1e-6);
        assert!((dec.radius - (balls[i].radius as f32)).abs() < 1e-3);
        let mut mode_buf = Vec::new();
        encode_single_ball_with_meta(&mut mode_buf, &balls[i], 42, None);
        assert_eq!(mode_buf.len(), expected_ball_sizes[i]);
    }

    let mut single_buf = Vec::new();
    let meta = SlimItemMeta {
        group_id: 27,
        category_id: 6,
        owner_id: 90_000_001,
        char_id: Some(90_000_001),
        corp_id: 1_000_125,
        alliance_id: 99_000_001,
        security_status: 2.5,
        is_cloaked: false,
    };
    encode_single_ball_with_meta(&mut single_buf, &balls[2], 42, Some(&meta));
    assert_eq!(i64::from_le_bytes(single_buf[47..55].try_into().unwrap()), 99_000_001);
    assert_eq!(i32::from_le_bytes(single_buf[55..59].try_into().unwrap()), 1_000_125);
    assert_eq!(i32::from_le_bytes(single_buf[59..63].try_into().unwrap()), -1);

    let mut orbit_buf = Vec::new();
    encode_single_ball_with_meta(&mut orbit_buf, &balls[4], 42, Some(&meta));
    assert_eq!(orbit_buf[99], 0xFF);
    assert_eq!(u32::from_le_bytes(orbit_buf[100..104].try_into().unwrap()), 60_003_760);
    assert_eq!(f64::from_le_bytes(orbit_buf[104..112].try_into().unwrap()), 5_000.0);

    let mut warp_buf = Vec::new();
    encode_single_ball_with_meta(&mut warp_buf, &balls[3], 42, Some(&meta));
    assert_eq!(warp_buf[99], 0xFF);
    assert_eq!(i32::from_le_bytes(warp_buf[124..128].try_into().unwrap()), 42);
    assert_eq!(i64::from_le_bytes(warp_buf[128..136].try_into().unwrap()), (-1.0f64).to_bits() as i64);
    assert_eq!(i64::from_le_bytes(warp_buf[136..144].try_into().unwrap()), 15_000.0f64.to_bits() as i64);
    assert_eq!(i32::from_le_bytes(warp_buf[144..148].try_into().unwrap()), 30);

    let mut missile_buf = Vec::new();
    encode_single_ball_with_meta(&mut missile_buf, &balls[5], 42, Some(&meta));
    assert_eq!(i64::from_le_bytes(missile_buf[100..108].try_into().unwrap()), 1001);
    assert_eq!(i64::from_le_bytes(missile_buf[112..120].try_into().unwrap()), 90_000_001);
    assert_eq!(i32::from_le_bytes(missile_buf[120..124].try_into().unwrap()), 42);

    let mut mushroom_buf = Vec::new();
    encode_single_ball_with_meta(&mut mushroom_buf, &balls[6], 42, Some(&meta));
    assert_eq!(i32::from_le_bytes(mushroom_buf[112..116].try_into().unwrap()), 42);
    assert_eq!(i64::from_le_bytes(mushroom_buf[116..124].try_into().unwrap()), 90_000_001);

    let mut troll_buf = Vec::new();
    encode_single_ball_with_meta(&mut troll_buf, &balls[8], 42, Some(&meta));
    assert_eq!(u32::from_le_bytes(troll_buf[100..104].try_into().unwrap()), 42);

    let mut form_buf = Vec::new();
    encode_single_ball_with_meta(&mut form_buf, &balls[12], 42, Some(&meta));
    assert_eq!(i32::from_le_bytes(form_buf[112..116].try_into().unwrap()), 42);

    assert_eq!(decoded[0].direction, Some(DVec3::new(0.6, 0.8, 0.0)));
    assert_eq!(decoded[1].target_id, Some(40_009_076));
    assert_eq!(decoded[1].follow_range, Some(2_500.0));
    assert_eq!(decoded[3].warp_destination, Some(DVec3::new(2.0 * AU_METERS, 1.0e8, -5.0e7)));
    assert_eq!(decoded[3].warp_speed_tenth_au_s, Some(30));
    assert_eq!(decoded[4].target_id, Some(60_003_760));
    assert_eq!(decoded[4].follow_range, Some(5_000.0));
    assert_eq!(decoded[5].target_id, Some(1001));
    assert_eq!(decoded[5].owner_id, Some(1));
    assert_eq!(decoded[5].effect_stamp, Some(42));
    assert_eq!(decoded[6].owner_id, Some(1));
    assert_eq!(decoded[6].effect_stamp, Some(42));
    assert_eq!(decoded[8].effect_stamp, Some(42));
    assert_eq!(decoded[12].effect_stamp, Some(42));
    assert!(decoded[11].velocity.is_none(), "Rigid ball has no velocity sector");
    assert!(decoded[11].mass.is_none(), "Rigid ball has no MassSector");
}

#[test]
// parity: plan:P-7, feat:navigation.destiny-updates, notify:DoDestinyUpdate
// parity: destiny:AddBalls2, destiny:RemoveBalls, destiny:RemoveBall
// parity: destiny:GotoDirection, destiny:FollowBall, destiny:Orbit, destiny:Stop
fn test_do_destiny_update_and_set_state_marshal_roundtrip() {
    let mut park = Ballpark::new(30_000_142);
    park.add_static_ball(
        Ball {
            id: 40_009_076,
            type_id: 3802,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 0.0,
            mass: 1.0e30,
            radius: 513_100_000.0,
            speed_fraction: 0.0,
            mode: MovementMode::Rigid,
        },
        6,
    );
    let ship = sample_ball(100_000_001, MovementMode::Stop);
    park.add_ball(ship.clone());
    park.set_ball_slim_meta(
        100_000_001,
        SlimItemMeta {
            group_id: 25,
            category_id: 6,
            owner_id: 90_000_001,
            char_id: Some(90_000_001),
            corp_id: 1_000_125,
            alliance_id: 99_000_001,
            security_status: 1.75,
            is_cloaked: false,
        },
    );

    let set_state_update = encode_set_state_update(&park, 100_000_001);
    let marshaled = eve_proto::marshal::encode(&set_state_update);
    assert_eq!(marshaled[0], 0x7E, "Must start with 0x7E EVE marshal magic");
    let unmarshaled =
        eve_proto::marshal::decode(&marshaled).expect("SetState marshal roundtrip");
    assert_eq!(unmarshaled, set_state_update);

    // Extract the binary state buffer from SetState and decode it
    let PyRep::Tuple(top) = &unmarshaled else {
        panic!("Expected outer (stamp, action) tuple");
    };
    let PyRep::Tuple(action) = &top[1] else {
        panic!("Expected (action_name, args) tuple");
    };
    assert_eq!(action[0].as_str(), Some("SetState"));
    let PyRep::Tuple(args) = &action[1] else {
        panic!("Expected args tuple");
    };
    let state_bytes = match &args[0] {
        PyRep::KeyVal(map) => map.get("state").and_then(|v| match v {
            PyRep::Bytes(b) | PyRep::Buffer(b) => Some(b.as_slice()),
            _ => None,
        }),
        _ => None,
    }
    .expect("SetState KeyVal must contain PyRep::Bytes under 'state'");
    let (pkt_type, _, dec_balls) =
        decode_add_balls(state_bytes).expect("SetState state bytes must decode");
    assert_eq!(pkt_type, 0);
    assert_eq!(dec_balls.len(), 2);
    assert_eq!(dec_balls[0].id, 40_009_076);
    assert_eq!(dec_balls[1].id, 100_000_001);
    assert_eq!(dec_balls[1].corp_id, Some(1_000_125));
    assert_eq!(dec_balls[1].alliance_id, Some(99_000_001));

    // Also test all DoDestinyUpdate action tuples marshal roundtrip
    let events = vec![
        DestinyEvent::AddBalls2 {
            balls: vec![ship.clone()],
        },
        DestinyEvent::RemoveBalls {
            ball_ids: vec![100_000_001],
        },
        DestinyEvent::RemoveBall {
            ball_id: 100_000_001,
        },
        DestinyEvent::GotoDirection {
            ball_id: 100_000_001,
            direction: [1.0, 0.0, 0.0],
        },
        DestinyEvent::Follow {
            ball_id: 100_000_001,
            target_id: 40_009_076,
            range: 2_500.0,
        },
        DestinyEvent::Orbit {
            ball_id: 100_000_001,
            target_id: 40_009_076,
            range: 5_000.0,
        },
        DestinyEvent::Stop {
            ball_id: 100_000_001,
        },
        DestinyEvent::WarpTo {
            ball_id: 100_000_001,
            destination: [1.0e9, 2.0e9, 3.0e9],
            warp_speed_au_s: 3.0,
        },
        DestinyEvent::OnActivatingWarp {
            ball_id: 100_000_001,
        },
        DestinyEvent::OnDeactivatingWarp {
            ball_id: 100_000_001,
        },
        DestinyEvent::OnSpecialFX {
            ball_id: 100_000_001,
            module_id: Some(12345),
            guid: "effects.Warping".to_string(),
            is_active: true,
        },
    ];
    let updates_pyrep = encode_destiny_updates(7, &events, Some(&park));
    let updates_bytes = eve_proto::marshal::encode(&updates_pyrep);
    let decoded_updates =
        eve_proto::marshal::decode(&updates_bytes).expect("DoDestinyUpdate list roundtrip");
    assert_eq!(decoded_updates, updates_pyrep);
}
