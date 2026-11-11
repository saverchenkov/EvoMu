//! Edge-case unit tests for `wire_decode`: header validation, truncation at every
//! binary field offset, rigid vs non-rigid flags, global vs free sectors, all 13 modes,
//! and unknown mode rejection.

use super::*;
use crate::wire::{
    encode_add_balls_with_park, encode_add_balls_with_type, BALL_FLAG_IS_FREE,
    BALL_FLAG_IS_GLOBAL, BALL_FLAG_IS_INTERACTIVE, BALL_FLAG_IS_MASSIVE,
};
use crate::{
    Ball, Ballpark, CombatState, FormationState, MovementMode, MushroomState, SlimItemMeta,
    TrollState, WarpPhase,
};
use glam::DVec3;

fn make_ball(id: i64, mode: MovementMode) -> Ball {
    Ball {
        id,
        type_id: 587,
        position: DVec3::new(123.4, -567.8, 901.2),
        velocity: DVec3::new(11.0, 22.0, -33.0),
        max_velocity: 410.0,
        inertia: 2.25,
        mass: 1_500_000.0,
        radius: 38.5,
        speed_fraction: 0.65,
        mode,
    }
}

#[test]
fn short_buffers_and_empty_payload() {
    for len in 0..5 {
        let buf = vec![0u8; len];
        let err = decode_add_balls(&buf).unwrap_err();
        assert!(
            err.contains("too short for 5-byte header"),
            "len={len} err={err}"
        );
    }

    let header_only = encode_add_balls_with_type(1, 0x1234_5678, &[]);
    assert_eq!(header_only.len(), 5);
    let (ptype, stamp, balls) = decode_add_balls(&header_only).unwrap();
    assert_eq!(ptype, 1);
    assert_eq!(stamp, 0x1234_5678);
    assert!(balls.is_empty());
}

#[test]
fn unknown_mode_byte_returns_error() {
    let mut buf = encode_add_balls_with_type(0, 10, &[]);
    // id (8) + mode (1) + radius (4) + pos (24) + flags (1) + mass (8) + cloak (1) +
    // alliance (8) + corp (4) + harmonic (4) + formation_id (1)
    buf.extend_from_slice(&999_i64.to_le_bytes());
    buf.push(99u8); // unknown mode
    buf.extend_from_slice(&40.0f32.to_le_bytes());
    buf.extend_from_slice(&1.0f64.to_le_bytes());
    buf.extend_from_slice(&2.0f64.to_le_bytes());
    buf.extend_from_slice(&3.0f64.to_le_bytes());
    buf.push(0u8); // flags = 0 (not IS_FREE)
    buf.extend_from_slice(&1000.0f64.to_le_bytes()); // mass
    buf.push(0u8); // cloak
    buf.extend_from_slice(&(-1i64).to_le_bytes()); // alliance
    buf.extend_from_slice(&0i32.to_le_bytes()); // corp
    buf.extend_from_slice(&(-1i32).to_le_bytes()); // harmonic
    buf.push(0xFFu8); // formation_id

    let err = decode_add_balls(&buf).unwrap_err();
    assert!(err.contains("Unknown Destiny ball mode byte 99"), "err={err}");
}

#[test]
fn rigid_ball_skips_mass_and_data_sectors_even_if_is_free_flag_set() {
    let mut buf = encode_add_balls_with_type(0, 77, &[]);
    buf.extend_from_slice(&10_001_i64.to_le_bytes());
    buf.push(DST_BALL_RIGID);
    buf.extend_from_slice(&500.0f32.to_le_bytes());
    buf.extend_from_slice(&10.0f64.to_le_bytes());
    buf.extend_from_slice(&20.0f64.to_le_bytes());
    buf.extend_from_slice(&30.0f64.to_le_bytes());
    // Set IS_FREE bit explicitly on RIGID; decoder must still skip mass & data sectors.
    buf.push(BALL_FLAG_IS_FREE | BALL_FLAG_IS_GLOBAL);
    buf.push(0xAAu8); // formation_id

    let (ptype, stamp, balls) = decode_add_balls(&buf).unwrap();
    assert_eq!(ptype, 0);
    assert_eq!(stamp, 77);
    assert_eq!(balls.len(), 1);
    let b = &balls[0];
    assert_eq!(b.id, 10_001);
    assert_eq!(b.mode, DST_BALL_RIGID);
    assert!(b.mass.is_none());
    assert!(b.cloak.is_none());
    assert!(b.alliance_id.is_none());
    assert!(b.corp_id.is_none());
    assert!(b.harmonic.is_none());
    assert!(b.max_velocity.is_none());
    assert!(b.velocity.is_none());
    assert!(b.inertia.is_none());
    assert!(b.speed_fraction.is_none());
    assert_eq!(b.formation_id, 0xAA);
}

#[test]
fn all_modes_with_park_metadata_and_exhaustive_truncation_errors() {
    let mut park = Ballpark::new(30_000_142);
    let balls = vec![
        make_ball(
            1,
            MovementMode::GotoDirection {
                direction: DVec3::new(0.5, -0.5, 0.7),
            },
        ),
        make_ball(
            2,
            MovementMode::Follow {
                target_id: 1,
                range: 1_500.0,
            },
        ),
        make_ball(
            3,
            MovementMode::Orbit {
                target_id: 1,
                range: 5_000.0,
            },
        ),
        make_ball(
            4,
            MovementMode::Warp {
                origin: DVec3::ZERO,
                destination: DVec3::new(1e9, 2e9, 3e9),
                warp_speed_au_s: 4.5,
                total_distance: 3.74e9,
                phase: WarpPhase::Aligning,
                elapsed_warp_time: 0.0,
            },
        ),
        make_ball(5, MovementMode::Missile { target_id: 2 }),
        make_ball(6, MovementMode::Mushroom),
        make_ball(7, MovementMode::Troll),
        make_ball(8, MovementMode::Formation),
        make_ball(9, MovementMode::Field),
        make_ball(10, MovementMode::Rigid),
        make_ball(11, MovementMode::Stop),
        make_ball(12, MovementMode::Boid),
        make_ball(13, MovementMode::Miniball),
    ];

    for b in &balls {
        park.add_ball(b.clone());
    }
    park.ball_harmonics.insert(1, 3);
    park.combat_states.insert(
        1,
        CombatState {
            is_cloaked: true,
            ..CombatState::default()
        },
    );
    park.set_ball_slim_meta(
        1,
        SlimItemMeta {
            owner_id: 500,
            corp_id: 98_000_001,
            alliance_id: 99_000_001,
            ..SlimItemMeta::default()
        },
    );
    park.mushroom_states.insert(
        6,
        MushroomState {
            max_radius: 15_000.0,
            duration_s: 8.0,
            effect_stamp: 55,
            owner_id: 777,
        },
    );
    park.troll_states.insert(7, TrollState { effect_stamp: 88 });
    park.formation_states.insert(
        8,
        FormationState {
            leader_id: 1,
            slot_offset: DVec3::ZERO,
            follow_range: 750.0,
            effect_stamp: 99,
        },
    );

    let refs: Vec<&Ball> = balls.iter().collect();
    let encoded = encode_add_balls_with_park(1, 100, &refs, Some(&park));
    let (ptype, stamp, decoded) = decode_add_balls(&encoded).unwrap();
    assert_eq!(ptype, 1);
    assert_eq!(stamp, 100);
    assert_eq!(decoded.len(), 13);

    // Ball 1: cloaked, corp/alliance/harmonic populated, free flag
    assert_eq!(decoded[0].cloak, Some(1));
    assert_eq!(decoded[0].alliance_id, Some(99_000_001));
    assert_eq!(decoded[0].corp_id, Some(98_000_001));
    assert_eq!(decoded[0].harmonic, Some(3));
    assert_eq!(
        decoded[0].flags,
        BALL_FLAG_IS_FREE | BALL_FLAG_IS_MASSIVE | BALL_FLAG_IS_INTERACTIVE
    );
    assert_eq!(decoded[0].velocity, Some(DVec3::new(11.0, 22.0, -33.0)));

    // Ball 4: Aligning warp encodes effect_stamp = -1
    assert_eq!(decoded[3].mode, DST_BALL_WARP);
    assert_eq!(decoded[3].effect_stamp, Some(-1));
    assert_eq!(decoded[3].warp_speed_milli_au_s, Some(4500));

    // Ball 6: Mushroom state
    assert_eq!(decoded[5].mode, DST_BALL_MUSHROOM);
    assert_eq!(decoded[5].owner_id, Some(777));
    assert_eq!(decoded[5].effect_stamp, Some(55));
    assert!((decoded[5].follow_range.unwrap() - 15_000.0).abs() < 1e-2);

    // Ball 7: Troll state
    assert_eq!(decoded[6].mode, DST_BALL_TROLL);
    assert_eq!(decoded[6].effect_stamp, Some(88));

    // Ball 8: Formation state
    assert_eq!(decoded[7].mode, DST_BALL_FORMATION);
    assert_eq!(decoded[7].target_id, Some(1));
    assert!((decoded[7].follow_range.unwrap() - 750.0).abs() < 1e-2);
    assert_eq!(decoded[7].effect_stamp, Some(99));

    // Ball 9: Field has MassSector but no DataSector (IS_GLOBAL only)
    assert_eq!(decoded[8].mode, DST_BALL_FIELD);
    assert_eq!(decoded[8].flags, BALL_FLAG_IS_GLOBAL);
    assert!(decoded[8].mass.is_some());
    assert!(decoded[8].max_velocity.is_none());

    // Compute exact prefix boundaries where valid whole-ball sequences end
    let mut valid_boundaries = std::collections::HashSet::new();
    for count in 0..=refs.len() {
        let prefix = encode_add_balls_with_park(1, 100, &refs[..count], Some(&park));
        valid_boundaries.insert(prefix.len());
    }

    // Every non-boundary truncation must fail cleanly with Unexpected EOF or header error
    let mut saw_eof_u8 = false;
    let mut saw_eof_i32 = false;
    let mut saw_eof_i64 = false;
    let mut saw_eof_f32 = false;
    let mut saw_eof_f64 = false;
    for trunc_len in 0..encoded.len() {
        if valid_boundaries.contains(&trunc_len) {
            assert!(decode_add_balls(&encoded[..trunc_len]).is_ok());
        } else {
            let err = decode_add_balls(&encoded[..trunc_len]).unwrap_err();
            if err.contains("reading u8") {
                saw_eof_u8 = true;
            } else if err.contains("reading i32") {
                saw_eof_i32 = true;
            } else if err.contains("reading i64") {
                saw_eof_i64 = true;
            } else if err.contains("reading f32") {
                saw_eof_f32 = true;
            } else if err.contains("reading f64") {
                saw_eof_f64 = true;
            }
        }
    }
    assert!(saw_eof_u8 && saw_eof_i32 && saw_eof_i64 && saw_eof_f32 && saw_eof_f64);
}
