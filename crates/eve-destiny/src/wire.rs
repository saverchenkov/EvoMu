//! Binary `Destiny` wire-format encoder/decoder (`AddBalls` / `AddBalls2` / `SetState` state buffer)
//! and `DoDestinyUpdate` `PyRep` tuple marshaling (`P-7`).

use crate::{Ball, BallId, Ballpark, CombatState, DestinyEvent, MovementMode, SlimItemMeta};
use eve_proto::PyRep;

// parity: plan:P-7, feat:navigation.destiny-updates, notify:DoDestinyUpdate
// parity: destiny:AddBalls2, destiny:RemoveBall, destiny:RemoveBalls
// parity: destiny:GotoDirection, destiny:FollowBall, destiny:Orbit, destiny:Stop

/// All 13 Ball mode byte values (`Destiny::Ball::Mode` in `DestinyStructs.h`).
pub const DST_BALL_GOTO: u8 = 0;
pub const DST_BALL_FOLLOW: u8 = 1;
pub const DST_BALL_STOP: u8 = 2;
pub const DST_BALL_WARP: u8 = 3;
pub const DST_BALL_ORBIT: u8 = 4;
pub const DST_BALL_MISSILE: u8 = 5;
pub const DST_BALL_MUSHROOM: u8 = 6;
pub const DST_BALL_BOID: u8 = 7;
pub const DST_BALL_TROLL: u8 = 8;
pub const DST_BALL_MINIBALL: u8 = 9;
pub const DST_BALL_FIELD: u8 = 10;
pub const DST_BALL_RIGID: u8 = 11;
pub const DST_BALL_FORMATION: u8 = 12;

/// Canonical aliases matching `DSTBALL_*` names.
pub const DSTBALL_GOTO: u8 = DST_BALL_GOTO;
pub const DSTBALL_FOLLOW: u8 = DST_BALL_FOLLOW;
pub const DSTBALL_STOP: u8 = DST_BALL_STOP;
pub const DSTBALL_WARP: u8 = DST_BALL_WARP;
pub const DSTBALL_ORBIT: u8 = DST_BALL_ORBIT;
pub const DSTBALL_MISSILE: u8 = DST_BALL_MISSILE;
pub const DSTBALL_MUSHROOM: u8 = DST_BALL_MUSHROOM;
pub const DSTBALL_BOID: u8 = DST_BALL_BOID;
pub const DSTBALL_TROLL: u8 = DST_BALL_TROLL;
pub const DSTBALL_MINIBALL: u8 = DST_BALL_MINIBALL;
pub const DSTBALL_FIELD: u8 = DST_BALL_FIELD;
pub const DSTBALL_RIGID: u8 = DST_BALL_RIGID;
pub const DSTBALL_FORMATION: u8 = DST_BALL_FORMATION;

/// Ball flags (`Destiny::Ball::Flag` in `DestinyStructs.h`).
pub const BALL_FLAG_IS_FREE: u8 = 0x01;
pub const BALL_FLAG_IS_GLOBAL: u8 = 0x02;
pub const BALL_FLAG_IS_MASSIVE: u8 = 0x04;
pub const BALL_FLAG_IS_INTERACTIVE: u8 = 0x08;
pub const BALL_FLAG_IS_MORIBUND: u8 = 0x10;
pub const BALL_FLAG_HAS_MINIBALLS: u8 = 0x40;

#[path = "wire_decode.rs"]
mod wire_decode;
pub use wire_decode::{decode_add_balls, DecodedBall};

/// Encodes a binary `AddBalls` / `SetState` buffer (`packet_type = 0`).
pub fn encode_add_balls(stamp: u32, balls: &[&Ball]) -> Vec<u8> {
    encode_add_balls_with_type(0, stamp, balls)
}

/// Encodes a binary `AddBalls` buffer with explicit `packet_type` (`0` = `SetState`, `1` = `AddBalls2`).
pub fn encode_add_balls_with_type(packet_type: u8, stamp: u32, balls: &[&Ball]) -> Vec<u8> {
    encode_add_balls_with_park(packet_type, stamp, balls, None)
}

/// Encodes a binary `AddBalls` buffer sourcing `MassSector` metadata (`allianceID`, `corporationID`, `cloak`) from `park`.
pub fn encode_add_balls_with_park(
    packet_type: u8,
    stamp: u32,
    balls: &[&Ball],
    park: Option<&Ballpark>,
) -> Vec<u8> {
    let mut buf = Vec::with_capacity(24 + balls.len() * 128);
    buf.push(packet_type);
    buf.extend_from_slice(&stamp.to_le_bytes());

    for ball in balls {
        let mut meta = park.and_then(|p| p.ball_slim_meta(ball.id)).cloned();
        if let Some(p) = park {
            if let Some(cs) = p.combat_states.get(&ball.id) {
                if let Some(ref mut m) = meta {
                    m.is_cloaked = cs.is_cloaked;
                }
            }
        }
        encode_single_ball_with_park(&mut buf, ball, stamp, meta.as_ref(), park);
    }

    buf
}

/// Encodes a single `Ball` entry into a binary `Destiny` buffer.
pub fn encode_single_ball(buf: &mut Vec<u8>, ball: &Ball) {
    encode_single_ball_with_meta(buf, ball, 0, None);
}

/// Encodes a single `Ball` entry into a binary `Destiny` buffer matching `DestinyStructs.h` layout.
pub fn encode_single_ball_with_meta(
    buf: &mut Vec<u8>,
    ball: &Ball,
    stamp: u32,
    meta: Option<&SlimItemMeta>,
) {
    encode_single_ball_with_park(buf, ball, stamp, meta, None);
}

/// Encodes a single `Ball` entry into a binary `Destiny` buffer, reading mode-specific state from `park` when available.
pub fn encode_single_ball_with_park(
    buf: &mut Vec<u8>,
    ball: &Ball,
    stamp: u32,
    meta: Option<&SlimItemMeta>,
    park: Option<&Ballpark>,
) {
    let is_rigid = matches!(ball.mode, MovementMode::Rigid);
    let (mode_byte, mut flags) = match ball.mode {
        MovementMode::GotoDirection { .. } => (
            DST_BALL_GOTO,
            BALL_FLAG_IS_FREE | BALL_FLAG_IS_MASSIVE | BALL_FLAG_IS_INTERACTIVE,
        ),
        MovementMode::Follow { .. } => (
            DST_BALL_FOLLOW,
            BALL_FLAG_IS_FREE | BALL_FLAG_IS_MASSIVE | BALL_FLAG_IS_INTERACTIVE,
        ),
        MovementMode::Stop => {
            let is_non_free = park.is_some_and(|p| p.non_free_ball_ids.contains(&ball.id));
            let stop_flags = if is_non_free {
                BALL_FLAG_IS_MASSIVE | BALL_FLAG_IS_INTERACTIVE
            } else {
                BALL_FLAG_IS_FREE | BALL_FLAG_IS_MASSIVE | BALL_FLAG_IS_INTERACTIVE
            };
            (DST_BALL_STOP, stop_flags)
        }
        MovementMode::Warp { .. } => (
            DST_BALL_WARP,
            BALL_FLAG_IS_FREE | BALL_FLAG_IS_MASSIVE | BALL_FLAG_IS_INTERACTIVE,
        ),
        MovementMode::Orbit { .. } => (
            DST_BALL_ORBIT,
            BALL_FLAG_IS_FREE | BALL_FLAG_IS_MASSIVE | BALL_FLAG_IS_INTERACTIVE,
        ),
        MovementMode::Missile { .. } => (DST_BALL_MISSILE, BALL_FLAG_IS_FREE),
        MovementMode::Mushroom => (DST_BALL_MUSHROOM, BALL_FLAG_IS_FREE),
        MovementMode::Boid => (DST_BALL_BOID, BALL_FLAG_IS_FREE),
        MovementMode::Troll => (DST_BALL_TROLL, BALL_FLAG_IS_FREE),
        MovementMode::Miniball => (DST_BALL_MINIBALL, BALL_FLAG_IS_FREE),
        MovementMode::Field => (DST_BALL_FIELD, BALL_FLAG_IS_GLOBAL),
        MovementMode::Rigid => (DST_BALL_RIGID, BALL_FLAG_IS_GLOBAL),
        MovementMode::Formation => (DST_BALL_FORMATION, BALL_FLAG_IS_FREE),
    };
    if let Some(is_inter) = park.and_then(|p| p.ball_interactive_overrides.get(&ball.id).copied()) {
        if is_inter {
            flags |= BALL_FLAG_IS_INTERACTIVE;
        } else {
            flags &= !BALL_FLAG_IS_INTERACTIVE;
        }
    }

    // BallHeader (38 bytes): entityID(i64), mode(u8), radius(f32), posX(f64), posY(f64), posZ(f64), flags(u8)
    buf.extend_from_slice(&ball.id.to_le_bytes());
    buf.push(mode_byte);
    buf.extend_from_slice(&(ball.radius as f32).to_le_bytes());
    buf.extend_from_slice(&ball.position.x.to_le_bytes());
    buf.extend_from_slice(&ball.position.y.to_le_bytes());
    buf.extend_from_slice(&ball.position.z.to_le_bytes());
    buf.push(flags);

    // MassSector (25 bytes, included if mode != RIGID, `DestinyStructs.h:106-112`):
    // double mass; uint8 cloak; int64 allianceID; int32 corporationID; int32 harmonic;
    if !is_rigid {
        let cloak = meta.map(|m| u8::from(m.is_cloaked)).unwrap_or(0);
        let alliance_id = meta
            .map(|m| if m.alliance_id > 0 { m.alliance_id } else { -1 })
            .unwrap_or(-1);
        let corp_id = meta.map(|m| m.corp_id.max(0) as i32).unwrap_or(0);
        let harmonic: i32 = park
            .and_then(|p| p.ball_harmonics.get(&ball.id).copied())
            .unwrap_or(-1);
        buf.extend_from_slice(&ball.mass.to_le_bytes());
        buf.push(cloak);
        buf.extend_from_slice(&alliance_id.to_le_bytes());
        buf.extend_from_slice(&corp_id.to_le_bytes());
        buf.extend_from_slice(&harmonic.to_le_bytes());
    }

    // DataSector (36 bytes, included if IsFree):
    // float maxSpeed; double velX; double velY; double velZ; float inertia; float speedfraction;
    if !is_rigid && (flags & BALL_FLAG_IS_FREE) != 0 {
        buf.extend_from_slice(&(ball.max_velocity as f32).to_le_bytes());
        buf.extend_from_slice(&ball.velocity.x.to_le_bytes());
        buf.extend_from_slice(&ball.velocity.y.to_le_bytes());
        buf.extend_from_slice(&ball.velocity.z.to_le_bytes());
        buf.extend_from_slice(&(ball.inertia as f32).to_le_bytes());
        buf.extend_from_slice(&(ball.speed_fraction as f32).to_le_bytes());
    }

    let owner_id = meta.map(|m| m.owner_id).unwrap_or(1);
    let effect_stamp = stamp as i32;

    // Mode-specific sector (`DestinyStructs.h:136-210`)
    match &ball.mode {
        MovementMode::GotoDirection { direction } => {
            // GOTO_Struct (25 bytes): uint8 formationID, double x, double y, double z
            buf.push(0xFFu8);
            buf.extend_from_slice(&direction.x.to_le_bytes());
            buf.extend_from_slice(&direction.y.to_le_bytes());
            buf.extend_from_slice(&direction.z.to_le_bytes());
        }
        MovementMode::Follow { target_id, range } => {
            // FOLLOW_Struct (13 bytes, `DestinyStructs.h:143-147`): uint8 formationID, int64 followID, float followRange
            buf.push(0xFFu8);
            buf.extend_from_slice(&target_id.to_le_bytes());
            buf.extend_from_slice(&(*range as f32).to_le_bytes());
        }
        MovementMode::Orbit { target_id, range } => {
            // ORBIT_Struct (13 bytes, V24.01 `_destiny.so` `0x74da8`): uint8 formationID, int64 targetID, float followRange
            buf.push(0xFFu8);
            buf.extend_from_slice(&target_id.to_le_bytes());
            buf.extend_from_slice(&(*range as f32).to_le_bytes());
        }
        MovementMode::Stop | MovementMode::Field | MovementMode::Rigid | MovementMode::Boid | MovementMode::Miniball => {
            // STOP_Struct / FIELD_Struct / RIGID_Struct (1 byte): uint8 formationID
            buf.push(0xFFu8);
        }
        MovementMode::Warp {
            origin,
            destination,
            warp_speed_au_s,
            total_distance,
            phase,
            elapsed_warp_time,
        } => {
            // WARP_Struct (53 bytes, V24.01 `_destiny.so` `0x74eeb..0x74f53`):
            // uint8 formationID, double targX, targY, targZ, int32 effectStamp, double totalDistance, double stopDistance, int64 warpSpeed (milli-AU/s)
            buf.push(0xFFu8);
            buf.extend_from_slice(&destination.x.to_le_bytes());
            buf.extend_from_slice(&destination.y.to_le_bytes());
            buf.extend_from_slice(&destination.z.to_le_bytes());
            let (warp_effect_stamp, wire_total_distance) = if *phase == crate::WarpPhase::Aligning {
                (-1_i32, -1.0_f64)
            } else {
                let start_stamp = (stamp as i32 - elapsed_warp_time.round() as i32).max(0);
                let dist = if *total_distance > 0.0 {
                    *total_distance
                } else {
                    (*destination - *origin).length()
                };
                (start_stamp, dist)
            };
            buf.extend_from_slice(&warp_effect_stamp.to_le_bytes());
            buf.extend_from_slice(&wire_total_distance.to_le_bytes());
            buf.extend_from_slice(&0.0_f64.to_le_bytes());
            buf.extend_from_slice(&((*warp_speed_au_s * 1000.0).round() as i64).to_le_bytes());
        }
        MovementMode::Missile { target_id } => {
            // MISSILE_Struct (49 bytes, `DestinyStructs.h:170-179`):
            // uint8 formationID, int64 targetID, float followRange, int64 ownerID, int32 effectStamp, double x, double y, double z
            buf.push(0xFFu8);
            buf.extend_from_slice(&target_id.to_le_bytes());
            buf.extend_from_slice(&0.0f32.to_le_bytes());
            buf.extend_from_slice(&owner_id.to_le_bytes());
            buf.extend_from_slice(&effect_stamp.to_le_bytes());
            buf.extend_from_slice(&ball.velocity.x.to_le_bytes());
            buf.extend_from_slice(&ball.velocity.y.to_le_bytes());
            buf.extend_from_slice(&ball.velocity.z.to_le_bytes());
        }
        MovementMode::Mushroom => {
            // MUSHROOM_Struct (25 bytes, `DestinyStructs.h:181-187`):
            // uint8 formationID, float followRange, double unknown125, int32 effectStamp, int64 ownerID
            let (max_r, dur_s, est, oid) = park
                .and_then(|p| p.mushroom_states.get(&ball.id))
                .map(|ms| {
                    (
                        ms.max_radius as f32,
                        ms.duration_s,
                        ms.effect_stamp as i32,
                        ms.owner_id,
                    )
                })
                .unwrap_or((ball.radius as f32, 1.0f64, effect_stamp, owner_id));
            buf.push(0xFFu8);
            buf.extend_from_slice(&max_r.to_le_bytes());
            buf.extend_from_slice(&dur_s.to_le_bytes());
            buf.extend_from_slice(&est.to_le_bytes());
            buf.extend_from_slice(&oid.to_le_bytes());
        }
        MovementMode::Troll => {
            // TROLL_Struct (5 bytes, `DestinyStructs.h:189-192`):
            // uint8 formationID, uint32 effectStamp
            let troll_stamp = park
                .and_then(|p| p.troll_states.get(&ball.id))
                .map(|ts| ts.effect_stamp as u32)
                .unwrap_or(stamp);
            buf.push(0xFFu8);
            buf.extend_from_slice(&troll_stamp.to_le_bytes());
        }
        MovementMode::Formation => {
            // FORMATION_Struct (17 bytes, `DestinyStructs.h:204-209`):
            // uint8 formationID, int64 followID, float followRange, int32 effectStamp
            let (leader_id, follow_rng, est) = park
                .and_then(|p| p.formation_states.get(&ball.id))
                .map(|fs| (fs.leader_id, fs.follow_range as f32, fs.effect_stamp as i32))
                .unwrap_or((0i64, 0.0f32, effect_stamp));
            buf.push(0xFFu8);
            buf.extend_from_slice(&leader_id.to_le_bytes());
            buf.extend_from_slice(&follow_rng.to_le_bytes());
            buf.extend_from_slice(&est.to_le_bytes());
        }
    }
}

/// Resolves [`SlimItemMeta`] for `ball` from `park` when registered, or infers sensible defaults.
pub fn resolve_slim_item_meta(park: Option<&Ballpark>, ball: &Ball) -> SlimItemMeta {
    if let Some(p) = park {
        if let Some(m) = p.ball_slim_meta(ball.id) {
            return m.clone();
        }
        if let Some(gid) = p.ball_group_id(ball.id) {
            let cid = match gid {
                6..=10 => 2,
                15 => 3,
                _ => 6,
            };
            return SlimItemMeta {
                group_id: gid,
                category_id: cid,
                ..SlimItemMeta::default()
            };
        }
    }
    if matches!(ball.mode, MovementMode::Rigid) {
        if ball.id >= 60_000_000 && ball.id < 70_000_000 {
            return SlimItemMeta {
                group_id: 15,
                category_id: 3,
                ..SlimItemMeta::default()
            };
        }
        if ball.id >= 50_000_000 && ball.id < 60_000_000 {
            return SlimItemMeta {
                group_id: 10,
                category_id: 2,
                ..SlimItemMeta::default()
            };
        }
        if ball.type_id == 15 {
            return SlimItemMeta {
                group_id: 9,
                category_id: 2,
                ..SlimItemMeta::default()
            };
        }
        if matches!(ball.type_id, 32882 | 55914 | 56002) {
            return SlimItemMeta {
                group_id: 4079,
                category_id: 2,
                ..SlimItemMeta::default()
            };
        }
        return SlimItemMeta {
            group_id: 6,
            category_id: 2,
            ..SlimItemMeta::default()
        };
    }
    if matches!(ball.mode, MovementMode::Field) {
        return SlimItemMeta {
            group_id: 361,
            category_id: 22,
            ..SlimItemMeta::default()
        };
    }
    SlimItemMeta::default()
}

/// Builds the plain slim-item attribute dict for a `Ball` (string keys -> values).
///
/// The V24 client assigns this dict straight to `SlimItem().__dict__` for `AddBalls2`
/// (`michelle.pyj:1697-1698`), and the `foo.SlimItem` class (`eve/common/script/util/slimItem.pyj`)
/// has no `__setstate__`, so the unpickled object state must also be a plain dict
/// (evemu_Crucible `SystemBubble.cpp:726` wraps `MakeSlimItem()`'s `PyDict` the same way).
pub fn slim_item_dict(ball: &Ball, meta: &SlimItemMeta) -> PyRep {
    let char_rep = meta.char_id.map(PyRep::Integer).unwrap_or(PyRep::None);
    let corp_rep = if meta.corp_id > 0 {
        PyRep::Integer(meta.corp_id)
    } else {
        PyRep::None
    };
    let alliance_rep = if meta.alliance_id > 0 {
        PyRep::Integer(meta.alliance_id)
    } else {
        PyRep::None
    };
    let war_faction_rep = meta
        .war_faction_id
        .filter(|&w| w > 0)
        .map(PyRep::Integer)
        .unwrap_or(PyRep::None);
    let wire_type_id = match ball.type_id {
        0 => 6,
        32882 | 56002 => 55914,
        other => other,
    };
    // V24.01 `StateSvc.CheckStates` (`state.pyj:874`) and `IsStandingRelevant` (`state.pyj:927`)
    // treat `invConst.ownerSystem == 1` (and `None` / NPC IDs) as non-player owners, whereas `0`
    // falls into `_GetRelationship` as a player character ID (`#722`).
    let wire_owner_id = if meta.owner_id == 0 { 1 } else { meta.owner_id };
    let mut entries = vec![
        ("itemID", PyRep::Integer(ball.id)),
        ("typeID", PyRep::Integer(wire_type_id as i64)),
        ("groupID", PyRep::Integer(meta.group_id as i64)),
        ("categoryID", PyRep::Integer(meta.category_id as i64)),
        ("ownerID", PyRep::Integer(wire_owner_id)),
        ("charID", char_rep),
        ("corpID", corp_rep),
        ("allianceID", alliance_rep),
        ("warFactionID", war_faction_rep),
        ("securityStatus", PyRep::Real(meta.security_status)),
        ("radius", PyRep::Real(ball.radius)),
        ("skinID", meta.skin_id.map(|s| PyRep::Integer(i64::from(s))).unwrap_or(PyRep::None)),
        ("skinMaterialSetID", meta.skin_material_set_id.map(|m| PyRep::Integer(i64::from(m))).unwrap_or(PyRep::None)),
    ];
    let slim_name = meta
        .name
        .as_deref()
        .filter(|n| !n.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            if meta.category_id == 6 {
                format!("Ship {}", ball.id)
            } else {
                format!("Item {}", ball.id)
            }
        });
    entries.push(("name", PyRep::String(slim_name)));
    if meta.group_id == 10 {
        // DERIVED: V24.01 `StargateSetup` indexes corruption/suppression 2-tuples directly,
        // and `SetupStargateBanners` calls `GetFactionIconTexturePath(ownerID)` which crashes
        // if given `1` (`ownerSystem`). Only emit real faction/sovereignty IDs (> 1); emit `None`
        // for unclaimed/nullsec/synthetic gates (`#767`).
        let zero_of_five = PyRep::Tuple(vec![PyRep::Integer(0), PyRep::Integer(5)]);
        let origin_owner = meta
            .origin_system_owner_id
            .filter(|&id| id > 1)
            .or_else(|| (meta.owner_id > 1).then_some(meta.owner_id));
        let dest_owner = meta.dest_system_owner_id.filter(|&id| id > 1);
        entries.push(("destinationSystemStatusIcons", PyRep::List(vec![])));
        entries.push((
            "originSystemOwnerID",
            origin_owner.map_or(PyRep::None, PyRep::Integer),
        ));
        entries.push((
            "destinationSystemOwnerID",
            dest_owner.map_or(PyRep::None, PyRep::Integer),
        ));
        entries.push(("destinationCorruptionStageAndMaximum", zero_of_five.clone()));
        entries.push(("localCorruptionStageAndMaximum", zero_of_five.clone()));
        entries.push(("destinationSuppressionStageAndMaximum", zero_of_five.clone()));
        entries.push(("localSuppressionStageAndMaximum", zero_of_five));
        let jump_list = meta
            .jump_target
            .map(|(dest_gate_id, dest_sys_id)| {
                vec![PyRep::keyval([
                    ("toCelestialID", PyRep::Integer(dest_gate_id)),
                    ("locationID", PyRep::Integer(i64::from(dest_sys_id))),
                ])]
            })
            .unwrap_or_default();
        entries.push(("jumps", PyRep::List(jump_list)));
    }
    if meta.group_id == 988 {
        entries.push(("nebulaType", PyRep::Integer(11806)));
        entries.push(("maxShipJumpMass", PyRep::Real(300_000_000.0)));
        entries.push(("isDestTriglavian", PyRep::Integer(0)));
    }
    if meta.category_id == 11 {
        // DERIVED: V24.01 `entitystandings.pyj:63` requires non-None standing thresholds on NPC balls.
        entries.push(("hostile_response_threshold", PyRep::Real(11.0)));
        entries.push(("friendly_response_threshold", PyRep::Real(11.0)));
    }
    PyRep::Dict(
        entries
            .into_iter()
            .map(|(k, v)| (PyRep::String(k.into()), v))
            .collect(),
    )
}

/// Builds a `foo.SlimItem` dict for a `Ball`, including `Ballpark`-level wormhole stage attributes
/// (`wormholeAge`, `wormholeSize`) when present.
pub fn slim_item_dict_with_park(park: Option<&Ballpark>, ball: &Ball, meta: &SlimItemMeta) -> PyRep {
    let mut rep = slim_item_dict(ball, meta);
    let stages = park
        .and_then(|p| p.ball_wormhole_stages.get(&ball.id).copied())
        .or_else(|| (meta.group_id == 988).then_some((0, 1.0)));
    if let (Some((age, size)), PyRep::Dict(entries)) = (stages, &mut rep) {
        entries.push((PyRep::String("wormholeAge".into()), PyRep::Integer(age)));
        entries.push((PyRep::String("wormholeSize".into()), PyRep::Real(size)));
    }
    rep
}

/// Builds a `foo.SlimItem` `PyRep::Object` (state = plain dict) for a `Ball` using [`SlimItemMeta`] and [`Ballpark`].
pub fn encode_slim_item_with_park(park: Option<&Ballpark>, ball: &Ball, meta: &SlimItemMeta) -> PyRep {
    PyRep::Object {
        type_name: "foo.SlimItem".into(),
        args: Box::new(slim_item_dict_with_park(park, ball, meta)),
    }
}

/// Builds a `foo.SlimItem` `PyRep::Object` (state = plain dict) for a `Ball` using [`SlimItemMeta`].
pub fn encode_slim_item_with_meta(ball: &Ball, meta: &SlimItemMeta) -> PyRep {
    encode_slim_item_with_park(None, ball, meta)
}

/// Builds a `foo.SlimItem` `PyRep::Object` for a `Ball`.
pub fn encode_slim_item(ball: &Ball, group_id: u32, category_id: u32) -> PyRep {
    let meta = SlimItemMeta {
        group_id,
        category_id,
        ..SlimItemMeta::default()
    };
    encode_slim_item_with_meta(ball, &meta)
}

/// Builds the 3-element EVE damage state tuple `((shield_ratio, tau, stamp), armor_ratio, structure_ratio)`.
pub fn encode_damage_state_tuple(cs: Option<&CombatState>, stamp: u32) -> PyRep {
    let (s, a, st) = if let Some(c) = cs {
        (
            (c.shield_hp / c.max_shield_hp.max(1.0)).clamp(0.0, 1.0),
            (c.armor_hp / c.max_armor_hp.max(1.0)).clamp(0.0, 1.0),
            (c.structure_hp / c.max_structure_hp.max(1.0)).clamp(0.0, 1.0),
        )
    } else {
        (1.0, 1.0, 1.0)
    };
    let tau_ms = cs.map(|c| c.shield_recharge_time_ms).unwrap_or(120_000.0) / 5.0;
    damage_state_wire(s, tau_ms, a, st, stamp)
}

/// `DoDestinyDamageState` wire layout `((shield_ratio, shield_recharge_ms, stamp), armor_ratio, structure_ratio)`.
fn damage_state_wire(shield: f64, tau_ms: f64, armor: f64, structure: f64, stamp: u32) -> PyRep {
    PyRep::Tuple(vec![
        PyRep::Tuple(vec![
            PyRep::Real(shield),
            PyRep::Real(tau_ms),
            PyRep::Integer(stamp as i64),
        ]),
        PyRep::Real(armor),
        PyRep::Real(structure),
    ])
}

/// Builds the `SetState` `util.KeyVal` payload for `observer_ball_id` in `park`,
/// including the binary `PyRep::Bytes` `state` buffer (`encode_add_balls_with_park`), `slims`, `damageState`, `ego`, and `stamp`.
pub fn encode_set_state_payload(park: &Ballpark, observer_ball_id: BallId) -> PyRep {
    let stamp = park.current_tick as u32;
    let visible = park.visible_balls_for_observer(observer_ball_id);
    let state_bytes = encode_add_balls_with_park(0, stamp, &visible, Some(park));

    let mut slims = Vec::with_capacity(visible.len());
    let mut damage_dict = Vec::with_capacity(visible.len());
    for ball in &visible {
        let meta = resolve_slim_item_meta(Some(park), ball);
        slims.push(encode_slim_item_with_park(Some(park), ball, &meta));
        if !park.is_static_ball(ball.id) || park.combat_states.contains_key(&ball.id) {
            let dmg = encode_damage_state_tuple(park.combat_states.get(&ball.id), stamp);
            damage_dict.push((PyRep::Integer(ball.id), dmg));
        }
    }

    // `droneState` is iterated row by row (`michelle.pyj:1228-1231` reads `d.droneID`, `d.ownerID`,
    // ...); `util.Rowset` has no `__setstate__`, so its state is the plain `{header, RowClass, lines}`
    // dict that `PyRep::Rowset` marshals (column list from evemu_Crucible `Destiny.xmlp:168`).
    let drone_state = PyRep::rowset(
        vec![
            "droneID",
            "ownerID",
            "controllerID",
            "activityState",
            "typeID",
            "controllerOwnerID",
            "targetID",
        ],
        Vec::new(),
    );

    // `solItem` is a `util.Row`; `Row.__setstate__` unpacks `(header, line)`
    // (`carbon/common/script/sys/row.pyj:98-99`). Columns follow evemu_Crucible `Destiny.xmlp:176`.
    let sol_item = PyRep::Object {
        type_name: "util.Row".into(),
        args: Box::new(PyRep::Tuple(vec![
            PyRep::List(
                [
                    "itemID",
                    "typeID",
                    "ownerID",
                    "locationID",
                    "flagID",
                    "contraband",
                    "singleton",
                    "quantity",
                    "groupID",
                    "categoryID",
                    "customInfo",
                ]
                .into_iter()
                .map(|c| PyRep::String(c.into()))
                .collect(),
            ),
            PyRep::List(vec![
                PyRep::Integer(park.solar_system_id as i64),
                PyRep::Integer(5),
                PyRep::Integer(1),
                PyRep::Integer(park.constellation_id as i64),
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(1),
                PyRep::Integer(-1),
                PyRep::Integer(5),
                PyRep::Integer(2),
                PyRep::String(String::new()),
            ]),
        ])),
    };

    PyRep::keyval([
        ("stamp", PyRep::Integer(stamp as i64)),
        ("state", PyRep::Bytes(state_bytes)),
        ("ego", PyRep::Integer(observer_ball_id)),
        ("damageState", PyRep::Dict(damage_dict)),
        ("aggressors", PyRep::Dict(vec![])),
        ("slims", PyRep::List(slims)),
        ("effectStates", PyRep::List(vec![])),
        ("allianceBridges", PyRep::List(vec![])),
        ("droneState", drone_state),
        ("solItem", sol_item),
        ("industryLevel", PyRep::Integer(0)),
        ("researchLevel", PyRep::Integer(0)),
        ("dbuffState", PyRep::List(vec![])),
    ])
}

/// Wraps [`encode_set_state_payload`] into a `(stamp, ("SetState", (keyval,)))` `DoDestinyUpdate` entry.
pub fn encode_set_state_update(park: &Ballpark, observer_ball_id: BallId) -> PyRep {
    let stamp = park.current_tick as i64;
    let payload = encode_set_state_payload(park, observer_ball_id);
    PyRep::Tuple(vec![
        PyRep::Integer(stamp),
        PyRep::Tuple(vec![
            PyRep::String("SetState".into()),
            PyRep::Tuple(vec![payload]),
        ]),
    ])
}

/// `AddBalls2` `extraBallData`: one `(slimItemDict, damageState)` tuple per ball. The V24 client
/// unpacks the tuple and assigns the dict to `SlimItem().__dict__` (`michelle.pyj:1685-1700`), as
/// evemu_Crucible `SystemBubble.cpp:775-777` builds it.
fn encode_ball_extra_list(balls: &[&Ball], park: Option<&Ballpark>, stamp: u32) -> Vec<PyRep> {
    balls
        .iter()
        .map(|ball| {
            let meta = resolve_slim_item_meta(park, ball);
            let slim = slim_item_dict_with_park(park, ball, &meta);
            let dmg = encode_damage_state_tuple(
                park.and_then(|p| p.combat_states.get(&ball.id)),
                stamp,
            );
            PyRep::Tuple(vec![slim, dmg])
        })
        .collect()
}

/// Encodes an `AddBall` `DoDestinyUpdate` action tuple `(stamp, ("AddBall", (extra_data, is_global)))`.
pub fn encode_add_ball_action(stamp: u32, ball: &Ball, park: Option<&Ballpark>) -> PyRep {
    let meta = resolve_slim_item_meta(park, ball);
    let slim = encode_slim_item_with_park(park, ball, &meta);
    let dmg = encode_damage_state_tuple(
        park.and_then(|p| p.combat_states.get(&ball.id)),
        stamp,
    );
    PyRep::Tuple(vec![
        PyRep::Integer(stamp as i64),
        PyRep::Tuple(vec![
            PyRep::String("AddBall".into()),
            PyRep::Tuple(vec![PyRep::Tuple(vec![slim, dmg]), PyRep::Bool(false)]),
        ]),
    ])
}

/// Encodes an `AddBalls` `DoDestinyUpdate` action tuple
/// `(stamp, ("AddBalls", ((state_bytes, [foo.SlimItem, ...], {ballID: damageState}),)))`.
///
/// The V24 client unpacks `state, slims, damageDict = chunk` (`michelle.pyj:1645-1671`); slims are
/// `foo.SlimItem` objects (it reads `slimItem.itemID`), and the binary header uses packet type `1`
/// ("balls"), as evemu_Crucible `SystemBubble.cpp:712-735` builds it.
pub fn encode_add_balls_action(stamp: u32, balls: &[&Ball], park: Option<&Ballpark>) -> PyRep {
    let state_bytes = encode_add_balls_with_park(1, stamp, balls, park);
    let mut slims = Vec::with_capacity(balls.len());
    let mut damage = Vec::with_capacity(balls.len());
    for ball in balls {
        let meta = resolve_slim_item_meta(park, ball);
        slims.push(encode_slim_item_with_park(park, ball, &meta));
        damage.push((
            PyRep::Integer(ball.id),
            encode_damage_state_tuple(park.and_then(|p| p.combat_states.get(&ball.id)), stamp),
        ));
    }
    PyRep::Tuple(vec![
        PyRep::Integer(stamp as i64),
        PyRep::Tuple(vec![
            PyRep::String("AddBalls".into()),
            PyRep::Tuple(vec![PyRep::Tuple(vec![
                PyRep::Bytes(state_bytes),
                PyRep::List(slims),
                PyRep::Dict(damage),
            ])]),
        ]),
    ])
}

/// Encodes an `AddBalls2` `DoDestinyUpdate` action tuple `(stamp, ("AddBalls2", ((state_bytes, extra_ball_data_list),)))`.
pub fn encode_add_balls2_action(stamp: u32, balls: &[&Ball], park: Option<&Ballpark>) -> PyRep {
    let state_bytes = encode_add_balls_with_park(1, stamp, balls, park);
    let extra_data = encode_ball_extra_list(balls, park, stamp);
    PyRep::Tuple(vec![
        PyRep::Integer(stamp as i64),
        PyRep::Tuple(vec![
            PyRep::String("AddBalls2".into()),
            PyRep::Tuple(vec![PyRep::Tuple(vec![
                PyRep::Bytes(state_bytes),
                PyRep::List(extra_data),
            ])]),
        ]),
    ])
}

/// Encodes a single [`DestinyEvent`] into a `(stamp, (action_name, args_tuple))` `DoDestinyUpdate` tuple.
pub fn encode_destiny_event(stamp: u32, event: &DestinyEvent, park: Option<&Ballpark>) -> PyRep {
    let action_tuple = match event {
        DestinyEvent::SetState { observer_ball_id } => {
            if let Some(p) = park {
                return encode_set_state_update(p, *observer_ball_id);
            }
            PyRep::Tuple(vec![
                PyRep::String("SetState".into()),
                PyRep::Tuple(vec![PyRep::keyval([(
                    "ego",
                    PyRep::Integer(*observer_ball_id),
                )])]),
            ])
        }
        DestinyEvent::AddBall { ball } => {
            return encode_add_ball_action(stamp, ball, park);
        }
        DestinyEvent::AddBalls { balls } => {
            let refs: Vec<&Ball> = balls.iter().collect();
            return encode_add_balls_action(stamp, &refs, park);
        }
        DestinyEvent::AddBalls2 { balls } => {
            let refs: Vec<&Ball> = balls.iter().collect();
            return encode_add_balls2_action(stamp, &refs, park);
        }
        DestinyEvent::RemoveBall { ball_id } => PyRep::Tuple(vec![
            PyRep::String("RemoveBall".into()),
            PyRep::Tuple(vec![PyRep::Integer(*ball_id)]),
        ]),
        DestinyEvent::RemoveBalls { ball_ids } => PyRep::Tuple(vec![
            PyRep::String("RemoveBalls".into()),
            PyRep::Tuple(vec![PyRep::List(
                ball_ids.iter().map(|&id| PyRep::Integer(id)).collect(),
            )]),
        ]),
        DestinyEvent::GotoDirection { ball_id, direction } => PyRep::Tuple(vec![
            PyRep::String("GotoDirection".into()),
            PyRep::Tuple(vec![
                PyRep::Integer(*ball_id),
                PyRep::Real(direction[0]),
                PyRep::Real(direction[1]),
                PyRep::Real(direction[2]),
            ]),
        ]),
        DestinyEvent::Follow {
            ball_id,
            target_id,
            range,
        } => PyRep::Tuple(vec![
            PyRep::String("FollowBall".into()),
            PyRep::Tuple(vec![
                PyRep::Integer(*ball_id),
                PyRep::Integer(*target_id),
                PyRep::Real(*range),
            ]),
        ]),
        DestinyEvent::SetSpeedFraction { ball_id, fraction } => PyRep::Tuple(vec![
            PyRep::String("SetSpeedFraction".into()),
            PyRep::Tuple(vec![PyRep::Integer(*ball_id), PyRep::Real(*fraction)]),
        ]),
        DestinyEvent::Orbit {
            ball_id,
            target_id,
            range,
        } => PyRep::Tuple(vec![
            PyRep::String("Orbit".into()),
            PyRep::Tuple(vec![
                PyRep::Integer(*ball_id),
                PyRep::Integer(*target_id),
                PyRep::Real(*range),
            ]),
        ]),
        DestinyEvent::Stop { ball_id } => PyRep::Tuple(vec![
            PyRep::String("Stop".into()),
            PyRep::Tuple(vec![PyRep::Integer(*ball_id)]),
        ]),
        DestinyEvent::WarpTo {
            ball_id,
            destination,
            warp_speed_au_s,
        } => PyRep::Tuple(vec![
            PyRep::String("WarpTo".into()),
            // V24 `_destiny.Ballpark.WarpTo(ballID, x, y, z, distance, warpSpeedMilliAuPerSec)` 6-tuple
            // (`autopilot.pyj:221` slices `dstVector = args[1:-2]` to extract `(x, y, z)`;
            // V24 `_destiny.so` divides the 6th integer arg by `1000.0` to compute warp acceleration).
            PyRep::Tuple(vec![
                PyRep::Integer(*ball_id),
                PyRep::Real(destination[0]),
                PyRep::Real(destination[1]),
                PyRep::Real(destination[2]),
                PyRep::Integer(0),
                PyRep::Integer((*warp_speed_au_s * 1000.0).round() as i64),
            ]),
        ]),
        DestinyEvent::SetBallMassive {
            ball_id,
            is_massive,
        } => PyRep::Tuple(vec![
            PyRep::String("SetBallMassive".into()),
            PyRep::Tuple(vec![
                PyRep::Integer(*ball_id),
                PyRep::Integer(i64::from(*is_massive)),
            ]),
        ]),
        DestinyEvent::OnDamageStateChange {
            ball_id,
            shield_ratio,
            armor_ratio,
            structure_ratio,
        } => PyRep::Tuple(vec![
            PyRep::String("OnDamageStateChange".into()),
            PyRep::Tuple(vec![
                PyRep::Integer(*ball_id),
                damage_state_wire(
                    *shield_ratio,
                    park.and_then(|p| p.combat_states.get(ball_id))
                        .map(|c| c.shield_recharge_time_ms)
                        .unwrap_or(120_000.0)
                        / 5.0,
                    *armor_ratio,
                    *structure_ratio,
                    stamp,
                ),
            ]),
        ]),
        DestinyEvent::TerminalExplosion {
            ball_id,
            destruction_effect_id,
        } => PyRep::Tuple(vec![
            // `Park.TerminalPlayDestructionEffect(shipID, destructionEffectId)` (`michelle.pyj:1596`).
            PyRep::String("TerminalPlayDestructionEffect".into()),
            PyRep::Tuple(vec![
                PyRep::Integer(*ball_id),
                destruction_effect_id.map(PyRep::Integer).unwrap_or(PyRep::None),
            ]),
        ]),
        DestinyEvent::Collision { ball_a, ball_b } => PyRep::Tuple(vec![
            PyRep::String("Collision".into()),
            PyRep::Tuple(vec![PyRep::Integer(*ball_a), PyRep::Integer(*ball_b)]),
        ]),
        DestinyEvent::OnTarget {
            mode,
            source_id,
            target_id,
        } => PyRep::Tuple(vec![
            PyRep::String("OnTarget".into()),
            PyRep::Tuple(vec![
                PyRep::String(mode.clone()),
                PyRep::Integer(*source_id),
                PyRep::Integer(*target_id),
            ]),
        ]),
        DestinyEvent::OnActivatingWarp { ball_id } => PyRep::Tuple(vec![
            PyRep::String("OnActivatingWarp".into()),
            PyRep::Tuple(vec![PyRep::Integer(*ball_id), PyRep::Integer(i64::from(stamp))]),
        ]),
        DestinyEvent::OnDeactivatingWarp { ball_id } => PyRep::Tuple(vec![
            PyRep::String("OnDeactivatingWarp".into()),
            PyRep::Tuple(vec![PyRep::Integer(*ball_id), PyRep::Integer(i64::from(stamp))]),
        ]),
        DestinyEvent::OnSpecialFX {
            ball_id,
            module_id,
            guid,
            is_active,
        } => PyRep::Tuple(vec![
            PyRep::String("OnSpecialFX".into()),
            // V24 14-arg layout (`michelle.py:2087`, `spaceMgr.py:605`):
            // (shipID, moduleID, moduleTypeID, targetID, otherTypeID, guid, isOffensive,
            //  start, active, duration, repeat, startTime, timeFromStart, graphicInfo).
            PyRep::Tuple(vec![
                PyRep::Integer(*ball_id),
                module_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                PyRep::None,
                PyRep::None,
                PyRep::None,
                PyRep::String(guid.clone()),
                PyRep::Bool(false),
                PyRep::Integer(i64::from(*is_active)),
                PyRep::Integer(i64::from(*is_active)),
                PyRep::Integer(-1),
                PyRep::None,
                PyRep::None,
                PyRep::Integer(0),
                PyRep::None,
            ]),
        ]),
        DestinyEvent::CloakBall { ball_id, cloaked } => PyRep::Tuple(vec![
            // evemu_Crucible `Destiny.xmlp:670-681` (`SetBallCloak`): `(entityID, bool cloaked)`.
            PyRep::String("CloakBall".into()),
            PyRep::Tuple(vec![PyRep::Integer(*ball_id), PyRep::Bool(*cloaked != 0)]),
        ]),
        DestinyEvent::GotoPoint { ball_id, point } => PyRep::Tuple(vec![
            PyRep::String("GotoPoint".into()),
            PyRep::Tuple(vec![
                PyRep::Integer(*ball_id),
                PyRep::Real(point[0]),
                PyRep::Real(point[1]),
                PyRep::Real(point[2]),
            ]),
        ]),
        DestinyEvent::OnClientControllerEvent {
            ball_id,
            target_id,
            event_name,
            value,
            delay,
        } => PyRep::Tuple(vec![
            PyRep::String("OnClientControllerEvent".into()),
            PyRep::Tuple(vec![
                PyRep::Integer(*ball_id),
                target_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                PyRep::String(event_name.clone()),
                PyRep::Integer(*value),
                delay.map(PyRep::Real).unwrap_or(PyRep::None),
            ]),
        ]),
        DestinyEvent::OnDbuffUpdated { ball_id, dbuffs } => PyRep::Tuple(vec![
            PyRep::String("OnDbuffUpdated".into()),
            PyRep::Tuple(vec![
                PyRep::Integer(*ball_id),
                PyRep::List(
                    dbuffs
                        .iter()
                        .map(|&(id, val, exp)| {
                            PyRep::Tuple(vec![
                                PyRep::Integer(id),
                                PyRep::Tuple(vec![PyRep::Real(val), PyRep::Integer(exp)]),
                            ])
                        })
                        .collect(),
                ),
            ]),
        ]),
        DestinyEvent::OnDotVictimUpdated {
            ball_id,
            flat_dps,
            pct_dps,
            remaining_ticks,
            expiry_filetime,
        } => {
            let apps = if *remaining_ticks > 0 {
                let effective_expiry = if *expiry_filetime > 0 {
                    *expiry_filetime
                } else {
                    let base_unix = park
                        .map(|p| p.effective_unix_secs())
                        .unwrap_or_else(crate::current_system_unix_secs);
                    crate::unix_to_filetime(base_unix.saturating_add(*remaining_ticks as u64))
                };
                vec![PyRep::keyval([
                    ("attackerID", PyRep::Integer(0)),
                    ("targetID", PyRep::Integer(*ball_id)),
                    ("weaponTypeID", PyRep::Integer(84_115)),
                    ("expiryTime", PyRep::Integer(effective_expiry)),
                    ("maxDamage", PyRep::Real(*flat_dps)),
                    ("maxHPNormalizedRatio", PyRep::Real(*pct_dps)),
                    ("activityState", PyRep::Integer(1)),
                ])]
            } else {
                Vec::new()
            };
            PyRep::Tuple(vec![
                PyRep::String("OnDotVictimUpdated".into()),
                PyRep::Tuple(vec![PyRep::Integer(*ball_id), PyRep::PySet(apps)]),
            ])
        }
        DestinyEvent::OnDroneStateChange {
            drone_id,
            owner_id,
            controller_id,
            activity_state,
            type_id,
            controller_owner_id,
            target_id,
        } => PyRep::Tuple(vec![
            PyRep::String("OnDroneStateChange".into()),
            PyRep::Tuple(vec![
                PyRep::Integer(*drone_id),
                PyRep::Integer(*owner_id),
                PyRep::Integer(*controller_id),
                PyRep::Integer(*activity_state),
                PyRep::Integer(*type_id as i64),
                PyRep::Integer(*controller_owner_id),
                target_id.map(PyRep::Integer).unwrap_or(PyRep::None),
            ]),
        ]),
        DestinyEvent::OnFleetDamageStateChange {
            ball_id,
            shield_ratio,
            armor_ratio,
            structure_ratio,
        } => PyRep::Tuple(vec![
            PyRep::String("OnFleetDamageStateChange".into()),
            PyRep::Tuple(vec![
                PyRep::Integer(*ball_id),
                damage_state_wire(
                    *shield_ratio,
                    park.and_then(|p| p.combat_states.get(ball_id))
                        .map(|c| c.shield_recharge_time_ms)
                        .unwrap_or(120_000.0)
                        / 5.0,
                    *armor_ratio,
                    *structure_ratio,
                    stamp,
                ),
            ]),
        ]),
        DestinyEvent::OnShipStateUpdate { module_states } => PyRep::Tuple(vec![
            PyRep::String("OnShipStateUpdate".into()),
            PyRep::Tuple(vec![PyRep::Dict(
                module_states
                    .iter()
                    .map(|&(owner_id, item_id, attr_id, time_ft, new_val, old_val)| {
                        (
                            PyRep::Integer(item_id),
                            PyRep::Tuple(vec![
                                PyRep::Integer(owner_id),
                                PyRep::Integer(item_id),
                                PyRep::Integer(attr_id as i64),
                                PyRep::Integer(time_ft),
                                PyRep::Real(new_val),
                                PyRep::Real(old_val),
                            ]),
                        )
                    })
                    .collect(),
            )]),
        ]),
        DestinyEvent::OnSlimItemChange { ball_id } => {
            let slim = park
                .and_then(|p| p.balls.get(ball_id).map(|b| (p, b, resolve_slim_item_meta(Some(p), b))))
                .map(|(p, b, m)| encode_slim_item_with_park(Some(p), b, &m))
                .unwrap_or_else(|| PyRep::keyval([("itemID", PyRep::Integer(*ball_id))]));
            PyRep::Tuple(vec![
                PyRep::String("OnSlimItemChange".into()),
                PyRep::Tuple(vec![PyRep::Integer(*ball_id), slim]),
            ])
        }
        DestinyEvent::OnSovereigntyChanged { solar_system_id, alliance_id } => PyRep::Tuple(vec![
            PyRep::String("OnSovereigntyChanged".into()),
            PyRep::Tuple(vec![PyRep::Integer(*solar_system_id as i64), PyRep::Integer(*alliance_id)]),
        ]),
        DestinyEvent::PackagedAction { actions_bytes } => PyRep::Tuple(vec![
            PyRep::String("PackagedAction".into()),
            PyRep::Bytes(actions_bytes.clone()),
        ]),
        DestinyEvent::SetBallFree { ball_id, is_free } => PyRep::Tuple(vec![
            PyRep::String("SetBallFree".into()),
            PyRep::Tuple(vec![PyRep::Integer(*ball_id), PyRep::Integer(i64::from(*is_free))]),
        ]),
        DestinyEvent::SetBallHarmonic { ball_id, harmonic } => {
            let meta = park.and_then(|p| p.ball_slim_meta(*ball_id));
            let alliance_id = meta
                .map(|m| if m.alliance_id > 0 { m.alliance_id } else { -1 })
                .unwrap_or(-1);
            let corp_id = meta
                .map(|m| if m.corp_id > 0 { m.corp_id } else { -1 })
                .unwrap_or(-1);
            PyRep::Tuple(vec![
                PyRep::String("SetBallHarmonic".into()),
                PyRep::Tuple(vec![
                    PyRep::Integer(*ball_id),
                    PyRep::Integer(alliance_id),
                    PyRep::Integer(corp_id),
                    PyRep::Integer(-1),
                    PyRep::Integer(*harmonic as i64),
                ]),
            ])
        }
        DestinyEvent::SetBallInteractive { ball_id, is_interactive } => PyRep::Tuple(vec![
            PyRep::String("SetBallInteractive".into()),
            PyRep::Tuple(vec![PyRep::Integer(*ball_id), PyRep::Integer(i64::from(*is_interactive))]),
        ]),
        DestinyEvent::SetBallRadius { ball_id, radius } => PyRep::Tuple(vec![
            PyRep::String("SetBallRadius".into()),
            PyRep::Tuple(vec![PyRep::Integer(*ball_id), PyRep::Real(*radius)]),
        ]),
        DestinyEvent::SetMaxSpeed { ball_id, max_speed } => PyRep::Tuple(vec![
            PyRep::String("SetMaxSpeed".into()),
            PyRep::Tuple(vec![PyRep::Integer(*ball_id), PyRep::Real(*max_speed)]),
        ]),
        DestinyEvent::SetBallPosition { ball_id, position } => PyRep::Tuple(vec![
            PyRep::String("SetBallPosition".into()),
            PyRep::Tuple(vec![
                PyRep::Integer(*ball_id),
                PyRep::Real(position[0]),
                PyRep::Real(position[1]),
                PyRep::Real(position[2]),
            ]),
        ]),
        DestinyEvent::SetBallVelocity { ball_id, velocity } => PyRep::Tuple(vec![
            PyRep::String("SetBallVelocity".into()),
            PyRep::Tuple(vec![
                PyRep::Integer(*ball_id),
                PyRep::Real(velocity[0]),
                PyRep::Real(velocity[1]),
                PyRep::Real(velocity[2]),
            ]),
        ]),
        DestinyEvent::OnAudioActivated => PyRep::Tuple(vec![
            PyRep::String("OnAudioActivated".into()),
            PyRep::Tuple(Vec::new()),
        ]),
        DestinyEvent::OnDockingAccepted {
            start_pos,
            end_pos,
            station_id,
        } => PyRep::Tuple(vec![
            PyRep::String("OnDockingAccepted".into()),
            PyRep::Tuple(vec![
                PyRep::List(vec![
                    PyRep::Real(start_pos[0]),
                    PyRep::Real(start_pos[1]),
                    PyRep::Real(start_pos[2]),
                ]),
                PyRep::List(vec![
                    PyRep::Real(end_pos[0]),
                    PyRep::Real(end_pos[1]),
                    PyRep::Real(end_pos[2]),
                ]),
                PyRep::Integer(*station_id),
            ]),
        ]),
        DestinyEvent::OnDroneActivityChange {
            drone_id,
            activity_state,
            activity,
        } => PyRep::Tuple(vec![
            PyRep::String("OnDroneActivityChange".into()),
            PyRep::Tuple(vec![
                PyRep::Integer(*drone_id),
                PyRep::Integer(*activity_state),
                activity
                    .as_deref()
                    .map(|s| PyRep::String(s.into()))
                    .unwrap_or(PyRep::None),
            ]),
        ]),
        DestinyEvent::OnDroneControlLost { drone_id } => PyRep::Tuple(vec![
            PyRep::String("OnDroneControlLost".into()),
            PyRep::Tuple(vec![PyRep::Integer(*drone_id)]),
        ]),
        DestinyEvent::OnDroneStateChange2 {
            drone_id,
            owner_id,
            controller_id,
            activity_state,
            type_id,
            controller_owner_id,
            target_id,
        } => PyRep::Tuple(vec![
            PyRep::String("OnDroneStateChange2".into()),
            PyRep::Tuple(vec![
                PyRep::Integer(*drone_id),
                PyRep::Integer(*owner_id),
                PyRep::Integer(*controller_id),
                PyRep::Integer(*activity_state),
                PyRep::Integer(*type_id as i64),
                PyRep::Integer(*controller_owner_id),
                target_id.map(PyRep::Integer).unwrap_or(PyRep::None),
            ]),
        ]),
        // parity: destiny:OnFleetStateChange
        DestinyEvent::OnFleetStateChange {
            fleet_id,
            member_id,
            fleet_state,
        } => PyRep::Tuple(vec![
            PyRep::String("OnFleetStateChange".into()),
            PyRep::Tuple(vec![PyRep::keyval([
                ("fleetID", PyRep::Integer(*fleet_id)),
                ("characterID", PyRep::Integer(*member_id)),
                ("fleetState", PyRep::Integer(*fleet_state)),
                ("targetTags", PyRep::Dict(Vec::new())),
            ])]),
        ]),
        DestinyEvent::ScatterEwars {
            ball_id,
            effect_states,
        } => PyRep::Tuple(vec![
            PyRep::String("ScatterEwars".into()),
            PyRep::Tuple(vec![
                PyRep::Integer(*ball_id),
                PyRep::Bytes(effect_states.clone()),
            ]),
        ]),
        DestinyEvent::SetControllerVariableOnBall {
            ball_id,
            var_name,
            value,
        } => PyRep::Tuple(vec![
            PyRep::String("SetControllerVariableOnBall".into()),
            PyRep::Tuple(vec![
                PyRep::Integer(*ball_id),
                PyRep::String(var_name.clone()),
                PyRep::Integer(*value),
            ]),
        ]),
    };

    PyRep::Tuple(vec![PyRep::Integer(stamp as i64), action_tuple])
}

pub use crate::wire_batch::encode_destiny_updates;
#[cfg(test)] #[path = "wire_unit_tests.rs"] mod wire_unit_tests;
