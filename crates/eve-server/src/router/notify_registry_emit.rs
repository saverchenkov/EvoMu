//! Standalone typed notification builders, target emitters, and session push helpers (`#1196`).

#![forbid(unsafe_code)]

use crate::router::cache_notifications::{
    next_notification_seq_id, push_or_stage_pending_notification, PendingMachoNotification,
};
#[cfg(any(test, feature = "testkit"))]
use crate::router::notify_registry::assert_registered_entry_shape;
use crate::router::notify_registry::{
    encode_macho_packet_for_emit, find_registered_notification_checked,
    push_session_notification_for_emit, typed_args_checked, TypedNotification,
};
use eve_db::lock_rank::AnyRwLock;
use eve_proto::PyRep;
use std::collections::HashMap;
use tokio::sync::broadcast;

/// Emits a typed character notification directly to `pending_notifications` and `notify_tx`
/// without holding a full [`ServerContext`] (used by `SystemActor` / dungeon hooks, retaining
/// their 256-entry FIFO queue bound).
pub fn emit_typed_char_core<N: TypedNotification>(
    db: &eve_db::WorldRepository,
    pending_notifications: Option<&dyn AnyRwLock<HashMap<i64, Vec<PendingMachoNotification>>>>,
    notify_tx: Option<&broadcast::Sender<(i64, PendingMachoNotification)>>,
    char_id: i64,
    user_id_opt: Option<u64>,
    notif: N,
) {
    if char_id <= 0 {
        return;
    }
    let now_ft = db.clock().now_blueos_filetime_ticks();
    build_and_push_typed_notification(
        pending_notifications,
        notify_tx,
        char_id,
        user_id_opt,
        N::DEFAULT_ID_TYPE,
        now_ft,
        notif,
        Some(256),
    );
}

/// Looks up `char_id`'s `user_id` in `db` and emits a typed character notification when
/// `pending_notifications` is present.
pub fn emit_typed_char_with_db<N: TypedNotification>(
    db: &eve_db::WorldRepository,
    pending_notifications: Option<&dyn AnyRwLock<HashMap<i64, Vec<PendingMachoNotification>>>>,
    notify_tx: Option<&broadcast::Sender<(i64, PendingMachoNotification)>>,
    char_id: i64,
    notif: N,
) {
    if char_id <= 0 || pending_notifications.is_none() {
        return;
    }
    let uid_opt = db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.user_id));
    emit_typed_char_core(db, pending_notifications, notify_tx, char_id, uid_opt, notif);
}

/// Builds and pushes a typed notification to a single recipient's pending queue + live broadcast
/// using the standard router [`push_pending_notification_capped`] queue bound.
pub fn emit_typed_target_to_char_with_id_type<N: TypedNotification>(
    pending_notifications: Option<&dyn AnyRwLock<HashMap<i64, Vec<PendingMachoNotification>>>>,
    notify_tx: Option<&broadcast::Sender<(i64, PendingMachoNotification)>>,
    char_id: i64,
    user_id_opt: Option<u64>,
    id_type: &str,
    created_filetime: i64,
    notif: N,
) {
    build_and_push_typed_notification(
        pending_notifications,
        notify_tx,
        char_id,
        user_id_opt,
        id_type,
        created_filetime,
        notif,
        None,
    );
}

/// Builds and pushes a typed notification to a single recipient's pending queue + live broadcast
/// with an explicit FIFO queue bound (used by lifecycle hooks retaining a 512-entry bound).
#[allow(clippy::too_many_arguments)]
pub fn emit_typed_target_to_char_capped<N: TypedNotification>(
    pending_notifications: Option<&dyn AnyRwLock<HashMap<i64, Vec<PendingMachoNotification>>>>,
    notify_tx: Option<&broadcast::Sender<(i64, PendingMachoNotification)>>,
    char_id: i64,
    user_id_opt: Option<u64>,
    id_type: &str,
    created_filetime: i64,
    queue_cap: usize,
    notif: N,
) {
    build_and_push_typed_notification(
        pending_notifications,
        notify_tx,
        char_id,
        user_id_opt,
        id_type,
        created_filetime,
        notif,
        Some(queue_cap),
    );
}

/// Builds one fleet-scoped typed notification and enqueues/broadcasts it to each fleet member
/// (used by dungeon/escalation hooks outside `ServerContext`, retaining the 256-entry FIFO bound).
#[track_caller]
pub fn emit_typed_to_fleet_members_core<N: TypedNotification>(
    db: &eve_db::WorldRepository,
    pending_notifications: Option<&dyn AnyRwLock<HashMap<i64, Vec<PendingMachoNotification>>>>,
    notify_tx: Option<&broadcast::Sender<(i64, PendingMachoNotification)>>,
    fleet_id: i64,
    members: &[i64],
    notif: N,
) {
    if fleet_id <= 0 || members.is_empty() {
        return;
    }
    let now_ft = db.clock().now_blueos_filetime_ticks();
    let scene_scope = notif.scene_scope();
    let args = typed_args_checked(notif);
    let (packet, marshaled_packet) = encode_macho_packet_for_emit(
        fleet_id,
        None,
        N::EVENT,
        N::DEFAULT_ID_TYPE,
        args.clone(),
    );
    for &member_id in members {
        if member_id <= 0 {
            continue;
        }
        let pending = PendingMachoNotification {
            seq_id: next_notification_seq_id(),
            recipient_char_id: member_id,
            event: N::EVENT.to_string(),
            id_type: N::DEFAULT_ID_TYPE.to_string(),
            args: args.clone(),
            packet: packet.clone(),
            marshaled_packet: marshaled_packet.clone(),
            created_filetime: now_ft,
            scene_scope,
        };
        push_or_stage_pending_notification(
            pending_notifications,
            notify_tx,
            pending,
            Some(256),
        );
    }
}

/// Broadcasts one typed notification to all in-space characters currently in `solar_system_id`
/// outside [`ServerContext`] (used by expedition destruction hooks, retaining the 512-entry FIFO bound).
#[track_caller]
pub fn emit_typed_to_solar_system_in_space_core<N: TypedNotification>(
    db: &eve_db::WorldRepository,
    pending_notifications: &dyn AnyRwLock<HashMap<i64, Vec<PendingMachoNotification>>>,
    notify_tx: &broadcast::Sender<(i64, PendingMachoNotification)>,
    solar_system_id: u32,
    notif: N,
) {
    let now_ft = db.clock().now_blueos_filetime_ticks();
    let recips: Vec<(i64, Option<u64>)> = db.with_snapshot(|snap| {
        snap.characters
            .values()
            .filter(|c| {
                c.station_id.is_none()
                    && c.structure_id.is_none()
                    && c.solar_system_id == solar_system_id
            })
            .map(|c| (c.character_id, Some(c.user_id)))
            .collect()
    });
    let base_scope = notif
        .scene_scope()
        .or_else(|| crate::outbound_scene_scope::space_scope(Some(solar_system_id), None));
    let args = typed_args_checked(notif);
    for (cid, uid) in recips {
        if cid <= 0 {
            continue;
        }
        let (packet, marshaled_packet) = encode_macho_packet_for_emit(
            cid,
            uid,
            N::EVENT,
            N::DEFAULT_ID_TYPE,
            args.clone(),
        );
        let pending = PendingMachoNotification {
            seq_id: next_notification_seq_id(),
            recipient_char_id: cid,
            event: N::EVENT.to_string(),
            id_type: N::DEFAULT_ID_TYPE.to_string(),
            args: args.clone(),
            packet,
            marshaled_packet,
            created_filetime: now_ft,
            scene_scope: crate::outbound_scene_scope::bind_db_recipient_scene_scope(
                db,
                base_scope,
                cid,
                uid,
            ),
        };
        push_or_stage_pending_notification(
            Some(pending_notifications),
            Some(notify_tx),
            pending,
            Some(512),
        );
    }
}

/// Returns whether an active [`eve_machonet::SessionState`] satisfies an optional [`OutboundSceneScope`]
/// for direct session notification push (`in_space` + matching `solar_system_id`).
pub fn session_accepts_scene_scope(
    session: &eve_machonet::SessionState,
    scope: Option<crate::outbound_sequencer::OutboundSceneScope>,
) -> bool {
    let Some(scope) = scope else {
        return true;
    };
    if let Some(target_sys) = scope.scene_sys_id {
        if session.solar_system_id != Some(target_sys) {
            return false;
        }
    } else if scope.requires_in_space {
        return false;
    }
    if !scope.requires_in_space {
        return true;
    }
    session.station_id.is_none()
        && (session.structure_id.is_none() || session.structure_id == session.ship_id)
}

/// Pushes a typed notification onto an active [`eve_machonet::SessionState`].
#[track_caller]
pub fn push_typed_session_notification<N: TypedNotification>(
    session: &mut eve_machonet::SessionState,
    notif: N,
) {
    let scene_scope = notif.scene_scope();
    let args = typed_args_checked(notif);
    if !session_accepts_scene_scope(session, scene_scope) {
        return;
    }
    push_session_notification_for_emit(session, N::EVENT, args);
}

/// Flushes an already-constructed [`PendingMachoNotification`] onto an active [`eve_machonet::SessionState`].
pub fn push_pending_session_notification(
    session: &mut eve_machonet::SessionState,
    notif: PendingMachoNotification,
) {
    if !session_accepts_scene_scope(session, notif.scene_scope) {
        return;
    }
    push_session_notification_for_emit(session, notif.event, notif.args);
}

fn build_pending_notification_for_event(
    recipient_char_id: i64,
    user_id_opt: Option<u64>,
    event: &str,
    id_type: &str,
    args: Vec<PyRep>,
    created_filetime: i64,
) -> PendingMachoNotification {
    let (packet, marshaled_packet) = encode_macho_packet_for_emit(
        recipient_char_id,
        user_id_opt,
        event,
        id_type,
        args.clone(),
    );
    PendingMachoNotification {
        seq_id: next_notification_seq_id(),
        recipient_char_id,
        event: event.to_string(),
        id_type: id_type.to_string(),
        args,
        packet,
        marshaled_packet,
        created_filetime,
        scene_scope: None,
    }
}

/// Encodes a strongly typed notification into a wire-ready [`PendingMachoNotification`]
/// without pushing it to any queue (used by callers applying custom queue coalescing).
#[track_caller]
pub fn build_pending_typed_notification<N: TypedNotification>(
    recipient_char_id: i64,
    user_id_opt: Option<u64>,
    id_type: &str,
    created_filetime: i64,
    notif: N,
) -> PendingMachoNotification {
    let scene_scope = notif.scene_scope();
    let args = typed_args_checked(notif);
    build_pending_notification_for_event(
        recipient_char_id,
        user_id_opt,
        N::EVENT,
        id_type,
        args,
        created_filetime,
    )
    .with_scene_scope(scene_scope)
}

/// Pushes a prebuilt notification into `pend` with a 256-entry FIFO bound and broadcasts on `notify_tx`.
pub fn push_pending_char_notification_256(
    pend: &dyn AnyRwLock<HashMap<i64, Vec<PendingMachoNotification>>>,
    notify_tx: Option<&broadcast::Sender<(i64, PendingMachoNotification)>>,
    char_id: i64,
    notif: PendingMachoNotification,
) {
    let mut queues = pend.write().unwrap_or_else(|e| e.into_inner());
    let q = queues.entry(char_id).or_default();
    if q.len() >= 256 {
        q.remove(0);
    }
    q.push(notif.clone());
    if let Some(tx) = notify_tx {
        let _ = tx.send((char_id, notif));
    }
}

/// Encodes a registered notification by event name into a wire-ready [`PendingMachoNotification`]
/// after verifying that `event` is registered in [`super::notify_registry::all_registered_notifications`]
/// and `args.len()` satisfies its registered arity bounds.
#[track_caller]
pub fn build_pending_registered_notification(
    recipient_char_id: i64,
    user_id_opt: Option<u64>,
    event: &str,
    id_type: &str,
    args: Vec<PyRep>,
    created_filetime: i64,
) -> Option<PendingMachoNotification> {
    let entry = find_registered_notification_checked(event, args.len())?;
    if entry.is_ballpark_scoped {
        return None;
    }
    #[cfg(any(test, feature = "testkit"))]
    assert_registered_entry_shape(entry, &args);
    Some(build_pending_notification_for_event(
        recipient_char_id,
        user_id_opt,
        entry.event,
        id_type,
        args,
        created_filetime,
    ))
}

#[track_caller]
#[allow(clippy::too_many_arguments)]
fn build_and_push_typed_notification<N: TypedNotification>(
    pending_notifications: Option<&dyn AnyRwLock<HashMap<i64, Vec<PendingMachoNotification>>>>,
    notify_tx: Option<&broadcast::Sender<(i64, PendingMachoNotification)>>,
    char_id: i64,
    user_id_opt: Option<u64>,
    id_type: &str,
    created_filetime: i64,
    notif: N,
    explicit_queue_cap: Option<usize>,
) {
    if char_id <= 0 {
        return;
    }
    let pending = build_pending_typed_notification(
        char_id,
        user_id_opt,
        id_type,
        created_filetime,
        notif,
    );
    push_or_stage_pending_notification(
        pending_notifications,
        notify_tx,
        pending,
        explicit_queue_cap,
    );
}
