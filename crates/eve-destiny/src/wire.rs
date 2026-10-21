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
    let tau_ms = cs.map(|c| c.shield_recharge_time_ms).unwrap_or(120_000.0);
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

fn encode_ball_extra_list(balls: &[&Ball], park: Option<&Ballpark>, stamp: u32) -> Vec<PyRep> {
    balls
        .iter()
        .map(|ball| {
            let meta = resolve_slim_item_meta(park, ball);
            let slim = encode_slim_item_with_meta(ball, &meta);
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
    let slim = encode_slim_item_with_meta(ball, &meta);
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

/// Encodes an `AddBalls` `DoDestinyUpdate` action tuple `(stamp, ("AddBalls", ((state_bytes, extra_ball_data_list),)))`.
pub fn encode_add_balls_action(stamp: u32, balls: &[&Ball], park: Option<&Ballpark>) -> PyRep {
    let state_bytes = encode_add_balls_with_park(0, stamp, balls, park);
    let extra_data = encode_ball_extra_list(balls, park, stamp);
    PyRep::Tuple(vec![
        PyRep::Integer(stamp as i64),
        PyRep::Tuple(vec![
            PyRep::String("AddBalls".into()),
            PyRep::Tuple(vec![PyRep::Tuple(vec![
                PyRep::Bytes(state_bytes),
                PyRep::List(extra_data),
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
                damage_state_wire(
                    *shield_ratio,
                    park.and_then(|p| p.combat_states.get(ball_id))
                        .map(|c| c.shield_recharge_time_ms)
                        .unwrap_or(120_000.0),
                    *armor_ratio,
                    *structure_ratio,
                    stamp,
                ),
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
            PyRep::Tuple(vec![PyRep::Integer(*ball_id)]),
        ]),
        DestinyEvent::OnDeactivatingWarp { ball_id } => PyRep::Tuple(vec![
            PyRep::String("OnDeactivatingWarp".into()),
            PyRep::Tuple(vec![PyRep::Integer(*ball_id)]),
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
            PyRep::String("CloakBall".into()),
            PyRep::Tuple(vec![PyRep::Integer(*ball_id), PyRep::Integer(i64::from(*cloaked))]),
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
        } => {
            let apps = if *remaining_ticks > 0 {
                vec![PyRep::keyval([
                    ("attackerID", PyRep::Integer(0)),
                    ("targetID", PyRep::Integer(*ball_id)),
                    ("weaponTypeID", PyRep::Integer(84_115)),
                    ("expiryTime", PyRep::Integer((stamp as i64 + *remaining_ticks as i64) * 10_000_000)),
                    ("maxDamage", PyRep::Real(*flat_dps)),
                    ("maxHPNormalizedRatio", PyRep::Real(*pct_dps)),
                    ("activityState", PyRep::Integer(1)),
                ])]
            } else {
                Vec::new()
            };
            PyRep::Tuple(vec![
                PyRep::String("OnDotVictimUpdated".into()),
                PyRep::Tuple(vec![PyRep::Integer(*ball_id), PyRep::List(apps)]),
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
                        .unwrap_or(120_000.0),
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
                .and_then(|p| p.balls.get(ball_id).map(|b| (b, resolve_slim_item_meta(Some(p), b))))
                .map(|(b, m)| encode_slim_item_with_meta(b, &m))
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
        DestinyEvent::SetBallHarmonic { ball_id, harmonic } => PyRep::Tuple(vec![
            PyRep::String("SetBallHarmonic".into()),
            PyRep::Tuple(vec![PyRep::Integer(*ball_id), PyRep::Integer(*harmonic as i64)]),
        ]),
        DestinyEvent::SetBallInteractive { ball_id, is_interactive } => PyRep::Tuple(vec![
            PyRep::String("SetBallInteractive".into()),
            PyRep::Tuple(vec![PyRep::Integer(*ball_id), PyRep::Integer(i64::from(*is_interactive))]),
        ]),
        DestinyEvent::SetBallRadius { ball_id, radius } => PyRep::Tuple(vec![
            PyRep::String("SetBallRadius".into()),
            PyRep::Tuple(vec![PyRep::Integer(*ball_id), PyRep::Real(*radius)]),
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
                PyRep::Tuple(vec![
                    PyRep::Real(start_pos[0]),
                    PyRep::Real(start_pos[1]),
                    PyRep::Real(start_pos[2]),
                ]),
                PyRep::Tuple(vec![
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
        } => PyRep::Tuple(vec![
            PyRep::String("OnDroneActivityChange".into()),
            PyRep::Tuple(vec![PyRep::Integer(*drone_id), PyRep::Integer(*activity_state)]),
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
        DestinyEvent::OnFleetStateChange {
            fleet_id,
            member_id,
            fleet_state,
        } => PyRep::Tuple(vec![
            PyRep::String("OnFleetStateChange".into()),
            PyRep::Tuple(vec![
                PyRep::Integer(*fleet_id),
                PyRep::Integer(*member_id),
                PyRep::Integer(*fleet_state),
            ]),
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
