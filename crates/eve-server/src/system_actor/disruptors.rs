//! Mobile warp disruptor anchoring and ECM burst area-of-effect execution.

use crate::system_actor::{ActiveModuleCycle, PendingAnchoringDisruptor, SystemCommand};
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
        ballpark.ball_group_ids.insert(id, 361);
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
    let prev_jammed: HashMap<BallId, (f64, Vec<BallId>, Option<eve_destiny::EcmJamSources>)> =
        ballpark
            .combat_states
            .iter()
            .map(|(&id, cs)| {
                (
                    id,
                    (
                        cs.ecm_jammed_remaining_s,
                        cs.ecm_jammed_by.clone(),
                        ballpark.ecm_jam_sources.get(&id).cloned(),
                    ),
                )
            })
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
        if let Some((rem, by, sources_opt)) = prev_jammed.get(&cid) {
            if let Some(cs) = ballpark.combat_states.get_mut(&cid) {
                cs.ecm_jammed_remaining_s = *rem;
                cs.ecm_jammed_by = by.clone();
            }
            if let Some(sources) = sources_opt {
                ballpark.ecm_jam_sources.insert(cid, sources.clone());
            } else {
                ballpark.ecm_jam_sources.remove(&cid);
            }
        } else {
            ballpark.clear_ecm_jam(cid);
        }
    }
    jammed
}

impl super::SystemHandle {
    pub fn try_remove_module_cycle(&self, ship_id: BallId, module_id: i64) {
        if !eve_db::lock_rank::held_lock_ranks().is_empty() {
            let (tx, _rx) = tokio::sync::oneshot::channel();
            let _ = self.try_send_actor_cmd(SystemCommand::TryRemoveModuleCycle {
                ship_id,
                module_id,
                wait_for_post_unlock: false,
                reply: tx,
            });
            return;
        }
        let wait_for_post_unlock = super::post_unlock::caller_can_wait_for_handle(self);
        let _ = super::ball_ops::dispatch_actor_sync_reply_with_deadline(
            self,
            "TryRemoveModuleCycle",
            super::ball_ops::ACTOR_CMD_TIMEOUT,
            (ship_id, module_id, wait_for_post_unlock),
            |(ship_id, module_id, wait_for_post_unlock), reply| SystemCommand::TryRemoveModuleCycle {
                ship_id,
                module_id,
                wait_for_post_unlock,
                reply,
            },
            |ctx, (ship_id, module_id, _)| {
                super::handle_cmds::try_remove_module_cycle_in_ctx(ctx, ship_id, module_id);
            },
        );
    }
}

pub(in crate::system_actor) fn apply_destiny_event_in_actor(
    ballpark: &mut Ballpark,
    ball_id: BallId,
    event: eve_destiny::DestinyEvent,
) -> bool {
    use eve_destiny::{DestinyEvent, MovementMode};
    let known = ballpark.balls.contains_key(&ball_id)
        || matches!(event, DestinyEvent::OnSovereigntyChanged { .. });
    if known {
        let scope = ballpark.balls.contains_key(&ball_id).then_some(ball_id);
        match event {
            DestinyEvent::SetBallFree {
                ball_id: id,
                is_free,
            } if id == ball_id => {
                ballpark.set_ball_free(id, is_free);
            }
            DestinyEvent::SetBallHarmonic {
                ball_id: id,
                harmonic,
            } if id == ball_id => {
                ballpark.set_ball_harmonic(id, harmonic);
            }
            DestinyEvent::SetBallInteractive {
                ball_id: id,
                is_interactive,
            } => {
                if ballpark.ball_interactive_overrides.get(&id).copied() != Some(is_interactive)
                    && !ballpark.set_ball_interactive(id, is_interactive)
                {
                    ballpark.ball_interactive_overrides.insert(id, is_interactive);
                    ballpark.emit_event(
                        scope,
                        DestinyEvent::SetBallInteractive {
                            ball_id: id,
                            is_interactive,
                        },
                    );
                }
            }
            DestinyEvent::SetBallRadius {
                ball_id: id,
                radius,
            } if id == ball_id => {
                ballpark.set_ball_radius(id, radius);
            }
            DestinyEvent::OnSovereigntyChanged { alliance_id, .. } => {
                ballpark.set_sovereignty(alliance_id);
            }
            DestinyEvent::OnClientControllerEvent {
                ball_id: id,
                target_id,
                event_name,
                value,
                delay,
            } if id == ball_id => {
                ballpark.emit_client_controller_event(id, target_id, event_name, value, delay);
            }
            DestinyEvent::OnShipStateUpdate { module_states } => {
                ballpark.update_ship_module_states(ball_id, module_states);
            }
            DestinyEvent::OnDbuffUpdated {
                ball_id: id,
                dbuffs,
            } if id == ball_id => {
                ballpark.update_dbuffs(id, dbuffs);
            }
            DestinyEvent::OnDotVictimUpdated {
                ball_id: id,
                flat_dps,
                pct_dps,
                remaining_ticks,
                expiry_filetime,
            } if id == ball_id => {
                ballpark.apply_breacher_dot_with_expiry(
                    id,
                    flat_dps,
                    pct_dps,
                    remaining_ticks,
                    expiry_filetime,
                );
            }
            DestinyEvent::OnDroneStateChange {
                drone_id,
                owner_id,
                controller_id,
                activity_state,
                type_id,
                controller_owner_id,
                target_id,
            } if drone_id == ball_id => {
                ballpark.update_drone_state(
                    drone_id,
                    owner_id,
                    controller_id,
                    activity_state,
                    type_id,
                    controller_owner_id,
                    target_id,
                );
            }
            DestinyEvent::GotoDirection {
                ball_id: id,
                direction,
            } if id == ball_id => {
                let _ = ballpark.set_mode(
                    id,
                    MovementMode::GotoDirection {
                        direction: glam::DVec3::from_array(direction),
                    },
                );
            }
            DestinyEvent::SetSpeedFraction {
                ball_id: id,
                fraction,
            } if id == ball_id => {
                ballpark.set_speed_fraction(id, fraction);
            }
            DestinyEvent::Stop { ball_id: id } if id == ball_id => {
                let _ = ballpark.set_mode(id, MovementMode::Stop);
            }
            other => {
                if let DestinyEvent::OnSlimItemChange { ball_id: slim_id } = &other {
                    if let Some(meta) = ballpark.ball_slim_meta.get_mut(slim_id) {
                        meta.char_id = (*slim_id == ball_id).then_some(meta.owner_id);
                    }
                }
                ballpark.emit_event(scope, other);
            }
        }
    }
    known
}

#[cfg(test)]
mod set_ball_interactive_tests {
    use super::*;

    #[test]
    fn set_ball_interactive_unapplied_ball_emits_single_event() {
        let mut bp = Ballpark::new(30_000_142);
        bp.add_ball(Ball {
            id: 1001, type_id: 587, position: DVec3::ZERO, velocity: DVec3::ZERO, max_velocity: 100.0,
            inertia: 1.0, mass: 1_000_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        });
        assert!(apply_destiny_event_in_actor(&mut bp, 1001, eve_destiny::DestinyEvent::SetBallInteractive { ball_id: 2002, is_interactive: true }));
        let count = bp.pending_events.iter().filter(|ev| matches!(ev, eve_destiny::DestinyEvent::SetBallInteractive { ball_id: 2002, is_interactive: true })).count();
        assert_eq!(count, 1);
        assert_eq!(bp.ball_interactive_overrides.get(&2002), Some(&true));
    }
}

#[cfg(test)]
#[path = "disruptors_tests.rs"]
mod disruptors_tests;

