//! Binary `Destiny` `AddBalls` / `SetState` decoder (`P-7`).

use crate::wire::{
    BALL_FLAG_IS_FREE, DST_BALL_BOID, DST_BALL_FIELD, DST_BALL_FOLLOW, DST_BALL_FORMATION,
    DST_BALL_GOTO, DST_BALL_MINIBALL, DST_BALL_MISSILE, DST_BALL_MUSHROOM, DST_BALL_ORBIT,
    DST_BALL_RIGID, DST_BALL_STOP, DST_BALL_TROLL, DST_BALL_WARP,
};
use crate::BallId;
use glam::DVec3;

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

/// Decodes a binary `AddBalls` / `SetState` buffer produced by [`crate::wire::encode_add_balls`]
/// or [`crate::wire::encode_add_balls_with_type`]. Returns `(packet_type, stamp, decoded_balls)`.
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
                let tid = read_i64(bytes, &mut offset)?;
                let rng = read_f32(bytes, &mut offset)?;
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
                let ws = read_i64(bytes, &mut offset)? as u32;
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
