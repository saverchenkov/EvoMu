//! Lock-free `SystemView` snapshot, damage fractions, and system-wide effects state for `system_actor`.

#![forbid(unsafe_code)]

use super::*;
use eve_destiny::{CombatState, GridBubble, MovementMode, TargetLock, WarpBubbleMgr};
use std::collections::{BTreeMap, BTreeSet, HashSet};

/// Normalized shield, armor, and structure HP fractions (`0.0..=1.0`) for a ball.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BallDamageFractions {
    pub shield: f64,
    pub armor: f64,
    pub structure: f64,
}

impl BallDamageFractions {
    /// Computes normalized HP fractions `[0.0, 1.0]` from a [`CombatState`].
    pub fn from_combat_state(cs: &CombatState) -> Self {
        let ratio = |hp: f64, max: f64| -> f64 {
            if max > 0.0 && max.is_finite() {
                (hp / max).clamp(0.0, 1.0)
            } else {
                0.0
            }
        };
        Self {
            shield: ratio(cs.shield_hp, cs.max_shield_hp),
            armor: ratio(cs.armor_hp, cs.max_armor_hp),
            structure: ratio(cs.structure_hp, cs.max_structure_hp),
        }
    }
}

/// Immutable, lock-free snapshot of a solar system's live `Ballpark` and active-module
/// state published by the system actor and synchronous mutators (`RFC-1 §7.3`, `#1342`).
#[derive(Debug, Clone)]
pub struct SystemView {
    pub solar_system_id: SolarSystemId,
    pub current_tick: u64,
    pub sim_unix_secs: u64,
    pub tidi_factor: f64,
    pub sim_elapsed_secs: f64,
    pub balls: BTreeMap<BallId, Ball>,
    pub static_ball_ids: HashSet<BallId>,
    pub non_free_ball_ids: HashSet<BallId>,
    pub ball_group_ids: HashMap<BallId, u32>,
    pub ball_slim_meta: HashMap<BallId, SlimItemMeta>,
    pub combat_states: HashMap<BallId, CombatState>,
    pub scrambled_balls: HashSet<BallId>,
    pub cloaked_balls: HashSet<BallId>,
    pub tethered_balls: HashSet<BallId>,
    pub invulnerable_balls: HashSet<BallId>,
    pub warp_disrupted_balls: HashSet<BallId>,
    pub warp_bubble_mgr: WarpBubbleMgr,
    pub ball_grid_ids: HashMap<BallId, u64>,
    pub grids: BTreeMap<u64, GridBubble>,
    pub registered_observers: Option<BTreeSet<BallId>>,
    pub target_locks: BTreeMap<BallId, Vec<TargetLock>>,
    pub active_module_ships: HashSet<BallId>,
    pub active_bastion_ships: HashSet<BallId>,
    pub active_triage_ships: HashSet<BallId>,
    pub active_siege_ships: HashSet<BallId>,
    pub active_industrial_core_ships: HashSet<BallId>,
    pub active_microjump_ships: HashSet<BallId>,
    pub active_immobilize_or_no_assist_ships: HashSet<BallId>,
    pub warp_provenance: HashMap<BallId, WarpProvenance>,
    pub active_warps: HashMap<BallId, CharacterId>,
    pub return_warps: HashSet<BallId>,
    pub pending_warp_finished: HashSet<BallId>,
    pub active_drone_miners: HashMap<BallId, super::DroneMiningState>,
    pub drone_next_ready_unix: HashMap<BallId, u64>,
    pub active_drones_in_space: HashMap<BallId, super::DroneSpaceState>,
    pub active_drones_needs_sqlite_hydration: bool,
    pub stargate_activation_states: BTreeMap<BallId, eve_destiny::StargateActivationState>,
    pub active_module_cycles: Vec<ActiveModuleCycle>,
    pub external_active_modules: HashSet<(BallId, ItemId)>,
    pub active_nanite_repairs: Vec<ActiveNaniteRepair>,
    pub pending_anchoring_disruptor_count: usize,
    pub protection_registry: HashMap<BallId, ActorShipProtection>,
    pub fit_projections: HashMap<BallId, eve_dogma::FitProjection>,
    pub asteroids: HashMap<BallId, AsteroidState>,
    pub fighters: super::SystemFighterState,
    pub ewar: super::ActorEwarState,
    pub module_cooldowns: super::ActorModuleCooldownState,
}

impl Default for SystemView {
    fn default() -> Self {
        Self {
            solar_system_id: 0,
            current_tick: 0,
            sim_unix_secs: 0,
            tidi_factor: 1.0,
            sim_elapsed_secs: 0.0,
            balls: BTreeMap::new(),
            static_ball_ids: HashSet::new(),
            non_free_ball_ids: HashSet::new(),
            ball_group_ids: HashMap::new(),
            ball_slim_meta: HashMap::new(),
            combat_states: HashMap::new(),
            scrambled_balls: HashSet::new(),
            cloaked_balls: HashSet::new(),
            tethered_balls: HashSet::new(),
            invulnerable_balls: HashSet::new(),
            warp_disrupted_balls: HashSet::new(),
            warp_bubble_mgr: WarpBubbleMgr::default(),
            ball_grid_ids: HashMap::new(),
            grids: BTreeMap::new(),
            registered_observers: None,
            target_locks: BTreeMap::new(),
            active_module_ships: HashSet::new(),
            active_bastion_ships: HashSet::new(),
            active_triage_ships: HashSet::new(),
            active_siege_ships: HashSet::new(),
            active_industrial_core_ships: HashSet::new(),
            active_microjump_ships: HashSet::new(),
            active_immobilize_or_no_assist_ships: HashSet::new(),
            warp_provenance: HashMap::new(),
            active_warps: HashMap::new(),
            return_warps: HashSet::new(),
            pending_warp_finished: HashSet::new(),
            active_drone_miners: HashMap::new(),
            drone_next_ready_unix: HashMap::new(),
            active_drones_in_space: HashMap::new(),
            active_drones_needs_sqlite_hydration: false,
            stargate_activation_states: BTreeMap::new(),
            active_module_cycles: Vec::new(),
            external_active_modules: HashSet::new(),
            active_nanite_repairs: Vec::new(),
            pending_anchoring_disruptor_count: 0,
            protection_registry: HashMap::new(),
            fit_projections: HashMap::new(),
            asteroids: HashMap::new(),
            fighters: super::SystemFighterState::default(),
            ewar: super::ActorEwarState::default(),
            module_cooldowns: super::ActorModuleCooldownState::default(),
        }
    }
}

#[derive(Default)]
struct ViewActorSnapshot {
    sim_elapsed_secs: f64,
    active_module_ships: HashSet<BallId>,
    active_bastion_ships: HashSet<BallId>,
    active_triage_ships: HashSet<BallId>,
    active_siege_ships: HashSet<BallId>,
    active_industrial_core_ships: HashSet<BallId>,
    active_microjump_ships: HashSet<BallId>,
    active_immobilize_or_no_assist_ships: HashSet<BallId>,
    warp_provenance: HashMap<BallId, WarpProvenance>,
    active_warps: HashMap<BallId, CharacterId>,
    return_warps: HashSet<BallId>,
    pending_warp_finished: HashSet<BallId>,
    active_drone_miners: HashMap<BallId, super::DroneMiningState>,
    drone_next_ready_unix: HashMap<BallId, u64>,
    active_drones_in_space: HashMap<BallId, super::DroneSpaceState>,
    active_drones_needs_sqlite_hydration: bool,
    active_module_cycles: Vec<ActiveModuleCycle>,
    external_active_modules: HashSet<(BallId, ItemId)>,
    active_nanite_repairs: Vec<ActiveNaniteRepair>,
    pending_anchoring_disruptor_count: usize,
    protection_registry: HashMap<BallId, ActorShipProtection>,
    fit_projections: HashMap<BallId, eve_dogma::FitProjection>,
    asteroids: HashMap<BallId, AsteroidState>,
    fighters: super::SystemFighterState,
    ewar: super::ActorEwarState,
    module_cooldowns: super::ActorModuleCooldownState,
}

impl SystemView {
    /// Captures a consistent [`SystemView`] from `ballpark` and optional `actor_state`.
    ///
    /// When `actor_state` is `None` (because the caller only holds `ballpark`), `previous`
    /// supplies the previously observed `sim_elapsed_secs` and active-module ship sets so
    /// synchronous ballpark-only mutations do not erase active module flags.
    pub(crate) fn from_ballpark(ballpark: &Ballpark, actor_state: Option<&SystemActorState>, tidi_factor: f64, previous: Option<&SystemView>) -> Self {
        let (mut cloaked_balls, mut tethered_balls, mut invulnerable_balls, mut scrambled_balls, mut warp_disrupted_balls) =
            (HashSet::new(), HashSet::new(), HashSet::new(), HashSet::new(), HashSet::new());

        for (&ball_id, cs) in &ballpark.combat_states {
            if cs.has_any_cloak() {
                cloaked_balls.insert(ball_id);
            }
            if cs.is_tethered {
                tethered_balls.insert(ball_id);
            }
            if cs.is_protected() {
                invulnerable_balls.insert(ball_id);
            }
            if cs.is_warp_scrambled() {
                scrambled_balls.insert(ball_id);
            }
        }
        for (&ball_id, meta) in &ballpark.ball_slim_meta {
            if meta.is_cloaked {
                cloaked_balls.insert(ball_id);
            }
        }
        for (&ball_id, ball) in &ballpark.balls {
            if !ballpark.is_ball_bubble_immune(ball_id)
                && ballpark.warp_bubble_mgr.is_scrambled_at(ball.position)
            {
                warp_disrupted_balls.insert(ball_id);
                scrambled_balls.insert(ball_id);
            }
        }

        let actor = if let Some(st) = actor_state {
            let mut mods = HashSet::new();
            let mut bastion = HashSet::new();
            let mut triage = HashSet::new();
            let mut siege = HashSet::new();
            let mut ind_core = HashSet::new();
            let mut microjump = HashSet::new();
            let mut immobilize_or_no_assist = HashSet::new();
            for c in &st.active_module_cycles {
                mods.insert(c.ship_id);
                let eff_lower = c.effect_name.to_ascii_lowercase();
                if c.module_type_id == 33400 || eff_lower.contains("bastion") {
                    bastion.insert(c.ship_id);
                }
                if matches!(c.module_type_id, 27951 | 4294) || eff_lower.contains("triage") {
                    triage.insert(c.ship_id);
                }
                if matches!(c.module_type_id, 20280 | 4292) || eff_lower.contains("siege") {
                    siege.insert(c.ship_id);
                }
                if matches!(c.module_type_id, 28583 | 42890 | 58945 | 58950 | 62590 | 62591)
                    || eff_lower.contains("industrial")
                {
                    ind_core.insert(c.ship_id);
                }
                if c.mjd_jump_distance_m.is_some() || eff_lower.contains("microjump") {
                    microjump.insert(c.ship_id);
                }
                if c.immobilize_self || c.self_disallow_assistance {
                    immobilize_or_no_assist.insert(c.ship_id);
                }
            }
            for &(ship_id, _) in &st.external_active_modules {
                mods.insert(ship_id);
            }
            ViewActorSnapshot {
                sim_elapsed_secs: st.sim_elapsed_secs,
                active_module_ships: mods,
                active_bastion_ships: bastion,
                active_triage_ships: triage,
                active_siege_ships: siege,
                active_industrial_core_ships: ind_core,
                active_microjump_ships: microjump,
                active_immobilize_or_no_assist_ships: immobilize_or_no_assist,
                warp_provenance: st.warp_state.provenance.clone(),
                active_warps: st.warp_state.active_warps.clone(),
                return_warps: st.warp_state.return_warps.clone(),
                pending_warp_finished: st.warp_state.pending_warp_finished.clone(),
                active_drone_miners: st.active_drone_miners.clone(),
                drone_next_ready_unix: st.drone_next_ready_unix.clone(),
                active_drones_in_space: st.active_drones_in_space.clone(),
                active_drones_needs_sqlite_hydration: st.active_drones_needs_sqlite_hydration,
                active_module_cycles: st.active_module_cycles.clone(),
                external_active_modules: st.external_active_modules.clone(),
                active_nanite_repairs: st.active_nanite_repairs.clone(),
                pending_anchoring_disruptor_count: st.pending_anchoring_disruptors.len(),
                protection_registry: st.protection_registry.clone(),
                fit_projections: st.fit_projections.clone(),
                asteroids: st.asteroids.clone(),
                fighters: st.fighters.clone(),
                ewar: st.ewar_state.clone(),
                module_cooldowns: st.module_cooldowns.clone(),
            }
        } else if let Some(prev) = previous {
            ViewActorSnapshot {
                sim_elapsed_secs: prev.sim_elapsed_secs,
                active_module_ships: prev.active_module_ships.clone(),
                active_bastion_ships: prev.active_bastion_ships.clone(),
                active_triage_ships: prev.active_triage_ships.clone(),
                active_siege_ships: prev.active_siege_ships.clone(),
                active_industrial_core_ships: prev.active_industrial_core_ships.clone(),
                active_microjump_ships: prev.active_microjump_ships.clone(),
                active_immobilize_or_no_assist_ships: prev
                    .active_immobilize_or_no_assist_ships
                    .clone(),
                warp_provenance: prev.warp_provenance.clone(),
                active_warps: prev.active_warps.clone(),
                return_warps: prev.return_warps.clone(),
                pending_warp_finished: prev.pending_warp_finished.clone(),
                active_drone_miners: prev.active_drone_miners.clone(),
                drone_next_ready_unix: prev.drone_next_ready_unix.clone(),
                active_drones_in_space: prev.active_drones_in_space.clone(),
                active_drones_needs_sqlite_hydration: prev.active_drones_needs_sqlite_hydration,
                active_module_cycles: prev.active_module_cycles.clone(),
                external_active_modules: prev.external_active_modules.clone(),
                active_nanite_repairs: prev.active_nanite_repairs.clone(),
                pending_anchoring_disruptor_count: prev.pending_anchoring_disruptor_count,
                protection_registry: prev.protection_registry.clone(),
                fit_projections: prev.fit_projections.clone(),
                asteroids: prev.asteroids.clone(),
                fighters: prev.fighters.clone(),
                ewar: prev.ewar.clone(),
                module_cooldowns: prev.module_cooldowns.clone(),
            }
        } else {
            ViewActorSnapshot::default()
        };

        Self {
            solar_system_id: ballpark.solar_system_id,
            current_tick: ballpark.current_tick,
            sim_unix_secs: ballpark.sim_unix_secs,
            tidi_factor,
            sim_elapsed_secs: actor.sim_elapsed_secs,
            balls: ballpark.balls.clone(),
            static_ball_ids: ballpark.static_ball_ids.iter().copied().collect(),
            non_free_ball_ids: ballpark.non_free_ball_ids.iter().copied().collect(),
            ball_group_ids: ballpark.ball_group_ids.iter().map(|(&k, &v)| (k, v)).collect(),
            ball_slim_meta: ballpark
                .ball_slim_meta
                .iter()
                .map(|(&k, v)| (k, v.clone()))
                .collect(),
            combat_states: ballpark
                .combat_states
                .iter()
                .map(|(&k, v)| (k, v.clone()))
                .collect(),
            scrambled_balls,
            cloaked_balls,
            tethered_balls,
            invulnerable_balls,
            warp_disrupted_balls,
            warp_bubble_mgr: ballpark.warp_bubble_mgr.clone(),
            ball_grid_ids: ballpark.ball_grid_ids.iter().map(|(&k, &v)| (k, v)).collect(),
            grids: ballpark.grids.clone(),
            registered_observers: ballpark.registered_observers.clone(),
            target_locks: ballpark.target_locks.clone(),
            active_module_ships: actor.active_module_ships,
            active_bastion_ships: actor.active_bastion_ships,
            active_triage_ships: actor.active_triage_ships,
            active_siege_ships: actor.active_siege_ships,
            active_industrial_core_ships: actor.active_industrial_core_ships,
            active_microjump_ships: actor.active_microjump_ships,
            active_immobilize_or_no_assist_ships: actor.active_immobilize_or_no_assist_ships,
            warp_provenance: actor.warp_provenance,
            active_warps: actor.active_warps,
            return_warps: actor.return_warps,
            pending_warp_finished: actor.pending_warp_finished,
            active_drone_miners: actor.active_drone_miners,
            drone_next_ready_unix: actor.drone_next_ready_unix,
            active_drones_in_space: actor.active_drones_in_space,
            active_drones_needs_sqlite_hydration: actor.active_drones_needs_sqlite_hydration,
            stargate_activation_states: ballpark.stargate_activation_states.clone(),
            active_module_cycles: actor.active_module_cycles,
            external_active_modules: actor.external_active_modules,
            active_nanite_repairs: actor.active_nanite_repairs,
            pending_anchoring_disruptor_count: actor.pending_anchoring_disruptor_count,
            protection_registry: actor.protection_registry,
            fit_projections: actor.fit_projections,
            asteroids: actor.asteroids,
            fighters: actor.fighters,
            ewar: actor.ewar,
            module_cooldowns: actor.module_cooldowns,
        }
    }

    /// Mirrors `Ballpark::stargate_activation_state`.
    pub fn stargate_activation_state(
        &self,
        gate_id: BallId,
    ) -> Option<eve_destiny::StargateActivationState> {
        self.stargate_activation_states.get(&gate_id).copied()
    }

    pub fn ball(&self, ball_id: BallId) -> Option<&Ball> {
        self.balls.get(&ball_id)
    }

    pub fn contains_ball(&self, ball_id: BallId) -> bool {
        self.balls.contains_key(&ball_id)
    }

    pub fn ball_position(&self, ball_id: BallId) -> Option<DVec3> {
        self.balls.get(&ball_id).map(|b| b.position)
    }

    pub fn ball_velocity(&self, ball_id: BallId) -> Option<DVec3> {
        self.balls.get(&ball_id).map(|b| b.velocity)
    }

    pub fn movement_mode(&self, ball_id: BallId) -> Option<&MovementMode> {
        self.balls.get(&ball_id).map(|b| &b.mode)
    }

    pub fn group_id(&self, ball_id: BallId) -> Option<u32> {
        self.ball_slim_meta
            .get(&ball_id)
            .map(|m| m.group_id)
            .or_else(|| self.ball_group_ids.get(&ball_id).copied())
    }

    pub fn owner_id(&self, ball_id: BallId) -> Option<i64> {
        self.ball_slim_meta.get(&ball_id).map(|m| m.owner_id)
    }

    pub fn corp_id(&self, ball_id: BallId) -> Option<i64> {
        self.ball_slim_meta.get(&ball_id).map(|m| m.corp_id)
    }

    pub fn alliance_id(&self, ball_id: BallId) -> Option<i64> {
        self.ball_slim_meta.get(&ball_id).map(|m| m.alliance_id)
    }

    pub fn combat_state(&self, ball_id: BallId) -> Option<&CombatState> {
        self.combat_states.get(&ball_id)
    }

    pub fn slim_meta(&self, ball_id: BallId) -> Option<&SlimItemMeta> {
        self.ball_slim_meta.get(&ball_id)
    }

    pub fn damage_fractions(&self, ball_id: BallId) -> Option<BallDamageFractions> {
        self.combat_states
            .get(&ball_id)
            .map(BallDamageFractions::from_combat_state)
    }

    /// Mirrors `Ballpark::is_static_ball`.
    pub fn is_static_ball(&self, ball_id: BallId) -> bool {
        self.static_ball_ids.contains(&ball_id)
            || self
                .balls
                .get(&ball_id)
                .is_some_and(|b| matches!(b.mode, MovementMode::Rigid))
    }

    /// Mirrors `Ballpark::non_free_ball_ids`.
    pub fn is_non_free_ball(&self, ball_id: BallId) -> bool {
        self.non_free_ball_ids.contains(&ball_id)
    }

    pub fn is_ball_scrambled(&self, ball_id: BallId) -> bool {
        self.scrambled_balls.contains(&ball_id)
    }

    pub fn is_ball_cloaked(&self, ball_id: BallId) -> bool {
        self.cloaked_balls.contains(&ball_id)
    }

    pub fn is_gate_cloaked(&self, ball_id: BallId) -> bool {
        self.combat_states
            .get(&ball_id)
            .is_some_and(CombatState::has_gate_cloak)
    }

    pub fn is_ball_tethered(&self, ball_id: BallId) -> bool {
        self.tethered_balls.contains(&ball_id)
    }

    pub fn is_ball_invulnerable(&self, ball_id: BallId) -> bool {
        self.invulnerable_balls.contains(&ball_id)
    }

    pub fn is_ball_warp_disrupted(&self, ball_id: BallId) -> bool {
        self.warp_disrupted_balls.contains(&ball_id)
    }

    /// Mirrors `Ballpark::is_ball_bubble_immune`.
    pub fn is_ball_bubble_immune(&self, ball_id: BallId) -> bool {
        self.ball_group_ids.get(&ball_id) == Some(&31)
            || self
                .combat_states
                .get(&ball_id)
                .is_some_and(|cs| cs.is_interdiction_nullified)
    }

    pub fn has_active_module(&self, ball_id: BallId) -> bool {
        self.active_module_ships.contains(&ball_id)
    }

    pub fn has_active_bastion(&self, ball_id: BallId) -> bool {
        self.active_bastion_ships.contains(&ball_id)
    }

    pub fn has_active_triage(&self, ball_id: BallId) -> bool {
        self.active_triage_ships.contains(&ball_id)
    }

    pub fn has_active_siege(&self, ball_id: BallId) -> bool {
        self.active_siege_ships.contains(&ball_id)
    }

    pub fn has_active_industrial_core(&self, ball_id: BallId) -> bool {
        self.active_industrial_core_ships.contains(&ball_id)
    }

    pub fn has_active_microjump(&self, ball_id: BallId) -> bool {
        self.active_microjump_ships.contains(&ball_id)
    }

    pub fn has_immobilize_or_no_assist_module(&self, ball_id: BallId) -> bool {
        self.active_immobilize_or_no_assist_ships.contains(&ball_id)
    }

    pub fn warp_provenance(&self, ball_id: BallId) -> Option<WarpProvenance> {
        self.warp_provenance.get(&ball_id).copied()
    }

    pub fn active_warp_char(&self, ball_id: BallId) -> Option<CharacterId> {
        self.active_warps.get(&ball_id).copied()
    }

    pub fn is_active_warp(&self, ball_id: BallId) -> bool {
        self.active_warps.contains_key(&ball_id)
    }

    pub fn is_return_warp(&self, ball_id: BallId) -> bool {
        self.return_warps.contains(&ball_id)
    }

    pub fn is_pending_warp_finished(&self, ball_id: BallId) -> bool {
        self.pending_warp_finished.contains(&ball_id)
    }

    /// Yields every ball ID currently holding a lock (locking or locked) on `target_id`.
    pub fn targeters_of(&self, target_id: BallId) -> impl Iterator<Item = BallId> + '_ {
        self.target_locks
            .iter()
            .filter_map(move |(&source_id, locks)| {
                if source_id != target_id && locks.iter().any(|l| l.target_id == target_id) {
                    Some(source_id)
                } else {
                    None
                }
            })
    }

    pub fn is_ball_targeted(&self, target_id: BallId) -> bool {
        self.targeters_of(target_id).next().is_some()
    }

    /// Mirrors `Ballpark::bubble_id_for_ball`.
    pub fn bubble_id_for_ball(&self, ball_id: BallId) -> Option<u64> {
        if let Some(&gid) = self.ball_grid_ids.get(&ball_id) {
            return Some(gid);
        }
        let ball = self.balls.get(&ball_id)?;
        self.grids
            .values()
            .find(|g| g.contains(ball.position))
            .map(|g| g.id)
    }

    pub fn grid_bubble(&self, grid_id: u64) -> Option<&GridBubble> {
        self.grids.get(&grid_id)
    }

    /// Mirrors `Ballpark::visible_balls_for_observer`.
    pub fn visible_balls_for_observer(&self, observer_ball_id: BallId) -> Vec<&Ball> {
        let obs_gid = self.ball_grid_ids.get(&observer_ball_id).copied();
        self.balls
            .values()
            .filter(|b| {
                if self.is_static_ball(b.id)
                    || matches!(b.mode, MovementMode::Rigid | MovementMode::Field)
                {
                    return true;
                }
                let Some(gid) = obs_gid else {
                    return false;
                };
                b.id == observer_ball_id
                    || (self.ball_grid_ids.get(&b.id) == Some(&gid)
                        && !self.combat_states.get(&b.id).is_some_and(|c| c.is_cloaked))
            })
            .collect()
    }

    /// Mirrors `Ballpark::dynamic_balls_in_bubble`.
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
}

/// Per-`SystemRegistry` overrides for solar-system-wide environment effects (`#1583` Track 2).
#[derive(Debug, Default)]
pub struct SystemWideEffectsState {
    /// Active system-wide effect beacon `type_id` per `solar_system_id`.
    pub effects: HashMap<SolarSystemId, u32>,
    /// Overlapping system-wide effect beacon `type_id`s per `solar_system_id`.
    pub effect_lists: HashMap<SolarSystemId, Vec<u32>>,
}
