//! Mobile warp disruptor anchoring and ECM burst area-of-effect execution.

use crate::system_actor::{ActiveModuleCycle, PendingAnchoringDisruptor};
use eve_destiny::{Ball, BallId, Ballpark, MovementMode};
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

pub(crate) fn handle_schedule_mobile_warp_disruptor(
    ballpark: &mut Ballpark,
    pending_anchoring_disruptors: &mut Vec<PendingAnchoringDisruptor>,
    id: BallId,
    type_id: u32,
    position: DVec3,
    radius: f64,
    anchoring_delay_s: f64,
) -> bool {
    let ball = Ball {
        id,
        type_id,
        position,
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 1.0,
        mass: 50_000.0,
        radius: 70.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    if ballpark.balls.contains_key(&id) {
        let _ = ballpark.remove_ball(id);
    }
    let ok = ballpark.add_ball(ball);
    if ok {
        ballpark.set_ball_free(id, false);
        ballpark.set_ball_radius(id, radius);
        ballpark.set_ball_harmonic(id, 1);
    }
    pending_anchoring_disruptors.push(PendingAnchoringDisruptor {
        id,
        type_id,
        position,
        radius,
        remaining_s: anchoring_delay_s.max(1.0),
    });
    ok
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_apply_ecm_burst(
    ballpark: &mut Ballpark,
    active_module_cycles: &[ActiveModuleCycle],
    sde_opt: &Option<Arc<SdeRegistry>>,
    mechanic_rng_opt: &Option<Arc<crate::rng::MechanicRng>>,
    source_id: BallId,
    burst_radius_m: f64,
    jam_strengths: [f64; 4],
    _duration_s: f64,
    roll: f64,
    ship_eval_overrides: Option<HashMap<BallId, (usize, f64, f64)>>,
) -> Vec<BallId> {
    let source_pos = ballpark
        .balls
        .get(&source_id)
        .map(|b| b.position)
        .unwrap_or(DVec3::ZERO);
    let ball_meta: HashMap<BallId, (u32, f64)> = ballpark
        .balls
        .iter()
        .map(|(&id, b)| (id, (b.type_id, (b.position - source_pos).length())))
        .collect();
    let ewar_immune_ships: Vec<BallId> = active_module_cycles
        .iter()
        .filter(|c| c.self_ewar_immune)
        .map(|c| c.ship_id)
        .collect();
    let cloaked_ships: Vec<BallId> = ballpark
        .balls
        .keys()
        .copied()
        .filter(|&id| {
            ballpark
                .ball_slim_meta
                .get(&id)
                .is_some_and(|m| m.is_cloaked)
                || ballpark
                    .combat_states
                    .get(&id)
                    .is_some_and(|cs| cs.is_cloaked)
        })
        .collect();
    let prev_jammed: HashMap<BallId, f64> = ballpark
        .combat_states
        .iter()
        .map(|(&id, cs)| (id, cs.ecm_jammed_remaining_s))
        .collect();
    let mut cand_idx = 0usize;
    let jammed = ballpark.apply_ecm_burst(source_id, burst_radius_m + 1e-3, |cid| {
        if ewar_immune_ships.contains(&cid) || cloaked_ships.contains(&cid) {
            return false;
        }
        let Some(&(target_type_id, dist)) = ball_meta.get(&cid) else {
            return false;
        };
        if dist > burst_radius_m + 1e-3 {
            return false;
        }
        let is_ship = sde_opt
            .as_ref()
            .map(|sde| sde.types.get(&target_type_id).is_some_and(|t| t.category_id == 6))
            .unwrap_or(true);
        if !is_ship {
            return false;
        }
        let (sensor_idx, sensor_str, eff_roll) =
            if let Some(&(s_idx, s_str, s_roll)) = ship_eval_overrides.as_ref().and_then(|m| m.get(&cid)) {
                (s_idx.min(3), s_str.max(1.0), s_roll)
            } else {
                let (s_idx, s_str) = sde_opt
                    .as_ref()
                    .and_then(|sde| sde.types.get(&target_type_id))
                    .map(|t| {
                        let grav = t.default_attributes.get(&211).copied().unwrap_or(0.0);
                        let ladar = t.default_attributes.get(&209).copied().unwrap_or(0.0);
                        let mag = t.default_attributes.get(&210).copied().unwrap_or(0.0);
                        let radar = t.default_attributes.get(&208).copied().unwrap_or(0.0);
                        if grav >= ladar && grav >= mag && grav >= radar && grav > 0.0 {
                            (0usize, grav)
                        } else if ladar >= mag && ladar >= radar && ladar > 0.0 {
                            (1usize, ladar)
                        } else if mag >= radar && mag > 0.0 {
                            (2usize, mag)
                        } else if radar > 0.0 {
                            (3usize, radar)
                        } else {
                            (0usize, 10.0)
                        }
                    })
                    .unwrap_or((0usize, 10.0));
                let r = if cand_idx == 0 {
                    cand_idx += 1;
                    roll
                } else {
                    cand_idx += 1;
                    mechanic_rng_opt
                        .as_ref()
                        .and_then(|rng| rng.try_unit_f64())
                        .unwrap_or(roll)
                };
                (s_idx, s_str, r)
            };
        let jam_str = jam_strengths[sensor_idx];
        let chance = eve_dogma::compute_ecm_jam_chance(
            jam_str,
            sensor_str,
            dist.min(burst_radius_m),
            burst_radius_m,
            0.0,
        );
        chance > 0.0 && eff_roll < chance
    });
    for &cid in &jammed {
        if let Some(cs) = ballpark.combat_states.get_mut(&cid) {
            cs.ecm_jammed_remaining_s = prev_jammed.get(&cid).copied().unwrap_or(0.0);
        }
    }
    jammed
}
