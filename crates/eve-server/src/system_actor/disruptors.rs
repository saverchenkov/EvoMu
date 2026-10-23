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

pub(crate) fn handle_apply_ecm_burst(
    ballpark: &mut Ballpark,
    active_module_cycles: &[ActiveModuleCycle],
    sde_opt: &Option<Arc<SdeRegistry>>,
    source_id: BallId,
    burst_radius_m: f64,
    jam_strengths: [f64; 4],
    duration_s: f64,
    roll: f64,
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
    let jammed = ballpark.apply_ecm_burst(source_id, burst_radius_m, |cid| {
        if ewar_immune_ships.contains(&cid) {
            return false;
        }
        let Some(&(target_type_id, dist)) = ball_meta.get(&cid) else {
            return false;
        };
        if dist > burst_radius_m {
            return false;
        }
        let (sensor_idx, sensor_str) = sde_opt
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
        let jam_str = jam_strengths[sensor_idx];
        let chance =
            eve_dogma::compute_ecm_jam_chance(jam_str, sensor_str, dist, burst_radius_m, 0.0);
        chance > 0.0 && roll < chance
    });
    for &cid in &jammed {
        if let Some(cs) = ballpark.combat_states.get_mut(&cid) {
            cs.ecm_jammed_remaining_s = duration_s;
        }
    }
    jammed
}
