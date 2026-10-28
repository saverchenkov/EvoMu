//! Target-lock management, ECM jam/burst application, and grid bubble observer queries for [`Ballpark`].

use crate::{Ball, BallId, Ballpark, DestinyEvent, MovementMode, TargetLock};
use std::collections::BTreeMap;

impl Ballpark {
    pub fn add_target_lock(&mut self, source_id: BallId, lock: TargetLock) -> bool {
        let source_blocked = self.combat_states.get(&source_id).is_some_and(|cs| {
            (cs.is_cloaked && cs.gate_cloak_remaining_ticks == 0)
                || cs.decloak_targeting_delay_remaining_s > 0.0
        });
        let target_untargetable = self.combat_states.get_mut(&lock.target_id).is_some_and(|cs| {
            cs.sync_action_breaks();
            cs.is_cloaked || cs.gate_cloak_remaining_ticks > 0 || cs.is_warp_exit_invulnerable()
        });
        if source_blocked || target_untargetable {
            self.pending_events.push(DestinyEvent::OnTarget {
                mode: "lost".to_string(),
                source_id,
                target_id: lock.target_id,
            });
            return false;
        }

        let mut uncloaked = false;
        if let Some(cs) = self.combat_states.get_mut(&source_id) {
            cs.undock_invulnerable_remaining_ticks = 0;
            cs.warp_exit_invulnerable_remaining_ticks = 0;
            cs.sync_invuln_with_undock_timer = false;
            if cs.gate_cloak_remaining_ticks > 0 {
                cs.gate_cloak_remaining_ticks = 0;
                cs.is_cloaked = false;
                uncloaked = true;
            }
        }
        if uncloaked {
            if let Some(meta) = self.ball_slim_meta.get_mut(&source_id) {
                meta.is_cloaked = false;
            }
            if let (Some(&gid), Some(b)) = (
                self.ball_grid_ids.get(&source_id),
                self.balls.get(&source_id).cloned(),
            ) {
                self.queue_event_for_grid_observers(
                    gid,
                    Some(source_id),
                    &DestinyEvent::AddBalls2 { balls: vec![b] },
                );
            }
        }
        self.target_locks.entry(source_id).or_default().push(lock);
        true
    }

    pub fn remove_target_lock(&mut self, source_id: BallId, target_id: BallId) -> bool {
        if let Some(locks) = self.target_locks.get_mut(&source_id) {
            let before = locks.len();
            locks.retain(|l| l.target_id != target_id);
            if locks.len() < before {
                self.pending_events.push(DestinyEvent::OnTarget {
                    mode: "lost".to_string(),
                    source_id,
                    target_id,
                });
                return true;
            }
        }
        false
    }

    pub fn clear_target_locks(&mut self, source_id: BallId) -> usize {
        if let Some(locks) = self.target_locks.remove(&source_id) {
            let count = locks.len();
            for l in &locks {
                self.pending_events.push(DestinyEvent::OnTarget {
                    mode: "lost".to_string(),
                    source_id,
                    target_id: l.target_id,
                });
            }
            count
        } else {
            0
        }
    }

    /// Applies an ECM jam to `target_id`, clearing all existing locked targets and preventing
    /// target locking for `duration_s` seconds (`feat:modules.ecm`, `DG-5`).
    pub fn apply_ecm_jam(&mut self, target_id: BallId, duration_s: f64) {
        if let Some(cs) = self.combat_states.get_mut(&target_id) {
            cs.sync_action_breaks();
            if cs.is_cloaked || cs.is_invulnerable() {
                return;
            }
            cs.ecm_jammed_remaining_s = duration_s;
        }
        self.clear_target_locks(target_id);
    }

    /// Applies an ECM burst AoE around `source_id`, jamming all valid target balls within `burst_radius_m`
    /// for which `jam_chance_test(target_id)` succeeds (`feat:modules.ecm`, `DG-5`).
    pub fn apply_ecm_burst<F>(
        &mut self,
        source_id: BallId,
        burst_radius_m: f64,
        mut jam_test: F,
    ) -> Vec<BallId>
    where
        F: FnMut(BallId) -> bool,
    {
        let Some(source_pos) = self.balls.get(&source_id).map(|b| b.position) else {
            return Vec::new();
        };
        let candidates: Vec<BallId> = self
            .balls
            .iter()
            .filter(|(&id, b)| {
                id != source_id
                    && !self.is_static_ball(id)
                    && self.combat_states.get(&id).is_some_and(|cs| {
                        let mut c = cs.clone();
                        c.sync_action_breaks();
                        !c.is_cloaked && !c.is_invulnerable()
                    })
                    && (b.position - source_pos).length() <= burst_radius_m
            })
            .map(|(&id, _)| id)
            .collect();

        let mut jammed = Vec::new();
        for cid in candidates {
            if jam_test(cid) {
                self.apply_ecm_jam(cid, 20.0);
                jammed.push(cid);
            }
        }
        jammed
    }

    pub fn get_target_locks(&self, source_id: BallId) -> Vec<TargetLock> {
        self.target_locks
            .get(&source_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Drains and returns all bubble-scoped `DestinyEvent`s accumulated for `observer_ball_id`.
    pub fn drain_observer_events(&mut self, observer_ball_id: BallId) -> Vec<DestinyEvent> {
        self.observer_events
            .get_mut(&observer_ball_id)
            .map(std::mem::take)
            .unwrap_or_default()
    }

    /// Drains and returns the map of `observer_ball_id -> Vec<DestinyEvent>` for all dynamic observers.
    pub fn drain_all_observer_events(&mut self) -> BTreeMap<BallId, Vec<DestinyEvent>> {
        let mut out = BTreeMap::new();
        for (&id, events) in &mut self.observer_events {
            if !events.is_empty() {
                out.insert(id, std::mem::take(events));
            }
        }
        out
    }

    /// Returns all balls visible to `observer_ball_id` in its shared [`GridBubble`]:
    /// all system-wide static balls (`Rigid` / `Field`) plus uncloaked dynamic balls in the same [`GridBubble`].
    pub fn visible_balls_for_observer(&self, observer_ball_id: BallId) -> Vec<&Ball> {
        let Some(obs_gid) = self.ball_grid_ids.get(&observer_ball_id).copied() else {
            return self
                .balls
                .values()
                .filter(|b| {
                    self.is_static_ball(b.id)
                        || matches!(b.mode, MovementMode::Rigid | MovementMode::Field)
                })
                .collect();
        };
        self.balls
            .values()
            .filter(|b| {
                if self.is_static_ball(b.id)
                    || matches!(b.mode, MovementMode::Rigid | MovementMode::Field)
                {
                    return true;
                }
                if b.id == observer_ball_id {
                    return true;
                }
                let in_grid = self.ball_grid_ids.get(&b.id).copied() == Some(obs_gid);
                let is_cloaked = self.combat_states.get(&b.id).is_some_and(|c| c.is_cloaked);
                in_grid && !is_cloaked
            })
            .collect()
    }

    /// Returns only the dynamic (non-static) balls in the same shared [`GridBubble`] as `observer_ball_id`.
    pub fn dynamic_balls_in_bubble(&self, observer_ball_id: BallId) -> Vec<&Ball> {
        let Some(obs_gid) = self.ball_grid_ids.get(&observer_ball_id).copied() else {
            return Vec::new();
        };
        let Some(grid) = self.grids.get(&obs_gid) else {
            return Vec::new();
        };
        grid.members
            .iter()
            .filter_map(|&id| {
                let b = self.balls.get(&id)?;
                if id != observer_ball_id
                    && self.combat_states.get(&id).is_some_and(|c| c.is_cloaked)
                {
                    return None;
                }
                Some(b)
            })
            .collect()
    }

    /// Returns all balls visible to `observer_ball_id` (or within `radius_meters` if `observer_ball_id` is static).
    pub fn balls_in_bubble(&self, observer_ball_id: BallId, radius_meters: f64) -> Vec<&Ball> {
        if self.ball_grid_ids.contains_key(&observer_ball_id) {
            return self.visible_balls_for_observer(observer_ball_id);
        }
        let Some(observer) = self.balls.get(&observer_ball_id) else {
            return Vec::new();
        };
        let r2 = radius_meters * radius_meters;
        self.balls
            .values()
            .filter(|b| {
                self.is_static_ball(b.id)
                    || matches!(b.mode, MovementMode::Rigid | MovementMode::Field)
                    || (b.position - observer.position).length_squared() <= r2
            })
            .collect()
    }
}
