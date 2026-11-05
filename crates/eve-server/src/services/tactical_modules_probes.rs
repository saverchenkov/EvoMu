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
}

/// Returns active Stasis Webification Probe velocity multipliers (`0.05..=1.0`) currently affecting balls in `ballpark`.
pub fn active_wubble_multipliers_for_ballpark(
    ballpark: &Ballpark,
) -> std::collections::HashMap<i64, f64> {
    ballpark
        .active_wubble_multipliers
        .iter()
        .map(|(&ball_id, &mult)| (ball_id, mult))
        .collect()
}

/// Registers a newly launched Stasis Webification Probe (`56748`) using SDE `speedFactor` (`20`),
/// `doomsdayWarningDuration` (`2262`), `doomsdayAOERange` (`2279`), and `doomsdayAOEDuration` (`2280`).
pub fn register_launched_wubble_probe(
    ctx: &ServerContext,
    probe_id: i64,
    probe_type_id: u32,
    solar_system_id: u32,
    position: DVec3,
    fallback_radius_m: f64,
) {
    let probe_sde = ctx.sde.types.get(&probe_type_id);
    let radius_m = probe_sde
        .and_then(|t| t.default_attributes.get(&2279).copied())
        .filter(|&v| v > 0.0)
        .unwrap_or(fallback_radius_m);
    let arming_s = probe_sde
        .and_then(|t| t.default_attributes.get(&2262).copied())
        .map(|ms| (ms / 1000.0).max(0.0))
        .unwrap_or(2.0);
    let duration_s = probe_sde
        .and_then(|t| t.default_attributes.get(&2280).copied())
        .map(|ms| (ms / 1000.0).max(1.0))
        .unwrap_or(30.0);
    let velocity_multiplier = probe_sde
        .and_then(|t| t.default_attributes.get(&20).copied())
        .map(|sf| (1.0 + sf / 100.0).clamp(0.05, 1.0))
        .unwrap_or(0.80);

    let db_key = tactical_modules::db_ptr_key(&ctx.db);
    ctx.tactical_state
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .pending_wubble_probes
        .push((
            db_key,
            PendingWubbleProbe {
                probe_id,
                solar_system_id,
                position,
                radius_m,
                remaining_arming_s: arming_s,
                duration_s,
                velocity_multiplier,
                detonated: false,
            },
        ));
}

/// Advances pending Stasis Webification Probes and active non-stacking wubble debuffs without
/// overwriting `ball.max_velocity` directly (`reconcile_tackle_and_bubbles` composes them non-destructively).
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
        let expired = if let Some(debuff) = st.active_wubble_debuffs.get_mut(&(db_key, *sid)) {
            debuff.remaining_s -= dt;
            debuff.remaining_s <= 1e-6
        } else {
            false
        };
        if expired {
            st.active_wubble_debuffs.remove(&(db_key, *sid));
        }
    }

    let mut dead_or_detonated_ids = Vec::new();
    let pending = std::mem::take(&mut st.pending_wubble_probes);
    let mut kept_pending = Vec::with_capacity(pending.len());
    for (k, mut probe) in pending {
        if k != db_key || probe.solar_system_id != ballpark.solar_system_id {
            kept_pending.push((k, probe));
            continue;
        }
        let probe_alive = ballpark.balls.contains_key(&probe.probe_id)
            && ballpark
                .combat_states
                .get(&probe.probe_id)
                .is_none_or(|cs| cs.structure_hp > 0.0);
        if !probe_alive {
            st.active_interdiction_probes
                .remove(&(db_key, probe.probe_id));
            dead_or_detonated_ids.push(probe.probe_id);
            continue;
        }
        probe.remaining_arming_s -= dt;
        if probe.remaining_arming_s <= 1e-6 {
            for (&sid, ball) in &ballpark.balls {
                if is_combatant_ship_or_entity_ball(sde_opt.as_ref(), db_opt.as_ref(), sid, ball.type_id)
                    && (ball.position - probe.position).length() <= probe.radius_m
                {
                    st.active_wubble_debuffs.insert(
                        (db_key, sid),
                        ActiveWubbleDebuff {
                            remaining_s: probe.duration_s,
                            velocity_multiplier: probe.velocity_multiplier,
                        },
                    );
                }
            }
            st.active_interdiction_probes
                .remove(&(db_key, probe.probe_id));
            dead_or_detonated_ids.push(probe.probe_id);
        } else {
            kept_pending.push((k, probe));
        }
    }
    st.pending_wubble_probes = kept_pending;

    ballpark.active_wubble_multipliers.clear();
    for sid in &ship_ids {
        if let Some(debuff) = st.active_wubble_debuffs.get(&(db_key, *sid)) {
            ballpark
                .active_wubble_multipliers
                .insert(*sid, debuff.velocity_multiplier);
        }
    }
    drop(st);
    for pid in dead_or_detonated_ids {
        ballpark.remove_warp_bubble(pid);
        let _ = ballpark.remove_ball(pid);
    }
}

#[cfg(test)]
#[path = "tactical_modules_probes_tests.rs"]
mod tactical_modules_probes_tests;

