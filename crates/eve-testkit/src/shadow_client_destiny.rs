//! Binary Destiny ballpark stream decoder for [`super::ShadowClient`].

#![forbid(unsafe_code)]

use super::{
    ShadowBall, ShadowBallMode, BALL_FLAG_IS_FREE, DST_BALL_BOID, DST_BALL_FIELD,
    DST_BALL_FOLLOW, DST_BALL_FORMATION, DST_BALL_GOTO, DST_BALL_MINIBALL, DST_BALL_MISSILE,
    DST_BALL_MUSHROOM, DST_BALL_ORBIT, DST_BALL_RIGID, DST_BALL_STOP, DST_BALL_TROLL,
    DST_BALL_WARP,
};

/// Decodes a binary `AddBalls` / `SetState` buffer (`packet_type: u8`, `stamp: u32`, ball sectors).
pub fn decode_destiny_ball_buffer(bytes: &[u8]) -> Result<(u8, u32, Vec<ShadowBall>), String> {
    if bytes.len() < 5 {
        return Err(format!("Destiny buffer too short: {} bytes", bytes.len()));
    }
    let packet_type = bytes[0];
    let stamp = u32::from_le_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]);
    let mut off = 5usize;
    let mut balls = Vec::new();

    let ru8 = |b: &[u8], o: &mut usize| -> Result<u8, String> {
        let v = *b.get(*o).ok_or_else(|| format!("EOF u8 at {}", *o))?;
        *o += 1;
        Ok(v)
    };
    let ri32 = |b: &[u8], o: &mut usize| -> Result<i32, String> {
        let s = b.get(*o..*o + 4).ok_or_else(|| format!("EOF i32 at {}", *o))?;
        *o += 4;
        Ok(i32::from_le_bytes(s.try_into().unwrap()))
    };
    let ri64 = |b: &[u8], o: &mut usize| -> Result<i64, String> {
        let s = b.get(*o..*o + 8).ok_or_else(|| format!("EOF i64 at {}", *o))?;
        *o += 8;
        Ok(i64::from_le_bytes(s.try_into().unwrap()))
    };
    let rf32 = |b: &[u8], o: &mut usize| -> Result<f32, String> {
        let s = b.get(*o..*o + 4).ok_or_else(|| format!("EOF f32 at {}", *o))?;
        *o += 4;
        Ok(f32::from_le_bytes(s.try_into().unwrap()))
    };
    let rf64 = |b: &[u8], o: &mut usize| -> Result<f64, String> {
        let s = b.get(*o..*o + 8).ok_or_else(|| format!("EOF f64 at {}", *o))?;
        *o += 8;
        Ok(f64::from_le_bytes(s.try_into().unwrap()))
    };

    while off < bytes.len() {
        let ball_id = ri64(bytes, &mut off)?;
        let mode_byte = ru8(bytes, &mut off)?;
        let radius = f64::from(rf32(bytes, &mut off)?);
        let (px, py, pz) = (rf64(bytes, &mut off)?, rf64(bytes, &mut off)?, rf64(bytes, &mut off)?);
        let flags = ru8(bytes, &mut off)?;
        let is_rigid = mode_byte == DST_BALL_RIGID;
        let is_cloaked = if !is_rigid {
            let _mass = rf64(bytes, &mut off)?;
            let cloak = ru8(bytes, &mut off)?;
            let _alliance_id = ri64(bytes, &mut off)?;
            let _corp_id = ri32(bytes, &mut off)?;
            let _harmonic = ri32(bytes, &mut off)?;
            cloak != 0
        } else {
            false
        };
        let is_free = !is_rigid && (flags & BALL_FLAG_IS_FREE) != 0;
        let (max_velocity, velocity, speed_fraction) = if is_free {
            let max_v = f64::from(rf32(bytes, &mut off)?);
            let (vx, vy, vz) = (rf64(bytes, &mut off)?, rf64(bytes, &mut off)?, rf64(bytes, &mut off)?);
            let _inertia = rf32(bytes, &mut off)?;
            let sf = f64::from(rf32(bytes, &mut off)?);
            (Some(max_v), [vx, vy, vz], sf)
        } else {
            (None, [0.0, 0.0, 0.0], 0.0)
        };
        let _formation_id = ru8(bytes, &mut off)?;
        let mut target_id = None;
        let mut follow_range = None;
        let mut warp_destination = None;
        let mut warp_speed_milli_au_s = None;

        match mode_byte {
            DST_BALL_GOTO => {
                let (_, _, _) = (rf64(bytes, &mut off)?, rf64(bytes, &mut off)?, rf64(bytes, &mut off)?);
            }
            DST_BALL_FOLLOW | DST_BALL_ORBIT => {
                target_id = Some(ri64(bytes, &mut off)?);
                follow_range = Some(f64::from(rf32(bytes, &mut off)?));
            }
            DST_BALL_WARP => {
                let (wx, wy, wz) = (rf64(bytes, &mut off)?, rf64(bytes, &mut off)?, rf64(bytes, &mut off)?);
                let (_, _, _) = (ri32(bytes, &mut off)?, rf64(bytes, &mut off)?, ri64(bytes, &mut off)?);
                warp_destination = Some([wx, wy, wz]);
                warp_speed_milli_au_s = Some(ri64(bytes, &mut off)? as u32);
            }
            DST_BALL_MISSILE => {
                target_id = Some(ri64(bytes, &mut off)?);
                follow_range = Some(f64::from(rf32(bytes, &mut off)?));
                let (_, _) = (ri64(bytes, &mut off)?, ri32(bytes, &mut off)?);
                let (_, _, _) = (rf64(bytes, &mut off)?, rf64(bytes, &mut off)?, rf64(bytes, &mut off)?);
            }
            DST_BALL_MUSHROOM => {
                follow_range = Some(f64::from(rf32(bytes, &mut off)?));
                let (_, _, _) = (rf64(bytes, &mut off)?, ri32(bytes, &mut off)?, ri64(bytes, &mut off)?);
            }
            DST_BALL_TROLL => {
                let _ = ri32(bytes, &mut off)?;
            }
            DST_BALL_FORMATION => {
                target_id = Some(ri64(bytes, &mut off)?);
                follow_range = Some(f64::from(rf32(bytes, &mut off)?));
                let _ = ri32(bytes, &mut off)?;
            }
            DST_BALL_STOP | DST_BALL_BOID | DST_BALL_MINIBALL | DST_BALL_FIELD | DST_BALL_RIGID => {}
            other => return Err(format!("Unknown Destiny ball mode byte {other}")),
        }

        balls.push(ShadowBall {
            ball_id,
            mode: ShadowBallMode::from_mode_byte(mode_byte),
            position: [px, py, pz],
            velocity,
            radius,
            max_velocity,
            speed_fraction,
            is_free,
            is_cloaked,
            target_id,
            follow_range,
            warp_destination,
            warp_speed_milli_au_s,
        });
    }
    Ok((packet_type, stamp, balls))
}
