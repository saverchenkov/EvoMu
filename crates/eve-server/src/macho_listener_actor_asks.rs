//! Bounded `SystemActor` post-RPC ask and Destiny resync helpers for `macho_listener` (`#1170`).

use super::*;

const MAX_SET_STATE_RESYNC_ATTEMPTS: usize = 3;

pub(super) struct ResyncedSetState {
    pub(super) tick: u64,
    pub(super) events: Vec<eve_destiny::DestinyEvent>,
    pub(super) updates_pyrep: PyRep,
    pub(super) post_batches: Vec<crate::outbound_sequencer::DestinyBatch>,
}

/// Bounded deadline for post-RPC session asks against a solar system actor (`#1170`).
pub(super) const POST_RPC_ACTOR_ASK_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(2);

/// Single owner for handling a timed-out or failed `SystemActor` session ask (`#1170` A1).
///
/// Always places `wire_state` into the same `AwaitingSetState` recovery state as a lagged
/// broadcast (`on_broadcast_lagged`), logs the failure once, and formats the canonical retryable
/// `ServiceTemporarilyUnavailable` (`ClientError::CustomNotify`) error.
pub(super) fn on_actor_ask_timeout_or_failure(
    wire_state: &mut crate::outbound_sequencer::SessionWireState,
    label: &str,
    context: &str,
    err: impl std::fmt::Display,
) -> BridgeRpcError {
    wire_state.on_broadcast_lagged();
    let msg = format!("{label} ask timed out or failed ({context}): {err}");
    warn!(
        session_id = wire_state.session_id,
        ask = label,
        context,
        error = %msg,
        "SystemActor ask timed out or failed; marked stream AwaitingSetState"
    );
    BridgeRpcError::with_client_error(
        "ServiceTemporarilyUnavailable",
        msg.clone(),
        eve_proto::ClientError::CustomNotify(msg),
    )
}

/// Executes a `SystemActor` ask with a bounded deadline, routing both deadline expiry and actor
/// errors through [`on_actor_ask_timeout_or_failure`].
pub(super) async fn bounded_actor_ask<T, F>(
    wire_state: &mut crate::outbound_sequencer::SessionWireState,
    label: &str,
    context: &str,
    timeout: std::time::Duration,
    fut: F,
) -> std::result::Result<T, BridgeRpcError>
where
    F: std::future::Future<Output = std::result::Result<T, String>>,
{
    match tokio::time::timeout(timeout, fut).await {
        Ok(Ok(val)) => Ok(val),
        Ok(Err(err)) => Err(on_actor_ask_timeout_or_failure(
            wire_state,
            label,
            context,
            err,
        )),
        Err(_) => Err(on_actor_ask_timeout_or_failure(
            wire_state,
            label,
            context,
            format!("deadline {}ms exceeded", timeout.as_millis()),
        )),
    }
}

/// Flushes peer observer updates with a bounded deadline, transitioning `wire_state` via
/// [`on_actor_ask_timeout_or_failure`] if the actor ask times out or fails.
pub(super) async fn best_effort_flush_peer_observer_updates(
    wire_state: &mut crate::outbound_sequencer::SessionWireState,
    sys_handle: &crate::system_actor::SystemHandle,
    exclude_observer_id: Option<i64>,
    ask_timeout: std::time::Duration,
    context: &str,
) -> bool {
    bounded_actor_ask(
        wire_state,
        "flush_peer_observer_updates",
        context,
        ask_timeout,
        sys_handle.flush_peer_observer_updates(exclude_observer_id),
    )
    .await
    .is_ok()
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn resync_set_state_with_sys_rx_with_mode_bounded(
    ctx: &ServerContext,
    sys_handle: &crate::system_actor::SystemHandle,
    sys_rx: &mut tokio::sync::broadcast::Receiver<crate::system_actor::TickBroadcast>,
    sys_id: u32,
    observer_ball_id: i64,
    observer_ids: &[i64],
    wire_state: &mut crate::outbound_sequencer::SessionWireState,
    shared_structure_observer: bool,
    ask_timeout: std::time::Duration,
) -> std::result::Result<Option<ResyncedSetState>, BridgeRpcError> {
    let ep = wire_state.current_epoch();
    for _ in 0..MAX_SET_STATE_RESYNC_ATTEMPTS {
        let (tick, events, updates_pyrep, watermark_seq) = if shared_structure_observer {
            let (tick, _state_kv, updates_pyrep, watermark_seq) = bounded_actor_ask(
                wire_state,
                "get_set_state_with_seq",
                "set_state_resync",
                ask_timeout,
                sys_handle.get_set_state_with_seq(observer_ball_id),
            )
            .await?;
            (
                tick,
                vec![eve_destiny::DestinyEvent::SetState { observer_ball_id }],
                updates_pyrep,
                watermark_seq,
            )
        } else {
            bounded_actor_ask(
                wire_state,
                "push_set_state",
                "set_state_resync",
                ask_timeout,
                sys_handle.push_set_state(observer_ball_id),
            )
            .await?;
            let snapshot = bounded_actor_ask(
                wire_state,
                "drain_observer_updates_with_seq",
                "set_state_resync",
                ask_timeout,
                sys_handle.drain_observer_updates_with_seq(observer_ball_id),
            )
            .await?;
            sys_handle.record_warp_stops_from_events(&snapshot.1);
            snapshot
        };
        let mut post_batches = Vec::new();
        if !drain_sys_rx_discarding_up_to_seq(
            ctx,
            sys_rx,
            sys_id,
            observer_ids,
            ep,
            watermark_seq,
            &mut post_batches,
        ) {
            return Ok(Some(ResyncedSetState {
                tick,
                events,
                updates_pyrep,
                post_batches,
            }));
        }
    }
    wire_state.on_broadcast_lagged();
    Ok(None)
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
async fn resync_set_state_with_sys_rx_with_mode(
    ctx: &ServerContext,
    sys_handle: &crate::system_actor::SystemHandle,
    sys_rx: &mut tokio::sync::broadcast::Receiver<crate::system_actor::TickBroadcast>,
    sys_id: u32,
    observer_ball_id: i64,
    observer_ids: &[i64],
    wire_state: &mut crate::outbound_sequencer::SessionWireState,
    shared_structure_observer: bool,
) -> Option<ResyncedSetState> {
    resync_set_state_with_sys_rx_with_mode_bounded(
        ctx,
        sys_handle,
        sys_rx,
        sys_id,
        observer_ball_id,
        observer_ids,
        wire_state,
        shared_structure_observer,
        POST_RPC_ACTOR_ASK_TIMEOUT,
    )
    .await
    .ok()
    .flatten()
}

/// Pushes a fresh `SetState` snapshot for `ship_id`, drains it with its `watermark_seq`,
/// and catches up `sys_rx` while checking `drain_sys_rx_discarding_up_to_seq` continuity.
/// Retries boundedly if `sys_rx` suffers post-watermark lag during the resync; leaves `wire_state`
/// in `AwaitingSetState` (`on_broadcast_lagged()`) and returns `None` if retries exhaust or asks fail.
#[cfg(test)]
pub(super) async fn resync_set_state_with_sys_rx(
    ctx: &ServerContext,
    sys_handle: &crate::system_actor::SystemHandle,
    sys_rx: &mut tokio::sync::broadcast::Receiver<crate::system_actor::TickBroadcast>,
    sys_id: u32,
    ship_id: i64,
    observer_ids: &[i64],
    wire_state: &mut crate::outbound_sequencer::SessionWireState,
) -> Option<ResyncedSetState> {
    resync_set_state_with_sys_rx_with_mode(
        ctx,
        sys_handle,
        sys_rx,
        sys_id,
        ship_id,
        observer_ids,
        wire_state,
        false,
    )
    .await
}

/// Non-destructively snapshots `SetState` and `broadcast_seq` for shared static structure
/// observer `structure_id` (`#1461` F-2, F-1461-04) without enqueueing into or draining
/// `observer_events[structure_id]`, so concurrent pilots viewing the same structure never steal
/// each other's events or drop/replay same-tick broadcasts.
#[cfg(test)]
pub(super) async fn resync_structure_set_state_with_sys_rx(
    ctx: &ServerContext,
    sys_handle: &crate::system_actor::SystemHandle,
    sys_rx: &mut tokio::sync::broadcast::Receiver<crate::system_actor::TickBroadcast>,
    sys_id: u32,
    structure_id: i64,
    observer_ids: &[i64],
    wire_state: &mut crate::outbound_sequencer::SessionWireState,
) -> Option<ResyncedSetState> {
    resync_set_state_with_sys_rx_with_mode(
        ctx,
        sys_handle,
        sys_rx,
        sys_id,
        structure_id,
        observer_ids,
        wire_state,
        true,
    )
    .await
}

pub(super) struct ShipSwapDestinySync {
    pub(super) pre_batches: Vec<crate::outbound_sequencer::DestinyBatch>,
    pub(super) direct_batch: Option<crate::outbound_sequencer::DestinyBatch>,
    pub(super) post_batches: Vec<crate::outbound_sequencer::DestinyBatch>,
}

/// Synchronizes Destiny state across an asynchronous same-system in-space ship swap (`notify_rx`).
/// Splits `sys_rx` around `drain_observer_updates_with_seq`'s `watermark_seq`, updates live state on
/// `TryRecvError::Lagged`, and re-snapshots `SetState` whenever `wire_state.destiny` is
/// `AwaitingSetState` without a `SetState` or post-watermark `sys_rx` lag occurred (`#1172`).
#[allow(clippy::too_many_arguments)]
pub(super) async fn sync_in_space_ship_swap_destiny(
    ctx: &ServerContext,
    sys_handle: &crate::system_actor::SystemHandle,
    sys_rx: &mut tokio::sync::broadcast::Receiver<crate::system_actor::TickBroadcast>,
    wire_state: &mut crate::outbound_sequencer::SessionWireState,
    session_id: u64,
    sys_id: u32,
    new_ship_id: i64,
    prev_ship_id: Option<i64>,
    prev_in_same_space: bool,
) -> Option<ShipSwapDestinySync> {
    sync_in_space_ship_swap_destiny_bounded(
        ctx,
        sys_handle,
        sys_rx,
        wire_state,
        session_id,
        sys_id,
        new_ship_id,
        prev_ship_id,
        prev_in_same_space,
        POST_RPC_ACTOR_ASK_TIMEOUT,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn sync_in_space_ship_swap_destiny_bounded(
    ctx: &ServerContext,
    sys_handle: &crate::system_actor::SystemHandle,
    sys_rx: &mut tokio::sync::broadcast::Receiver<crate::system_actor::TickBroadcast>,
    wire_state: &mut crate::outbound_sequencer::SessionWireState,
    session_id: u64,
    sys_id: u32,
    new_ship_id: i64,
    prev_ship_id: Option<i64>,
    prev_in_same_space: bool,
    ask_timeout: std::time::Duration,
) -> Option<ShipSwapDestinySync> {
    let mut ep = wire_state.ensure_in_space(Some(sys_id));
    let (mut tick, mut events, mut updates_pyrep, watermark_seq) = bounded_actor_ask(
        wire_state,
        "drain_observer_updates_with_seq",
        "ship_swap",
        ask_timeout,
        sys_handle.drain_observer_updates_with_seq(new_ship_id),
    )
    .await
    .ok()?;
    sys_handle.record_warp_stops_from_events(&events);
    let has_set_state = events
        .iter()
        .any(|e| matches!(e, eve_destiny::DestinyEvent::SetState { .. }));
    let suppressed_ids = [new_ship_id, prev_ship_id.unwrap_or(new_ship_id)];
    let mut pre_batches = Vec::new();
    let mut post_batches = Vec::new();
    let mut is_live_same_system = matches!(
        wire_state.destiny,
        crate::outbound_sequencer::DestinyStream::Live { .. }
    ) && prev_in_same_space;
    let mut post_lagged = false;
    if matches!(
        wire_state.destiny,
        crate::outbound_sequencer::DestinyStream::Live { .. }
    ) {
        let (next_ep, next_live) = drain_pending_tick_batches_for_sys_with_watermark(
            ctx,
            sys_rx,
            sys_id,
            &suppressed_ids,
            wire_state,
            Some(watermark_seq),
            &mut pre_batches,
            &mut post_batches,
            session_id,
        );
        ep = next_ep;
        is_live_same_system = is_live_same_system && next_live;
        if !next_live && has_set_state {
            post_lagged = true;
        }
    } else if has_set_state {
        post_lagged = drain_sys_rx_discarding_up_to_seq(
            ctx,
            sys_rx,
            sys_id,
            &suppressed_ids,
            ep,
            watermark_seq,
            &mut post_batches,
        );
    }

    let mut aborted_resync = false;
    if (matches!(
        wire_state.destiny,
        crate::outbound_sequencer::DestinyStream::AwaitingSetState { .. }
    ) && !has_set_state
        && !is_live_same_system)
        || post_lagged
    {
        post_batches.clear();
        if let Some(resynced) = resync_set_state_with_sys_rx_with_mode_bounded(
            ctx,
            sys_handle,
            sys_rx,
            sys_id,
            new_ship_id,
            &suppressed_ids,
            wire_state,
            false,
            ask_timeout,
        )
        .await
        .ok()
        .flatten()
        {
            tick = resynced.tick;
            events = resynced.events;
            updates_pyrep = resynced.updates_pyrep;
            post_batches = resynced.post_batches;
            ep = wire_state.current_epoch();
        } else {
            aborted_resync = true;
        }
    }

    let direct_batch = if aborted_resync {
        None
    } else {
        build_destiny_batch(
            ctx,
            ep,
            tick,
            &events,
            updates_pyrep,
            &suppressed_ids,
            SelfDestinyWireContext::NonDock,
            crate::outbound_scene_scope::space_scope(
                Some(sys_id),
                Some(ctx.session_scene_epoch(session_id)),
            ),
        )
    };
    Some(ShipSwapDestinySync {
        pre_batches,
        direct_batch,
        post_batches,
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn emit_live_space_tick_set_state_resync<W: AsyncWriteExt + Unpin>(
    ctx: &ServerContext,
    sys_handle: &crate::system_actor::SystemHandle,
    writer: &mut W,
    wire_state: &mut crate::outbound_sequencer::SessionWireState,
    sys_rx: &mut tokio::sync::broadcast::Receiver<crate::system_actor::TickBroadcast>,
    client_id: u64,
    session: &SessionState,
    sys_id: u32,
    observer_id: i64,
    reason: &str,
) -> std::result::Result<(), ()> {
    emit_live_space_tick_set_state_resync_bounded(
        ctx,
        sys_handle,
        writer,
        wire_state,
        sys_rx,
        client_id,
        session,
        sys_id,
        observer_id,
        reason,
        POST_RPC_ACTOR_ASK_TIMEOUT,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn emit_live_space_tick_set_state_resync_bounded<W: AsyncWriteExt + Unpin>(
    ctx: &ServerContext,
    sys_handle: &crate::system_actor::SystemHandle,
    writer: &mut W,
    wire_state: &mut crate::outbound_sequencer::SessionWireState,
    sys_rx: &mut tokio::sync::broadcast::Receiver<crate::system_actor::TickBroadcast>,
    client_id: u64,
    session: &SessionState,
    sys_id: u32,
    observer_id: i64,
    reason: &str,
    ask_timeout: std::time::Duration,
) -> std::result::Result<(), ()> {
    let shared_structure_observer = is_shared_structure_observer(session, observer_id);
    let resynced_opt = resync_set_state_with_sys_rx_with_mode_bounded(
        ctx,
        sys_handle,
        sys_rx,
        sys_id,
        observer_id,
        &[observer_id],
        wire_state,
        shared_structure_observer,
        ask_timeout,
    )
    .await
    .ok()
    .flatten();
    let Some(resynced) = resynced_opt else {
        return Ok(());
    };
    if !session_owns_char_notifications(ctx, session) {
        return Ok(());
    }
    let ep = wire_state.current_epoch();
    let set_state_write_failed = if let Some(batch) = build_destiny_batch(
        ctx,
        ep,
        resynced.tick,
        &resynced.events,
        resynced.updates_pyrep,
        &[observer_id],
        SelfDestinyWireContext::NonDock,
        crate::outbound_scene_scope::space_scope(
            session.solar_system_id,
            Some(wire_state.scene_epoch),
        ),
    ) {
        emit_tick_batch_with_observability(
            ctx,
            wire_state,
            writer,
            client_id,
            session,
            batch,
            reason,
        )
        .await
        .is_err()
    } else {
        false
    };
    if set_state_write_failed
        || emit_tick_batches_with_observability(
            ctx,
            wire_state,
            writer,
            client_id,
            session,
            resynced.post_batches,
            "system_tick_broadcast",
        )
        .await
        .is_err()
    {
        return Err(());
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn emit_live_space_tick_broadcast<W: AsyncWriteExt + Unpin>(
    ctx: &ServerContext,
    writer: &mut W,
    wire_state: &mut crate::outbound_sequencer::SessionWireState,
    client_id: u64,
    session: &SessionState,
    observer_id: i64,
    tick_msg: &crate::system_actor::TickBroadcast,
) -> std::result::Result<(), ()> {
    if let Some(updates_pyrep) = tick_msg.observer_updates_pyrep.get(&observer_id) {
        let evs = tick_msg
            .observer_events
            .get(&observer_id)
            .map(Vec::as_slice)
            .unwrap_or(&tick_msg.events);
        let ep = wire_state.current_epoch();
        if let Some(batch) = build_destiny_batch(
            ctx,
            ep,
            tick_msg.tick,
            evs,
            updates_pyrep.clone(),
            &[observer_id],
            SelfDestinyWireContext::NonDock,
            crate::outbound_scene_scope::space_scope(Some(tick_msg.solar_system_id), None),
        ) {
            if emit_tick_batch_with_observability(
                ctx,
                wire_state,
                writer,
                client_id,
                session,
                batch,
                "system_tick_broadcast",
            )
            .await
            .is_err()
            {
                return Err(());
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_live_space_tick_broadcast<W: AsyncWriteExt + Unpin>(
    ctx: &ServerContext,
    sys_handle: Option<&crate::system_actor::SystemHandle>,
    writer: &mut W,
    wire_state: &mut crate::outbound_sequencer::SessionWireState,
    sys_rx: Option<&mut tokio::sync::broadcast::Receiver<crate::system_actor::TickBroadcast>>,
    client_id: u64,
    session: &SessionState,
    observer_id: i64,
    tick_msg: &crate::system_actor::TickBroadcast,
) -> std::result::Result<(), ()> {
    if wire_state.needs_awaiting_set_state_resync() {
        if let (Some(sys_id), Some(rx), Some(sys_handle)) =
            (session.solar_system_id, sys_rx, sys_handle)
        {
            emit_live_space_tick_set_state_resync(
                ctx,
                sys_handle,
                writer,
                wire_state,
                rx,
                client_id,
                session,
                sys_id,
                observer_id,
                "awaiting_set_state_resync_tick",
            )
            .await?;
        }
        return Ok(());
    }
    emit_live_space_tick_broadcast(
        ctx,
        writer,
        wire_state,
        client_id,
        session,
        observer_id,
        tick_msg,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run_death_respawn_docking_ask_bounded(
    wire_state: &mut crate::outbound_sequencer::SessionWireState,
    old_sys_handle: &crate::system_actor::SystemHandle,
    old_obs_id: i64,
    ask_timeout: std::time::Duration,
) -> std::result::Result<(), BridgeRpcError> {
    bounded_actor_ask(
        wire_state,
        "drain_observer_updates",
        "death_respawn_docking",
        ask_timeout,
        old_sys_handle.drain_observer_updates(old_obs_id),
    )
    .await
    .map(|_| ())
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run_pre_dock_observer_ask_bounded(
    ctx: &ServerContext,
    sys_handle: &crate::system_actor::SystemHandle,
    wire_state: &mut crate::outbound_sequencer::SessionWireState,
    sys_rx: Option<&mut tokio::sync::broadcast::Receiver<crate::system_actor::TickBroadcast>>,
    sys_id: u32,
    obs_id: i64,
    session_id: u64,
    space_scope: Option<crate::outbound_scene_scope::OutboundSceneScope>,
    ask_timeout: std::time::Duration,
) -> std::result::Result<
    (
        Option<crate::outbound_sequencer::DestinyBatch>,
        Vec<crate::outbound_sequencer::DestinyBatch>,
    ),
    BridgeRpcError,
> {
    let (tick, obs_evs, updates_pyrep, watermark_seq) = bounded_actor_ask(
        wire_state,
        "drain_observer_updates_with_seq",
        "pre_dock",
        ask_timeout,
        sys_handle.drain_observer_updates_with_seq(obs_id),
    )
    .await?;
    let mut pre_dock_ticks = Vec::new();
    let mut ignored_post = Vec::new();
    let (ep, live_ok) = if matches!(wire_state.destiny, DestinyStream::Live { .. }) {
        if let Some(rx) = sys_rx {
            drain_pending_tick_batches_for_sys_with_watermark(
                ctx,
                rx,
                sys_id,
                &[obs_id],
                wire_state,
                Some(watermark_seq),
                &mut pre_dock_ticks,
                &mut ignored_post,
                session_id,
            )
        } else {
            (wire_state.current_epoch(), true)
        }
    } else {
        if let Some(rx) = sys_rx {
            let _ = drain_sys_rx_discarding_lag(rx);
        }
        (wire_state.current_epoch(), true)
    };
    let pre_dock_destiny = if live_ok {
        build_destiny_batch(
            ctx,
            ep,
            tick,
            &obs_evs,
            updates_pyrep,
            &[obs_id],
            SelfDestinyWireContext::DockSettle,
            space_scope,
        )
    } else {
        pre_dock_ticks.clear();
        None
    };
    Ok((pre_dock_destiny, pre_dock_ticks))
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run_pending_dock_settle_destiny_sync<W: AsyncWriteExt + Unpin>(
    ctx: &ServerContext,
    handle: &crate::system_actor::SystemHandle,
    writer: &mut W,
    wire_state: &mut crate::outbound_sequencer::SessionWireState,
    sys_rx: Option<&mut tokio::sync::broadcast::Receiver<crate::system_actor::TickBroadcast>>,
    client_id: u64,
    session_id: u64,
    session: &SessionState,
    plan: &crate::services::space_warp_jump::DockWarpSettlePlan,
    ask_timeout: std::time::Duration,
) -> std::result::Result<(), ()> {
    if let Ok((tick, events, updates_pyrep, watermark_seq)) = bounded_actor_ask(
        wire_state,
        "drain_observer_updates_with_seq",
        "pending_dock_settle",
        ask_timeout,
        handle.drain_observer_updates_with_seq(plan.ship_id),
    )
    .await
    {
        if session_owns_char_notifications(ctx, session) {
            let mut pre_batches = Vec::new();
            let mut post_batches = Vec::new();
            let ep = if matches!(
                wire_state.destiny,
                crate::outbound_sequencer::DestinyStream::Live { .. }
            ) {
                if let Some(rx) = sys_rx {
                    drain_pending_tick_batches_for_sys_with_watermark(
                        ctx,
                        rx,
                        plan.sys_id,
                        &[plan.ship_id],
                        wire_state,
                        Some(watermark_seq),
                        &mut pre_batches,
                        &mut post_batches,
                        session_id,
                    )
                    .0
                } else {
                    wire_state.current_epoch()
                }
            } else {
                wire_state.current_epoch()
            };
            let settle_write_failed = emit_tick_batches_with_observability(
                ctx,
                wire_state,
                writer,
                client_id,
                session,
                pre_batches,
                "pending_dock_settle_pre_tick",
            )
            .await
            .is_err()
                || if !events.is_empty() || plan.stopped_now {
                    if let Some(batch) = build_destiny_batch(
                        ctx,
                        ep,
                        tick,
                        &events,
                        updates_pyrep,
                        &[plan.ship_id],
                        SelfDestinyWireContext::DockSettle,
                        crate::outbound_scene_scope::space_scope(
                            session.solar_system_id,
                            Some(wire_state.scene_epoch),
                        ),
                    ) {
                        emit_tick_batch_with_observability(
                            ctx,
                            wire_state,
                            writer,
                            client_id,
                            session,
                            batch,
                            "pending_dock_settle_tick",
                        )
                        .await
                        .is_err()
                    } else {
                        false
                    }
                } else {
                    false
                }
                || emit_tick_batches_with_observability(
                    ctx,
                    wire_state,
                    writer,
                    client_id,
                    session,
                    post_batches,
                    "system_tick_broadcast",
                )
                .await
                .is_err();
            if settle_write_failed {
                return Err(());
            }
        }
    }
    if plan.stopped_now && session_owns_char_notifications(ctx, session) {
        best_effort_flush_peer_observer_updates(
            wire_state,
            handle,
            Some(plan.ship_id),
            ask_timeout,
            "pending_dock_settle",
        )
        .await;
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(super) async fn execute_call_req_wire_with_ask_timeout(
    ctx: &ServerContext,
    session: &mut SessionState,
    wire: &mut CallReqWireRefs<'_>,
    req_source: &MachoAddress,
    req_dest: &MachoAddress,
    service: &str,
    bound_id: Option<u64>,
    method: &str,
    call_args: &[PyRep],
    call_kwargs: &HashMap<String, PyRep>,
    ask_timeout: std::time::Duration,
) -> CallReqWireOutcome {
    let mut inner = InternalCallReqWireRefs {
        client_id: wire.client_id,
        sys_rx: TickRxSlot::Borrowed(wire.sys_rx),
        subscribed_sys_id: wire.subscribed_sys_id,
        wire_state: wire.wire_state,
        deferred_ship_id: wire.deferred_ship_id,
    };
    execute_call_req_wire_inner(
        ctx,
        session,
        &mut inner,
        req_source,
        req_dest,
        service,
        bound_id,
        method,
        call_args,
        call_kwargs,
        ask_timeout,
    )
    .await
}

#[cfg(test)]
#[path = "macho_listener_actor_asks_tests.rs"]
mod macho_listener_actor_asks_tests;
