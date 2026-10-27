//! `tactical_modules_probes` — Stasis Webification Probes and Tactical Module Formulas.

#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use eve_destiny::{Ballpark, DVec3};
use std::sync::{Arc, RwLock};

/// Computes Strontium Clathrates consumption for Siege Module based on Tactical Weapon Reconfiguration (`22043`).
pub fn compute_siege_strontium_cost(skill_level: u8) -> u32 {
    (250i32 - 25i32 * (skill_level.min(5) as i32)).max(125) as u32
}

/// Computes Strontium Clathrates consumption for Triage Module based on Tactical Logistics Reconfiguration (`27906`).
pub fn compute_triage_strontium_cost(skill_level: u8) -> u32 {
    (200i32 - 25i32 * (skill_level.min(5) as i32)).max(75) as u32
}

/// Scales Interdiction Nullifier (`group 4117`) duration (+10%/lvl) and reactivation delay (-10%/lvl)
/// based on the pilot's Interdiction Nullification (`57319`) skill level (`feat:modules.interdiction-nullifier`).
pub fn compute_nullifier_duration_and_cooldown(
    base_duration_s: f64,
    base_reactivation_delay_s: f64,
    skill_level: u8,
) -> (f64, f64) {
    let lvl = skill_level.min(5) as f64;
    let duration_s = base_duration_s * (1.0 + 0.10 * lvl);
    let reactivation_delay_s = (base_reactivation_delay_s * (1.0 - 0.10 * lvl)).max(0.0);
    (duration_s, reactivation_delay_s)
}

pub fn is_combatant_ship_or_entity_ball(
    sde_opt: Option<&Arc<eve_sde::SdeRegistry>>,
    db_opt: Option<&Arc<eve_db::WorldRepository>>,
    ball_id: i64,
    ball_type_id: u32,
) -> bool {
    if (40_000_000..80_000_000).contains(&ball_id) {
        return false;
    }
    let eff_tid = if ball_type_id != 0 {
        ball_type_id
    } else {
        db_opt
            .and_then(|db| db.with_snapshot(|s| s.items.get(&ball_id).map(|i| i.type_id)))
            .unwrap_or(0)
    };
    if eff_tid != 0 {
        if let Some(t) = sde_opt.and_then(|sde| sde.types.get(&eff_tid)) {
            if matches!(t.group_id, 12 | 186 | 340 | 448 | 649) {
                return false;
            }
            return matches!(t.category_id, 6 | 11 | 18 | 65 | 87);
        }
    }
    true
}

pub fn is_player_ship_ball(
    sde_opt: Option<&Arc<eve_sde::SdeRegistry>>,
    db_opt: Option<&Arc<eve_db::WorldRepository>>,
    ball_id: i64,
    ball_type_id: u32,
) -> bool {
    if (40_000_000..80_000_000).contains(&ball_id) {
        return false;
    }
    let eff_tid = if ball_type_id != 0 {
        ball_type_id
    } else {
        db_opt
            .and_then(|db| db.with_snapshot(|s| s.items.get(&ball_id).map(|i| i.type_id)))
            .unwrap_or(0)
    };
    if eff_tid != 0 {
        if let Some(t) = sde_opt.and_then(|sde| sde.types.get(&eff_tid)) {
            return t.category_id == 6;
        }
    }
    false
}

#[derive(Debug, Clone)]
pub struct PendingWubbleProbe {
    pub probe_id: i64,
    pub solar_system_id: u32,
    pub position: DVec3,
    pub radius_m: f64,
    pub remaining_arming_s: f64,
    pub duration_s: f64,
    pub velocity_multiplier: f64,
    pub detonated: bool,
}

#[derive(Debug, Clone)]
pub struct ActiveWubbleDebuff {
    pub remaining_s: f64,
    pub velocity_multiplier: f64,
    pub base_max_velocity: f64,
}

/// Registers a newly launched Stasis Webification Probe (`56748`) with a `3.0s` arming delay.
pub fn register_launched_wubble_probe(ctx: &ServerContext, probe_id: i64, solar_system_id: u32, position: DVec3, radius_m: f64) {
    let db_key = tactical_modules::db_ptr_key(&ctx.db);
    ctx.tactical_state.write().unwrap_or_else(|e| e.into_inner()).pending_wubble_probes.push((
        db_key,
        PendingWubbleProbe {
            probe_id,
            solar_system_id,
            position,
            radius_m,
            remaining_arming_s: 3.0,
            duration_s: 30.0,
            velocity_multiplier: 0.70,
            detonated: false,
        },
    ));
}

/// Advances pending Stasis Webification Probes (3s arming delay) and active 30s non-stacking wubble debuffs.
pub fn step_wubble_probes_and_debuffs(
    ballpark: &mut Ballpark,
    sde_opt: &Option<Arc<eve_sde::SdeRegistry>>,
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
    tactical_state: &RwLock<tactical_modules::TacticalModulesState>,
    dt: f64,
) {
    let Some(db_ref) = db_opt else {
        return;
    };
    let db_key = tactical_modules::db_ptr_key(db_ref);
    let mut st = tactical_state.write().unwrap_or_else(|e| e.into_inner());

    let ship_ids: Vec<i64> = ballpark.balls.keys().copied().collect();
    for sid in &ship_ids {
        let mut expired_base = None;
        if let Some(debuff) = st.active_wubble_debuffs.get_mut(&(db_key, *sid)) {
            debuff.remaining_s -= dt;
            if debuff.remaining_s <= 1e-6 {
                expired_base = Some(debuff.base_max_velocity);
            }
        }
        if let Some(base_vel) = expired_base {
            st.active_wubble_debuffs.remove(&(db_key, *sid));
            if let Some(ball) = ballpark.balls.get_mut(sid) {
                ball.max_velocity = base_vel;
            }
        }
    }

    let mut idx = 0;
    while idx < st.pending_wubble_probes.len() {
        if st.pending_wubble_probes[idx].0 != db_key {
            idx += 1;
            continue;
        }
        st.pending_wubble_probes[idx].1.remaining_arming_s -= dt;
        if st.pending_wubble_probes[idx].1.remaining_arming_s <= 1e-6 {
            let (_, probe) = st.pending_wubble_probes.remove(idx);
            let eligible_sids: Vec<i64> = ballpark
                .balls
                .iter()
                .filter_map(|(&sid, ball)| {
                    if is_combatant_ship_or_entity_ball(sde_opt.as_ref(), db_opt.as_ref(), sid, ball.type_id)
                        && (ball.position - probe.position).length() <= probe.radius_m
                    {
                        Some(sid)
                    } else {
                        None
                    }
                })
                .collect();
            for sid in eligible_sids {
                if let Some(ball) = ballpark.balls.get_mut(&sid) {
                    if let Some(existing) = st.active_wubble_debuffs.get_mut(&(db_key, sid)) {
                        existing.remaining_s = probe.duration_s;
                        ball.max_velocity = existing.base_max_velocity * probe.velocity_multiplier;
                    } else {
                        let base_vel = ball.max_velocity;
                        ball.max_velocity = base_vel * probe.velocity_multiplier;
                        st.active_wubble_debuffs.insert((db_key, sid), ActiveWubbleDebuff {
                            remaining_s: probe.duration_s,
                            velocity_multiplier: probe.velocity_multiplier,
                            base_max_velocity: base_vel,
                        });
                    }
                }
            }
        } else {
            idx += 1;
        }
    }
}
