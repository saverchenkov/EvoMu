//! Object caching (`carbon.common.script.net.objectCaching.CachedObject`,
//! `carbon.common.script.net.cachedObject.CachedObject`,
//! `carbon.common.script.net.objectCaching.CachedMethodCallResult`)
//! and binary-marshaled `MachoPacket` client notification routing (`P-3`, `P-5`).

#![forbid(unsafe_code)]

use crate::router::context::ServerContext;
use eve_machonet::{MachoAddress, MachoPacket, MachoPacketType};
use eve_proto::marshal::{
    self, V24_CACHED_METHOD_CALL_RESULT_CLASS, V24_CACHED_OBJECT_CLASS, V24_CACHE_HINT_CLASS,
};
use eve_proto::PyRep;
use std::collections::HashMap;

/// Standard IEEE 802.3 CRC-32 (`0xEDB8_8320`) matching `CRC32::Generate` in `CachedObjectMgr.cpp:238`.
pub fn crc32_ieee(bytes: &[u8]) -> i32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in bytes {
        crc ^= b as u32;
        for _ in 0..8 {
            if (crc & 1) != 0 {
                crc = (crc >> 1) ^ 0xEDB8_8320;
            } else {
                crc >>= 1;
            }
        }
    }
    (!crc) as i32
}

/// Produces a deterministic lookup key for any `objectID` `PyRep` (`CachedObjectMgr::OIDToString`).
pub fn cache_object_key(object_id: &PyRep) -> String {
    match object_id {
        PyRep::String(s) | PyRep::Token(s) => s.clone(),
        PyRep::Tuple(items) => {
            let parts: Vec<String> = items.iter().map(cache_object_key).collect();
            format!("({})", parts.join(","))
        }
        PyRep::Integer(i) => i.to_string(),
        other => serde_json::to_string(other).unwrap_or_else(|_| format!("{other:?}")),
    }
}

/// Stored server-side cache entry for `objectCaching` (`P-3`).
#[derive(Debug, Clone, PartialEq)]
pub struct CachedObjectRecord {
    pub object_id: PyRep,
    pub timestamp: i64,
    pub version: i32,
    pub node_id: i64,
    pub shared: bool,
    pub compressed: bool,
    pub marshaled_bytes: Vec<u8>,
    pub payload: PyRep,
}

impl CachedObjectRecord {
    /// Encodes as V24 `carbon.common.script.net.objectCaching.CachedObject` (`objectCaching.pyj:772`).
    pub fn to_cached_object_pyrep(&self) -> PyRep {
        PyRep::Object {
            type_name: V24_CACHED_OBJECT_CLASS.into(),
            args: Box::new(PyRep::Tuple(vec![
                PyRep::Tuple(vec![
                    PyRep::Integer(self.timestamp),
                    PyRep::Integer(self.version as i64),
                ]),
                PyRep::None,
                PyRep::Integer(self.node_id),
                PyRep::Integer(if self.shared { 1 } else { 0 }),
                PyRep::Buffer(self.marshaled_bytes.clone()),
                PyRep::Integer(if self.compressed { 1 } else { 0 }),
                self.object_id.clone(),
            ])),
        }
    }

    /// Encodes as V24 `carbon.common.script.net.cachedObject.CachedObject` (`cachedObject.pyj:12`).
    pub fn to_cache_hint_pyrep(&self) -> PyRep {
        let mut fields = vec![
            self.object_id.clone(),
            PyRep::Integer(self.node_id),
            PyRep::Tuple(vec![
                PyRep::Integer(self.timestamp),
                PyRep::Integer(self.version as i64),
            ]),
        ];
        if !self.shared {
            fields.push(PyRep::Integer(0));
        }
        PyRep::Object {
            type_name: V24_CACHE_HINT_CLASS.into(),
            args: Box::new(PyRep::Tuple(fields)),
        }
    }

    /// Encodes as an inline V24 `carbon.common.script.net.objectCaching.CachedMethodCallResult` (`objectCaching.pyj:837`).
    pub fn to_cached_method_call_result_pyrep(
        &self,
        client_when: &str,
        session_info: Option<&str>,
    ) -> PyRep {
        let mut details = vec![(
            PyRep::String("versionCheck".into()),
            PyRep::Tuple(vec![
                PyRep::String(client_when.into()),
                PyRep::None,
                PyRep::None,
            ]),
        )];
        if let Some(si) = session_info {
            details.push((PyRep::String("sessionInfo".into()), PyRep::String(si.into())));
        }
        PyRep::Object {
            type_name: V24_CACHED_METHOD_CALL_RESULT_CLASS.into(),
            args: Box::new(PyRep::Tuple(vec![
                PyRep::Dict(details),
                PyRep::SubStream(Box::new(self.payload.clone())),
                PyRep::List(vec![
                    PyRep::Integer(self.timestamp),
                    PyRep::Integer(self.version as i64),
                ]),
            ])),
        }
    }

    /// Encodes as a hint-backed V24 `carbon.common.script.net.objectCaching.CachedMethodCallResult` (`objectCaching.pyj:837`).
    pub fn to_cached_method_call_hint_pyrep(&self, version_check: &str) -> PyRep {
        PyRep::Object {
            type_name: V24_CACHED_METHOD_CALL_RESULT_CLASS.into(),
            args: Box::new(PyRep::Tuple(vec![
                PyRep::Dict(vec![(
                    PyRep::String("versionCheck".into()),
                    PyRep::String(version_check.into()),
                )]),
                self.to_cache_hint_pyrep(),
                PyRep::None,
            ])),
        }
    }
}

/// Target scope for server-initiated client notifications (`P-5`, `EntityList::Multicast`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotificationTarget {
    Character(i64),
    Corporation(i64),
    Alliance(i64),
    Fleet(i64),
    SolarSystem(u32),
    Station(i64),
    All,
}

static NEXT_NOTIFICATION_SEQ_ID: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(1);

/// Returns a monotonically increasing sequence ID for [`PendingMachoNotification`].
pub fn next_notification_seq_id() -> u64 {
    NEXT_NOTIFICATION_SEQ_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Queued binary-marshaled `MachoPacket` (`Notification` = `12`) ready for delivery (`P-5`).
#[derive(Debug, Clone)]
pub struct PendingMachoNotification {
    pub seq_id: u64,
    pub recipient_char_id: i64,
    pub event: String,
    pub id_type: String,
    pub args: Vec<PyRep>,
    pub packet: MachoPacket,
    pub marshaled_packet: Vec<u8>,
    pub created_filetime: i64,
    pub scene_scope: Option<crate::outbound_sequencer::OutboundSceneScope>,
}

impl PendingMachoNotification {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        recipient_char_id: i64,
        event: impl Into<String>,
        id_type: impl Into<String>,
        args: Vec<PyRep>,
        packet: MachoPacket,
        marshaled_packet: Vec<u8>,
        created_filetime: i64,
    ) -> Self {
        Self {
            seq_id: next_notification_seq_id(),
            recipient_char_id,
            event: event.into(),
            id_type: id_type.into(),
            args,
            packet,
            marshaled_packet,
            created_filetime,
            scene_scope: None,
        }
    }

    pub fn with_scene_scope(
        mut self,
        scene_scope: Option<crate::outbound_sequencer::OutboundSceneScope>,
    ) -> Self {
        self.scene_scope = scene_scope;
        self
    }
}

/// Maximum queued real-time notifications per online character (`R51-P1-3`).
pub const MAX_PENDING_NOTIFICATIONS_PER_CHAR: usize = 128;

/// Hard ceiling for notifications emitted to the active RPC caller during a single in-flight RPC (`#850` / `B12`).
pub const MAX_ACTIVE_RPC_NOTIFICATIONS_PER_CHAR: usize = 4_096;

tokio::task_local! {
    static DISPATCH_ACTIVE_CHAR_ID: std::cell::Cell<Option<i64>>;
}

/// Returns `true` if `char_id` is the character executing the current in-flight `dispatch_rpc` call (`#850`).
pub fn is_active_dispatch_char(char_id: i64) -> bool {
    DISPATCH_ACTIVE_CHAR_ID
        .try_with(|c| c.get() == Some(char_id))
        .unwrap_or(false)
}

/// Updates the active character ID inside an in-flight `dispatch_rpc` scope (e.g. during `SelectCharacterID`).
pub fn set_active_dispatch_char(char_id: Option<i64>) {
    let _ = DISPATCH_ACTIVE_CHAR_ID.try_with(|c| c.set(char_id));
}

/// Runs `fut` with `char_id` registered as the active `dispatch_rpc` caller so bulk RPCs (`MultiAdd`, `FitMany`)
/// do not evict their own in-flight notifications at `MAX_PENDING_NOTIFICATIONS_PER_CHAR` (`#850` / `B12`).
pub async fn with_active_dispatch_char<F, R>(char_id: Option<i64>, fut: F) -> R
where
    F: std::future::Future<Output = R>,
{
    DISPATCH_ACTIVE_CHAR_ID
        .scope(std::cell::Cell::new(char_id), fut)
        .await
}

#[derive(Debug, Clone)]
struct StagedDispatchNotification {
    write_queue: bool,
    send_tx: bool,
    notif: PendingMachoNotification,
    explicit_queue_cap: Option<usize>,
}

struct DispatchNotificationStage {
    scope_id: u64,
    target_pend_ptr: usize,
    target_notify_tx: tokio::sync::broadcast::Sender<(i64, PendingMachoNotification)>,
    staged: Vec<StagedDispatchNotification>,
    rpc_method_stack: Vec<String>,
    switch_handles: Vec<std::sync::Weak<super::delta_item_switch::ProjectedItemSwitchHandle>>,
}

impl Drop for DispatchNotificationStage {
    fn drop(&mut self) {
        for weak in self.switch_handles.drain(..) {
            if let Some(handle) = weak.upgrade() {
                handle.expire_dispatch_scope(self.scope_id);
            }
        }
    }
}

pub(crate) fn register_dispatch_switch_handle(
    pend: &dyn eve_db::lock_rank::AnyRwLock<HashMap<i64, Vec<PendingMachoNotification>>>,
    notify_tx: &tokio::sync::broadcast::Sender<(i64, PendingMachoNotification)>,
    handle: &std::sync::Arc<super::delta_item_switch::ProjectedItemSwitchHandle>,
) -> Option<u64> {
    let pend_ptr = pending_notifications_ptr(pend);
    DISPATCH_STAGED_NOTIFICATIONS
        .try_with(|cell| {
            let mut stage = cell.borrow_mut();
            stage_matches_target(&stage, Some(pend_ptr), Some(notify_tx)).then(|| {
                let weak = std::sync::Arc::downgrade(handle);
                if !stage.switch_handles.iter().any(|existing| existing.ptr_eq(&weak)) {
                    stage.switch_handles.push(weak);
                }
                stage.scope_id
            })
        })
        .ok()
        .flatten()
}

static NEXT_STAGING_SCOPE_ID: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(1);

tokio::task_local! {
    static DISPATCH_STAGED_NOTIFICATIONS: std::cell::RefCell<DispatchNotificationStage>;
}

#[inline]
pub(crate) fn pending_notifications_ptr(
    pend: &dyn eve_db::lock_rank::AnyRwLock<HashMap<i64, Vec<PendingMachoNotification>>>,
) -> usize {
    pend as *const dyn eve_db::lock_rank::AnyRwLock<HashMap<i64, Vec<PendingMachoNotification>>>
        as *const () as usize
}

#[inline]
fn stage_matches_target(
    stage: &DispatchNotificationStage,
    pend_ptr: Option<usize>,
    notify_tx: Option<&tokio::sync::broadcast::Sender<(i64, PendingMachoNotification)>>,
) -> bool {
    match (pend_ptr, notify_tx) {
        (Some(p), _) => p == stage.target_pend_ptr,
        (None, Some(tx)) => tx.same_channel(&stage.target_notify_tx),
        (None, None) => false,
    }
}

pub(crate) fn has_staged_dispatch_notifications_scope() -> bool {
    DISPATCH_STAGED_NOTIFICATIONS.try_with(|_| ()).is_ok()
}

pub(crate) async fn with_staged_dispatch_notifications<F, R>(
    pend: &dyn eve_db::lock_rank::AnyRwLock<HashMap<i64, Vec<PendingMachoNotification>>>,
    notify_tx: &tokio::sync::broadcast::Sender<(i64, PendingMachoNotification)>,
    fut: F,
) -> R
where
    F: std::future::Future<Output = R>,
{
    let stage = DispatchNotificationStage {
        scope_id: NEXT_STAGING_SCOPE_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        target_pend_ptr: pending_notifications_ptr(pend),
        target_notify_tx: notify_tx.clone(),
        staged: Vec::new(),
        rpc_method_stack: Vec::new(),
        switch_handles: Vec::new(),
    };
    DISPATCH_STAGED_NOTIFICATIONS
        .scope(std::cell::RefCell::new(stage), fut)
        .await
}

pub(crate) fn push_dispatch_rpc_method(
    pend: &dyn eve_db::lock_rank::AnyRwLock<HashMap<i64, Vec<PendingMachoNotification>>>,
    notify_tx: &tokio::sync::broadcast::Sender<(i64, PendingMachoNotification)>,
    rpc_method: String,
) -> Option<usize> {
    let pend_ptr = pending_notifications_ptr(pend);
    let pushed = DISPATCH_STAGED_NOTIFICATIONS
        .try_with(|cell| {
            let mut stage = cell.borrow_mut();
            stage_matches_target(&stage, Some(pend_ptr), Some(notify_tx)).then(|| {
                stage.rpc_method_stack.push(rpc_method);
            })
        })
        .ok()
        .flatten()
        .is_some();
    pushed.then_some(pend_ptr)
}

pub(crate) fn pop_dispatch_rpc_method(pend_ptr: usize) {
    let _ = DISPATCH_STAGED_NOTIFICATIONS.try_with(|cell| {
        let mut stage = cell.borrow_mut();
        if stage.target_pend_ptr == pend_ptr {
            stage.rpc_method_stack.pop();
        }
    });
}

pub(crate) fn current_dispatch_staging_scope_info(
    pend: Option<&dyn eve_db::lock_rank::AnyRwLock<HashMap<i64, Vec<PendingMachoNotification>>>>,
    notify_tx: Option<&tokio::sync::broadcast::Sender<(i64, PendingMachoNotification)>>,
) -> Option<(u64, Option<String>)> {
    let pend_ptr = pend.map(pending_notifications_ptr);
    DISPATCH_STAGED_NOTIFICATIONS
        .try_with(|cell| {
            let stage = cell.borrow();
            stage_matches_target(&stage, pend_ptr, notify_tx)
                .then(|| (stage.scope_id, stage.rpc_method_stack.last().cloned()))
        })
        .ok()
        .flatten()
}

pub(crate) fn is_staging_dispatch_notifications(
    pend: Option<&dyn eve_db::lock_rank::AnyRwLock<HashMap<i64, Vec<PendingMachoNotification>>>>,
    notify_tx: Option<&tokio::sync::broadcast::Sender<(i64, PendingMachoNotification)>>,
) -> bool {
    current_dispatch_staging_scope_info(pend, notify_tx).is_some()
}

pub(crate) fn try_stage_dispatch_notification(
    pend: Option<&dyn eve_db::lock_rank::AnyRwLock<HashMap<i64, Vec<PendingMachoNotification>>>>,
    notify_tx: Option<&tokio::sync::broadcast::Sender<(i64, PendingMachoNotification)>>,
    notif: &PendingMachoNotification,
    explicit_queue_cap: Option<usize>,
) -> bool {
    let pend_ptr = pend.map(pending_notifications_ptr);
    DISPATCH_STAGED_NOTIFICATIONS
        .try_with(|cell| {
            let mut stage = cell.borrow_mut();
            if !stage_matches_target(&stage, pend_ptr, notify_tx) {
                return false;
            }
            stage.staged.push(StagedDispatchNotification {
                write_queue: pend.is_some(),
                send_tx: notify_tx.is_some(),
                notif: notif.clone(),
                explicit_queue_cap,
            });
            true
        })
        .unwrap_or(false)
}

pub(crate) fn clear_staged_notifications_for_char(
    pend: &dyn eve_db::lock_rank::AnyRwLock<HashMap<i64, Vec<PendingMachoNotification>>>,
    char_id: i64,
) {
    let pend_ptr = pending_notifications_ptr(pend);
    let _ = DISPATCH_STAGED_NOTIFICATIONS.try_with(|cell| {
        let mut stage = cell.borrow_mut();
        if stage.target_pend_ptr == pend_ptr {
            stage.staged.retain(|item| item.notif.recipient_char_id != char_id);
        }
    });
}

pub(crate) fn find_staged_notification_by_seq(
    pend: &dyn eve_db::lock_rank::AnyRwLock<HashMap<i64, Vec<PendingMachoNotification>>>,
    seq_id: u64,
) -> Option<PendingMachoNotification> {
    let pend_ptr = pending_notifications_ptr(pend);
    DISPATCH_STAGED_NOTIFICATIONS
        .try_with(|cell| {
            let stage = cell.borrow();
            (stage.target_pend_ptr == pend_ptr).then(|| {
                stage.staged.iter().find(|item| item.notif.seq_id == seq_id).map(|i| i.notif.clone())
            }).flatten()
        })
        .ok()
        .flatten()
}

pub(crate) fn insert_before_staged_seq_or_stage(
    pend: &dyn eve_db::lock_rank::AnyRwLock<HashMap<i64, Vec<PendingMachoNotification>>>,
    notify_tx: &tokio::sync::broadcast::Sender<(i64, PendingMachoNotification)>,
    seq_id: u64,
    notif: &mut PendingMachoNotification,
) -> Option<u64> {
    let pend_ptr = pending_notifications_ptr(pend);
    let new_tail_seq = DISPATCH_STAGED_NOTIFICATIONS
        .try_with(|cell| {
            let mut stage = cell.borrow_mut();
            if stage.target_pend_ptr != pend_ptr {
                return None;
            }
            let pos = stage.staged.iter().position(|i| i.notif.seq_id == seq_id)?;
            let tail_seq = next_notification_seq_id();
            notif.seq_id = seq_id;
            stage.staged[pos].notif.seq_id = tail_seq;
            stage.staged.insert(pos, StagedDispatchNotification {
                write_queue: true,
                send_tx: true,
                notif: notif.clone(),
                explicit_queue_cap: None,
            });
            Some(tail_seq)
        })
        .ok()
        .flatten();
    if new_tail_seq.is_none() {
        let _ = try_stage_dispatch_notification(Some(pend), Some(notify_tx), notif, None);
    }
    new_tail_seq
}

pub(crate) fn assemble_pending_notification(
    cid: i64,
    uid: Option<u64>,
    event: &str,
    id_type: &str,
    args: Vec<PyRep>,
    now_ft: i64,
) -> PendingMachoNotification {
    let (pkt, raw) = ServerContext::build_macho_notification_packet(cid, uid, event, id_type, args.clone());
    PendingMachoNotification::new(cid, event, id_type, args, pkt, raw, now_ft)
}

pub(crate) fn enqueue_or_stage_pending_batch(
    pend: &dyn eve_db::lock_rank::AnyRwLock<HashMap<i64, Vec<PendingMachoNotification>>>,
    notify_tx: &tokio::sync::broadcast::Sender<(i64, PendingMachoNotification)>,
    notifs: &[PendingMachoNotification],
) {
    if notifs.is_empty() { return; }
    if is_staging_dispatch_notifications(Some(pend), Some(notify_tx)) {
        for n in notifs { let _ = try_stage_dispatch_notification(Some(pend), Some(notify_tx), n, None); }
        return;
    }
    let mut queues = pend.write().unwrap_or_else(|e| e.into_inner());
    for notif in notifs {
        push_pending_notification_capped(queues.entry(notif.recipient_char_id).or_default(), notif.clone());
        let _ = notify_tx.send((notif.recipient_char_id, notif.clone()));
    }
}

fn has_staged_notification_for_char_and_id(
    pend: &dyn eve_db::lock_rank::AnyRwLock<HashMap<i64, Vec<PendingMachoNotification>>>,
    char_id: i64,
    event: &str,
    notification_id: i64,
) -> bool {
    let pend_ptr = pending_notifications_ptr(pend);
    DISPATCH_STAGED_NOTIFICATIONS
        .try_with(|cell| {
            let stage = cell.borrow();
            stage.target_pend_ptr == pend_ptr
                && stage.staged.iter().any(|item| {
                    item.notif.recipient_char_id == char_id
                        && item.notif.event == event
                        && item.notif.args.first() == Some(&PyRep::Integer(notification_id))
                })
        })
        .unwrap_or(false)
}

pub(crate) fn flush_staged_dispatch_notifications(
    pend: &dyn eve_db::lock_rank::AnyRwLock<HashMap<i64, Vec<PendingMachoNotification>>>,
    notify_tx: &tokio::sync::broadcast::Sender<(i64, PendingMachoNotification)>,
) {
    let pend_ptr = pending_notifications_ptr(pend);
    let items = DISPATCH_STAGED_NOTIFICATIONS
        .try_with(|cell| {
            let mut stage = cell.borrow_mut();
            if stage.target_pend_ptr == pend_ptr {
                std::mem::take(&mut stage.staged)
            } else {
                Vec::new()
            }
        })
        .unwrap_or_default();
    if items.is_empty() {
        return;
    }
    let has_queue_writes = items.iter().any(|item| item.write_queue);
    let mut queues = has_queue_writes.then(|| pend.write().unwrap_or_else(|e| e.into_inner()));
    for item in items {
        if item.write_queue {
            if let Some(queues) = queues.as_mut() {
                let queue = queues.entry(item.notif.recipient_char_id).or_default();
                push_pending_notification_with_cap(
                    queue,
                    item.notif.clone(),
                    item.explicit_queue_cap,
                );
            }
        }
        if item.send_tx {
            let _ = notify_tx.send((item.notif.recipient_char_id, item.notif));
        }
    }
}

fn push_pending_notification_with_cap(
    queue: &mut Vec<PendingMachoNotification>,
    notif: PendingMachoNotification,
    explicit_queue_cap: Option<usize>,
) {
    if let Some(cap) = explicit_queue_cap {
        while queue.len() >= cap {
            queue.remove(0);
        }
        queue.push(notif);
    } else {
        push_pending_notification_capped(queue, notif);
    }
}

pub(crate) fn push_or_stage_pending_notification(
    pend: Option<&dyn eve_db::lock_rank::AnyRwLock<HashMap<i64, Vec<PendingMachoNotification>>>>,
    notify_tx: Option<&tokio::sync::broadcast::Sender<(i64, PendingMachoNotification)>>,
    notif: PendingMachoNotification,
    explicit_queue_cap: Option<usize>,
) {
    if try_stage_dispatch_notification(pend, notify_tx, &notif, explicit_queue_cap) {
        return;
    }
    let cid = notif.recipient_char_id;
    if let Some(pend) = pend {
        let mut queues = pend.write().unwrap_or_else(|e| e.into_inner());
        let queue = queues.entry(cid).or_default();
        push_pending_notification_with_cap(queue, notif.clone(), explicit_queue_cap);
    }
    if let Some(tx) = notify_tx {
        let _ = tx.send((cid, notif));
    }
}

/// Pushes `notif` into `queue`, evicting the oldest entry if the per-character cap is reached.
/// When `notif.recipient_char_id` is the active `dispatch_rpc` caller (`#850`), all notifications emitted
/// by the in-flight RPC are preserved up to [`MAX_ACTIVE_RPC_NOTIFICATIONS_PER_CHAR`] until the post-RPC drain.
pub fn push_pending_notification_capped(
    queue: &mut Vec<PendingMachoNotification>,
    notif: PendingMachoNotification,
) {
    let cap = if is_active_dispatch_char(notif.recipient_char_id) {
        MAX_ACTIVE_RPC_NOTIFICATIONS_PER_CHAR
    } else {
        MAX_PENDING_NOTIFICATIONS_PER_CHAR
    };
    while queue.len() >= cap {
        queue.remove(0);
    }
    queue.push(notif);
}

/// Persistent notification inbox record for `notificationMgr` (`NotificationMgrService.cpp:66-90`).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NotificationRecord {
    pub notification_id: i64,
    pub type_id: i32,
    pub group_id: i32,
    pub sender_id: i64,
    pub receiver_id: i64,
    pub processed: bool,
    pub created: i64,
    pub data: String,
    pub deleted: bool,
    pub interacted: bool,
}

/// Returns true if `flag` represents a corporation hangar (`CorpSAG1..7` = `116..=122`), `CorpDeliveries` (`62`), or `CapsuleerDeliveries` (`187`).
pub fn is_corp_hangar_flag(flag: eve_db::InventoryFlag) -> bool {
    matches!(flag as i64, 62 | 116..=122 | 187)
}

impl ServerContext {
    pub fn is_corp_hangar_flag(flag: eve_db::InventoryFlag) -> bool {
        is_corp_hangar_flag(flag)
    }
}

impl ServerContext {
    /// Builds the `("Method Call", "server", (service, method))` cache `objectID` tuple.
    pub fn method_call_object_id(service: &str, method: &str) -> PyRep {
        PyRep::Tuple(vec![
            PyRep::String("Method Call".into()),
            PyRep::String("server".into()),
            PyRep::Tuple(vec![
                PyRep::String(service.into()),
                PyRep::String(method.into()),
            ]),
        ])
    }

    /// Stores or updates a cached object in `self.object_cache`, computing its IEEE CRC32 version
    /// and advancing the BlueOS filetime timestamp monotonically even across invalidations (`R51-P1-4`).
    pub fn put_cached_object(
        &self,
        object_id: PyRep,
        payload: PyRep,
        shared: bool,
    ) -> CachedObjectRecord {
        let key = cache_object_key(&object_id);
        let marshaled_bytes = marshal::encode(&payload);
        let version = crc32_ieee(&marshaled_bytes);
        let now_ticks = self.now_blueos_filetime_ticks();
        let mut cache = self.object_cache.write().unwrap_or_else(|e| e.into_inner());
        let mut ts_map = self
            .object_cache_timestamps
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let prev_ts = cache
            .get(&key)
            .map(|prev| prev.timestamp)
            .or_else(|| ts_map.get(&key).copied());
        let timestamp = prev_ts
            .map(|pts| now_ticks.max(pts + 1))
            .unwrap_or(now_ticks);
        ts_map.insert(key.clone(), timestamp);
        let compressed = marshaled_bytes.first().copied() == Some(eve_proto::ZLIB_HEADER_BYTE);
        let rec = CachedObjectRecord {
            object_id,
            timestamp,
            version,
            node_id: 888_444,
            shared,
            compressed,
            marshaled_bytes,
            payload,
        };
        cache.insert(key, rec.clone());
        rec
    }

    /// Invalidates a cached object by its `objectID`, returning `true` if an entry was removed.
    pub fn invalidate_cached_object(&self, object_id: &PyRep) -> bool {
        let key = cache_object_key(object_id);
        let mut cache = self.object_cache.write().unwrap_or_else(|e| e.into_inner());
        if let Some(removed) = cache.remove(&key) {
            self.object_cache_timestamps
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .entry(key)
                .and_modify(|ts| *ts = (*ts).max(removed.timestamp))
                .or_insert(removed.timestamp);
            true
        } else {
            false
        }
    }

    /// Invalidates a cached method call `("Method Call", "server", (service, method))`.
    pub fn invalidate_cached_method(&self, service: &str, method: &str) -> bool {
        let oid = Self::method_call_object_id(service, method);
        self.invalidate_cached_object(&oid)
    }

    /// Wraps a service method result in V24 `carbon.common.script.net.objectCaching.CachedMethodCallResult`
    /// and registers it in `self.object_cache`.
    pub fn cache_method_call_result(
        &self,
        service: &str,
        method: &str,
        payload: PyRep,
        client_when: &str,
        session_info: Option<&str>,
    ) -> PyRep {
        let oid = Self::method_call_object_id(service, method);
        let rec = self.put_cached_object(oid, payload, true);
        rec.to_cached_method_call_result_pyrep(client_when, session_info)
    }

    /// Retrieves a cached object or lazily populates backed SDE/DB cache objects (`R51-P1-1`).
    pub fn get_or_populate_cached_object(
        &self,
        object_id: &PyRep,
        shared: bool,
    ) -> Option<CachedObjectRecord> {
        let key = cache_object_key(object_id);
        if let Some(existing) = self
            .object_cache
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
            .cloned()
        {
            return Some(existing);
        }

        let synthesized = match key.as_str() {
            "(Method Call,server,(config,GetUnits))"
            | "config.Units"
            | "config.BulkData.units" => Some(self.build_config_units_payload()),
            "(Method Call,server,(marketProxy,GetMarketGroups))" | "config.MarketGroups" => {
                let mut mgs: Vec<&eve_sde::MarketGroupInfo> =
                    self.sde.market_groups.values().collect();
                mgs.sort_by_key(|mg| mg.market_group_id);
                let rows = mgs
                    .into_iter()
                    .map(|mg| {
                        vec![
                            PyRep::Integer(mg.market_group_id as i64),
                            mg.parent_group_id
                                .map(|pid| PyRep::Integer(pid as i64))
                                .unwrap_or(PyRep::None),
                            PyRep::String(mg.name.clone()),
                            PyRep::String(mg.name.clone()),
                            mg.icon_id
                                .map(|iid| PyRep::Integer(iid as i64))
                                .unwrap_or(PyRep::None),
                            PyRep::Integer(i64::from(mg.has_types)),
                        ]
                    })
                    .collect();
                Some(PyRep::rowset(
                    vec![
                        "marketGroupID",
                        "parentGroupID",
                        "marketGroupName",
                        "description",
                        "iconID",
                        "hasTypes",
                    ],
                    rows,
                ))
            }
            "config.Groups" => {
                let mut grps: Vec<&eve_sde::GroupInfo> = self.sde.groups.values().collect();
                grps.sort_by_key(|g| g.group_id);
                let rows = grps
                    .into_iter()
                    .map(|g| {
                        vec![
                            PyRep::Integer(g.group_id as i64),
                            PyRep::Integer(g.category_id as i64),
                            PyRep::String(g.name.clone()),
                            PyRep::None,
                            PyRep::Integer(i64::from(g.use_base_price)),
                            PyRep::Integer(i64::from(g.anchored)),
                            PyRep::Integer(i64::from(g.anchorable)),
                            PyRep::Integer(i64::from(g.fittable_non_singleton)),
                            PyRep::Integer(i64::from(g.published)),
                        ]
                    })
                    .collect();
                Some(PyRep::rowset(
                    vec![
                        "groupID",
                        "categoryID",
                        "groupName",
                        "iconID",
                        "useBasePrice",
                        "anchored",
                        "anchorable",
                        "fittableNonSingleton",
                        "published",
                    ],
                    rows,
                ))
            }
            "config.Categories" => {
                let mut cats: Vec<&eve_sde::CategoryInfo> = self.sde.categories.values().collect();
                cats.sort_by_key(|c| c.category_id);
                let rows = cats
                    .into_iter()
                    .map(|c| {
                        vec![
                            PyRep::Integer(c.category_id as i64),
                            PyRep::String(c.name.clone()),
                            PyRep::None,
                            PyRep::Integer(i64::from(c.published)),
                        ]
                    })
                    .collect();
                Some(PyRep::rowset(
                    vec!["categoryID", "categoryName", "iconID", "published"],
                    rows,
                ))
            }
            "config.StaticOwners" => {
                let mut rows = Vec::new();
                let mut fac_ids: Vec<u32> = self.sde.factions.keys().copied().collect();
                fac_ids.sort_unstable();
                for fid in fac_ids {
                    if let Some(fac) = self.sde.factions.get(&fid) {
                        rows.push(vec![
                            PyRep::Integer(fid as i64),
                            PyRep::String(fac.name.clone()),
                            PyRep::Integer(30),
                        ]);
                    }
                }
                let mut corp_ids: Vec<i64> = self.sde.npc_corporations.keys().copied().collect();
                corp_ids.sort_unstable();
                for cid in corp_ids {
                    if let Some(corp) = self.sde.npc_corporations.get(&cid) {
                        rows.push(vec![
                            PyRep::Integer(cid),
                            PyRep::String(corp.name.clone()),
                            PyRep::Integer(2),
                        ]);
                    }
                }
                Some(PyRep::rowset(
                    vec!["ownerID", "ownerName", "typeID"],
                    rows,
                ))
            }
            "(Method Call,server,(stationSvc,GetAllianceSystems))" => {
                let rows = self.db.with_snapshot(|snap| {
                    let mut out = Vec::new();
                    let mut sys_ids: Vec<u32> = snap.system_states.keys().copied().collect();
                    sys_ids.sort_unstable();
                    for sid in sys_ids {
                        if let Some(st) = snap.system_states.get(&sid) {
                            if let Some(aid) = st.sov_alliance_id {
                                out.push(vec![
                                    PyRep::Integer(sid as i64),
                                    PyRep::Integer(aid),
                                ]);
                            }
                        }
                    }
                    out
                });
                Some(PyRep::rowset(vec!["solarSystemID", "allianceID"], rows))
            }
            "config.Races" | "charCreationInfo.races" => {
                let mut races: Vec<&eve_sde::RaceInfo> = self.sde.races.values().collect();
                races.sort_by_key(|r| r.race_id);
                let rows = races
                    .into_iter()
                    .map(|r| {
                        vec![
                            PyRep::Integer(r.race_id as i64),
                            PyRep::String(r.name.clone()),
                            PyRep::String(r.name.clone()),
                            PyRep::None,
                            PyRep::String(r.name.clone()),
                        ]
                    })
                    .collect();
                Some(PyRep::rowset(
                    vec!["raceID", "raceName", "description", "iconID", "shortDescription"],
                    rows,
                ))
            }
            "config.Bloodlines" | "charCreationInfo.bloodlines" => {
                let mut bls: Vec<&eve_sde::BloodlineInfo> = self.sde.bloodlines.values().collect();
                bls.sort_by_key(|b| b.bloodline_id);
                let rows = bls
                    .into_iter()
                    .map(|b| {
                        vec![
                            PyRep::Integer(b.bloodline_id as i64),
                            PyRep::String(b.name.clone()),
                            PyRep::Integer(b.race_id as i64),
                            PyRep::Integer(b.corporation_id),
                        ]
                    })
                    .collect();
                Some(PyRep::rowset(
                    vec!["bloodlineID", "bloodlineName", "raceID", "corporationID"],
                    rows,
                ))
            }
            "config.Ancestries" | "charCreationInfo.ancestries" => {
                let mut ancs: Vec<&eve_sde::AncestryInfo> = self.sde.ancestries.values().collect();
                ancs.sort_by_key(|a| a.ancestry_id);
                let rows = ancs
                    .into_iter()
                    .map(|a| {
                        vec![
                            PyRep::Integer(a.ancestry_id as i64),
                            PyRep::String(a.name.clone()),
                            PyRep::Integer(a.bloodline_id as i64),
                        ]
                    })
                    .collect();
                Some(PyRep::rowset(
                    vec!["ancestryID", "ancestryName", "bloodlineID"],
                    rows,
                ))
            }
            "config.Schools" | "charCreationInfo.schools" => {
                let mut schs: Vec<&eve_sde::SchoolInfo> = self.sde.schools.values().collect();
                schs.sort_by_key(|s| s.school_id);
                let rows = schs
                    .into_iter()
                    .map(|s| {
                        vec![
                            PyRep::Integer(s.school_id as i64),
                            PyRep::String(s.name.clone()),
                            PyRep::Integer(s.race_id as i64),
                            PyRep::Integer(s.corporation_id),
                            PyRep::Integer(s.station_id),
                            PyRep::Integer(s.solar_system_id as i64),
                        ]
                    })
                    .collect();
                Some(PyRep::rowset(
                    vec![
                        "schoolID",
                        "schoolName",
                        "raceID",
                        "corporationID",
                        "stationID",
                        "solarSystemID",
                    ],
                    rows,
                ))
            }
            _ => None,
        }?;

        Some(self.put_cached_object(object_id.clone(), synthesized, shared))
    }

    /// Builds the `config.GetUnits` `util.IndexRowset` keyed by `unitID` (`ConfigDB.cpp:332-341`).
    pub fn build_config_units_payload(&self) -> PyRep {
        let units: &[(i64, &str, &str)] = &[
            (1, "Length", "m"),
            (2, "Mass", "kg"),
            (3, "Time", "sec"),
            (4, "Electric Current", "A"),
            (5, "Temperature", "K"),
            (9, "Volume", "m3"),
            (10, "Speed", "m/sec"),
            (101, "Milliseconds", "s"),
            (102, "Millimeters", "mm"),
            (104, "Multiplier", "x"),
            (105, "Percentage", "%"),
            (106, "Teraflops", "tf"),
            (107, "MegaWatts", "MW"),
            (108, "Inverse Absolute Percent", "%"),
            (109, "Modifier Percent", "%"),
            (111, "Inversed Modifier Percent", "%"),
            (113, "Hitpoints", "HP"),
            (114, "capacitor units", "GJ"),
            (115, "groupID", "groupID"),
            (116, "typeID", "typeID"),
            (117, "Sizeclass", "1=small 2=medium 3=l"),
            (119, "attributeID", "attributeID"),
            (121, "realPercent", "%"),
            (122, "Fitting slots", ""),
            (133, "ISK", "ISK"),
            (134, "m3/hour", "m3/h"),
            (139, "Bonus", "+"),
            (140, "Level", "Level"),
            (141, "Hardpoints", "hardpoints"),
        ];
        let items = units
            .iter()
            .map(|&(id, name, disp)| {
                (
                    PyRep::Integer(id),
                    vec![
                        PyRep::Integer(id),
                        PyRep::String(name.into()),
                        PyRep::String(disp.into()),
                    ],
                )
            })
            .collect();
        PyRep::index_rowset("unitID", vec!["unitID", "unitName", "displayName"], items)
    }

    /// Returns `true` if `character_id` (owned by `user_id`) currently has an active online session (`R51-P1-3`).
    pub fn is_character_online(&self, character_id: i64, user_id: u64) -> bool {
        self.db
            .get_online_session(user_id)
            .is_some_and(|os| os.character_id == Some(character_id))
    }

    /// Resolves online target `(character_id, user_id)` pairs from a [`NotificationTarget`] (`R51-P1-3`).
    pub fn resolve_notification_recipients(
        &self,
        target: &NotificationTarget,
    ) -> Vec<(i64, Option<u64>)> {
        let candidates: Vec<(i64, u64)> = self.db.with_snapshot(|snap| {
            let mut out = Vec::new();
            match target {
                NotificationTarget::Character(cid) => {
                    if let Some(c) = snap.characters.get(cid) {
                        out.push((*cid, c.user_id));
                    }
                }
                NotificationTarget::Corporation(corp_id) => {
                    for c in snap.characters.values() {
                        if c.corporation_id == *corp_id {
                            out.push((c.character_id, c.user_id));
                        }
                    }
                }
                NotificationTarget::Alliance(ally_id) => {
                    for c in snap.characters.values() {
                        if c.alliance_id == Some(*ally_id) {
                            out.push((c.character_id, c.user_id));
                        }
                    }
                }
                NotificationTarget::Fleet(fleet_id) => {
                    if let Some(f) = snap.fleets.get(fleet_id) {
                        for &cid in &f.members {
                            if let Some(c) = snap.characters.get(&cid) {
                                out.push((cid, c.user_id));
                            }
                        }
                    }
                }
                NotificationTarget::SolarSystem(sys_id) => {
                    for c in snap.characters.values() {
                        if c.solar_system_id == *sys_id {
                            out.push((c.character_id, c.user_id));
                        }
                    }
                }
                NotificationTarget::Station(station_id) => {
                    for c in snap.characters.values() {
                        if c.station_id == Some(*station_id)
                            || c.structure_id == Some(*station_id)
                        {
                            out.push((c.character_id, c.user_id));
                        }
                    }
                }
                NotificationTarget::All => {
                    for c in snap.characters.values() {
                        out.push((c.character_id, c.user_id));
                    }
                }
            }
            out
        });
        let mut out: Vec<(i64, Option<u64>)> = candidates
            .into_iter()
            .filter(|&(cid, uid)| self.is_character_online(cid, uid))
            .map(|(cid, uid)| (cid, Some(uid)))
            .collect();
        out.sort_by_key(|(cid, _)| *cid);
        out.dedup_by_key(|(cid, _)| *cid);
        out
    }

    /// Constructs a `MachoPacket` (`Notification` = `12`) targeting a client-side service
    /// (e.g. `target.FailLockTarget(targetID)`) via `MachoAddress::Client { service: Some(..) }` (`#1082`).
    pub fn build_macho_service_call_packet(
        node_id: u64,
        client_id: u64,
        user_id: Option<u64>,
        service: &str,
        method: &str,
        args: Vec<PyRep>,
    ) -> MachoPacket {
        let stream_payload = PyRep::Tuple(vec![PyRep::Tuple(vec![
            PyRep::Integer(0),
            PyRep::SubStream(Box::new(PyRep::Tuple(vec![
                PyRep::Integer(1),
                PyRep::String(method.to_string()),
                PyRep::Tuple(args),
            ]))),
        ])]);
        MachoPacket {
            packet_type: MachoPacketType::Notification,
            source: MachoAddress::Node {
                node_id,
                service: None,
                call_id: 0,
            },
            destination: MachoAddress::Client {
                client_id,
                call_id: 0,
                service: Some(service.to_string()),
            },
            user_id,
            payload: stream_payload,
            named_payload: HashMap::new(),
        }
    }

    /// Constructs a `MachoPacket` (`Notification` = `12`) and encodes it into binary `eveMarshal` wire bytes.
    pub fn build_macho_notification_packet(
        recipient_char_id: i64,
        user_id: Option<u64>,
        event: &str,
        id_type: &str,
        args: Vec<PyRep>,
    ) -> (MachoPacket, Vec<u8>) {
        if let Some(service) = id_type.strip_prefix("service:") {
            let pkt = Self::build_macho_service_call_packet(
                888_444,
                recipient_char_id.max(0) as u64,
                user_id,
                service,
                event,
                args,
            );
            let marshaled = marshal::encode(&pkt.to_pyrep());
            return (pkt, marshaled);
        }
        let stream_payload = PyRep::Tuple(vec![PyRep::Tuple(vec![
            PyRep::Integer(0),
            PyRep::SubStream(Box::new(PyRep::Tuple(vec![
                PyRep::Integer(0),
                PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Tuple(args)]),
            ]))),
        ])]);
        let pkt = MachoPacket {
            packet_type: MachoPacketType::Notification,
            source: MachoAddress::Node { node_id: 888_444, service: None, call_id: 0 },
            destination: MachoAddress::Broadcast {
                broadcast_id: event.to_string(),
                id_type: id_type.to_string(),
                narrowcast: vec![recipient_char_id.max(0) as u64],
            },
            user_id,
            payload: stream_payload,
            named_payload: HashMap::new(),
        };
        let marshaled = marshal::encode(&pkt.to_pyrep());
        (pkt, marshaled)
    }

    /// Constructs a `MachoPacket` (`CallRsp` = `7`) and encodes it into binary `eveMarshal` wire bytes (`Z-1`).
    pub fn build_macho_call_response_packet(
        source_service: Option<&str>,
        dest_client_id: u64,
        call_id: u64,
        user_id: Option<u64>,
        value: PyRep,
    ) -> (MachoPacket, Vec<u8>) {
        let pkt = MachoPacket {
            packet_type: MachoPacketType::CallRsp,
            source: MachoAddress::Node { node_id: 888_444, service: source_service.map(str::to_string), call_id },
            destination: MachoAddress::Client { client_id: dest_client_id, call_id, service: None },
            user_id,
            payload: PyRep::Tuple(vec![PyRep::SubStream(Box::new(value))]),
            named_payload: HashMap::new(),
        };
        let marshaled = marshal::encode(&pkt.to_pyrep());
        (pkt, marshaled)
    }

    /// Constructs a `MachoPacket` (`ErrorResponse` = `15`) and encodes it into binary `eveMarshal` wire bytes (`Z-1`).
    pub fn build_macho_error_response_packet(
        source_service: Option<&str>,
        dest_client_id: u64,
        call_id: u64,
        user_id: Option<u64>,
        error_code: &str,
        error_message: &str,
    ) -> (MachoPacket, Vec<u8>) {
        let pkt = MachoPacket {
            packet_type: MachoPacketType::ErrorResponse,
            source: MachoAddress::Node { node_id: 888_444, service: source_service.map(str::to_string), call_id },
            destination: MachoAddress::Client { client_id: dest_client_id, call_id, service: None },
            user_id,
            payload: PyRep::Tuple(vec![
                PyRep::Integer(MachoPacketType::CallReq as i64),
                PyRep::Integer(1),
                PyRep::Tuple(vec![PyRep::SubStream(Box::new(PyRep::keyval([
                    ("code", PyRep::String(error_code.to_string())),
                    ("message", PyRep::String(error_message.to_string())),
                ])))]),
            ]),
            named_payload: HashMap::new(),
        };
        let marshaled = marshal::encode(&pkt.to_pyrep());
        (pkt, marshaled)
    }

    /// Returns the active online session's transition `scene_epoch` for `char_id`, if any (`#1190`).
    pub fn active_session_scene_epoch_for_char(&self, char_id: i64) -> Option<u64> {
        let uid = self
            .db
            .with_snapshot(|snap| snap.characters.get(&char_id).map(|c| c.user_id))?;
        self.db
            .get_online_session(uid)
            .filter(|os| os.character_id == Some(char_id))
            .map(|os| self.session_scene_epoch(os.session_id))
    }

    /// Resolves a space [`crate::outbound_sequencer::OutboundSceneScope`] bound to `sys_id` and
    /// `char_id`'s current `scene_epoch` (`#1190`, `#1565`).
    pub fn character_space_scene_scope(
        &self,
        char_id: i64,
        sys_id: Option<u32>,
    ) -> crate::outbound_sequencer::OutboundSceneScope {
        let resolved_sys = sys_id.or_else(|| {
            self.db
                .with_snapshot(|s| s.characters.get(&char_id).map(|c| c.solar_system_id))
        });
        let epoch = self.active_session_scene_epoch_for_char(char_id);
        crate::outbound_sequencer::OutboundSceneScope::space(resolved_sys, epoch)
    }

    /// Emits a binary-marshaled `MachoPacket` notification to all online recipients matching `target` (`P-5`, `R51-P1-3`).
    pub fn notify(
        &self,
        target: NotificationTarget,
        event: &str,
        id_type: &str,
        args: Vec<PyRep>,
    ) -> Vec<PendingMachoNotification> {
        let default_scope = match &target {
            NotificationTarget::SolarSystem(sys_id) => Some(
                crate::outbound_sequencer::OutboundSceneScope::space(Some(*sys_id), None),
            ),
            _ => None,
        };
        self.notify_inner(target, event, id_type, args, false, default_scope)
    }

    /// Scene-scoped variant of [`Self::notify`], tagging emitted notifications with
    /// [`crate::outbound_sequencer::OutboundSceneScope`] so `outbound_sequencer` drops stale-epoch
    /// or mismatched-scene frames and holds newer-epoch frames until scene entry (`#1190` RB3).
    pub fn notify_in_scene(
        &self,
        target: NotificationTarget,
        event: &str,
        id_type: &str,
        args: Vec<PyRep>,
        scene_scope: Option<crate::outbound_sequencer::OutboundSceneScope>,
    ) -> Vec<PendingMachoNotification> {
        self.notify_inner(target, event, id_type, args, false, scene_scope)
    }

    fn notify_inner(
        &self,
        target: NotificationTarget,
        event: &str,
        id_type: &str,
        args: Vec<PyRep>,
        already_switch_checked: bool,
        scene_scope: Option<crate::outbound_sequencer::OutboundSceneScope>,
    ) -> Vec<PendingMachoNotification> {
        let recipients = self.resolve_notification_recipients(&target);
        let now_ft = self.now_blueos_filetime_ticks();
        if event == "OnAccountChange" {
            if let Some(emitted) = super::delta_commit_listener::try_handle_legacy_account_change_emit(
                self, &recipients, id_type, &args, now_ft,
            ) {
                return emitted;
            }
        } else if matches!(
            event,
            "OnItemChange" | "OnMultipleItemChange" | "OnBlueprintsUpdated"
        ) {
            self.observe_active_fuel_emit(&recipients, event, &args);
            if !already_switch_checked && event == "OnItemChange" && recipients.len() == 1 {
                if self.try_consume_projected_on_item_change_args(recipients[0].0, &args, "notify") {
                    return Vec::new();
                }
            }
            super::delta_item_compare::record_legacy_item_compare(
                self, "notify", &recipients, id_type, event, &args,
            );
        }
        let mut emitted = Vec::with_capacity(recipients.len());
        for (cid, uid) in recipients {
            let mut recipient_args = args.clone();
            if event == "OnModuleAttributeChanges" {
                if cid > 0 {
                    if let Some(PyRep::List(items) | PyRep::Tuple(items)) =
                        recipient_args.first_mut()
                    {
                        for item in items.iter_mut() {
                            if let PyRep::Tuple(t) | PyRep::List(t) = item {
                                if matches!(t.get(1), Some(PyRep::None)) {
                                    t[1] = PyRep::Integer(cid);
                                }
                                if matches!(t.get(4), Some(PyRep::Integer(0))) {
                                    t[4] = PyRep::Integer(now_ft);
                                }
                            }
                        }
                    }
                }
                recipient_args = crate::router::client_compat::normalize_macho_notification_with_sde(
                    event,
                    recipient_args,
                    Some(&self.sde),
                );
            }
            let recipient_scope = scene_scope.map(|mut scope| {
                if scope.scene_epoch.is_none() {
                    scope.scene_epoch = uid
                        .and_then(|u| self.db.get_online_session(u))
                        .filter(|os| os.character_id == Some(cid))
                        .map(|os| self.session_scene_epoch(os.session_id))
                        .or_else(|| self.active_session_scene_epoch_for_char(cid));
                }
                scope
            });
            emitted.push(
                assemble_pending_notification(
                    cid,
                    uid,
                    event,
                    id_type,
                    recipient_args,
                    now_ft,
                )
                .with_scene_scope(recipient_scope),
            );
        }
        if !emitted.is_empty() {
            if is_staging_dispatch_notifications(Some(&*self.pending_notifications), Some(&self.notify_tx)) {
                for n in &emitted {
                    let _ = try_stage_dispatch_notification(Some(&*self.pending_notifications), Some(&self.notify_tx), n, None);
                }
            } else {
                let mut queues = self.pending_notifications.write().unwrap_or_else(|e| e.into_inner());
                for n in &emitted {
                    push_pending_notification_capped(queues.entry(n.recipient_char_id).or_default(), n.clone());
                    let _ = self.notify_tx.send((n.recipient_char_id, n.clone()));
                }
            }
        }
        emitted
    }

    pub(crate) fn notify_with_source(
        &self,
        target: NotificationTarget,
        event: &str,
        id_type: &str,
        args: Vec<PyRep>,
        source: &'static str,
    ) -> Vec<PendingMachoNotification> {
        let _scope = super::delta_item_compare::push_compare_source_scope(self, source);
        self.notify_inner(target, event, id_type, args, true, None)
    }

    /// Returns the highest queued `seq_id` for `char_id` (or `0` when empty).
    pub fn latest_notification_seq_for_char(&self, char_id: i64) -> u64 {
        self.pending_notifications
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&char_id)
            .and_then(|q| q.iter().map(|n| n.seq_id).max())
            .unwrap_or(0)
    }

    /// Drains all pending notifications queued for `char_id`.
    pub fn drain_notifications_for_char(&self, char_id: i64) -> Vec<PendingMachoNotification> {
        self.pending_notifications
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&char_id)
            .unwrap_or_default()
    }

    /// Purges all pending notifications queued for `char_id` on logoff or character re-selection (`#881` / `C19`).
    pub fn clear_notifications_for_char(&self, char_id: i64) {
        self.pending_notifications
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&char_id);
        clear_staged_notifications_for_char(&*self.pending_notifications, char_id);
    }

    /// Re-queues system shutdown/maintenance notifications after `logoff_character_sync` clears stale per-character queues.
    pub fn restore_pending_notifications(&self, notifs: Vec<PendingMachoNotification>) {
        if notifs.is_empty() {
            return;
        }
        let mut queues = self
            .pending_notifications
            .write()
            .unwrap_or_else(|e| e.into_inner());
        for notif in notifs {
            let q = queues.entry(notif.recipient_char_id).or_default();
            if !q.iter().any(|existing| existing.seq_id == notif.seq_id) {
                push_pending_notification_capped(q, notif);
            }
        }
    }

    /// Emits an `OnRemoteMessage` notification (`msg_key`, `dict`).
    pub fn notify_remote_message(
        &self,
        target: NotificationTarget,
        msg_key: &str,
        entries: Vec<(&str, PyRep)>,
    ) -> Vec<PendingMachoNotification> {
        let dict = PyRep::Dict(
            entries
                .into_iter()
                .map(|(k, v)| (PyRep::String(k.into()), v))
                .collect(),
        );
        self.notify(
            target,
            "OnRemoteMessage",
            "charid",
            vec![PyRep::String(msg_key.into()), dict],
        )
    }

    /// Emits an `OnItemChange` notification (`itemRow` packed `DBRow`, `changeDict`).
    pub fn notify_item_change(&self, char_id: i64, item_row: PyRep, changes: Vec<(i32, PyRep)>) -> Vec<PendingMachoNotification> {
        if self.try_consume_projected_item_change_row_with_changes(char_id, &item_row, &changes, "notify_item_change") {
            return Vec::new();
        }
        self.notify_with_source(NotificationTarget::Character(char_id), "OnItemChange", "charid", vec![item_row, super::delta_item_switch::build_item_change_dict(changes)], "notify_item_change")
    }

    /// Emits an `OnSkillTrained` notification (`skillItemID`, `skillTypeID`, `level`).
    pub fn notify_skill_trained(
        &self,
        char_id: i64,
        skill_item_id: i64,
        skill_type_id: u32,
        level: u8,
    ) -> Vec<PendingMachoNotification> {
        self.notify(
            NotificationTarget::Character(char_id),
            "OnSkillTrained",
            "charid",
            vec![PyRep::Integer(skill_item_id), PyRep::Integer(skill_type_id as i64), PyRep::Integer(level as i64)],
        )
    }

    /// Emits an `OnSkillQueuePaused` notification to character (`feat:skills.queue-pause`).
    pub fn notify_skill_queue_paused(&self, char_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnSkillQueuePaused", "charid", vec![])
    }

    /// Emits an `OnSkillQueueChanged` notification to character.
    pub fn notify_skill_queue_changed(&self, char_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnSkillQueueChanged", "charid", vec![])
    }

    /// Creates a persistent notification in `self.notification_records` and pushes an `OnNotificationReceived`
    /// `MachoPacket` notification to `receiver_id`.
    pub fn create_character_notification(
        &self,
        sender_id: i64,
        receiver_id: i64,
        type_id: i32,
        group_id: i32,
        data: &str,
    ) -> NotificationRecord {
        let max_in_mem = self
            .notification_records
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|r| r.notification_id)
            .max()
            .unwrap_or(0);
        let notification_id = self
            .db
            .with_mut_snapshot(|snap| snap.allocate_notification_id_above(max_in_mem));
        let created = self.now_blueos_filetime_ticks();
        let rec = NotificationRecord {
            notification_id, type_id, group_id, sender_id, receiver_id,
            processed: false, created, data: data.to_string(), deleted: false, interacted: false,
        };
        self.notification_records.write().unwrap_or_else(|e| e.into_inner()).push(rec.clone());
        self.persist_notification_record(&rec);
        let _ = self.notify(
            NotificationTarget::Character(receiver_id),
            "OnNotificationReceived",
            "charid",
            vec![
                PyRep::Integer(notification_id),
                PyRep::Integer(type_id as i64),
                PyRep::Integer(sender_id),
                PyRep::Integer(created),
                PyRep::String(data.to_string()),
            ],
        );
        rec
    }

    pub fn persist_notification_record(&self, rec: &NotificationRecord) {
        let now = self.now_unix();
        if let Ok(json) = serde_json::to_string(rec) {
            let note_id = -(7_900_000_000_000 + rec.notification_id);
            self.db.with_mut_snapshot(|snap| {
                snap.owner_notes.insert(note_id, eve_db::OwnerNoteRecord {
                    note_id, character_id: -79, item_id: rec.receiver_id,
                    label: format!("notif:{}", rec.notification_id), note: json, timestamp_unix: now,
                });
            });
        }
    }

    pub fn sync_character_notifications_to_db(&self, char_id: i64) {
        let now = self.now_unix();
        let char_recs: Vec<NotificationRecord> = self.notification_records.read().unwrap_or_else(|e| e.into_inner())
            .iter().filter(|r| r.receiver_id == char_id).cloned().collect();
        self.db.with_mut_snapshot(|snap| {
            for rec in &char_recs {
                if let Ok(json) = serde_json::to_string(rec) {
                    let note_id = -(7_900_000_000_000 + rec.notification_id);
                    snap.owner_notes.insert(note_id, eve_db::OwnerNoteRecord {
                        note_id, character_id: -79, item_id: rec.receiver_id,
                        label: format!("notif:{}", rec.notification_id), note: json, timestamp_unix: now,
                    });
                }
            }
        });
    }

    pub fn deliver_unprocessed_notifications_on_login(&self, char_id: i64) {
        let user_id = self.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.user_id));
        let unprocessed: Vec<NotificationRecord> = self.notification_records.read().unwrap_or_else(|e| e.into_inner())
            .iter().filter(|r| r.receiver_id == char_id && !r.deleted && !r.processed).cloned().collect();
        if unprocessed.is_empty() { return; }
        let now_ft = self.now_blueos_filetime_ticks();
        let stage_in_dispatch = is_staging_dispatch_notifications(
            Some(&*self.pending_notifications),
            Some(&self.notify_tx),
        );
        let mut queues = (!stage_in_dispatch).then(|| {
            self.pending_notifications
                .write()
                .unwrap_or_else(|e| e.into_inner())
        });
        for rec in unprocessed {
            let already_queued = if let Some(queues) = queues.as_ref() {
                queues.get(&char_id).is_some_and(|q| {
                    q.iter().any(|n| {
                        n.event == "OnNotificationReceived"
                            && n.args.first() == Some(&PyRep::Integer(rec.notification_id))
                    })
                })
            } else {
                self.pending_notifications
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&char_id)
                    .is_some_and(|q| {
                        q.iter().any(|n| {
                            n.event == "OnNotificationReceived"
                                && n.args.first() == Some(&PyRep::Integer(rec.notification_id))
                        })
                    })
                    || has_staged_notification_for_char_and_id(
                        &*self.pending_notifications,
                        char_id,
                        "OnNotificationReceived",
                        rec.notification_id,
                    )
            };
            if already_queued {
                continue;
            }
            let args = vec![
                PyRep::Integer(rec.notification_id), PyRep::Integer(rec.type_id as i64),
                PyRep::Integer(rec.sender_id), PyRep::Integer(rec.created), PyRep::String(rec.data.clone()),
            ];
            let (packet, marshaled_packet) = Self::build_macho_notification_packet(char_id, user_id, "OnNotificationReceived", "charid", args.clone());
            let notif = PendingMachoNotification::new(
                char_id,
                "OnNotificationReceived",
                "charid",
                args,
                packet,
                marshaled_packet,
                now_ft,
            );
            if let Some(queues) = queues.as_mut() {
                let q = queues.entry(char_id).or_default();
                push_pending_notification_capped(q, notif.clone());
                let _ = self.notify_tx.send((char_id, notif));
            } else {
                let _ = try_stage_dispatch_notification(
                    Some(&*self.pending_notifications),
                    Some(&self.notify_tx),
                    &notif,
                    None,
                );
            }
        }
    }

    /// Emits an `OnDamageStateChange` notification (`itemID`, `(shield, armor, structure)`) (`Z-2`).
    /// Emits an `OnDamageStateChange` notification for `item_id` (`R33-2`).
    pub fn notify_damage_state_change(
        &self,
        target: NotificationTarget,
        item_id: i64,
        shield: f64,
        armor: f64,
        structure: f64,
    ) -> Vec<PendingMachoNotification> {
        let dmg = crate::services::notify_emitters_space_dungeons::build_damage_state_tuple(
            self, item_id, shield, armor, structure,
        );
        let sys_opt = match target {
            NotificationTarget::SolarSystem(sid) => Some(sid),
            NotificationTarget::Character(cid) => self.db.with_snapshot(|snap| {
                snap.items
                    .get(&item_id)
                    .and_then(|it| {
                        u32::try_from(it.location_id)
                            .ok()
                            .filter(|&loc| (30_000_000..40_000_000).contains(&loc))
                    })
                    .or_else(|| {
                        snap.characters
                            .get(&cid)
                            .filter(|c| {
                                c.station_id.is_none()
                                    && c.structure_id.is_none_or(|st| c.active_ship_id == st)
                            })
                            .map(|c| c.solar_system_id)
                    })
            }),
            _ => None,
        };
        let scope = Some(crate::outbound_sequencer::OutboundSceneScope::space(
            sys_opt, None,
        ));
        self.notify_in_scene(
            target,
            "OnDamageStateChange",
            "itemid",
            vec![PyRep::Integer(item_id), dmg],
            scope,
        )
    }

    /// Emits an `OnGodmaShipEffect` notification for active module cycle events (`Z-2`).
    pub fn notify_godma_ship_effect(
        &self,
        target: NotificationTarget,
        item_id: i64,
        effect_id: i32,
        active: bool,
        duration_ms: i64,
        repeat: i32,
    ) -> Vec<PendingMachoNotification> {
        let sys_opt = match target {
            NotificationTarget::SolarSystem(sid) => Some(sid),
            NotificationTarget::Character(cid) => self.db.with_snapshot(|s| {
                s.characters
                    .get(&cid)
                    .filter(|c| {
                        c.station_id.is_none()
                            && c.structure_id.is_none_or(|st| c.active_ship_id == st)
                    })
                    .map(|c| c.solar_system_id)
            }),
            _ => None,
        };
        let now_ticks = self.now_system_sim_filetime_ticks(sys_opt);
        let flag = if active { 1 } else { 0 };
        let env = PyRep::Tuple(vec![
            PyRep::Integer(item_id), PyRep::None, PyRep::Integer(item_id),
            PyRep::None, PyRep::None, PyRep::List(Vec::new()), PyRep::Integer(effect_id as i64),
        ]);
        let scope = if matches!(target, NotificationTarget::SolarSystem(_))
            || (effect_id != 16 && sys_opt.is_some())
        {
            Some(crate::outbound_sequencer::OutboundSceneScope::space(
                sys_opt, None,
            ))
        } else {
            None
        };
        self.notify_in_scene(
            target,
            "OnGodmaShipEffect",
            "charid",
            vec![
                PyRep::Integer(item_id), PyRep::Integer(effect_id as i64), PyRep::Integer(now_ticks),
                PyRep::Integer(flag), PyRep::Integer(flag), env, PyRep::Integer(now_ticks),
                PyRep::Real((duration_ms.max(0)) as f64), PyRep::Integer(repeat as i64),
                PyRep::Integer(0), PyRep::None,
            ],
            scope,
        )
    }

    /// Emits an `OnFleetBroadcast` notification (`broadcastName`, `senderID`, `targetID`, `fleetID`) (`Z-2`).
    pub fn notify_fleet_broadcast(
        &self,
        fleet_id: i64,
        broadcast_name: &str,
        sender_id: i64,
        target_id: i64,
    ) -> Vec<PendingMachoNotification> {
        self.notify(
            NotificationTarget::Fleet(fleet_id),
            "OnFleetBroadcast",
            "fleetid",
            vec![
                PyRep::String(broadcast_name.to_string()),
                PyRep::Integer(sender_id),
                PyRep::Integer(target_id),
                PyRep::Integer(fleet_id),
            ],
        )
    }

    /// Emits an `OnClientEvent_ChatMessageSent` chat message notification (`channelID`, `senderID`, `text`) (`Z-2`).
    pub fn notify_lsc_message(
        &self,
        target: NotificationTarget,
        _channel_id: &str,
        _sender_id: i64,
        _text: &str,
    ) -> Vec<PendingMachoNotification> {
        self.notify(target, "OnClientEvent_ChatMessageSent", "channelid", Vec::new())
    }

    /// Emits an `OnWarpToAsteroidBelt` notification (`charID`, `beltID`) (`Z-2`).
    pub fn notify_warp_to_asteroid_belt(&self, char_id: i64, belt_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(
            NotificationTarget::Character(char_id),
            "OnWarpToAsteroidBelt",
            "charid",
            vec![PyRep::Integer(char_id), PyRep::Integer(belt_id)],
        )
    }

    /// Emits an `OnAsteroidTerminated` notification (`asteroidID`) to the solar system (`Z-2`).
    pub fn notify_asteroid_terminated(&self, sys_id: u32, asteroid_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::SolarSystem(sys_id), "OnAsteroidTerminated", "solarsystemid", vec![PyRep::Integer(asteroid_id)])
    }

    /// Emits an `OnMiningLedgerGraphSettingChanged` notification (`settingKey`, `settingValue`) (`Z-2`).
    pub fn notify_mining_ledger_graph_setting_changed(
        &self,
        char_id: i64,
        setting_key: &str,
        setting_value: PyRep,
    ) -> Vec<PendingMachoNotification> {
        self.notify(
            NotificationTarget::Character(char_id),
            "OnMiningLedgerGraphSettingChanged",
            "charid",
            vec![PyRep::String(setting_key.to_string()), setting_value],
        )
    }

    /// Synchronizes local chat membership for a character when entering or changing solar systems (SO-3).
    pub fn sync_character_local_chat(
        &self,
        char_id: i64,
        old_sys: Option<u32>,
        new_sys: u32,
    ) {
        let old_chan_opt = old_sys
            .filter(|&old| old != new_sys)
            .map(|old| format!("solarsystemid2:{old}"));
        let candidate_speakers = old_chan_opt
            .as_deref()
            .map(|cid| {
                crate::services::fleet_social::fleet_social_chat::get_delayed_local_speakers(
                    self, cid,
                )
            })
            .unwrap_or_default();
        let mut dropped_speakers = Vec::new();
        let _ = self.db.try_with_mut_snapshot(|snap| {
            if let Some(ref old_chan_id) = old_chan_opt {
                if let Some(chan) = snap.chat_channels.get_mut(old_chan_id) {
                    chan.members.remove(&char_id);
                    let rem_set = chan.members.clone();
                    dropped_speakers = crate::services::fleet_social::fleet_social_chat::collect_wh_local_witness_speakers_to_drop_in_snap(
                        snap,
                        old_chan_id,
                        char_id,
                        &rem_set,
                        &candidate_speakers,
                    );
                }
            }
            let new_chan_id = format!("solarsystemid2:{}", new_sys);
            let chan = snap.chat_channels.entry(new_chan_id.clone()).or_insert_with(|| eve_db::ChatChannelRecord {
                channel_id: new_chan_id.clone(), name: format!("Local: {}", new_sys), motd: String::new(),
                owner_id: None, members: std::collections::HashSet::new(), is_system_local: true,
                solar_system_id: Some(new_sys), is_private: false, operators: std::collections::HashSet::new(),
                password: None, is_delayed_local: crate::services::quasar_social_chat::is_delayed_local_system(new_sys),
            });
            chan.members.insert(char_id);
            Ok::<(), std::convert::Infallible>(())
        });
        if let Some(ref old_chan_id) = old_chan_opt {
            crate::services::fleet_social::fleet_social_chat::remove_delayed_local_speakers(
                self,
                old_chan_id,
                &dropped_speakers,
            );
        }

        if old_sys.is_none() {
            self.deliver_unprocessed_notifications_on_login(char_id);
        }

        let ch_res = self.db.get_character(char_id).ok();
        {
            let mut subs = self.quasar_gateway.subscribers.write().unwrap_or_else(|e| e.into_inner());
            for sub in subs.values_mut() {
                if sub.character_id == Some(char_id as u32) {
                    sub.solar_system_id = None;
                } else if sub.character_id.is_none() && sub.user_id.is_some() {
                    if let Some(ch) = &ch_res {
                        if Some(ch.user_id as i32) == sub.user_id {
                            sub.character_id = Some(char_id as u32);
                            sub.solar_system_id = None;
                            sub.corporation_id = Some(ch.corporation_id as u32);
                            sub.alliance_id = ch.alliance_id.map(|a| a as i32);
                        }
                    }
                }
            }
        }

        let now = self.now_unix();
        if let Some(old) = old_sys {
            if old != new_sys {
                let _ = crate::services::quasar_social_chat::on_character_leave_local_chat(
                    &self.quasar_gateway, &self.db, char_id as u32, old, now,
                );
            }
        }
        let _ = crate::services::quasar_social_chat::on_character_enter_local_chat(
            &self.quasar_gateway, &self.db, char_id as u32, new_sys, now,
        );
        if let Ok(mut subs) = self.quasar_gateway.subscribers.write() {
            for sub in subs.values_mut() {
                if sub.character_id == Some(char_id as u32) {
                    sub.solar_system_id = Some(new_sys);
                }
            }
        }

        if !crate::services::quasar_social_chat::is_delayed_local_system(new_sys) {
            self.notify(
                NotificationTarget::SolarSystem(new_sys), "OnChannelJoined", "channelid",
                vec![PyRep::String(format!("solarsystemid2:{}", new_sys)), PyRep::String("Join".into()), PyRep::Integer(char_id)],
            );
        }
        if old_sys != Some(new_sys) {
            crate::services::pve_incursion_lifecycle::emit_active_incursion_tale_data_for_system(
                self, new_sys,
            );
        }
    }

    pub fn effect_id_for_module(&self, type_id: u32, effect_name: Option<&str>) -> Option<i64> {
        crate::router::binding::effect_id_for_module_sde(&self.sde, type_id, effect_name)
    }

    /// Returns `(char_id, user_id)` pairs for online in-space characters in the same solar system
    /// as `ship_id` / `exclude_char_id` (excluding `exclude_char_id`), for `OnSpecialFX` fan-out.
    pub fn space_observer_chars_for_ship(
        &self,
        ship_id: i64,
        exclude_char_id: i64,
    ) -> Vec<(i64, u64)> {
        let candidates: Vec<(i64, u64)> = self.db.with_snapshot(|snap| {
            let solar_system_id = snap
                .characters
                .get(&exclude_char_id)
                .map(|c| c.solar_system_id)
                .or_else(|| {
                    snap.characters
                        .values()
                        .find(|c| c.active_ship_id == ship_id)
                        .map(|c| c.solar_system_id)
                });
            let Some(sys_id) = solar_system_id else {
                return Vec::new();
            };
            snap.characters
                .values()
                .filter(|c| {
                    c.character_id != exclude_char_id
                        && c.solar_system_id == sys_id
                        && c.station_id.is_none()
                        && c.structure_id.is_none()
                })
                .map(|c| (c.character_id, c.user_id))
                .collect()
        });
        let mut out: Vec<(i64, u64)> = candidates
            .into_iter()
            .filter(|&(cid, uid)| self.is_character_online(cid, uid))
            .collect();
        out.sort_unstable_by_key(|&(cid, _)| cid);
        out.dedup_by_key(|&mut (cid, _)| cid);
        out
    }

    pub fn notify_module_effect_started(
        &self,
        char_id: i64,
        ship_id: i64,
        module_id: i64,
        module_type_id: u32,
        effect_name: Option<&str>,
        target_id: Option<i64>,
    ) -> Vec<PendingMachoNotification> {
        self.notify_module_effect_started_with_cycle_impl(
            char_id, ship_id, module_id, module_type_id, effect_name, target_id, None, None, None, true,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn notify_module_effect_started_with_cycle(
        &self,
        char_id: i64,
        ship_id: i64,
        module_id: i64,
        module_type_id: u32,
        effect_name: Option<&str>,
        target_id: Option<i64>,
        repeat_opt: Option<i32>,
        duration_ms_opt: Option<f64>,
    ) -> Vec<PendingMachoNotification> {
        self.notify_module_effect_started_with_cycle_impl(
            char_id, ship_id, module_id, module_type_id, effect_name, target_id, repeat_opt, duration_ms_opt, None, true,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn notify_module_effect_started_with_cycle_and_charge(
        &self,
        char_id: i64,
        ship_id: i64,
        module_id: i64,
        module_type_id: u32,
        effect_name: Option<&str>,
        target_id: Option<i64>,
        repeat_opt: Option<i32>,
        duration_ms_opt: Option<f64>,
        charge_type_id_override: Option<u32>,
    ) -> Vec<PendingMachoNotification> {
        self.notify_module_effect_started_with_cycle_impl(
            char_id, ship_id, module_id, module_type_id, effect_name, target_id,
            repeat_opt, duration_ms_opt, charge_type_id_override, true,
        )
    }

    /// Emits `OnGodmaShipEffect` (`start=1`) without `OnSpecialFX` (used by guided missiles whose
    /// deployment `OnSpecialFX` is broadcast after next-tick `AddBalls2`).
    #[allow(clippy::too_many_arguments)]
    pub fn notify_module_godma_effect_started_only(
        &self,
        char_id: i64,
        ship_id: i64,
        module_id: i64,
        module_type_id: u32,
        effect_name: Option<&str>,
        target_id: Option<i64>,
        repeat_opt: Option<i32>,
        duration_ms_opt: Option<f64>,
    ) -> Vec<PendingMachoNotification> {
        self.notify_module_effect_started_with_cycle_impl(
            char_id, ship_id, module_id, module_type_id, effect_name, target_id,
            repeat_opt, duration_ms_opt, None, false,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn notify_module_effect_started_with_cycle_impl(
        &self,
        char_id: i64,
        ship_id: i64,
        module_id: i64,
        module_type_id: u32,
        effect_name: Option<&str>,
        target_id: Option<i64>,
        repeat_opt: Option<i32>,
        duration_ms_opt: Option<f64>,
        charge_type_id_override: Option<u32>,
        emit_special_fx: bool,
    ) -> Vec<PendingMachoNotification> {
        let effect_id = self.effect_id_for_module(module_type_id, effect_name).unwrap_or(0);
        let duration_ms = duration_ms_opt.unwrap_or_else(|| {
            self.ship_fittings
                .read()
                .ok()
                .and_then(|map| map.get(&ship_id).cloned())
                .filter(|fit| fit.modules.contains_key(&module_id))
                .map(|fit| {
                    let d73 = fit.get_module_effective_attribute(module_id, 73, &self.sde);
                    if d73 > 0.0 {
                        d73
                    } else {
                        fit.get_module_effective_attribute(module_id, 51, &self.sde)
                    }
                })
                .or_else(|| {
                    self.sde.types.get(&module_type_id).and_then(|t| {
                        t.default_attributes
                            .get(&73)
                            .or_else(|| t.default_attributes.get(&51))
                            .copied()
                    })
                })
                .unwrap_or(0.0)
        });
        let repeat = if effect_id == 16 {
            0
        } else {
            repeat_opt.unwrap_or(1000)
        };
        let now = self.now_system_sim_filetime_ticks_for_ship(char_id, ship_id);
        let environment = PyRep::Tuple(vec![
            PyRep::Integer(module_id),
            PyRep::Integer(char_id),
            PyRep::Integer(ship_id),
            target_id.map(PyRep::Integer).unwrap_or(PyRep::None),
            PyRep::None,
            PyRep::List(vec![]),
            PyRep::Integer(effect_id),
        ]);
        let notifs = self.notify(
            NotificationTarget::Character(char_id),
            "OnGodmaShipEffect",
            "charid",
            vec![
                PyRep::Integer(module_id),
                PyRep::Integer(effect_id),
                PyRep::Integer(now),
                PyRep::Integer(1),
                PyRep::Integer(1),
                environment,
                PyRep::Integer(now),
                PyRep::Real(duration_ms),
                PyRep::Integer(i64::from(repeat)),
                PyRep::None,
            ],
        );
        if emit_special_fx {
            self.emit_module_special_fx_started(
                char_id,
                ship_id,
                module_id,
                module_type_id,
                effect_id,
                effect_name,
                target_id,
                repeat,
                duration_ms,
                now,
                charge_type_id_override,
            );
        }
        notifs
    }

    /// Emits only `OnSpecialFX` (`start=1`, `active=1`) to `char_id` and same-system space observers
    /// (used for grouped weapon bank slave turrets so every barrel emits visual FX without duplicating `OnGodmaShipEffect`).
    #[allow(clippy::too_many_arguments)]
    pub fn notify_module_special_fx_started(
        &self,
        char_id: i64,
        ship_id: i64,
        module_id: i64,
        module_type_id: u32,
        effect_name: Option<&str>,
        target_id: Option<i64>,
        repeat: i32,
        duration_ms: f64,
        charge_type_id_override: Option<u32>,
    ) {
        let effect_id = self.effect_id_for_module(module_type_id, effect_name).unwrap_or(0);
        let now = self.now_system_sim_filetime_ticks_for_ship(char_id, ship_id);
        self.emit_module_special_fx_started(
            char_id,
            ship_id,
            module_id,
            module_type_id,
            effect_id,
            effect_name,
            target_id,
            repeat,
            duration_ms,
            now,
            charge_type_id_override,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_module_special_fx_started(
        &self,
        char_id: i64,
        ship_id: i64,
        module_id: i64,
        module_type_id: u32,
        effect_id: i64,
        effect_name: Option<&str>,
        target_id: Option<i64>,
        repeat: i32,
        duration_ms: f64,
        now: i64,
        charge_type_id_override: Option<u32>,
    ) {
        if let Some((sfx_guid, is_offensive)) =
            crate::router::binding::special_fx_guid_for_module_sde(
                &self.sde,
                module_type_id,
                effect_id,
                effect_name,
            )
            .filter(|(guid, _)| guid != "effects.Cloak")
        {
            let charge_type_id = charge_type_id_override
                .or_else(|| {
                    self.db
                        .with_snapshot(|snap| snap.module_active_charges.get(&module_id).copied())
                })
                .or_else(|| {
                    self.get_or_rebuild_ship_fitting(char_id, ship_id)
                        .ok()
                        .and_then(|f| f.modules.get(&module_id).and_then(|fm| fm.charge_type_id))
                });
            let sfx_args = vec![
                PyRep::Integer(ship_id), PyRep::Integer(module_id), PyRep::Integer(module_type_id as i64),
                target_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                charge_type_id.map(|tid| PyRep::Integer(i64::from(tid))).unwrap_or(PyRep::None),
                PyRep::String(sfx_guid), PyRep::Bool(is_offensive), PyRep::Integer(1), PyRep::Integer(1),
                PyRep::Real(duration_ms), PyRep::Integer(i64::from(repeat)), PyRep::Integer(now),
                PyRep::Real(0.0), PyRep::None,
            ];
            let ship_sys_opt = self.db.with_snapshot(|snap| {
                snap.items
                    .get(&ship_id)
                    .and_then(|it| {
                        u32::try_from(it.location_id)
                            .ok()
                            .filter(|&loc| (30_000_000..40_000_000).contains(&loc))
                    })
                    .or_else(|| {
                        snap.characters
                            .get(&char_id)
                            .filter(|c| {
                                c.station_id.is_none()
                                    && c.structure_id.is_none_or(|st| c.active_ship_id == st)
                            })
                            .map(|c| c.solar_system_id)
                    })
            });
            let sfx_scope = Some(crate::outbound_sequencer::OutboundSceneScope::space(
                ship_sys_opt,
                None,
            ));
            self.notify_in_scene(
                NotificationTarget::Character(char_id),
                "OnSpecialFX",
                "charid",
                sfx_args.clone(),
                sfx_scope,
            );
            for (obs_cid, _) in self.space_observer_chars_for_ship(ship_id, char_id) {
                self.notify_in_scene(
                    NotificationTarget::Character(obs_cid),
                    "OnSpecialFX",
                    "charid",
                    sfx_args.clone(),
                    sfx_scope,
                );
            }
        }
    }

    pub fn notify_module_online_state(
        &self,
        char_id: i64,
        module_id: i64,
        online: bool,
    ) -> Vec<PendingMachoNotification> {
        let (new_v, old_v) = if online { (1.0, 0.0) } else { (0.0, 1.0) };
        let (ship_id_opt, char_sys_opt) = self.db.with_snapshot(|s| {
            let ship_id = s
                .items
                .get(&module_id)
                .map(|m| m.location_id)
                .filter(|&id| id > 0)
                .or_else(|| s.characters.get(&char_id).map(|c| c.active_ship_id).filter(|&id| id > 0));
            let char_sys = s
                .characters
                .get(&char_id)
                .filter(|c| c.station_id.is_none() && c.structure_id.is_none())
                .map(|c| c.solar_system_id);
            (ship_id, char_sys)
        });
        let sys_opt = ship_id_opt
            .and_then(|ship_id| self.systems.find_ball_system_sync(ship_id))
            .or(char_sys_opt);
        let now = self.now_system_sim_filetime_ticks(sys_opt);
        let entry = PyRep::Tuple(vec![
            PyRep::String("OnModuleAttributeChange".into()), PyRep::Integer(char_id), PyRep::Integer(module_id),
            PyRep::Integer(2), PyRep::Integer(now), PyRep::Real(new_v), PyRep::Real(old_v), PyRep::None,
        ]);
        self.notify(NotificationTarget::Character(char_id), "OnModuleAttributeChanges", "charid", vec![PyRep::List(vec![entry])])
    }

    /// Emits `OnModuleAttributeChanges` for ship `cpuLoad` (49) and `powerLoad` (15) changes.
    pub fn notify_ship_fitting_load_changes(
        &self,
        char_id: i64,
        ship_id: i64,
        old_cpu: f64,
        new_cpu: f64,
        old_pg: f64,
        new_pg: f64,
    ) -> Vec<PendingMachoNotification> {
        let now = self.now_system_sim_filetime_ticks_for_ship(char_id, ship_id);
        let changes = vec![
            PyRep::Tuple(vec![
                PyRep::String("OnModuleAttributeChange".into()), PyRep::Integer(char_id), PyRep::Integer(ship_id),
                PyRep::Integer(i64::from(eve_sde::attr_ids::CPU_LOAD)), PyRep::Integer(now),
                PyRep::Real(new_cpu), PyRep::Real(old_cpu), PyRep::None,
            ]),
            PyRep::Tuple(vec![
                PyRep::String("OnModuleAttributeChange".into()), PyRep::Integer(char_id), PyRep::Integer(ship_id),
                PyRep::Integer(i64::from(eve_sde::attr_ids::POWER_LOAD)), PyRep::Integer(now),
                PyRep::Real(new_pg), PyRep::Real(old_pg), PyRep::None,
            ]),
        ];
        self.notify(NotificationTarget::Character(char_id), "OnModuleAttributeChanges", "charid", vec![PyRep::List(changes)])
    }

    /// Builds a V24 `OnModuleAttributeChange` entry for `CHARGE` (attr 18 — current capacitor GJ)
    /// matching `godma.StateManager.OnModuleAttributeChanges` (`#1002`).
    pub(crate) fn build_ship_capacitor_attr_change_rep(
        char_id: i64,
        ship_id: i64,
        now_filetime: i64,
        new_cap: f64,
        old_cap: f64,
    ) -> PyRep {
        PyRep::Tuple(vec![
            PyRep::String("OnModuleAttributeChange".into()), PyRep::Integer(char_id), PyRep::Integer(ship_id),
            PyRep::Integer(18), PyRep::Integer(now_filetime), PyRep::Real(new_cap), PyRep::Real(old_cap), PyRep::None,
        ])
    }

    /// Synchronizes both `OnCapacitorChange` (test/IT observers) and `OnModuleAttributeChanges`
    /// (`CHARGE` attr 18, which drives V24 `godma` -> `CapacitorContainer` HUD updates) (`#1002`).
    pub fn notify_ship_capacitor_change(
        &self,
        char_id: i64,
        ship_id: i64,
        new_cap: f64,
        max_cap: f64,
        old_cap: Option<f64>,
    ) -> Vec<PendingMachoNotification> {
        if char_id <= 0 || ship_id <= 0 || !new_cap.is_finite() || !max_cap.is_finite() {
            return Vec::new();
        }
        let now = self.now_system_sim_filetime_ticks_for_ship(char_id, ship_id);
        let prev = old_cap.filter(|v| v.is_finite()).unwrap_or(new_cap);
        let mut out = self.notify(
            NotificationTarget::Character(char_id),
            "OnCapacitorChange",
            "charid",
            vec![
                PyRep::Integer(ship_id),
                PyRep::Real(new_cap),
                PyRep::Real(max_cap),
            ],
        );
        out.extend(self.notify(
            NotificationTarget::Character(char_id),
            "OnModuleAttributeChanges",
            "charid",
            vec![PyRep::List(vec![Self::build_ship_capacitor_attr_change_rep(
                char_id, ship_id, now, new_cap, prev,
            )])],
        ));
        out
    }

    /// Emits a canonical `OnItemChange(row, changes)` notification using `DBRowDescriptor::inv_item_header`.
    pub fn notify_item_record_change(&self, char_id: i64, item: &eve_db::InventoryItemRecord, changes: Vec<(i32, PyRep)>) -> Vec<PendingMachoNotification> {
        if self.try_consume_projected_item_mutation_with_changes(char_id, item, &changes, "notify_item_record_change") {
            return Vec::new();
        }
        let (row, change_dict) = self.build_item_change_row_and_dict(item, changes);
        self.notify_with_source(NotificationTarget::Character(char_id), "OnItemChange", "charid", vec![row, change_dict], "notify_item_record_change")
    }

    fn build_item_change_row_and_dict(&self, item: &eve_db::InventoryItemRecord, changes: Vec<(i32, PyRep)>) -> (PyRep, PyRep) {
        let cells = crate::services::inventory_bound::item_to_packed_cells(self, item);
        (eve_proto::DBRowDescriptor::inv_item_header().build_dbrow(cells), super::delta_item_switch::build_item_change_dict(changes))
    }

    /// Emits a canonical `OnItemChange(row, changes)` notification to a specific [`NotificationTarget`].
    pub fn notify_item_record_change_with_target(&self, target: NotificationTarget, id_type: &str, item: &eve_db::InventoryItemRecord, changes: Vec<(i32, PyRep)>) -> Vec<PendingMachoNotification> {
        if self.should_skip_projected_item_change_for_target(&target, item, &changes, "notify_item_record_change_with_target") {
            return Vec::new();
        }
        let (row, change_dict) = self.build_item_change_row_and_dict(item, changes);
        self.notify_with_source(target, "OnItemChange", id_type, vec![row, change_dict], "notify_item_record_change_with_target")
    }

    /// Emits `OnItemChange(row, changes)` (unless already emitted by commit-time projection),
    /// refreshes subscribed `industryMonitor` scopes (`OnIndustryMaterials`), and then emits
    /// `OnMultipleItemChange([row], changes)` to `targets`.
    ///
    /// Emitting `OnIndustryMaterials` before `OnMultipleItemChange` guarantees V24 `IndustryService`
    /// has already updated `job.available` before `MaterialGroups.OnMultipleItemChange` runs
    /// `OnRunsChanged()` / `UpdateState()`. Both item events share one packed row without
    /// duplicating `invCache.OnItemChange`.
    fn emit_item_change_with_industry_refresh(
        &self,
        targets: &[(NotificationTarget, &'static str)],
        item: &eve_db::InventoryItemRecord,
        changes: Vec<(i32, PyRep)>,
        scopes: &[(i64, i64, i64, u32)],
        helper: &'static str,
        skip_item_change: bool,
    ) {
        let (row, change_dict) = self.build_item_change_row_and_dict(item, changes);
        if !skip_item_change {
            for (target, id_type) in targets {
                self.notify_with_source(
                    target.clone(),
                    "OnItemChange",
                    id_type,
                    vec![row.clone(), change_dict.clone()],
                    helper,
                );
            }
        }
        crate::services::notify_emitters_character_services::emit_industry_materials_for_scopes(
            self, scopes,
        );
        let _paired_multi_skip = super::delta_item_compare::push_paired_multi_item_skip_scope(self);
        for (target, id_type) in targets {
            self.notify(
                target.clone(),
                "OnMultipleItemChange",
                id_type,
                vec![PyRep::List(vec![row.clone()]), change_dict.clone()],
            );
        }
    }

    /// Computes canonical V24 `OnItemChange` delta entries (`old_value` keyed by column index).
    /// Never emits `ixQuantity` (key 5); stack size deltas use `ixStackSize` (key 9).
    pub fn compute_item_change_dict(
        old: &eve_db::InventoryItemRecord,
        new: &eve_db::InventoryItemRecord,
    ) -> Vec<(i32, PyRep)> {
        let mut changes = Vec::new();
        if old.owner_id != new.owner_id {
            changes.push((2, PyRep::Integer(old.owner_id)));
        }
        if old.location_id != new.location_id {
            changes.push((3, PyRep::Integer(old.location_id)));
        }
        if old.flag != new.flag {
            changes.push((4, PyRep::Integer(old.flag as i64)));
        }
        if old.quantity != new.quantity {
            changes.push((9, PyRep::Integer(old.quantity as i64)));
        }
        if old.singleton != new.singleton {
            changes.push((10, PyRep::Integer(if old.singleton { 1 } else { 0 })));
        }
        changes
    }

    /// Builds a `locJunkyard` (location 6) tombstone record + delta dict for deleted/consumed items.
    pub fn build_item_tombstone(
        old: &eve_db::InventoryItemRecord,
    ) -> (eve_db::InventoryItemRecord, Vec<(i32, PyRep)>) {
        crate::router::delta_projection::build_item_tombstone(old)
    }

    /// Resolves whether an item belongs to a corporation hangar/deliveries scope or personal scope.
    pub fn resolve_item_notification_target(
        &self,
        fallback_char_id: i64,
        item: &eve_db::InventoryItemRecord,
    ) -> (NotificationTarget, &'static str) {
        let (is_corp, is_other_char) = self.db.with_snapshot(|s| {
            let corp =
                is_corp_hangar_flag(item.flag) && s.corporations.contains_key(&item.owner_id);
            let other_char = item.owner_id > 0
                && item.owner_id != fallback_char_id
                && s.characters.contains_key(&item.owner_id);
            (corp, other_char)
        });
        if is_corp {
            (NotificationTarget::Corporation(item.owner_id), "corpid")
        } else if is_other_char {
            (NotificationTarget::Character(item.owner_id), "charid")
        } else {
            (NotificationTarget::Character(fallback_char_id), "charid")
        }
    }

    /// Emits `OnItemChange`, refreshes matching `industryMonitor` monitors, and emits
    /// `OnMultipleItemChange` to an explicit `(target, id_type)` (used for corp `Deliveries`
    /// market refunds where `item.flag == Deliveries` is outside `CorpSAG*` hangar flags).
    pub fn emit_item_change_with_target(&self, target: NotificationTarget, id_type: &'static str, item: &eve_db::InventoryItemRecord, changes: Vec<(i32, PyRep)>) {
        let scopes = super::delta_item_switch::collect_item_change_scopes(item, &changes);
        let skip = self.should_skip_projected_item_change_for_target(&target, item, &changes, "emit_item_change_with_target");
        self.emit_item_change_with_industry_refresh(&[(target, id_type)], item, changes, &scopes, "emit_item_change_with_target", skip);
    }

    /// Emits routed `OnItemChange` and `OnMultipleItemChange` notifications for an updated or
    /// created item (to `Corporation(owner_id)` for corp hangar flags `116..=122 | 62 | 187` or
    /// `Character(fallback_char_id)` otherwise), and refreshes matching `industryMonitor` monitors.
    pub fn emit_item_change_routed(&self, fallback_char_id: i64, item: &eve_db::InventoryItemRecord, changes: Vec<(i32, PyRep)>) {
        let scopes = super::delta_item_switch::collect_item_change_scopes(item, &changes);
        let target_pair = self.resolve_item_notification_target(fallback_char_id, item);
        let skip = self.should_skip_projected_item_change_for_target(&target_pair.0, item, &changes, "emit_item_change_routed");
        self.emit_item_change_with_industry_refresh(&[target_pair], item, changes, &scopes, "emit_item_change_routed", skip);
    }

    /// Emits a routed `locJunkyard` tombstone `OnItemChange` + `OnMultipleItemChange` for a deleted
    /// or fully consumed item, and refreshes any matching `industryMonitor` material subscriptions.
    pub fn emit_item_deleted_routed(&self, fallback_char_id: i64, old_item: &eve_db::InventoryItemRecord) {
        let (tomb, changes) = Self::build_item_tombstone(old_item);
        let target_pair = self.resolve_item_notification_target(fallback_char_id, old_item);
        let scopes = [(old_item.owner_id, old_item.location_id, old_item.flag as i64, old_item.type_id)];
        let skip = self.should_skip_projected_item_deleted_for_target(&target_pair.0, old_item, &changes, "emit_item_deleted_routed");
        self.emit_item_change_with_industry_refresh(&[target_pair], &tomb, changes, &scopes, "emit_item_deleted_routed", skip);
    }

    /// Emits `OnItemChange`, `OnIndustryMaterials`, and `OnMultipleItemChange` by diffing `pre` and
    /// `post` item states, routing to both old and new targets if ownership/corp-hangar scope changed,
    /// and emits `OnBlueprintsUpdated(ownerID)` for blueprint mutations (including corp-owned
    /// blueprints in non-hangar flags and in-place ME/TE/runs updates).
    pub fn notify_item_mutation(
        &self,
        fallback_char_id: i64,
        pre: Option<&eve_db::InventoryItemRecord>,
        post: Option<&eve_db::InventoryItemRecord>,
    ) {
        let is_bp = |it: &eve_db::InventoryItemRecord| {
            self.sde.blueprints.contains_key(&it.type_id)
                || self.sde.types.get(&it.type_id).is_some_and(|t| t.category_id == 9)
        };
        let emit_bp_update = |it: &eve_db::InventoryItemRecord| {
            let owner_id = if it.owner_id > 0 {
                it.owner_id
            } else {
                fallback_char_id
            };
            let is_corp = self.db.with_snapshot(|s| {
                s.corporations.contains_key(&owner_id)
                    || (!s.characters.contains_key(&owner_id)
                        && (1_000_000..=2_000_000).contains(&owner_id))
            });
            let target = if is_corp {
                NotificationTarget::Corporation(owner_id)
            } else {
                NotificationTarget::Character(owner_id)
            };
            self.notify_with_source(
                target,
                "OnBlueprintsUpdated",
                "ownerID",
                vec![PyRep::Integer(owner_id)],
                "notify_item_mutation",
            );
        };
        match (pre, post) {
            (None, Some(new_item)) => {
                // Signal prior locationID = 0 (`ixLocationID` = 3) so V24 `invCache` / open `InvCont`
                // windows insert the newly created row immediately instead of only updating capacity (#1026).
                self.emit_item_change_routed(fallback_char_id, new_item, vec![(3, PyRep::Integer(0))]);
                if is_bp(new_item) { emit_bp_update(new_item); }
            }
            (Some(old_item), None) => {
                self.emit_item_deleted_routed(fallback_char_id, old_item);
                if is_bp(old_item) { emit_bp_update(old_item); }
            }
            (Some(old_item), Some(new_item)) => {
                let changes = Self::compute_item_change_dict(old_item, new_item);
                let bp_involved = is_bp(new_item) || is_bp(old_item);
                let bp_in_place_changed = bp_involved
                    && (old_item.type_id != new_item.type_id
                        || old_item.is_blueprint_copy != new_item.is_blueprint_copy
                        || old_item.blueprint_runs_remaining != new_item.blueprint_runs_remaining
                        || old_item.dynamic_attributes != new_item.dynamic_attributes);
                if changes.is_empty() && !bp_in_place_changed {
                    return;
                }
                if !changes.is_empty() {
                    let (old_target, old_id_type) = self.resolve_item_notification_target(fallback_char_id, old_item);
                    let (new_target, new_id_type) = self.resolve_item_notification_target(fallback_char_id, new_item);
                    let mut targets = Vec::with_capacity(2);
                    targets.push((new_target.clone(), new_id_type));
                    if old_target != new_target { targets.push((old_target, old_id_type)); }
                    let skip = self.should_skip_projected_item_update(&targets, old_item, new_item, &changes, "notify_item_mutation");
                    self.emit_item_change_with_industry_refresh(
                        &targets,
                        new_item,
                        changes,
                        &[
                            (old_item.owner_id, old_item.location_id, old_item.flag as i64, old_item.type_id),
                            (new_item.owner_id, new_item.location_id, new_item.flag as i64, new_item.type_id),
                        ],
                        "notify_item_mutation",
                        skip,
                    );
                }
                if bp_involved {
                    emit_bp_update(new_item);
                    if old_item.owner_id != new_item.owner_id { emit_bp_update(old_item); }
                }
            }
            (None, None) => {}
        }
    }

    pub fn notify_target_events(
        &self,
        char_id: i64,
        events: &[(&str, i64, &str)],
    ) -> Vec<PendingMachoNotification> {
        let list = events
            .iter()
            .map(|(what, tid, reason)| {
                PyRep::Tuple(vec![
                    PyRep::Tuple(vec![
                        PyRep::String("OnTarget".into()),
                        PyRep::String((*what).into()),
                        PyRep::Integer(*tid),
                        PyRep::String((*reason).into()),
                    ]),
                    PyRep::Integer(0),
                ])
            })
            .collect();
        let sys_opt = self.db.with_snapshot(|snap| {
            snap.characters
                .get(&char_id)
                .filter(|c| {
                    c.station_id.is_none()
                        && c.structure_id.is_none_or(|st| c.active_ship_id == st)
                })
                .map(|c| c.solar_system_id)
        });
        let scope = Some(crate::outbound_sequencer::OutboundSceneScope::space(
            sys_opt, None,
        ));
        self.notify_in_scene(
            NotificationTarget::Character(char_id),
            "OnMultiEvent",
            "charid",
            vec![PyRep::List(list)],
            scope,
        )
    }

    pub fn notify_system_criminal_flag_updates(
        &self,
        sys_id: u32,
        new_idles: &[i64],
        new_suspects: &[i64],
        new_criminals: &[i64],
    ) -> Vec<PendingMachoNotification> {
        let ids = |v: &[i64]| PyRep::List(v.iter().map(|c| PyRep::Integer(*c)).collect());
        self.notify(
            NotificationTarget::SolarSystem(sys_id),
            "OnSystemCriminalFlagUpdates",
            "solarsystemid",
            vec![ids(new_idles), ids(new_suspects), ids(new_criminals)],
        )
    }

    pub fn notify_fitted_item_trashed(
        &self, char_id: i64, item_id: i64, type_id: i64, quantity: i64, old_location_id: i64, old_flag: i64,
    ) -> Vec<PendingMachoNotification> {
        let args = super::delta_item_switch::build_fitted_item_trashed_args(
            &self.sde, char_id, item_id, type_id, quantity, old_location_id, old_flag,
        );
        self.notify_with_source(
            NotificationTarget::Character(char_id),
            "OnItemChange",
            "charid",
            args,
            "notify_fitted_item_trashed",
        )
    }

    /// Registers a post-commit listener on `self.db` so `TxNotificationEffect` entries pushed to
    /// `WorldTx::effects` and committed wallet/PLEX deltas dispatch right after SQLite `COMMIT`
    /// + snapshot publication, and vanish on any abort/panic/SQLite error (`RFC-2 §3.2`, `#915`, `#931`).
    pub fn install_tx_post_commit_listener(&self) {
        super::delta_commit_listener::install_tx_post_commit_listener(self);
    }
}

#[cfg(test)] #[path = "cache_notifications_unit_tests.rs"] mod cache_notifications_unit_tests;
#[cfg(test)] #[path = "cache_notifications_edges_tests.rs"] mod cache_notifications_edges_tests;
