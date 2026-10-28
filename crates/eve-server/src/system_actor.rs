//! `system_actor` — Bounded Actor Mailbox and Subscription Handle for Solar System `Ballpark`s.

#![forbid(unsafe_code)]

mod celestials;
pub use celestials::*;
mod belts;
pub use belts::*;
mod cycles;
pub(crate) use cycles::*;
mod cycles_tackle;
pub(crate) use cycles_tackle::*;
mod disruptors;
pub(crate) use disruptors::*;
mod types;
pub use types::*;

use belts::{
    build_tick_broadcast, ensure_default_slim_meta, handle_mine_asteroid,
    reconcile_spawned_belt_asteroids, respawn_due_asteroids,
};
use eve_destiny::wire::{
    encode_destiny_updates, encode_set_state_payload, encode_set_state_update,
};
use eve_destiny::{Ball, BallId, Ballpark, DestinyEvent, SlimItemMeta};
use eve_proto::PyRep;
use eve_sde::{SdeRegistry, SolarSystemId};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tokio::sync::{broadcast, mpsc};

// parity: plan:P-7, plan:SP-1, plan:SP-2, plan:SP-4, plan:SP-5
// parity: feat:navigation.celestials-balls, feat:navigation.bubbles-grid
// parity: feat:navigation.destiny-updates, feat:serverops.server-tick
// parity: notify:DoDestinyUpdate
// parity: destiny:AddBalls2, destiny:RemoveBalls, destiny:RemoveBall, destiny:OnSlimItemChange

/// Bounded mailbox capacity per solar system actor.
pub const SYSTEM_MAILBOX_CAPACITY: usize = 256;

/// DERIVED default maintenance/downtime belt respawn period in seconds (86,400s = 1 day),
/// configurable via `ServerConfig::belt_respawn_interval_secs` (`D-5`, `MN-03`).
/// Per EVE wiki `Asteroids_and_ore.md:69`, belts respawn after downtime only if there are no asteroids left in the belt.
pub const DEFAULT_BELT_RESPAWN_INTERVAL_SECS: u64 = 86_400;

/// Default daily server downtime UTC time of day in seconds (11:00 UTC = 39,600s per EVE wiki `Evergreen_Daily_Login_Rewards.md:21`, `Skillbook_Program.md:43`),
/// configurable via `ServerConfig::downtime_utc_secs_of_day` (`D-5`, `MN-03`).
pub const DEFAULT_DOWNTIME_UTC_SECS_OF_DAY: u64 = 39_600;

/// Computes the strictly next downtime Unix timestamp (`> now_unix`) aligned to `downtime_utc_secs_of_day`.
pub fn next_downtime_unix_secs(now_unix: u64, interval_secs: u64, downtime_utc_secs_of_day: u64) -> u64 {
    let period = interval_secs.max(1);
    let phase = downtime_utc_secs_of_day % period;
    let rem = now_unix % period;
    let delta = if rem < phase { phase - rem } else { period - (rem - phase) };
    now_unix.saturating_add(delta)
}

pub struct SystemRegistry {
    systems: RwLock<HashMap<SolarSystemId, SystemHandle>>,
    sde: Option<Arc<SdeRegistry>>,
    pub db: Option<Arc<eve_db::WorldRepository>>,
    belt_respawn_interval_secs: Arc<std::sync::atomic::AtomicU64>,
    downtime_utc_secs_of_day: Arc<std::sync::atomic::AtomicU64>,
    /// Game-mechanic RNG (seeded test RNG): OS CSPRNG by default, shared with `ServerContext::mechanic_rng`.
    mechanic_rng: Arc<crate::rng::MechanicRng>,
    pub(crate) npc_death_hook: Arc<RwLock<Option<NpcDeathHook>>>,
    pub(crate) module_deactivation_hook: Arc<RwLock<Option<ModuleDeactivationHook>>>,
    pub(crate) system_step_hook: Arc<RwLock<Option<SystemStepHook>>>,
    /// PvE special-site balls (system, ball id) already spawned once into this registry's ballparks.
    /// Runtime-only (lives and dies with the ballparks), so no sentinel ball is needed in space.
    special_site_spawned_balls: std::sync::Mutex<std::collections::HashSet<(SolarSystemId, i64)>>,
    pub tactical_state: Arc<RwLock<crate::services::tactical_modules::TacticalModulesState>>,
}

impl Default for SystemRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemRegistry {
    pub fn new() -> Self {
        Self {
            systems: RwLock::new(HashMap::new()),
            sde: None,
            db: None,
            belt_respawn_interval_secs: Arc::new(std::sync::atomic::AtomicU64::new(DEFAULT_BELT_RESPAWN_INTERVAL_SECS)),
            downtime_utc_secs_of_day: Arc::new(std::sync::atomic::AtomicU64::new(DEFAULT_DOWNTIME_UTC_SECS_OF_DAY)),
            mechanic_rng: Arc::new(crate::rng::MechanicRng::os()),
            npc_death_hook: Arc::new(RwLock::new(None)),
            module_deactivation_hook: Arc::new(RwLock::new(None)),
            system_step_hook: Arc::new(RwLock::new(None)),
            special_site_spawned_balls: std::sync::Mutex::new(std::collections::HashSet::new()),
            tactical_state: Arc::new(RwLock::new(Default::default())),
        }
    }

    pub fn with_sde(sde: Arc<SdeRegistry>) -> Self {
        Self {
            systems: RwLock::new(HashMap::new()),
            sde: Some(sde),
            db: None,
            belt_respawn_interval_secs: Arc::new(std::sync::atomic::AtomicU64::new(DEFAULT_BELT_RESPAWN_INTERVAL_SECS)),
            downtime_utc_secs_of_day: Arc::new(std::sync::atomic::AtomicU64::new(DEFAULT_DOWNTIME_UTC_SECS_OF_DAY)),
            mechanic_rng: Arc::new(crate::rng::MechanicRng::os()),
            npc_death_hook: Arc::new(RwLock::new(None)),
            module_deactivation_hook: Arc::new(RwLock::new(None)),
            system_step_hook: Arc::new(RwLock::new(None)),
            special_site_spawned_balls: std::sync::Mutex::new(std::collections::HashSet::new()),
            tactical_state: Arc::new(RwLock::new(Default::default())),
        }
    }

    pub fn with_sde_and_db(sde: Arc<SdeRegistry>, db: Arc<eve_db::WorldRepository>) -> Self {
        Self {
            systems: RwLock::new(HashMap::new()),
            sde: Some(sde),
            db: Some(db),
            belt_respawn_interval_secs: Arc::new(std::sync::atomic::AtomicU64::new(DEFAULT_BELT_RESPAWN_INTERVAL_SECS)),
            downtime_utc_secs_of_day: Arc::new(std::sync::atomic::AtomicU64::new(DEFAULT_DOWNTIME_UTC_SECS_OF_DAY)),
            mechanic_rng: Arc::new(crate::rng::MechanicRng::os()),
            npc_death_hook: Arc::new(RwLock::new(None)),
            module_deactivation_hook: Arc::new(RwLock::new(None)),
            system_step_hook: Arc::new(RwLock::new(None)),
            special_site_spawned_balls: std::sync::Mutex::new(std::collections::HashSet::new()),
            tactical_state: Arc::new(RwLock::new(Default::default())),
        }
    }

    pub fn with_tactical_state(
        mut self,
        tactical_state: Arc<RwLock<crate::services::tactical_modules::TacticalModulesState>>,
    ) -> Self {
        self.tactical_state = tactical_state;
        self
    }

    pub fn set_npc_death_hook(&self, hook: NpcDeathHook) {
        *self.npc_death_hook.write().unwrap_or_else(|e| e.into_inner()) = Some(hook);
    }

    pub fn set_module_deactivation_hook(&self, hook: ModuleDeactivationHook) {
        *self.module_deactivation_hook.write().unwrap_or_else(|e| e.into_inner()) = Some(hook);
    }

    pub fn set_system_step_hook(&self, hook: SystemStepHook) {
        *self.system_step_hook.write().unwrap_or_else(|e| e.into_inner()) = Some(hook);
    }

    /// Shares `rng` as this registry's game-mechanic RNG (seeded test RNG).
    pub fn with_mechanic_rng(mut self, rng: Arc<crate::rng::MechanicRng>) -> Self {
        self.mechanic_rng = rng;
        self
    }

    pub fn belt_respawn_interval_secs(&self) -> u64 {
        self.belt_respawn_interval_secs
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn set_belt_respawn_interval_secs(&self, secs: u64) {
        self.belt_respawn_interval_secs
            .store(secs.clamp(1, 604_800), std::sync::atomic::Ordering::Relaxed);
    }

    pub fn downtime_utc_secs_of_day(&self) -> u64 {
        self.downtime_utc_secs_of_day
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn set_downtime_utc_secs_of_day(&self, secs: u64) {
        self.downtime_utc_secs_of_day
            .store(secs % 86_400, std::sync::atomic::Ordering::Relaxed);
    }
}

pub(crate) fn sync_ball_positions(
    ballpark: &Ballpark,
    cache: &RwLock<HashMap<BallId, DVec3>>,
    scrambled: &RwLock<std::collections::HashSet<BallId>>,
) {
    let mut guard = cache.write().unwrap_or_else(|e| e.into_inner());
    let mut scram_guard = scrambled.write().unwrap_or_else(|e| e.into_inner());
    guard.clear();
    scram_guard.clear();
    for (&id, ball) in &ballpark.balls {
        guard.insert(id, ball.position);
        if ballpark.is_ball_scrambled(id) {
            scram_guard.insert(id);
        }
    }
}

impl SystemRegistry {
    pub fn get_ball_position_sync(
        &self,
        solar_system_id: SolarSystemId,
        ball_id: BallId,
    ) -> Option<DVec3> {
        let guard = self.systems.read().unwrap_or_else(|e| e.into_inner());
        guard
            .get(&solar_system_id)
            .and_then(|h| h.get_ball_position_sync(ball_id))
    }

    pub fn is_ball_scrambled_sync(
        &self,
        solar_system_id: SolarSystemId,
        ball_id: BallId,
    ) -> bool {
        let guard = self.systems.read().unwrap_or_else(|e| e.into_inner());
        guard
            .get(&solar_system_id)
            .is_some_and(|h| h.is_ball_scrambled_sync(ball_id))
    }

    pub fn has_active_modules_or_cloak_sync(
        &self,
        solar_system_id: SolarSystemId,
        ball_id: BallId,
    ) -> bool {
        let guard = self.systems.read().unwrap_or_else(|e| e.into_inner());
        guard
            .get(&solar_system_id)
            .is_some_and(|h| h.has_active_modules_or_cloak_sync(ball_id))
    }

    pub fn find_ball_position_sync(&self, ball_id: BallId) -> Option<DVec3> {
        let guard = self.systems.read().unwrap_or_else(|e| e.into_inner());
        for handle in guard.values() {
            if let Some(pos) = handle.get_ball_position_sync(ball_id) {
                return Some(pos);
            }
        }
        None
    }

    pub fn find_ball_system_sync(&self, ball_id: BallId) -> Option<SolarSystemId> {
        let guard = self.systems.read().unwrap_or_else(|e| e.into_inner());
        for (&sys_id, handle) in guard.iter() {
            if handle.get_ball_position_sync(ball_id).is_some() {
                return Some(sys_id);
            }
        }
        None
    }

    pub fn get_system_sim_elapsed(&self, solar_system_id: SolarSystemId) -> f64 {
        let guard = self.systems.read().unwrap_or_else(|e| e.into_inner());
        guard
            .get(&solar_system_id)
            .map(|h| h.sim_elapsed_secs())
            .unwrap_or(0.0)
    }

    pub fn take_pending_cycle_actions(&self, solar_system_id: SolarSystemId) -> Vec<PendingCycleAction> {
        let guard = self.systems.read().unwrap_or_else(|e| e.into_inner());
        guard
            .get(&solar_system_id)
            .map(|h| h.take_pending_cycle_actions())
            .unwrap_or_default()
    }

    pub fn active_systems_count(&self) -> usize {
        self.systems.read().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub fn active_system_handles(&self) -> Vec<(SolarSystemId, SystemHandle)> {
        self.systems
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(&id, h)| (id, h.clone()))
            .collect()
    }

    /// Records that PvE special-site ball `ball_id` has been spawned in `solar_system_id`.
    /// Returns `true` only the first time, so destroyed site NPCs are never re-spawned.
    pub fn claim_special_site_ball(&self, solar_system_id: SolarSystemId, ball_id: i64) -> bool {
        self.special_site_spawned_balls.lock().unwrap_or_else(|e| e.into_inner()).insert((solar_system_id, ball_id))
    }

    /// Spawns or retrieves a running Solar System Actor for `solar_system_id`.
    /// Populates static SDE celestials, stargates, and stations (`SP-1`), re-materializes persisted
    /// space objects (`SP-7`), and hibernates (`LOD 0`) with zero timer wakes until dynamic balls exist.
    pub fn get_or_spawn(&self, solar_system_id: SolarSystemId) -> SystemHandle {
        if let Some(existing) = self
            .systems
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&solar_system_id)
        {
            return existing.clone();
        }

        let (cmd_tx, mut cmd_rx) = mpsc::channel::<SystemCommand>(SYSTEM_MAILBOX_CAPACITY);
        let (event_tx, _) = broadcast::channel::<TickBroadcast>(64);
        let event_tx_actor = event_tx.clone();
        let sde_opt = self.sde.clone();
        let db_opt = self.db.clone();
        let belt_interval_atomic = self.belt_respawn_interval_secs.clone();
        let downtime_utc_atomic = self.downtime_utc_secs_of_day.clone();
        let mechanic_rng = self.mechanic_rng.clone();
        let module_deactivation_hook = self.module_deactivation_hook.clone();
        let module_deactivation_hook_actor = module_deactivation_hook.clone();
        let mut initial_ballpark = Ballpark::new(solar_system_id);
        if let Some(sde) = &sde_opt {
            populate_system_celestials(&mut initial_ballpark, solar_system_id, sde);
        }
        let mut initial_asteroids = HashMap::new();
        let mut initial_asteroid_templates = HashMap::new();
        let initial_pending_disruptors = if let (Some(sde), Some(db)) = (&sde_opt, &db_opt) {
            let first_sde_belt_ast_id = 750_000_000i64
                + if solar_system_id == 30_000_144 {
                    0
                } else {
                    ((solar_system_id as i64 % 10_000_000).max(1)) * 1_000
                };
            let has_persisted_belt_asteroids =
                db.with_snapshot(|s| s.asteroids.contains_key(&first_sde_belt_ast_id));
            if has_persisted_belt_asteroids {
                let spawned = populate_asteroid_belt_entities(&mut initial_ballpark, solar_system_id, sde);
                let _ = reconcile_spawned_belt_asteroids(
                    &mut initial_ballpark,
                    solar_system_id,
                    spawned,
                    &mut initial_asteroids,
                    &mut initial_asteroid_templates,
                    &db_opt,
                    belt_interval_atomic.load(std::sync::atomic::Ordering::Relaxed),
                    downtime_utc_atomic.load(std::sync::atomic::Ordering::Relaxed),
                );
            }
            crate::sp6_lifecycle::populate_persisted_space_objects(
                &mut initial_ballpark,
                solar_system_id,
                sde,
                db,
            )
        } else {
            Vec::new()
        };
        let ball_positions = Arc::new(RwLock::new(HashMap::new()));
        let scrambled_balls = Arc::new(RwLock::new(std::collections::HashSet::new()));
        let active_module_ships = Arc::new(RwLock::new(std::collections::HashSet::new()));
        sync_ball_positions(&initial_ballpark, &ball_positions, &scrambled_balls);
        let ball_positions_actor = ball_positions.clone();
        let scrambled_balls_actor = scrambled_balls.clone();
        let active_module_ships_actor = active_module_ships.clone();
        let ballpark_shared = Arc::new(RwLock::new(initial_ballpark));
        let ballpark_actor = ballpark_shared.clone();
        let actor_state_shared = Arc::new(RwLock::new(SystemActorState {
            asteroids: initial_asteroids,
            asteroid_templates: initial_asteroid_templates,
            pending_anchoring_disruptors: initial_pending_disruptors,
            sde_opt,
            db_opt,
            mechanic_rng_opt: Some(mechanic_rng.clone()),
            tactical_state: self.tactical_state.clone(),
            ..Default::default()
        }));
        let actor_state_actor = actor_state_shared.clone();
        let last_auto_stepped_unix = Arc::new(std::sync::atomic::AtomicU64::new(0));

        tokio::spawn(async move {
            // parity: feat:lifecycle.world-unobserved-system-sim, plan:SP-7
            while let Some(cmd) = cmd_rx.recv().await {
                let mut ballpark = ballpark_actor.write().unwrap_or_else(|e| e.into_inner());
                let mut actor_st = actor_state_actor.write().unwrap_or_else(|e| e.into_inner());
                let SystemActorState {
                    active_module_cycles,
                    pending_anchoring_disruptors,
                    active_nanite_repairs,
                    ship_rack_heat,
                    rah_incoming_damage,
                    base_scramble_strengths,
                    base_core_strengths,
                    asteroids,
                    asteroid_templates,
                    sde_opt,
                    db_opt,
                    mechanic_rng_opt,
                    sim_elapsed_secs,
                    pending_cycle_actions,
                    tactical_state,
                } = &mut *actor_st;
                {
                    {
                        match cmd {
                            SystemCommand::SpawnBall { ball, reply } => {
                                let ball_id = ball.id;
                                let existing_meta = ballpark.ball_slim_meta.get(&ball_id).cloned();
                                if ballpark.balls.contains_key(&ball_id) {
                                    let _ = ballpark.remove_ball(ball_id);
                                }
                                if let Some(meta) = existing_meta {
                                    ballpark.set_ball_slim_meta(ball_id, meta);
                                } else {
                                    ensure_default_slim_meta(&mut ballpark, &ball, sde_opt);
                                }
                                ballpark.add_ball(ball);
                                sync_ball_positions(&ballpark, &ball_positions_actor, &scrambled_balls_actor);
                                let _ = reply.send(Ok(ballpark.current_tick));
                            }
                            SystemCommand::SpawnBallWithCombat {
                                ball,
                                combat_state,
                                reply,
                            } => {
                                let ball_id = ball.id;
                                let existing_meta = ballpark.ball_slim_meta.get(&ball_id).cloned();
                                if ballpark.balls.contains_key(&ball_id) {
                                    let _ = ballpark.remove_ball(ball_id);
                                }
                                if let Some(meta) = existing_meta {
                                    ballpark.set_ball_slim_meta(ball_id, meta);
                                } else {
                                    ensure_default_slim_meta(&mut ballpark, &ball, sde_opt);
                                }
                                base_scramble_strengths.insert(ball_id, combat_state.warp_scramble_strength);
                                base_core_strengths.insert(ball_id, combat_state.warp_core_strength);
                                let mut cs = combat_state;
                                if cs.base_signature_radius.is_none() {
                                    let base = if cs.signature_radius > 0.0 {
                                        cs.signature_radius
                                    } else {
                                        sde_opt
                                            .as_ref()
                                            .and_then(|s| s.types.get(&ball.type_id)?.default_attributes.get(&552).copied())
                                            .unwrap_or(ball.radius.max(35.0))
                                    };
                                    cs.base_signature_radius = Some(base);
                                }
                                ballpark.add_ball(ball);
                                ballpark.combat_states.insert(ball_id, cs);
                                sync_ball_positions(&ballpark, &ball_positions_actor, &scrambled_balls_actor);
                                let _ = reply.send(Ok(ballpark.current_tick));
                            }
                            SystemCommand::SpawnBallWithMeta {
                                ball,
                                combat_state,
                                slim_meta,
                                reply,
                            } => {
                                let ball_id = ball.id;
                                if ballpark.balls.contains_key(&ball_id) {
                                    let _ = ballpark.remove_ball(ball_id);
                                }
                                base_scramble_strengths.insert(ball_id, combat_state.warp_scramble_strength);
                                base_core_strengths.insert(ball_id, combat_state.warp_core_strength);
                                let mut cs = combat_state;
                                if cs.base_signature_radius.is_none() {
                                    let base = if cs.signature_radius > 0.0 {
                                        cs.signature_radius
                                    } else {
                                        sde_opt
                                            .as_ref()
                                            .and_then(|s| s.types.get(&ball.type_id)?.default_attributes.get(&552).copied())
                                            .unwrap_or(ball.radius.max(35.0))
                                    };
                                    cs.base_signature_radius = Some(base);
                                }
                                ballpark.set_ball_slim_meta(ball_id, slim_meta);
                                ballpark.add_ball(ball);
                                ballpark.combat_states.insert(ball_id, cs);
                                sync_ball_positions(&ballpark, &ball_positions_actor, &scrambled_balls_actor);
                                let _ = reply.send(Ok(ballpark.current_tick));
                            }
                            SystemCommand::ChangeSlimItem {
                                ball_id,
                                slim_meta,
                                reply,
                            } => {
                                let changed = ballpark.change_slim_item(ball_id, slim_meta);
                                let _ = reply.send(changed);
                            }
                            SystemCommand::SpawnAsteroid { asteroid, ball, reply } => {
                                let ball_id = asteroid.asteroid_id;
                                if asteroids.contains_key(&ball_id) {
                                    let _ = reply.send(Ok(ball_id));
                                    continue;
                                }
                                let (group_id, category_id) = sde_opt
                                    .as_ref()
                                    .and_then(|s| s.types.get(&ball.type_id))
                                    .map(|t| (t.group_id, t.category_id))
                                    .unwrap_or((25, 25));
                                ballpark.set_ball_slim_meta(
                                    ball_id,
                                    SlimItemMeta {
                                        group_id,
                                        category_id,
                                        owner_id: 1,
                                        char_id: None,
                                        corp_id: -1,
                                        alliance_id: -1,
                                        security_status: 0.0,
                                        is_cloaked: false,
                                    },
                                );
                                ballpark.add_ball(ball.clone());
                                if let Some(db) = &db_opt {
                                    if db.get_asteroid(ball_id).is_none() {
                                        let _ = db.spawn_asteroid(eve_db::AsteroidRecord {
                                            asteroid_id: ball_id,
                                            belt_id: asteroid.belt_id,
                                            type_id: ball.type_id,
                                            solar_system_id,
                                            position: (ball.position.x, ball.position.y, ball.position.z),
                                            quantity: i64::try_from(asteroid.remaining_units).unwrap_or(i64::MAX),
                                            initial_quantity: i64::try_from(asteroid.initial_units).unwrap_or(i64::MAX),
                                            radius: ball.radius,
                                        });
                                    }
                                }
                                asteroid_templates.insert(ball_id, (asteroid.clone(), ball));
                                asteroids.insert(ball_id, asteroid);
                                sync_ball_positions(&ballpark, &ball_positions_actor, &scrambled_balls_actor);
                                let _ = reply.send(Ok(ball_id));
                            }
                            SystemCommand::MineAsteroid {
                                asteroid_id,
                                requested_units,
                                waste_probability,
                                reply,
                            } => {
                                let res = handle_mine_asteroid(
                                    solar_system_id,
                                    asteroid_id,
                                    requested_units,
                                    waste_probability,
                                    &mut ballpark,
                                    asteroids,
                                    &mechanic_rng,
                                    &db_opt,
                                    &belt_interval_atomic,
                                    &downtime_utc_atomic,
                                );
                                sync_ball_positions(&ballpark, &ball_positions_actor, &scrambled_balls_actor);
                                let _ = reply.send(res);
                            }
                            SystemCommand::GetAsteroidState { asteroid_id, reply } => {
                                let _ = reply.send(asteroids.get(&asteroid_id).cloned());
                            }
                            SystemCommand::BreakGateCloak { ball_id, reply } => {
                                let broken = ballpark.deactivate_cloak(ball_id);
                                if let Some(mut meta) = ballpark.ball_slim_meta.get(&ball_id).cloned() {
                                    meta.is_cloaked = false;
                                    ballpark.change_slim_item(ball_id, meta);
                                }
                                let _ = reply.send(broken);
                            }
                            SystemCommand::PopulateAsteroids { reply } => {
                                let first_sde_belt_ast_id = 750_000_000i64
                                    + if solar_system_id == 30_000_144 {
                                        0
                                    } else {
                                        ((solar_system_id as i64 % 10_000_000).max(1)) * 1_000
                                    };
                                let mut populated = if !asteroid_templates.contains_key(&first_sde_belt_ast_id) {
                                    if let Some(sde) = &sde_opt {
                                        let spawned = populate_asteroid_belt_entities(&mut ballpark, solar_system_id, sde);
                                        reconcile_spawned_belt_asteroids(
                                            &mut ballpark,
                                            solar_system_id,
                                            spawned,
                                            asteroids,
                                            asteroid_templates,
                                            &db_opt,
                                            belt_interval_atomic.load(std::sync::atomic::Ordering::Relaxed),
                                            downtime_utc_atomic.load(std::sync::atomic::Ordering::Relaxed),
                                        )
                                    } else {
                                        Vec::new()
                                    }
                                } else {
                                    Vec::new()
                                };
                                for ast in asteroids.values() {
                                    if !populated.iter().any(|p| p.asteroid_id == ast.asteroid_id) {
                                        populated.push(ast.clone());
                                    }
                                }
                                populated.sort_by_key(|a| a.asteroid_id);
                                sync_ball_positions(&ballpark, &ball_positions_actor, &scrambled_balls_actor);
                                let _ = reply.send(populated);
                            }
                            SystemCommand::RemoveBall { ball_id, reply } => {
                                let deact_guard = module_deactivation_hook_actor.read().unwrap_or_else(|e| e.into_inner()).clone();
                                remove_all_ship_module_cycles(
                                    &mut ballpark,
                                    active_module_cycles,
                                    &base_scramble_strengths,
                                    &base_core_strengths,
                                    &db_opt,
                                    ball_id,
                                    deact_guard.as_ref(),
                                );
                                base_scramble_strengths.remove(&ball_id);
                                base_core_strengths.remove(&ball_id);
                                asteroids.remove(&ball_id);
                                let docking_evs: Vec<DestinyEvent> = ballpark
                                    .observer_events
                                    .get(&ball_id)
                                    .map(|evs| {
                                        evs.iter()
                                            .filter(|e| matches!(e, DestinyEvent::OnDockingAccepted { .. }))
                                            .cloned()
                                            .collect()
                                    })
                                    .unwrap_or_default();
                                let removed = ballpark.remove_ball(ball_id);
                                if !docking_evs.is_empty() {
                                    ballpark.observer_events.insert(ball_id, docking_evs);
                                }
                                sync_ball_positions(&ballpark, &ball_positions_actor, &scrambled_balls_actor);
                                let _ = reply.send(removed);
                            }
                            SystemCommand::AcceptDocking {
                                ship_id,
                                station_id,
                                station_pos,
                                reply,
                            } => {
                                let deact_guard = module_deactivation_hook_actor.read().unwrap_or_else(|e| e.into_inner()).clone();
                                remove_all_ship_module_cycles(
                                    &mut ballpark,
                                    active_module_cycles,
                                    &base_scramble_strengths,
                                    &base_core_strengths,
                                    &db_opt,
                                    ship_id,
                                    deact_guard.as_ref(),
                                );
                                base_scramble_strengths.remove(&ship_id);
                                base_core_strengths.remove(&ship_id);
                                asteroids.remove(&ship_id);
                                let removed = if ballpark.balls.contains_key(&station_id) {
                                    ballpark.accept_docking(ship_id, station_id)
                                } else {
                                    ballpark.accept_docking_at(ship_id, station_id, station_pos)
                                };
                                sync_ball_positions(&ballpark, &ball_positions_actor, &scrambled_balls_actor);
                                let _ = reply.send(removed);
                            }
                            SystemCommand::EmitPackagedActions {
                                ball_id,
                                events,
                                reply,
                            } => {
                                let known = ballpark.balls.contains_key(&ball_id);
                                if known {
                                    ballpark.emit_packaged_actions(Some(ball_id), &events);
                                }
                                let _ = reply.send(known);
                            }
                            SystemCommand::SetMovementMode { ball_id, mode, reply } => {
                                if ballpark.balls.contains_key(&ball_id) {
                                    match ballpark.set_mode(ball_id, mode) {
                                        Ok(()) => {
                                            let _ = reply.send(Ok(ballpark.current_tick));
                                        }
                                        Err(e) => {
                                            let _ = reply.send(Err(e));
                                        }
                                    }
                                } else {
                                    let _ = reply.send(Err(format!("Ball {ball_id} not in system {solar_system_id}")));
                                }
                            }
                            SystemCommand::SetWarpScrambleStrength { ball_id, strength, reply } => {
                                if ballpark.set_warp_scramble_strength(ball_id, strength) {
                                    base_scramble_strengths.insert(ball_id, strength);
                                    reconcile_tackle_and_bubbles(
                                        &mut ballpark,
                                        active_module_cycles,
                                        &base_scramble_strengths,
                                        &base_core_strengths,
                                    );
                                    sync_ball_positions(&ballpark, &ball_positions_actor, &scrambled_balls_actor);
                                    let _ = reply.send(Ok(ballpark.current_tick));
                                } else {
                                    let _ = reply.send(Err(format!("Ball {ball_id} not in system or has no combat state")));
                                }
                            }
                            SystemCommand::SetWarpCoreStrength { ball_id, strength, reply } => {
                                if ballpark.set_warp_core_strength(ball_id, strength) {
                                    base_core_strengths.insert(ball_id, strength);
                                    reconcile_tackle_and_bubbles(
                                        &mut ballpark,
                                        active_module_cycles,
                                        &base_scramble_strengths,
                                        &base_core_strengths,
                                    );
                                    sync_ball_positions(&ballpark, &ball_positions_actor, &scrambled_balls_actor);
                                    let _ = reply.send(Ok(ballpark.current_tick));
                                } else {
                                    let _ = reply.send(Err(format!("Ball {ball_id} not in system or has no combat state")));
                                }
                            }
                            SystemCommand::AddWarpBubble { id, position, radius, reply } => {
                                ballpark.add_warp_bubble(id, position, radius);
                                let _ = reply.send(Ok(ballpark.current_tick));
                            }
                            SystemCommand::RemoveWarpBubble { id, reply } => {
                                let removed = ballpark.remove_warp_bubble(id).is_some();
                                let _ = reply.send(removed);
                            }
                            SystemCommand::SetSpeedFraction {
                                ball_id,
                                fraction,
                                reply,
                            } => {
                                if ballpark.set_speed_fraction(ball_id, fraction) {
                                    let _ = reply.send(Ok(ballpark.current_tick));
                                } else {
                                    let _ = reply.send(Err(format!("Ball {ball_id} not in system {solar_system_id}")));
                                }
                            }
                            SystemCommand::SetBallPosition {
                                ball_id,
                                position,
                                reply,
                            } => {
                                let updated = ballpark.set_ball_position(ball_id, position);
                                sync_ball_positions(&ballpark, &ball_positions_actor, &scrambled_balls_actor);
                                let _ = reply.send(updated);
                            }
                            SystemCommand::EmitDestinyEvent { ball_id, event, reply } => {
                                let known = ballpark.balls.contains_key(&ball_id)
                                    || matches!(event, DestinyEvent::OnSovereigntyChanged { .. });
                                if known {
                                    let scope = ballpark.balls.contains_key(&ball_id).then_some(ball_id);
                                    match event {
                                        DestinyEvent::SetBallFree { ball_id: id, is_free } if id == ball_id => {
                                            ballpark.set_ball_free(id, is_free);
                                        }
                                        DestinyEvent::SetBallHarmonic { ball_id: id, harmonic } if id == ball_id => {
                                            ballpark.set_ball_harmonic(id, harmonic);
                                        }
                                        DestinyEvent::SetBallInteractive { ball_id: id, is_interactive } if id == ball_id => {
                                            ballpark.set_ball_interactive(id, is_interactive);
                                        }
                                        DestinyEvent::SetBallRadius { ball_id: id, radius } if id == ball_id => {
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
                                            ballpark.emit_client_controller_event(
                                                id,
                                                target_id,
                                                event_name,
                                                value,
                                                delay,
                                            );
                                        }
                                        DestinyEvent::OnShipStateUpdate { module_states } => {
                                            ballpark.update_ship_module_states(ball_id, module_states);
                                        }
                                        DestinyEvent::OnDbuffUpdated { ball_id: id, dbuffs } if id == ball_id => {
                                            ballpark.update_dbuffs(id, dbuffs);
                                        }
                                        DestinyEvent::OnDotVictimUpdated {
                                            ball_id: id,
                                            flat_dps,
                                            pct_dps,
                                            remaining_ticks,
                                        } if id == ball_id => {
                                            ballpark.apply_breacher_dot(id, flat_dps, pct_dps, remaining_ticks);
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
                                let _ = reply.send(known);
                            }
                            SystemCommand::GotoPoint { ball_id, point, reply } => {
                                let res = ballpark
                                    .goto_point(ball_id, point)
                                    .map(|()| ballpark.current_tick);
                                let _ = reply.send(res);
                            }
                            SystemCommand::PushSetState {
                                observer_ball_id,
                                reply,
                            } => {
                                if ballpark.queue_set_state(observer_ball_id) {
                                    let _ = reply.send(Ok(ballpark.current_tick));
                                } else {
                                    let _ = reply.send(Err(format!(
                                        "Observer ball {observer_ball_id} not in system {solar_system_id}"
                                    )));
                                }
                            }
                            SystemCommand::GetBallSnapshot { ball_id, reply } => {
                                let res = ballpark.balls.get(&ball_id).cloned().map(|b| (ballpark.current_tick, b));
                                let _ = reply.send(res);
                            }
                            SystemCommand::GetBallGroupId { ball_id, reply } => {
                                let _ = reply.send(ballpark.ball_group_id(ball_id));
                            }
                            SystemCommand::ListBalls { reply } => {
                                let balls: Vec<Ball> = ballpark.balls.values().cloned().collect();
                                let _ = reply.send(balls);
                            }
                            SystemCommand::ListBallsInBubble {
                                observer_ball_id,
                                dynamic_only,
                                reply,
                            } => {
                                let balls: Vec<Ball> = if dynamic_only {
                                    ballpark
                                        .dynamic_balls_in_bubble(observer_ball_id)
                                        .into_iter()
                                        .cloned()
                                        .collect()
                                } else {
                                    ballpark
                                        .visible_balls_for_observer(observer_ball_id)
                                        .into_iter()
                                        .cloned()
                                        .collect()
                                };
                                let _ = reply.send(balls);
                            }
                            SystemCommand::GetSetState {
                                observer_ball_id,
                                reply,
                            } => {
                                if ballpark.balls.contains_key(&observer_ball_id) {
                                    let payload = encode_set_state_payload(&ballpark, observer_ball_id);
                                    let update_list = PyRep::List(vec![encode_set_state_update(
                                        &ballpark,
                                        observer_ball_id,
                                    )]);
                                    let _ = reply.send(Ok((ballpark.current_tick, payload, update_list)));
                                } else {
                                    let _ = reply.send(Err(format!(
                                        "Observer ball {observer_ball_id} not in system {solar_system_id}"
                                    )));
                                }
                            }
                            SystemCommand::GetBubbleIdForBall { ball_id, reply } => {
                                let bid = ballpark.bubble_id_for_ball(ball_id);
                                let _ = reply.send(bid);
                            }
                            SystemCommand::DrainObserverUpdates {
                                observer_ball_id,
                                reply,
                            } => {
                                sync_player_slim_items(&mut ballpark, &db_opt);
                                let events = ballpark.drain_observer_events(observer_ball_id);
                                let stamp = ballpark.current_tick as u32;
                                let pyrep = encode_destiny_updates(stamp, &events, Some(&ballpark));
                                let _ = reply.send((ballpark.current_tick, events, pyrep));
                            }
                            SystemCommand::ApplyDamage { ball_id, damage, reply } => {
                                let is_invuln_stronghold = db_opt
                                    .as_ref()
                                    .map(|db| crate::services::insurgency_services::is_stronghold_invulnerable_in_db(db, solar_system_id, ball_id))
                                    .unwrap_or(false);
                                if is_invuln_stronghold {
                                    if let Some(b) = ballpark.balls.get(&ball_id).cloned() {
                                        let _ = reply.send(Some((0.0, false, b)));
                                    } else {
                                        let _ = reply.send(None);
                                    }
                                } else if let Some((applied, destroyed, ball_opt)) = ballpark.apply_damage_to_ball(ball_id, damage) {
                                    if applied > 0.0 || damage.total() > 0.0 {
                                        let entry = rah_incoming_damage.entry(ball_id).or_insert([0.0; 4]);
                                        entry[0] += damage.em;
                                        entry[1] += damage.thermal;
                                        entry[2] += damage.kinetic;
                                        entry[3] += damage.explosive;
                                    }
                                    if let (Some(db), Some(cs)) = (&db_opt, ballpark.combat_states.get(&ball_id)) {
                                        crate::sp6_lifecycle::persist_ship_combat_hp_in_db(db, ball_id, cs);
                                    }
                                    sync_ball_positions(&ballpark, &ball_positions_actor, &scrambled_balls_actor);
                                    if let Some(ball_snap) = ball_opt {
                                        let _ = reply.send(Some((applied, destroyed, ball_snap)));
                                    } else {
                                        let _ = reply.send(None);
                                    }
                                } else {
                                    let _ = reply.send(None);
                                }
                            }
                            SystemCommand::StepManual { dt, reply } => {
                                if dt > 0.0 && dt.is_finite() {
                                    *sim_elapsed_secs += dt;
                                }
                                respawn_due_asteroids(
                                    &mut ballpark,
                                    asteroids,
                                    &asteroid_templates,
                                    &sde_opt,
                                    &db_opt,
                                );
                                sync_player_slim_items(&mut ballpark, &db_opt);
                                let events = ballpark.step(dt);
                                let deact_guard = module_deactivation_hook_actor.read().unwrap_or_else(|e| e.into_inner()).clone();
                                let destroyed_balls = step_cycles_and_capacitor(
                                    &mut ballpark, active_module_cycles, pending_anchoring_disruptors,
                                    active_nanite_repairs, ship_rack_heat, rah_incoming_damage,
                                    &base_scramble_strengths, &base_core_strengths, &sde_opt, &db_opt,
                                    tactical_state, pending_cycle_actions, dt, deact_guard.as_ref(),
                                );
                                if let Some(db) = &db_opt {
                                    for (&bid, cs) in &ballpark.combat_states {
                                        if !ballpark.is_static_ball(bid) && bid < 700_000_000 {
                                            crate::sp6_lifecycle::persist_ship_combat_hp_in_db(db, bid, cs);
                                        }
                                    }
                                }
                                sync_ball_positions(&ballpark, &ball_positions_actor, &scrambled_balls_actor);
                                let broadcast = build_tick_broadcast(solar_system_id, &mut ballpark, events);
                                let _ = event_tx_actor.send(broadcast.clone());
                                let _ = reply.send((broadcast, destroyed_balls));
                            }
                            SystemCommand::GetTargetLocks { ball_id, reply } => {
                                let locks = ballpark.get_target_locks(ball_id);
                                let _ = reply.send(locks);
                            }
                            SystemCommand::AddTargetLock { source_id, lock, reply } => {
                                let res = if ballpark.add_target_lock(source_id, lock) {
                                    Ok(())
                                } else {
                                    Err("Targeting systems cannot acquire lock while cloaked or during sensor recalibration delay, or target is untargetable".to_string())
                                };
                                let _ = reply.send(res);
                            }
                            SystemCommand::RemoveTargetLock { source_id, target_id, reply } => {
                                let removed = ballpark.remove_target_lock(source_id, target_id);
                                let _ = reply.send(removed);
                            }
                            SystemCommand::ClearTargetLocks { source_id, reply } => {
                                let count = ballpark.clear_target_locks(source_id);
                                let _ = reply.send(count);
                            }
                            SystemCommand::SetCapacitor { ball_id, capacitor, reply } => {
                                let cs = ballpark.combat_states.entry(ball_id).or_default();
                                cs.capacitor = capacitor;
                                let _ = reply.send(Ok(()));
                            }
                            SystemCommand::HealCombatState { ball_id, reply } => {
                                let cs = ballpark.combat_states.entry(ball_id).or_default();
                                cs.shield_hp = cs.max_shield_hp;
                                cs.armor_hp = cs.max_armor_hp;
                                cs.structure_hp = cs.max_structure_hp;
                                cs.capacitor = cs.max_capacitor;
                                let cloned = cs.clone();
                                if let Some(db) = &db_opt {
                                    crate::sp6_lifecycle::persist_ship_combat_hp_in_db(db, ball_id, &cloned);
                                }
                                let _ = reply.send(Ok(cloned));
                            }
                            SystemCommand::SetCombatState { ball_id, combat_state, reply } => {
                                base_scramble_strengths.insert(ball_id, combat_state.warp_scramble_strength);
                                base_core_strengths.insert(ball_id, combat_state.warp_core_strength);
                                if let Some(db) = &db_opt {
                                    crate::sp6_lifecycle::persist_ship_combat_hp_in_db(db, ball_id, &combat_state);
                                }
                                ballpark.combat_states.insert(ball_id, combat_state);
                                reconcile_tackle_and_bubbles(
                                    &mut ballpark,
                                    active_module_cycles,
                                    &base_scramble_strengths,
                                    &base_core_strengths,
                                );
                                sync_ball_positions(&ballpark, &ball_positions_actor, &scrambled_balls_actor);
                                let _ = reply.send(Ok(()));
                            }
                            SystemCommand::GetCombatState { ball_id, reply } => {
                                let cs = ballpark.combat_states.get(&ball_id).cloned();
                                let _ = reply.send(cs);
                            }
                            SystemCommand::AddModuleCycle { cycle, reply } => {
                                let res = handle_add_module_cycle(
                                    &mut ballpark,
                                    active_module_cycles,
                                    &base_scramble_strengths,
                                    &base_core_strengths,
                                    &sde_opt,
                                    &db_opt,
                                    cycle,
                                );
                                sync_ball_positions(&ballpark, &ball_positions_actor, &scrambled_balls_actor);
                                let _ = reply.send(res);
                            }
                            SystemCommand::RemoveModuleCycle { ship_id, module_id, reply } => {
                                let mut remaining_cooldown = None;
                                if let Some(pos) = active_module_cycles.iter().position(|c| c.ship_id == ship_id && c.module_id == module_id) {
                                    let removed = active_module_cycles.remove(pos);
                                    let rem_cycle = (removed.duration_s - removed.elapsed_s).max(0.0);
                                    remaining_cooldown = Some(rem_cycle + removed.reactivation_delay_s);
                                    cleanup_removed_module_cycle(&mut ballpark, &removed, &db_opt);
                                    reconcile_tackle_and_bubbles(
                                        &mut ballpark,
                                        active_module_cycles,
                                        &base_scramble_strengths,
                                        &base_core_strengths,
                                    );
                                }
                                sync_ball_positions(&ballpark, &ball_positions_actor, &scrambled_balls_actor);
                                {
                                    let mut mod_guard = active_module_ships_actor.write().unwrap_or_else(|e| e.into_inner());
                                    mod_guard.clear();
                                    for c in active_module_cycles.iter() {
                                        mod_guard.insert(c.ship_id);
                                    }
                                }
                                let _ = reply.send(Ok(remaining_cooldown));
                            }
                            SystemCommand::HasModuleCycle { ship_id, module_id, reply } => {
                                let exists = active_module_cycles
                                    .iter()
                                    .any(|c| c.ship_id == ship_id && c.module_id == module_id);
                                let _ = reply.send(exists);
                            }
                            SystemCommand::UpdateModuleOverload { ship_id, module_id, is_overloaded, reply } => {
                                let mut updated = false;
                                for c in active_module_cycles.iter_mut() {
                                    if c.ship_id == ship_id && c.module_id == module_id {
                                        c.is_overloaded = is_overloaded;
                                        updated = true;
                                    }
                                }
                                let _ = reply.send(updated);
                            }
                            SystemCommand::StartNaniteRepair { repair, reply } => {
                                active_nanite_repairs.retain(|r| !(r.ship_id == repair.ship_id && r.module_id == repair.module_id));
                                active_nanite_repairs.push(repair);
                                let _ = reply.send(());
                            }
                            SystemCommand::StopNaniteRepair { ship_id, module_id, reply } => {
                                let mut rem = None;
                                if let Some(pos) = active_nanite_repairs.iter().position(|r| r.ship_id == ship_id && r.module_id == module_id) {
                                    let r = active_nanite_repairs.remove(pos);
                                    let frac = (r.elapsed_s / r.duration_s.max(0.001)).clamp(0.0, 1.0);
                                    rem = Some(r.initial_damage_hp * (1.0 - frac));
                                }
                                let _ = reply.send(rem);
                            }
                            SystemCommand::RecordIncomingArmorDamage { ship_id, damage, reply } => {
                                let entry = rah_incoming_damage.entry(ship_id).or_insert([0.0; 4]);
                                for (dst, src) in entry.iter_mut().zip(damage.iter()) {
                                    *dst += *src;
                                }
                                let _ = reply.send(());
                            }
                            SystemCommand::StopNonWarpModuleCycles { ship_id, reply } => {
                                let deact_guard = module_deactivation_hook_actor.read().unwrap_or_else(|e| e.into_inner()).clone();
                                remove_non_warp_ship_module_cycles(
                                    &mut ballpark,
                                    active_module_cycles,
                                    &base_scramble_strengths,
                                    &base_core_strengths,
                                    &db_opt,
                                    ship_id,
                                    deact_guard.as_ref(),
                                );
                                sync_ball_positions(&ballpark, &ball_positions_actor, &scrambled_balls_actor);
                                {
                                    let mut mod_guard = active_module_ships_actor.write().unwrap_or_else(|e| e.into_inner());
                                    mod_guard.clear();
                                    for c in active_module_cycles.iter() {
                                        mod_guard.insert(c.ship_id);
                                    }
                                }
                                let _ = reply.send(());
                            }
                            SystemCommand::SetInterdictionNullified { ball_id, nullified, reply } => {
                                let _ = reply.send(ballpark.set_interdiction_nullified(ball_id, nullified));
                            }
                            SystemCommand::MicroJump { ball_id, distance_meters, reply } => {
                                let res = ballpark.micro_jump(ball_id, distance_meters);
                                sync_ball_positions(&ballpark, &ball_positions_actor, &scrambled_balls_actor);
                                let _ = reply.send(res);
                            }
                            SystemCommand::MicroJumpField { ball_id, distance_meters, field_radius_meters, max_jumped_ships, reply } => {
                                let res = ballpark.micro_jump_field(ball_id, distance_meters, field_radius_meters, max_jumped_ships);
                                sync_ball_positions(&ballpark, &ball_positions_actor, &scrambled_balls_actor);
                                let _ = reply.send(res);
                            }
                            SystemCommand::IsBallScrambled { ball_id, reply } => {
                                let _ = reply.send(ballpark.is_ball_scrambled(ball_id));
                            }
                            SystemCommand::AddInterdictionSphere { id, position, radius, reply } => {
                                let ok = ballpark.add_interdiction_sphere(id, position, radius);
                                reconcile_tackle_and_bubbles(&mut ballpark, active_module_cycles, &base_scramble_strengths, &base_core_strengths);
                                sync_ball_positions(&ballpark, &ball_positions_actor, &scrambled_balls_actor);
                                let _ = reply.send(ok);
                            }
                            SystemCommand::AddMobileWarpDisruptor { id, position, radius, reply } => {
                                let ok = ballpark.add_mobile_warp_disruptor(id, position, radius);
                                reconcile_tackle_and_bubbles(&mut ballpark, active_module_cycles, &base_scramble_strengths, &base_core_strengths);
                                sync_ball_positions(&ballpark, &ball_positions_actor, &scrambled_balls_actor);
                                let _ = reply.send(ok);
                            }
                            SystemCommand::ScheduleMobileWarpDisruptor { id, type_id, position, radius, anchoring_delay_s, reply } => {
                                let ok = handle_schedule_mobile_warp_disruptor(&mut ballpark, pending_anchoring_disruptors, id, type_id, position, radius, anchoring_delay_s);
                                sync_ball_positions(&ballpark, &ball_positions_actor, &scrambled_balls_actor);
                                let _ = reply.send(ok);
                            }
                            SystemCommand::ApplyEcmJam { target_id, duration_s, reply } => {
                                let target_ewar_immune = active_module_cycles.iter().any(|c| c.ship_id == target_id && c.self_ewar_immune);
                                let exists = ballpark.combat_states.contains_key(&target_id) && !target_ewar_immune;
                                if exists {
                                    ballpark.apply_ecm_jam(target_id, duration_s);
                                }
                                let _ = reply.send(exists);
                            }
                            SystemCommand::ApplyEcmBurst { source_id, burst_radius_m, jam_strengths, duration_s, roll, ship_eval_overrides, reply } => {
                                let jammed = handle_apply_ecm_burst(&mut ballpark, &active_module_cycles, &sde_opt, &mechanic_rng_opt, source_id, burst_radius_m, jam_strengths, duration_s, roll, ship_eval_overrides);
                                let _ = reply.send(jammed);
                            }
                        }
                        {
                            let mut mod_guard = active_module_ships_actor.write().unwrap_or_else(|e| e.into_inner());
                            mod_guard.clear();
                            for c in active_module_cycles.iter() {
                                mod_guard.insert(c.ship_id);
                            }
                        }
                    }
                }
            }
        });

        let handle = SystemHandle {
            solar_system_id,
            cmd_tx,
            event_tx,
            ball_positions,
            scrambled_balls,
            active_module_ships,
            ballpark: ballpark_shared,
            actor_state: actor_state_shared,
            last_auto_stepped_unix,
            npc_death_hook: Arc::clone(&self.npc_death_hook),
            module_deactivation_hook: Arc::clone(&self.module_deactivation_hook),
            system_step_hook: Arc::clone(&self.system_step_hook),
        };
        let mut guard = self.systems.write().unwrap_or_else(|e| e.into_inner());
        if let Some(existing) = guard.get(&solar_system_id) {
            return existing.clone();
        }
        guard.insert(solar_system_id, handle.clone());
        handle
    }
}
