//! `space_warp_jump` — Warp navigation, Stargate jumps, Docking/Undocking, and Session-Change Timers (`SP-3`, `SP-4`, `SP-5`).

#![forbid(unsafe_code)]

use crate::router::{BoundObjectEntry, ServerContext};
use eve_destiny::MovementMode;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::attr_ids;
use glam::DVec3;
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

use super::{
    check_session_change_timer_active, compute_pilot_warp_speed_au_s, record_session_change_timer,
    sync_ship_cloak_and_mjfg_from_fitting, wormhole_item_id_for_signature, AU_IN_METERS,
};
pub use crate::services::space_destiny::{
    break_location_change_invulnerability, grant_location_change_invulnerability, is_location_change_invulnerable,
};

const BEYONCE_SVC: &str = "beyonce";

/// Wall-clock transition cooldown duration (`7,000 ms`) returned in `[boundID, readyAt]` (`#1080`, `#1085`).
pub(crate) const TRANSITION_COOLDOWN_MS: i64 = 7_000;

/// Computes wall-clock `readyAt` FILETIME ticks (`now + 7,000 ms`) independent of TiDi (`#1080`, `#1085`).
pub(crate) fn transition_ready_at_filetime_ticks(ctx: &ServerContext) -> i64 {
    ctx.now_machonet_wallclock_filetime_ticks()
        .saturating_add(TRANSITION_COOLDOWN_MS * 10_000)
}

/// Zarzakh solar system ID (mirrors the V24 client constant `solarSystemZarzakh`).
pub(crate) const SOLAR_SYSTEM_ZARZAKH: u32 = 30_100_000;
/// Zarzakh entry-gate lock duration: 6 hours (EVE wiki `Zarzakh.md`).
const ZARZAKH_GATE_LOCK_SECS: u64 = 21_600;

#[cfg(any(test, feature = "testkit"))]
fn set_char_hook_in<T>(
    slot: &OnceLock<Mutex<HashMap<(u64, i64), T>>>,
    ctx: &ServerContext,
    char_id: i64,
    hook: Option<T>,
) {
    let map = slot.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = map.lock().unwrap_or_else(|e| e.into_inner());
    let key = (ctx.instance_id, char_id);
    match hook {
        Some(h) => {
            guard.insert(key, h);
        }
        None => {
            guard.remove(&key);
        }
    }
}

#[cfg(any(test, feature = "testkit"))]
fn take_char_hook_from<T>(
    slot: &OnceLock<Mutex<HashMap<(u64, i64), T>>>,
    ctx: &ServerContext,
    char_id: i64,
) -> Option<T> {
    let map = slot.get()?;
    let mut guard = map.lock().unwrap_or_else(|e| e.into_inner());
    guard.remove(&(ctx.instance_id, char_id))
}

#[cfg(any(test, feature = "testkit"))]
pub type PreTakeBallHook = Box<
    dyn FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
        + Send
        + 'static,
>;

#[cfg(any(test, feature = "testkit"))]
static PRE_TAKE_BALL_HOOKS: OnceLock<Mutex<HashMap<(u64, i64), PreTakeBallHook>>> =
    OnceLock::new();

#[cfg(test)]
type PostZarzakhTollHook = Box<dyn FnOnce() + Send + 'static>;

#[cfg(test)]
static POST_ZARZAKH_TOLL_HOOKS: OnceLock<Mutex<HashMap<(u64, i64), PostZarzakhTollHook>>> =
    OnceLock::new();

#[cfg(test)]
static POST_TAKE_BALL_HOOKS: OnceLock<Mutex<HashMap<(u64, i64), PreTakeBallHook>>> =
    OnceLock::new();

/// Installs or clears a one-shot test barrier immediately before `old_handle.take_ball`
/// inside `handle_cmd_stargate_jump`, scoped by `(ctx.instance_id, char_id)`.
#[cfg(any(test, feature = "testkit"))]
pub fn set_pre_take_ball_hook_for_test(
    ctx: &ServerContext,
    char_id: i64,
    hook: Option<PreTakeBallHook>,
) {
    set_char_hook_in(&PRE_TAKE_BALL_HOOKS, ctx, char_id, hook);
}

#[cfg(any(test, feature = "testkit"))]
async fn run_pre_take_ball_hook_for_test(ctx: &ServerContext, char_id: i64) {
    if let Some(h) = take_char_hook_from(&PRE_TAKE_BALL_HOOKS, ctx, char_id) {
        h().await;
    }
}

#[cfg(any(test, feature = "testkit"))]
static POST_DEST_SPAWN_HOOKS: OnceLock<Mutex<HashMap<(u64, i64), PreTakeBallHook>>> =
    OnceLock::new();

/// Installs or clears a one-shot test barrier immediately after `new_handle.spawn_ball_with_meta_tagged`
/// succeeds and before `rollback_guard.disarm()` inside `handle_cmd_stargate_jump`, scoped by
/// `(ctx.instance_id, char_id)`.
#[cfg(any(test, feature = "testkit"))]
pub fn set_post_dest_spawn_hook_for_test(
    ctx: &ServerContext,
    char_id: i64,
    hook: Option<PreTakeBallHook>,
) {
    set_char_hook_in(&POST_DEST_SPAWN_HOOKS, ctx, char_id, hook);
}

#[cfg(any(test, feature = "testkit"))]
async fn run_post_dest_spawn_hook_for_test(ctx: &ServerContext, char_id: i64) {
    if let Some(h) = take_char_hook_from(&POST_DEST_SPAWN_HOOKS, ctx, char_id) {
        h().await;
    }
}

#[cfg(test)]
fn set_post_zarzakh_toll_hook_for_test(
    ctx: &ServerContext,
    char_id: i64,
    hook: Option<PostZarzakhTollHook>,
) {
    set_char_hook_in(&POST_ZARZAKH_TOLL_HOOKS, ctx, char_id, hook);
}

#[cfg(test)]
fn run_post_zarzakh_toll_hook_for_test(ctx: &ServerContext, char_id: i64) {
    if let Some(h) = take_char_hook_from(&POST_ZARZAKH_TOLL_HOOKS, ctx, char_id) {
        h();
    }
}

#[cfg(test)]
fn set_post_take_ball_hook_for_test(
    ctx: &ServerContext,
    char_id: i64,
    hook: Option<PreTakeBallHook>,
) {
    set_char_hook_in(&POST_TAKE_BALL_HOOKS, ctx, char_id, hook);
}

#[cfg(test)]
async fn run_post_take_ball_hook_for_test(ctx: &ServerContext, char_id: i64) {
    if let Some(h) = take_char_hook_from(&POST_TAKE_BALL_HOOKS, ctx, char_id) {
        h().await;
    }
}

fn session_owns_online_lease(ctx: &ServerContext, session: &SessionState, char_id: i64) -> bool {
    let Some(uid) = session.user_id else {
        return false;
    };
    let now_unix = ctx.now_unix_secs();
    if ctx
        .db
        .check_session_revoked(uid, session.session_id, now_unix)
        .is_some()
    {
        return false;
    }
    ctx.db
        .get_online_session(uid)
        .is_some_and(|os| {
            os.session_id == session.session_id
                && os.character_id.is_none_or(|cid| cid == char_id)
        })
}

fn check_jump_session_lease(
    ctx: &ServerContext,
    session: &SessionState,
    char_id: i64,
    entry_had_online_lease: bool,
) -> Result<(), BridgeRpcError> {
    let Some(uid) = session.user_id else {
        if entry_had_online_lease {
            return Err(crate::router::binding::session_kicked_error(
                "Session lost account binding before stargate handoff committed",
            ));
        }
        return Ok(());
    };
    let now_unix = ctx.now_unix_secs();
    if let Some((code, message)) =
        ctx.db
            .check_session_revoked(uid, session.session_id, now_unix)
    {
        return Err(crate::router::binding::session_revoked_error(
            code, message,
        ));
    }
    let still_valid = match ctx.db.get_online_session(uid) {
        Some(os) => {
            os.session_id == session.session_id
                && os.character_id.is_none_or(|cid| cid == char_id)
        }
        None => !entry_had_online_lease,
    };
    if !still_valid {
        return Err(crate::router::binding::session_kicked_error(
            "Session displaced or closed before stargate handoff committed",
        ));
    }
    Ok(())
}

/// RAII guard owning a [`crate::system_actor::TakenBall`] removed from a source solar system
/// during a stargate jump or docking handoff (`#1237` / `HP-06`).
///
/// While armed, dropping the guard (due to an early return, panic, or async future
/// cancellation) automatically enqueues [`crate::system_actor::SystemCommand::RestoreBallIfAbsent`]
/// into `sys_handle` without blocking the executor, so the actor atomically checks ball
/// absence before restoring. Call [`Self::restore`] on explicit error paths to await
/// restoration before returning `Err`, or [`Self::disarm`] once the handoff has permanently
/// committed.
pub(crate) struct TakenBallGuard {
    sys_handle: crate::system_actor::SystemHandle,
    taken: Option<crate::system_actor::TakenBall>,
}

impl TakenBallGuard {
    pub(crate) fn new(
        sys_handle: crate::system_actor::SystemHandle,
        taken: crate::system_actor::TakenBall,
    ) -> Self {
        Self {
            sys_handle,
            taken: Some(taken),
        }
    }

    pub(crate) fn taken(&self) -> Option<&crate::system_actor::TakenBall> {
        self.taken.as_ref()
    }

    /// Disarms the guard after the handoff has permanently committed and returns the owned
    /// [`crate::system_actor::TakenBall`].
    pub(crate) fn disarm(mut self) -> crate::system_actor::TakenBall {
        self.taken
            .take()
            .expect("TakenBallGuard::disarm called on already-disarmed guard")
    }

    /// Explicitly awaits atomic restoration of the taken ball into the source system if still
    /// absent, disarming the guard.
    pub(crate) async fn restore(mut self) {
        if let Some(taken) = self.taken.take() {
            let _ = self.sys_handle.restore_ball_if_absent(taken).await;
        }
    }
}

impl Drop for TakenBallGuard {
    fn drop(&mut self) {
        let Some(taken) = self.taken.take() else {
            return;
        };
        let ship_id = taken.ball.id;
        let sys_id = self.sys_handle.solar_system_id;
        tracing::warn!(
            ship_id,
            sys_id,
            "TakenBallGuard dropped while armed; restoring ball if absent to source solar system"
        );
        let (tx, _rx) = tokio::sync::oneshot::channel();
        match self
            .sys_handle
            .cmd_tx
            .try_send(crate::system_actor::SystemCommand::RestoreBallIfAbsent { taken, reply: tx })
        {
            Ok(()) => {}
            Err(tokio::sync::mpsc::error::TrySendError::Full(
                crate::system_actor::SystemCommand::RestoreBallIfAbsent { taken, .. },
            )) => {
                if let Ok(rt) = tokio::runtime::Handle::try_current() {
                    let sys_handle = self.sys_handle.clone();
                    rt.spawn(async move {
                        let _ = sys_handle.restore_ball_if_absent(taken).await;
                    });
                } else {
                    tracing::error!(
                        ship_id,
                        sys_id,
                        "TakenBallGuard drop could not restore ball: mailbox full and no Tokio runtime"
                    );
                }
            }
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {}
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                tracing::warn!(
                    ship_id,
                    sys_id,
                    "TakenBallGuard drop skipped restoration: source solar system actor stopped"
                );
            }
        }
    }
}

static NEXT_STARGATE_SPAWN_TOKEN: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(1);

fn next_stargate_spawn_token() -> u64 {
    NEXT_STARGATE_SPAWN_TOKEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

fn enqueue_remove_ball_nonblocking(
    sys_handle: &crate::system_actor::SystemHandle,
    ball_id: eve_destiny::BallId,
    spawn_token: u64,
) {
    let sys_id = sys_handle.solar_system_id;
    let (tx, _rx) = tokio::sync::oneshot::channel();
    match sys_handle.cmd_tx.try_send(
        crate::system_actor::SystemCommand::RemoveBallIfToken {
            ball_id,
            spawn_token,
            reply: tx,
        },
    ) {
        Ok(()) => {}
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
            if let Ok(rt) = tokio::runtime::Handle::try_current() {
                let sys_handle = sys_handle.clone();
                rt.spawn(async move {
                    let _ = sys_handle.remove_ball_if_token(ball_id, spawn_token).await;
                });
            } else {
                tracing::error!(
                    ball_id,
                    sys_id,
                    spawn_token,
                    "StargateRollbackGuard drop could not remove destination ball: mailbox full and no Tokio runtime"
                );
            }
        }
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {}
    }
}

fn refund_zarzakh_gate_toll_in_snap(
    s: &mut eve_db::WorldStateSnapshot,
    char_id: i64,
    debited_cents: Option<eve_db::IskCents>,
) -> Option<eve_db::IskCents> {
    let refund_cents = debited_cents.filter(|c| c.0 > 0)?;
    let c = s.characters.get_mut(&char_id)?;
    let new_bal = c.balance.checked_add(refund_cents)?;
    c.balance = new_bal;
    s.next_entity_id += 1;
    let j_id = s.next_entity_id;
    s.wallet_journal.push(eve_db::WalletJournalEntry {
        entry_id: j_id,
        character_id: char_id,
        entry_type: "zarzakh_gate_toll_refund".into(),
        amount: refund_cents,
        balance_after: new_bal,
        reason: "Refund Zarzakh stargate transit toll after aborted stargate jump".into(),
    });
    Some(new_bal)
}

fn refund_zarzakh_gate_toll(
    ctx: &ServerContext,
    char_id: i64,
    debited_cents: Option<eve_db::IskCents>,
) {
    let Some(refund_cents) = debited_cents.filter(|c| c.0 > 0) else {
        return;
    };
    let bal_after_res: Result<Option<eve_db::IskCents>, String> =
        ctx.db.try_with_mut_snapshot(|s| {
            Ok(refund_zarzakh_gate_toll_in_snap(s, char_id, Some(refund_cents)))
        });
    match bal_after_res {
        Ok(Some(bal_after)) => {
            ctx.notify_wallet_balance(false, char_id, 1, bal_after);
        }
        Ok(None) => {}
        Err(err) => {
            tracing::error!(
                char_id,
                error = %err,
                "refund_zarzakh_gate_toll transaction failed"
            );
        }
    }
}

pub(crate) async fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("beyonce", "CmdWarpToStuff") | ("beyonce", "CmdWarpToStuffAutopilot") => Some(
            Box::pin(handle_cmd_warp_to_stuff(
                ctx,
                session,
                bound_entry,
                args,
                kwargs,
                method == "CmdWarpToStuffAutopilot",
            ))
            .await,
        ),
        ("beyonce", "CmdStargateJump") | ("beyonce", "StargateJump") => {
            Some(Box::pin(handle_cmd_stargate_jump(ctx, session, bound_entry, args, kwargs)).await)
        }
        // parity: plan:SP-4, plan:SP-5, feat:navigation.dock-station, feat:navigation.dock-structure, feat:navigation.undock, feat:navigation.session-change-timer-triggers
        // parity: rpc:beyonce.SetSessionCheck, rpc:beyonce.CmdDock
        ("beyonce", "CmdDock") => Some(Box::pin(handle_dock(ctx, session, bound_entry, args)).await),
        ("beyonce", "Undock") => Box::pin(handle_undock(ctx, session, args, kwargs)).await,
        ("beyonce", "SetSessionCheck") => {
            Some(Box::pin(crate::services::nav_warp_interdiction::handle_set_session_check(ctx, session, args)).await)
        }
        _ => None,
    }
}

#[path = "space_warp_jump_helpers.rs"]
mod space_warp_jump_helpers;
use space_warp_jump_helpers::{
    bookmark_expired_err, emit_structure_dock_notifications, handle_undock,
    resolve_bound_or_session_sys as resolve_bound_or_session_sys_inner,
};

fn resolve_bound_or_session_sys(
    bound_entry: Option<&BoundObjectEntry>,
    session: &SessionState,
) -> Result<u32, BridgeRpcError> {
    resolve_bound_or_session_sys_inner(bound_entry, session, BEYONCE_SVC)
}

#[inline(never)]
fn record_warp_progression(ctx: &ServerContext, char_id: i64) {
    crate::quasar_gateway::isolate_side_effect(
        "space_warp_jump.CmdWarpToStuff.cosmetics",
        || {
            let already_warped = ctx.db.with_snapshot(|snap| {
                crate::services::cosmetics_ui_ops::load_cosmetics_ui_state(snap)
                    .characters_warped
                    .contains(&char_id)
            });
            if !already_warped {
                ctx.db.with_mut_snapshot(|snap| {
                    let mut ext = crate::services::cosmetics_ui_ops::load_cosmetics_ui_state(snap);
                    if !ext.characters_warped.contains(&char_id) {
                        ext.characters_warped.push(char_id);
                        crate::services::cosmetics_ui_ops::save_cosmetics_ui_state(snap, &ext);
                    }
                });
            }
        },
    );
    ctx.quasar_gateway.progression.achievements.on_gameplay_event_by_name(
        char_id as u32,
        &["timesWarped"],
        1,
        ctx.now_unix(),
        &ctx.quasar_gateway,
    );
    crate::services::operations_manager_helpers::record_server_operation_event(
        ctx,
        char_id,
        "warpToLocation",
        &[],
        1,
    );
}

fn resolve_bookmark_warp_pos(
    ctx: &ServerContext,
    session: &SessionState,
    char_id: i64,
    sys_id: u32,
    target_id: i64,
) -> Result<DVec3, BridgeRpcError> {
    if sys_id == SOLAR_SYSTEM_ZARZAKH {
        return Err(BridgeRpcError::from_user_key(
            eve_proto::UserErrorKey::DunBlacklistCannotWarp,
        ));
    }
    let now_unix = ctx.now_unix();
    if crate::services::fleet_social::is_bookmark_expired(ctx, target_id, now_unix) {
        return Err(bookmark_expired_err(target_id));
    }
    let bm_opt = ctx.db.with_snapshot(|snap| {
        let b = snap.bookmarks.get(&target_id)?;
        if b.solar_system_id != sys_id {
            return None;
        }
        let can_use = b.owner_id == char_id
            || b.folder_id.is_some_and(|fid| {
                snap.bookmark_folders.get(&fid).is_some_and(|f| {
                    crate::services::fleet_social::fleet_social_access_group_bookmarks::folder_access_level(
                        snap, session, char_id, f,
                    ) >= crate::services::fleet_social::fleet_social_access_group_bookmarks::AccessLevel::Use
                })
            });
        if can_use {
            Some(DVec3::new(b.x, b.y, b.z))
        } else {
            None
        }
    });
    bm_opt.ok_or_else(|| BridgeRpcError {
        code: "BookmarkNotFound".into(),
        message: format!("Bookmark {target_id} not found"),
        client_error: None,
    })
}

async fn handle_cmd_warp_to_stuff(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
    is_autopilot: bool,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    if args.first().and_then(|a| a.as_str()) == Some("bookmark") {
        let bm_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
        if crate::services::fleet_social::is_bookmark_expired(ctx, bm_id, ctx.now_unix()) {
            return Err(bookmark_expired_err(bm_id));
        }
    }
    if let Some(ship_id) = session.ship_id {
        crate::services::structure_control_ops::reject_if_controlling_structure(ctx, ship_id, "warp")?;
    }
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(BridgeRpcError {
            code: "NotInSpace".into(),
            message: "Cannot warp while docked in station or structure".into(),
            client_error: None,
        });
    }
    let sys_id = resolve_bound_or_session_sys(bound_entry, session)?;
    let Some(ship_id) = session.ship_id else {
        return Err(BridgeRpcError {
            code: "NoActiveShip".into(),
            message: "Session has no active ship".into(),
            client_error: None,
        });
    };
    crate::services::tourney_service::check_tourney_warp_restricted(ctx, sys_id, char_id)?;
    crate::services::tourney_service::check_tourney_move_restricted(ctx, sys_id, char_id)?;
    let is_fleet_warp_kw = kwargs
        .get("fleet")
        .is_some_and(|v| matches!(v, PyRep::Bool(true)) || v.as_i64().is_some_and(|i| i != 0) || v.as_str().is_some())
        || kwargs.contains_key("formationID");
    if is_fleet_warp_kw && !is_autopilot && session.fleet_id.is_some() {
        let mut wf_args = Vec::new();
        if args.first().and_then(|a| a.as_str()).is_some() {
            if let Some(tid) = args.get(1) {
                wf_args.push(tid.clone());
            }
            if let Some(mr) = args.get(2) {
                wf_args.push(mr.clone());
            }
        } else {
            wf_args.extend_from_slice(args);
        }
        if let Some(res) =
            crate::services::fleet_social::fleet_social_fleet_ops::handle_fleet_warp(ctx, session, &wf_args, kwargs).await
        {
            if let Ok(ref rep) = res {
                let caller_warped = rep
                    .dict_get("warpedMemberIDs")
                    .and_then(|v| match v {
                        PyRep::List(items) | PyRep::Tuple(items) => {
                            Some(items.iter().any(|item| item.as_i64() == Some(char_id)))
                        }
                        _ => None,
                    })
                    .unwrap_or(false);
                if caller_warped {
                    break_location_change_invulnerability(ctx, char_id);
                    let _ = crate::sp6_lifecycle::maybe_abort_safe_logoff_on_action(
                        ctx,
                        char_id,
                        "SAFE_LOGOFF_ABORT_MOVING",
                    );
                }
            }
            return res;
        }
    }
    let mjd_handle = ctx.systems.get_or_spawn(sys_id);
    crate::services::nav_warp_interdiction::validate_pre_warp_common(
        ctx,
        char_id,
        ship_id,
        sys_id,
        &mjd_handle,
    )
    .await?;
    let (is_stuff_call, stuff_type, target_id, target_str_opt, raw_min_range) =
        if let Some(s) = args.first().and_then(|a| a.as_str()) {
            (
                true,
                s,
                args.get(1).and_then(|a| a.as_i64()).unwrap_or(0),
                args.get(1).and_then(|a| a.as_str()),
                args.get(2).and_then(|a| a.as_f64()),
            )
        } else if is_autopilot {
            (
                true,
                "item",
                args.first().and_then(|a| a.as_i64()).unwrap_or(0),
                None,
                args.get(1).and_then(|a| a.as_f64()),
            )
        } else {
            (
                false,
                "item",
                0,
                None,
                None,
            )
        };
    if !is_stuff_call {
        return Err(BridgeRpcError {
            code: "InvalidWarpDestination".into(),
            message: "Invalid warp destination".into(),
            client_error: None,
        });
    }
    if target_id <= 0 && target_str_opt.is_none() {
        return Err(BridgeRpcError {
            code: "InvalidTarget".into(),
            message: "Warp target ID must be a positive integer".into(),
            client_error: None,
        });
    }
    if target_id == ship_id {
        return Err(BridgeRpcError {
            code: "InvalidTarget".into(),
            message: "Cannot warp to own ship".into(),
            client_error: None,
        });
    }
    let default_min_range = if is_autopilot { 15_000.0 } else { 0.0 };
    let candidate_min_range = raw_min_range.or_else(|| {
        kwargs
            .get("minRange")
            .or_else(|| kwargs.get("warpInDistance"))
            .or_else(|| kwargs.get("range"))
            .and_then(|v| v.as_f64())
    });
    if let Some(r) = candidate_min_range {
        if !r.is_finite() {
            return Err(BridgeRpcError {
                code: "InvalidArgument".into(),
                message: "Warp range must be finite".into(),
                client_error: None,
            });
        }
    }
    let min_range = candidate_min_range
        .unwrap_or(default_min_range)
        .clamp(0.0, 100_000.0);
    tracing::info!(
        char_id,
        sys_id,
        ship_id,
        stuff_type,
        target_id,
        ?target_str_opt,
        min_range,
        "CmdWarpToStuff request"
    );
    let handle = ctx.systems.get_or_spawn(sys_id);
    let Some((_, current_ball)) = (match handle.get_ball_snapshot(ship_id).await {
        Ok(v) => v,
        Err(e) => {
            return Err(BridgeRpcError {
                code: "DestinyCommandFailed".into(),
                message: e,
                client_error: None,
            })
        }
    }) else {
        return Err(BridgeRpcError {
            code: "ShipNotInSpace".into(),
            message: "Active ship is not spawned in Ballpark".into(),
            client_error: None,
        });
    };

    // Resolve read-only bookmark targets (Zarzakh ban, expiry, ACL lookup) before ship movement
    // validation so missing/unauthorized bookmarks report BookmarkNotFound instead of WarpScrambled.
    let pre_resolved_bookmark_pos = if stuff_type == "bookmark" {
        Some(resolve_bookmark_warp_pos(
            ctx, session, char_id, sys_id, target_id,
        )?)
    } else {
        None
    };

    let validated = ctx
        .validate_ship_movement(
            char_id,
            ship_id,
            crate::services::space_nav_movement_guard::MovementIntent::Warp { sys_id },
        )
        .await?;
    let is_covert_cloaked = validated.is_covert_cloaked;

    let needs_pve_seed = matches!(
        stuff_type,
        "epinstance" | "ep_instance" | "site" | "anomaly" | "dungeon" | "location"
    ) || target_str_opt.is_some_and(|s| {
        s.starts_with("PVE-ANOM-")
            || s.starts_with("EXP-")
            || s.starts_with("DED-")
            || s.starts_with("DSG-")
            || s.starts_with("DAT-")
            || s.starts_with("REL-")
            || s.starts_with("GHO-")
            || s.starts_with("SLC-")
    });
    if needs_pve_seed {
        crate::services::pve_exploration::ensure_system_pve_sites_seeded(ctx, sys_id).await;
    }

    let mut resolved_pos: Option<DVec3> = None;
    let mut fleet_ship_ball_id: Option<i64> = None;
    match stuff_type {
        "bookmark" => {
            resolved_pos = pre_resolved_bookmark_pos;
        }
        "mission" | "dungeon" | "location" | "epinstance" | "ep_instance" | "site" | "anomaly" => {
            let deadspace_type = if stuff_type == "ep_instance" {
                "epinstance"
            } else {
                stuff_type
            };
            resolved_pos =
                crate::services::pve_dungeon_runtime::resolve_deadspace_or_escalation_warp_target(
                    ctx,
                    char_id,
                    sys_id,
                    deadspace_type,
                    target_id,
                    target_str_opt,
                )?;
            if resolved_pos.is_none() && target_str_opt.is_some() {
                resolved_pos = ctx.db.with_snapshot(|snap| {
                    if let Some(anoms) = snap.cosmic_anomalies.get(&sys_id) {
                        for a in anoms {
                            if target_str_opt == Some(a.anomaly_id.as_str()) && !a.cleared {
                                return Some(DVec3::from_array(a.position));
                            }
                        }
                    }
                    if let Some(sigs) = snap.cosmic_signatures.get(&sys_id) {
                        for s in sigs {
                            let eff_strength =
                                ctx.effective_signature_signal_strength(char_id, sys_id, s);
                            if target_str_opt == Some(s.signature_id.as_str())
                                && eff_strength >= 1.0
                                && !s.collapsed
                            {
                                return Some(
                                    crate::services::space_destiny::space_pochven_wormholes::pochven_wormhole_ball_position(
                                        &ctx.sde, s,
                                    ),
                                );
                            }
                        }
                    }
                    None
                });
            }
            if resolved_pos.is_none() && !matches!(stuff_type, "epinstance" | "ep_instance") {
                resolved_pos = ctx.db.with_snapshot(|snap| {
                    if snap.active_missions.values().any(|m| {
                        m.character_id == char_id
                            && m.solar_system_id == sys_id
                            && m.state == "Accepted"
                            && (target_id == m.mission_id as i64 || target_id == m.agent_id as i64)
                    }) {
                        return Some(DVec3::new(250_000_000.0, 0.0, 250_000_000.0));
                    }
                    None
                });
            }
        }
        "char" | "fleet" => {
            if let Some(fid) = session.fleet_id {
                fleet_ship_ball_id = ctx.db.with_snapshot(|snap| {
                    let is_member = snap
                        .fleets
                        .get(&fid)
                        .map(|f| f.members.contains(&target_id))
                        .unwrap_or(false);
                    if is_member && target_id != char_id {
                        snap.characters
                            .get(&target_id)
                            .filter(|c| c.solar_system_id == sys_id && c.station_id.is_none())
                            .map(|c| c.active_ship_id)
                    } else {
                        None
                    }
                });
            }
        }
        _ => {
            if let Some(sys_rec) = ctx.sde.solar_systems.get(&sys_id) {
                resolved_pos = sys_rec
                    .stations
                    .iter()
                    .find(|st| st.station_id == target_id)
                    .map(|st| st.position);
            }
            if resolved_pos.is_none() {
                resolved_pos = ctx
                    .sde
                    .jumps
                    .get(&target_id)
                    .filter(|j| j.from_solar_system_id == sys_id)
                    .map(|j| j.position);
            }
            if resolved_pos.is_none() {
                resolved_pos = ctx
                    .sde
                    .celestials_in_system(sys_id)
                    .into_iter()
                    .find(|c| c.celestial_id == target_id)
                    .map(|c| c.position);
            }
            if resolved_pos.is_none() {
                let has_scanned = ctx.has_scanned_signature(char_id, sys_id, target_id);
                resolved_pos = ctx.db.with_snapshot(|snap| {
                    if let Some(st) = snap.structures.get(&target_id) {
                        if st.solar_system_id == sys_id && st.state != "Destroyed" {
                            return Some(DVec3::from_array(st.position));
                        }
                    }
                    if target_id == 750_000_001 && has_scanned {
                        return snap
                            .system_states
                            .get(&sys_id)
                            .and_then(|s| s.signature_position)
                            .map(DVec3::from_array);
                    }
                    if let Some(sigs) = snap.cosmic_signatures.get(&sys_id) {
                        for s in sigs {
                            let wh_id = wormhole_item_id_for_signature(sys_id, &s.signature_id);
                            let eff_strength =
                                ctx.effective_signature_signal_strength(char_id, sys_id, s);
                            if ((wh_id == target_id && (has_scanned || eff_strength >= 1.0))
                                || (target_str_opt == Some(s.signature_id.as_str())
                                    && eff_strength >= 1.0))
                                && !s.collapsed
                            {
                                let base_pos = crate::services::space_destiny::space_pochven_wormholes::pochven_wormhole_ball_position(&ctx.sde, s);
                                if crate::services::wormhole_lifecycle_ops::is_drifter_wormhole(&ctx.sde, s) {
                                    return Some(base_pos + DVec3::new(80_000.0, 0.0, 0.0));
                                }
                                return Some(base_pos);
                            }
                        }
                    }
                    if let Some(anoms) = snap.cosmic_anomalies.get(&sys_id) {
                        for a in anoms {
                            if target_str_opt == Some(a.anomaly_id.as_str()) && !a.cleared {
                                return Some(DVec3::from_array(a.position));
                            }
                        }
                    }
                    None
                });
            }
        }
    }
    if resolved_pos.is_none() {
        if let Some(fb_id) = fleet_ship_ball_id {
            if let Ok(Some((_, fb))) = handle.get_ball_snapshot(fb_id).await {
                resolved_pos = Some(fb.position);
            }
        }
    }
    let is_unresolved_sig_ball = target_id > 0
        && !ctx.has_scanned_signature(char_id, sys_id, target_id)
        && ctx.db.with_snapshot(|snap| {
            snap.cosmic_signatures.get(&sys_id).is_some_and(|sigs| {
                sigs.iter().any(|s| {
                    wormhole_item_id_for_signature(sys_id, &s.signature_id) == target_id
                        && ctx.effective_signature_signal_strength(char_id, sys_id, s) < 1.0
                })
            })
        });
    if resolved_pos.is_none() && target_id > 0 && target_id != ship_id && !is_unresolved_sig_ball {
        if let Ok(Some((_, tb))) = handle.get_ball_snapshot(target_id).await {
            let is_asteroid = ctx
                .sde
                .types
                .get(&tb.type_id)
                .is_some_and(|t| t.category_id == 25);
            if matches!(tb.mode, MovementMode::Rigid) || is_asteroid {
                let sys_wh_class = crate::services::wormhole_lifecycle_ops::system_wormhole_profile_sde(&ctx.sde, sys_id).0;
                let is_drifter_ball = matches!(tb.type_id, 34_494 | 35_650..=35_654)
                    || ((14..=18).contains(&sys_wh_class)
                        && ctx.sde.types.get(&tb.type_id).is_some_and(|t| t.group_id == 988));
                resolved_pos = Some(if is_drifter_ball {
                    tb.position + DVec3::new(80_000.0, 0.0, 0.0)
                } else {
                    tb.position
                });
            }
        }
    }
    if let Some(dest_pos) = resolved_pos.filter(|_| stuff_type != "bookmark") {
        crate::services::pve_dungeon_runtime_rpc::check_warp_target_blocked_by_gated_dungeon_room(
            ctx,
            sys_id,
            dest_pos,
            (target_id > 0).then_some(target_id),
        )?;
    } else if target_id > 0 && stuff_type != "bookmark" {
        if let Ok(Some((_, tb))) = handle.get_ball_snapshot(target_id).await {
            crate::services::pve_dungeon_runtime_rpc::check_warp_target_blocked_by_gated_dungeon_room(
                ctx,
                sys_id,
                tb.position,
                Some(target_id),
            )?;
        }
    }
    let Some(anchor_pos) = resolved_pos else {
        return Err(BridgeRpcError {
            code: "InvalidWarpDestination".into(),
            message: format!(
                "Warp target ({stuff_type}, {target_id}) is not a valid warpable object in system {sys_id}"
            ),
            client_error: None,
        });
    };
    crate::services::nav_warp_interdiction::ensure_not_in_active_warp(&current_ball.mode)?;

    let preview_dir = (anchor_pos - current_ball.position).normalize_or_zero();
    let preview_dest = anchor_pos - preview_dir * min_range;
    crate::services::pve_dungeon_runtime::check_deadspace_warp_allowed(
        ctx,
        sys_id,
        current_ball.position.to_array(),
        anchor_pos.to_array(),
        preview_dest.to_array(),
    )?;
    crate::services::pve_ess_reserve_banks::check_ess_warp_destination_allowed(
        ctx,
        sys_id,
        anchor_pos,
        preview_dest,
    )
    .await?;

    let landing_pos = crate::services::nav_warp_interdiction::resolve_warp_landing_point(
        ctx,
        sys_id,
        target_id,
        current_ball.position,
        anchor_pos,
    );
    let landing_dist = current_ball.position.distance(landing_pos);

    let warp_dir = (landing_pos - current_ball.position).normalize_or_zero();
    let mut dest_pos = landing_pos - warp_dir * min_range;
    let requested_dist_m = current_ball.position.distance(dest_pos);
    let effective_warp_dist_m = (landing_dist - min_range).min(requested_dist_m);
    if effective_warp_dist_m < 150_000.0 {
        return Err(BridgeRpcError {
            code: "WarpDistanceTooShort".into(),
            message: format!(
                "Cannot warp {:.1} m (minimum warp distance is 150,000 m)",
                effective_warp_dist_m.max(0.0)
            ),
            client_error: None,
        });
    }

    let (base_warp_cap_need, sde_max_cap) = ctx
        .sde
        .types
        .get(&current_ball.type_id)
        .map(|t| {
            let wcn = t.default_attributes.get(&153).copied().unwrap_or(1.24e-6);
            let cap = t
                .default_attributes
                .get(&attr_ids::CAPACITOR_CAPACITY)
                .copied()
                .unwrap_or(250.0);
            (wcn, cap)
        })
        .unwrap_or((1.24e-6, 250.0));
    let wdo_lvl = ctx.db.with_snapshot(|snap| {
        snap.skills
            .get(&char_id)
            .and_then(|m| m.get(&3455))
            .map(|s| s.level.min(5))
            .unwrap_or(0)
    });
    let eff_warp_cap_need = base_warp_cap_need * (1.0 - 0.10 * (wdo_lvl as f64));
    let cap_per_au = eff_warp_cap_need * current_ball.mass;
    let cap_per_m = cap_per_au / AU_IN_METERS;
    let req_cap = cap_per_m * requested_dist_m;

    let mut cap_debit_update: Option<(f64, f64, f64, f64)> = None;
    if let Ok(Some(mut cs)) = handle.get_combat_state(ship_id).await {
        let mut repaired_max_cap = false;
        if (cs.max_capacitor - 250.0).abs() < 1e-6
            && (cs.capacitor - 250.0).abs() < 1e-6
            && sde_max_cap > 250.0
        {
            cs.max_capacitor = sde_max_cap;
            cs.capacitor = sde_max_cap;
            repaired_max_cap = true;
        }
        let old_cap = cs.capacitor;
        let pre_debit_cap = cs.capacitor;
        if cs.capacitor < req_cap {
            let affordable_dist_m = if cap_per_m > 0.0 {
                cs.capacitor / cap_per_m
            } else {
                requested_dist_m
            };
            if affordable_dist_m < 150_000.0 {
                return Err(BridgeRpcError {
                    code: "NotEnoughCapacitor".into(),
                    message: format!(
                        "Insufficient capacitor ({:.3} GJ) to initiate minimum 150 km warp (affordable distance {:.1} m)",
                        cs.capacitor, affordable_dist_m
                    ),
                    client_error: None,
                });
            }
            dest_pos = current_ball.position + warp_dir * affordable_dist_m;
            cs.capacitor = 0.0;
        } else {
            cs.capacitor = (cs.capacitor - req_cap).max(0.0);
        }
        if repaired_max_cap {
            let new_cap = cs.capacitor;
            let new_max_cap = cs.max_capacitor;
            let _ = handle
                .modify_combat_state(ship_id, move |live_cs| {
                    live_cs.max_capacitor = new_max_cap;
                    live_cs.capacitor = new_cap;
                })
                .await;
        } else {
            let _ = handle.set_capacitor(ship_id, cs.capacitor).await;
        }
        cap_debit_update = Some((cs.capacitor, cs.max_capacitor, old_cap, pre_debit_cap));
    }

    let warp_speed = compute_pilot_warp_speed_au_s(ctx, char_id, sys_id, current_ball.type_id);
    let mode = MovementMode::new_warp(current_ball.position, dest_pos, warp_speed);
    if is_covert_cloaked {
        // Ensure a stale `is_covert_cloak = false` flag on `CombatState` does not cause
        // `ballpark.set_movement_mode` to reject a verified active Covert Ops cloak module.
        let _ = handle
            .modify_combat_state(ship_id, |live_cs| {
                if live_cs.is_cloaked && !live_cs.is_covert_cloak {
                    live_cs.is_covert_cloak = true;
                }
            })
            .await;
    }
    let target_belt_id = ctx
        .sde
        .celestials_in_system(sys_id)
        .into_iter()
        .find(|c| c.group_id == 9 && (c.celestial_id == target_id || c.position.distance(dest_pos) <= 200_000.0))
        .map(|c| c.celestial_id);
    if target_belt_id.is_some() {
        let _ = handle.populate_asteroid_belt_entities().await;
    }
    // Snapshot active gate cloak and undock invulnerability read-only immediately before
    // `set_warp_movement_mode`, which clears them inside `Ballpark::set_mode` so `Ok(tick)`
    // can emit `OnJumpCloakCancelled` / `OnInvulnCancelled` without dropping them.
    let (had_gate_cloak, had_undock_invuln) = match handle.get_combat_state(ship_id).await {
        Ok(Some(live_cs)) => (
            !is_covert_cloaked
                && (live_cs.gate_cloak_remaining_ticks > 0
                    || (live_cs.is_cloaked && !live_cs.is_covert_cloak)),
            live_cs.undock_invulnerable_remaining_ticks > 0,
        ),
        _ => (false, false),
    };
    match handle
        .set_warp_movement_mode(
            ship_id,
            mode,
            crate::system_actor::WarpProvenance::PlayerOrFleetWarp,
        )
        .await
    {
        Ok(tick) => {
            if let Some((new_cap, max_cap, old_cap, _)) = cap_debit_update {
                let _ = ctx.notify_ship_capacitor_change(
                    char_id,
                    ship_id,
                    new_cap,
                    max_cap,
                    Some(old_cap),
                );
            }
            if had_gate_cloak || had_undock_invuln {
                crate::services::notify_emitters_space_dungeons::emit_warp_cloak_or_invuln_cancelled(
                    ctx,
                    char_id,
                    ship_id,
                    had_gate_cloak,
                    had_undock_invuln,
                );
            }
            // Run tether & fitting cloak reconciliation only after `ball.mode` is `MovementMode::Warp`
            // so `sync_tether` breaks tether cleanly without granting a +25% capacitor repair.
            sync_ship_cloak_and_mjfg_from_fitting(ctx, char_id, sys_id, ship_id).await;
            if stuff_type == "item" && target_id > 0 {
                let _ = ctx.systems.ensure_stargate_destination_loaded_for_intent(
                    sys_id,
                    target_id,
                    ctx.now_machonet_wallclock_ms(),
                );
            }
            // [plan:DG-4] Entering warp halts active non-warp module cycles (weapons, scrams, webs, mining lasers, etc.)
            crate::services::nav_warp_interdiction::on_warp_initiated_hooks(
                ctx, &handle, char_id, ship_id, sys_id,
            )
            .await;
            let _ = crate::sp6_lifecycle::spawn_mission_wave_on_pocket_entry_if_needed(
                ctx, char_id, sys_id, dest_pos,
            );
            crate::services::pve_dungeon_runtime::on_warp_start_leave_dungeon_if_needed(
                ctx, char_id, sys_id, current_ball.position.to_array(), dest_pos.to_array(),
            );
            crate::services::space_destiny::track_player_or_fleet_warp(ctx, None, ship_id, char_id);
            record_warp_progression(ctx, char_id);
            Ok(PyRep::Integer(tick as i64))
        }
        Err(e) => {
            if let Some((_, _, _, pre_debit_cap)) = cap_debit_update {
                let _ = handle
                    .modify_combat_state(ship_id, move |live_cs| {
                        live_cs.capacitor = pre_debit_cap;
                    })
                    .await;
            }
            let code = if e.contains("WarpScrambled") {
                "WarpScrambled"
            } else if e.contains("CloakedCannotWarp") {
                "CloakedCannotWarp"
            } else {
                "DestinyCommandFailed"
            };
            Err(BridgeRpcError {
                code: code.into(),
                message: e,
                client_error: None,
            })
        }
    }
}

fn verify_stargate_character_db_state(
    ch: Option<&eve_db::CharacterRecord>,
    char_id: i64,
    entry_db_state: Option<(Option<i64>, Option<i64>, u32)>,
) -> Result<(), BridgeRpcError> {
    let ch = ch.ok_or_else(|| BridgeRpcError {
        code: "CharacterNotFound".into(),
        message: format!("Character {char_id} not found"),
        client_error: None,
    })?;
    let entry_db_undocked =
        entry_db_state.is_some_and(|(st, sr, _)| st.is_none() && sr.is_none());
    let entry_db_sys = entry_db_state.map(|(_, _, sys)| sys).unwrap_or(0);
    if entry_db_undocked {
        let err_msg = if ch.station_id.is_some() || ch.structure_id.is_some() {
            Some(format!(
                "Character {char_id} is docked and cannot transition in space"
            ))
        } else if ch.solar_system_id != entry_db_sys {
            Some(format!(
                "Character {char_id} moved to solar system {} concurrently",
                ch.solar_system_id
            ))
        } else {
            None
        };
        if let Some(message) = err_msg {
            return Err(BridgeRpcError {
                code: "SystemTransitionFailed".into(),
                message,
                client_error: None,
            });
        }
    } else if entry_db_state
        .is_some_and(|e| (ch.station_id, ch.structure_id, ch.solar_system_id) != e)
    {
        return Err(BridgeRpcError {
            code: "SystemTransitionFailed".into(),
            message: format!("Character {char_id} transitioned concurrently"),
            client_error: None,
        });
    }
    Ok(())
}

fn check_stargate_entry_db_state(
    ctx: &ServerContext,
    char_id: i64,
    entry_db_state: Option<(Option<i64>, Option<i64>, u32)>,
) -> Result<(), BridgeRpcError> {
    ctx.db.with_snapshot(|snap| {
        verify_stargate_character_db_state(snap.characters.get(&char_id), char_id, entry_db_state)
    })
}

#[derive(Debug, Clone)]
struct StargateDbRollbackSnapshot {
    dest_sys_id: u32,
    prev_solar_system_id: u32,
    prev_constellation_id: u32,
    prev_region_id: u32,
    prev_station_id: Option<i64>,
    prev_structure_id: Option<i64>,
    committed_system_epoch: u64,
    ship_id: i64,
    active_ship_id: i64,
    prev_active_ship_location: Option<(i64, eve_db::InventoryFlag)>,
    extra_ship_id: Option<i64>,
    prev_extra_ship_location: Option<(i64, eve_db::InventoryFlag)>,
    prev_combat_hp: Option<eve_db::PersistedCombatHp>,
    prev_meta_combat_hp: Option<eve_db::PersistedCombatHp>,
    prev_item_position: Option<[f64; 3]>,
    prev_armor_damage: Option<f64>,
    prev_hull_damage: Option<f64>,
    prev_active_mods: Vec<i64>,
    prev_active_charges: Vec<(i64, u32)>,
    phenomena_mod_ids: Vec<i64>,
    prev_ship_fit_inputs: Option<eve_db::ShipFitInputsRecord>,
    had_overloads: bool,
    prev_transient_attrs: Vec<(i64, Vec<(u16, f64)>)>,
}

const STARGATE_SHIP_CYCLE_TRANSIENT_ATTR_IDS: [u16; 13] =
    [109, 110, 111, 112, 113, 267, 268, 269, 270, 271, 272, 273, 274];
const STARGATE_MOD_CYCLE_TRANSIENT_ATTR_IDS: [u16; 5] = [84, 267, 268, 269, 270];

fn take_transient_attrs_subset(
    transient_map: &mut HashMap<i64, HashMap<u16, f64>>,
    item_id: i64,
    attr_ids: &[u16],
    out: &mut Vec<(i64, Vec<(u16, f64)>)>,
) {
    let Some(tr) = transient_map.get_mut(&item_id) else {
        return;
    };
    let mut removed = Vec::new();
    for &aid in attr_ids {
        if let Some(val) = tr.remove(&aid) {
            removed.push((aid, val));
        }
    }
    if tr.is_empty() {
        transient_map.remove(&item_id);
    }
    if !removed.is_empty() {
        out.push((item_id, removed));
    }
}

struct StargateLocationUpdate<'a> {
    solar_system_id: u32,
    constellation_id: u32,
    region_id: u32,
    station_id: Option<i64>,
    structure_id: Option<i64>,
    ships: &'a [(i64, Option<i64>, i64, Option<eve_db::InventoryFlag>)],
    damage_attrs_restore: Option<(i64, Option<f64>, Option<f64>)>,
}

/// Shared atomic character + ship location update used by both `commit_stargate_db_transition`
/// and `rollback_stargate_db_transition` so ship cargo/module `owner_id` trees stay untouched.
fn apply_stargate_char_and_ships_in_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    char_id: i64,
    update: StargateLocationUpdate<'_>,
) -> Option<eve_db::CharacterRecord> {
    let ch = snap.characters.get_mut(&char_id)?;
    ch.solar_system_id = update.solar_system_id;
    ch.constellation_id = update.constellation_id;
    ch.region_id = update.region_id;
    ch.station_id = update.station_id;
    ch.structure_id = update.structure_id;
    ch.version += 1;
    ch.system_epoch += 1;
    let res = ch.clone();
    // Move only the ship item record itself (`flag` optional) without `set_owner_recursive`.
    for &(target_ship_id, expected_loc, new_loc, new_flag) in update.ships {
        if let Some(sh) = snap.items.get_mut(&target_ship_id) {
            let mut touched = false;
            if expected_loc.is_none() || expected_loc == Some(sh.location_id) {
                sh.location_id = new_loc;
                if let Some(flag) = new_flag {
                    sh.flag = flag;
                }
                touched = true;
            }
            if let Some((d_ship_id, prev_armor_dmg, prev_hull_dmg)) = update.damage_attrs_restore {
                if d_ship_id == target_ship_id {
                    match prev_armor_dmg {
                        Some(v) => {
                            sh.dynamic_attributes.insert(attr_ids::ARMOR_DAMAGE, v);
                        }
                        None => {
                            sh.dynamic_attributes.remove(&attr_ids::ARMOR_DAMAGE);
                        }
                    }
                    match prev_hull_dmg {
                        Some(v) => {
                            sh.dynamic_attributes.insert(attr_ids::DAMAGE, v);
                        }
                        None => {
                            sh.dynamic_attributes.remove(&attr_ids::DAMAGE);
                        }
                    }
                    touched = true;
                }
            }
            if touched {
                sh.version += 1;
            }
        }
    }
    Some(res)
}

fn rollback_stargate_db_transition(
    ctx: &ServerContext,
    char_id: i64,
    rollback: &StargateDbRollbackSnapshot,
    debited_toll: Option<eve_db::IskCents>,
) -> bool {
    let outcome: Result<(bool, Option<eve_db::IskCents>), String> =
        ctx.db.try_with_mut_snapshot(|snap| {
            // Check `solar_system_id`, `system_epoch`, and undocked status atomically in the
            // same write transaction rather than `ch.version` (unrelated mutations such as wallet
            // or standing updates must not strand the character in `dest_sys_id`).
            let still_our_commit = snap.characters.get(&char_id).is_some_and(|ch| {
                ch.solar_system_id == rollback.dest_sys_id
                    && ch.system_epoch == rollback.committed_system_epoch
                    && ch.station_id.is_none()
                    && ch.structure_id.is_none()
            });
            if !still_our_commit {
                return Ok((false, None));
            }
            let (active_loc, active_flag) =
                rollback.prev_active_ship_location.unwrap_or_else(|| {
                    let loc = rollback
                        .prev_station_id
                        .or(rollback.prev_structure_id)
                        .unwrap_or_else(|| i64::from(rollback.prev_solar_system_id));
                    let flag = if rollback.prev_station_id.is_some()
                        || rollback.prev_structure_id.is_some()
                    {
                        eve_db::InventoryFlag::Hangar
                    } else {
                        eve_db::InventoryFlag::None
                    };
                    (loc, flag)
                });
            let dest_loc = i64::from(rollback.dest_sys_id);
            let mut ship_updates = [(
                rollback.active_ship_id,
                Some(dest_loc),
                active_loc,
                Some(active_flag),
            ); 2];
            let ship_count = if let (Some(extra_id), Some((extra_loc, extra_flag))) =
                (rollback.extra_ship_id, rollback.prev_extra_ship_location)
            {
                ship_updates[1] = (extra_id, Some(dest_loc), extra_loc, Some(extra_flag));
                2
            } else {
                1
            };
            apply_stargate_char_and_ships_in_snap(
                snap,
                char_id,
                StargateLocationUpdate {
                    solar_system_id: rollback.prev_solar_system_id,
                    constellation_id: rollback.prev_constellation_id,
                    region_id: rollback.prev_region_id,
                    station_id: rollback.prev_station_id,
                    structure_id: rollback.prev_structure_id,
                    ships: &ship_updates[..ship_count],
                    damage_attrs_restore: Some((
                        rollback.ship_id,
                        rollback.prev_armor_damage,
                        rollback.prev_hull_damage,
                    )),
                },
            );
            match rollback.prev_combat_hp {
                Some(hp) => {
                    snap.sp6_lifecycle
                        .ship_combat_hp
                        .insert(rollback.ship_id, hp);
                }
                None => {
                    snap.sp6_lifecycle.ship_combat_hp.remove(&rollback.ship_id);
                }
            }
            match rollback.prev_meta_combat_hp {
                Some(meta) => {
                    snap.sp6_lifecycle
                        .ship_combat_hp
                        .insert(-rollback.ship_id, meta);
                }
                None => {
                    snap.sp6_lifecycle
                        .ship_combat_hp
                        .remove(&(-rollback.ship_id));
                }
            }
            match rollback.prev_item_position {
                Some(pos) => {
                    snap.item_positions.insert(rollback.ship_id, pos);
                }
                None => {
                    snap.item_positions.remove(&rollback.ship_id);
                }
            }
            for &mid in &rollback.prev_active_mods {
                snap.module_active_states.insert(mid);
                snap.module_active_charges.remove(&mid);
            }
            for &(mid, ch_id) in &rollback.prev_active_charges {
                snap.module_active_charges.insert(mid, ch_id);
            }
            match &rollback.prev_ship_fit_inputs {
                Some(rec) => {
                    snap.ship_fit_inputs.insert(rollback.ship_id, rec.clone());
                }
                None => {
                    snap.ship_fit_inputs.remove(&rollback.ship_id);
                }
            }
            for (item_id, attrs) in &rollback.prev_transient_attrs {
                let entry = snap.transient_item_attributes.entry(*item_id).or_default();
                for &(aid, val) in attrs {
                    entry.insert(aid, val);
                }
            }
            let refunded_bal = refund_zarzakh_gate_toll_in_snap(snap, char_id, debited_toll);
            Ok((true, refunded_bal))
        });
    match outcome {
        Ok((true, bal_after_opt)) => {
            if let Some(bal_after) = bal_after_opt {
                ctx.notify_wallet_balance(false, char_id, 1, bal_after);
            }
            true
        }
        Ok((false, _)) => false,
        Err(err) => {
            tracing::error!(
                char_id,
                error = %err,
                "rollback_stargate_db_transition transaction failed"
            );
            false
        }
    }
}

fn pop_self_phenomena_dbuffs_for_ship(
    ctx: &ServerContext,
    ship_id: i64,
    phenomena_mod_ids: &[i64],
) -> Vec<crate::services::inventory_fitting::fitting_scanners_superweapons::ActiveDbuffEntry> {
    if phenomena_mod_ids.is_empty() {
        return Vec::new();
    }
    let mut st = crate::services::inventory_fitting::fitting_scanners_superweapons::scanners_superweapons_state()
        .write()
        .unwrap_or_else(|e| e.into_inner());
    let key = (ctx.instance_id, ship_id);
    let mut removed = Vec::new();
    let should_remove_key = if let Some(entries) = st.ship_dbuffs.get_mut(&key) {
        let mut kept = Vec::with_capacity(entries.len());
        for entry in entries.drain(..) {
            if phenomena_mod_ids.contains(&entry.source_module_id) {
                removed.push(entry);
            } else {
                kept.push(entry);
            }
        }
        *entries = kept;
        entries.is_empty()
    } else {
        false
    };
    if should_remove_key {
        st.ship_dbuffs.remove(&key);
    }
    removed
}

fn restore_self_phenomena_dbuffs_for_ship(
    ctx: &ServerContext,
    ship_id: i64,
    entries: Vec<crate::services::inventory_fitting::fitting_scanners_superweapons::ActiveDbuffEntry>,
) {
    if entries.is_empty() {
        return;
    }
    let mut st = crate::services::inventory_fitting::fitting_scanners_superweapons::scanners_superweapons_state()
        .write()
        .unwrap_or_else(|e| e.into_inner());
    st.ship_dbuffs
        .entry((ctx.instance_id, ship_id))
        .or_default()
        .extend(entries);
}

#[allow(clippy::too_many_arguments)]
fn finish_stargate_leave_space_cleanup(
    ctx: &ServerContext,
    old_sys: u32,
    char_id: i64,
    ship_id: i64,
    deactivated_mods: &[i64],
    phenomena_mod_ids: &[i64],
    cleared_overloads: bool,
    self_dbuffs_cleared: bool,
) {
    crate::services::doomsday_aoe_ops::cancel_aoe_superweapon_cycles_for_ship(ctx, ship_id);
    for (_, handle) in ctx.systems.active_system_handles() {
        handle.cancel_ship_nanite_repairs_sync(ship_id, true);
    }
    let _ = crate::services::notify_emitters_space_dungeons::flush_ship_all_ewar_sync(
        ctx,
        char_id,
        Some(old_sys),
        ship_id,
    );
    if !deactivated_mods.is_empty() || cleared_overloads || self_dbuffs_cleared {
        ctx.invalidate_ship_fitting(ship_id);
    }
    let mut dbuff_sync_ships = if self_dbuffs_cleared {
        vec![ship_id]
    } else {
        Vec::new()
    };
    for &mid in phenomena_mod_ids {
        let cleared = crate::services::inventory_fitting::clear_ship_dbuffs_by_source_module(ctx, mid);
        dbuff_sync_ships.extend(cleared);
    }
    dbuff_sync_ships.sort_unstable();
    dbuff_sync_ships.dedup();
    if !dbuff_sync_ships.is_empty() {
        crate::services::inventory_fitting::sync_ship_dbuffs_and_destiny_sync(
            ctx,
            &dbuff_sync_ships,
            ctx.now_unix_secs(),
        );
    }
    for &mid in deactivated_mods {
        let _ = crate::services::notify_emitters_space_dungeons::flush_module_ewar_end(
            ctx,
            char_id,
            Some(old_sys),
            ship_id,
            mid,
        );
        crate::services::tactical_modules::remove_projected_remote_assistance(ctx, ship_id, mid);
        ctx.notify_godma_ship_effect(
            crate::router::NotificationTarget::Character(char_id),
            mid,
            10,
            false,
            0,
            0,
        );
    }
}

struct StargateRollbackGuard {
    ctx: ServerContext,
    char_id: i64,
    ship_id: i64,
    taken_guard: Option<TakenBallGuard>,
    zarzakh_debited_toll: Option<eve_db::IskCents>,
    db_rollback: Option<StargateDbRollbackSnapshot>,
    removed_self_dbuffs: Vec<crate::services::inventory_fitting::fitting_scanners_superweapons::ActiveDbuffEntry>,
    dest_spawn: Option<(crate::system_actor::SystemHandle, eve_destiny::BallId, u64)>,
    armed: bool,
}

impl StargateRollbackGuard {
    fn new(ctx: &ServerContext, char_id: i64, ship_id: i64, taken_guard: TakenBallGuard) -> Self {
        Self {
            ctx: ctx.clone(),
            char_id,
            ship_id,
            taken_guard: Some(taken_guard),
            zarzakh_debited_toll: None,
            db_rollback: None,
            removed_self_dbuffs: Vec::new(),
            dest_spawn: None,
            armed: true,
        }
    }

    fn taken(&self) -> Option<&crate::system_actor::TakenBall> {
        self.taken_guard.as_ref().and_then(|g| g.taken())
    }

    fn record_zarzakh_toll(&mut self, debited: Option<eve_db::IskCents>) {
        self.zarzakh_debited_toll = debited;
    }

    fn record_db_commit(&mut self, rollback: StargateDbRollbackSnapshot) {
        self.db_rollback = Some(rollback);
    }

    fn record_removed_self_dbuffs(
        &mut self,
        entries: Vec<crate::services::inventory_fitting::fitting_scanners_superweapons::ActiveDbuffEntry>,
    ) {
        self.removed_self_dbuffs = entries;
    }

    fn record_dest_spawn_target(
        &mut self,
        dest_handle: crate::system_actor::SystemHandle,
        ship_id: eve_destiny::BallId,
        spawn_token: u64,
    ) {
        self.dest_spawn = Some((dest_handle, ship_id, spawn_token));
    }

    fn disarm(&mut self) {
        self.armed = false;
        self.zarzakh_debited_toll = None;
        self.db_rollback = None;
        self.removed_self_dbuffs.clear();
        self.dest_spawn = None;
        if let Some(tg) = self.taken_guard.take() {
            let _ = tg.disarm();
        }
    }

    fn prepare_rollback_cleanup(
        &mut self,
    ) -> Option<(
        Option<(crate::system_actor::SystemHandle, eve_destiny::BallId, u64)>,
        Option<TakenBallGuard>,
    )> {
        if !self.armed {
            return None;
        }
        self.armed = false;
        let dest_spawn = self.dest_spawn.take();
        let toll = self.zarzakh_debited_toll.take();
        let removed_dbuffs = std::mem::take(&mut self.removed_self_dbuffs);
        let (db_ok, needs_fit_invalidation) = match self.db_rollback.take() {
            Some(db_rb) => {
                let had_fit_changes = !db_rb.prev_active_mods.is_empty()
                    || !db_rb.prev_active_charges.is_empty()
                    || db_rb.had_overloads
                    || !db_rb.prev_transient_attrs.is_empty()
                    || !removed_dbuffs.is_empty();
                (
                    rollback_stargate_db_transition(&self.ctx, self.char_id, &db_rb, toll),
                    had_fit_changes,
                )
            }
            None => {
                refund_zarzakh_gate_toll(&self.ctx, self.char_id, toll);
                (true, !removed_dbuffs.is_empty())
            }
        };
        if !db_ok {
            // A superseding concurrent transition already moved/docked the character out of
            // `dest_sys_id`; do not refund the toll, restore self-dbuffs, touch destination balls,
            // or restore a ghost ball into `old_sys`.
            if let Some(tg) = self.taken_guard.take() {
                let _ = tg.disarm();
            }
            return None;
        }
        if !removed_dbuffs.is_empty() {
            restore_self_phenomena_dbuffs_for_ship(&self.ctx, self.ship_id, removed_dbuffs);
        }
        if needs_fit_invalidation {
            self.ctx.invalidate_ship_fitting_cache_only(self.ship_id);
        }
        Some((dest_spawn, self.taken_guard.take()))
    }

    async fn rollback_and_restore(&mut self) {
        let Some((dest_spawn, taken_guard)) = self.prepare_rollback_cleanup() else {
            return;
        };
        if let Some((dest_handle, ship_id, spawn_token)) = dest_spawn {
            let _ = dest_handle.remove_ball_if_token(ship_id, spawn_token).await;
        }
        if let Some(tg) = taken_guard {
            tg.restore().await;
        }
    }
}

impl Drop for StargateRollbackGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let lock_safe_for_db = !eve_db::lock_rank::held_lock_ranks()
            .iter()
            .any(|&r| r >= eve_db::lock_rank::RANK_WORLD_INNER);
        let needs_db_or_toll =
            self.db_rollback.is_some() || self.zarzakh_debited_toll.is_some();
        if !lock_safe_for_db && needs_db_or_toll {
            let mut deferred = Self {
                ctx: self.ctx.clone(),
                char_id: self.char_id,
                ship_id: self.ship_id,
                taken_guard: self.taken_guard.take(),
                zarzakh_debited_toll: self.zarzakh_debited_toll.take(),
                db_rollback: self.db_rollback.take(),
                removed_self_dbuffs: std::mem::take(&mut self.removed_self_dbuffs),
                dest_spawn: self.dest_spawn.take(),
                armed: true,
            };
            self.armed = false;
            if let Ok(rt) = tokio::runtime::Handle::try_current() {
                rt.spawn(async move {
                    deferred.rollback_and_restore().await;
                });
            } else {
                tracing::error!(
                    char_id = deferred.char_id,
                    ship_id = deferred.ship_id,
                    "StargateRollbackGuard dropped while DB/world lock held without Tokio runtime; skipping rollback to avoid split-brain"
                );
                deferred.disarm();
            }
            return;
        }
        let Some((dest_spawn, taken_guard)) = self.prepare_rollback_cleanup() else {
            return;
        };
        if let Some((dest_handle, ship_id, spawn_token)) = dest_spawn {
            enqueue_remove_ball_nonblocking(&dest_handle, ship_id, spawn_token);
        }
        drop(taken_guard);
    }
}

#[allow(clippy::too_many_arguments)]
fn commit_stargate_db_transition(
    ctx: &ServerContext,
    session: &SessionState,
    char_id: i64,
    ship_id: i64,
    dest_sys_id: u32,
    entry_db_state: Option<(Option<i64>, Option<i64>, u32)>,
    entry_had_online_lease: bool,
    arrival_pos: DVec3,
    prev_combat_state: Option<&eve_destiny::CombatState>,
) -> Result<(eve_db::CharacterRecord, StargateDbRollbackSnapshot), BridgeRpcError> {
    let entry_db_undocked =
        entry_db_state.is_some_and(|(st, sr, _)| st.is_none() && sr.is_none());
    ctx.db.try_with_mut_snapshot(|snap| {
        if let Some(uid) = session.user_id {
            if ctx.db.kicked_reason(uid).is_some() {
                return Err(crate::router::binding::session_kicked_error(
                    "Session kicked before stargate handoff committed",
                ));
            }
            let still_valid = match ctx.db.get_online_session(uid) {
                Some(os) => {
                    os.session_id == session.session_id
                        && os.character_id.is_none_or(|cid| cid == char_id)
                }
                None => !entry_had_online_lease,
            };
            if !still_valid {
                return Err(crate::router::binding::session_kicked_error(
                    "Session displaced or closed before stargate handoff committed",
                ));
            }
        } else if entry_had_online_lease {
            return Err(crate::router::binding::session_kicked_error(
                "Session lost account binding before stargate handoff committed",
            ));
        }

        verify_stargate_character_db_state(snap.characters.get(&char_id), char_id, entry_db_state)?;
        let missing_char_err = || BridgeRpcError {
            code: "CharacterNotFound".into(),
            message: format!("Character {char_id} not found"),
            client_error: None,
        };
        let ch = snap.characters.get(&char_id).ok_or_else(missing_char_err)?;
        let prev_solar_system_id = ch.solar_system_id;
        let prev_constellation_id = ch.constellation_id;
        let prev_region_id = ch.region_id;
        let prev_station_id = ch.station_id;
        let prev_structure_id = ch.structure_id;
        let (const_id, reg_id) = ctx
            .sde
            .solar_systems
            .get(&dest_sys_id)
            .map(|s| (s.constellation_id, s.region_id))
            .unwrap_or_else(|| {
                let c = if ch.constellation_id != 0 {
                    ch.constellation_id
                } else {
                    20_000_020
                };
                let r = if ch.region_id != 0 {
                    ch.region_id
                } else {
                    10_000_002
                };
                (c, r)
            });
        let active_ship_id = ch.active_ship_id;
        let prev_active_ship_location = snap
            .items
            .get(&active_ship_id)
            .map(|sh| (sh.location_id, sh.flag));
        let (extra_ship_id, prev_extra_ship_location) = if ship_id != active_ship_id {
            (
                Some(ship_id),
                snap.items.get(&ship_id).map(|sh| (sh.location_id, sh.flag)),
            )
        } else {
            (None, None)
        };
        let prev_combat_hp = snap.sp6_lifecycle.ship_combat_hp.get(&ship_id).copied();
        let prev_meta_combat_hp = snap.sp6_lifecycle.ship_combat_hp.get(&(-ship_id)).copied();
        let prev_item_position = snap.item_positions.get(&ship_id).copied();
        let (prev_armor_damage, prev_hull_damage) = snap
            .items
            .get(&ship_id)
            .map(|it| {
                (
                    it.dynamic_attributes.get(&attr_ids::ARMOR_DAMAGE).copied(),
                    it.dynamic_attributes.get(&attr_ids::DAMAGE).copied(),
                )
            })
            .unwrap_or((None, None));
        // Capture `module_active_charges` separately from `module_active_states` so charge-only
        // ship items (e.g., slave `linked_turrets`) are cleared on commit and restored on rollback
        // without fabricating `module_active_states` entries (`#1467`, cf. `cycles.rs:2157-2159`).
        let mut prev_active_mods = Vec::new();
        let mut prev_active_charges = Vec::new();
        for it in snap.items.values() {
            if it.location_id != ship_id {
                continue;
            }
            if snap.module_active_states.contains(&it.item_id) {
                prev_active_mods.push(it.item_id);
            }
            if let Some(&ch_id) = snap.module_active_charges.get(&it.item_id) {
                prev_active_charges.push((it.item_id, ch_id));
            }
        }
        prev_active_mods.sort_unstable();
        prev_active_charges.sort_unstable_by_key(|&(mid, _)| mid);
        let phenomena_mod_ids: Vec<i64> = prev_active_mods
            .iter()
            .copied()
            .filter(|mid| {
                snap.items
                    .get(mid)
                    .and_then(|it| ctx.sde.types.get(&it.type_id))
                    .is_some_and(|t| t.group_id == 1815)
            })
            .collect();
        let prev_ship_fit_inputs = snap.ship_fit_inputs.get(&ship_id).cloned();

        let active_flag = if entry_db_undocked {
            Some(eve_db::InventoryFlag::None)
        } else {
            None
        };
        let mut ship_updates = [(active_ship_id, None, i64::from(dest_sys_id), active_flag); 2];
        let ship_count = if let Some(extra_id) = extra_ship_id {
            ship_updates[1] = (extra_id, None, i64::from(dest_sys_id), None);
            2
        } else {
            1
        };
        let res = apply_stargate_char_and_ships_in_snap(
            snap,
            char_id,
            StargateLocationUpdate {
                solar_system_id: dest_sys_id,
                constellation_id: const_id,
                region_id: reg_id,
                station_id: None,
                structure_id: None,
                ships: &ship_updates[..ship_count],
                damage_attrs_restore: None,
            },
        )
        .ok_or_else(missing_char_err)?;
        crate::sp6_lifecycle::leave_space_in_snap(
            snap,
            ship_id,
            prev_combat_state,
            Some(dest_sys_id),
            Some(arrival_pos.to_array()),
        );
        for &mid in &prev_active_mods {
            snap.module_active_states.remove(&mid);
        }
        for &(mid, _) in &prev_active_charges {
            snap.module_active_charges.remove(&mid);
        }
        let had_overloads =
            crate::router::ServerContext::clear_all_persisted_ship_overloads(snap, ship_id);
        let mut transient_mod_ids = prev_active_mods.clone();
        for it in snap.items.values().filter(|it| {
            it.location_id == ship_id && snap.transient_item_attributes.contains_key(&it.item_id)
        }) {
            transient_mod_ids.push(it.item_id);
        }
        transient_mod_ids.sort_unstable();
        transient_mod_ids.dedup();
        let mut prev_transient_attrs = Vec::new();
        take_transient_attrs_subset(
            &mut snap.transient_item_attributes,
            ship_id,
            &STARGATE_SHIP_CYCLE_TRANSIENT_ATTR_IDS,
            &mut prev_transient_attrs,
        );
        for mid in transient_mod_ids {
            take_transient_attrs_subset(
                &mut snap.transient_item_attributes,
                mid,
                &STARGATE_MOD_CYCLE_TRANSIENT_ATTR_IDS,
                &mut prev_transient_attrs,
            );
        }
        let rollback = StargateDbRollbackSnapshot {
            dest_sys_id,
            prev_solar_system_id,
            prev_constellation_id,
            prev_region_id,
            prev_station_id,
            prev_structure_id,
            committed_system_epoch: res.system_epoch,
            ship_id,
            active_ship_id,
            prev_active_ship_location,
            extra_ship_id,
            prev_extra_ship_location,
            prev_combat_hp,
            prev_meta_combat_hp,
            prev_item_position,
            prev_armor_damage,
            prev_hull_damage,
            prev_active_mods,
            prev_active_charges,
            phenomena_mod_ids,
            prev_ship_fit_inputs,
            had_overloads,
            prev_transient_attrs,
        };
        Ok((res, rollback))
    })
}

async fn handle_cmd_stargate_jump(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    if let Some(ship_id) = session.ship_id {
        crate::services::structure_control_ops::reject_if_controlling_structure(ctx, ship_id, "jump")?;
    }
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(BridgeRpcError {
            code: "ShipDocked".into(),
            message: "Cannot jump stargate while docked".into(),
            client_error: None,
        });
    }
    let old_sys = resolve_bound_or_session_sys(bound_entry, session)?;
    let Some(ship_id) = session.ship_id else {
        return Err(BridgeRpcError {
            code: "NoActiveShip".into(),
            message: "Session has no active ship".into(),
            client_error: None,
        });
    };
    let entry_db_state = ctx.db.with_snapshot(|s| {
        s.characters
            .get(&char_id)
            .map(|c| (c.station_id, c.structure_id, c.solar_system_id))
    });
    let entry_had_online_lease = session_owns_online_lease(ctx, session, char_id);
    crate::services::space_nav_movement_guard::validate_pre_movement_locks(
        ctx,
        ship_id,
        &crate::services::space_nav_movement_guard::MovementIntent::StargateJump {
            from_sys: old_sys,
            to_sys: old_sys,
            gate_id: 0,
        },
    )?;
    let Some(gate_id) = args.first().and_then(|a| a.as_i64()) else {
        return Err(BridgeRpcError {
            code: "InvalidArgument".into(),
            message: "CmdStargateJump requires integer stargateID".into(),
            client_error: None,
        });
    };
    let Some(jump) = ctx.sde.jumps.get(&gate_id) else {
        return Err(BridgeRpcError {
            code: "UnknownStargate".into(),
            message: format!("Stargate {gate_id} does not exist in SDE"),
            client_error: None,
        });
    };
    if jump.from_solar_system_id != old_sys {
        return Err(BridgeRpcError {
            code: "StargateWrongSystem".into(),
            message: format!(
                "Stargate {gate_id} is in system {}, not current system {old_sys}",
                jump.from_solar_system_id
            ),
            client_error: None,
        });
    }

    let now_unix = ctx.now_unix();
    crate::services::combat_crimewatch::check_crimewatch_action_restriction(
        ctx,
        char_id,
        ship_id,
        old_sys,
        "stargate_jump",
        now_unix,
    )?;

    let old_handle = ctx.systems.get_or_spawn(old_sys);
    let Some((cur_tick, current_ball)) = (match old_handle.get_ball_snapshot(ship_id).await {
        Ok(v) => v,
        Err(e) => {
            return Err(BridgeRpcError {
                code: "DestinyQueryFailed".into(),
                message: e,
                client_error: None,
            })
        }
    }) else {
        return Err(BridgeRpcError {
            code: "ShipNotInSpace".into(),
            message: "Active ship ball is not in space".into(),
            client_error: None,
        });
    };

    if let Some(active_timer) =
        check_session_change_timer_active(ctx, char_id, now_unix, Some((old_sys, cur_tick)))
    {
        return Err(BridgeRpcError {
            code: "SessionChangeInProgress".into(),
            message: format!(
                "Cannot jump stargate while 10s session-change timer ({}) is active",
                active_timer.reason
            ),
            client_error: None,
        });
    }

    let gate_ball_radius = old_handle
        .get_ball_snapshot(gate_id)
        .await
        .ok()
        .flatten()
        .map(|(_, b)| b.radius);
    let gate_type_id = ctx
        .sde
        .stargate_type_and_name(gate_id)
        .map(|(tid, _)| tid)
        .or(Some(16));
    let gate_celestial_radius = ctx
        .sde
        .celestials_in_system_slice(old_sys)
        .iter()
        .find(|c| c.celestial_id == gate_id)
        .map(|c| c.radius);
    let (gate_radius, _) = resolve_physical_radius(
        ctx,
        gate_type_id,
        Some(gate_id),
        PhysicalRadiusInput {
            item_radius: gate_ball_radius.filter(|&r| r.is_finite() && r > 0.0).or(gate_celestial_radius),
            ..Default::default()
        },
    );
    let effective_ship = resolve_ship_ball_for_perimeter(ctx, &current_ball);
    let warp_perimeter = evaluate_stargate_jump_perimeter(&effective_ship, jump.position, gate_radius);
    let allow_arriving_warp = matches!(warp_perimeter, Ok(true));

    // Pochven Triglavian Clade Home Systems & Zarzakh Horizon Siege checked after unified movement validation.
    ctx.validate_ship_movement_with_warp_arrival(
        char_id,
        ship_id,
        crate::services::space_nav_movement_guard::MovementIntent::StargateJump {
            from_sys: old_sys,
            to_sys: jump.to_solar_system_id,
            gate_id,
        },
        allow_arriving_warp,
    )
    .await?;

    // DERIVED: Pochven Triglavian Clade Home Systems (Kino, Niarja, Archee in region 10_000_070) require +6.0 Triglavian standing to jump stargates
    let is_pochven_home_system = ctx.sde.solar_systems.get(&jump.to_solar_system_id).is_some_and(|sys| {
        sys.region_id == 10_000_070 && matches!(sys.name.as_str(), "Kino" | "Niarja" | "Archee")
    });
    if is_pochven_home_system {
        let trig_standing = ctx.db.with_snapshot(|s| {
            s.characters
                .get(&char_id)
                .and_then(|c| c.standings.get(&500_026).copied())
                .unwrap_or(0.0)
        });
        if trig_standing < 6.0 {
            return Err(BridgeRpcError::user_error("SystemCheck_JumpFailed_StandingsTooLow"));
        }
    }

    if old_sys == SOLAR_SYSTEM_ZARZAKH {
        let locked_gate = ctx
            .db
            .with_snapshot(|snap| snap.zarzakh_gate_locks.get(&char_id).copied());
        if let Some((allowed_gate_id, expires_at)) = locked_gate {
            if now_unix < expires_at && gate_id != allowed_gate_id {
                return Err(BridgeRpcError {
                    code: "ZarzakhGateLocked".into(),
                    message: format!(
                        "Zarzakh 6-hour gate lock active: character {char_id} entered via gate {allowed_gate_id} and cannot exit via gate {gate_id}"
                    ),
                    client_error: None,
                });
            }
        }
    }

    let arrived_at_warp_dest = warp_perimeter?;
    let dest_sys_id = jump.to_solar_system_id;
    let now_ms = ctx.now_machonet_wallclock_ms();

    let _ = old_handle.settle_stargate_activation_states(now_ms);
    let mut needs_lazy_wake = false;
    if let Some(st) = old_handle.stargate_activation_state(gate_id) {
        if st.activation_state != eve_destiny::STARGATE_ACTIVATION_STATE_OPEN {
            // Under `lazy_stargate_activation = true`, gates pointing to not-yet-loaded
            // destination systems initialize as `CLOSED` until navigation intent wakes them
            // (`#1080`). A pilot already within jump range (`<= 2,500 m`, e.g. after login or
            // server restart) sends `CmdStargateJump` directly without a preceding
            // `CmdWarpToStuff`/`CmdFollowBall`, so lazily load the unloaded destination and
            // open the gate here after all pre-jump validation/range/lock/DB/toll guards pass
            // (`#1413`, `#1429`).
            // Explicitly closed gates (`lazy_stargate_activation == false`, or destination
            // already loaded while `CLOSED`) and mid-transition `ACTIVATING` gates still reject
            // with `GateClosed`.
            let is_lazy_unloaded_gate =
                st.activation_state == eve_destiny::STARGATE_ACTIVATION_STATE_CLOSED
                    && ctx.systems.lazy_stargate_activation()
                    && !ctx.systems.is_system_loaded(dest_sys_id);
            if is_lazy_unloaded_gate {
                needs_lazy_wake = true;
            } else {
                return Err(BridgeRpcError::gate_closed(gate_id));
            }
        }
    }

    // Verify session lease, DB entry state, and read-only Zarzakh entry toll BEFORE lazily
    // waking an unloaded destination system or opening its source stargate (`#1429`).
    // Actual ISK debit stays after `take_ball` + post-`take_ball` session-lease verification
    // so ISK is never debited on a failed or displaced jump.
    check_jump_session_lease(ctx, session, char_id, entry_had_online_lease)?;
    check_stargate_entry_db_state(ctx, char_id, entry_db_state)?;
    if dest_sys_id == SOLAR_SYSTEM_ZARZAKH {
        crate::services::space_navigation_advanced::prevalidate_zarzakh_gate_toll(
            ctx,
            char_id,
            ship_id,
            gate_id,
            jump.destination_stargate_id,
            &current_ball,
            args,
            kwargs,
        )?;
    }

    if needs_lazy_wake {
        if ctx
            .systems
            .ensure_stargate_destination_loaded_for_intent(old_sys, gate_id, now_ms)
            .is_some()
            && ctx.systems.is_system_loaded(dest_sys_id)
        {
            let _ = old_handle.set_stargate_activation_state(
                gate_id,
                eve_destiny::STARGATE_ACTIVATION_STATE_OPEN,
                0,
                0,
            );
        } else {
            return Err(BridgeRpcError::gate_closed(gate_id));
        }
    }

    let dest_gate_pos = ctx
        .sde
        .jumps
        .get(&jump.destination_stargate_id)
        .map(|j| j.position)
        .unwrap_or(DVec3::ZERO);
    let arrival_pos = dest_gate_pos + DVec3::new(gate_radius + 1_500.0, 0.0, 0.0);

    check_jump_session_lease(ctx, session, char_id, entry_had_online_lease)?;
    let _ = old_handle
        .emit_destiny_event(
            ship_id,
            eve_destiny::DestinyEvent::OnClientControllerEvent {
                ball_id: gate_id,
                target_id: Some(ship_id),
                event_name: "GateActivate".into(),
                value: 1,
                delay: None,
            },
        )
        .await;
    let _ = old_handle
        .emit_destiny_event(
            ship_id,
            eve_destiny::DestinyEvent::OnSpecialFXTimed {
                ball_id: ship_id,
                module_id: None,
                target_id: Some(gate_id),
                guid: "effects.JumpOut".into(),
                start: true,
                is_active: false,
                duration_ms: None,
                repeat: None,
                graphic_info: Some(vec![dest_sys_id as i64]),
            },
        )
        .await;
    if arrived_at_warp_dest {
        let _ = old_handle.force_stop_warp(ship_id);
        crate::services::space_destiny::clear_warp_tracking(ctx, Some(&old_handle), ship_id);
    }
    #[cfg(any(test, feature = "testkit"))]
    run_pre_take_ball_hook_for_test(ctx, char_id).await;
    check_jump_session_lease(ctx, session, char_id, entry_had_online_lease)?;
    let Some(taken) = old_handle.take_ball_preserving_db(ship_id).await else {
        return Err(BridgeRpcError {
            code: "SystemTransitionFailed".into(),
            message: format!("Ship {ship_id} is no longer in solar system {old_sys}"),
            client_error: None,
        });
    };
    let mut rollback_guard = StargateRollbackGuard::new(
        ctx,
        char_id,
        ship_id,
        TakenBallGuard::new(old_handle.clone(), taken),
    );
    let prev_combat_state = rollback_guard
        .taken()
        .and_then(|t| t.combat_state.clone());
    #[cfg(test)]
    run_post_take_ball_hook_for_test(ctx, char_id).await;

    if let Err(e) = check_jump_session_lease(ctx, session, char_id, entry_had_online_lease) {
        rollback_guard.rollback_and_restore().await;
        return Err(e);
    }

    // Zarzakh Gate Toll & Gate Lock (`feat:navigation.zarzakh-gate-toll`):
    // Validated and debited BEFORE mutating character solar system state (`P2` rollback safety),
    // and immediately after the post-`take_ball` session-lease verification (`#1171` / `BF-05`),
    // storing this session's exact debited amount instead of scanning shared `wallet_journal`.
    if dest_sys_id == SOLAR_SYSTEM_ZARZAKH {
        match crate::services::space_navigation_advanced::process_zarzakh_gate_toll_with_amount(
            ctx,
            char_id,
            ship_id,
            gate_id,
            jump.destination_stargate_id,
            &current_ball,
            args,
            kwargs,
        ) {
            Ok(debited) => {
                rollback_guard.record_zarzakh_toll(debited);
            }
            Err(e) => {
                rollback_guard.rollback_and_restore().await;
                return Err(e);
            }
        }
        #[cfg(test)]
        run_post_zarzakh_toll_hook_for_test(ctx, char_id);
        if let Err(e) = check_jump_session_lease(ctx, session, char_id, entry_had_online_lease) {
            rollback_guard.rollback_and_restore().await;
            return Err(e);
        }
    }

    let updated_ch_res = commit_stargate_db_transition(
        ctx,
        session,
        char_id,
        ship_id,
        dest_sys_id,
        entry_db_state,
        entry_had_online_lease,
        arrival_pos,
        prev_combat_state.as_ref(),
    );
    let (updated_ch, deactivated_mods, phenomena_mod_ids, cleared_overloads) =
        match updated_ch_res {
            Ok((ch, db_rollback)) => {
                let mods = db_rollback.prev_active_mods.clone();
                let phen_mods = db_rollback.phenomena_mod_ids.clone();
                let had_ovl = db_rollback.had_overloads;
                rollback_guard.record_db_commit(db_rollback);
                (ch, mods, phen_mods, had_ovl)
            }
            Err(e) => {
                rollback_guard.rollback_and_restore().await;
                return Err(e);
            }
        };
    let removed_self_dbuffs =
        pop_self_phenomena_dbuffs_for_ship(ctx, ship_id, &phenomena_mod_ids);
    let self_dbuffs_cleared = !removed_self_dbuffs.is_empty();
    rollback_guard.record_removed_self_dbuffs(removed_self_dbuffs);
    // Drop cached active/overloaded fit before building the destination CombatState; leave DB/world
    // side-effects and notifications deferred until `spawn_ball_with_meta_tagged` commits.
    ctx.invalidate_ship_fitting_cache_only(ship_id);

    let old_tidi = crate::services::serverops_cluster::get_system_tidi_factor(ctx, old_sys);
    let new_handle = ctx.systems.get_or_spawn(dest_sys_id);
    let dest_tidi = new_handle.tidi_factor();
    let mut new_ball = rollback_guard
        .taken()
        .expect("TakenBallGuard armed before destination spawn")
        .ball
        .clone();
    new_ball.position = arrival_pos;
    new_ball.velocity = DVec3::ZERO;
    new_ball.speed_fraction = 0.0;
    new_ball.mode = MovementMode::Stop;
    let mut combat_state = ctx.build_ship_combat_state(char_id, ship_id, new_ball.type_id);
    crate::services::r6_t1_p1_helpers::apply_preserved_combat_state(
        &mut combat_state,
        prev_combat_state.clone(),
    );
    combat_state.is_cloaked = true;
    combat_state.gate_cloak_remaining_ticks = 60;
    combat_state.undock_invulnerable_remaining_ticks = 60;
    combat_state.sync_invuln_with_undock_timer = true;
    let mut slim_meta = ctx.build_ship_slim_meta(char_id, new_ball.type_id);
    slim_meta.is_cloaked = true;
    let spawn_token = next_stargate_spawn_token();
    rollback_guard.record_dest_spawn_target(new_handle.clone(), ship_id, spawn_token);
    let spawn_tick = match new_handle
        .spawn_ball_with_meta_tagged(new_ball, combat_state, slim_meta, spawn_token)
        .await
    {
        Ok(tick) => {
            #[cfg(any(test, feature = "testkit"))]
            run_post_dest_spawn_hook_for_test(ctx, char_id).await;
            rollback_guard.disarm();
            tick
        }
        Err(spawn_err) => {
            tracing::error!(
                char_id,
                ship_id,
                old_sys,
                dest_sys_id,
                error = %spawn_err,
                "Stargate destination spawn_ball_with_meta failed; rolling back stargate jump"
            );
            rollback_guard.rollback_and_restore().await;
            let msg = format!(
                "Failed to materialize ship {ship_id} in destination solar system {dest_sys_id}: {spawn_err}"
            );
            return Err(BridgeRpcError::with_client_error(
                "SystemTransitionFailed",
                msg.clone(),
                eve_proto::ClientError::CustomInfo(msg),
            ));
        }
    };

    finish_stargate_leave_space_cleanup(
        ctx,
        old_sys,
        char_id,
        ship_id,
        &deactivated_mods,
        &phenomena_mod_ids,
        cleared_overloads,
        self_dbuffs_cleared,
    );
    if let Some(proj) = ctx.build_fit_projection(char_id, ship_id, ctx.bump_fit_rev(ship_id)) {
        let _ = new_handle.apply_fit_projection(ship_id, proj).await;
    }
    crate::sp6_lifecycle::abort_self_destruct_for_ship(ctx, ship_id);

    let mut zarzakh_lock_expiry = None;
    if dest_sys_id == SOLAR_SYSTEM_ZARZAKH {
        let expiry = now_unix.saturating_add(ZARZAKH_GATE_LOCK_SECS);
        ctx.db.with_mut_snapshot(|snap| {
            snap.zarzakh_gate_locks
                .insert(char_id, (jump.destination_stargate_id, expiry));
        });
        zarzakh_lock_expiry = Some(expiry);
    }
    crate::services::notify_emitters_space_dungeons::emit_exiting_dungeon_on_warp_if_present(
        ctx, char_id, old_sys, current_ball.position.to_array(),
    );
    crate::sp6_lifecycle::abandon_drones_and_fighters_for_ship_sync(ctx, char_id, ship_id, old_sys);
    crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
        rt.authorized_shipping_storage.retain(|&(_, c)| c != char_id);
    });
    ctx.sync_character_local_chat(char_id, Some(old_sys), dest_sys_id);
    // Destination grid watchers already have the destination stargate materialized and must not
    // receive `AddBalls2` or `OnSpecialFX(ship_id)` while the arriving ship holds gate cloak (#871/#1084).
    // Notify destination gate grid observers via V24 `OnClientControllerEvent("Arrival")` on the
    // stargate (avoiding `effects.GateActivity`, whose V24 `Jump.py` handler invokes removed
    // `gateBall.Activate()`/`JumpArrival()` methods `#1080`), and deliver ship-bound `effects.JumpIn`
    // solely to the pilot's observer queue (`SetState` prepends index 0).
    let _ = new_handle
        .emit_destiny_event(
            ship_id,
            eve_destiny::DestinyEvent::OnClientControllerEvent {
                ball_id: jump.destination_stargate_id,
                target_id: Some(ship_id),
                event_name: "Arrival".into(),
                value: 1,
                delay: None,
            },
        )
        .await;
    let _ = new_handle
        .emit_observer_destiny_event(
            ship_id,
            eve_destiny::DestinyEvent::OnSpecialFXTimed {
                ball_id: ship_id,
                module_id: None,
                target_id: None,
                guid: "effects.JumpIn".into(),
                start: true,
                is_active: false,
                duration_ms: Some(5_000.0),
                repeat: None,
                graphic_info: None,
            },
        )
        .await;
    let _ = ctx.living_world.materialize_system_lod2(dest_sys_id, &ctx.systems, &ctx.sde).await;
    let _ = crate::services::serverops_cluster::sync_jump_clock_and_emit_rebase_if_needed(
        ctx,
        char_id,
        old_sys,
        dest_sys_id,
        old_tidi,
        dest_tidi,
    );
    let _ = crate::services::serverops_cluster::emit_sim_clock_rebase_on_entry_if_needed(
        ctx,
        char_id,
        dest_sys_id,
    );
    let _ = new_handle.push_set_state(ship_id).await;
    crate::services::notify_emitters_space_dungeons::emit_jump_cloak_started(ctx, char_id, ship_id);
    crate::services::notify_emitters_seasons_tactical::emit_restoring_invuln_updated(ctx, char_id, ship_id, 600_000_000);

    let jump_spec = crate::router::SessionTransitionSpec::for_kind(
        char_id,
        crate::router::TransitionKind::JumpSystem { solar_system_id: updated_ch.solar_system_id },
        "stargate_jump",
    )
    .with_apply_db(false)
    .with_cleanup_departing_space(false)
    .with_sync_local_chat(false)
    .with_emit_on_session_changed(false)
    .with_preserve_session_ship(true);
    let _ = ctx.execute_session_transition_sync(Some(session), jump_spec);
    crate::services::map_service::record_solar_system_jump(ctx, char_id, dest_sys_id);
    crate::services::nav_warp_interdiction::advance_waypoints_on_arrival(
        ctx,
        char_id,
        dest_sys_id as i64,
    );
    let _ = crate::services::combat_npc_police::check_and_spawn_faction_police(ctx, dest_sys_id).await;
    crate::services::combat_crimewatch::schedule_highsec_criminal_concord_if_needed(
        ctx,
        char_id,
        ship_id,
        dest_sys_id,
        now_unix,
    );
    record_session_change_timer(ctx, char_id, now_unix, dest_sys_id, spawn_tick, "stargate_jump");

    crate::quasar_gateway::isolate_side_effect(
        "space_warp_jump.CmdStargateJump.secondary_hooks",
        || {
            if let Some(expiry) = zarzakh_lock_expiry {
                let _ = crate::services::quasar_sov_services::publish_stargate_lock_activated_notice(
                    &ctx.quasar_gateway, char_id, dest_sys_id, jump.destination_stargate_id as u64, expiry, now_unix,
                );
            }
            ctx.quasar_gateway
                .progression
                .daily_goals
                .on_gameplay_event(char_id as u32, 4, 1, &ctx.quasar_gateway);
            ctx.quasar_gateway
                .progression
                .career_goals
                .on_career_gameplay_event(
                    char_id as u32,
                    crate::services::quasar_career_goals::solar_system_career_event(
                        &ctx.sde,
                        "travel",
                        dest_sys_id,
                    ),
                    1,
                    &ctx.quasar_gateway,
                );
            ctx.quasar_gateway
                .progression
                .career_goals
                .on_career_gameplay_event(
                    char_id as u32,
                    crate::services::quasar_career_goals::solar_system_career_event(
                        &ctx.sde,
                        "advanced_travel",
                        dest_sys_id,
                    ),
                    1,
                    &ctx.quasar_gateway,
                );
            ctx.quasar_gateway.progression.achievements.on_gameplay_event_by_name(char_id as u32, &["stargateJumps"], 1, now_unix, &ctx.quasar_gateway);
        },
    );

    let new_beyonce_id = ctx.bind_object(session, BEYONCE_SVC, dest_sys_id as i64)?;
    let ready_at_ft = transition_ready_at_filetime_ticks(ctx);

    Ok(PyRep::keyval([
        ("solarSystemID", PyRep::Integer(dest_sys_id as i64)),
        ("beyonceBoundID", PyRep::Integer(new_beyonce_id as i64)),
        (
            "boundResult",
            PyRep::List(vec![
                PyRep::Integer(new_beyonce_id as i64),
                PyRep::Integer(ready_at_ft),
            ]),
        ),
        ("jumpCooldownMinutes", PyRep::Real(0.0)),
        ("jumpFatigueMinutes", PyRep::Real(0.0)),
        (
            "sessionChangeTimerSeconds",
            PyRep::Integer(
                crate::services::space_destiny::STARGATE_SESSION_CHANGE_TIMER_SECS as i64,
            ),
        ),
        ("sessionChangeTimerFiletime", PyRep::Integer(ready_at_ft)),
        ("sessionChangeTimer", PyRep::Integer(ready_at_ft)),
    ]))
}

const SURFACE_PERIMETER_METERS: f64 = 2_500.0;
const DOGMA_ATTR_RADIUS: eve_sde::AttributeId = 162;
pub const DEFAULT_PHYSICAL_RADIUS_METERS: f64 = 15_000.0;

/// Provenance of a resolved entity physical radius in meters (`#1088`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RadiusSource {
    ItemPhysicalRadius,
    TypePhysicalRadius,
    ItemRadius,
    TypeRadius,
    InteractionRadius,
    FallbackDefault,
}

impl RadiusSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ItemPhysicalRadius => "item.physicalRadius",
            Self::TypePhysicalRadius => "type.physicalRadius",
            Self::ItemRadius => "item.radius",
            Self::TypeRadius => "type.radius",
            Self::InteractionRadius => "interactionRadius",
            Self::FallbackDefault => "default_15000m",
        }
    }
}

/// Ordered candidate radii evaluated by [`physical_radius`] (`#1088`).
#[derive(Debug, Clone, Copy, Default)]
pub struct PhysicalRadiusInput {
    pub item_physical_radius: Option<f64>,
    pub type_physical_radius: Option<f64>,
    pub item_radius: Option<f64>,
    pub type_radius: Option<f64>,
    pub interaction_radius: Option<f64>,
}

/// Resolves an entity's physical radius in meters using the priority chain:
/// `item.physicalRadius -> type.physicalRadius -> item.radius -> type.radius -> interactionRadius -> 15_000.0`.
pub fn physical_radius(input: &PhysicalRadiusInput) -> (f64, RadiusSource) {
    let valid = |opt: Option<f64>| opt.filter(|&r| r.is_finite() && r > 0.0);
    if let Some(r) = valid(input.item_physical_radius) {
        return (r, RadiusSource::ItemPhysicalRadius);
    }
    if let Some(r) = valid(input.type_physical_radius) {
        return (r, RadiusSource::TypePhysicalRadius);
    }
    if let Some(r) = valid(input.item_radius) {
        return (r, RadiusSource::ItemRadius);
    }
    if let Some(r) = valid(input.type_radius) {
        return (r, RadiusSource::TypeRadius);
    }
    if let Some(r) = valid(input.interaction_radius) {
        return (r, RadiusSource::InteractionRadius);
    }
    (DEFAULT_PHYSICAL_RADIUS_METERS, RadiusSource::FallbackDefault)
}

static LOGGED_FALLBACK_RADIUS_TYPES: OnceLock<Mutex<HashSet<u32>>> = OnceLock::new();

fn log_fallback_radius_once(
    type_id: Option<u32>,
    item_id: Option<i64>,
    source: RadiusSource,
) -> bool {
    let key = type_id.unwrap_or(0);
    let inserted = LOGGED_FALLBACK_RADIUS_TYPES
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key);
    if inserted {
        tracing::warn!(
            target: "eve_server::physical_radius",
            type_id = ?type_id,
            item_id = ?item_id,
            source = source.as_str(),
            fallback_m = DEFAULT_PHYSICAL_RADIUS_METERS,
            "physical_radius hit 15,000 m fallback for type without item/type/interaction radius"
        );
    }
    inserted
}

/// Resolves physical radius using SDE type metadata + live item/ball inputs, logging once per
/// `type_id` whenever the final `15,000 m` default fallback is reached.
///
/// Lock-free on `ctx.db` so callers inside DB snapshot closures never nest DB locks.
pub(crate) fn resolve_physical_radius(
    ctx: &ServerContext,
    type_id: Option<u32>,
    item_id: Option<i64>,
    input: PhysicalRadiusInput,
) -> (f64, RadiusSource) {
    let type_rec = type_id.and_then(|tid| ctx.sde.types.get(&tid));
    let sde_type_radius = type_rec.and_then(|t| {
        if t.radius.is_finite() && t.radius > 0.0 {
            Some(t.radius)
        } else {
            t.default_attributes
                .get(&DOGMA_ATTR_RADIUS)
                .copied()
                .filter(|&r| r.is_finite() && r > 0.0)
        }
    });
    let (radius, source) = physical_radius(&PhysicalRadiusInput {
        type_radius: input.type_radius.or(sde_type_radius),
        ..input
    });
    if source == RadiusSource::FallbackDefault {
        log_fallback_radius_once(type_id, item_id, source);
    }
    (radius, source)
}

/// Ensures a ship `Ball` used in surface-perimeter checks has a positive resolved radius (`#1072`, `#1088`).
pub(crate) fn resolve_ship_ball_for_perimeter(
    ctx: &ServerContext,
    ball: &eve_destiny::Ball,
) -> eve_destiny::Ball {
    if ball.radius.is_finite() && ball.radius > 0.0 {
        return ball.clone();
    }
    let tid = (ball.type_id > 0).then_some(ball.type_id);
    let (ship_r, _) =
        resolve_physical_radius(ctx, tid, Some(ball.id), PhysicalRadiusInput::default());
    let mut updated = ball.clone();
    updated.radius = ship_r;
    updated
}

/// Returns the maximum center-to-center distance (`target_radius + ball.radius + 2,500 m`)
/// corresponding to a `2,500 m` ball-to-ball surface perimeter.
#[inline]
fn max_center_dist_for_perimeter(ball: &eve_destiny::Ball, target_radius: f64) -> f64 {
    target_radius.max(0.0) + ball.radius.max(0.0) + SURFACE_PERIMETER_METERS
}

/// Evaluates whether `ball` is inside the `2,500 m` ball-to-ball surface perimeter around
/// `(target_pos, target_radius)`, including arriving `MovementMode::Warp` within `<= 4.0 s`
/// of deceleration (`#1001`).
///
/// Returns `Ok(true)` when an arriving warp must be terminated, `Ok(false)` when already inside
/// the static perimeter in a non-warp mode, or builds the caller's error on mid-warp / out-of-range.
fn evaluate_surface_perimeter(
    ball: &eve_destiny::Ball,
    target_pos: DVec3,
    target_radius: f64,
    on_in_warp: impl FnOnce() -> BridgeRpcError,
    on_outside: impl FnOnce(f64, f64, f64) -> BridgeRpcError,
) -> Result<bool, BridgeRpcError> {
    let target_r = target_radius.max(0.0);
    let ship_r = ball.radius.max(0.0);
    let max_dist = max_center_dist_for_perimeter(ball, target_r);
    let dist = ball.position.distance(target_pos);
    match ball.mode {
        MovementMode::Warp { destination, .. } => {
            if destination.distance(target_pos) <= max_dist
                && (dist <= max_dist
                    || eve_destiny::physics::is_warp_finishing_within(ball, 4.0))
            {
                Ok(true)
            } else {
                Err(on_in_warp())
            }
        }
        _ => {
            if dist > max_dist {
                let surface_dist = (dist - ship_r - target_r).max(0.0);
                Err(on_outside(dist, max_dist, surface_dist))
            } else {
                Ok(false)
            }
        }
    }
}

/// Evaluates the unconditional `gate_radius + ball.radius + 2,500 m` EVE stargate jump
/// perimeter (`surface_dist <= 2,500 m`), mirroring `evaluate_dock_perimeter`:
///
/// - When `ball.mode` is `MovementMode::Warp`, jumping is permitted (`Ok(true)`, signalling
///   that the active warp must be terminated on jump) only when `destination` lies inside
///   `gate_radius + ball.radius + 2,500 m` of `gate_pos` AND the ship is either already inside
///   that range or within the final `<= 4.0 s` of `WarpPhase::Decelerating` (`#1001`); all other
///   warp states return `ShipInWarp`.
/// - For non-warp movement modes, returns `NotCloseEnoughToJump` when
///   `(dist - ball.radius - gate_radius).max(0.0) > 2,500.0` and `Ok(false)` otherwise.
pub(crate) fn evaluate_stargate_jump_perimeter(
    ball: &eve_destiny::Ball,
    gate_pos: DVec3,
    gate_radius: f64,
) -> Result<bool, BridgeRpcError> {
    evaluate_surface_perimeter(
        ball,
        gate_pos,
        gate_radius,
        || BridgeRpcError {
            code: "ShipInWarp".into(),
            message: "Cannot jump stargate while ship is still in warp".into(),
            client_error: None,
        },
        |dist, max_dist, surface_dist| {
            BridgeRpcError::not_close_enough_to_jump(
                surface_dist,
                dist,
                max_dist,
                SURFACE_PERIMETER_METERS,
            )
        },
    )
}

/// Evaluates the unconditional `target_radius + ball.radius + 2,500 m` EVE docking perimeter
/// (`surface_dist <= 2,500 m`, `SP-4`) across `beyonce.CmdDock`, `stationSvc.Dock`, and
/// `structureDocking.Dock`.
///
/// - When `ball.mode` is `MovementMode::Warp`, docking is permitted (`Ok(true)`, signalling that
///   the active warp must be terminated) only when `destination` lies inside
///   `target_radius + ball.radius + 2,500 m` of `target_pos` AND the ship is either already inside
///   `max_dock_dist` or within the final `<= 4.0 s` of `WarpPhase::Decelerating`
///   (`eve_destiny::physics::is_warp_finishing_within`, `#1001`); all other warp states return
///   `ShipInWarp`.
/// - For non-warp movement modes, returns `OutsideDockingPerimeter` when `dist > max_dock_dist`
///   and `Ok(false)` otherwise, regardless of `session.address`.
pub(crate) fn evaluate_dock_perimeter(
    ball: &eve_destiny::Ball,
    target_pos: DVec3,
    target_radius: f64,
) -> Result<bool, BridgeRpcError> {
    evaluate_surface_perimeter(
        ball,
        target_pos,
        target_radius,
        || BridgeRpcError {
            code: "ShipInWarp".into(),
            message: "Cannot dock while ship is still in warp".into(),
            client_error: None,
        },
        |dist, max_dock_dist, _surface_dist| {
            BridgeRpcError::outside_docking_perimeter(dist, max_dock_dist)
        },
    )
}

async fn handle_dock(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    if let Some(ship_id) = session.ship_id {
        crate::services::structure_control_ops::reject_if_controlling_structure(ctx, ship_id, "dock")?;
    }
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(BridgeRpcError {
            code: "AlreadyDocked".into(),
            message: "Character is already docked in a station or structure".into(),
            client_error: None,
        });
    }
    let Some(mut ship_id) = session.ship_id else {
        return Err(BridgeRpcError {
            code: "NoActiveShip".into(),
            message: "Cannot dock without an active ship in space".into(),
            client_error: None,
        });
    };
    let Some(dock_id) = args.first().and_then(|a| a.as_i64()) else {
        return Err(BridgeRpcError {
            code: "MissingLocationId".into(),
            message: "Dock requires an explicit station or structure ID argument".into(),
            client_error: None,
        });
    };
    let Some(sys_id) = bound_entry
        .map(|b| b.object_id as u32)
        .or(session.solar_system_id)
    else {
        return Err(BridgeRpcError {
            code: "NoSolarSystem".into(),
            message: "Session has no active solarSystemID".into(),
            client_error: None,
        });
    };

    let now_unix = ctx.now_unix();
    let handle = ctx.systems.get_or_spawn(sys_id);
    let mut ball_snap = match handle.get_ball_snapshot(ship_id).await {
        Ok(v) => v,
        Err(e) => {
            return Err(BridgeRpcError {
                code: "DestinyQueryFailed".into(),
                message: e,
                client_error: None,
            })
        }
    };
    if ball_snap.is_none() {
        if let Some(db_ship_id) = ctx
            .db
            .with_snapshot(|s| s.characters.get(&char_id).map(|c| c.active_ship_id))
            .filter(|&id| id > 0 && id != ship_id)
        {
            if let Ok(Some(v)) = handle.get_ball_snapshot(db_ship_id).await {
                ship_id = db_ship_id;
                session.ship_id = Some(db_ship_id);
                ball_snap = Some(v);
            }
        }
    }
    crate::services::tactical_modules::check_ship_navigation_lock(ctx, ship_id, "dock")?;
    crate::services::space_navigation_advanced::check_immobilizing_module_active(ctx, ship_id, "dock")?;
    if args.get(1).and_then(|a| a.as_i64()).is_some_and(|req| req != ship_id) {
        return Err(BridgeRpcError {
            code: "InvalidShipId".into(),
            message: "Requested shipID does not match pilot's active ship".into(),
            client_error: None,
        });
    }
    let cur_tick_opt = ball_snap.as_ref().map(|(cur_tick, _)| (sys_id, *cur_tick));
    if let Some(active_timer) =
        check_session_change_timer_active(ctx, char_id, now_unix, cur_tick_opt)
    {
        return Err(BridgeRpcError {
            code: "SessionChangeInProgress".into(),
            message: format!(
                "Cannot dock while 10s session-change timer ({}) is active",
                active_timer.reason
            ),
            client_error: None,
        });
    }
    let Some((cur_tick, ball)) = ball_snap else {
        return Err(BridgeRpcError {
            code: "ShipNotInSpace".into(),
            message: format!("Ship {ship_id} is not spawned in solar system {sys_id} Ballpark"),
            client_error: None,
        });
    };

    let sys_sec = ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .map(|s| s.security_status)
        .unwrap_or(1.0);
    let ship_type_id = ctx
        .db
        .with_snapshot(|s| s.items.get(&ship_id).map(|i| i.type_id))
        .unwrap_or(ball.type_id);
    let ship_group_id = ctx
        .sde
        .types
        .get(&ship_type_id)
        .map(|t| t.group_id)
        .unwrap_or(0);
    let is_capsule = ship_type_id == 670 || ship_group_id == 29;

    let is_structure = dock_id >= 1_000_000_000_000
        || ctx.db.with_snapshot(|snap| snap.structures.contains_key(&dock_id));
    if let Err(e) = crate::services::fw_gameplay::check_docking_allowed(ctx, char_id, sys_id, dock_id, is_structure) {
        return Err(e);
    }

    {
        let cw = ctx.crimewatch.read().unwrap_or_else(|e| e.into_inner());
        if let Some(cw_state) = cw.states.get(&char_id) {
            if cw_state.is_weapon_timer_active(now_unix) {
                return Err(BridgeRpcError {
                    code: "WeaponsTimerActive".into(),
                    message: "Cannot dock while a 60s Weapons Timer is active".into(),
                    client_error: None,
                });
            }
            if sys_sec >= 0.45 && cw_state.is_criminal(now_unix) && !is_capsule {
                return Err(BridgeRpcError {
                    code: "CantDockGlobalCriminalFlag".into(),
                    message: "Cannot dock a non-capsule ship in high-sec while a Criminal Timer is active".into(),
                    client_error: None,
                });
            }
        }
    }

    if crate::services::combat_crimewatch::combat_npc_police::is_outlaw_ship_docking_refused(ctx, sys_id, char_id, ship_id) {
        return Err(BridgeRpcError {
            code: "FactionPoliceRefusedDocking".into(),
            message: "Faction police refuse docking permissions to outlaw ships in high-security space".into(),
            client_error: None,
        });
    }

    let (target_pos, target_type_id) = if is_structure {
        let st_opt = ctx
            .db
            .with_snapshot(|snap| snap.structures.get(&dock_id).cloned());
        let Some(st) = st_opt else {
            return Err(BridgeRpcError {
                code: "StructureNotFound".into(),
                message: format!("Structure {dock_id} does not exist"),
                client_error: None,
            });
        };
        if st.solar_system_id != sys_id {
            return Err(BridgeRpcError {
                code: "DockTargetWrongSystem".into(),
                message: format!(
                    "Structure {dock_id} is in system {}, but pilot is in {sys_id}",
                    st.solar_system_id
                ),
                client_error: None,
            });
        }
        if st.state == "Destroyed" {
            return Err(BridgeRpcError {
                code: "StructureDestroyed".into(),
                message: format!("Structure {dock_id} is destroyed"),
                client_error: None,
            });
        }
        if matches!(st.state.as_str(), "Unanchored" | "AnchorVulnerable" | "Anchoring") {
            return Err(BridgeRpcError {
                code: "StructureNotDockable".into(),
                message: format!("Structure {dock_id} is in state '{}' and cannot be docked at", st.state),
                client_error: None,
            });
        }
        crate::services::structure_acl_defense_rules::apply_pending_profile_applications(ctx);
        let acl_denied = ctx.db.with_snapshot(|snap| {
            !crate::services::station_structure::structure_upwell_ops::evaluate_character_structure_access_in_snap(
                snap, dock_id, char_id,
            )
        });
        if acl_denied {
            return Err(BridgeRpcError {
                code: "StructureDockingAccessDenied".into(),
                message: format!(
                    "Character {char_id} is denied docking access by structure {dock_id} ACL"
                ),
                client_error: None,
            });
        }
        let is_medium_structure = matches!(st.type_id, 35832 | 35825 | 35835);
        let is_large_structure = matches!(st.type_id, 35833 | 35826 | 35836);
        let is_capital_ship =
            matches!(ship_group_id, 30 | 485 | 547 | 659 | 883 | 1538 | 4594); // freighters dock at all Upwell hulls
        let is_supercapital_ship = matches!(ship_group_id, 30 | 659);
        if (is_medium_structure && is_capital_ship)
            || (is_large_structure && is_supercapital_ship)
        {
            return Err(BridgeRpcError {
                code: "ShipTooLargeForStructure".into(),
                message: format!(
                    "Ship type {} (group {}) is too large to dock at structure {} (type {})",
                    ball.type_id, ship_group_id, dock_id, st.type_id
                ),
                client_error: None,
            });
        }
        (DVec3::from_array(st.position), Some(st.type_id))
    } else {
        crate::services::space_navigation_advanced::validate_npc_station_docking(ship_group_id, true)?;
        let st_opt = ctx
            .sde
            .stations
            .get(&dock_id)
            .filter(|st| st.solar_system_id == sys_id)
            .cloned();
        let Some(st) = st_opt else {
            return Err(BridgeRpcError {
                code: "DockTargetWrongSystem".into(),
                message: format!(
                    "Station {dock_id} does not exist in current solar system {sys_id}"
                ),
                client_error: None,
            });
        };
        crate::services::station_structure_handlers::validate_npc_station_docking_gate(ctx, char_id, dock_id)?;
        (st.position, Some(st.station_type_id))
    };

    let ball_radius = handle
        .get_ball_snapshot(dock_id)
        .await
        .ok()
        .flatten()
        .map(|(_, b)| b.radius);
    let (target_radius, _) = resolve_physical_radius(
        ctx,
        target_type_id,
        Some(dock_id),
        PhysicalRadiusInput {
            item_radius: ball_radius,
            ..Default::default()
        },
    );
    let effective_ball = resolve_ship_ball_for_perimeter(ctx, &ball);
    let arrived_at_warp_dest = evaluate_dock_perimeter(&effective_ball, target_pos, target_radius)?;
    if is_structure && handle.is_ball_scrambled(ship_id).await.unwrap_or(false) {
        return Err(BridgeRpcError {
            code: "ShipWarpScrambled".into(),
            message: "Cannot dock at an Upwell structure while ship is warp scrambled".into(),
            client_error: None,
        });
    }
    if arrived_at_warp_dest {
        stop_arriving_warp_for_dock(ctx, &handle, &ball, target_pos, target_radius);
    }
    let Some(taken) = handle.take_ball_for_docking(ship_id, dock_id, target_pos).await else {
        return Err(BridgeRpcError {
            code: "DockFailed".into(),
            message: format!("Ship {ship_id} is no longer in space in solar system {sys_id}"),
            client_error: None,
        });
    };
    let taken_guard = TakenBallGuard::new(handle.clone(), taken);
    #[cfg(test)]
    run_post_take_ball_hook_for_test(ctx, char_id).await;
    let docked_combat_state =
        crate::services::station_structure::docked_combat_state_recharging_shield_and_cap(
            taken_guard.taken().and_then(|t| t.combat_state.as_ref()),
        );
    match ctx.db.dock_character_if_in_space(char_id, sys_id, dock_id, is_structure) {
        Ok(_) => {
            let _committed_taken = taken_guard.disarm();
            handle.clear_warp_stopped_at(ship_id);
            ctx.db.with_mut_snapshot(|snap| {
                crate::sp6_lifecycle::leave_space_in_snap(
                    snap,
                    ship_id,
                    docked_combat_state.as_ref(),
                    None,
                    None,
                );
            });
            crate::services::notify_emitters_space_dungeons::emit_exiting_dungeon_on_warp_if_present(
                ctx,
                char_id,
                sys_id,
                ball.position.to_array(),
            );
            let dock_spec = crate::router::SessionTransitionSpec::for_kind(
                char_id,
                crate::router::TransitionKind::Dock { dock_id, is_structure },
                "dock",
            )
            .with_apply_db(false)
            .with_cleanup_departing_space(false)
            .with_preserve_session_ship(true);
            let _ = ctx.execute_session_transition_sync(Some(session), dock_spec);
            deactivate_ship_modules_on_leave_space(ctx, char_id, ship_id);
            crate::sp6_lifecycle::abort_self_destruct_for_ship(ctx, ship_id);
            crate::sp6_lifecycle::abandon_drones_and_fighters_for_ship_sync(ctx, char_id, ship_id, sys_id);
            if let Some(ship_rec) = ctx.db.with_snapshot(|snap| snap.items.get(&ship_id).cloned()) {
                ctx.notify_item_record_change(
                    char_id,
                    &ship_rec,
                    vec![
                        (3, PyRep::Integer(sys_id as i64)),
                        (4, PyRep::Integer(eve_db::InventoryFlag::None as i64)),
                    ],
                );
            }
            record_session_change_timer(ctx, char_id, now_unix, sys_id, cur_tick, "dock");
            crate::services::nav_warp_interdiction::advance_waypoints_on_arrival(
                ctx, char_id, dock_id,
            );
            ctx.emit_typed(
                crate::router::NotificationTarget::Character(char_id),
                crate::router::notify_registry::OnDockingAccepted { item_id: dock_id },
            );
            ctx.emit_typed(
                crate::router::NotificationTarget::Character(char_id),
                crate::router::notify_registry::OnDockingFinished { item_id: dock_id },
            );
            if is_structure {
                let (corp_id, ally_id, fac_id, st_type_id) = ctx.db.with_snapshot(|snap| {
                    let c = snap.characters.get(&char_id);
                    let corp = c.map(|ch| ch.corporation_id).unwrap_or(0);
                    let ally = c.and_then(|ch| ch.alliance_id);
                    let fac = c.and_then(|ch| ch.war_faction_id);
                    let st_tid = snap.structures.get(&dock_id).map(|s| s.type_id).unwrap_or(35832);
                    (corp, ally, fac, st_tid)
                });
                emit_structure_dock_notifications(
                    ctx, dock_id, char_id, corp_id, ally_id, fac_id, st_type_id, ship_id,
                );
            }
            crate::quasar_gateway::isolate_side_effect("space_warp_jump.dock.quasar_hooks", || {
                ctx.quasar_gateway.on_session_changed(session, now_unix);
                ctx.quasar_gateway.progression.achievements.on_gameplay_event_by_name(char_id as u32, &["dockInStation"], 1, now_unix, &ctx.quasar_gateway);
            });
            Ok(PyRep::keyval([
                ("locationID", PyRep::Integer(dock_id)),
                ("isStructure", PyRep::Bool(is_structure)),
                ("sessionChangeTimerSeconds", PyRep::Integer(10)),
            ]))
        }
        Err(e) => {
            taken_guard.restore().await;
            Err(BridgeRpcError {
                code: "DockFailed".into(),
                message: e.to_string(),
                client_error: None,
            })
        }
    }
}

/// Delay between flushing warp-stop Destiny updates and completing dock session change so
/// V24 `Warp.WarpLoop` deceleration (`OnWarpDecelerate` + `blue.synchro.SleepSim(500)`) exits
/// before `OnReleaseBallpark`, including when `stationSvc.Dock` arrives ~2.6s after `AdvanceClock`.
pub const WARP_DOCK_SETTLE_DURATION: std::time::Duration =
    std::time::Duration::from_millis(3_500);

pub(crate) struct DockWarpSettlePlan {
    pub sys_id: eve_sde::SolarSystemId,
    pub ship_id: eve_destiny::BallId,
    pub stopped_now: bool,
    pub settle_deadline: tokio::time::Instant,
}

/// Terminates an arriving warp before docking and clears `active_warps`, snapping the ball to
/// the verified warp landing point when `is_warp_finishing_within(ball, 4.0)` accepted it outside
/// the static `target_radius + ball.radius + 2,500 m` shell so post-settle perimeter checks
/// remain valid.
pub(crate) fn stop_arriving_warp_for_dock(
    ctx: &ServerContext,
    handle: &crate::system_actor::SystemHandle,
    ball: &eve_destiny::Ball,
    target_pos: DVec3,
    target_radius: f64,
) {
    let max_dock_dist = max_center_dist_for_perimeter(ball, target_radius);
    let landing_pos = match ball.mode {
        MovementMode::Warp { destination, .. }
            if ball.position.distance(target_pos) > max_dock_dist && destination.is_finite() =>
        {
            Some(destination)
        }
        _ => None,
    };
    let _ = handle.finish_warp_for_dock(ball.id, landing_pos);
    crate::services::space_destiny::clear_warp_tracking(ctx, Some(handle), ball.id);
}

fn resolve_dock_target_for_settle(
    ctx: &ServerContext,
    sys_id: eve_sde::SolarSystemId,
    service: &str,
    method: &str,
    target_id: i64,
) -> Option<(DVec3, Option<u32>, bool)> {
    let station_target = || {
        let st = ctx.sde.stations.get(&target_id)?;
        if st.solar_system_id != sys_id {
            return None;
        }
        let pos = DVec3::new(st.position.x, st.position.y, st.position.z);
        Some((pos, Some(st.station_type_id), false))
    };
    let structure_target = || {
        ctx.db.with_snapshot(|snap| {
            let st = snap.structures.get(&target_id)?;
            if st.solar_system_id != sys_id || st.state == "Destroyed" || st.state == "Anchoring" {
                return None;
            }
            let pos = DVec3::from_array(st.position);
            Some((pos, Some(st.type_id), true))
        })
    };
    match (service, method) {
        ("stationSvc", "Dock") => station_target(),
        ("structureDocking", "Dock") => structure_target(),
        ("beyonce", "CmdDock") => station_target().or_else(structure_target),
        _ => None,
    }
}

/// Evaluates on the live TCP path whether a valid dock request needs a `DoDestinyUpdate(Stop)`
/// flush and `WARP_DOCK_SETTLE_DURATION` wait before completing `OnReleaseBallpark`.
///
/// The V24 client's `Warp.WarpLoop` tasklet (`eve/client/script/environment/effects/Warp.py`)
/// polls `ball.mode == DSTBALL_WARP` every `blue.synchro.SleepSim(500)` and invokes
/// `LocationArrivalIndicationService.on_warp_finished` after loop exit; delaying the dock
/// session transition until `Stop` has been delivered and settled prevents `OnReleaseBallpark`
/// (`michelle.__bp = None`) from racing ahead of `on_warp_finished`.
pub(crate) async fn prepare_dock_warp_settle(
    ctx: &ServerContext,
    session: &SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Option<DockWarpSettlePlan> {
    let is_turbo = match (service, method) {
        ("stationSvc", "Dock") | ("structureDocking", "Dock") | ("beyonce", "CmdDock") => false,
        ("beyonce", "CmdTurboDock") => true,
        _ => return None,
    };
    let char_id = session.character_id?;
    let sys_id = session.solar_system_id?;
    let ship_id = session.ship_id?;
    if session.station_id.is_some() || session.structure_id.is_some() {
        return None;
    }
    if !is_turbo
        && args
            .get(1)
            .and_then(|a| a.as_i64())
            .is_some_and(|req| req > 0 && req != ship_id)
    {
        return None;
    }
    if crate::services::tactical_modules::check_ship_navigation_lock(ctx, ship_id, "dock").is_err()
        || crate::services::space_navigation_advanced::check_immobilizing_module_active(
            ctx, ship_id, "dock",
        )
        .is_err()
    {
        return None;
    }

    let target_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    let handle = ctx.systems.get_or_spawn(sys_id);
    let (cur_tick, ball) = handle.get_ball_snapshot(ship_id).await.ok().flatten()?;

    let stopped_now = if is_turbo {
        if !session.has_gm_role() {
            return None;
        }
        let target_exists = ctx.sde.stations.contains_key(&target_id)
            || ctx.db.with_snapshot(|snap| snap.structures.contains_key(&target_id));
        if !target_exists {
            return None;
        }
        if matches!(ball.mode, MovementMode::Warp { .. }) {
            let _ = handle.force_stop_warp(ship_id);
            crate::services::space_destiny::clear_warp_tracking(ctx, Some(&handle), ship_id);
            true
        } else {
            false
        }
    } else {
        let now_unix = ctx.now_unix();
        if check_session_change_timer_active(ctx, char_id, now_unix, Some((sys_id, cur_tick)))
            .is_some()
        {
            return None;
        }
        let (target_pos, target_type_id, is_structure) =
            resolve_dock_target_for_settle(ctx, sys_id, service, method, target_id)?;
        let target_ball_radius = handle
            .get_ball_snapshot(target_id)
            .await
            .ok()
            .flatten()
            .map(|(_, b)| b.radius);
        let (target_radius, _) = resolve_physical_radius(
            ctx,
            target_type_id,
            Some(target_id),
            PhysicalRadiusInput {
                item_radius: target_ball_radius,
                ..Default::default()
            },
        );
        let effective_ball = resolve_ship_ball_for_perimeter(ctx, &ball);
        let arrived_at_warp_dest =
            evaluate_dock_perimeter(&effective_ball, target_pos, target_radius).ok()?;
        if crate::services::combat_crimewatch::check_crimewatch_action_restriction(
            ctx, char_id, ship_id, sys_id, "dock", now_unix,
        )
        .is_err()
        {
            return None;
        }
        if is_structure {
            let acl_ok = ctx.db.with_snapshot(|snap| {
                crate::services::structure_acl_defense_rules::verify_structure_docking_acl(
                    ctx, snap, target_id, char_id,
                )
                .is_ok()
            });
            if !acl_ok || handle.is_ball_scrambled(ship_id).await.unwrap_or(false) {
                return None;
            }
        }
        if arrived_at_warp_dest {
            stop_arriving_warp_for_dock(ctx, &handle, &ball, target_pos, target_radius);
            true
        } else {
            false
        }
    };

    let settle_deadline = handle.warp_stop_settle_deadline(ship_id, WARP_DOCK_SETTLE_DURATION)?;
    Some(DockWarpSettlePlan {
        sys_id,
        ship_id,
        stopped_now,
        settle_deadline,
    })
}

pub use crate::services::space_navigation_advanced::deactivate_ship_modules_on_leave_space;

#[cfg(test)] #[path = "space_warp_jump_unit_tests.rs"] mod space_warp_jump_unit_tests;

#[cfg(test)]
mod jump_displacement_and_toll_tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn jump_session_lease_returns_typed_session_kicked_client_error() {
        let ctx = Arc::new(ServerContext::new());
        let mut sess = SessionState::new(11_111);
        sess.user_id = Some(1);
        sess.character_id = Some(90_000_001);

        ctx.db
            .acquire_online_lock(1, 11_111, Some(90_000_001), true, ctx.now_unix())
            .expect("lock by S1");
        ctx.db
            .acquire_online_lock(1, 22_222, Some(90_000_001), true, ctx.now_unix())
            .expect("S2 displaces S1");

        let err = check_jump_session_lease(&ctx, &sess, 90_000_001, true)
            .expect_err("displaced session must fail jump lease check");
        assert_eq!(err.code, "SessionKicked");
        assert!(
            matches!(
                err.client_error.as_deref(),
                Some(eve_proto::ClientError::SessionMismatch(_))
            ),
            "expected typed ClientError::SessionMismatch, got {:?}",
            err.client_error
        );

        let unrevoked = SessionState::new(33_333);
        let mut unrevoked_with_user = unrevoked;
        unrevoked_with_user.user_id = Some(1);
        unrevoked_with_user.character_id = Some(90_000_001);
        let err_multi = check_jump_session_lease(&ctx, &unrevoked_with_user, 90_000_001, true)
            .expect_err("non-owner session must fail jump lease check");
        assert_eq!(err_multi.code, "MultiLoginBlocked");
        assert!(
            matches!(
                err_multi.client_error.as_deref(),
                Some(eve_proto::ClientError::SessionMismatch(_))
            ),
            "expected typed ClientError::SessionMismatch on MultiLoginBlocked, got {:?}",
            err_multi.client_error
        );
    }

    #[tokio::test]
    async fn zarzakh_gate_jump_displaced_or_failed_preserves_isk_and_ball() {
        let ctx = Arc::new(ServerContext::new());
        let char_id = 90_000_001_i64;
        let (old_sys, ship_id, zarzakh_gate_id, dest_gate_id) = ctx.db.with_snapshot(|s| {
            let ch = &s.characters[&char_id];
            let (gate_id, dest_gate_id) = ctx
                .sde
                .jumps
                .iter()
                .find(|(_, j)| j.to_solar_system_id == SOLAR_SYSTEM_ZARZAKH)
                .map(|(&gid, j)| (gid, j.destination_stargate_id))
                .expect("SDE must include a stargate leading to Zarzakh");
            (ch.solar_system_id, ch.active_ship_id, gate_id, dest_gate_id)
        });
        let gate_from_sys = ctx.sde.jumps[&zarzakh_gate_id].from_solar_system_id;
        let gate_pos = ctx.sde.jumps[&zarzakh_gate_id].position;
        let start_isk = eve_db::IskCents::from_isk(500_000_000);
        let trig_gate = if old_sys == gate_from_sys {
            (zarzakh_gate_id, dest_gate_id)
        } else {
            ctx.db.with_mut_snapshot(|s| {
                let ch = s.characters.get_mut(&char_id).unwrap();
                ch.solar_system_id = gate_from_sys;
                ch.station_id = None;
                ch.structure_id = None;
                if let Some(sh) = s.items.get_mut(&ship_id) {
                    sh.location_id = i64::from(gate_from_sys);
                    sh.flag = eve_db::InventoryFlag::None;
                }
            });
            (zarzakh_gate_id, dest_gate_id)
        };
        let _ = trig_gate;

        ctx.db.with_mut_snapshot(|s| {
            let ch = s.characters.get_mut(&char_id).unwrap();
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = gate_from_sys;
            ch.balance = start_isk;
            if let Some(sh) = s.items.get_mut(&ship_id) {
                sh.location_id = i64::from(gate_from_sys);
                sh.flag = eve_db::InventoryFlag::None;
            }
        });
        let sys_handle = ctx.systems.get_or_spawn(gate_from_sys);
        sys_handle
            .spawn_ball(eve_destiny::Ball {
                id: ship_id,
                type_id: 587,
                position: gate_pos + DVec3::new(500.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 2.5,
                mass: 1_200_000.0,
                radius: 40.0,
                speed_fraction: 1.0,
                mode: eve_destiny::MovementMode::Stop,
            })
            .await
            .expect("spawn ball near Zarzakh gate");

        let mut sess = SessionState::new(33_001);
        sess.user_id = Some(1);
        sess.character_id = Some(char_id);
        sess.solar_system_id = Some(gate_from_sys);
        sess.ship_id = Some(ship_id);
        ctx.db
            .acquire_online_lock(1, sess.session_id, Some(char_id), true, ctx.now_unix())
            .expect("S1 online lock");

        let ctx_displace = Arc::clone(&ctx);
        let hook: PreTakeBallHook = Box::new(move || {
            let ctx_d = Arc::clone(&ctx_displace);
            Box::pin(async move {
                let _ = ctx_d.db.acquire_online_lock(
                    1,
                    33_002,
                    None,
                    true,
                    ctx_d.now_unix(),
                );
            })
        });
        set_pre_take_ball_hook_for_test(&ctx, char_id, Some(hook));

        let mut kw = HashMap::new();
        kw.insert("confirmToll".to_string(), PyRep::Bool(true));
        let err = handle(
            &ctx,
            &mut sess,
            None,
            "beyonce",
            "CmdStargateJump",
            &[PyRep::Integer(zarzakh_gate_id)],
            &kw,
        )
        .await
        .expect("handled")
        .expect_err("displaced Zarzakh jump must fail");
        assert_eq!(err.code, "SessionKicked");
        assert_eq!(
            ctx.db.with_snapshot(|s| s.characters[&char_id].balance),
            start_isk,
            "displaced Zarzakh jump must not debit gate toll"
        );
        assert!(
            sys_handle.get_ball_snapshot(ship_id).await.unwrap().is_some(),
            "ship ball must remain in origin system after displaced jump"
        );

        // Also verify refund_zarzakh_gate_toll restores debited toll cleanly on rollback.
        let half_isk = eve_db::IskCents::from_isk(499_990_000);
        let toll_cents = eve_db::IskCents::from_isk(10_000);
        ctx.db.with_mut_snapshot(|s| {
            s.characters.get_mut(&char_id).unwrap().balance = half_isk;
        });
        refund_zarzakh_gate_toll(&ctx, char_id, Some(toll_cents));
        assert_eq!(
            ctx.db.with_snapshot(|s| s.characters[&char_id].balance),
            start_isk,
            "refund_zarzakh_gate_toll must restore exact debited ISK"
        );
        assert!(
            ctx.db.with_snapshot(|s| {
                s.wallet_journal
                    .iter()
                    .any(|j| j.character_id == char_id && j.entry_type == "zarzakh_gate_toll_refund" && j.amount == toll_cents)
            }),
        "refund must record a zarzakh_gate_toll_refund journal entry"
        );
    }

    #[tokio::test]
    async fn displaced_jump_before_gate_activate_emits_no_gate_fx_or_stop_warp() {
        let ctx = Arc::new(ServerContext::new());
        let char_id = 90_000_001_i64;
        let old_sys = 30_000_142_u32;
        let gate = ctx
            .sde
            .jumps
            .values()
            .find(|j| j.from_solar_system_id == old_sys)
            .cloned()
            .expect("SDE stargate in 30000142");
        let ship_id = ctx.db.with_mut_snapshot(|s| {
            let ch = s.characters.get_mut(&char_id).unwrap();
            ch.solar_system_id = old_sys;
            ch.station_id = None;
            ch.structure_id = None;
            let ship_id = ch.active_ship_id;
            if let Some(sh) = s.items.get_mut(&ship_id) {
                sh.location_id = i64::from(old_sys);
                sh.flag = eve_db::InventoryFlag::None;
            }
            ship_id
        });
        let sys_handle = ctx.systems.get_or_spawn(old_sys);
        sys_handle
            .spawn_ball(eve_destiny::Ball {
                id: ship_id,
                type_id: 587,
                position: gate.position + DVec3::new(500.0, 0.0, 0.0),
                velocity: DVec3::new(100.0, 0.0, 0.0),
                max_velocity: 350.0,
                inertia: 2.0,
                mass: 1_100_000.0,
                radius: 40.0,
                speed_fraction: 1.0,
                mode: eve_destiny::MovementMode::Warp {
                    origin: gate.position + DVec3::new(200_000.0, 0.0, 0.0),
                    destination: gate.position,
                    warp_speed_au_s: 3.0,
                    total_distance: 200_000.0,
                    phase: eve_destiny::WarpPhase::Decelerating,
                    elapsed_warp_time: 10.0,
                },
            })
            .await
            .expect("spawn warping ball");

        ctx.advance_secs(12);
        let mut sess = SessionState::new(34_001);
        sess.user_id = Some(1);
        sess.character_id = Some(char_id);
        sess.solar_system_id = Some(old_sys);
        sess.ship_id = Some(ship_id);
        ctx.db
            .acquire_online_lock(1, sess.session_id, Some(char_id), true, ctx.now_unix())
            .expect("S1 online lock");
        ctx.db
            .acquire_online_lock(1, 34_002, Some(char_id), true, ctx.now_unix())
            .expect("S2 displaces S1");

        let kw = HashMap::new();
        let err = handle(
            &ctx,
            &mut sess,
            None,
            "beyonce",
            "CmdStargateJump",
            &[PyRep::Integer(gate.stargate_id)],
            &kw,
        )
        .await
        .expect("handled")
        .expect_err("displaced jump before GateActivate must fail");
        assert_eq!(err.code, "SessionKicked");

        let (_tick, evs, _updates, _seq) = sys_handle
            .drain_observer_updates_with_seq(ship_id)
            .await
            .expect("drain observer updates");
        assert!(
            !evs.iter().any(|ev| matches!(
                ev,
                eve_destiny::DestinyEvent::OnClientControllerEvent { event_name, .. }
                    if event_name == "GateActivate"
            ) || matches!(
                ev,
                eve_destiny::DestinyEvent::OnSpecialFXTimed { guid, .. }
                    if guid == "effects.JumpOut"
            )),
            "displaced jump must emit neither GateActivate nor effects.JumpOut: {evs:?}"
        );
        let (_, ball) = sys_handle
            .get_ball_snapshot(ship_id)
            .await
            .unwrap()
            .expect("ball remains in old system");
        assert!(
            matches!(ball.mode, eve_destiny::MovementMode::Warp { .. }),
            "displaced jump must not force-stop warp before GateActivate: {:?}",
            ball.mode
        );
    }

    #[test]
    fn commit_stargate_db_transition_rejects_displaced_session_atomically() {
        let ctx = ServerContext::new();
        let char_id = 90_000_001_i64;
        let old_sys = 30_000_142_u32;
        let dest_sys = 30_000_144_u32;
        let (ship_id, before_ver, before_epoch) = ctx.db.with_mut_snapshot(|s| {
            let ch = s.characters.get_mut(&char_id).unwrap();
            ch.solar_system_id = old_sys;
            ch.station_id = None;
            ch.structure_id = None;
            let ship_id = ch.active_ship_id;
            if let Some(sh) = s.items.get_mut(&ship_id) {
                sh.location_id = i64::from(old_sys);
                sh.flag = eve_db::InventoryFlag::None;
            }
            (ship_id, ch.version, ch.system_epoch)
        });

        let mut sess = SessionState::new(35_001);
        sess.user_id = Some(1);
        sess.character_id = Some(char_id);
        sess.solar_system_id = Some(old_sys);
        sess.ship_id = Some(ship_id);
        ctx.db
            .acquire_online_lock(1, sess.session_id, Some(char_id), true, ctx.now_unix())
            .expect("S1 online lock");
        // S2 takes online lock before atomic commit runs.
        ctx.db
            .acquire_online_lock(1, 35_002, Some(char_id), true, ctx.now_unix())
            .expect("S2 displaces S1");

        let err = commit_stargate_db_transition(
            &ctx,
            &sess,
            char_id,
            ship_id,
            dest_sys,
            Some((None, None, old_sys)),
            true,
            DVec3::ZERO,
            None,
        )
        .expect_err("atomic commit must reject displaced session");
        assert_eq!(err.code, "SessionKicked");

        ctx.db.with_snapshot(|s| {
            let ch = &s.characters[&char_id];
            assert_eq!(ch.solar_system_id, old_sys);
            assert_eq!(ch.version, before_ver);
            assert_eq!(ch.system_epoch, before_epoch);
            assert_eq!(s.items[&ship_id].location_id, i64::from(old_sys));
        });
    }

    #[tokio::test]
    async fn zarzakh_gate_jump_displaced_with_concurrent_toll_refunds_only_own_debit() {
        let ctx = Arc::new(ServerContext::new());
        let char_id = 90_000_001_i64;
        let (zarzakh_gate_id, dest_gate_id) = ctx
            .sde
            .jumps
            .iter()
            .find(|(_, j)| j.to_solar_system_id == SOLAR_SYSTEM_ZARZAKH)
            .map(|(&gid, j)| (gid, j.destination_stargate_id))
            .expect("SDE must include a stargate leading to Zarzakh");
        let gate_from_sys = ctx.sde.jumps[&zarzakh_gate_id].from_solar_system_id;
        let gate_pos = ctx.sde.jumps[&zarzakh_gate_id].position;
        let start_isk = eve_db::IskCents::from_isk(500_000_000);

        let ship_id = ctx.db.with_mut_snapshot(|s| {
            let ch = s.characters.get_mut(&char_id).unwrap();
            ch.solar_system_id = gate_from_sys;
            ch.station_id = None;
            ch.structure_id = None;
            ch.balance = start_isk;
            let sh_id = ch.active_ship_id;
            if let Some(sh) = s.items.get_mut(&sh_id) {
                sh.location_id = i64::from(gate_from_sys);
                sh.flag = eve_db::InventoryFlag::None;
            }
            sh_id
        });
        let ball = eve_destiny::Ball {
            id: ship_id,
            type_id: 587,
            position: gate_pos + DVec3::new(500.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.5,
            mass: 1_200_000.0,
            radius: 40.0,
            speed_fraction: 1.0,
            mode: eve_destiny::MovementMode::Stop,
        };
        let sys_handle = ctx.systems.get_or_spawn(gate_from_sys);
        sys_handle
            .spawn_ball(ball.clone())
            .await
            .expect("spawn ball near Zarzakh gate");

        let mut sess = SessionState::new(34_001);
        sess.user_id = Some(1);
        sess.character_id = Some(char_id);
        sess.solar_system_id = Some(gate_from_sys);
        sess.ship_id = Some(ship_id);
        ctx.db
            .acquire_online_lock(1, sess.session_id, Some(char_id), true, ctx.now_unix())
            .expect("S1 online lock");

        let mut kw = HashMap::new();
        kw.insert("confirmToll".to_string(), PyRep::Bool(true));

        let ctx_hook = Arc::clone(&ctx);
        let kw_hook = kw.clone();
        set_post_zarzakh_toll_hook_for_test(
            &ctx,
            char_id,
            Some(Box::new(move || {
                // While S1 is paused right after paying its own Zarzakh toll, S2 displaces S1
                // and debits a second concurrent Zarzakh toll for `char_id`.
                let _ = ctx_hook.db.acquire_online_lock(
                    1,
                    34_002,
                    Some(char_id),
                    true,
                    ctx_hook.now_unix(),
                );
                let s2_toll = crate::services::space_navigation_advanced::process_zarzakh_gate_toll_with_amount(
                    &ctx_hook,
                    char_id,
                    ship_id,
                    zarzakh_gate_id,
                    dest_gate_id,
                    &ball,
                    &[],
                    &kw_hook,
                )
                .expect("S2 Zarzakh toll succeeds")
                .expect("S2 toll debited");
                assert_eq!(s2_toll, eve_db::IskCents::from_isk(10_000));
            })),
        );

        let err = handle(
            &ctx,
            &mut sess,
            None,
            "beyonce",
            "CmdStargateJump",
            &[PyRep::Integer(zarzakh_gate_id)],
            &kw,
        )
        .await
        .expect("handled")
        .expect_err("displaced S1 jump must unwind cleanly");
        assert_eq!(err.code, "SessionKicked");

        // Total debits = S1 (10k) + S2 (10k); S1 rollback must refund ONLY its own 10k toll.
        let expected_after_s2_debit_and_s1_refund = eve_db::IskCents::from_isk(499_990_000);
        ctx.db.with_snapshot(|s| {
            assert_eq!(
                s.characters[&char_id].balance,
                expected_after_s2_debit_and_s1_refund,
                "displaced S1 must refund only its own 10k ISK toll, never S2's concurrent toll"
            );
            let refunds: Vec<_> = s
                .wallet_journal
                .iter()
                .filter(|j| j.character_id == char_id && j.entry_type == "zarzakh_gate_toll_refund")
                .collect();
            assert_eq!(refunds.len(), 1);
            assert_eq!(refunds[0].amount, eve_db::IskCents::from_isk(10_000));
        });
        assert!(
            sys_handle.get_ball_snapshot(ship_id).await.unwrap().is_some(),
            "ship ball must be restored cleanly in origin system"
        );
    }

    #[derive(Clone, Copy)]
    struct DestAttackerSpec {
        target_ship_id: i64,
        atk_char_id: i64,
        atk_ship_id: i64,
        atk_scram_id: i64,
        scram_type_id: u32,
        atk_master_gun_id: i64,
        atk_slave_gun_id: i64,
        gun_type_id: u32,
        atk_ammo_tid: u32,
    }

    struct RollbackChargeFixture {
        gate: eve_sde::StargateJump,
        char_id: i64,
        ship_id: i64,
        ab_mod_id: i64,
        master_gun_id: i64,
        slave_gun_id: i64,
        origin_ammo_tid: u32,
        atk: DestAttackerSpec,
        sess: SessionState,
        old_handle: crate::system_actor::SystemHandle,
        dest_handle: crate::system_actor::SystemHandle,
    }

    async fn setup_rollback_charge_and_attacker_fixture(
        ctx: &Arc<ServerContext>,
        seed: i64,
    ) -> RollbackChargeFixture {
        let gate = ctx
            .sde
            .jumps
            .values()
            .filter(|j| {
                j.from_solar_system_id != j.to_solar_system_id
                    && j.from_solar_system_id != SOLAR_SYSTEM_ZARZAKH
                    && j.to_solar_system_id != SOLAR_SYSTEM_ZARZAKH
                    && ctx
                        .sde
                        .solar_systems
                        .get(&j.from_solar_system_id)
                        .is_some_and(|s| s.security_status >= 0.45 && s.region_id != 10_000_070)
                    && ctx
                        .sde
                        .solar_systems
                        .get(&j.to_solar_system_id)
                        .is_some_and(|s| s.security_status >= 0.45 && s.region_id != 10_000_070)
                    && ctx.sde.jumps.contains_key(&j.destination_stargate_id)
            })
            .min_by_key(|j| j.stargate_id)
            .cloned()
            .expect("SDE must contain a non-Zarzakh non-Pochven highsec roundtrip stargate");

        let char_id = 91_467_000 + seed * 10 + 1;
        let ship_id = 899_467_000 + seed * 100 + 1;
        let ab_mod_id = ship_id + 10;
        let master_gun_id = ship_id + 11;
        let slave_gun_id = ship_id + 12;

        let atk_char_id = 91_467_000 + seed * 10 + 2;
        let atk_ship_id = 899_467_000 + seed * 100 + 50;
        let atk_scram_id = atk_ship_id + 10;
        let atk_master_gun_id = atk_ship_id + 11;
        let atk_slave_gun_id = atk_ship_id + 12;

        let ship_type_id = 587_u32;
        let ab_type_id = ctx
            .sde
            .types
            .values()
            .filter(|t| t.group_id == 46 && t.published)
            .min_by_key(|t| t.type_id)
            .map(|t| t.type_id)
            .unwrap_or(12058);
        let gun_type_id = ctx
            .sde
            .types
            .values()
            .filter(|t| t.group_id == 55 && t.published)
            .min_by_key(|t| t.type_id)
            .map(|t| t.type_id)
            .unwrap_or(484);
        let scram_type_id = ctx
            .sde
            .types
            .values()
            .filter(|t| t.group_id == 52 && t.published)
            .min_by_key(|t| t.type_id)
            .map(|t| t.type_id)
            .unwrap_or(3242);
        let origin_ammo_tid = ctx
            .sde
            .types
            .values()
            .filter(|t| t.category_id == 8 && t.published)
            .min_by_key(|t| t.type_id)
            .map(|t| t.type_id)
            .unwrap_or(200);
        let atk_ammo_tid = origin_ammo_tid;

        let uid = ctx
            .db
            .provision_player_account(&format!("rb1467_{char_id}"), "pw");
        let atk_uid = ctx
            .db
            .provision_player_account(&format!("rb1467_atk_{atk_char_id}"), "pw");

        let origin_pos = gate.position + DVec3::new(500.0, 0.0, 0.0);
        ctx.db.with_mut_snapshot(|snap| {
            let template = snap
                .characters
                .values()
                .next()
                .cloned()
                .expect("demo seed character");
            let mut ch = template.clone();
            ch.character_id = char_id;
            ch.user_id = uid;
            ch.solar_system_id = gate.from_solar_system_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
            snap.characters.insert(char_id, ch);

            let mut atk_ch = template;
            atk_ch.character_id = atk_char_id;
            atk_ch.user_id = atk_uid;
            atk_ch.solar_system_id = gate.to_solar_system_id;
            atk_ch.station_id = None;
            atk_ch.structure_id = None;
            atk_ch.active_ship_id = atk_ship_id;
            snap.characters.insert(atk_char_id, atk_ch);

            for (iid, tid, owner, loc, flag) in [
                (ship_id, ship_type_id, char_id, i64::from(gate.from_solar_system_id), eve_db::InventoryFlag::None),
                (ab_mod_id, ab_type_id, char_id, ship_id, eve_db::InventoryFlag::MedSlot0),
                (master_gun_id, gun_type_id, char_id, ship_id, eve_db::InventoryFlag::HiSlot0),
                (slave_gun_id, gun_type_id, char_id, ship_id, eve_db::InventoryFlag::HiSlot1),
                (atk_ship_id, ship_type_id, atk_char_id, i64::from(gate.to_solar_system_id), eve_db::InventoryFlag::None),
                (atk_scram_id, scram_type_id, atk_char_id, atk_ship_id, eve_db::InventoryFlag::MedSlot0),
                (atk_master_gun_id, gun_type_id, atk_char_id, atk_ship_id, eve_db::InventoryFlag::HiSlot0),
                (atk_slave_gun_id, gun_type_id, atk_char_id, atk_ship_id, eve_db::InventoryFlag::HiSlot1),
            ] {
                snap.items.insert(
                    iid,
                    eve_db::InventoryItemRecord {
                        item_id: iid,
                        type_id: tid,
                        owner_id: owner,
                        location_id: loc,
                        flag,
                        quantity: 1,
                        singleton: true,
                        ..Default::default()
                    },
                );
                if loc == ship_id || loc == atk_ship_id {
                    snap.module_online_states.insert(iid, true);
                }
            }
            snap.item_positions.insert(ship_id, origin_pos.to_array());

            // Origin fit: uncharged active AB + charged master turret + charge-only slave turret.
            snap.module_active_states.insert(ab_mod_id);
            snap.module_active_states.insert(master_gun_id);
            snap.module_active_charges.insert(master_gun_id, origin_ammo_tid);
            snap.module_active_charges.insert(slave_gun_id, origin_ammo_tid);

            // Attacker fit: active scram + active master turret + charge-only slave turret.
            snap.module_active_states.insert(atk_scram_id);
            snap.module_active_states.insert(atk_master_gun_id);
            snap.module_active_charges.insert(atk_master_gun_id, atk_ammo_tid);
            snap.module_active_charges.insert(atk_slave_gun_id, atk_ammo_tid);
        });

        let old_handle = ctx.systems.get_or_spawn(gate.from_solar_system_id);
        let origin_cs = ctx.build_ship_combat_state(char_id, ship_id, ship_type_id);
        old_handle
            .spawn_ball_with_combat(
                eve_destiny::Ball {
                    id: ship_id,
                    type_id: ship_type_id,
                    position: origin_pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 350.0,
                    inertia: 2.0,
                    mass: 1_100_000.0,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                origin_cs,
            )
            .await
            .expect("spawn origin ball");
        old_handle
            .add_module_cycle(crate::system_actor::ActiveModuleCycle {
                ship_id,
                module_id: ab_mod_id,
                module_type_id: ab_type_id,
                owner_char_id: Some(char_id),
                effect_name: "moduleBonusAfterburner".into(),
                repeat: 100,
                duration_s: 10.0,
                cap_need_per_cycle: 10.0,
                speed_boost_factor: 1.5,
                ..Default::default()
            })
            .await
            .expect("add origin ab cycle");
        old_handle
            .add_module_cycle(crate::system_actor::ActiveModuleCycle {
                ship_id,
                module_id: master_gun_id,
                module_type_id: gun_type_id,
                owner_char_id: Some(char_id),
                effect_name: "targetAttack".into(),
                repeat: 100,
                duration_s: 10.0,
                cap_need_per_cycle: 1.0,
                linked_turrets: vec![slave_gun_id],
                ammo_type_id: Some(origin_ammo_tid),
                ..Default::default()
            })
            .await
            .expect("add origin master gun cycle");

        let dest_handle = ctx.systems.get_or_spawn(gate.to_solar_system_id);
        let atk_cs = ctx.build_ship_combat_state(atk_char_id, atk_ship_id, ship_type_id);
        dest_handle
            .spawn_ball_with_combat(
                eve_destiny::Ball {
                    id: atk_ship_id,
                    type_id: ship_type_id,
                    position: DVec3::new(1_000.0, 0.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 350.0,
                    inertia: 2.0,
                    mass: 1_100_000.0,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                atk_cs,
            )
            .await
            .expect("spawn attacker ball");

        let mut sess = SessionState::new(77_000 + seed as u64);
        sess.user_id = Some(uid);
        sess.character_id = Some(char_id);
        sess.solar_system_id = Some(gate.from_solar_system_id);
        sess.ship_id = Some(ship_id);
        ctx.db
            .acquire_online_lock(uid, sess.session_id, Some(char_id), true, ctx.now_unix())
            .expect("online lock");

        RollbackChargeFixture {
            gate,
            char_id,
            ship_id,
            ab_mod_id,
            master_gun_id,
            slave_gun_id,
            origin_ammo_tid,
            atk: DestAttackerSpec {
                target_ship_id: ship_id,
                atk_char_id,
                atk_ship_id,
                atk_scram_id,
                scram_type_id,
                atk_master_gun_id,
                atk_slave_gun_id,
                gun_type_id,
                atk_ammo_tid,
            },
            sess,
            old_handle,
            dest_handle,
        }
    }

    async fn attach_dest_attacker_cycles_on(
        dest_handle: &crate::system_actor::SystemHandle,
        atk: DestAttackerSpec,
    ) {
        dest_handle
            .add_module_cycle(crate::system_actor::ActiveModuleCycle {
                ship_id: atk.atk_ship_id,
                module_id: atk.atk_scram_id,
                module_type_id: atk.scram_type_id,
                owner_char_id: Some(atk.atk_char_id),
                effect_name: "warpScramble".into(),
                target_id: Some(atk.target_ship_id),
                repeat: 100,
                duration_s: 10.0,
                ..Default::default()
            })
            .await
            .expect("add dest attacker scram cycle");
        dest_handle
            .add_module_cycle(crate::system_actor::ActiveModuleCycle {
                ship_id: atk.atk_ship_id,
                module_id: atk.atk_master_gun_id,
                module_type_id: atk.gun_type_id,
                owner_char_id: Some(atk.atk_char_id),
                effect_name: "targetAttack".into(),
                target_id: Some(atk.target_ship_id),
                repeat: 100,
                duration_s: 10.0,
                linked_turrets: vec![atk.atk_slave_gun_id],
                ammo_type_id: Some(atk.atk_ammo_tid),
                ..Default::default()
            })
            .await
            .expect("add dest attacker linked gun cycle");
    }

    #[tokio::test]
    async fn post_spawn_rollback_preserves_origin_db_active_states_charges_and_cleans_attacker_cycles() {
        let ctx = Arc::new(ServerContext::new());
        let mut fx = setup_rollback_charge_and_attacker_fixture(&ctx, 1).await;

        let dest_handle_hook = fx.dest_handle.clone();
        let ship_id = fx.ship_id;
        let atk = fx.atk;
        let (materialized_tx, mut materialized_rx) = tokio::sync::oneshot::channel::<bool>();

        crate::system_actor::set_spawn_ball_with_meta_pre_reply_hook_for_test(
            fx.gate.to_solar_system_id,
            fx.ship_id,
            Some(Box::new(move || {
                Box::pin(async move {
                    let present = dest_handle_hook
                        .get_ball_snapshot(ship_id)
                        .await
                        .unwrap()
                        .is_some();
                    if present {
                        attach_dest_attacker_cycles_on(&dest_handle_hook, atk).await;
                    }
                    let _ = materialized_tx.send(present);
                    std::future::pending::<()>().await;
                })
            })),
        );

        let args = [PyRep::Integer(fx.gate.stargate_id)];
        let kw = HashMap::new();
        {
            let mut jump_fut = Box::pin(handle(
                &ctx,
                &mut fx.sess,
                None,
                "beyonce",
                "CmdStargateJump",
                &args,
                &kw,
            ));
            tokio::select! {
                biased;
                res = &mut materialized_rx => {
                    assert!(
                        res.expect("pre_reply_hook signal"),
                        "destination ball must be materialized before cancellation"
                    );
                }
                res = &mut jump_fut => {
                    panic!("expected future cancellation during pre_reply_hook suspension, got {res:?}");
                }
            }
            drop(jump_fut);
        }

        let mut rolled_back = false;
        for _ in 0..100 {
            let dest_gone = fx
                .dest_handle
                .get_ball_snapshot(fx.ship_id)
                .await
                .unwrap()
                .is_none();
            let orig_back = fx
                .old_handle
                .get_ball_snapshot(fx.ship_id)
                .await
                .unwrap()
                .is_some();
            if dest_gone && orig_back {
                rolled_back = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(
            rolled_back,
            "post-spawn cancellation must remove destination ball and restore origin ball"
        );

        // Origin DB `module_active_states` and `module_active_charges` (including charge-only slave turret)
        // must survive `RemoveBallIfToken`, while destination attacker DB states must be cleared.
        ctx.db.with_snapshot(|snap| {
            assert!(
                snap.module_active_states.contains(&fx.ab_mod_id),
                "restored origin AB must remain in DB module_active_states"
            );
            assert!(
                snap.module_active_states.contains(&fx.master_gun_id),
                "restored origin master weapon must remain in DB module_active_states"
            );
            assert!(
                !snap.module_active_states.contains(&fx.slave_gun_id),
                "charge-only linked slave turret must not be fabricated in module_active_states"
            );
            assert!(
                !snap.module_active_charges.contains_key(&fx.ab_mod_id),
                "uncharged origin AB must not gain a module_active_charges entry"
            );
            assert_eq!(
                snap.module_active_charges.get(&fx.master_gun_id).copied(),
                Some(fx.origin_ammo_tid),
                "origin master weapon charge must survive RemoveBallIfToken"
            );
            assert_eq!(
                snap.module_active_charges.get(&fx.slave_gun_id).copied(),
                Some(fx.origin_ammo_tid),
                "origin linked slave turret charge must survive RemoveBallIfToken"
            );

            assert!(!snap.module_active_states.contains(&fx.atk.atk_scram_id));
            assert!(!snap.module_active_states.contains(&fx.atk.atk_master_gun_id));
            assert!(!snap.module_active_charges.contains_key(&fx.atk.atk_master_gun_id));
            assert!(!snap.module_active_charges.contains_key(&fx.atk.atk_slave_gun_id));
        });

        // Origin actor cycles must survive in sync and keep advancing; destination attacker cycles must stop.
        assert!(fx.old_handle.view().active_module_ships.contains(&fx.ship_id));
        assert!(fx.old_handle.has_module_cycle(fx.ship_id, fx.ab_mod_id).await);
        assert!(fx.old_handle.has_module_cycle(fx.ship_id, fx.master_gun_id).await);
        assert!(!fx.dest_handle.has_module_cycle(fx.atk.atk_ship_id, fx.atk.atk_scram_id).await);
        assert!(!fx.dest_handle.has_module_cycle(fx.atk.atk_ship_id, fx.atk.atk_master_gun_id).await);

        let atk_notes = ctx.drain_notifications_for_char(fx.atk.atk_char_id);
        assert!(
            atk_notes.iter().any(|n| n.event == "OnGodmaShipEffect"),
            "destination attacker targeting rolled-back ship must receive OnGodmaShipEffect deactivation"
        );
        let orig_notes = ctx.drain_notifications_for_char(fx.char_id);
        assert!(
            !orig_notes.iter().any(|n| n.event == "OnGodmaShipEffect"),
            "origin ship must not receive OnGodmaShipEffect deactivation during post-spawn rollback"
        );

        fx.old_handle.step_manual(1.0).await.expect("origin step advances");
        let st = fx.old_handle.actor_state.read().unwrap();
        let ab_elapsed = st
            .active_module_cycles
            .iter()
            .find(|c| c.ship_id == fx.ship_id && c.module_id == fx.ab_mod_id)
            .map(|c| c.elapsed_s);
        let gun_elapsed = st
            .active_module_cycles
            .iter()
            .find(|c| c.ship_id == fx.ship_id && c.module_id == fx.master_gun_id)
            .map(|c| c.elapsed_s);
        assert!(ab_elapsed.is_some_and(|e| e >= 0.99), "origin AB cycle must keep advancing: {ab_elapsed:?}");
        assert!(gun_elapsed.is_some_and(|e| e >= 0.99), "origin gun cycle must keep advancing: {gun_elapsed:?}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn deferred_under_lock_post_spawn_rollback_preserves_db_active_states_and_slave_charges() {
        let ctx = Arc::new(ServerContext::new());
        let fx = setup_rollback_charge_and_attacker_fixture(&ctx, 2).await;

        let taken = fx
            .old_handle
            .take_ball_preserving_db(fx.ship_id)
            .await
            .expect("take origin ball preserving DB");
        let mut guard = StargateRollbackGuard::new(
            &ctx,
            fx.char_id,
            fx.ship_id,
            TakenBallGuard::new(fx.old_handle.clone(), taken.clone()),
        );
        let (_ch, db_rb) = commit_stargate_db_transition(
            &ctx,
            &fx.sess,
            fx.char_id,
            fx.ship_id,
            fx.gate.to_solar_system_id,
            Some((None, None, fx.gate.from_solar_system_id)),
            true,
            DVec3::new(1_000.0, 0.0, 0.0),
            taken.combat_state.as_ref(),
        )
        .expect("commit_stargate_db_transition succeeds");

        assert_eq!(db_rb.prev_active_mods, vec![fx.ab_mod_id, fx.master_gun_id]);
        assert_eq!(
            db_rb.prev_active_charges,
            vec![
                (fx.master_gun_id, fx.origin_ammo_tid),
                (fx.slave_gun_id, fx.origin_ammo_tid),
            ]
        );
        ctx.db.with_snapshot(|snap| {
            assert!(!snap.module_active_states.contains(&fx.ab_mod_id));
            assert!(!snap.module_active_states.contains(&fx.master_gun_id));
            assert!(!snap.module_active_charges.contains_key(&fx.master_gun_id));
            assert!(
                !snap.module_active_charges.contains_key(&fx.slave_gun_id),
                "commit_stargate_db_transition must clear charge-only slave turret charges too"
            );
        });
        guard.record_db_commit(db_rb);

        let spawn_token = next_stargate_spawn_token();
        let mut dest_ball = taken.ball.clone();
        dest_ball.position = DVec3::new(1_000.0, 0.0, 0.0);
        guard.record_dest_spawn_target(fx.dest_handle.clone(), fx.ship_id, spawn_token);
        fx.dest_handle
            .spawn_ball_with_meta_tagged(
                dest_ball,
                taken.combat_state.clone().unwrap_or_default(),
                ctx.build_ship_slim_meta(fx.char_id, 587),
                spawn_token,
            )
            .await
            .expect("materialize tagged destination ball");

        attach_dest_attacker_cycles_on(&fx.dest_handle, fx.atk).await;

        if cfg!(any(debug_assertions, feature = "lock-rank")) {
            ctx.db.with_mut_snapshot(|_| {
                drop(guard);
            });
        } else {
            drop(guard);
        }

        let mut rolled_back = false;
        for _ in 0..100 {
            let db_restored = ctx.db.with_snapshot(|snap| {
                snap.characters[&fx.char_id].solar_system_id == fx.gate.from_solar_system_id
                    && snap.module_active_states.contains(&fx.ab_mod_id)
                    && snap.module_active_states.contains(&fx.master_gun_id)
                    && snap.module_active_charges.get(&fx.master_gun_id).copied() == Some(fx.origin_ammo_tid)
                    && snap.module_active_charges.get(&fx.slave_gun_id).copied() == Some(fx.origin_ammo_tid)
            });
            let dest_gone = fx
                .dest_handle
                .get_ball_snapshot(fx.ship_id)
                .await
                .unwrap()
                .is_none();
            let orig_back = fx
                .old_handle
                .get_ball_snapshot(fx.ship_id)
                .await
                .unwrap()
                .is_some();
            let atk_stopped = !fx.dest_handle.has_module_cycle(fx.atk.atk_ship_id, fx.atk.atk_scram_id).await
                && !fx.dest_handle.has_module_cycle(fx.atk.atk_ship_id, fx.atk.atk_master_gun_id).await;
            if db_restored && dest_gone && orig_back && atk_stopped {
                rolled_back = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(
            rolled_back,
            "deferred post-spawn rollback must restore DB active states/slave charges + origin cycles while deactivating destination attacker"
        );

        ctx.db.with_snapshot(|snap| {
            assert!(!snap.module_active_states.contains(&fx.slave_gun_id));
            assert!(!snap.module_active_charges.contains_key(&fx.ab_mod_id));
            assert!(!snap.module_active_states.contains(&fx.atk.atk_scram_id));
            assert!(!snap.module_active_states.contains(&fx.atk.atk_master_gun_id));
            assert!(!snap.module_active_charges.contains_key(&fx.atk.atk_master_gun_id));
            assert!(!snap.module_active_charges.contains_key(&fx.atk.atk_slave_gun_id));
        });
        assert!(fx.old_handle.view().active_module_ships.contains(&fx.ship_id));
        assert!(fx.old_handle.has_module_cycle(fx.ship_id, fx.ab_mod_id).await);
        assert!(fx.old_handle.has_module_cycle(fx.ship_id, fx.master_gun_id).await);
    }
}
