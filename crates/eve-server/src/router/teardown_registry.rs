//! Exhaustive declarative `Transition` teardown registry (`#1190`, `#848`, `#428`, `#1122`).
//!
//! Centralizes the cross-subsystem teardown matrix across every `TransitionKind` and
//! `Disconnect` so new transitions or subsystems cannot silently drop cleanup hooks.

#![forbid(unsafe_code)]

use std::collections::HashMap;

use eve_db::CharacterRecord;
use eve_machonet::SessionState;
use eve_proto::PyRep;

use super::notify_registry::OnTradeCancel;
use super::session_transition::{SessionTransitionSpec, TransitionKind};
use super::{NotificationTarget, ServerContext};

/// Normalized lifecycle triggers handled by the declarative teardown registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TransitionTrigger {
    Dock,
    Undock,
    JumpSystem,
    BoardShip,
    EjectToPod,
    CloneJump,
    DeathRespawn,
    Teleport,
    CharacterLogin,
    Logout,
    Disconnect,
}

impl TransitionTrigger {
    /// Total list of transition and disconnect triggers in canonical order.
    pub const ALL: [Self; 11] = [
        Self::Dock,
        Self::Undock,
        Self::JumpSystem,
        Self::BoardShip,
        Self::EjectToPod,
        Self::CloneJump,
        Self::DeathRespawn,
        Self::Teleport,
        Self::CharacterLogin,
        Self::Logout,
        Self::Disconnect,
    ];

    /// Exhaustively maps a runtime `TransitionKind` to its registry trigger.
    pub const fn from_kind(kind: TransitionKind) -> Self {
        match kind {
            TransitionKind::Dock { .. } => Self::Dock,
            TransitionKind::Undock { .. } => Self::Undock,
            TransitionKind::JumpSystem { .. } => Self::JumpSystem,
            TransitionKind::BoardShip { .. } => Self::BoardShip,
            TransitionKind::EjectToPod { .. } => Self::EjectToPod,
            TransitionKind::CloneJump { .. } => Self::CloneJump,
            TransitionKind::DeathRespawn { .. } => Self::DeathRespawn,
            TransitionKind::Teleport { .. } => Self::Teleport,
            TransitionKind::CharacterLogin { .. } => Self::CharacterLogin,
            TransitionKind::Logout => Self::Logout,
        }
    }
}

/// Every stateful subsystem participating in transition/disconnect teardown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TeardownSubsystem {
    TradeSessions,
    SafeLogoffState,
    SystemWideEffects,
    ModuleCycles,
    TargetLocks,
    DronesAndFighters,
    Probes,
    SelfDestructTimer,
    ConcordResponse,
    CloakAndTether,
    BoundObjects,
    LocalChatMembership,
    DisconnectPresenceAndControl,
}

impl TeardownSubsystem {
    /// Total list of teardown-managed subsystems.
    pub const ALL: [Self; 13] = [
        Self::TradeSessions,
        Self::SafeLogoffState,
        Self::SystemWideEffects,
        Self::ModuleCycles,
        Self::TargetLocks,
        Self::DronesAndFighters,
        Self::Probes,
        Self::SelfDestructTimer,
        Self::ConcordResponse,
        Self::CloakAndTether,
        Self::BoundObjects,
        Self::LocalChatMembership,
        Self::DisconnectPresenceAndControl,
    ];
}

/// Ordered teardown execution stage within a transition or disconnect lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TeardownPhase {
    PreDepartingSpace = 0,
    PreDbCommit = 1,
    InDbCommit = 2,
    PostTransitionSync = 3,
    SessionFinalize = 4,
}

/// Declarative action executed for a `(TeardownSubsystem, TransitionTrigger)` cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TeardownHookAction {
    CancelOpenTrades,
    CleanupOrAbortSafeLogoff { emit_abort_on_pending: bool },
    ClearSafeLogoffSilent,
    DeactivateShipModulesWhenLeavingSpace,
    ClearBallparkTargetLocksAndDepartingBall,
    AbandonDronesFightersAndResetProbes,
    AbortSelfDestructTimer,
    ClearCloakAndWarpSettle,
    CleanupCombatStorageAndSyncConcord,
    EmitDepartingSystemWideEffectsStopAndSyncInfo,
    InvalidateScopedBoundObjects,
    SyncLocalChatMembership,
    RunDisconnectPresenceAndControlCleanup,
    NoOp,
}

/// Single declarative policy cell in the exhaustive transition teardown matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TeardownPolicyRow {
    pub subsystem: TeardownSubsystem,
    pub trigger: TransitionTrigger,
    pub phase: TeardownPhase,
    pub action: TeardownHookAction,
    pub rationale: &'static str,
}

/// Exhaustive declarative teardown matrix over every `(TeardownSubsystem, TransitionTrigger)` pair.
pub const fn declared_teardown_policy(
    subsystem: TeardownSubsystem,
    trigger: TransitionTrigger,
) -> TeardownPolicyRow {
    use TeardownHookAction as A;
    use TeardownPhase as P;
    use TeardownSubsystem as S;
    use TransitionTrigger as T;

    let (phase, action, rationale) = match subsystem {
        S::TradeSessions => match trigger {
            T::Dock | T::Undock | T::JumpSystem | T::BoardShip | T::EjectToPod
            | T::CloneJump | T::DeathRespawn | T::Teleport | T::CharacterLogin
            | T::Logout | T::Disconnect => (
                P::InDbCommit, A::CancelOpenTrades,
                "Cancel open player trades and unlock escrowed items on every transition, logout, or disconnect (#848).",
            ),
        },
        S::SafeLogoffState => match trigger {
            T::Dock | T::Undock | T::JumpSystem | T::BoardShip | T::EjectToPod
            | T::CloneJump | T::DeathRespawn | T::Teleport => (
                P::PreDbCommit, A::CleanupOrAbortSafeLogoff { emit_abort_on_pending: true },
                "Clear completed safe-logoff state or abort active safe-logoff countdown on action (#428).",
            ),
            T::CharacterLogin | T::Logout | T::Disconnect => (
                P::PostTransitionSync, A::ClearSafeLogoffSilent,
                "Purge completed or stale safe-logoff entry silently during login, logout, or disconnect (#428).",
            ),
        },
        S::SystemWideEffects => match trigger {
            T::Dock | T::JumpSystem | T::CloneJump | T::DeathRespawn
            | T::Teleport | T::Logout | T::Disconnect => (
                P::PostTransitionSync, A::EmitDepartingSystemWideEffectsStopAndSyncInfo,
                "Emit char-scoped OnSystemWideEffectStop on space/system exit and resync info bubble (#1122).",
            ),
            T::Undock | T::BoardShip | T::EjectToPod | T::CharacterLogin => (
                P::PostTransitionSync, A::NoOp,
                "Destination stays in or enters same-system space; session diff syncs destination effects.",
            ),
        },
        S::ModuleCycles => match trigger {
            T::Dock | T::JumpSystem | T::CloneJump | T::DeathRespawn | T::Teleport => (
                P::PreDepartingSpace, A::DeactivateShipModulesWhenLeavingSpace,
                "Deactivate active ship modules and superweapons before departing space scene.",
            ),
            T::Undock | T::BoardShip | T::EjectToPod | T::CharacterLogin | T::Logout | T::Disconnect => (
                P::PreDepartingSpace, A::NoOp,
                "Not leaving space via transition cleanup or handled by disconnect hold lifecycle.",
            ),
        },
        S::TargetLocks => match trigger {
            T::Dock | T::JumpSystem | T::CloneJump | T::DeathRespawn | T::Teleport => (
                P::PreDepartingSpace, A::ClearBallparkTargetLocksAndDepartingBall,
                "Clear ballpark target locks and remove or dock departing vessel ball.",
            ),
            T::Undock | T::BoardShip | T::EjectToPod | T::CharacterLogin | T::Logout | T::Disconnect => (
                P::PreDepartingSpace, A::NoOp,
                "No ballpark removal needed or handled by disconnect hold lifecycle.",
            ),
        },
        S::DronesAndFighters => match trigger {
            T::Dock | T::JumpSystem | T::CloneJump | T::DeathRespawn | T::Teleport => (
                P::PreDepartingSpace, A::AbandonDronesFightersAndResetProbes,
                "Abandon deployed drones/fighters and break controller ownership when leaving space (#206).",
            ),
            T::Undock | T::BoardShip | T::EjectToPod | T::CharacterLogin | T::Logout | T::Disconnect => (
                P::PreDepartingSpace, A::NoOp,
                "No space exit or handled by vessel handoff / disconnect hold rules.",
            ),
        },
        S::Probes => match trigger {
            T::Dock | T::JumpSystem | T::CloneJump | T::DeathRespawn | T::Teleport => (
                P::PreDepartingSpace, A::AbandonDronesFightersAndResetProbes,
                "Sync scanner probe state when leaving space.",
            ),
            T::Undock | T::BoardShip | T::EjectToPod | T::CharacterLogin | T::Logout | T::Disconnect => (
                P::PreDepartingSpace, A::NoOp,
                "Probe state stays attached unless leaving space cleanly.",
            ),
        },
        S::SelfDestructTimer => match trigger {
            T::Dock | T::JumpSystem | T::BoardShip | T::EjectToPod
            | T::CloneJump | T::DeathRespawn | T::Teleport | T::Disconnect => (
                P::PreDepartingSpace, A::AbortSelfDestructTimer,
                "Abort pending vessel self-destruct timer when leaving or swapping ship cleanly (#78).",
            ),
            T::Undock | T::CharacterLogin | T::Logout => (
                P::PreDepartingSpace, A::NoOp,
                "Docked ship or login/logout view switch does not hold an in-space countdown.",
            ),
        },
        S::ConcordResponse => match trigger {
            T::Dock | T::Undock | T::JumpSystem | T::BoardShip | T::EjectToPod
            | T::CloneJump | T::DeathRespawn | T::Teleport | T::CharacterLogin
            | T::Logout | T::Disconnect => (
                P::PostTransitionSync, A::CleanupCombatStorageAndSyncConcord,
                "Prune in-space ALSC authorization on departure and schedule high-sec criminal CONCORD on space entry (#559).",
            ),
        },
        S::CloakAndTether => match trigger {
            T::Dock | T::JumpSystem | T::BoardShip | T::EjectToPod
            | T::CloneJump | T::DeathRespawn | T::Teleport | T::Logout | T::Disconnect => (
                P::PostTransitionSync, A::ClearCloakAndWarpSettle,
                "Clear cloak activation timestamp and warp-stop settle deadline on space or ship exit.",
            ),
            T::Undock | T::CharacterLogin => (
                P::PostTransitionSync, A::NoOp,
                "Not departing an existing in-space vessel state.",
            ),
        },
        S::BoundObjects => match trigger {
            T::Dock | T::Undock | T::JumpSystem | T::BoardShip | T::EjectToPod
            | T::CloneJump | T::DeathRespawn | T::Teleport | T::Logout | T::Disconnect => (
                P::SessionFinalize, A::InvalidateScopedBoundObjects,
                "Invalidate scoped Macho bound objects on scene, station, or session change.",
            ),
            T::CharacterLogin => (
                P::SessionFinalize, A::NoOp,
                "Initial login binds fresh objects rather than invalidating prior bounds.",
            ),
        },
        S::LocalChatMembership => match trigger {
            T::JumpSystem | T::CloneJump | T::DeathRespawn | T::Teleport | T::CharacterLogin => (
                P::PostTransitionSync, A::SyncLocalChatMembership,
                "Move local solar-system chat channel membership to destination system.",
            ),
            T::Dock | T::Undock | T::BoardShip | T::EjectToPod | T::Logout | T::Disconnect => (
                P::PostTransitionSync, A::NoOp,
                "Same solar system retains local chat or disconnect/logout fleet-social handles removal.",
            ),
        },
        S::DisconnectPresenceAndControl => match trigger {
            T::Disconnect => (
                P::PostTransitionSync, A::RunDisconnectPresenceAndControlCleanup,
                "Run safe/combat disconnect ship handling, watchlist offline, fleet/social disconnect, XMPP revocation, and queue purge.",
            ),
            T::Dock | T::Undock | T::JumpSystem | T::BoardShip | T::EjectToPod
            | T::CloneJump | T::DeathRespawn | T::Teleport | T::CharacterLogin | T::Logout => (
                P::PostTransitionSync, A::NoOp,
                "Online client connection remains attached.",
            ),
        },
    };
    TeardownPolicyRow { subsystem, trigger, phase, action, rationale }
}

/// Returns the policy cell for `(subsystem, trigger)`.
pub fn lookup_teardown_policy(
    subsystem: TeardownSubsystem,
    trigger: TransitionTrigger,
) -> TeardownPolicyRow {
    declared_teardown_policy(subsystem, trigger)
}

/// Collects the complete matrix across every `(TeardownSubsystem, TransitionTrigger)` pair.
pub fn all_teardown_policies() -> Vec<TeardownPolicyRow> {
    let mut rows = Vec::with_capacity(TeardownSubsystem::ALL.len() * TransitionTrigger::ALL.len());
    for subsystem in TeardownSubsystem::ALL {
        for trigger in TransitionTrigger::ALL {
            rows.push(declared_teardown_policy(subsystem, trigger));
        }
    }
    rows
}

pub(crate) fn policy_matches(
    subsystem: TeardownSubsystem,
    trigger: TransitionTrigger,
    action: TeardownHookAction,
) -> bool {
    declared_teardown_policy(subsystem, trigger).action == action
}

/// Captured pre-transition spatial and vessel state before DB or session mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DepartingSceneSnapshot {
    pub sys_id: u32,
    pub dock_id: Option<i64>,
    pub ship_id: i64,
    pub in_space: bool,
}

fn departing_from_sources(
    session_opt: Option<&SessionState>,
    pre_ch: Option<&CharacterRecord>,
    prefer_session: bool,
    fallback_ch_dock_on_session: bool,
) -> DepartingSceneSnapshot {
    let sess_sys = session_opt.and_then(|s| s.solar_system_id);
    let sess_dock = session_opt.and_then(|s| s.station_id.or(s.structure_id));
    let sess_ship = session_opt.and_then(|s| s.ship_id);
    let ch_sys = pre_ch.map(|c| c.solar_system_id);
    let ch_dock = pre_ch.and_then(|c| c.station_id.or(c.structure_id));
    let ch_ship = pre_ch.map(|c| c.active_ship_id);
    let (sys_id, dock_id, ship_id) = if prefer_session {
        (
            sess_sys.or(ch_sys).unwrap_or(0),
            if fallback_ch_dock_on_session { sess_dock.or(ch_dock) } else { sess_dock },
            sess_ship.or(ch_ship).unwrap_or(0),
        )
    } else {
        (
            ch_sys.or(sess_sys).unwrap_or(0),
            ch_dock.or(sess_dock),
            ch_ship.or(sess_ship).unwrap_or(0),
        )
    };
    DepartingSceneSnapshot { sys_id, dock_id, ship_id, in_space: dock_id.is_none() && sys_id != 0 }
}

/// Resolves the departing `(solar_system_id, dock_id, active_ship_id)` accurately for both
/// DB-owning transitions (`apply_db == true`) and session-sync calls (`apply_db == false`)
/// where callers mutated DB before invoking `execute_session_transition*`.
pub fn resolve_departing_scene(
    prev_session: &SessionState,
    pre_ch: Option<&CharacterRecord>,
    has_live_session: bool,
    spec: &SessionTransitionSpec,
) -> DepartingSceneSnapshot {
    let is_login = matches!(spec.kind, TransitionKind::CharacterLogin { .. });
    let use_session =
        !spec.apply_db && has_live_session && !is_login && prev_session.solar_system_id.is_some();
    departing_from_sources(Some(prev_session), pre_ch, use_session, false)
}

/// Releases `"trademgr"` bound objects and emits `OnTradeCancel` to both parties for
/// trades cancelled inside a DB transaction (`#848`).
pub fn emit_cancelled_trade_notifications(
    ctx: &ServerContext,
    cancelled_trades: &[(i64, i64, i64)],
) {
    for &(trade_id, char_a, char_b) in cancelled_trades {
        ctx.release_bound_objects_for_object("trademgr", trade_id);
        for party in [char_a, char_b] {
            ctx.emit_typed(
                NotificationTarget::Character(party),
                OnTradeCancel {
                    container_id: trade_id,
                },
            );
        }
    }
}

/// Cross-system, dock, or ship-swap `Teleport` runs full departing-space teardown; same-system,
/// same-ship space teleports (`/tr` within one solar system) keep in-space modules, target locks,
/// drones, probes, and self-destruct intact (`BF-TEARDOWN-01`).
pub(crate) fn allows_departing_space_or_ship_teardown(
    spec: &SessionTransitionSpec,
    departing: &DepartingSceneSnapshot,
) -> bool {
    match spec.kind {
        TransitionKind::Teleport { solar_system_id, dock_id, ship_id } if departing.in_space => {
            dock_id.is_some()
                || solar_system_id != departing.sys_id
                || ship_id.is_some_and(|next_ship| next_ship > 0 && next_ship != departing.ship_id)
        }
        _ => true,
    }
}

/// Returns whether asynchronous `Ballpark` target-lock clearance and ball removal should run.
pub fn should_run_departing_ballpark_cleanup(
    spec: &SessionTransitionSpec,
    departing: &DepartingSceneSnapshot,
) -> bool {
    let trigger = TransitionTrigger::from_kind(spec.kind);
    spec.cleanup_departing_space
        && departing.in_space
        && departing.ship_id > 0
        && allows_departing_space_or_ship_teardown(spec, departing)
        && policy_matches(
            TeardownSubsystem::TargetLocks,
            trigger,
            TeardownHookAction::ClearBallparkTargetLocksAndDepartingBall,
        )
}

/// Executes pre-DB transition teardown hooks according to `declared_teardown_policy`.
pub fn run_pre_transition_teardown_sync(
    ctx: &ServerContext,
    spec: &SessionTransitionSpec,
    departing: &DepartingSceneSnapshot,
) {
    use TeardownHookAction as A;
    use TeardownSubsystem as S;

    let trigger = TransitionTrigger::from_kind(spec.kind);
    if spec.clear_completed_safe_logoff
        && matches!(
            declared_teardown_policy(S::SafeLogoffState, trigger).action,
            A::CleanupOrAbortSafeLogoff { .. }
        )
    {
        let _ = crate::sp6_lifecycle::maybe_abort_safe_logoff_on_action(
            ctx,
            spec.char_id,
            spec.reason,
        );
    }

    if !allows_departing_space_or_ship_teardown(spec, departing) {
        return;
    }

    if spec.cleanup_departing_space && departing.in_space && departing.ship_id > 0 {
        let abandon = policy_matches(S::DronesAndFighters, trigger, A::AbandonDronesFightersAndResetProbes)
            || policy_matches(S::Probes, trigger, A::AbandonDronesFightersAndResetProbes);
        if spec.abandon_drones && abandon {
            crate::sp6_lifecycle::abandon_drones_and_fighters_for_ship_sync(
                ctx, spec.char_id, departing.ship_id, departing.sys_id,
            );
        }
    }

    if spec.abort_self_destruct && policy_matches(S::SelfDestructTimer, trigger, A::AbortSelfDestructTimer) {
        if departing.ship_id > 0 {
            crate::sp6_lifecycle::abort_self_destruct_for_ship(ctx, departing.ship_id);
        }
        if let Some(target_sid) = spec.kind.target_ship_id() {
            if target_sid > 0 && target_sid != departing.ship_id {
                crate::sp6_lifecycle::abort_self_destruct_for_ship(ctx, target_sid);
            }
        }
    }

    if spec.cleanup_departing_space
        && departing.in_space
        && departing.ship_id > 0
        && spec.deactivate_modules
        && policy_matches(S::ModuleCycles, trigger, A::DeactivateShipModulesWhenLeavingSpace)
    {
        crate::services::space_navigation_advanced::deactivate_ship_modules_on_leave_space(
            ctx, spec.char_id, departing.ship_id,
        );
    }
    if spec.char_id > 0 {
        ctx.scanners_superweapons.clear_hacking_session_for_char(spec.char_id);
    }
}

/// Cancels open direct trades via DB mutation when a transition skipped `apply_db` (`#848`).
pub fn run_non_db_trade_teardown_sync(ctx: &ServerContext, spec: &SessionTransitionSpec) {
    let trigger = TransitionTrigger::from_kind(spec.kind);
    if spec.close_active_trades
        && policy_matches(
            TeardownSubsystem::TradeSessions,
            trigger,
            TeardownHookAction::CancelOpenTrades,
        )
    {
        crate::services::wallet_services_ext::cancel_open_trades_for_character(ctx, spec.char_id);
    }
}

fn emit_departing_system_wide_stops_if_exited(
    ctx: &ServerContext,
    char_id: i64,
    departing: &DepartingSceneSnapshot,
    exited_space_scene: bool,
) {
    if !exited_space_scene {
        return;
    }
    let active = crate::services::notify_emitters_tournaments_anomalies::collect_active_system_wide_effects_in_system(
        ctx,
        Some(departing.sys_id),
        true,
    );
    for (effect_id, source_item_id, source_type_id) in active {
        crate::services::notify_emitters_tournaments_anomalies::emit_system_wide_effect_stop_for_character(
            ctx,
            char_id,
            effect_id,
            source_item_id,
            i64::from(source_type_id),
        );
    }
}

/// Clears a ship's cloak activation timestamp (for Mobile Observatory targeting) and warp settle deadline.
pub(crate) fn clear_ship_cloak_and_warp_settle_for_ship(
    ctx: &ServerContext,
    sys_id: u32,
    ship_id: i64,
) {
    crate::services::mobile_deployables_ext::clear_ship_cloak_activated(ctx, ship_id);
    if sys_id != 0 {
        if let Some(handle) = ctx.systems.get_handle(sys_id) {
            handle.clear_warp_stopped_at(ship_id);
        }
    }
}

fn clear_ship_cloak_and_warp_settle(ctx: &ServerContext, departing: &DepartingSceneSnapshot) {
    clear_ship_cloak_and_warp_settle_for_ship(ctx, departing.sys_id, departing.ship_id);
}

fn prune_alsc_shipping_storage(ctx: &ServerContext, char_id: i64) {
    crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
        rt.authorized_shipping_storage
            .retain(|&(_, owner)| owner != char_id);
    });
}

fn compute_space_exit_state(
    departing: &DepartingSceneSnapshot,
    updated_ch: Option<&CharacterRecord>,
    is_logout: bool,
) -> (Option<u32>, bool, i64, bool) {
    let (next_sys_opt, next_in_space, next_ship) = match updated_ch {
        Some(ch) if !is_logout => (
            (ch.solar_system_id != 0).then_some(ch.solar_system_id),
            ch.station_id.is_none() && ch.structure_id.is_none() && ch.solar_system_id != 0,
            ch.active_ship_id,
        ),
        _ => (None, false, 0),
    };
    let exited_space_scene = departing.in_space
        && (is_logout || !next_in_space || next_sys_opt != Some(departing.sys_id));
    (next_sys_opt, next_in_space, next_ship, exited_space_scene)
}

/// Executes post-DB / pre-session-finalize registry hooks: `LocalChatMembership`,
/// `CloakAndTether`, `ConcordResponse`, and departing `SystemWideEffects` stop events.
pub fn run_post_db_pre_finalize_hooks_sync(
    ctx: &ServerContext,
    spec: &SessionTransitionSpec,
    departing: &DepartingSceneSnapshot,
    updated_ch: Option<&CharacterRecord>,
) {
    let trigger = TransitionTrigger::from_kind(spec.kind);
    let is_logout = matches!(spec.kind, TransitionKind::Logout) || spec.reason == "logout";
    let (next_sys_opt, next_in_space, next_ship, exited_space_scene) =
        compute_space_exit_state(departing, updated_ch, is_logout);

    use TeardownHookAction as A;
    use TeardownSubsystem as S;

    if spec.clear_completed_safe_logoff
        && policy_matches(S::SafeLogoffState, trigger, A::ClearSafeLogoffSilent)
    {
        crate::sp6_lifecycle::clear_safe_logoff(&ctx.db, spec.char_id);
    }

    if spec.sync_local_chat
        && policy_matches(S::LocalChatMembership, trigger, A::SyncLocalChatMembership)
    {
        if let Some(ch) = updated_ch {
            let prev_opt =
                if matches!(spec.kind, TransitionKind::CharacterLogin { .. }) || departing.sys_id == 0 {
                    None
                } else {
                    Some(departing.sys_id)
                };
            ctx.sync_character_local_chat(spec.char_id, prev_opt, ch.solar_system_id);
        }
    }

    if policy_matches(S::CloakAndTether, trigger, A::ClearCloakAndWarpSettle)
        && departing.in_space
        && departing.ship_id > 0
        && (exited_space_scene || (next_ship > 0 && next_ship != departing.ship_id))
    {
        clear_ship_cloak_and_warp_settle(ctx, departing);
    }

    if policy_matches(S::ConcordResponse, trigger, A::CleanupCombatStorageAndSyncConcord) {
        if exited_space_scene {
            prune_alsc_shipping_storage(ctx, spec.char_id);
        }
        if let Some(next_sys) = next_sys_opt.filter(|_| !is_logout && next_in_space && next_ship > 0)
        {
            crate::services::combat_crimewatch::schedule_highsec_criminal_concord_if_needed(
                ctx, spec.char_id, next_ship, next_sys, ctx.now_unix(),
            );
        }
    }

    if spec.emit_system_wide_effect_stop
        && policy_matches(S::SystemWideEffects, trigger, A::EmitDepartingSystemWideEffectsStopAndSyncInfo)
    {
        emit_departing_system_wide_stops_if_exited(ctx, spec.char_id, departing, exited_space_scene);
    }
}

/// Ensures `OnUpdateSystemWideEffectsInfo` is emitted after exiting a space scene even when
/// `emit_on_session_changed` was skipped (`emit_on_session_changed == false`, empty diff, or `Logout`).
pub fn run_post_finalize_system_wide_sync(
    ctx: &ServerContext,
    spec: &SessionTransitionSpec,
    departing: &DepartingSceneSnapshot,
    updated_ch: Option<&CharacterRecord>,
    diff: &HashMap<String, (PyRep, PyRep)>,
    emitted_session_changed_to_char: bool,
) {
    let trigger = TransitionTrigger::from_kind(spec.kind);
    if !spec.emit_system_wide_effect_stop
        || !policy_matches(
            TeardownSubsystem::SystemWideEffects,
            trigger,
            TeardownHookAction::EmitDepartingSystemWideEffectsStopAndSyncInfo,
        )
    {
        return;
    }
    let is_logout = matches!(spec.kind, TransitionKind::Logout) || spec.reason == "logout";
    let (next_sys_opt, next_in_space, _, exited_space_scene) =
        compute_space_exit_state(departing, updated_ch, is_logout);
    if !exited_space_scene {
        return;
    }
    let already_synced = spec.emit_on_session_changed
        && emitted_session_changed_to_char
        && !diff.is_empty()
        && diff.keys().any(|k| {
            matches!(
                k.as_str(),
                "shipid" | "solarsystemid" | "solarsystemid2" | "stationid" | "structureid"
            )
        });
    if !already_synced {
        crate::services::notify_emitters_tournaments_anomalies::emit_update_system_wide_effects_info(
            ctx, spec.char_id, next_sys_opt, next_in_space,
        );
    }
}

/// Executes the full declarative `TransitionTrigger::Disconnect` teardown pipeline.
pub fn run_disconnect_teardown_sync(
    ctx: &ServerContext,
    char_id: i64,
    session_opt: Option<&SessionState>,
) {
    use TeardownHookAction as A;
    use TeardownSubsystem as S;

    let pre_ch = ctx.db.get_character(char_id).ok();
    let departing = departing_from_sources(session_opt, pre_ch.as_ref(), true, true);

    let dc = TransitionTrigger::Disconnect;
    if policy_matches(S::TradeSessions, dc, A::CancelOpenTrades) {
        crate::services::wallet_services_ext::cancel_open_trades_for_character(ctx, char_id);
    }
    if policy_matches(S::SystemWideEffects, dc, A::EmitDepartingSystemWideEffectsStopAndSyncInfo)
        && departing.in_space
    {
        emit_departing_system_wide_stops_if_exited(ctx, char_id, &departing, true);
        crate::services::notify_emitters_tournaments_anomalies::emit_update_system_wide_effects_info(
            ctx, char_id, None, false,
        );
    }
    if policy_matches(S::DisconnectPresenceAndControl, dc, A::RunDisconnectPresenceAndControlCleanup)
    {
        let held_ship = crate::sp6_lifecycle::handle_character_logoff_or_disconnect_sync(ctx, char_id);
        if policy_matches(S::CloakAndTether, dc, A::ClearCloakAndWarpSettle)
            && held_ship.is_none()
            && departing.in_space
            && departing.ship_id > 0
        {
            clear_ship_cloak_and_warp_settle(ctx, &departing);
        }
        if policy_matches(S::SelfDestructTimer, dc, A::AbortSelfDestructTimer)
            && held_ship.is_none()
            && departing.ship_id > 0
        {
            crate::sp6_lifecycle::abort_self_destruct_for_ship(ctx, departing.ship_id);
        }
        if policy_matches(S::SafeLogoffState, dc, A::ClearSafeLogoffSilent) {
            crate::sp6_lifecycle::clear_safe_logoff(&ctx.db, char_id);
        }
        if policy_matches(S::ConcordResponse, dc, A::CleanupCombatStorageAndSyncConcord)
            && departing.in_space
        {
            prune_alsc_shipping_storage(ctx, char_id);
        }
        crate::services::character_skills::notify_watchlist_presence(ctx, char_id, false);
        crate::services::fleet_social::fleet_social_notify::on_character_logout(ctx, char_id);
        crate::services::fleet_social::fleet_social_notify::handle_character_disconnect(ctx, char_id);
        crate::services::fleet_social::revoke_xmpp_auth_token(ctx, char_id);
        if let Ok(mut cn) = ctx.systems.cluster_nodes.write() {
            cn.char_rebased_system_gen.remove(&char_id);
        }
    }
    if policy_matches(S::BoundObjects, dc, A::InvalidateScopedBoundObjects) {
        ctx.invalidate_bound_objects_for_session_transition(char_id, 0, None, "logout");
    }

    ctx.scanners_superweapons.clear_hacking_session_for_char(char_id);
    ctx.clear_notifications_for_char(char_id);
}

#[cfg(test)]
#[path = "teardown_registry_tests.rs"]
mod tests;
