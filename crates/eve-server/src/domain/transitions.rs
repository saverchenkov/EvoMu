//! Protocol-agnostic solar-system space relocation domain operations (`#1193`, `#1542`).
//!
//! Shared across GM relocation commands (`/tr`, `/teleport`, `/translocate`, `/moveme`) and
//! live gameplay cross-system jumps (Starter Shipcaster, Wormhole jumps) so departure cleanup,
//! `#1542` non-waking origin lookups (`get_handle`), DB transition with `restore_ball` rollback,
//! arrival ball spawning, combat-state preservation, gate-cloak/invulnerability policy, and
//! `execute_session_transition_sync` converge on one canonical path.

#![forbid(unsafe_code)]

use crate::domain::DomainError;
use crate::router::session_transition::{
    record_session_change_timer, SessionTransitionSpec, TransitionKind,
};
use crate::router::ServerContext;
use crate::system_actor::{SessionArrivalWakeReason, SystemHandle};
use eve_db::CharacterRecord;
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_machonet::SessionState;
use glam::DVec3;

const DEFAULT_SHIP_MAX_VELOCITY: f64 = 300.0;
const DEFAULT_SHIP_INERTIA: f64 = 0.5;
const DEFAULT_SHIP_MASS: f64 = 1_000_000.0;
const DEFAULT_SHIP_RADIUS: f64 = 50.0;

/// Policy governing how the destination solar system actor is woken after validation (`#1542`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DestinationWakePolicy {
    /// Wake as a live gameplay session arrival (e.g. jump, shipcaster, wormhole).
    SessionArrival(SessionArrivalWakeReason),
    /// Wake on behalf of an explicit GM/QA slash command.
    GmCommand(String),
}

/// Session-level transition variant passed to `execute_session_transition_sync`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpaceTransitionKind {
    /// GM relocation (`/tr`, `/teleport`, `/translocate`, `/moveme`).
    Teleport,
    /// Gameplay space jump (`shipcaster`, `wormhole_jump`, etc.).
    JumpSystem,
}

/// Policy determining how destination ship hull/armor/shield/capacitor state is initialized.
#[derive(Debug, Clone)]
pub enum CombatStatePreservation {
    /// Build a fresh combat state from persisted DB/SDE attributes.
    Fresh,
    /// Preserve ratios via `apply_preserved_combat_state` (used by wormhole jumps).
    PreserveNormalized { fallback: Option<CombatState> },
    /// Preserve exact shield/armor/hull/capacitor values clamped to destination maxes (shipcaster).
    PreserveClamped { fallback: Option<CombatState> },
}

impl CombatStatePreservation {
    fn fallback_combat_state(&self) -> Option<&CombatState> {
        match self {
            Self::Fresh => None,
            Self::PreserveNormalized { fallback } | Self::PreserveClamped { fallback } => {
                fallback.as_ref()
            }
        }
    }
}

/// Arrival gate-cloak and post-jump invulnerability policy applied to the spawned ball.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GateCloakInvulnPolicy {
    pub gate_cloak_ticks: u32,
    pub undock_invuln_ticks: u32,
    pub sync_invuln_with_undock_timer: bool,
    pub slim_meta_cloaked: bool,
}

impl GateCloakInvulnPolicy {
    /// No arrival gate cloak or jump invulnerability (GM `/tr`, `/moveme`).
    pub fn none() -> Self {
        Self {
            gate_cloak_ticks: 0,
            undock_invuln_ticks: 0,
            sync_invuln_with_undock_timer: false,
            slim_meta_cloaked: false,
        }
    }

    /// Standard 60-tick post-jump gate cloak and invulnerability timer.
    pub fn standard_jump_cloak() -> Self {
        Self {
            gate_cloak_ticks: 60,
            undock_invuln_ticks: 60,
            sync_invuln_with_undock_timer: true,
            slim_meta_cloaked: true,
        }
    }
}

/// Controls whether `leave_space_in_snap` is recorded on departure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaveSpaceSnapMode {
    Never,
    OnlyIfTakenBall,
    Always,
}

/// Cleanup flags executed when leaving the departing space scene.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeaveSpaceCleanupPolicy {
    pub abandon_drones: bool,
    pub abort_self_destruct: bool,
    pub deactivate_modules: bool,
    pub leave_space_in_snap: LeaveSpaceSnapMode,
    pub stop_and_flush_warp_on_cross_system: bool,
}

impl LeaveSpaceCleanupPolicy {
    /// Cleanup policy for GM `/tr` / `/moveme` relocations (`#1193`, `#1542`).
    pub fn gm_teleport() -> Self {
        Self {
            abandon_drones: true,
            abort_self_destruct: true,
            deactivate_modules: true,
            leave_space_in_snap: LeaveSpaceSnapMode::OnlyIfTakenBall,
            stop_and_flush_warp_on_cross_system: true,
        }
    }

    /// Full gameplay cross-system space departure cleanup policy.
    pub fn gameplay_jump() -> Self {
        Self {
            abandon_drones: true,
            abort_self_destruct: true,
            deactivate_modules: true,
            leave_space_in_snap: LeaveSpaceSnapMode::Always,
            stop_and_flush_warp_on_cross_system: false,
        }
    }
}

/// Parameters for [`relocate_character_in_space`].
#[derive(Debug, Clone)]
pub struct RelocateInSpaceRequest {
    pub char_id: i64,
    pub ship_id: i64,
    pub old_sys_id: u32,
    pub old_dock: Option<(i64, bool)>,
    pub dest_sys_id: u32,
    pub was_in_space: bool,
    /// Explicit destination position; when `None`, cross-system or undock arrival spawns use
    /// `fallback_arrival_position`, while same-system in-space keeps the current ball position.
    pub arrival_position: Option<DVec3>,
    pub fallback_arrival_position: DVec3,
    pub arrival_velocity: DVec3,
    pub require_take_ball: bool,
    pub take_ball_missing_code: &'static str,
    pub db_transition_failed_code: &'static str,
    pub destination_wake: DestinationWakePolicy,
    pub transition_kind: SpaceTransitionKind,
    pub transition_reason: &'static str,
    pub combat_state_preservation: CombatStatePreservation,
    pub gate_cloak_invuln: GateCloakInvulnPolicy,
    pub cleanup: LeaveSpaceCleanupPolicy,
    pub sync_local_chat: bool,
    pub persist_item_position: bool,
    pub record_session_change_timer: bool,
    pub record_solar_system_jump: bool,
    pub bind_beyonce: bool,
    pub push_set_state: bool,
}

/// Outcome returned by [`relocate_character_in_space`].
#[derive(Clone)]
pub struct RelocateInSpaceOutcome {
    pub updated_ch: CharacterRecord,
    pub dest_handle: SystemHandle,
    pub dest_tick: u64,
    pub ship_type_id: u32,
    pub same_system_in_space: bool,
    pub stopped_mid_warp: bool,
    pub arrival_position: Option<DVec3>,
}

impl std::fmt::Debug for RelocateInSpaceOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RelocateInSpaceOutcome")
            .field("character_id", &self.updated_ch.character_id)
            .field("solar_system_id", &self.updated_ch.solar_system_id)
            .field("dest_tick", &self.dest_tick)
            .field("ship_type_id", &self.ship_type_id)
            .field("same_system_in_space", &self.same_system_in_space)
            .field("stopped_mid_warp", &self.stopped_mid_warp)
            .field("arrival_position", &self.arrival_position)
            .finish()
    }
}

async fn wake_destination_handle(
    ctx: &ServerContext,
    dest_sys_id: u32,
    policy: &DestinationWakePolicy,
) -> Result<SystemHandle, DomainError> {
    let res = match policy {
        DestinationWakePolicy::SessionArrival(reason) => {
            ctx.systems
                .wake_for_session_arrival_async(dest_sys_id, *reason)
                .await
        }
        DestinationWakePolicy::GmCommand(cmd) => {
            ctx.systems
                .wake_for_gm_command_async(dest_sys_id, cmd.as_str())
                .await
        }
    };
    res.map_err(|e| {
        DomainError::internal(
            "SystemTransitionFailed",
            format!("Failed to wake solar system {dest_sys_id}: {e}"),
        )
    })
}

async fn rollback_failed_destination(
    ctx: &ServerContext,
    req: &RelocateInSpaceRequest,
    old_handle: Option<&SystemHandle>,
    taken: Option<crate::system_actor::TakenBall>,
) {
    if let Some((dock_id, is_structure)) = req.old_dock {
        let _ = ctx.db.dock_character(req.char_id, dock_id, is_structure);
    } else if req.old_sys_id > 0 {
        let _ = ctx
            .db
            .set_character_solar_system(req.char_id, req.old_sys_id, &ctx.sde);
    }
    if let (Some(oh), Some(ball)) = (old_handle, taken) {
        let _ = oh.restore_ball(ball).await;
    }
}

async fn detect_and_stop_warp(handle: &SystemHandle, ship_id: i64, flush_peers: bool) -> bool {
    let tracked_warp = handle.is_active_warp(ship_id) || handle.is_return_warp(ship_id);
    let ball_warping = handle
        .get_ball_snapshot(ship_id)
        .await
        .ok()
        .flatten()
        .is_some_and(|(_, ball)| matches!(ball.mode, MovementMode::Warp { .. }));
    if !tracked_warp && !ball_warping {
        return false;
    }
    let _ = handle.force_stop_warp(ship_id);
    crate::services::space_destiny::clear_warp_tracking(handle, ship_id);
    if flush_peers {
        let _ = handle.flush_peer_observer_updates(None).await;
    }
    true
}

fn lookup_ship_type_id(ctx: &ServerContext, ship_id: i64) -> u32 {
    ctx.db
        .with_snapshot(|snap| snap.items.get(&ship_id).map(|i| i.type_id))
        .unwrap_or(587)
}

fn apply_arrival_combat_and_cloak_policy(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    taken_cs: Option<CombatState>,
    preservation: &CombatStatePreservation,
    cloak_policy: &GateCloakInvulnPolicy,
) -> (CombatState, eve_destiny::SlimItemMeta) {
    let mut combat_state = ctx.build_ship_combat_state(char_id, ship_id, ship_type_id);
    match preservation {
        CombatStatePreservation::Fresh => {}
        CombatStatePreservation::PreserveNormalized { fallback } => {
            crate::services::r6_t1_p1_helpers::apply_preserved_combat_state(
                &mut combat_state,
                taken_cs.or_else(|| fallback.clone()),
            );
        }
        CombatStatePreservation::PreserveClamped { fallback } => {
            if let Some(prev) = taken_cs.or_else(|| fallback.clone()) {
                if prev.shield_hp.is_finite() {
                    combat_state.shield_hp =
                        prev.shield_hp.clamp(0.0, combat_state.max_shield_hp.max(0.0));
                }
                if prev.armor_hp.is_finite() {
                    combat_state.armor_hp =
                        prev.armor_hp.clamp(0.0, combat_state.max_armor_hp.max(0.0));
                }
                if prev.structure_hp.is_finite() && prev.structure_hp > 0.0 {
                    combat_state.structure_hp =
                        prev.structure_hp.clamp(1e-6, combat_state.max_structure_hp.max(1e-6));
                }
                if prev.capacitor.is_finite() {
                    combat_state.capacitor =
                        prev.capacitor.clamp(0.0, combat_state.max_capacitor.max(0.0));
                }
            }
        }
    }

    if cloak_policy.gate_cloak_ticks > 0 {
        combat_state.is_cloaked = true;
        combat_state.gate_cloak_remaining_ticks = cloak_policy.gate_cloak_ticks;
    }
    if cloak_policy.undock_invuln_ticks > 0 {
        combat_state.undock_invulnerable_remaining_ticks = cloak_policy.undock_invuln_ticks;
        combat_state.sync_invuln_with_undock_timer = cloak_policy.sync_invuln_with_undock_timer;
    }

    let mut slim_meta = ctx.build_ship_slim_meta(char_id, ship_type_id);
    if cloak_policy.slim_meta_cloaked {
        slim_meta.is_cloaked = true;
    }
    (combat_state, slim_meta)
}

async fn spawn_arrival_ship_ball(
    ctx: &ServerContext,
    dest_handle: &SystemHandle,
    req: &RelocateInSpaceRequest,
    ship_id: i64,
    arrival_pos: DVec3,
    taken: Option<&crate::system_actor::TakenBall>,
) -> Result<(u64, u32), DomainError> {
    if ship_id <= 0 {
        return Ok((0, 0));
    }
    let (mut new_ball, ship_type_id, taken_cs) = if let Some(t) = taken {
        let tid = t.ball.type_id;
        (t.ball.clone(), tid, t.combat_state.clone())
    } else {
        let tid = lookup_ship_type_id(ctx, ship_id);
        let ball = Ball {
            id: ship_id,
            type_id: tid,
            position: arrival_pos,
            velocity: req.arrival_velocity,
            max_velocity: DEFAULT_SHIP_MAX_VELOCITY,
            inertia: DEFAULT_SHIP_INERTIA,
            mass: DEFAULT_SHIP_MASS,
            radius: DEFAULT_SHIP_RADIUS,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        };
        (ball, tid, None)
    };
    new_ball.position = arrival_pos;
    new_ball.velocity = req.arrival_velocity;
    new_ball.speed_fraction = 0.0;
    new_ball.mode = MovementMode::Stop;

    let (combat_state, slim_meta) = apply_arrival_combat_and_cloak_policy(
        ctx,
        req.char_id,
        ship_id,
        ship_type_id,
        taken_cs,
        &req.combat_state_preservation,
        &req.gate_cloak_invuln,
    );
    let dest_tick = dest_handle
        .spawn_ball_with_meta(new_ball, combat_state, slim_meta)
        .await
        .map_err(|msg| {
            DomainError::internal(
                req.db_transition_failed_code,
                format!(
                    "Failed to spawn ship ball {ship_id} in solar system {}: {msg}",
                    req.dest_sys_id
                ),
            )
        })?;
    Ok((dest_tick, ship_type_id))
}

fn finish_session_and_post_hooks(
    ctx: &ServerContext,
    mut session: Option<&mut SessionState>,
    req: &RelocateInSpaceRequest,
    dest_tick: u64,
) {
    if req.record_session_change_timer {
        record_session_change_timer(
            ctx,
            req.char_id,
            ctx.now_unix(),
            req.dest_sys_id,
            dest_tick,
            req.transition_reason,
        );
    }
    let transition_kind = match req.transition_kind {
        SpaceTransitionKind::Teleport => TransitionKind::Teleport {
            solar_system_id: req.dest_sys_id,
            dock_id: None,
            ship_id: None,
        },
        SpaceTransitionKind::JumpSystem => TransitionKind::JumpSystem {
            solar_system_id: req.dest_sys_id,
        },
    };
    let _ = ctx.execute_session_transition_sync(
        session.as_deref_mut(),
        SessionTransitionSpec::for_kind(req.char_id, transition_kind, req.transition_reason)
            .with_apply_db(false)
            .with_cleanup_departing_space(false)
            .with_sync_local_chat(false)
            .with_emit_on_session_changed(false),
    );
    if req.record_solar_system_jump {
        crate::services::map_service::record_solar_system_jump(ctx, req.char_id, req.dest_sys_id);
    }
    if req.bind_beyonce {
        if let Some(sess) = session {
            let _ = ctx.bind_object(sess, "beyonce", i64::from(req.dest_sys_id));
        }
    }
}

/// Shared space relocation operation for GM `/tr` / `/moveme` and gameplay cross-system jumps.
///
/// Cold origin system actors are never woken (`#1542`): departure looks up `old_sys_id` with
/// `ctx.systems.get_handle(old_sys_id)`. On DB persistence, destination-wake, or arrival-spawn
/// failure after `take_ball`, character location and the removed ball are rolled back.
pub async fn relocate_character_in_space(
    ctx: &ServerContext,
    session: Option<&mut SessionState>,
    req: RelocateInSpaceRequest,
) -> Result<RelocateInSpaceOutcome, DomainError> {
    let (outcome, ()) = relocate_character_in_space_with_pre_spawn(ctx, session, req, || ()).await?;
    Ok(outcome)
}

/// Variant of [`relocate_character_in_space`] invoking `pre_spawn_hook` after departure cleanups
/// (such as `deactivate_ship_modules_on_leave_space`) and immediately before destination
/// `spawn_arrival_ship_ball` / `build_ship_combat_state` so fitting heat/damage state stays synced.
pub async fn relocate_character_in_space_with_pre_spawn<F, T>(
    ctx: &ServerContext,
    mut session: Option<&mut SessionState>,
    req: RelocateInSpaceRequest,
    pre_spawn_hook: F,
) -> Result<(RelocateInSpaceOutcome, T), DomainError>
where
    F: FnOnce() -> T + Send,
{
    if req.dest_sys_id == 0 {
        return Err(DomainError::not_found(
            "SolarSystemNotFound",
            "Destination solar system 0 is invalid",
        ));
    }

    let same_system = req.old_sys_id > 0 && req.old_sys_id == req.dest_sys_id;
    let same_system_in_space = req.was_in_space && same_system && !req.require_take_ball;
    let old_handle = (req.was_in_space && req.old_sys_id > 0)
        .then(|| ctx.systems.get_handle(req.old_sys_id))
        .flatten();

    if same_system && !req.require_take_ball {
        let stopped_mid_warp = if same_system_in_space && req.ship_id > 0 {
            if let Some(ref pre_handle) = old_handle {
                detect_and_stop_warp(pre_handle, req.ship_id, false).await
            } else {
                false
            }
        } else {
            false
        };

        let updated_ch = ctx
            .db
            .set_character_solar_system(req.char_id, req.dest_sys_id, &ctx.sde)
            .map_err(|e| {
                DomainError::internal(
                    req.db_transition_failed_code,
                    format!("Failed to update character solar system: {e:?}"),
                )
            })?;
        let effective_ship_id = if updated_ch.active_ship_id > 0 {
            updated_ch.active_ship_id
        } else {
            req.ship_id
        };

        let dest_handle =
            match wake_destination_handle(ctx, req.dest_sys_id, &req.destination_wake).await {
                Ok(h) => h,
                Err(err) => {
                    rollback_failed_destination(ctx, &req, None, None).await;
                    return Err(err);
                }
            };
        let hook_out = pre_spawn_hook();
        let (dest_tick, ship_type_id, persisted_pos) = if effective_ship_id > 0 {
            if dest_handle
                .get_ball_snapshot(effective_ship_id)
                .await
                .ok()
                .flatten()
                .is_some()
            {
                if !stopped_mid_warp {
                    let _ = dest_handle.force_stop_warp(effective_ship_id);
                }
                if let Some(pos) = req.arrival_position {
                    let _ = dest_handle.set_ball_position(effective_ship_id, pos).await;
                }
                (0, lookup_ship_type_id(ctx, effective_ship_id), req.arrival_position)
            } else {
                let spawn_pos = req.arrival_position.unwrap_or(req.fallback_arrival_position);
                let (tick, tid) = match spawn_arrival_ship_ball(
                    ctx,
                    &dest_handle,
                    &req,
                    effective_ship_id,
                    spawn_pos,
                    None,
                )
                .await
                {
                    Ok(v) => v,
                    Err(err) => {
                        rollback_failed_destination(ctx, &req, None, None).await;
                        return Err(err);
                    }
                };
                (tick, tid, Some(spawn_pos))
            }
        } else {
            (0, 0, None)
        };

        if effective_ship_id > 0 && req.persist_item_position {
            if let Some(pos) = persisted_pos {
                ctx.db.with_mut_snapshot(|snap| {
                    snap.item_positions
                        .insert(effective_ship_id, [pos.x, pos.y, pos.z]);
                });
            }
        }

        if !req.was_in_space && req.sync_local_chat {
            ctx.sync_character_local_chat(
                req.char_id,
                (req.old_sys_id > 0).then_some(req.old_sys_id),
                req.dest_sys_id,
            );
        }

        finish_session_and_post_hooks(ctx, session.as_deref_mut(), &req, dest_tick);
        return Ok((
            RelocateInSpaceOutcome {
                updated_ch,
                dest_handle,
                dest_tick,
                ship_type_id,
                same_system_in_space,
                stopped_mid_warp,
                arrival_position: persisted_pos,
            },
            hook_out,
        ));
    }

    let mut stopped_mid_warp = false;
    let taken = if req.ship_id > 0 {
        if let Some(ref oh) = old_handle {
            if req.cleanup.stop_and_flush_warp_on_cross_system {
                stopped_mid_warp = detect_and_stop_warp(oh, req.ship_id, true).await;
            }
            let ball = oh.take_ball(req.ship_id).await;
            if req.require_take_ball && ball.is_none() {
                return Err(DomainError::precondition_failed(
                    req.take_ball_missing_code,
                    format!(
                        "Ship ball {} not found in solar system {}",
                        req.ship_id, req.old_sys_id
                    ),
                ));
            }
            ball
        } else if req.require_take_ball {
            return Err(DomainError::precondition_failed(
                req.take_ball_missing_code,
                format!(
                    "Origin solar system {} has no active actor for ship {}",
                    req.old_sys_id, req.ship_id
                ),
            ));
        } else {
            None
        }
    } else if req.require_take_ball {
        return Err(DomainError::precondition_failed(
            req.take_ball_missing_code,
            "Active ship is required for space transition",
        ));
    } else {
        None
    };

    let updated_ch = match ctx
        .db
        .set_character_solar_system(req.char_id, req.dest_sys_id, &ctx.sde)
    {
        Ok(ch) => ch,
        Err(e) => {
            if let (Some(oh), Some(ball)) = (&old_handle, taken) {
                let _ = oh.restore_ball(ball).await;
            }
            return Err(DomainError::internal(
                req.db_transition_failed_code,
                format!("Failed to transition character to system {}: {e:?}", req.dest_sys_id),
            ));
        }
    };

    let dest_handle =
        match wake_destination_handle(ctx, req.dest_sys_id, &req.destination_wake).await {
            Ok(h) => h,
            Err(err) => {
                rollback_failed_destination(ctx, &req, old_handle.as_ref(), taken).await;
                return Err(err);
            }
        };

    let effective_ship_id = if updated_ch.active_ship_id > 0 {
        updated_ch.active_ship_id
    } else {
        req.ship_id
    };
    let spawn_pos = req.arrival_position.unwrap_or(req.fallback_arrival_position);

    if effective_ship_id > 0 {
        if req.cleanup.abort_self_destruct && req.was_in_space {
            crate::sp6_lifecycle::abort_self_destruct_for_ship(ctx, effective_ship_id);
        }
        if req.cleanup.abandon_drones && req.was_in_space && req.old_sys_id > 0 {
            crate::sp6_lifecycle::abandon_drones_and_fighters_without_wake_sync(
                ctx,
                req.char_id,
                effective_ship_id,
                req.old_sys_id,
            );
        }
        if req.cleanup.deactivate_modules {
            crate::services::space_navigation_advanced::deactivate_ship_modules_on_leave_space(
                ctx,
                req.char_id,
                effective_ship_id,
            );
        }

        let should_leave_space = match req.cleanup.leave_space_in_snap {
            LeaveSpaceSnapMode::Never => false,
            LeaveSpaceSnapMode::OnlyIfTakenBall => taken.is_some(),
            LeaveSpaceSnapMode::Always => true,
        };
        let persist_pos = req
            .persist_item_position
            .then_some([spawn_pos.x, spawn_pos.y, spawn_pos.z]);
        if should_leave_space || persist_pos.is_some() {
            let cs_for_snap = taken
                .as_ref()
                .and_then(|t| t.combat_state.clone())
                .or_else(|| req.combat_state_preservation.fallback_combat_state().cloned());
            ctx.db.with_mut_snapshot(|snap| {
                if should_leave_space {
                    crate::sp6_lifecycle::leave_space_in_snap(
                        snap,
                        effective_ship_id,
                        cs_for_snap.as_ref(),
                        Some(req.dest_sys_id),
                        persist_pos,
                    );
                } else if let Some(pos) = persist_pos {
                    snap.item_positions.insert(effective_ship_id, pos);
                }
            });
        }
    }

    let hook_out = pre_spawn_hook();

    let (dest_tick, ship_type_id) = if effective_ship_id > 0 {
        match spawn_arrival_ship_ball(
            ctx,
            &dest_handle,
            &req,
            effective_ship_id,
            spawn_pos,
            taken.as_ref(),
        )
        .await
        {
            Ok(v) => v,
            Err(err) => {
                rollback_failed_destination(ctx, &req, old_handle.as_ref(), taken).await;
                return Err(err);
            }
        }
    } else {
        (0, 0)
    };
    drop(taken);

    if req.sync_local_chat && req.old_sys_id > 0 {
        ctx.sync_character_local_chat(req.char_id, Some(req.old_sys_id), req.dest_sys_id);
    }

    if req.push_set_state && effective_ship_id > 0 {
        let _ = dest_handle.push_set_state(effective_ship_id).await;
    }

    finish_session_and_post_hooks(ctx, session.as_deref_mut(), &req, dest_tick);

    Ok((
        RelocateInSpaceOutcome {
            updated_ch,
            dest_handle,
            dest_tick,
            ship_type_id,
            same_system_in_space: false,
            stopped_mid_warp,
            arrival_position: (effective_ship_id > 0).then_some(spawn_pos),
        },
        hook_out,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use eve_db::InventoryItemRecord;

    const JITA: u32 = 30_000_142;
    const PERIMETER: u32 = 30_000_144;
    const MAURASI: u32 = 30_000_140;

    fn make_session(cid: i64, corp_id: i64, sys_id: u32) -> SessionState {
        let mut s = SessionState::new(1);
        s.user_id = Some(1);
        s.character_id = Some(cid);
        s.corporation_id = Some(corp_id);
        s.solar_system_id = Some(sys_id);
        s
    }

    fn seed_test_pilot(ctx: &ServerContext, cid: i64, ship_id: i64, sys_id: u32) {
        let mut ch = CharacterRecord::default();
        ch.character_id = cid;
        ch.name = format!("Pilot_{cid}");
        ch.corporation_id = 1_000_167;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        ch.station_id = None;
        ch.structure_id = None;
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.insert(cid, ch);
            let mut ship = InventoryItemRecord::default();
            ship.item_id = ship_id;
            ship.type_id = 587;
            ship.owner_id = cid;
            ship.location_id = i64::from(sys_id);
            snap.items.insert(ship_id, ship);
        });
    }

    #[tokio::test]
    async fn cold_origin_system_stays_cold_and_dest_is_woken() {
        let ctx = ServerContext::new();
        let cid = 90_991_001_i64;
        let ship_id = 91_991_001_i64;
        seed_test_pilot(&ctx, cid, ship_id, MAURASI);
        assert!(ctx.systems.get_handle(MAURASI).is_none());
        assert!(ctx.systems.get_handle(PERIMETER).is_none());

        let mut sess = make_session(cid, 1_000_167, MAURASI);
        let outcome = relocate_character_in_space(
            &ctx,
            Some(&mut sess),
            RelocateInSpaceRequest {
                char_id: cid,
                ship_id,
                old_sys_id: MAURASI,
                old_dock: None,
                dest_sys_id: PERIMETER,
                was_in_space: true,
                arrival_position: Some(DVec3::new(1_000.0, 2_000.0, 3_000.0)),
                fallback_arrival_position: DVec3::new(10_000.0, 0.0, 0.0),
                arrival_velocity: DVec3::ZERO,
                require_take_ball: false,
                take_ball_missing_code: "SystemTransitionFailed",
                db_transition_failed_code: "TeleportFailed",
                destination_wake: DestinationWakePolicy::GmCommand("/tr".to_string()),
                transition_kind: SpaceTransitionKind::Teleport,
                transition_reason: "teleport",
                combat_state_preservation: CombatStatePreservation::Fresh,
                gate_cloak_invuln: GateCloakInvulnPolicy::none(),
                cleanup: LeaveSpaceCleanupPolicy::gm_teleport(),
                sync_local_chat: true,
                persist_item_position: true,
                record_session_change_timer: false,
                record_solar_system_jump: false,
                bind_beyonce: false,
                push_set_state: false,
            },
        )
        .await
        .expect("cold origin relocation succeeds");

        assert_eq!(outcome.updated_ch.solar_system_id, PERIMETER);
        assert_eq!(sess.solar_system_id, Some(PERIMETER));
        assert!(
            ctx.systems.get_handle(MAURASI).is_none(),
            "cold origin solar system must not be spawned"
        );
        assert!(
            ctx.systems.get_handle(PERIMETER).is_some(),
            "destination solar system must be woken"
        );
        let (_, ball) = outcome
            .dest_handle
            .get_ball_snapshot(ship_id)
            .await
            .expect("dest snapshot")
            .expect("ship spawned at dest");
        assert!((ball.position - DVec3::new(1_000.0, 2_000.0, 3_000.0)).length() < 1e-6);
    }

    #[tokio::test]
    async fn db_failure_restores_taken_ball_to_origin_system() {
        let ctx = ServerContext::new();
        let missing_cid = 90_991_999_i64;
        let ship_id = 91_991_999_i64;
        let origin_handle = ctx
            .systems
            .wake_for_session_arrival_async(JITA, SessionArrivalWakeReason::Undock)
            .await
            .expect("wake jita");
        origin_handle
            .spawn_ball_with_meta(
                Ball {
                    id: ship_id,
                    type_id: 587,
                    position: DVec3::new(500.0, 0.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 300.0,
                    inertia: 0.5,
                    mass: 1_000_000.0,
                    radius: 50.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                CombatState::default(),
                ctx.build_ship_slim_meta(missing_cid, 587),
            )
            .await
            .expect("spawn origin ball");

        let mut sess = make_session(missing_cid, 1_000_167, JITA);
        let err = relocate_character_in_space(
            &ctx,
            Some(&mut sess),
            RelocateInSpaceRequest {
                char_id: missing_cid,
                ship_id,
                old_sys_id: JITA,
                old_dock: None,
                dest_sys_id: PERIMETER,
                was_in_space: true,
                arrival_position: Some(DVec3::ZERO),
                fallback_arrival_position: DVec3::ZERO,
                arrival_velocity: DVec3::ZERO,
                require_take_ball: true,
                take_ball_missing_code: "SystemTransitionFailed",
                db_transition_failed_code: "SystemTransitionFailed",
                destination_wake: DestinationWakePolicy::SessionArrival(
                    SessionArrivalWakeReason::Jump,
                ),
                transition_kind: SpaceTransitionKind::JumpSystem,
                transition_reason: "shipcaster",
                combat_state_preservation: CombatStatePreservation::PreserveClamped {
                    fallback: None,
                },
                gate_cloak_invuln: GateCloakInvulnPolicy::standard_jump_cloak(),
                cleanup: LeaveSpaceCleanupPolicy::gameplay_jump(),
                sync_local_chat: true,
                persist_item_position: true,
                record_session_change_timer: true,
                record_solar_system_jump: true,
                bind_beyonce: true,
                push_set_state: true,
            },
        )
        .await
        .expect_err("missing character must fail DB transition and roll back ball");

        assert_eq!(err.code, "SystemTransitionFailed");
        assert!(
            origin_handle
                .get_ball_snapshot(ship_id)
                .await
                .expect("origin snapshot")
                .is_some(),
            "taken ball must be restored to origin system on DB transition failure"
        );
    }

    #[tokio::test]
    async fn destination_wake_failure_rolls_back_db_and_restores_ball() {
        let ctx = ServerContext::new();
        let cid = 90_991_888_i64;
        let ship_id = 91_991_888_i64;
        seed_test_pilot(&ctx, cid, ship_id, JITA);
        let origin_handle = ctx
            .systems
            .wake_for_session_arrival_async(JITA, SessionArrivalWakeReason::Undock)
            .await
            .expect("wake jita");
        origin_handle
            .spawn_ball_with_meta(
                Ball {
                    id: ship_id,
                    type_id: 587,
                    position: DVec3::new(750.0, 0.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 300.0,
                    inertia: 0.5,
                    mass: 1_000_000.0,
                    radius: 50.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                CombatState::default(),
                ctx.build_ship_slim_meta(cid, 587),
            )
            .await
            .expect("spawn origin ball");

        let mut sess = make_session(cid, 1_000_167, JITA);
        // Empty GM command string rejects inside `wake_for_gm_command_async` AFTER `take_ball` and
        // DB transition, exercising destination-wake rollback (`P2`).
        let err = relocate_character_in_space(
            &ctx,
            Some(&mut sess),
            RelocateInSpaceRequest {
                char_id: cid,
                ship_id,
                old_sys_id: JITA,
                old_dock: None,
                dest_sys_id: PERIMETER,
                was_in_space: true,
                arrival_position: Some(DVec3::ZERO),
                fallback_arrival_position: DVec3::ZERO,
                arrival_velocity: DVec3::ZERO,
                require_take_ball: true,
                take_ball_missing_code: "SystemTransitionFailed",
                db_transition_failed_code: "SystemTransitionFailed",
                destination_wake: DestinationWakePolicy::GmCommand(String::new()),
                transition_kind: SpaceTransitionKind::Teleport,
                transition_reason: "teleport",
                combat_state_preservation: CombatStatePreservation::Fresh,
                gate_cloak_invuln: GateCloakInvulnPolicy::none(),
                cleanup: LeaveSpaceCleanupPolicy::gm_teleport(),
                sync_local_chat: true,
                persist_item_position: true,
                record_session_change_timer: false,
                record_solar_system_jump: false,
                bind_beyonce: false,
                push_set_state: false,
            },
        )
        .await
        .expect_err("destination wake failure must return Err and roll back");

        assert_eq!(err.code, "SystemTransitionFailed");
        assert_eq!(sess.solar_system_id, Some(JITA));
        assert!(ctx.systems.get_handle(PERIMETER).is_none());
        ctx.db.with_snapshot(|snap| {
            assert_eq!(snap.characters[&cid].solar_system_id, JITA);
            assert_eq!(snap.items[&ship_id].location_id, i64::from(JITA));
        });
        let (_, restored) = origin_handle
            .get_ball_snapshot(ship_id)
            .await
            .expect("origin snapshot")
            .expect("taken ball must be restored to origin on wake failure");
        assert!((restored.position - DVec3::new(750.0, 0.0, 0.0)).length() < 1e-6);
    }
}
