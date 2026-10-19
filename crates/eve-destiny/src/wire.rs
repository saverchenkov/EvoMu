//! Binary `Destiny` wire-format encoder/decoder (`AddBalls` / `AddBalls2` / `SetState` state buffer)
//! and `DoDestinyUpdate` `PyRep` tuple marshaling (`P-7`).

use crate::{Ball, BallId, Ballpark, CombatState, DestinyEvent, MovementMode, SlimItemMeta};
use eve_proto::PyRep;
use glam::DVec3;

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

/// Decoded representation of a single ball unpacked from a binary `AddBalls` / `SetState` buffer.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedBall {
    pub id: BallId,
    pub mode: u8,
    pub radius: f32,
    pub position: DVec3,
    pub flags: u8,
    pub mass: Option<f64>,
    pub cloak: Option<u8>,
    pub alliance_id: Option<i64>,
    pub corp_id: Option<i32>,
    pub harmonic: Option<i32>,
    pub max_velocity: Option<f32>,
    pub velocity: Option<DVec3>,
    pub inertia: Option<f32>,
    pub speed_fraction: Option<f32>,
    pub formation_id: u8,
    pub direction: Option<DVec3>,
    pub target_id: Option<BallId>,
    pub follow_range: Option<f32>,
    pub owner_id: Option<i64>,
    pub effect_stamp: Option<i32>,
    pub warp_destination: Option<DVec3>,
    pub warp_speed_tenth_au_s: Option<u32>,
}

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
        encode_single_ball_with_meta(&mut buf, ball, stamp, meta.as_ref());
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
    let is_rigid = matches!(ball.mode, MovementMode::Rigid);
    let (mode_byte, flags) = match ball.mode {
        MovementMode::GotoDirection { .. } => (
            DST_BALL_GOTO,
            BALL_FLAG_IS_FREE | BALL_FLAG_IS_MASSIVE | BALL_FLAG_IS_INTERACTIVE,
        ),
        MovementMode::Follow { .. } => (
            DST_BALL_FOLLOW,
            BALL_FLAG_IS_FREE | BALL_FLAG_IS_MASSIVE | BALL_FLAG_IS_INTERACTIVE,
        ),
        MovementMode::Stop => (
            DST_BALL_STOP,
            BALL_FLAG_IS_FREE | BALL_FLAG_IS_MASSIVE | BALL_FLAG_IS_INTERACTIVE,
        ),
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
        let harmonic: i32 = -1;
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
            // ORBIT_Struct (13 bytes, `DestinyStructs.h:164-168`): uint8 formationID, uint32 targetID, double followRange
            buf.push(0xFFu8);
            buf.extend_from_slice(&(*target_id as u32).to_le_bytes());
            buf.extend_from_slice(&range.to_le_bytes());
        }
        MovementMode::Stop | MovementMode::Field | MovementMode::Rigid | MovementMode::Boid | MovementMode::Miniball => {
            // STOP_Struct / FIELD_Struct / RIGID_Struct (1 byte): uint8 formationID
            buf.push(0xFFu8);
        }
        MovementMode::Warp {
            destination,
            warp_speed_au_s,
            ..
        } => {
            // WARP_Struct (49 bytes, `DestinyStructs.h:153-162`):
            // uint8 formationID, double targX, targY, targZ, int32 effectStamp, int64 followRange, int64 followID, int32 speed
            buf.push(0xFFu8);
            buf.extend_from_slice(&destination.x.to_le_bytes());
            buf.extend_from_slice(&destination.y.to_le_bytes());
            buf.extend_from_slice(&destination.z.to_le_bytes());
            buf.extend_from_slice(&effect_stamp.to_le_bytes());
            buf.extend_from_slice(&((-1.0f64).to_bits() as i64).to_le_bytes());
            buf.extend_from_slice(&(15_000.0f64.to_bits() as i64).to_le_bytes());
            buf.extend_from_slice(&((*warp_speed_au_s * 10.0) as i32).to_le_bytes());
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
            buf.push(0xFFu8);
            buf.extend_from_slice(&(ball.radius as f32).to_le_bytes());
            buf.extend_from_slice(&1.0f64.to_le_bytes());
            buf.extend_from_slice(&effect_stamp.to_le_bytes());
            buf.extend_from_slice(&owner_id.to_le_bytes());
        }
        MovementMode::Troll => {
            // TROLL_Struct (5 bytes, `DestinyStructs.h:189-192`):
            // uint8 formationID, uint32 effectStamp
            buf.push(0xFFu8);
            buf.extend_from_slice(&stamp.to_le_bytes());
        }
        MovementMode::Formation => {
            // FORMATION_Struct (17 bytes, `DestinyStructs.h:204-209`):
            // uint8 formationID, int64 followID, float followRange, int32 effectStamp
            buf.push(0xFFu8);
            buf.extend_from_slice(&0i64.to_le_bytes());
            buf.extend_from_slice(&0.0f32.to_le_bytes());
            buf.extend_from_slice(&effect_stamp.to_le_bytes());
        }
    }
}

/// Decodes a binary `AddBalls` / `SetState` buffer produced by [`encode_add_balls`] or [`encode_add_balls_with_type`].
/// Returns `(packet_type, stamp, decoded_balls)`.
pub fn decode_add_balls(bytes: &[u8]) -> Result<(u8, u32, Vec<DecodedBall>), String> {
    if bytes.len() < 5 {
        return Err(format!(
            "Destiny buffer too short for 5-byte header: {} bytes",
            bytes.len()
        ));
    }
    let packet_type = bytes[0];
    let stamp = u32::from_le_bytes(bytes[1..5].try_into().unwrap());
    let mut offset = 5usize;
    let mut balls = Vec::new();

    let read_u8 = |buf: &[u8], off: &mut usize| -> Result<u8, String> {
        if *off + 1 > buf.len() {
            return Err(format!("Unexpected EOF reading u8 at offset {}", *off));
        }
        let v = buf[*off];
        *off += 1;
        Ok(v)
    };
    let read_i32 = |buf: &[u8], off: &mut usize| -> Result<i32, String> {
        if *off + 4 > buf.len() {
            return Err(format!("Unexpected EOF reading i32 at offset {}", *off));
        }
        let v = i32::from_le_bytes(buf[*off..*off + 4].try_into().unwrap());
        *off += 4;
        Ok(v)
    };
    let read_u32 = |buf: &[u8], off: &mut usize| -> Result<u32, String> {
        if *off + 4 > buf.len() {
            return Err(format!("Unexpected EOF reading u32 at offset {}", *off));
        }
        let v = u32::from_le_bytes(buf[*off..*off + 4].try_into().unwrap());
        *off += 4;
        Ok(v)
    };
    let read_i64 = |buf: &[u8], off: &mut usize| -> Result<i64, String> {
        if *off + 8 > buf.len() {
            return Err(format!("Unexpected EOF reading i64 at offset {}", *off));
        }
        let v = i64::from_le_bytes(buf[*off..*off + 8].try_into().unwrap());
        *off += 8;
        Ok(v)
    };
    let read_f32 = |buf: &[u8], off: &mut usize| -> Result<f32, String> {
        if *off + 4 > buf.len() {
            return Err(format!("Unexpected EOF reading f32 at offset {}", *off));
        }
        let v = f32::from_le_bytes(buf[*off..*off + 4].try_into().unwrap());
        *off += 4;
        Ok(v)
    };
    let read_f64 = |buf: &[u8], off: &mut usize| -> Result<f64, String> {
        if *off + 8 > buf.len() {
            return Err(format!("Unexpected EOF reading f64 at offset {}", *off));
        }
        let v = f64::from_le_bytes(buf[*off..*off + 8].try_into().unwrap());
        *off += 8;
        Ok(v)
    };

    while offset < bytes.len() {
        let id = read_i64(bytes, &mut offset)?;
        let mode = read_u8(bytes, &mut offset)?;
        let radius = read_f32(bytes, &mut offset)?;
        let px = read_f64(bytes, &mut offset)?;
        let py = read_f64(bytes, &mut offset)?;
        let pz = read_f64(bytes, &mut offset)?;
        let flags = read_u8(bytes, &mut offset)?;

        let is_rigid = mode == DST_BALL_RIGID;
        // MassSector (`DestinyStructs.h:106-112`): included if mode != RIGID
        // double mass; uint8 cloak; int64 allianceID; int32 corporationID; int32 harmonic;
        let (mass, cloak, alliance_id, corp_id, harmonic) = if !is_rigid {
            let m = read_f64(bytes, &mut offset)?;
            let cl = read_u8(bytes, &mut offset)?;
            let aid = read_i64(bytes, &mut offset)?;
            let cid = read_i32(bytes, &mut offset)?;
            let harm = read_i32(bytes, &mut offset)?;
            (Some(m), Some(cl), Some(aid), Some(cid), Some(harm))
        } else {
            (None, None, None, None, None)
        };

        let (max_velocity, velocity, inertia, speed_fraction) =
            if !is_rigid && (flags & BALL_FLAG_IS_FREE) != 0 {
                let max_v = read_f32(bytes, &mut offset)?;
                let vx = read_f64(bytes, &mut offset)?;
                let vy = read_f64(bytes, &mut offset)?;
                let vz = read_f64(bytes, &mut offset)?;
                let inert = read_f32(bytes, &mut offset)?;
                let sf = read_f32(bytes, &mut offset)?;
                (
                    Some(max_v),
                    Some(DVec3::new(vx, vy, vz)),
                    Some(inert),
                    Some(sf),
                )
            } else {
                (None, None, None, None)
            };

        let formation_id = read_u8(bytes, &mut offset)?;
        let mut direction = None;
        let mut target_id = None;
        let mut follow_range = None;
        let mut owner_id = None;
        let mut effect_stamp = None;
        let mut warp_destination = None;
        let mut warp_speed_tenth_au_s = None;

        match mode {
            DST_BALL_GOTO => {
                let dx = read_f64(bytes, &mut offset)?;
                let dy = read_f64(bytes, &mut offset)?;
                let dz = read_f64(bytes, &mut offset)?;
                direction = Some(DVec3::new(dx, dy, dz));
            }
            DST_BALL_FOLLOW => {
                let tid = read_i64(bytes, &mut offset)?;
                let rng = read_f32(bytes, &mut offset)?;
                target_id = Some(tid);
                follow_range = Some(rng);
            }
            DST_BALL_ORBIT => {
                let tid = read_u32(bytes, &mut offset)? as i64;
                let rng = read_f64(bytes, &mut offset)? as f32;
                target_id = Some(tid);
                follow_range = Some(rng);
            }
            DST_BALL_WARP => {
                let wx = read_f64(bytes, &mut offset)?;
                let wy = read_f64(bytes, &mut offset)?;
                let wz = read_f64(bytes, &mut offset)?;
                let est = read_i32(bytes, &mut offset)?;
                let _follow_range = read_f64(bytes, &mut offset)?;
                let _follow_id = read_i64(bytes, &mut offset)?;
                let ws = read_u32(bytes, &mut offset)?;
                effect_stamp = Some(est);
                warp_destination = Some(DVec3::new(wx, wy, wz));
                warp_speed_tenth_au_s = Some(ws);
            }
            DST_BALL_MISSILE => {
                let tid = read_i64(bytes, &mut offset)?;
                let rng = read_f32(bytes, &mut offset)?;
                let oid = read_i64(bytes, &mut offset)?;
                let est = read_i32(bytes, &mut offset)?;
                let mx = read_f64(bytes, &mut offset)?;
                let my = read_f64(bytes, &mut offset)?;
                let mz = read_f64(bytes, &mut offset)?;
                target_id = Some(tid);
                follow_range = Some(rng);
                owner_id = Some(oid);
                effect_stamp = Some(est);
                direction = Some(DVec3::new(mx, my, mz));
            }
            DST_BALL_MUSHROOM => {
                let rng = read_f32(bytes, &mut offset)?;
                let _u125 = read_f64(bytes, &mut offset)?;
                let est = read_i32(bytes, &mut offset)?;
                let oid = read_i64(bytes, &mut offset)?;
                follow_range = Some(rng);
                effect_stamp = Some(est);
                owner_id = Some(oid);
            }
            DST_BALL_TROLL => {
                let est = read_i32(bytes, &mut offset)?;
                effect_stamp = Some(est);
            }
            DST_BALL_FORMATION => {
                let fid = read_i64(bytes, &mut offset)?;
                let rng = read_f32(bytes, &mut offset)?;
                let est = read_i32(bytes, &mut offset)?;
                target_id = Some(fid);
                follow_range = Some(rng);
                effect_stamp = Some(est);
            }
            DST_BALL_STOP | DST_BALL_BOID | DST_BALL_MINIBALL | DST_BALL_FIELD
            | DST_BALL_RIGID => {}
            other => {
                return Err(format!("Unknown Destiny ball mode byte {other}"));
            }
        }

        balls.push(DecodedBall {
            id,
            mode,
            radius,
            position: DVec3::new(px, py, pz),
            flags,
            mass,
            cloak,
            alliance_id,
            corp_id,
            harmonic,
            max_velocity,
            velocity,
            inertia,
            speed_fraction,
            formation_id,
            direction,
            target_id,
            follow_range,
            owner_id,
            effect_stamp,
            warp_destination,
            warp_speed_tenth_au_s,
        });
    }

    Ok((packet_type, stamp, balls))
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

/// Builds a `foo.SlimItem` `PyRep::Object` for a `Ball` using [`SlimItemMeta`].
pub fn encode_slim_item_with_meta(ball: &Ball, meta: &SlimItemMeta) -> PyRep {
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
    PyRep::Object {
        type_name: "foo.SlimItem".into(),
        args: Box::new(PyRep::keyval([
            ("itemID", PyRep::Integer(ball.id)),
            ("typeID", PyRep::Integer(ball.type_id as i64)),
            ("groupID", PyRep::Integer(meta.group_id as i64)),
            ("categoryID", PyRep::Integer(meta.category_id as i64)),
            ("ownerID", PyRep::Integer(meta.owner_id)),
            ("charID", char_rep),
            ("corpID", corp_rep),
            ("allianceID", alliance_rep),
            ("securityStatus", PyRep::Real(meta.security_status)),
            ("radius", PyRep::Real(ball.radius)),
        ])),
    }
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
    PyRep::Tuple(vec![
        PyRep::Tuple(vec![
            PyRep::Real(s),
            PyRep::Real(400_000.0),
            PyRep::Integer(stamp as i64),
        ]),
        PyRep::Real(a),
        PyRep::Real(st),
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
        slims.push(encode_slim_item_with_meta(ball, &meta));
        if !park.is_static_ball(ball.id) {
            let dmg = encode_damage_state_tuple(park.combat_states.get(&ball.id), stamp);
            damage_dict.push((PyRep::Integer(ball.id), dmg));
        }
    }

    let drone_state = PyRep::Object {
        type_name: "util.Rowset".into(),
        args: Box::new(PyRep::keyval([
            (
                "header",
                PyRep::List(vec![
                    PyRep::String("droneID".into()),
                    PyRep::String("ownerID".into()),
                    PyRep::String("controllerID".into()),
                    PyRep::String("activityState".into()),
                    PyRep::String("typeID".into()),
                    PyRep::String("controllerOwnerID".into()),
                    PyRep::String("targetID".into()),
                ]),
            ),
            (
                "RowClass",
                PyRep::Token("util.Row".into()),
            ),
            ("lines", PyRep::List(vec![])),
        ])),
    };

    let sol_item = PyRep::Object {
        type_name: "util.Row".into(),
        args: Box::new(PyRep::keyval([
            (
                "header",
                PyRep::List(vec![
                    PyRep::String("itemID".into()),
                    PyRep::String("typeID".into()),
                    PyRep::String("ownerID".into()),
                    PyRep::String("locationID".into()),
                    PyRep::String("flagID".into()),
                    PyRep::String("contraband".into()),
                    PyRep::String("singleton".into()),
                    PyRep::String("quantity".into()),
                    PyRep::String("groupID".into()),
                    PyRep::String("categoryID".into()),
                    PyRep::String("customInfo".into()),
                ]),
            ),
            (
                "line",
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
            ),
            ("itemID", PyRep::Integer(park.solar_system_id as i64)),
            ("solarSystemID", PyRep::Integer(park.solar_system_id as i64)),
            ("typeID", PyRep::Integer(5)),
            ("ownerID", PyRep::Integer(1)),
            ("locationID", PyRep::Integer(park.constellation_id as i64)),
            ("flagID", PyRep::Integer(0)),
            ("contraband", PyRep::Integer(0)),
            ("singleton", PyRep::Integer(1)),
            ("quantity", PyRep::Integer(-1)),
            ("groupID", PyRep::Integer(5)),
            ("categoryID", PyRep::Integer(2)),
            ("customInfo", PyRep::String(String::new())),
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

/// Encodes an `AddBalls2` `DoDestinyUpdate` action tuple `(stamp, ("AddBalls2", ((state_bytes, extra_ball_data_list),)))`.
pub fn encode_add_balls2_action(stamp: u32, balls: &[&Ball], park: Option<&Ballpark>) -> PyRep {
    let state_bytes = encode_add_balls_with_park(1, stamp, balls, park);
    let mut extra_data = Vec::with_capacity(balls.len());
    for ball in balls {
        let meta = resolve_slim_item_meta(park, ball);
        let slim = encode_slim_item_with_meta(ball, &meta);
        let dmg = encode_damage_state_tuple(
            park.and_then(|p| p.combat_states.get(&ball.id)),
            stamp,
        );
        extra_data.push(PyRep::Tuple(vec![slim, dmg]));
    }
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
            return encode_add_balls2_action(stamp, &[ball], park);
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
            PyRep::Tuple(vec![
                PyRep::Integer(*ball_id),
                PyRep::Real(destination[0]),
                PyRep::Real(destination[1]),
                PyRep::Real(destination[2]),
                PyRep::Integer(0),
                PyRep::Integer((*warp_speed_au_s * 10.0).round() as i64),
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
                PyRep::Tuple(vec![
                    PyRep::Real(*shield_ratio),
                    PyRep::Real(*armor_ratio),
                    PyRep::Real(*structure_ratio),
                ]),
            ]),
        ]),
        DestinyEvent::TerminalExplosion {
            ball_id,
            wreck_ball_id,
        } => PyRep::Tuple(vec![
            PyRep::String("TerminalPlayDestructionEffect".into()),
            PyRep::Tuple(vec![
                PyRep::Integer(*ball_id),
                PyRep::Integer(*wreck_ball_id),
            ]),
        ]),
        DestinyEvent::Collision { ball_a, ball_b } => PyRep::Tuple(vec![
            PyRep::String("Collision".into()),
            PyRep::Tuple(vec![PyRep::Integer(*ball_a), PyRep::Integer(*ball_b)]),
        ]),
    };

    PyRep::Tuple(vec![PyRep::Integer(stamp as i64), action_tuple])
}

/// Encodes a slice of [`DestinyEvent`]s into a `DoDestinyUpdate` `PyRep::List` of `(stamp, (action, args))` tuples.
pub fn encode_destiny_updates(
    stamp: u32,
    events: &[DestinyEvent],
    park: Option<&Ballpark>,
) -> PyRep {
    PyRep::List(
        events
            .iter()
            .map(|ev| encode_destiny_event(stamp, ev, park))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AU_METERS;

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
            sample_ball(
                1001,
                MovementMode::GotoDirection {
                    direction: DVec3::new(0.6, 0.8, 0.0),
                },
            ),
            sample_ball(
                1002,
                MovementMode::Follow {
                    target_id: 40_009_076,
                    range: 2_500.0,
                },
            ),
            sample_ball(1003, MovementMode::Stop),
            sample_ball(
                1004,
                MovementMode::new_warp(
                    DVec3::ZERO,
                    DVec3::new(2.0 * AU_METERS, 1.0e8, -5.0e7),
                    3.0,
                ),
            ),
            sample_ball(
                1005,
                MovementMode::Orbit {
                    target_id: 60_003_760,
                    range: 5_000.0,
                },
            ),
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
        let (pkt_type, stamp, decoded) =
            decode_add_balls(&bytes).expect("decode_add_balls must succeed");
        assert_eq!(pkt_type, 0);
        assert_eq!(stamp, 42);
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
        // Exact total byte sizes per ball:
        // Header (38) + MassSector (25, except Rigid) + DataSector (36, except Field/Rigid) + ModeTail
        let expected_ball_sizes = [
            38 + 25 + 36 + 25, // GOTO (124)
            38 + 25 + 36 + 13, // FOLLOW (112)
            38 + 25 + 36 + 1,  // STOP (100)
            38 + 25 + 36 + 49, // WARP (148)
            38 + 25 + 36 + 13, // ORBIT (112)
            38 + 25 + 36 + 49, // MISSILE (148)
            38 + 25 + 36 + 25, // MUSHROOM (124)
            38 + 25 + 36 + 1,  // BOID (100)
            38 + 25 + 36 + 5,  // TROLL (104)
            38 + 25 + 36 + 1,  // MINIBALL (100)
            38 + 25 + 1,       // FIELD (64)
            38 + 1,            // RIGID (39)
            38 + 25 + 36 + 17, // FORMATION (116)
        ];
        for (i, dec) in decoded.iter().enumerate() {
            assert_eq!(dec.id, balls[i].id);
            assert_eq!(dec.mode, expected_modes[i]);
            assert!((dec.position - balls[i].position).length() < 1e-6);
            assert!((dec.radius - (balls[i].radius as f32)).abs() < 1e-3);
            let mut mode_buf = Vec::new();
            encode_single_ball_with_meta(&mut mode_buf, &balls[i], 42, None);
            assert_eq!(
                mode_buf.len(),
                expected_ball_sizes[i],
                "mode {} ball byte length must match DestinyStructs.h",
                expected_modes[i]
            );
        }

        // Verify MassSector layout (`DestinyStructs.h:106-112`): mass, cloak, allianceID, corporationID, harmonic
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
        // BallHeader is 38 bytes (8 + 1 + 4 + 24 + 1); MassSector starts at offset 38:
        // [38..46] mass (f64), [46] cloak (u8), [47..55] allianceID (i64), [55..59] corporationID (i32), [59..63] harmonic (i32)
        assert_eq!(
            i64::from_le_bytes(single_buf[47..55].try_into().unwrap()),
            99_000_001
        );
        assert_eq!(
            i32::from_le_bytes(single_buf[55..59].try_into().unwrap()),
            1_000_125
        );
        assert_eq!(
            i32::from_le_bytes(single_buf[59..63].try_into().unwrap()),
            -1
        );

        // Verify exact mode-tail byte offsets at offset 99 (38 Header + 25 MassSector + 36 DataSector)
        let mut orbit_buf = Vec::new();
        encode_single_ball_with_meta(&mut orbit_buf, &balls[4], 42, Some(&meta));
        assert_eq!(orbit_buf[99], 0xFF);
        assert_eq!(
            u32::from_le_bytes(orbit_buf[100..104].try_into().unwrap()),
            60_003_760
        );
        assert_eq!(
            f64::from_le_bytes(orbit_buf[104..112].try_into().unwrap()),
            5_000.0
        );

        let mut warp_buf = Vec::new();
        encode_single_ball_with_meta(&mut warp_buf, &balls[3], 42, Some(&meta));
        assert_eq!(warp_buf[99], 0xFF);
        assert_eq!(
            i32::from_le_bytes(warp_buf[124..128].try_into().unwrap()),
            42
        );
        assert_eq!(
            i64::from_le_bytes(warp_buf[128..136].try_into().unwrap()),
            (-1.0f64).to_bits() as i64
        );
        assert_eq!(
            i64::from_le_bytes(warp_buf[136..144].try_into().unwrap()),
            15_000.0f64.to_bits() as i64
        );
        assert_eq!(
            i32::from_le_bytes(warp_buf[144..148].try_into().unwrap()),
            30
        );

        let mut missile_buf = Vec::new();
        encode_single_ball_with_meta(&mut missile_buf, &balls[5], 42, Some(&meta));
        assert_eq!(
            i64::from_le_bytes(missile_buf[100..108].try_into().unwrap()),
            1001
        );
        assert_eq!(
            i64::from_le_bytes(missile_buf[112..120].try_into().unwrap()),
            90_000_001
        );
        assert_eq!(
            i32::from_le_bytes(missile_buf[120..124].try_into().unwrap()),
            42
        );

        let mut mushroom_buf = Vec::new();
        encode_single_ball_with_meta(&mut mushroom_buf, &balls[6], 42, Some(&meta));
        assert_eq!(
            i32::from_le_bytes(mushroom_buf[112..116].try_into().unwrap()),
            42
        );
        assert_eq!(
            i64::from_le_bytes(mushroom_buf[116..124].try_into().unwrap()),
            90_000_001
        );

        let mut troll_buf = Vec::new();
        encode_single_ball_with_meta(&mut troll_buf, &balls[8], 42, Some(&meta));
        assert_eq!(
            u32::from_le_bytes(troll_buf[100..104].try_into().unwrap()),
            42
        );

        let mut form_buf = Vec::new();
        encode_single_ball_with_meta(&mut form_buf, &balls[12], 42, Some(&meta));
        assert_eq!(
            i32::from_le_bytes(form_buf[112..116].try_into().unwrap()),
            42
        );

        // Check mode-specific decoded fields (`DestinyStructs.h:170-209`)
        assert_eq!(decoded[0].direction, Some(DVec3::new(0.6, 0.8, 0.0)));
        assert_eq!(decoded[1].target_id, Some(40_009_076));
        assert_eq!(decoded[1].follow_range, Some(2_500.0));
        assert_eq!(
            decoded[3].warp_destination,
            Some(DVec3::new(2.0 * AU_METERS, 1.0e8, -5.0e7))
        );
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
        ];
        let updates_pyrep = encode_destiny_updates(7, &events, Some(&park));
        let updates_bytes = eve_proto::marshal::encode(&updates_pyrep);
        let decoded_updates =
            eve_proto::marshal::decode(&updates_bytes).expect("DoDestinyUpdate list roundtrip");
        assert_eq!(decoded_updates, updates_pyrep);
    }
}
