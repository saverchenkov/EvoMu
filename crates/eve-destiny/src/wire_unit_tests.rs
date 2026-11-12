//! Unit tests for `wire.rs`: binary `AddBalls` / `SetState` encoding & decoding,
//! all 13 `DST_BALL_*` movement mode blocks, header flags, truncated buffer errors,
//! `SlimItem` dictionary fields, and `DoDestinyUpdate` action tuples.

use super::*;
use crate::{
    Ball, Ballpark, CombatState, DestinyEvent, FormationState, MovementMode, MushroomState,
    SlimItemMeta, TrollState, WarpPhase,
};
use eve_proto::PyRep;
use glam::DVec3;

fn sample_ball(id: i64, mode: MovementMode) -> Ball {
    Ball {
        id,
        type_id: 587,
        position: DVec3::new(1000.0, -2000.0, 3000.5),
        velocity: DVec3::new(10.0, -20.0, 30.0),
        max_velocity: 350.0,
        inertia: 2.5,
        mass: 1_200_000.0,
        radius: 42.0,
        speed_fraction: 0.75,
        mode,
    }
}

#[test]
fn encode_and_decode_all_thirteen_movement_modes_roundtrip() {
    let modes = vec![
        (100, MovementMode::GotoDirection { direction: DVec3::new(100.0, 200.0, -300.0) }),
        (101, MovementMode::Follow { target_id: 9001, range: 2500.0 }),
        (102, MovementMode::Stop),
        (
            103,
            MovementMode::Warp {
                origin: DVec3::ZERO,
                destination: DVec3::new(1.0e9, 2.0e9, -3.0e9),
                warp_speed_au_s: 3.0,
                total_distance: 3.74e9,
                phase: WarpPhase::Cruising,
                elapsed_warp_time: 4.0,
            },
        ),
        (104, MovementMode::Orbit { target_id: 9002, range: 7500.0 }),
        (105, MovementMode::Missile { target_id: 9003 }),
        (106, MovementMode::Mushroom),
        (107, MovementMode::Boid),
        (108, MovementMode::Troll),
        (109, MovementMode::Miniball),
        (110, MovementMode::Field),
        (111, MovementMode::Rigid),
        (112, MovementMode::Formation),
    ];

    let balls: Vec<Ball> = modes
        .into_iter()
        .map(|(id, mode)| sample_ball(id, mode))
        .collect();
    let refs: Vec<&Ball> = balls.iter().collect();

    let encoded = encode_add_balls(42, &refs);
    assert_eq!(encoded[0], 0);
    assert_eq!(u32::from_le_bytes(encoded[1..5].try_into().unwrap()), 42);

    let (packet_type, stamp, decoded) = decode_add_balls(&encoded).expect("decode all 13 modes");
    assert_eq!(packet_type, 0);
    assert_eq!(stamp, 42);
    assert_eq!(decoded.len(), 13);

    // 0: GotoDirection
    assert_eq!(decoded[0].id, 100);
    assert_eq!(decoded[0].mode, DST_BALL_GOTO);
    assert_eq!(
        decoded[0].flags,
        BALL_FLAG_IS_FREE | BALL_FLAG_IS_MASSIVE | BALL_FLAG_IS_INTERACTIVE
    );
    assert!(decoded[0].mass.is_some());
    assert_eq!(decoded[0].formation_id, 0xFF);
    assert_eq!(decoded[0].direction, Some(DVec3::new(100.0, 200.0, -300.0)));

    // 1: Follow
    assert_eq!(decoded[1].id, 101);
    assert_eq!(decoded[1].mode, DST_BALL_FOLLOW);
    assert_eq!(decoded[1].target_id, Some(9001));
    assert!((decoded[1].follow_range.unwrap() - 2500.0).abs() < 1e-3);

    // 2: Stop
    assert_eq!(decoded[2].id, 102);
    assert_eq!(decoded[2].mode, DST_BALL_STOP);
    assert!(decoded[2].target_id.is_none());

    // 3: Warp
    assert_eq!(decoded[3].id, 103);
    assert_eq!(decoded[3].mode, DST_BALL_WARP);
    assert_eq!(decoded[3].warp_destination, Some(DVec3::new(1.0e9, 2.0e9, -3.0e9)));
    assert_eq!(decoded[3].warp_speed_milli_au_s, Some(3000));
    assert_eq!(decoded[3].effect_stamp, Some(38));

    // 4: Orbit
    assert_eq!(decoded[4].id, 104);
    assert_eq!(decoded[4].mode, DST_BALL_ORBIT);
    assert_eq!(decoded[4].target_id, Some(9002));
    assert!((decoded[4].follow_range.unwrap() - 7500.0).abs() < 1e-3);

    // 5: Missile
    assert_eq!(decoded[5].id, 105);
    assert_eq!(decoded[5].mode, DST_BALL_MISSILE);
    assert_eq!(decoded[5].target_id, Some(9003));
    assert_eq!(decoded[5].owner_id, Some(1));
    assert_eq!(decoded[5].direction, Some(DVec3::new(10.0, -20.0, 30.0)));

    // 6: Mushroom
    assert_eq!(decoded[6].id, 106);
    assert_eq!(decoded[6].mode, DST_BALL_MUSHROOM);
    assert_eq!(decoded[6].flags, BALL_FLAG_IS_FREE);
    assert_eq!(decoded[6].effect_stamp, Some(42));
    assert_eq!(decoded[6].owner_id, Some(1));

    // 7: Boid
    assert_eq!(decoded[7].id, 107);
    assert_eq!(decoded[7].mode, DST_BALL_BOID);
    assert_eq!(decoded[7].formation_id, 0xFF);

    // 8: Troll
    assert_eq!(decoded[8].id, 108);
    assert_eq!(decoded[8].mode, DST_BALL_TROLL);
    assert_eq!(decoded[8].flags, BALL_FLAG_IS_FREE);
    assert_eq!(decoded[8].effect_stamp, Some(42));

    // 9: Miniball
    assert_eq!(decoded[9].id, 109);
    assert_eq!(decoded[9].mode, DST_BALL_MINIBALL);
    assert_eq!(decoded[9].flags, BALL_FLAG_IS_FREE);

    // 10: Field
    assert_eq!(decoded[10].id, 110);
    assert_eq!(decoded[10].mode, DST_BALL_FIELD);
    assert_eq!(decoded[10].flags, BALL_FLAG_IS_GLOBAL);
    assert!(decoded[10].mass.is_some());
    assert!(decoded[10].velocity.is_none());

    // 11: Rigid
    assert_eq!(decoded[11].id, 111);
    assert_eq!(decoded[11].mode, DST_BALL_RIGID);
    assert_eq!(decoded[11].flags, BALL_FLAG_IS_GLOBAL);
    assert!(decoded[11].mass.is_none());
    assert!(decoded[11].velocity.is_none());

    // 12: Formation
    assert_eq!(decoded[12].id, 112);
    assert_eq!(decoded[12].mode, DST_BALL_FORMATION);
    assert_eq!(decoded[12].target_id, Some(0));
    assert_eq!(decoded[12].effect_stamp, Some(42));
}

#[test]
fn encode_single_ball_with_park_populates_mode_states_and_meta() {
    let mut park = Ballpark::new(30000142);
    park.current_tick = 77;

    // 1. Mushroom ball with custom MushroomState and harmonic
    let mush = sample_ball(201, MovementMode::Mushroom);
    park.add_ball(mush.clone());
    park.mushroom_states.insert(
        201,
        MushroomState {
            max_radius: 12_500.0,
            duration_s: 15.0,
            effect_stamp: 55,
            owner_id: 9000001,
        },
    );
    park.ball_harmonics.insert(201, 3);
    park.set_ball_slim_meta(
        201,
        SlimItemMeta {
            corp_id: 1000125,
            alliance_id: 99000001,
            owner_id: 9000001,
            is_cloaked: true,
            ..SlimItemMeta::default()
        },
    );
    park.combat_states.remove(&201);

    // 2. Troll ball with custom TrollState
    let troll = sample_ball(202, MovementMode::Troll);
    park.add_ball(troll.clone());
    park.troll_states.insert(202, TrollState { effect_stamp: 88 });

    // 3. Formation ball with custom FormationState
    let form = sample_ball(203, MovementMode::Formation);
    park.add_ball(form.clone());
    park.formation_states.insert(
        203,
        FormationState {
            leader_id: 201,
            slot_offset: DVec3::new(500.0, -250.0, 1000.0),
            follow_range: 1500.0,
            effect_stamp: 66,
        },
    );

    // 4. Missile ball with owner_id in slim_meta and combat_states cloaked
    let missile = sample_ball(204, MovementMode::Missile { target_id: 202 });
    park.add_ball(missile.clone());
    park.set_ball_slim_meta(
        204,
        SlimItemMeta {
            owner_id: 7777,
            is_cloaked: false,
            ..SlimItemMeta::default()
        },
    );
    park.combat_states.insert(
        204,
        CombatState {
            is_cloaked: true,
            ..CombatState::default()
        },
    );

    let refs = [&mush, &troll, &form, &missile];
    let bytes = encode_add_balls_with_park(1, park.current_tick as u32, &refs, Some(&park));
    let (pkt, stamp, decoded) = decode_add_balls(&bytes).expect("decode park balls");
    assert_eq!(pkt, 1);
    assert_eq!(stamp, 77);
    assert_eq!(decoded.len(), 4);

    // Check mushroom
    assert_eq!(decoded[0].mode, DST_BALL_MUSHROOM);
    assert_eq!(decoded[0].cloak, Some(1));
    assert_eq!(decoded[0].corp_id, Some(1000125));
    assert_eq!(decoded[0].alliance_id, Some(99000001));
    assert_eq!(decoded[0].harmonic, Some(3));
    assert!((decoded[0].follow_range.unwrap() - 12_500.0).abs() < 1e-3);
    assert_eq!(decoded[0].effect_stamp, Some(55));
    assert_eq!(decoded[0].owner_id, Some(9000001));

    // Check troll
    assert_eq!(decoded[1].mode, DST_BALL_TROLL);
    assert_eq!(decoded[1].effect_stamp, Some(88));

    // Check formation
    assert_eq!(decoded[2].mode, DST_BALL_FORMATION);
    assert_eq!(decoded[2].target_id, Some(201));
    assert!((decoded[2].follow_range.unwrap() - 1500.0).abs() < 1e-3);
    assert_eq!(decoded[2].effect_stamp, Some(66));

    // Check missile (cloaked via combat_states, owner_id from slim_meta)
    assert_eq!(decoded[3].mode, DST_BALL_MISSILE);
    assert_eq!(decoded[3].cloak, Some(1));
    assert_eq!(decoded[3].owner_id, Some(7777));
}

#[test]
fn encode_single_ball_and_meta_helpers_roundtrip() {
    let mut buf = Vec::new();
    let ball = sample_ball(300, MovementMode::Stop);
    let meta = SlimItemMeta {
        corp_id: 500,
        alliance_id: 600,
        is_cloaked: true,
        ..SlimItemMeta::default()
    };
    encode_single_ball_with_meta(&mut buf, &ball, 10, Some(&meta));
    encode_single_ball(&mut buf, &ball);
    let mut header_and_balls = Vec::new();
    header_and_balls.push(0u8);
    header_and_balls.extend_from_slice(&10u32.to_le_bytes());
    header_and_balls.extend_from_slice(&buf);

    let (_, _, dec) = decode_add_balls(&header_and_balls).expect("decode single with meta");
    assert_eq!(dec.len(), 2);
    assert_eq!(dec[0].corp_id, Some(500));
    assert_eq!(dec[0].alliance_id, Some(600));
    assert_eq!(dec[0].cloak, Some(1));
    assert_eq!(dec[1].cloak, Some(0));
}

#[test]
fn decode_add_balls_rejects_truncated_and_invalid_buffers() {
    // 1. Shorter than 5-byte header
    assert!(decode_add_balls(&[]).is_err());
    assert!(decode_add_balls(&[0, 1, 2, 3]).is_err());

    // 2. Encode each mode and test every possible truncation prefix from 6..len
    let all_modes = vec![
        MovementMode::GotoDirection { direction: DVec3::X },
        MovementMode::Follow { target_id: 1, range: 100.0 },
        MovementMode::Stop,
        MovementMode::Warp {
            origin: DVec3::ZERO,
            destination: DVec3::X * 1.0e8,
            warp_speed_au_s: 3.0,
            total_distance: 1.0e8,
            phase: WarpPhase::Accelerating,
            elapsed_warp_time: 1.0,
        },
        MovementMode::Orbit { target_id: 2, range: 200.0 },
        MovementMode::Missile { target_id: 3 },
        MovementMode::Mushroom,
        MovementMode::Boid,
        MovementMode::Troll,
        MovementMode::Miniball,
        MovementMode::Field,
        MovementMode::Rigid,
        MovementMode::Formation,
    ];

    for (idx, mode) in all_modes.into_iter().enumerate() {
        let ball = sample_ball(1000 + idx as i64, mode);
        let full = encode_add_balls(1, &[&ball]);
        for cut in 6..full.len() {
            assert!(
                decode_add_balls(&full[..cut]).is_err(),
                "mode {idx} should fail when truncated at byte {cut}/{}",
                full.len()
            );
        }
    }

    // 3. Unknown ball mode byte (e.g. 99)
    let stop_ball = sample_ball(999, MovementMode::Stop);
    let mut bad_mode = encode_add_balls(1, &[&stop_ball]);
    bad_mode[13] = 99;
    let err = decode_add_balls(&bad_mode).expect_err("unknown ball mode should error");
    assert!(err.contains("99"), "unexpected error: {err}");
}

#[test]
fn slim_item_meta_and_dict_covers_stargate_wormhole_npc_and_player_ship() {
    let mut park = Ballpark::new(30000142);

    // 1. Default ship (not in park)
    let default_ball = sample_ball(400, MovementMode::Stop);
    let meta_def = resolve_slim_item_meta(None, &default_ball);
    assert_eq!(meta_def.group_id, 25);
    assert_eq!(meta_def.category_id, 6);

    // 2. Rigid ball heuristics when not in park: station (60M..70M), gate (50M..60M), belt (type_id 15), star/planet
    let station_ball = sample_ball(60_003_760, MovementMode::Rigid);
    assert_eq!(resolve_slim_item_meta(None, &station_ball).group_id, 15);
    let gate_heuristic = sample_ball(50_001_234, MovementMode::Rigid);
    assert_eq!(resolve_slim_item_meta(None, &gate_heuristic).group_id, 10);
    let mut belt_heuristic = sample_ball(40_000_001, MovementMode::Rigid);
    belt_heuristic.type_id = 15;
    assert_eq!(resolve_slim_item_meta(None, &belt_heuristic).group_id, 9);
    let planet_heuristic = sample_ball(40_000_002, MovementMode::Rigid);
    assert_eq!(resolve_slim_item_meta(None, &planet_heuristic).group_id, 6);
    let field_heuristic = sample_ball(40_000_003, MovementMode::Field);
    assert_eq!(resolve_slim_item_meta(None, &field_heuristic).group_id, 361);

    // 3. Stargate (group_id == 10) with and without jump_target
    let gate_meta = SlimItemMeta {
        group_id: 10,
        category_id: 2,
        jump_target: Some((50001234, 30000144)),
        ..SlimItemMeta::default()
    };
    let gate_dict = slim_item_dict(&gate_heuristic, &gate_meta);
    if let PyRep::Dict(entries) = gate_dict {
        assert!(entries.iter().any(|(k, _)| matches!(k, PyRep::String(s) if s == "jumps")));
        assert!(entries.iter().any(|(k, v)| matches!(k, PyRep::String(s) if s == "destinationSystemStatusIcons") && matches!(v, PyRep::List(l) if l.is_empty())));
        assert!(entries.iter().any(|(k, v)| matches!(k, PyRep::String(s) if s == "originSystemOwnerID") && matches!(v, PyRep::None)));
        assert!(entries.iter().any(|(k, v)| matches!(k, PyRep::String(s) if s == "destinationSystemOwnerID") && matches!(v, PyRep::None)));
    } else {
        panic!("expected PyRep::Dict");
    }

    // Stargate with explicit empire faction owner IDs emits Integer(faction_id) (#767)
    let faction_gate_meta = SlimItemMeta {
        group_id: 10,
        category_id: 2,
        jump_target: Some((50001234, 30000144)),
        origin_system_owner_id: Some(500_001),
        dest_system_owner_id: Some(500_002),
        ..SlimItemMeta::default()
    };
    if let PyRep::Dict(entries) = slim_item_dict(&gate_heuristic, &faction_gate_meta) {
        assert!(entries.iter().any(|(k, v)| matches!(k, PyRep::String(s) if s == "originSystemOwnerID") && matches!(v, PyRep::Integer(500_001))));
        assert!(entries.iter().any(|(k, v)| matches!(k, PyRep::String(s) if s == "destinationSystemOwnerID") && matches!(v, PyRep::Integer(500_002))));
    } else {
        panic!("expected PyRep::Dict");
    }

    // Named ship slim item includes non-empty "name" (#656)
    let named_ship_ball = sample_ball(499, MovementMode::Stop);
    let named_ship_meta = SlimItemMeta {
        group_id: 25,
        category_id: 6,
        owner_id: 90_000_001,
        char_id: Some(90_000_001),
        name: Some("My Rifter".to_string()),
        ..SlimItemMeta::default()
    };
    let named_dict = slim_item_dict(&named_ship_ball, &named_ship_meta);
    if let PyRep::Dict(entries) = named_dict {
        assert!(entries.iter().any(|(k, v)| matches!(k, PyRep::String(s) if s == "name") && matches!(v, PyRep::String(n) if n == "My Rifter")));
    } else {
        panic!("expected PyRep::Dict");
    }

    // 4. Wormhole (group_id == 988) with custom stages in park
    let wh_ball = sample_ball(403, MovementMode::Rigid);
    let wh_meta = SlimItemMeta {
        group_id: 988,
        category_id: 2,
        ..SlimItemMeta::default()
    };
    park.add_static_ball_with_meta(wh_ball.clone(), wh_meta.clone());
    park.set_ball_wormhole_stages(403, 2, 0.5);
    let wh_dict = slim_item_dict_with_park(Some(&park), &wh_ball, &wh_meta);
    if let PyRep::Dict(entries) = wh_dict {
        assert!(entries.iter().any(|(k, v)| {
            matches!(k, PyRep::String(s) if s == "wormholeAge") && matches!(v, PyRep::Integer(2))
        }));
        assert!(entries.iter().any(|(k, v)| {
            matches!(k, PyRep::String(s) if s == "wormholeSize") && matches!(v, PyRep::Real(r) if (*r - 0.5).abs() < 1e-6)
        }));
    }

    // 5. NPC entity (category_id == 11) with skin_id and skin_material_set_id
    let npc_ball = sample_ball(404, MovementMode::Orbit { target_id: 400, range: 5000.0 });
    let npc_meta = SlimItemMeta {
        group_id: 500,
        category_id: 11,
        owner_id: 500010,
        char_id: Some(9000123),
        corp_id: 1000125,
        alliance_id: 9900001,
        security_status: -5.0,
        is_cloaked: false,
        skin_id: Some(111),
        skin_material_set_id: Some(222),
        jump_target: None,
        ..SlimItemMeta::default()
    };
    let npc_dict = slim_item_dict(&npc_ball, &npc_meta);
    if let PyRep::Dict(entries) = npc_dict {
        assert!(entries.iter().any(|(k, _)| matches!(k, PyRep::String(s) if s == "hostile_response_threshold")));
        assert!(entries.iter().any(|(k, _)| matches!(k, PyRep::String(s) if s == "charID")));
        assert!(entries.iter().any(|(k, _)| matches!(k, PyRep::String(s) if s == "skinID")));
        assert!(entries.iter().any(|(k, _)| matches!(k, PyRep::String(s) if s == "skinMaterialSetID")));
    }

    let slim_basic = encode_slim_item(&default_ball, 25, 6);
    assert!(matches!(slim_basic, PyRep::Object { .. }));
}

#[test]
fn encode_destiny_events_and_hoist_set_state_in_updates() {
    let mut park = Ballpark::new(30000142);
    park.current_tick = 100;
    let b1 = sample_ball(1, MovementMode::Stop);
    let b2 = sample_ball(2, MovementMode::GotoDirection { direction: DVec3::X });
    park.add_ball(b1.clone());
    park.add_ball(b2.clone());

    let events = vec![
        DestinyEvent::AddBall { ball: b1.clone() },
        DestinyEvent::AddBalls { balls: vec![b1.clone(), b2.clone()] },
        DestinyEvent::AddBalls2 { balls: vec![b2.clone()] },
        DestinyEvent::RemoveBall { ball_id: 1 },
        DestinyEvent::RemoveBalls { ball_ids: vec![1, 2] },
        DestinyEvent::GotoDirection { ball_id: 1, direction: [1.0, 0.0, 0.0] },
        DestinyEvent::GotoPoint { ball_id: 1, point: [10.0, 20.0, 30.0] },
        DestinyEvent::Follow { ball_id: 1, target_id: 2, range: 1000.0 },
        DestinyEvent::SetSpeedFraction { ball_id: 1, fraction: 0.5 },
        DestinyEvent::Orbit { ball_id: 1, target_id: 2, range: 5000.0 },
        DestinyEvent::Stop { ball_id: 1 },
        DestinyEvent::WarpTo { ball_id: 1, destination: [1.0e8, 0.0, 0.0], warp_speed_au_s: 3.0 },
        DestinyEvent::SetBallMassive { ball_id: 1, is_massive: true },
        DestinyEvent::SetBallMassive { ball_id: 1, is_massive: false },
        DestinyEvent::SetBallFree { ball_id: 1, is_free: true },
        DestinyEvent::SetBallFree { ball_id: 1, is_free: false },
        DestinyEvent::SetBallHarmonic { ball_id: 1, harmonic: -1 },
        DestinyEvent::SetBallInteractive { ball_id: 1, is_interactive: true },
        DestinyEvent::SetBallInteractive { ball_id: 1, is_interactive: false },
        DestinyEvent::SetBallRadius { ball_id: 1, radius: 120.0 },
        DestinyEvent::SetBallPosition { ball_id: 1, position: [1.0, 2.0, 3.0] },
        DestinyEvent::SetBallVelocity { ball_id: 1, velocity: [4.0, 5.0, 6.0] },
        DestinyEvent::OnDamageStateChange { ball_id: 1, shield_ratio: 0.8, armor_ratio: 0.9, structure_ratio: 1.0 },
        DestinyEvent::OnFleetDamageStateChange { ball_id: 1, shield_ratio: 0.7, armor_ratio: 0.8, structure_ratio: 0.9 },
        DestinyEvent::TerminalExplosion { ball_id: 1, destruction_effect_id: None },
        DestinyEvent::TerminalExplosion { ball_id: 1, destruction_effect_id: Some(42) },
        DestinyEvent::Collision { ball_a: 1, ball_b: 2 },
        DestinyEvent::OnTarget { mode: "add".into(), source_id: 1, target_id: 2 },
        DestinyEvent::OnActivatingWarp { ball_id: 1 },
        DestinyEvent::OnDeactivatingWarp { ball_id: 1 },
        DestinyEvent::OnSpecialFX { ball_id: 1, module_id: Some(10), guid: "effects.Laser".into(), is_active: true },
        DestinyEvent::OnSpecialFX { ball_id: 1, module_id: None, guid: "effects.Warp".into(), is_active: false },
        DestinyEvent::CloakBall { ball_id: 1, cloaked: 1 },
        DestinyEvent::OnClientControllerEvent { ball_id: 1, target_id: Some(2), event_name: "ev".into(), value: 3, delay: Some(0.5) },
        DestinyEvent::OnClientControllerEvent { ball_id: 1, target_id: None, event_name: "ev".into(), value: 0, delay: None },
        DestinyEvent::OnDbuffUpdated { ball_id: 1, dbuffs: vec![(10, 1.5, 100)] },
        DestinyEvent::OnDotVictimUpdated { ball_id: 1, flat_dps: 50.0, pct_dps: 0.01, remaining_ticks: 10, expiry_filetime: 0 },
        DestinyEvent::OnDroneStateChange { drone_id: 99, owner_id: 1, controller_id: 1, activity_state: 1, type_id: 2456, controller_owner_id: 1, target_id: Some(2) },
        DestinyEvent::OnDroneStateChange { drone_id: 99, owner_id: 1, controller_id: 1, activity_state: 0, type_id: 2456, controller_owner_id: 1, target_id: None },
        DestinyEvent::OnDroneStateChange2 { drone_id: 99, owner_id: 1, controller_id: 1, activity_state: 1, type_id: 2456, controller_owner_id: 1, target_id: Some(2) },
        DestinyEvent::OnDroneActivityChange { drone_id: 99, activity_state: 4, activity: Some("returning".into()) },
        DestinyEvent::OnDroneActivityChange { drone_id: 99, activity_state: 0, activity: None },
        DestinyEvent::OnDroneControlLost { drone_id: 99 },
        DestinyEvent::OnShipStateUpdate { module_states: vec![(10, 20, 30, 1, 5.0, 10.0)] },
        DestinyEvent::OnSlimItemChange { ball_id: 1 },
        DestinyEvent::OnSlimItemChange { ball_id: 999_999 },
        DestinyEvent::OnSovereigntyChanged { solar_system_id: 30000142, alliance_id: 9900001 },
        DestinyEvent::PackagedAction { actions_bytes: vec![1, 2, 3] },
        DestinyEvent::OnAudioActivated,
        DestinyEvent::OnDockingAccepted { start_pos: [0.0, 0.0, 0.0], end_pos: [100.0, 0.0, 0.0], station_id: 60003760 },
        DestinyEvent::OnFleetStateChange { fleet_id: 10, member_id: 20, fleet_state: 1 },
        DestinyEvent::ScatterEwars { ball_id: 1, effect_states: vec![7, 8] },
        DestinyEvent::SetControllerVariableOnBall { ball_id: 1, var_name: "state".into(), value: 5 },
        // Put SetState at the end to verify encode_destiny_updates hoists it to index 0
        DestinyEvent::SetState { observer_ball_id: 1 },
    ];

    let updates = encode_destiny_updates(100, &events, Some(&park));
    let PyRep::List(items) = updates else {
        panic!("expected PyRep::List");
    };
    assert_eq!(items.len(), events.len());
    if let PyRep::Tuple(pair) = &items[0] {
        assert_eq!(pair[0], PyRep::Integer(100));
        if let PyRep::Tuple(action) = &pair[1] {
            assert_eq!(action[0], PyRep::String("SetState".into()));
        } else {
            panic!("expected action tuple");
        }
    } else {
        panic!("expected stamped tuple");
    }

    // Also test SetState without park fallback
    let set_state_no_park = encode_destiny_event(100, &DestinyEvent::SetState { observer_ball_id: 1 }, None);
    assert!(matches!(set_state_no_park, PyRep::Tuple(_)));

    // Verify helper functions directly
    let set_state_up = encode_set_state_update(&park, 1);
    assert!(matches!(set_state_up, PyRep::Tuple(_)));
    let add_ball_act = encode_add_ball_action(100, &b1, Some(&park));
    assert!(matches!(add_ball_act, PyRep::Tuple(_)));
    let add_balls_act = encode_add_balls_action(100, &[&b1], Some(&park));
    assert!(matches!(add_balls_act, PyRep::Tuple(_)));
    let add_balls2_act = encode_add_balls2_action(100, &[&b1], Some(&park));
    assert!(matches!(add_balls2_act, PyRep::Tuple(_)));

    // Exact non-unit damage ratios on encode_damage_state_tuple
    let custom_cs = CombatState {
        shield_hp: 50.0,
        max_shield_hp: 200.0,
        armor_hp: 150.0,
        max_armor_hp: 300.0,
        structure_hp: 300.0,
        max_structure_hp: 400.0,
        ..Default::default()
    };
    let wire_dmg = encode_damage_state_tuple(Some(&custom_cs), 77);
    if let PyRep::Tuple(top) = wire_dmg {
        if let PyRep::Tuple(sh) = &top[0] {
            assert_eq!(sh[0], PyRep::Real(0.25));
        } else {
            panic!("expected shield tuple");
        }
        assert_eq!(top[1], PyRep::Real(0.5));
        assert_eq!(top[2], PyRep::Real(0.75));
    } else {
        panic!("expected outer damage tuple");
    }

    // SetBallHarmonic with zero alliance/corp (-1, -1) vs positive alliance/corp
    let h_none = encode_destiny_event(
        10,
        &DestinyEvent::SetBallHarmonic {
            ball_id: 999,
            harmonic: 3,
        },
        Some(&park),
    );
    if let PyRep::Tuple(outer) = h_none {
        if let PyRep::Tuple(call) = &outer[1] {
            if let PyRep::Tuple(args) = &call[1] {
                assert_eq!(args[1], PyRep::Integer(-1));
                assert_eq!(args[2], PyRep::Integer(-1));
            }
        }
    }

    // SetState damageState omits static balls and slim_item_dict handles zero alliance_id
    let mut sp = Ballpark::new(30_000_142);
    sp.add_ball(b1.clone());
    sp.add_static_ball(
        Ball {
            id: 500_001,
            type_id: 16,
            position: DVec3::new(10_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            radius: 1000.0,
            mass: 1e9,
            inertia: 1.0,
            speed_fraction: 0.0,
            mode: MovementMode::Rigid,
        },
        16,
    );
    sp.ball_slim_meta.insert(
        1,
        SlimItemMeta {
            corp_id: 98_000_001,
            alliance_id: 0,
            ..Default::default()
        },
    );
    let state_kv = encode_set_state_payload(&sp, 1);
    if let PyRep::Dict(entries) = state_kv {
        let dmg_entry = entries
            .iter()
            .find(|(k, _)| matches!(k, PyRep::String(s) if s == "damageState"))
            .map(|(_, v)| v)
            .unwrap();
        if let PyRep::Dict(dmg_rows) = dmg_entry {
            assert!(dmg_rows.iter().any(|(k, _)| *k == PyRep::Integer(1)));
            assert!(!dmg_rows.iter().any(|(k, _)| *k == PyRep::Integer(500_001)));
        }
    }
}

#[test]
fn slim_item_dict_always_emits_non_empty_name_for_capsules_and_objects() {
    let cap_ball = sample_ball(777_001, MovementMode::Stop);
    let cap_dict = slim_item_dict(
        &cap_ball,
        &SlimItemMeta {
            group_id: 29,
            category_id: 6,
            owner_id: 90_000_001,
            char_id: Some(90_000_001),
            name: None,
            ..Default::default()
        },
    );
    assert_eq!(cap_dict.dict_get("name"), Some(&PyRep::String("Ship 777001".into())));

    let empty_name_obj = slim_item_dict(
        &sample_ball(777_002, MovementMode::Stop),
        &SlimItemMeta {
            group_id: 186,
            category_id: 2,
            owner_id: 90_000_001,
            name: Some(String::new()),
            ..Default::default()
        },
    );
    assert_eq!(
        empty_name_obj.dict_get("name"),
        Some(&PyRep::String("Item 777002".into()))
    );

    let zero_owner_asteroid = slim_item_dict(
        &sample_ball(777_003, MovementMode::Rigid),
        &SlimItemMeta {
            group_id: 462,
            category_id: 25,
            owner_id: 0,
            ..Default::default()
        },
    );
    assert_eq!(
        zero_owner_asteroid.dict_get("ownerID"),
        Some(&PyRep::Integer(1)),
        "zero owner_id on natural celestials/asteroids must serialize as invConst.ownerSystem (1)"
    );
    assert_eq!(
        cap_dict.dict_get("ownerID"),
        Some(&PyRep::Integer(90_000_001)),
        "non-zero player owner_id must be preserved verbatim"
    );
}

#[test]
fn non_free_stop_and_rigid_static_balls_encode_flags_omit_data_sector_and_resist_bumping() {
    let mut park = Ballpark::new(30_000_142);

    // 1. Static Rigid asteroid: system-wide global flag, no MassSector or DataSector (39 bytes),
    //    and keeps `has_dynamic_balls() == false` for LOD 0 hibernation.
    let ast_id = 750_000_001;
    let ast_pos = DVec3::new(300.0, 0.0, 0.0);
    park.add_static_ball_with_meta(
        Ball {
            id: ast_id,
            type_id: 1230,
            position: ast_pos,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 0.0,
            mass: 5.0e9,
            radius: 250.0,
            speed_fraction: 0.0,
            mode: MovementMode::Rigid,
        },
        SlimItemMeta {
            group_id: 462,
            category_id: 25,
            owner_id: 1,
            name: Some("Veldspar".into()),
            ..Default::default()
        },
    );
    assert!(!park.has_dynamic_balls());
    assert!(!park.is_ball_free(ast_id));

    // 2. Stationary pinned wreck (`mark_ball_non_free` before `add_ball`): `DST_BALL_STOP` with
    //    `BALL_FLAG_IS_MASSIVE | BALL_FLAG_IS_INTERACTIVE` (no `BALL_FLAG_IS_FREE`), 64 bytes (no 36-byte DataSector).
    let wreck_id = 880_001;
    let wreck_pos = DVec3::new(0.0, 300.0, 0.0);
    park.mark_ball_non_free(wreck_id, false);
    park.add_ball(Ball {
        id: wreck_id,
        type_id: 26_557,
        position: wreck_pos,
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 1.0,
        mass: 10_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });
    assert!(park.has_dynamic_balls());
    assert!(!park.is_ball_free(wreck_id));

    let ast_bytes = encode_add_balls_with_park(1, 7, &[&park.balls[&ast_id]], Some(&park));
    assert_eq!(ast_bytes.len(), 5 + 39);
    let (_, _, dec_ast) = decode_add_balls(&ast_bytes).expect("decode rigid asteroid");
    assert_eq!(dec_ast[0].mode, DST_BALL_RIGID);
    assert_eq!(dec_ast[0].flags, BALL_FLAG_IS_GLOBAL);
    assert_eq!(dec_ast[0].mass, None);
    assert_eq!(dec_ast[0].max_velocity, None);

    let wreck_bytes = encode_add_balls_with_park(1, 7, &[&park.balls[&wreck_id]], Some(&park));
    assert_eq!(wreck_bytes.len(), 5 + 64);
    let (_, _, dec_wreck) = decode_add_balls(&wreck_bytes).expect("decode non-free wreck");
    assert_eq!(dec_wreck[0].mode, DST_BALL_STOP);
    assert_eq!(
        dec_wreck[0].flags,
        BALL_FLAG_IS_MASSIVE | BALL_FLAG_IS_INTERACTIVE
    );
    assert_eq!(dec_wreck[0].flags & BALL_FLAG_IS_FREE, 0);
    assert_eq!(dec_wreck[0].max_velocity, None);
    assert_eq!(dec_wreck[0].velocity, None);

    // 3. Re-enabling `set_ball_free(wreck_id, true)` restores `BALL_FLAG_IS_FREE` and the 36-byte DataSector,
    //    and `set_ball_free(wreck_id, false)` pins it again.
    assert!(park.set_ball_free(wreck_id, true));
    assert!(park.is_ball_free(wreck_id));
    let free_bytes = encode_add_balls_with_park(1, 7, &[&park.balls[&wreck_id]], Some(&park));
    assert_eq!(free_bytes.len(), 5 + 100);
    assert!(park.set_ball_free(wreck_id, false));
    assert!(!park.is_ball_free(wreck_id));

    // 4. Ship ramming the asteroid or the pinned wreck bounces off without displacing either object.
    let rammer_ast = 880_010;
    park.add_ball(Ball {
        id: rammer_ast,
        type_id: 587,
        position: DVec3::new(10.0, 0.0, 0.0),
        velocity: DVec3::new(400.0, 0.0, 0.0),
        max_velocity: 400.0,
        inertia: 2.0,
        mass: 50_000_000.0,
        radius: 60.0,
        speed_fraction: 1.0,
        mode: MovementMode::GotoDirection {
            direction: DVec3::X,
        },
    });
    let rammer_wreck = 880_011;
    park.add_ball(Ball {
        id: rammer_wreck,
        type_id: 587,
        position: DVec3::new(0.0, 210.0, 0.0),
        velocity: DVec3::new(0.0, 400.0, 0.0),
        max_velocity: 400.0,
        inertia: 2.0,
        mass: 50_000_000.0,
        radius: 60.0,
        speed_fraction: 1.0,
        mode: MovementMode::GotoDirection {
            direction: DVec3::Y,
        },
    });
    park.step(1.0);
    assert_eq!(park.balls[&ast_id].position, ast_pos);
    assert_eq!(park.balls[&ast_id].velocity, DVec3::ZERO);
    assert_eq!(park.balls[&wreck_id].position, wreck_pos);
    assert_eq!(park.balls[&wreck_id].velocity, DVec3::ZERO);

    // 5. Tractor beam (`set_ball_position`) still repositions a pinned non-free wreck cleanly,
    //    and removing the static asteroid notifies grid observers via `RemoveBall`.
    let tractored_pos = DVec3::new(0.0, 150.0, 0.0);
    assert!(park.set_ball_position(wreck_id, tractored_pos));
    assert_eq!(park.balls[&wreck_id].position, tractored_pos);

    park.drain_all_observer_events();
    assert!(park.remove_ball(ast_id).is_some());
    let obs_evs = park.drain_observer_events(rammer_ast);
    assert!(obs_evs
        .iter()
        .any(|e| matches!(e, DestinyEvent::RemoveBall { ball_id } if *ball_id == ast_id)));
}

#[test]
fn warp_phases_fallbacks_and_park_metadata_edges_roundtrip() {
    // 1. WarpPhase::Aligning -> effect_stamp == -1, total_distance == -1.0
    let align_ball = sample_ball(
        501,
        MovementMode::Warp {
            origin: DVec3::ZERO,
            destination: DVec3::new(3.0e9, 4.0e9, 0.0),
            warp_speed_au_s: 4.5,
            total_distance: 5.0e9,
            phase: WarpPhase::Aligning,
            elapsed_warp_time: 2.0,
        },
    );
    // 2. WarpPhase::Cruising with total_distance == 0.0 (uses destination - origin) and elapsed_warp_time > stamp (clamped to 0)
    let cruise_zero_dist = sample_ball(
        502,
        MovementMode::Warp {
            origin: DVec3::ZERO,
            destination: DVec3::new(3.0e8, 4.0e8, 0.0),
            warp_speed_au_s: 3.0,
            total_distance: 0.0,
            phase: WarpPhase::Cruising,
            elapsed_warp_time: 99.0,
        },
    );
    // 3. Mushroom, Troll, Formation without park entries (unwrap_or fallbacks)
    let mush_def = sample_ball(503, MovementMode::Mushroom);
    let troll_def = sample_ball(504, MovementMode::Troll);
    let form_def = sample_ball(505, MovementMode::Formation);

    let refs = [&align_ball, &cruise_zero_dist, &mush_def, &troll_def, &form_def];
    let buf = encode_add_balls_with_type(1, 20, &refs);
    let dec = decode_add_balls(&buf);
    assert!(dec.is_ok());
    if let Ok((pkt, stamp, balls)) = dec {
        assert_eq!(pkt, 1);
        assert_eq!(stamp, 20);
        assert_eq!(balls.len(), 5);
        assert_eq!(balls[0].effect_stamp, Some(-1));
        assert_eq!(balls[0].warp_speed_milli_au_s, Some(4500));
        assert_eq!(balls[1].effect_stamp, Some(0));
        assert_eq!(balls[2].effect_stamp, Some(20));
        assert_eq!(balls[2].owner_id, Some(1));
        assert!((balls[2].follow_range.unwrap_or_default() - 42.0).abs() < 1e-3);
        assert_eq!(balls[3].effect_stamp, Some(20));
        assert_eq!(balls[4].target_id, Some(0));
        assert_eq!(balls[4].effect_stamp, Some(20));
    }

    // 4. Negative corp_id clamped to 0, non-positive alliance_id -> -1, combat_states cloak without slim_meta
    let mut buf_neg = Vec::new();
    let neg_meta = SlimItemMeta {
        corp_id: -50,
        alliance_id: 0,
        is_cloaked: false,
        ..Default::default()
    };
    encode_single_ball_with_meta(&mut buf_neg, &mush_def, 10, Some(&neg_meta));
    let mut full_neg = vec![0u8];
    full_neg.extend_from_slice(&10u32.to_le_bytes());
    full_neg.extend_from_slice(&buf_neg);
    let (_, _, neg_dec) = decode_add_balls(&full_neg).expect("decode neg meta");
    assert_eq!(neg_dec[0].corp_id, Some(0));
    assert_eq!(neg_dec[0].alliance_id, Some(-1));

    // Rigid ball truncation checks across all prefix cuts & multi-ball second-ball truncation
    let rigid = sample_ball(60_000_001, MovementMode::Rigid);
    let rigid_buf = encode_add_balls(1, &[&rigid]);
    assert_eq!(decode_add_balls(&rigid_buf[..5]).unwrap(), (0, 1, vec![]));
    for cut in 6..rigid_buf.len() {
        assert!(decode_add_balls(&rigid_buf[..cut]).is_err());
    }
    let two_buf = encode_add_balls(1, &[&rigid, &mush_def]);
    assert!(decode_add_balls(&two_buf[..two_buf.len() - 3]).is_err());
}

#[test]
fn slim_item_meta_boundaries_wire_type_remapping_and_remaining_events() {
    let mut park = Ballpark::new(30_000_142);

    // 1. park.ball_slim_meta and ball_group_id category mappings
    let mut b_gate = sample_ball(1001, MovementMode::Rigid);
    b_gate.type_id = 0; // remapped to 6 in slim_item_dict
    park.add_static_ball(b_gate.clone(), 10);
    assert_eq!(resolve_slim_item_meta(Some(&park), &b_gate).category_id, 2);

    let b_station = sample_ball(1002, MovementMode::Rigid);
    park.add_static_ball(b_station.clone(), 15);
    assert_eq!(resolve_slim_item_meta(Some(&park), &b_station).category_id, 3);

    let b_other = sample_ball(1003, MovementMode::Rigid);
    park.set_ball_group_id(b_other.id, 25);
    assert_eq!(resolve_slim_item_meta(Some(&park), &b_other).category_id, 6);

    // 2. Rigid heuristics boundaries without park
    let gate_lo = sample_ball(50_000_000, MovementMode::Rigid);
    let gate_hi = sample_ball(59_999_999, MovementMode::Rigid);
    let st_lo = sample_ball(60_000_000, MovementMode::Rigid);
    let st_hi = sample_ball(69_999_999, MovementMode::Rigid);
    let beyond_hi = sample_ball(70_000_000, MovementMode::Rigid);
    let mut abyss_gate = sample_ball(40_000_010, MovementMode::Rigid);
    abyss_gate.type_id = 32882;
    assert_eq!(resolve_slim_item_meta(None, &gate_lo).group_id, 10);
    assert_eq!(resolve_slim_item_meta(None, &gate_hi).group_id, 10);
    assert_eq!(resolve_slim_item_meta(None, &st_lo).group_id, 15);
    assert_eq!(resolve_slim_item_meta(None, &st_hi).group_id, 15);
    assert_eq!(resolve_slim_item_meta(None, &beyond_hi).group_id, 6);
    assert_eq!(resolve_slim_item_meta(None, &abyss_gate).group_id, 4079);

    // 3. slim_item_dict remaps type_id 0->6, 32882/56002->55914, war_faction_id, empty gate jumps, wormhole defaults
    let gate_no_jump_meta = SlimItemMeta {
        group_id: 10,
        category_id: 2,
        war_faction_id: Some(500_001),
        jump_target: None,
        ..Default::default()
    };
    let gate_dict = slim_item_dict(&b_gate, &gate_no_jump_meta);
    assert_eq!(gate_dict.dict_get("typeID"), Some(&PyRep::Integer(6)));
    assert_eq!(
        gate_dict.dict_get("warFactionID"),
        Some(&PyRep::Integer(500_001))
    );
    assert_eq!(gate_dict.dict_get("jumps"), Some(&PyRep::List(Vec::new())));

    let abyss_dict = slim_item_dict(&abyss_gate, &resolve_slim_item_meta(None, &abyss_gate));
    assert_eq!(abyss_dict.dict_get("typeID"), Some(&PyRep::Integer(55914)));

    let wh_meta = SlimItemMeta {
        group_id: 988,
        category_id: 2,
        war_faction_id: Some(0),
        ..Default::default()
    };
    let wh_obj = encode_slim_item_with_meta(&b_other, &wh_meta);
    if let PyRep::Object { args, .. } = wh_obj {
        assert_eq!(args.dict_get("warFactionID"), Some(&PyRep::None));
        assert_eq!(args.dict_get("wormholeAge"), Some(&PyRep::Integer(0)));
        assert_eq!(args.dict_get("wormholeSize"), Some(&PyRep::Real(1.0)));
    } else {
        panic!("expected foo.SlimItem Object");
    }

    // 4. encode_damage_state_tuple: None fallback, zero max_*_hp guard, clamping
    let def_dmg = encode_damage_state_tuple(None, 12);
    if let PyRep::Tuple(top) = def_dmg {
        assert_eq!(
            top[0],
            PyRep::Tuple(vec![
                PyRep::Real(1.0),
                PyRep::Real(24_000.0),
                PyRep::Integer(12),
            ])
        );
        assert_eq!(top[1], PyRep::Real(1.0));
        assert_eq!(top[2], PyRep::Real(1.0));
    }
    let clamped_cs = CombatState {
        shield_hp: 999.0,
        max_shield_hp: 0.0,
        armor_hp: -5.0,
        max_armor_hp: 0.0,
        structure_hp: 50.0,
        max_structure_hp: 100.0,
        shield_recharge_time_ms: 45_000.0,
        ..Default::default()
    };
    let clamped_dmg = encode_damage_state_tuple(Some(&clamped_cs), 15);
    if let PyRep::Tuple(top) = clamped_dmg {
        assert_eq!(
            top[0],
            PyRep::Tuple(vec![
                PyRep::Real(1.0),
                PyRep::Real(9_000.0),
                PyRep::Integer(15),
            ])
        );
        assert_eq!(top[1], PyRep::Real(0.0));
        assert_eq!(top[2], PyRep::Real(0.5));
    }

    // 5. Uncovered DestinyEvent branches: SetMaxSpeed, CloakBall(0), OnDotVictimUpdated(0 & auto-expiry),
    //    SetBallHarmonic with positive alliance/corp, OnDamageStateChange/OnFleetDamageStateChange with park tau
    park.combat_states.insert(1003, clamped_cs);
    park.ball_slim_meta.insert(
        1003,
        SlimItemMeta {
            alliance_id: 99_000_007,
            corp_id: 98_000_007,
            ..Default::default()
        },
    );

    let max_spd = encode_destiny_event(
        5,
        &DestinyEvent::SetMaxSpeed {
            ball_id: 1003,
            max_speed: 420.5,
        },
        None,
    );
    assert_eq!(
        max_spd,
        PyRep::Tuple(vec![
            PyRep::Integer(5),
            PyRep::Tuple(vec![
                PyRep::String("SetMaxSpeed".into()),
                PyRep::Tuple(vec![PyRep::Integer(1003), PyRep::Real(420.5)]),
            ]),
        ])
    );

    let uncloak = encode_destiny_event(
        5,
        &DestinyEvent::CloakBall {
            ball_id: 1003,
            cloaked: 0,
        },
        None,
    );
    if let PyRep::Tuple(outer) = uncloak {
        if let PyRep::Tuple(call) = &outer[1] {
            assert_eq!(
                call[1],
                PyRep::Tuple(vec![PyRep::Integer(1003), PyRep::Bool(false)])
            );
        }
    }

    let dot_empty = encode_destiny_event(
        5,
        &DestinyEvent::OnDotVictimUpdated {
            ball_id: 1003,
            flat_dps: 10.0,
            pct_dps: 0.01,
            remaining_ticks: 0,
            expiry_filetime: 0,
        },
        Some(&park),
    );
    if let PyRep::Tuple(outer) = dot_empty {
        if let PyRep::Tuple(call) = &outer[1] {
            if let PyRep::Tuple(args) = &call[1] {
                assert_eq!(args[1], PyRep::PySet(Vec::new()));
            }
        }
    }

    let dot_auto = encode_destiny_event(
        5,
        &DestinyEvent::OnDotVictimUpdated {
            ball_id: 1003,
            flat_dps: 25.0,
            pct_dps: 0.02,
            remaining_ticks: 5,
            expiry_filetime: 0,
        },
        Some(&park),
    );
    if let PyRep::Tuple(outer) = dot_auto {
        if let PyRep::Tuple(call) = &outer[1] {
            if let PyRep::Tuple(args) = &call[1] {
                if let PyRep::PySet(items) = &args[1] {
                    assert_eq!(items.len(), 1);
                    assert_eq!(items[0].dict_get("maxDamage"), Some(&PyRep::Real(25.0)));
                } else {
                    panic!("expected PySet");
                }
            }
        }
    }

    let harm_pos = encode_destiny_event(
        5,
        &DestinyEvent::SetBallHarmonic {
            ball_id: 1003,
            harmonic: 2,
        },
        Some(&park),
    );
    if let PyRep::Tuple(outer) = harm_pos {
        if let PyRep::Tuple(call) = &outer[1] {
            assert_eq!(
                call[1],
                PyRep::Tuple(vec![
                    PyRep::Integer(1003),
                    PyRep::Integer(99_000_007),
                    PyRep::Integer(98_000_007),
                    PyRep::Integer(-1),
                    PyRep::Integer(2),
                ])
            );
        }
    }

    let dmg_park = encode_destiny_event(
        7,
        &DestinyEvent::OnDamageStateChange {
            ball_id: 1003,
            shield_ratio: 0.6,
            armor_ratio: 0.7,
            structure_ratio: 0.8,
        },
        Some(&park),
    );
    if let PyRep::Tuple(outer) = dmg_park {
        if let PyRep::Tuple(call) = &outer[1] {
            if let PyRep::Tuple(args) = &call[1] {
                if let PyRep::Tuple(state) = &args[1] {
                    assert_eq!(
                        state[0],
                        PyRep::Tuple(vec![
                            PyRep::Real(0.6),
                            PyRep::Real(9_000.0),
                            PyRep::Integer(7),
                        ])
                    );
                }
            }
        }
    }
}

#[test]
fn issue_871_cloaked_spawn_suppresses_add_balls2_and_avoids_repeated_remove_balls() {
    let mut park = Ballpark::new(30_000_142);
    let obs = sample_ball(100, MovementMode::Stop);
    park.add_ball(obs);
    let _ = park.drain_observer_events(100);
    park.pending_events.clear();

    // 1. Insert combat_state with is_cloaked = true BEFORE add_ball (#871 spawn leak)
    let mut cloaked_cs = CombatState::default();
    cloaked_cs.is_cloaked = true;
    park.combat_states.insert(200, cloaked_cs);
    let mut cloaked_ship = sample_ball(200, MovementMode::Stop);
    cloaked_ship.position = DVec3::new(15_000.0, 20_000.0, 30_000.0);
    park.add_ball(cloaked_ship);

    assert!(
        park.drain_observer_events(100).is_empty(),
        "Observer must not receive AddBalls2 when a cloaked ship spawns on grid (#871)"
    );
    assert!(
        park.pending_events.is_empty(),
        "Global pending_events must not announce AddBall for a cloaked ship (#871)"
    );

    // 2. Step physics multiple ticks: observer must NOT receive RemoveBalls every tick (#871)
    for _ in 0..3 {
        let _ = park.step(1.0);
        let evs = park.drain_observer_events(100);
        assert!(
            !evs.iter().any(|e| matches!(e, DestinyEvent::RemoveBalls { .. })),
            "Observer must not receive repeated RemoveBalls while ball 200 remains cloaked: {evs:?}"
        );
    }

    // 3. Decloak and re-cloak transitions emit AddBalls2 once and RemoveBalls once, never repeating
    assert!(park.deactivate_cloak(200));
    let decloak_evs = park.drain_observer_events(100);
    assert_eq!(
        decloak_evs
            .iter()
            .filter(|e| matches!(e, DestinyEvent::AddBalls2 { .. }))
            .count(),
        1
    );
    let _ = park.step(1.0);
    assert!(
        !park
            .drain_observer_events(100)
            .iter()
            .any(|e| matches!(e, DestinyEvent::AddBalls2 { .. } | DestinyEvent::RemoveBalls { .. }))
    );

    park.activate_cloak(200, true, 5.0, 900.0).expect("activate_cloak");
    let cloak_evs = park.drain_observer_events(100);
    assert_eq!(
        cloak_evs
            .iter()
            .filter(|e| matches!(e, DestinyEvent::RemoveBalls { .. }))
            .count(),
        1
    );
    for _ in 0..3 {
        let _ = park.step(1.0);
        let evs = park.drain_observer_events(100);
        assert!(
            !evs.iter().any(|e| matches!(e, DestinyEvent::RemoveBalls { .. })),
            "Subsequent ticks after activate_cloak must not re-emit RemoveBalls: {evs:?}"
        );
    }
}

#[test]
fn mutant_killers_ball_flags_stargate_owner_gt_1_and_set_state_static_damage() {
    // 1. encode_single_ball_with_park exact flag byte (byte 37): Stop normal, Stop non_free, Field + interactive override
    let mut park = Ballpark::new(30_000_142);
    let stop_free = sample_ball(501, MovementMode::Stop);
    park.add_ball(stop_free.clone());
    let mut buf1 = Vec::new();
    encode_single_ball_with_park(&mut buf1, &stop_free, 0, None, Some(&park));
    assert_eq!(
        buf1[37],
        BALL_FLAG_IS_FREE | BALL_FLAG_IS_MASSIVE | BALL_FLAG_IS_INTERACTIVE
    );

    park.non_free_ball_ids.insert(501);
    let mut buf2 = Vec::new();
    encode_single_ball_with_park(&mut buf2, &stop_free, 0, None, Some(&park));
    assert_eq!(buf2[37], BALL_FLAG_IS_MASSIVE | BALL_FLAG_IS_INTERACTIVE);

    let field_ball = sample_ball(502, MovementMode::Field);
    park.add_ball(field_ball.clone());
    park.ball_interactive_overrides.insert(502, true);
    let mut buf3 = Vec::new();
    encode_single_ball_with_park(&mut buf3, &field_ball, 0, None, Some(&park));
    assert_eq!(buf3[37], BALL_FLAG_IS_GLOBAL | BALL_FLAG_IS_INTERACTIVE);

    // 2. slim_item_dict group_id == 10 stargate owner IDs filter `> 1` (ID == 1 must yield PyRep::None; ID == 500_001 -> Integer)
    let gate_ball = sample_ball(601, MovementMode::Rigid);
    let meta_sys = SlimItemMeta {
        group_id: 10,
        owner_id: 1,
        origin_system_owner_id: Some(1),
        dest_system_owner_id: Some(1),
        ..Default::default()
    };
    let PyRep::Dict(d_sys) = slim_item_dict(&gate_ball, &meta_sys) else {
        panic!("expected Dict");
    };
    let find_k = |entries: &[(PyRep, PyRep)], key: &str| -> PyRep {
        entries
            .iter()
            .find(|(k, _)| matches!(k, PyRep::String(s) if s == key))
            .map(|(_, v)| v.clone())
            .unwrap()
    };
    assert_eq!(find_k(&d_sys, "originSystemOwnerID"), PyRep::None);
    assert_eq!(find_k(&d_sys, "destinationSystemOwnerID"), PyRep::None);

    let meta_fac = SlimItemMeta {
        group_id: 10,
        owner_id: 500_001,
        origin_system_owner_id: None,
        dest_system_owner_id: Some(500_002),
        ..Default::default()
    };
    let PyRep::Dict(d_fac) = slim_item_dict(&gate_ball, &meta_fac) else {
        panic!("expected Dict");
    };
    assert_eq!(find_k(&d_fac, "originSystemOwnerID"), PyRep::Integer(500_001));
    assert_eq!(find_k(&d_fac, "destinationSystemOwnerID"), PyRep::Integer(500_002));

    // 3. encode_set_state_payload damageState inclusion: static without combat excluded, static WITH combat included
    let mut sp = Ballpark::new(30_000_142);
    let mut obs = sample_ball(701, MovementMode::Stop);
    obs.position = DVec3::ZERO;
    sp.add_ball(obs);
    let mut st_no_combat = sample_ball(702, MovementMode::Rigid);
    st_no_combat.position = DVec3::new(1_000.0, 0.0, 0.0);
    sp.add_static_ball(st_no_combat, 0);
    let mut st_with_combat = sample_ball(703, MovementMode::Rigid);
    st_with_combat.position = DVec3::new(2_000.0, 0.0, 0.0);
    sp.add_static_ball(st_with_combat, 0);
    sp.combat_states.insert(703, CombatState::default());

    let PyRep::KeyVal(kv) = encode_set_state_payload(&sp, 701) else {
        panic!("expected SetState KeyVal");
    };
    let PyRep::Dict(dmg_entries) = kv
        .into_iter()
        .find(|(k, _)| k == "damageState")
        .unwrap()
        .1
    else {
        panic!("expected damageState Dict");
    };
    let dmg_ids: Vec<i64> = dmg_entries.iter().filter_map(|(k, _)| k.as_i64()).collect();
    assert!(dmg_ids.contains(&701));
    assert!(!dmg_ids.contains(&702), "static without combat must NOT appear in damageState");
    assert!(dmg_ids.contains(&703), "static WITH combat MUST appear in damageState");
}
