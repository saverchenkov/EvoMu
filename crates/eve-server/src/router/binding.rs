//! Stateful object binding and moniker handles (`MachoBindObject`).

use crate::router::context::{BoundObjectEntry, ServerContext, NEXT_BOUND_ID};
use crate::router::notify_registry::{
    emit_typed_target_to_char_capped, OnChargeBeingLoadedToModule, OnGodmaShipEffect,
    OnMachoObjectDisconnect, OnMultiEvent, OnNewSkillQueueSaved, OnSkillQueuePausedServer,
    OnSpecialFX, TypedNotification,
};
use crate::router::{NotificationTarget, PendingMachoNotification};
use eve_db::lock_rank::{RankedRwLock, RANK_LEAF};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, RwLock, Weak};

/// Unified cluster-wide transport session ID allocator shared by `machoNet` TCP,
/// Python Client Bridge TCP, and Quasar HTTP/2 gRPC listeners (`#852`).
pub static NEXT_TRANSPORT_SESSION_ID: AtomicU64 = AtomicU64::new(10_000);

/// Allocates a globally unique transport `session_id` across all client listeners (`#852`).
pub fn alloc_transport_session_id() -> u64 {
    NEXT_TRANSPORT_SESSION_ID.fetch_add(1, Ordering::Relaxed)
}

const ERR_BIND_AUTH_DENIED: &str = "BindAuthorizationDenied";
pub(crate) const ERR_BOUND_OWNERSHIP: &str = "BoundObjectOwnershipViolation";

/// Maximum concurrent bound objects retained per client session (`#882` / `C20`).
pub const MAX_BOUND_OBJECTS_PER_SESSION: usize = 256;

/// Separate per-`(session_id, service)` quota for notification-pushed sub-object bindings
/// (e.g., incoming `trademgr` trade sessions) so cross-session notifications never evict
/// unrelated bound services (`invbroker`, `dogmaIM`, `station`, etc.).
pub const MAX_NOTIFICATION_SUB_OBJECTS_PER_TARGET_SERVICE: usize = 16;

/// Maximum unattached (`session_id == 0`) bound objects retained across synthetic test sessions.
const MAX_OFFLINE_BOUND_OBJECTS_TOTAL: usize = 256;

static NOTIFICATION_BOUND_IDS: OnceLock<
    RwLock<
        HashMap<
            u64,
            (
                Weak<RankedRwLock<HashMap<u64, BoundObjectEntry>, RANK_LEAF>>,
                HashSet<u64>,
            ),
        >,
    >,
> = OnceLock::new();

fn notification_bound_ids(
) -> &'static RwLock<
    HashMap<
        u64,
        (
            Weak<RankedRwLock<HashMap<u64, BoundObjectEntry>, RANK_LEAF>>,
            HashSet<u64>,
        ),
    >,
> {
    NOTIFICATION_BOUND_IDS.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Retention window (`30s`, matching V24 `basesession.pyj::ObjectConnection.__del__ ->
/// DisconnectObject(30)`) for in-flight read-only metadata calls (`invbroker`, owned
/// `ship.GetDirtTimestamp`, and `crimewatch.GetClientStates`) arriving during/right after a
/// session transition (`#1146`, `#1150`, `#1209`, `#1211`).
const TRANSITION_DISCONNECT_GRACE_TICKS: i64 = 30 * 10_000_000;

/// Maximum transition-disconnected grace entries retained per session.
const MAX_TRANSITION_GRACE_PER_SESSION: usize = 256;

#[derive(Clone)]
struct TransitionDisconnectedBinding {
    entry: BoundObjectEntry,
    disconnected_ticks: i64,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum DepartedLocationKind {
    Dock,
    SolarSystem,
}

#[derive(Clone)]
struct TransitionDepartedLocation {
    session_id: u64,
    user_id: u64,
    character_id: i64,
    kind: DepartedLocationKind,
    location_id: i64,
    departed_ticks: i64,
}

struct ContextGraceStore {
    owner: Weak<RankedRwLock<HashMap<u64, BoundObjectEntry>, RANK_LEAF>>,
    entries: HashMap<u64, TransitionDisconnectedBinding>,
    departed_locations: HashMap<(u64, DepartedLocationKind, i64), TransitionDepartedLocation>,
}

static TRANSITION_DISCONNECT_GRACE: OnceLock<RwLock<HashMap<u64, ContextGraceStore>>> =
    OnceLock::new();

fn transition_disconnect_grace() -> &'static RwLock<HashMap<u64, ContextGraceStore>> {
    TRANSITION_DISCONNECT_GRACE.get_or_init(|| RwLock::new(HashMap::new()))
}

/// RAII guard for TCP client connection tasks (`machoNet` and Python Client Bridge) ensuring
/// synchronous session state (`session_closers`, `bound_objects`, and `PublicGatewayRouter`
/// notice subscribers/receivers), online lock release, and in-space logoff handling are always
/// cleaned up, even if the connection future returns early, panics, or is aborted/dropped mid-await.
pub struct SessionCleanupGuard {
    ctx: Arc<ServerContext>,
    session_id: u64,
    reader_abort: Option<tokio::task::AbortHandle>,
}

impl SessionCleanupGuard {
    pub fn new(ctx: Arc<ServerContext>, session_id: u64) -> Self {
        Self {
            ctx,
            session_id,
            reader_abort: None,
        }
    }

    pub fn set_reader_abort(&mut self, abort_handle: tokio::task::AbortHandle) {
        self.reader_abort = Some(abort_handle);
    }
}

impl Drop for SessionCleanupGuard {
    fn drop(&mut self) {
        if let Some(ref handle) = self.reader_abort {
            handle.abort();
        }
        self.ctx.unregister_session_closer(self.session_id);
        self.ctx.release_session_bindings(self.session_id);
        self.ctx.release_session_rate_bucket(self.session_id);
        self.ctx.quasar_gateway.unsubscribe_session(self.session_id);
        let active_for_session = self.ctx.db.with_snapshot(|snap| {
            snap.sessions
                .values()
                .find(|os| os.session_id == self.session_id)
                .map(|os| (os.user_id, os.character_id))
        });
        if let Some((uid, char_opt)) = active_for_session {
            if self.ctx.db.release_online_lock(uid, self.session_id) {
                if let Some(cid) = char_opt {
                    self.ctx.logoff_character_sync(cid, None);
                }
            }
        }
    }
}

impl ServerContext {
    /// Registers a disconnect notification channel for an active TCP client session (`session_id`).
    pub fn register_session_closer(
        &self,
        session_id: u64,
        tx: tokio::sync::mpsc::UnboundedSender<(String, String)>,
    ) {
        self.session_closers
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session_id, tx);
    }

    /// Removes the disconnect notification channel for `session_id`.
    pub fn unregister_session_closer(&self, session_id: u64) {
        self.session_closers
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&session_id);
    }

    /// Actively signals an open TCP session (`session_id`) to send `GPSTransportClosed` and close its socket,
    /// releasing bound objects and unsubscribing any Quasar gateway notice stream/receiver.
    /// When no live TCP closer is registered (or sending fails), releases any active online lock
    /// for `session_id` and runs `logoff_character_sync` synchronously.
    pub fn disconnect_session(&self, session_id: u64, reason_code: &str, reason: &str) -> bool {
        self.release_session_bindings(session_id);
        self.quasar_gateway.unsubscribe_session(session_id);
        let tx = self
            .session_closers
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&session_id);
        let sent_to_live = tx
            .map(|sender| {
                sender
                    .send((reason_code.to_string(), reason.to_string()))
                    .is_ok()
            })
            .unwrap_or(false);
        if !sent_to_live {
            let active_for_session = self.db.with_snapshot(|snap| {
                snap.sessions
                    .values()
                    .find(|os| os.session_id == session_id)
                    .map(|os| (os.user_id, os.character_id))
            });
            if let Some((uid, char_opt)) = active_for_session {
                if self.db.release_online_lock(uid, session_id) {
                    if let Some(cid) = char_opt {
                        self.logoff_character_sync(cid, None);
                    }
                }
            }
        }
        sent_to_live
    }

    /// Releases all bound objects associated with `session_id` (called on disconnect or re-login).
    pub fn release_session_bindings(&self, session_id: u64) {
        {
            let mut map = self
                .bound_objects
                .write()
                .unwrap_or_else(|e| e.into_inner());
            map.retain(|_, v| v.session_id != session_id);
            self.prune_notification_bound_ids_locked(&map);
        }
        self.purge_transition_grace_for_session(session_id);
    }

    /// Releases all bound objects and Quasar notice subscriptions associated with `user_id`
    /// (called on ban, suspension, kick, or session displacement).
    pub fn release_user_bindings(&self, user_id: u64) {
        {
            let mut map = self
                .bound_objects
                .write()
                .unwrap_or_else(|e| e.into_inner());
            map.retain(|_, v| v.user_id != user_id);
            self.prune_notification_bound_ids_locked(&map);
        }
        self.purge_transition_grace_for_user(user_id);
        self.quasar_gateway.unsubscribe_user(user_id);
    }

    /// Unified character logoff/disconnect teardown shared across socket disconnects,
    /// `/kick`, `/ban`, `/suspend`, scheduled daily downtime, `authentication.Logout`,
    /// `userSvc.UserLogOffCharacter`, and task panic/abort cleanup (`#851` / `B13`, `#881` / `C19`).
    pub fn logoff_character_sync(
        &self,
        char_id: i64,
        mut session_opt: Option<&mut SessionState>,
    ) {
        crate::services::wallet_services_ext::cancel_open_trades_for_character(self, char_id);
        crate::services::structure_control_ops::release_all_structure_control_for_character(
            self,
            session_opt.as_deref_mut(),
            char_id,
        );
        if let Some(s) = session_opt.as_ref() {
            if let Some(struct_id) = s.structure_id {
                self.db.with_mut_snapshot(|snap| {
                    if let Some(ch) = snap.characters.get_mut(&char_id) {
                        ch.structure_id = Some(struct_id);
                        ch.station_id = None;
                        if let Some(sys) = s.solar_system_id {
                            ch.solar_system_id = sys;
                        }
                    }
                });
            }
        }
        let _ = crate::sp6_lifecycle::handle_character_logoff_or_disconnect_sync(self, char_id);
        crate::services::character_skills::notify_watchlist_presence(self, char_id, false);
        crate::services::fleet_social::fleet_social_notify::on_character_logout(self, char_id);
        crate::services::fleet_social::fleet_social_notify::handle_character_disconnect(
            self, char_id,
        );
        crate::services::fleet_social::revoke_xmpp_auth_token(self, char_id);
        if let Ok(mut cn) = self.systems.cluster_nodes.write() {
            cn.char_rebased_system_gen.remove(&char_id);
        }
        self.clear_notifications_for_char(char_id);
    }

    /// Unified disconnect cleanup shared by both native `machoNet` TCP (`handle_macho_client`)
    /// and Python Client Bridge TCP (`handle_bridge_client`) listeners.
    ///
    /// Unregisters the session closer, releases all bound objects, unsubscribes any
    /// `PublicGatewayRouter` notice subscriber and stream receiver for `session.session_id`,
    /// and—if this session still held the account's active online lock (or was revoked via
    /// `/kick`, `/ban`, or downtime without another session taking over)—releases structure
    /// control, persists in-space ship ball coordinates, runs in-space logoff/disconnect
    /// lifecycle, purges pending notifications, and notifies watchlist and fleet/social presence listeners.
    pub async fn cleanup_disconnected_client_session(&self, session: &mut SessionState) {
        let session_id = session.session_id;
        self.unregister_session_closer(session_id);
        if let Some(uid) = session.user_id {
            {
                let mut map = self
                    .bound_objects
                    .write()
                    .unwrap_or_else(|e| e.into_inner());
                map.retain(|_, v| {
                    !(v.session_id == session_id && (v.user_id == 0 || v.user_id == uid))
                });
                self.prune_notification_bound_ids_locked(&map);
            }
            self.purge_transition_grace_for_session(session_id);
        } else {
            self.release_session_bindings(session_id);
        }
        self.quasar_gateway.unsubscribe_session(session_id);
        if let Some(uid) = session.user_id {
            let released = self.db.release_online_lock(uid, session_id);
            if let Some(cid) = session.character_id {
                let needs_logoff = released
                    || (!self.is_character_online(cid, uid)
                        && !crate::sp6_lifecycle::has_active_disconnected_ship(&self.db, cid));
                if needs_logoff {
                    self.logoff_character_sync(cid, Some(session));
                } else {
                    self.clear_notifications_for_char(cid);
                }
            }
        }
    }

    /// Binds a stateful remote object scoped to `(session_id, user_id, character_id)`.
    pub fn bind_object(
        &self,
        session: &SessionState,
        service: &str,
        object_id: i64,
    ) -> Result<u64, BridgeRpcError> {
        let user_id = Self::require_auth(session)?;
        let char_id = match session.character_id {
            Some(cid) => cid,
            None if service == "populationCap" => {
                // `populationCap` is bound on the character-selection screen (`TryEnterGame`)
                // before `SelectCharacterID` sets `session.character_id`; the bound character ID
                // must belong to the authenticated account.
                let owned = self
                    .db
                    .get_characters_for_user(user_id)
                    .iter()
                    .any(|c| c.character_id == object_id);
                if !owned {
                    return Err(BridgeRpcError {
                        code: ERR_BIND_AUTH_DENIED.into(),
                        message: format!(
                            "Service '{service}' cannot be bound to character {object_id} not owned by user {user_id}"
                        ),
                        client_error: None,
                    });
                }
                object_id
            }
            None => {
                return Err(BridgeRpcError {
                    code: "CharacterNotSelected".into(),
                    message: "Cannot bind object before selecting a character".into(),
                    client_error: None,
                });
            }
        };

        let authorized =
            crate::router::binding_auth::authorize_service_binding(self, session, char_id, service, object_id);

        if !authorized {
            return Err(BridgeRpcError {
                code: ERR_BIND_AUTH_DENIED.into(),
                message: format!(
                    "Service '{service}' cannot be bound to object_id {object_id} for current session state"
                ),
                client_error: None,
            });
        }

        let bound_id = NEXT_BOUND_ID.fetch_add(1, Ordering::Relaxed);
        self.insert_bound_entry(session, bound_id, user_id, char_id, service, object_id);
        Ok(bound_id)
    }

    /// Binds a sub-object handed out by an already-bound service method (e.g. the inventory
    /// returned by `invbroker.GetInventoryFromId`). The caller must have verified that the
    /// session may access `object_id`; only character selection is checked here.
    pub fn bind_verified_sub_object(
        &self,
        session: &SessionState,
        service: &str,
        object_id: i64,
    ) -> Result<u64, BridgeRpcError> {
        let (user_id, char_id) = Self::require_character(session)?;
        let bound_id = NEXT_BOUND_ID.fetch_add(1, Ordering::Relaxed);
        self.insert_bound_entry(session, bound_id, user_id, char_id, service, object_id);
        Ok(bound_id)
    }

    fn make_bound_object_entry(
        &self,
        session_id: u64,
        bound_id: u64,
        user_id: u64,
        char_id: i64,
        service: &str,
        object_id: i64,
    ) -> BoundObjectEntry {
        let oid_str = format!("N={}:{bound_id}", eve_machonet::MACHO_PROXY_NODE_ID);
        let ref_id = (self.now_unix() as i64 + 11_644_473_600).saturating_mul(10_000_000);
        BoundObjectEntry {
            bound_id,
            session_id,
            user_id,
            character_id: char_id,
            service: service.to_string(),
            object_id,
            oid_str,
            ref_id,
        }
    }

    fn notification_bound_ids_snapshot(&self) -> HashSet<u64> {
        let guard = notification_bound_ids()
            .read()
            .unwrap_or_else(|e| e.into_inner());
        guard
            .get(&self.instance_id)
            .map(|(_, ids)| ids.clone())
            .unwrap_or_default()
    }

    fn record_notification_bound_id_locked(
        &self,
        live_map: &HashMap<u64, BoundObjectEntry>,
        bound_id: u64,
    ) {
        let owner = Arc::downgrade(&self.bound_objects);
        let mut guard = notification_bound_ids()
            .write()
            .unwrap_or_else(|e| e.into_inner());
        guard.retain(|_, (w, _)| w.strong_count() > 0);
        let (_, ids) = guard
            .entry(self.instance_id)
            .or_insert_with(|| (owner, HashSet::new()));
        ids.retain(|id| live_map.contains_key(id));
        ids.insert(bound_id);
    }

    fn prune_notification_bound_ids_locked(&self, live_map: &HashMap<u64, BoundObjectEntry>) {
        let Some(lock) = NOTIFICATION_BOUND_IDS.get() else {
            return;
        };
        let mut guard = lock.write().unwrap_or_else(|e| e.into_inner());
        guard.retain(|_, (w, _)| w.strong_count() > 0);
        if let Some((_, ids)) = guard.get_mut(&self.instance_id) {
            ids.retain(|id| live_map.contains_key(id));
            if ids.is_empty() {
                guard.remove(&self.instance_id);
            }
        }
    }

    fn insert_bound_entry_record(&self, entry: BoundObjectEntry) {
        let mut map = self
            .bound_objects
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let notif_ids = self.notification_bound_ids_snapshot();
        if entry.session_id != 0 {
            let session_count = map
                .values()
                .filter(|e| e.session_id == entry.session_id && !notif_ids.contains(&e.bound_id))
                .count();
            if session_count >= MAX_BOUND_OBJECTS_PER_SESSION {
                let excess = session_count - MAX_BOUND_OBJECTS_PER_SESSION + 1;
                let mut session_bids: Vec<u64> = map
                    .values()
                    .filter(|e| {
                        e.session_id == entry.session_id && !notif_ids.contains(&e.bound_id)
                    })
                    .map(|e| e.bound_id)
                    .collect();
                session_bids.sort_unstable();
                for old_bid in session_bids.into_iter().take(excess) {
                    map.remove(&old_bid);
                }
            }
        } else {
            let offline_count = map
                .values()
                .filter(|e| e.session_id == 0 && !notif_ids.contains(&e.bound_id))
                .count();
            if offline_count >= MAX_OFFLINE_BOUND_OBJECTS_TOTAL {
                let excess = offline_count - MAX_OFFLINE_BOUND_OBJECTS_TOTAL + 1;
                let mut offline_bids: Vec<u64> = map
                    .values()
                    .filter(|e| e.session_id == 0 && !notif_ids.contains(&e.bound_id))
                    .map(|e| e.bound_id)
                    .collect();
                offline_bids.sort_unstable();
                for old_bid in offline_bids.into_iter().take(excess) {
                    map.remove(&old_bid);
                }
            }
        }
        map.insert(entry.bound_id, entry);
    }

    fn insert_bound_entry(
        &self,
        session: &SessionState,
        bound_id: u64,
        user_id: u64,
        char_id: i64,
        service: &str,
        object_id: i64,
    ) {
        let entry = self.make_bound_object_entry(
            session.session_id,
            bound_id,
            user_id,
            char_id,
            service,
            object_id,
        );
        self.insert_bound_entry_record(entry);
    }

    /// Binds a notification-pushed sub-object for `char_id` on their active online session
    /// (or explicit `fallback_session` in headless unit tests), returning `(bound_id, wire_rep)`.
    /// Enforces a dedicated per-`(session_id, service)` quota and never evicts unrelated bound
    /// services on the recipient's session (`RV-1188-P1-01`).
    pub fn bind_sub_object_for_notification(
        &self,
        char_id: i64,
        fallback_session: Option<&SessionState>,
        service: &str,
        object_id: i64,
    ) -> Result<(u64, PyRep), BridgeRpcError> {
        let fallback_pair = fallback_session
            .filter(|s| s.character_id == Some(char_id))
            .and_then(|s| s.user_id.map(|uid| (s.session_id, uid)));
        let online_pair = fallback_pair.or_else(|| {
            self.db.online_sessions_snapshot().into_iter().find_map(|(uid, sid)| {
                self.db
                    .get_online_session(uid)
                    .filter(|rec| rec.character_id == Some(char_id))
                    .map(|_| (sid, uid))
            })
        });
        let (session_id, user_id) = match online_pair {
            Some(pair) => pair,
            None => {
                if self.db.get_character(char_id).is_err() {
                    return Err(BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("character {char_id} not found for bound notification"),
                        client_error: None,
                    });
                }
                return Err(BridgeRpcError {
                    code: "CharacterOffline".into(),
                    message: format!("character {char_id} is offline for bound notification"),
                    client_error: None,
                });
            }
        };
        let bound_id = NEXT_BOUND_ID.fetch_add(1, Ordering::Relaxed);
        let entry = self.make_bound_object_entry(
            session_id,
            bound_id,
            user_id,
            char_id,
            service,
            object_id,
        );
        let wire_rep = PyRep::SubStruct(Box::new(PyRep::SubStream(Box::new(PyRep::Tuple(vec![
            PyRep::ascii_bytes(entry.oid_str.clone()),
            PyRep::Integer(entry.ref_id),
        ])))));
        {
            let mut map = self
                .bound_objects
                .write()
                .unwrap_or_else(|e| e.into_inner());
            if session_id != 0 {
                let svc_count = map
                    .values()
                    .filter(|e| e.session_id == session_id && e.service == service)
                    .count();
                if svc_count >= MAX_NOTIFICATION_SUB_OBJECTS_PER_TARGET_SERVICE {
                    return Err(BridgeRpcError {
                        code: "TooManyBoundSubObjects".into(),
                        message: format!(
                            "Recipient session already holds the maximum {MAX_NOTIFICATION_SUB_OBJECTS_PER_TARGET_SERVICE} notification sub-objects for '{service}'"
                        ),
                        client_error: None,
                    });
                }
            } else {
                let char_svc_count = map
                    .values()
                    .filter(|e| {
                        e.session_id == 0 && e.character_id == char_id && e.service == service
                    })
                    .count();
                let offline_total = map.values().filter(|e| e.session_id == 0).count();
                if char_svc_count >= MAX_NOTIFICATION_SUB_OBJECTS_PER_TARGET_SERVICE
                    || offline_total >= MAX_OFFLINE_BOUND_OBJECTS_TOTAL
                {
                    return Err(BridgeRpcError {
                        code: "TooManyBoundSubObjects".into(),
                        message: format!(
                            "Synthetic offline notification sub-object limit reached for '{service}'"
                        ),
                        client_error: None,
                    });
                }
            }
            map.insert(entry.bound_id, entry);
            self.record_notification_bound_id_locked(&map, bound_id);
        }
        Ok((bound_id, wire_rep))
    }

    /// Parses a bound object ID from either an `"N=<nodeID>:<boundID>"` string or an integer `bound_id`.
    fn parse_bound_id_rep(rep: &PyRep) -> Option<u64> {
        match rep {
            PyRep::Integer(v) if *v > 0 => Some(*v as u64),
            _ => {
                let s = rep.as_str()?;
                let (_, right) = s.trim().rsplit_once(':')?;
                right.parse::<u64>().ok()
            }
        }
    }

    fn collect_bound_ids_from_pyrep(rep: &PyRep, out: &mut Vec<u64>) {
        if let Some(bid) = Self::parse_bound_id_rep(rep) {
            out.push(bid);
            return;
        }
        match rep {
            PyRep::List(items) | PyRep::Tuple(items) => {
                for item in items {
                    Self::collect_bound_ids_from_pyrep(item, out);
                }
            }
            PyRep::Dict(entries) => {
                for (k, v) in entries {
                    if let Some(bid) = Self::parse_bound_id_rep(k) {
                        out.push(bid);
                    } else {
                        Self::collect_bound_ids_from_pyrep(k, out);
                        Self::collect_bound_ids_from_pyrep(v, out);
                    }
                }
            }
            PyRep::SubStream(inner) | PyRep::SubStruct(inner) => {
                Self::collect_bound_ids_from_pyrep(inner, out);
            }
            _ => {}
        }
    }

    /// Releases bound objects owned by `session_id` referenced in `rep` (`OID-` or `ClientHasReleasedTheseObjects`, `#882` / `C20`).
    pub fn release_bound_objects_from_pyrep(&self, session_id: u64, rep: &PyRep) -> usize {
        let mut target_bids = Vec::new();
        Self::collect_bound_ids_from_pyrep(rep, &mut target_bids);
        if target_bids.is_empty() {
            return 0;
        }
        let removed = {
            let mut map = self
                .bound_objects
                .write()
                .unwrap_or_else(|e| e.into_inner());
            let mut count = 0;
            for &bid in &target_bids {
                if map.get(&bid).is_some_and(|e| e.session_id == session_id) {
                    map.remove(&bid);
                    count += 1;
                }
            }
            if count > 0 {
                self.prune_notification_bound_ids_locked(&map);
            }
            count
        };
        self.purge_transition_grace_with(
            |bid, v| v.entry.session_id == session_id && target_bids.contains(bid),
            |_, _| false,
        );
        removed
    }

    /// Processes client-initiated bound object releases from `pkt.named_payload["OID-"]` and
    /// `ClientHasReleasedTheseObjects` notification/broadcast packets (`#882` / `C20`).
    /// Returns `(released_count, is_dedicated_release_notification)`.
    pub fn process_client_bound_object_releases(
        &self,
        session_id: u64,
        pkt: &eve_machonet::MachoPacket,
    ) -> (usize, bool) {
        let mut released = 0;
        if let Some(oid_minus) = pkt.named_payload.get("OID-") {
            released += self.release_bound_objects_from_pyrep(session_id, oid_minus);
        }
        if pkt.packet_type != eve_machonet::MachoPacketType::Notification {
            return (released, false);
        }
        let is_release_broadcast = matches!(
            &pkt.destination,
            eve_machonet::MachoAddress::Broadcast { broadcast_id, .. }
                if broadcast_id == "ClientHasReleasedTheseObjects"
        );
        if is_release_broadcast {
            released += self.release_bound_objects_from_pyrep(session_id, &pkt.payload);
            return (released, true);
        }
        if let Some(release_args) = Self::extract_release_notification_args(&pkt.payload) {
            for rep in release_args {
                released += self.release_bound_objects_from_pyrep(session_id, rep);
            }
            return (released, true);
        }
        (released, false)
    }

    fn extract_release_notification_args(payload: &PyRep) -> Option<&[PyRep]> {
        let mut cur = payload;
        for _ in 0..4 {
            match cur {
                PyRep::SubStream(inner) | PyRep::SubStruct(inner) => {
                    cur = inner.as_ref();
                }
                PyRep::Tuple(items) if items.len() == 1 => {
                    cur = &items[0];
                }
                PyRep::Tuple(items)
                    if items.len() == 2
                        && matches!(items[0], PyRep::Integer(_))
                        && matches!(items[1], PyRep::SubStream(_) | PyRep::SubStruct(_) | PyRep::Tuple(_)) =>
                {
                    cur = &items[1];
                }
                _ => break,
            }
        }
        let PyRep::Tuple(items) = cur else {
            return None;
        };
        if items.first().and_then(|v| v.as_str()) == Some("ClientHasReleasedTheseObjects") {
            let end = if items.len() > 2 && matches!(items.last(), Some(PyRep::Dict(_))) {
                items.len() - 1
            } else {
                items.len()
            };
            return Some(&items[1..end]);
        }
        if items.len() >= 2 && items[1].as_str() == Some("ClientHasReleasedTheseObjects") {
            let end = if items.len() > 3 && matches!(items.last(), Some(PyRep::Dict(_))) {
                items.len() - 1
            } else {
                items.len()
            };
            return Some(&items[2..end]);
        }
        None
    }

    /// Wire form (`SubStruct(SubStream((b"N={nodeID}:{boundID}", refID)))`) of a bound-object
    /// reference for Macho notifications delivered over `:26000`.
    pub fn bound_object_wire_rep(&self, bound_id: u64) -> PyRep {
        let (oid_str, ref_id) = self
            .bound_objects
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&bound_id)
            .map(|b| (b.oid_str.clone(), b.ref_id))
            .unwrap_or_else(|| {
                (
                    format!("N={}:{bound_id}", eve_machonet::MACHO_PROXY_NODE_ID),
                    (self.now_unix() as i64 + 11_644_473_600).saturating_mul(10_000_000),
                )
            });
        PyRep::SubStruct(Box::new(PyRep::SubStream(Box::new(PyRep::Tuple(vec![
            PyRep::ascii_bytes(oid_str),
            PyRep::Integer(ref_id),
        ])))))
    }

    /// Wire form of a bound-object reference. The native client (`caller_trace == "machoNet:26000"`)
    /// gets `SubStruct(SubStream((b"N={nodeID}:{boundID}", refID)))`, which `ObjectCall.GetObjectID`
    /// (`carbon/common/script/net/ObjectCallGPCS.pyj`) / `ParseObjectID` turn into a remote object;
    /// the JSON bridge gets the plain bound ID.
    pub fn bound_object_rep(&self, bound_id: u64, caller_trace: Option<&str>) -> PyRep {
        if caller_trace == Some("machoNet:26000") {
            self.bound_object_wire_rep(bound_id)
        } else {
            PyRep::Integer(bound_id as i64)
        }
    }

    /// Verifies that `bound_id` exists and belongs to the caller's `(session_id, user_id, character_id)`.
    pub fn verify_bound_object(
        &self,
        session: &SessionState,
        bound_id: u64,
    ) -> Result<BoundObjectEntry, BridgeRpcError> {
        let entry = self
            .bound_objects
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&bound_id)
            .cloned()
            .ok_or_else(|| BridgeRpcError {
                code: "InvalidBoundObject".into(),
                message: format!("Bound object {bound_id} does not exist"),
                client_error: None,
            })?;

        let char_matches = Some(entry.character_id) == session.character_id
            || (entry.service == "populationCap" && session.character_id.is_none());
        if entry.session_id != session.session_id
            || Some(entry.user_id) != session.user_id
            || !char_matches
        {
            return Err(BridgeRpcError {
                code: ERR_BOUND_OWNERSHIP.into(),
                message: format!("Bound object {bound_id} is not owned by this session"),
                client_error: None,
            });
        }

        let context_valid =
            crate::router::binding_auth::verify_service_binding_context(self, session, &entry);
        if !context_valid {
            return Err(BridgeRpcError {
                code: ERR_BOUND_OWNERSHIP.into(),
                message: format!(
                    "Bound object {bound_id} ({}) for object_id {} is no longer valid in current session context",
                    entry.service, entry.object_id
                ),
                client_error: None,
            });
        }
        Ok(entry)
    }

    fn emit_macho_object_disconnect(&self, entry: &BoundObjectEntry) {
        if entry.character_id <= 0 {
            return;
        }
        let macho_client_id =
            1_000_000_000_000_u64.saturating_add(entry.session_id) as i64;
        self.emit_typed(
            NotificationTarget::Character(entry.character_id),
            OnMachoObjectDisconnect {
                object_id: PyRep::ascii_bytes(entry.oid_str.clone()),
                client_id: macho_client_id,
                ref_id: entry.ref_id,
            },
        );
    }

    fn is_grace_ticks_active(recorded_ticks: i64, now_ticks: i64) -> bool {
        now_ticks >= recorded_ticks
            && now_ticks.saturating_sub(recorded_ticks) <= TRANSITION_DISCONNECT_GRACE_TICKS
    }

    fn is_grace_entry_active(binding: &TransitionDisconnectedBinding, now_ticks: i64) -> bool {
        Self::is_grace_ticks_active(binding.disconnected_ticks, now_ticks)
    }

    fn has_matching_online_session(&self, session: &SessionState, uid: u64, cid: i64) -> bool {
        self.db.get_online_session(uid).is_none_or(|online| {
            online.session_id == session.session_id && online.character_id == Some(cid)
        })
    }

    fn is_station_or_structure_id(&self, object_id: i64) -> bool {
        object_id > 0
            && (self.sde.stations.contains_key(&object_id)
                || self
                    .db
                    .with_snapshot(|snap| snap.structures.contains_key(&object_id)))
    }

    fn is_owned_ship_dirt_target(
        &self,
        session: &SessionState,
        char_id: i64,
        args: &[PyRep],
    ) -> bool {
        let Some(target_ship_id) = args
            .first()
            .and_then(|a| a.as_i64())
            .or(session.ship_id)
        else {
            return false;
        };
        if target_ship_id <= 0 {
            return false;
        }
        if session.ship_id == Some(target_ship_id) {
            return true;
        }
        self.db.with_snapshot(|snap| {
            snap.items.get(&target_ship_id).is_some_and(|item| {
                item.owner_id == char_id
                    && self
                        .sde
                        .types
                        .get(&item.type_id)
                        .is_none_or(|t| t.category_id == 6)
            })
        })
    }

    fn excess_session_grace_keys<K: Ord>(mut keyed_ticks: Vec<(i64, K)>) -> Vec<K> {
        if keyed_ticks.len() <= MAX_TRANSITION_GRACE_PER_SESSION {
            return Vec::new();
        }
        keyed_ticks.sort_unstable();
        let excess = keyed_ticks.len() - MAX_TRANSITION_GRACE_PER_SESSION;
        keyed_ticks.into_iter().take(excess).map(|(_, k)| k).collect()
    }

    fn trim_store_session_caps(store: &mut ContextGraceStore, touched_sessions: &[u64]) {
        for &sid in touched_sessions {
            let excess_bids = Self::excess_session_grace_keys(
                store
                    .entries
                    .iter()
                    .filter(|(_, v)| v.entry.session_id == sid)
                    .map(|(&bid, v)| (v.disconnected_ticks, bid))
                    .collect(),
            );
            for bid in excess_bids {
                store.entries.remove(&bid);
            }
            let excess_locations = Self::excess_session_grace_keys(
                store
                    .departed_locations
                    .iter()
                    .filter(|((s, _, _), v)| *s == sid || v.session_id == sid)
                    .map(|(&key, v)| (v.departed_ticks, key))
                    .collect(),
            );
            for key in excess_locations {
                store.departed_locations.remove(&key);
            }
        }
    }

    fn with_grace_store_mut<R>(&self, now_ticks: i64, f: impl FnOnce(&mut ContextGraceStore) -> R) -> R {
        let owner = Arc::downgrade(&self.bound_objects);
        let mut outer = transition_disconnect_grace()
            .write()
            .unwrap_or_else(|e| e.into_inner());
        outer.retain(|_, store| store.owner.strong_count() > 0);
        let store = outer.entry(self.instance_id).or_insert_with(|| ContextGraceStore {
            owner,
            entries: HashMap::new(),
            departed_locations: HashMap::new(),
        });
        store
            .entries
            .retain(|_, v| Self::is_grace_entry_active(v, now_ticks));
        store
            .departed_locations
            .retain(|_, v| Self::is_grace_ticks_active(v.departed_ticks, now_ticks));
        f(store)
    }

    fn departed_location_kind_for_entry(
        &self,
        entry: &BoundObjectEntry,
    ) -> Option<DepartedLocationKind> {
        if entry.object_id <= 0 || entry.user_id == 0 || entry.character_id <= 0 {
            return None;
        }
        if matches!(
            entry.service.as_str(),
            "station"
                | "stationSvc"
                | "corpStationMgr"
                | "repairSvc"
                | "reprocessingSvc"
                | "insuranceSvc"
        ) || self.is_station_or_structure_id(entry.object_id)
        {
            return Some(DepartedLocationKind::Dock);
        }
        let is_sys_service = matches!(
            entry.service.as_str(),
            "beyonce" | "scanMgr" | "entity" | "crimewatch" | "planetOrbitalRegistryBroker"
        ) || (matches!(
            entry.service.as_str(),
            "keeper" | "posMgr" | "devIndexManager"
        ) && (30_000_000..=39_999_999).contains(&entry.object_id));
        if is_sys_service && crate::router::binding_auth::is_solar_system_id(self, entry.object_id) {
            return Some(DepartedLocationKind::SolarSystem);
        }
        None
    }

    fn record_transition_grace_bindings(&self, entries: &[BoundObjectEntry]) {
        let grace_entries: Vec<&BoundObjectEntry> = entries
            .iter()
            .filter(|e| matches!(e.service.as_str(), "invbroker" | "ship" | "crimewatch"))
            .collect();
        let location_departures: Vec<(DepartedLocationKind, &BoundObjectEntry)> = entries
            .iter()
            .filter_map(|e| self.departed_location_kind_for_entry(e).map(|kind| (kind, e)))
            .collect();
        if grace_entries.is_empty() && location_departures.is_empty() {
            return;
        }
        let now_ticks = self.now_machonet_wallclock_filetime_ticks();
        self.with_grace_store_mut(now_ticks, |store| {
            let mut touched_sessions = Vec::new();
            for entry in grace_entries {
                if !touched_sessions.contains(&entry.session_id) {
                    touched_sessions.push(entry.session_id);
                }
                store.entries.insert(
                    entry.bound_id,
                    TransitionDisconnectedBinding {
                        entry: entry.clone(),
                        disconnected_ticks: now_ticks,
                    },
                );
            }
            for (kind, entry) in location_departures {
                if !touched_sessions.contains(&entry.session_id) {
                    touched_sessions.push(entry.session_id);
                }
                store.departed_locations.insert(
                    (entry.session_id, kind, entry.object_id),
                    TransitionDepartedLocation {
                        session_id: entry.session_id,
                        user_id: entry.user_id,
                        character_id: entry.character_id,
                        kind,
                        location_id: entry.object_id,
                        departed_ticks: now_ticks,
                    },
                );
            }
            Self::trim_store_session_caps(store, &touched_sessions);
        });
    }

    fn purge_transition_grace_with<FE, FL>(&self, mut entry_pred: FE, mut loc_pred: FL)
    where
        FE: FnMut(&u64, &TransitionDisconnectedBinding) -> bool,
        FL: FnMut(&(u64, DepartedLocationKind, i64), &TransitionDepartedLocation) -> bool,
    {
        let Some(lock) = TRANSITION_DISCONNECT_GRACE.get() else {
            return;
        };
        let mut outer = lock.write().unwrap_or_else(|e| e.into_inner());
        outer.retain(|_, store| store.owner.strong_count() > 0);
        if let Some(store) = outer.get_mut(&self.instance_id) {
            store.entries.retain(|bid, v| !entry_pred(bid, v));
            store.departed_locations.retain(|k, v| !loc_pred(k, v));
            if store.entries.is_empty() && store.departed_locations.is_empty() {
                outer.remove(&self.instance_id);
            }
        }
    }

    pub(crate) fn purge_transition_grace_for_session(&self, session_id: u64) {
        self.purge_transition_grace_with(
            |_, v| v.entry.session_id == session_id,
            |&(sid, _, _), v| sid == session_id || v.session_id == session_id,
        );
    }

    fn purge_transition_grace_for_user(&self, user_id: u64) {
        self.purge_transition_grace_with(
            |_, v| v.entry.user_id == user_id,
            |_, v| v.user_id == user_id,
        );
    }

    pub(crate) fn purge_transition_grace_entry(&self, session_id: u64, bound_id: u64) {
        self.purge_transition_grace_with(
            |&bid, v| bid == bound_id && v.entry.session_id == session_id,
            |_, _| false,
        );
    }

    #[cfg(test)]
    pub(crate) fn update_transition_departed_dock(
        &self,
        session: &SessionState,
        prev_dock: Option<i64>,
        next_dock: Option<i64>,
    ) {
        self.update_transition_departed_locations(session, None, prev_dock, None, next_dock);
    }

    pub(crate) fn update_transition_departed_locations(
        &self,
        session: &SessionState,
        prev_sys: Option<u32>,
        prev_dock: Option<i64>,
        next_sys: Option<u32>,
        next_dock: Option<i64>,
    ) {
        let mut record = Vec::new();
        let mut purge = Vec::new();
        let prev_sys_id = prev_sys.map(i64::from);
        let next_sys_id = next_sys.map(i64::from);
        if prev_sys_id != next_sys_id {
            if let Some(old_sys) = prev_sys_id.filter(|&id| id > 0) {
                record.push((DepartedLocationKind::SolarSystem, old_sys));
            }
            if let Some(new_sys) = next_sys_id.filter(|&id| id > 0) {
                purge.push((DepartedLocationKind::SolarSystem, new_sys));
            }
        }
        if prev_dock != next_dock {
            if let Some(old_dock) = prev_dock.filter(|&id| id > 0) {
                record.push((DepartedLocationKind::Dock, old_dock));
            }
            if let Some(new_dock) = next_dock.filter(|&id| id > 0) {
                purge.push((DepartedLocationKind::Dock, new_dock));
            }
        }
        self.apply_departed_location_deltas(session, &record, &purge);
    }

    pub(crate) fn sync_transition_grace_on_session_change(
        &self,
        prev: &SessionState,
        next: &SessionState,
    ) {
        if prev.character_id.is_some() && next.character_id != prev.character_id {
            self.purge_transition_grace_for_session(next.session_id);
        } else if prev.character_id == next.character_id
            && next.character_id.is_some()
            && next.session_id != 0
        {
            self.update_transition_departed_locations(
                next,
                prev.solar_system_id,
                prev.station_id.or(prev.structure_id),
                next.solar_system_id,
                next.station_id.or(next.structure_id),
            );
        }
    }

    fn apply_departed_location_deltas(
        &self,
        session: &SessionState,
        record: &[(DepartedLocationKind, i64)],
        purge: &[(DepartedLocationKind, i64)],
    ) {
        if record.is_empty() && purge.is_empty() {
            return;
        }
        if record.is_empty() {
            self.purge_transition_grace_with(
                |_, _| false,
                |&(sid, kind, lid), _| {
                    sid == session.session_id && purge.contains(&(kind, lid))
                },
            );
            return;
        }
        let can_record = session
            .user_id
            .zip(session.character_id)
            .filter(|&(uid, cid)| self.has_matching_online_session(session, uid, cid));
        let Some((uid, cid)) = can_record else {
            if !purge.is_empty() {
                self.purge_transition_grace_with(
                    |_, _| false,
                    |&(sid, kind, lid), _| {
                        sid == session.session_id && purge.contains(&(kind, lid))
                    },
                );
            }
            return;
        };
        let now_ticks = self.now_machonet_wallclock_filetime_ticks();
        self.with_grace_store_mut(now_ticks, |store| {
            for &(kind, lid) in purge {
                store
                    .departed_locations
                    .remove(&(session.session_id, kind, lid));
            }
            for &(kind, lid) in record {
                store.departed_locations.insert(
                    (session.session_id, kind, lid),
                    TransitionDepartedLocation {
                        session_id: session.session_id,
                        user_id: uid,
                        character_id: cid,
                        kind,
                        location_id: lid,
                        departed_ticks: now_ticks,
                    },
                );
            }
            Self::trim_store_session_caps(store, &[session.session_id]);
        });
    }

    fn allocate_transition_grace_binding_for_departed_kinds(
        &self,
        session: &SessionState,
        service: &str,
        object_id: i64,
        allowed_kinds: &[DepartedLocationKind],
    ) -> Option<u64> {
        if object_id <= 0 || allowed_kinds.is_empty() {
            return None;
        }
        let uid = session.user_id?;
        let cid = session.character_id?;
        if !self.has_matching_online_session(session, uid, cid) {
            return None;
        }
        let now_ticks = self.now_machonet_wallclock_filetime_ticks();
        self.with_grace_store_mut(now_ticks, |store| {
            let active_departed = allowed_kinds.iter().any(|&kind| {
                store
                    .departed_locations
                    .get(&(session.session_id, kind, object_id))
                    .is_some_and(|departed| {
                        Self::is_grace_ticks_active(departed.departed_ticks, now_ticks)
                            && departed.session_id == session.session_id
                            && departed.user_id == uid
                            && departed.character_id == cid
                            && departed.kind == kind
                            && departed.location_id == object_id
                    })
            });
            if !active_departed {
                return None;
            }
            let bound_id = NEXT_BOUND_ID.fetch_add(1, Ordering::Relaxed);
            let entry = self.make_bound_object_entry(
                session.session_id,
                bound_id,
                uid,
                cid,
                service,
                object_id,
            );
            store.entries.insert(
                bound_id,
                TransitionDisconnectedBinding {
                    entry,
                    disconnected_ticks: now_ticks,
                },
            );
            Self::trim_store_session_caps(store, &[session.session_id]);
            Some(bound_id)
        })
    }

    /// Allocates a temporary transition-grace binding (kept out of active `bound_objects`) for an
    /// in-flight `ship.MachoBindObject((departed_dock_id, ...), ("GetDirtTimestamp", (ship_id,), {}))`
    /// racing immediately after `Undock` or `/moveme` (`#1150`, `#1152`, `#1153`).
    pub(crate) fn allocate_transition_grace_ship_dirt_binding(
        &self,
        session: &SessionState,
        object_id: i64,
        sub_args: &[PyRep],
    ) -> Option<u64> {
        let cid = session.character_id?;
        if !self.is_owned_ship_dirt_target(session, cid, sub_args) {
            return None;
        }
        self.allocate_transition_grace_binding_for_departed_kinds(
            session,
            "ship",
            object_id,
            &[DepartedLocationKind::Dock],
        )
    }

    /// Allocates a temporary transition-grace binding (kept out of active `bound_objects`) for an
    /// in-flight `crimewatch.MachoBindObject((departed_loc_id, ...), ("GetClientStates", (), {}))`
    /// racing during overlapping session transitions such as back-to-back `/tr` (`#1209`, `#1211`).
    pub(crate) fn allocate_transition_grace_crimewatch_binding(
        &self,
        session: &SessionState,
        object_id: i64,
        sub_args: &[PyRep],
    ) -> Option<u64> {
        if !sub_args.is_empty() {
            return None;
        }
        self.allocate_transition_grace_binding_for_departed_kinds(
            session,
            "crimewatch",
            object_id,
            &[DepartedLocationKind::SolarSystem, DepartedLocationKind::Dock],
        )
    }

    fn lookup_transition_grace_entry(
        &self,
        session: &SessionState,
        bound_id: u64,
        expected_services: Option<&[&str]>,
    ) -> Option<BoundObjectEntry> {
        let uid = session.user_id?;
        let cid = session.character_id?;
        let now_ticks = self.now_machonet_wallclock_filetime_ticks();
        let entry = {
            let outer = TRANSITION_DISCONNECT_GRACE
                .get()?
                .read()
                .unwrap_or_else(|e| e.into_inner());
            let store = outer.get(&self.instance_id)?;
            let binding = store.entries.get(&bound_id)?;
            let service_matches = expected_services
                .is_none_or(|expected| expected.contains(&binding.entry.service.as_str()));
            if !Self::is_grace_entry_active(binding, now_ticks)
                || !service_matches
                || binding.entry.session_id != session.session_id
                || binding.entry.user_id != uid
                || binding.entry.character_id != cid
            {
                return None;
            }
            binding.entry.clone()
        };
        if !self.has_matching_online_session(session, uid, cid) {
            return None;
        }
        Some(entry)
    }

    pub(crate) fn transition_grace_service_for_bound_id(
        &self,
        session: &SessionState,
        bound_id: u64,
    ) -> Option<String> {
        self.lookup_transition_grace_entry(session, bound_id, None)
            .map(|entry| entry.service)
    }

    /// Returns a recently transition-disconnected `invbroker` binding when an already-in-flight
    /// read-only metadata RPC (`GetSelfInvItem` / `GetAvailableTurretSlots`) arrives within the
    /// 30s transition grace window for the owning live session (`#1146`).
    pub(crate) fn grace_bound_object_for_inflight_invbroker(
        &self,
        session: &SessionState,
        bound_id: u64,
        method: &str,
    ) -> Option<BoundObjectEntry> {
        if !matches!(method, "GetSelfInvItem" | "GetAvailableTurretSlots") {
            return None;
        }
        self.lookup_transition_grace_entry(session, bound_id, Some(&["invbroker"]))
    }

    /// Returns a recently transition-disconnected read-only binding (`invbroker` metadata, owned
    /// `ship.GetDirtTimestamp`, or `crimewatch.GetClientStates`) arriving within the 30s
    /// transition grace window (`#1146`, `#1150`, `#1209`, `#1211`).
    pub(crate) fn grace_bound_object_for_inflight_transition(
        &self,
        session: &SessionState,
        bound_id: u64,
        method: &str,
        args: &[PyRep],
    ) -> Option<BoundObjectEntry> {
        if let Some(entry) =
            self.grace_bound_object_for_inflight_invbroker(session, bound_id, method)
        {
            return Some(entry);
        }
        if method == "GetClientStates" && args.is_empty() {
            return self.lookup_transition_grace_entry(session, bound_id, Some(&["crimewatch"]));
        }
        if method != "GetDirtTimestamp" {
            return None;
        }
        let entry = self.lookup_transition_grace_entry(session, bound_id, Some(&["ship"]))?;
        let cid = session.character_id?;
        if !self.is_owned_ship_dirt_target(session, cid, args) {
            return None;
        }
        Some(entry)
    }

    /// Returns whether `bound_id` is currently authorized via the transition-disconnect grace
    /// cache for `session` and `object_id` (`#1146`).
    pub(crate) fn has_transition_grace_invbroker_binding(
        &self,
        session: &SessionState,
        bound_id: u64,
        object_id: i64,
    ) -> bool {
        self.lookup_transition_grace_entry(session, bound_id, Some(&["invbroker"]))
            .is_some_and(|entry| entry.object_id == object_id)
    }

    /// Invalidates solar-system-scoped and station/structure-scoped bound objects for `char_id`
    /// across location transitions (`stargate_jump`, `dock`, `undock`, `podding`, `wormhole_jump`,
    /// `jump_drive`, `cyno_jump`, `filamentJump`, `jump_bridge`, `shipcaster`, `clone_jump`, etc.)
    /// and emits `OnMachoObjectDisconnect(oid_str, macho_client_id, ref_id)` for each removed binding.
    pub fn invalidate_bound_objects_for_session_transition(
        &self,
        char_id: i64,
        system_id: u32,
        active_dock_id: Option<i64>,
        reason: &str,
    ) {
        if !matches!(reason, "set_account_key")
            && !reason.starts_with("board")
            && !reason.starts_with("eject")
        {
            self.clear_space_ballpark_bootstrapped(char_id);
        }
        if matches!(reason, "login" | "logout") {
            self.clear_brain_primed_for_char(char_id);
        }
        if matches!(reason, "set_account_key" | "login") {
            return;
        }
        if reason == "logout" {
            self.purge_transition_grace_with(
                |_, v| v.entry.character_id == char_id,
                |_, v| v.character_id == char_id,
            );
        }
        let candidates: Vec<(u64, BoundObjectEntry)> = self
            .bound_objects
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|(_, b)| b.character_id == char_id)
            .map(|(&bid, b)| (bid, b.clone()))
            .collect();
        if candidates.is_empty() {
            return;
        }
        let mut to_remove_ids = Vec::new();
        for (bid, b) in &candidates {
            if crate::router::binding_auth::should_invalidate_bound_object(
                self,
                b,
                system_id,
                active_dock_id,
                reason,
            ) {
                to_remove_ids.push(*bid);
            }
        }
        if to_remove_ids.is_empty() {
            return;
        }
        let mut removed = Vec::with_capacity(to_remove_ids.len());
        {
            let mut map = self
                .bound_objects
                .write()
                .unwrap_or_else(|e| e.into_inner());
            for bid in to_remove_ids {
                if let Some(entry) = map.remove(&bid) {
                    removed.push(entry);
                }
            }
            self.prune_notification_bound_ids_locked(&map);
        }
        if reason != "logout" {
            self.record_transition_grace_bindings(&removed);
        }
        for entry in &removed {
            self.emit_macho_object_disconnect(entry);
        }
    }

    pub(crate) fn remove_bound_object_without_notify(&self, session_id: u64, bound_id: u64) {
        let mut map = self
            .bound_objects
            .write()
            .unwrap_or_else(|e| e.into_inner());
        if map.get(&bound_id).is_some_and(|e| e.session_id == session_id) {
            map.remove(&bound_id);
            self.prune_notification_bound_ids_locked(&map);
        }
        drop(map);
        self.purge_transition_grace_entry(session_id, bound_id);
    }

    /// Releases all bound objects across sessions for `(service, object_id)`
    /// (e.g., when a shared station direct trade window completes or cancels).
    pub fn release_bound_objects_for_object(&self, service: &str, object_id: i64) -> usize {
        let removed_count = {
            let mut map = self
                .bound_objects
                .write()
                .unwrap_or_else(|e| e.into_inner());
            let target_bids: Vec<u64> = map
                .iter()
                .filter(|(_, e)| e.service == service && e.object_id == object_id)
                .map(|(&bid, _)| bid)
                .collect();
            for &bid in &target_bids {
                map.remove(&bid);
            }
            if !target_bids.is_empty() {
                self.prune_notification_bound_ids_locked(&map);
            }
            target_bids.len()
        };
        self.purge_transition_grace_with(
            |_, v| v.entry.service == service && v.entry.object_id == object_id,
            |_, _| false,
        );
        removed_count
    }

    /// Releases a single bound object `bound_id` after verifying session ownership.
    pub fn release_bound_object(
        &self,
        session: &SessionState,
        bound_id: u64,
    ) -> Result<(), BridgeRpcError> {
        let _ = self.verify_bound_object(session, bound_id)?;
        let removed = {
            let mut map = self
                .bound_objects
                .write()
                .unwrap_or_else(|e| e.into_inner());
            let entry = map.remove(&bound_id);
            if entry.is_some() {
                self.prune_notification_bound_ids_locked(&map);
            }
            entry
        };
        if let Some(entry) = removed {
            self.purge_transition_grace_with(|&bid, _| bid == bound_id, |_, _| false);
            self.emit_macho_object_disconnect(&entry);
        }
        Ok(())
    }

    /// Validates container ownership, co-location, and 2,500m in-space loot range for `invbroker.GetContainerContents`.
    pub async fn get_container_contents(
        &self,
        session: &SessionState,
        bound_entry: Option<&BoundObjectEntry>,
        char_id: i64,
        container_id: i64,
    ) -> Result<eve_proto::PyRep, BridgeRpcError> {
        use eve_proto::{DBRowDescriptor, PyRep};

        let (owner_id, loc_id, group_id, category_id, owned_docked_or_ship_rooted, rows) =
            self.db.with_snapshot(|snap| {
                let Some(container) = snap.items.get(&container_id) else {
                    return Err(BridgeRpcError {
                        code: "ItemNotFound".into(),
                        message: format!("Container item {container_id} not found"),
                        client_error: None,
                    });
                };
                let (grp, cat) = self
                    .sde
                    .types
                    .get(&container.type_id)
                    .map(|t| (t.group_id, t.category_id))
                    .unwrap_or((0, 0));
                let mut owned_docked_or_ship_rooted = session.ship_id == Some(container_id);
                if !owned_docked_or_ship_rooted && container.owner_id == char_id {
                    let mut cur = container.location_id;
                    for _ in 0..8 {
                        if cur == container_id {
                            break;
                        }
                        if session.station_id == Some(cur)
                            || session.structure_id == Some(cur)
                            || session.ship_id == Some(cur)
                            || self.sde.stations.contains_key(&cur)
                            || snap
                                .structures
                                .get(&cur)
                                .is_some_and(|st| st.state != "Destroyed")
                        {
                            owned_docked_or_ship_rooted = true;
                            break;
                        }
                        let Some(parent) = snap.items.get(&cur) else {
                            break;
                        };
                        if parent.owner_id != char_id {
                            // Never traverse through a foreign ship/container ancestor (#1290).
                            break;
                        }
                        cur = parent.location_id;
                    }
                }
                let rows: Vec<Vec<PyRep>> = snap
                    .items
                    .values()
                    .filter(|i| i.location_id == container_id)
                    .map(|i| {
                        let (g, c) = self
                            .sde
                            .types
                            .get(&i.type_id)
                            .map(|t| (t.group_id as i64, t.category_id as i64))
                            .unwrap_or((0, 0));
                        let qty = if i.is_blueprint_copy {
                            -2
                        } else if i.singleton {
                            -1
                        } else {
                            i.quantity as i64
                        };
                        vec![
                            PyRep::Integer(i.item_id),
                            PyRep::Integer(i.type_id as i64),
                            PyRep::Integer(i.owner_id),
                            PyRep::Integer(i.location_id),
                            PyRep::Integer(i.flag as i64),
                            PyRep::Integer(qty),
                            PyRep::Integer(g),
                            PyRep::Integer(c),
                            PyRep::String(String::new()),
                        ]
                    })
                    .collect();
                Ok((
                    container.owner_id,
                    container.location_id,
                    grp,
                    cat,
                    owned_docked_or_ship_rooted,
                    rows,
                ))
            })?;

        let deny = || BridgeRpcError {
            code: "CantDoThatWithSomeoneElsesStuff".into(),
            message: format!("Container {container_id} is not accessible by character {char_id}"),
            client_error: None,
        };

        if let Some(b) = bound_entry {
            let bound_is_session_mgr = b.service == "invbroker"
                && (session.station_id == Some(b.object_id)
                    || session.structure_id == Some(b.object_id)
                    || (session.station_id.is_none()
                        && session.solar_system_id.map(i64::from) == Some(b.object_id)));
            if b.object_id != container_id && b.object_id != loc_id && !bound_is_session_mgr {
                return Err(deny());
            }
        }

        if owner_id == char_id {
            if !owned_docked_or_ship_rooted {
                let Some(sys_id) = u32::try_from(loc_id).ok().filter(|s| {
                    session.station_id.is_none()
                        && session.structure_id.is_none()
                        && session.solar_system_id == Some(*s)
                        && self.sde.solar_systems.contains_key(s)
                }) else {
                    return Err(deny());
                };
                let Some(ship_id) = session.ship_id else {
                    return Err(deny());
                };
                let sys_handle = self.systems.get_or_spawn(sys_id);
                let ship_ball = sys_handle.get_ball_snapshot(ship_id).await.ok().flatten();
                let cont_ball = sys_handle
                    .get_ball_snapshot(container_id)
                    .await
                    .ok()
                    .flatten();
                let (Some((_, sb)), Some((_, cb))) = (ship_ball, cont_ball) else {
                    return Err(deny());
                };
                if sb.surface_distance(&cb) > 2_500.0 {
                    return Err(deny());
                }
            }
        } else {
            let is_openable_container =
                category_id != 6 && matches!(group_id, 12 | 186 | 340 | 448 | 649);
            if !is_openable_container {
                return Err(deny());
            }
            let Some(sys_id) = u32::try_from(loc_id).ok().filter(|s| {
                session.station_id.is_none()
                    && session.structure_id.is_none()
                    && session.solar_system_id == Some(*s)
                    && self.sde.solar_systems.contains_key(s)
            }) else {
                return Err(deny());
            };
            let Some(ship_id) = session.ship_id else {
                return Err(deny());
            };
            let sys_handle = self.systems.get_or_spawn(sys_id);
            let ship_ball = sys_handle.get_ball_snapshot(ship_id).await.ok().flatten();
            let cont_ball = sys_handle
                .get_ball_snapshot(container_id)
                .await
                .ok()
                .flatten();
            let (Some((_, sb)), Some((_, cb))) = (ship_ball, cont_ball) else {
                return Err(deny());
            };
            if sb.surface_distance(&cb) > 2_500.0 {
                return Err(deny());
            }
        }

        Ok(DBRowDescriptor::inv_item_header().build_crowset(rows))
    }

    /// Builds a `CombatState` for a player's ship populated from its Dogma `ShipFittingEngine`
    /// attributes (or SDE `default_attributes` fallback), including all 12 shield/armor/hull resonances.
    pub fn build_ship_combat_state(
        &self,
        char_id: i64,
        ship_id: i64,
        type_id: u32,
    ) -> eve_destiny::CombatState {
        if let Some(proj) = self.build_fit_projection(char_id, ship_id, self.current_fit_rev(ship_id)) {
            let b = proj.baseline;
            let tp_mult = self.effective_target_painter_multiplier(ship_id);
            let base_sig = b.signature_radius;
            let sig = if tp_mult > 1.0 + 1e-6 { base_sig * tp_mult } else { base_sig };
            let mut cs = eve_destiny::CombatState {
                shield_hp: b.max_shield_hp,
                max_shield_hp: b.max_shield_hp,
                armor_hp: b.max_armor_hp,
                max_armor_hp: b.max_armor_hp,
                structure_hp: b.max_structure_hp,
                max_structure_hp: b.max_structure_hp,
                capacitor: b.max_capacitor,
                max_capacitor: b.max_capacitor,
                signature_radius: sig,
                base_signature_radius: Some(base_sig),
                tp_sig_multiplier: (tp_mult > 1.0 + 1e-6).then_some(tp_mult),
                shield_recharge_time_ms: b.shield_recharge_time_ms,
                capacitor_recharge_time_ms: b.capacitor_recharge_time_ms,
                shield_resonance: b.shield_resonance,
                armor_resonance: b.armor_resonance,
                structure_resonance: b.structure_resonance,
                base_max_velocity: (b.max_velocity > 0.0).then_some(b.max_velocity),
                base_mass: (b.mass > 0.0).then_some(b.mass),
                base_inertia: (b.agility > 0.0).then_some(b.agility),
                base_shield_resonance: Some(b.shield_resonance),
                base_armor_resonance: Some(b.armor_resonance),
                base_structure_resonance: Some(b.structure_resonance),
                base_max_shield_hp: Some(b.max_shield_hp),
                base_max_armor_hp: Some(b.max_armor_hp),
                base_max_structure_hp: Some(b.max_structure_hp),
                base_max_capacitor: Some(b.max_capacitor),
                base_capacitor_recharge_time_ms: Some(b.capacitor_recharge_time_ms),
                rng_state: eve_destiny::CombatState::seed_rng_from_u64(
                    (ship_id as u64)
                        ^ (char_id as u64).rotate_left(17)
                        ^ (u64::from(type_id) << 32),
                ),
                ..eve_destiny::CombatState::default()
            };
            crate::sp6_lifecycle::apply_persisted_hp_to_combat_state(self, ship_id, &mut cs);
            return cs;
        }
        let mut cs = self.build_type_combat_state(type_id);
        {
            use eve_sde::attr_ids as a;
            let mut eff_attrs: std::collections::HashMap<u16, f64> = [
                (a::SHIELD_CAPACITY, cs.max_shield_hp), (a::ARMOR_HP, cs.max_armor_hp),
                (a::HP_STRUCT, cs.max_structure_hp), (a::CAPACITOR_CAPACITY, cs.max_capacitor),
                (a::SIGNATURE_RADIUS, cs.signature_radius),
                (a::SHIELD_RECHARGE_RATE, cs.shield_recharge_time_ms),
                (a::RECHARGE_RATE, cs.capacitor_recharge_time_ms),
                (a::SHIELD_EM_DAMAGE_RESONANCE, cs.shield_resonance[0]),
                (a::SHIELD_THERMAL_DAMAGE_RESONANCE, cs.shield_resonance[1]),
                (a::SHIELD_KINETIC_DAMAGE_RESONANCE, cs.shield_resonance[2]),
                (a::SHIELD_EXPLOSIVE_DAMAGE_RESONANCE, cs.shield_resonance[3]),
                (a::ARMOR_EM_DAMAGE_RESONANCE, cs.armor_resonance[0]),
                (a::ARMOR_THERMAL_DAMAGE_RESONANCE, cs.armor_resonance[1]),
                (a::ARMOR_KINETIC_DAMAGE_RESONANCE, cs.armor_resonance[2]),
                (a::ARMOR_EXPLOSIVE_DAMAGE_RESONANCE, cs.armor_resonance[3]),
                (a::EM_DAMAGE_RESONANCE, cs.structure_resonance[0]),
                (a::THERMAL_DAMAGE_RESONANCE, cs.structure_resonance[1]),
                (a::KINETIC_DAMAGE_RESONANCE, cs.structure_resonance[2]),
                (a::EXPLOSIVE_DAMAGE_RESONANCE, cs.structure_resonance[3]),
            ].into_iter().collect();
            crate::services::inventory_fitting::fitting_scanners_superweapons::apply_active_dbuffs_and_system_effects_to_ship_attrs(self, ship_id, &mut eff_attrs);
            (cs.max_shield_hp, cs.shield_hp) = (eff_attrs[&a::SHIELD_CAPACITY], eff_attrs[&a::SHIELD_CAPACITY]);
            (cs.max_armor_hp, cs.armor_hp) = (eff_attrs[&a::ARMOR_HP], eff_attrs[&a::ARMOR_HP]);
            (cs.max_structure_hp, cs.structure_hp) = (eff_attrs[&a::HP_STRUCT], eff_attrs[&a::HP_STRUCT]);
            (cs.max_capacitor, cs.capacitor) = (eff_attrs[&a::CAPACITOR_CAPACITY], eff_attrs[&a::CAPACITOR_CAPACITY]);
            cs.signature_radius = eff_attrs[&a::SIGNATURE_RADIUS];
            cs.shield_recharge_time_ms = eff_attrs[&a::SHIELD_RECHARGE_RATE];
            cs.capacitor_recharge_time_ms = eff_attrs[&a::RECHARGE_RATE];
            cs.shield_resonance = [eff_attrs[&a::SHIELD_EM_DAMAGE_RESONANCE], eff_attrs[&a::SHIELD_THERMAL_DAMAGE_RESONANCE], eff_attrs[&a::SHIELD_KINETIC_DAMAGE_RESONANCE], eff_attrs[&a::SHIELD_EXPLOSIVE_DAMAGE_RESONANCE]];
            cs.armor_resonance = [eff_attrs[&a::ARMOR_EM_DAMAGE_RESONANCE], eff_attrs[&a::ARMOR_THERMAL_DAMAGE_RESONANCE], eff_attrs[&a::ARMOR_KINETIC_DAMAGE_RESONANCE], eff_attrs[&a::ARMOR_EXPLOSIVE_DAMAGE_RESONANCE]];
            cs.structure_resonance = [eff_attrs[&a::EM_DAMAGE_RESONANCE], eff_attrs[&a::THERMAL_DAMAGE_RESONANCE], eff_attrs[&a::KINETIC_DAMAGE_RESONANCE], eff_attrs[&a::EXPLOSIVE_DAMAGE_RESONANCE]];
            cs.base_max_shield_hp = Some(cs.max_shield_hp);
            cs.base_max_armor_hp = Some(cs.max_armor_hp);
            cs.base_max_structure_hp = Some(cs.max_structure_hp);
            cs.base_max_capacitor = Some(cs.max_capacitor);
            cs.base_capacitor_recharge_time_ms = Some(cs.capacitor_recharge_time_ms);
            cs.base_signature_radius = Some(cs.signature_radius);
            cs.base_shield_resonance = Some(cs.shield_resonance);
            cs.base_armor_resonance = Some(cs.armor_resonance);
            cs.base_structure_resonance = Some(cs.structure_resonance);
        }
        cs.rng_state = eve_destiny::CombatState::seed_rng_from_u64(
            (ship_id as u64) ^ (char_id as u64).rotate_left(17) ^ (u64::from(type_id) << 32),
        );
        crate::sp6_lifecycle::apply_persisted_hp_to_combat_state(self, ship_id, &mut cs);
        cs
    }

    /// Builds a `CombatState` directly from a type's SDE `default_attributes` (for NPCs, wrecks, or fallback hulls).
    pub fn build_type_combat_state(&self, type_id: u32) -> eve_destiny::CombatState {
        let seq = self
            .combat_rng_seq
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let rng_state =
            eve_destiny::CombatState::seed_rng_from_u64(seq ^ (u64::from(type_id) << 32));
        if let Some(t) = self.sde.types.get(&type_id) {
            let a = &t.default_attributes;
            let shield = a
                .get(&eve_sde::attr_ids::SHIELD_CAPACITY)
                .copied()
                .unwrap_or(50.0);
            let armor = a.get(&eve_sde::attr_ids::ARMOR_HP).copied().unwrap_or(50.0);
            let hull = a
                .get(&eve_sde::attr_ids::HP_STRUCT)
                .copied()
                .unwrap_or(50.0);
            let cap = a
                .get(&eve_sde::attr_ids::CAPACITOR_CAPACITY)
                .copied()
                .unwrap_or(100.0);
            let sig = a
                .get(&eve_sde::attr_ids::SIGNATURE_RADIUS)
                .copied()
                .unwrap_or(t.radius.max(25.0));
            let shield_rech = a
                .get(&eve_sde::attr_ids::SHIELD_RECHARGE_RATE)
                .copied()
                .unwrap_or(120_000.0);
            let cap_rech = a
                .get(&eve_sde::attr_ids::RECHARGE_RATE)
                .copied()
                .unwrap_or(120_000.0);
            let shield_res = [
                a.get(&eve_sde::attr_ids::SHIELD_EM_DAMAGE_RESONANCE)
                    .copied()
                    .unwrap_or(1.0),
                a.get(&eve_sde::attr_ids::SHIELD_THERMAL_DAMAGE_RESONANCE)
                    .copied()
                    .unwrap_or(1.0),
                a.get(&eve_sde::attr_ids::SHIELD_KINETIC_DAMAGE_RESONANCE)
                    .copied()
                    .unwrap_or(1.0),
                a.get(&eve_sde::attr_ids::SHIELD_EXPLOSIVE_DAMAGE_RESONANCE)
                    .copied()
                    .unwrap_or(1.0),
            ];
            let armor_res = [
                a.get(&eve_sde::attr_ids::ARMOR_EM_DAMAGE_RESONANCE)
                    .copied()
                    .unwrap_or(1.0),
                a.get(&eve_sde::attr_ids::ARMOR_THERMAL_DAMAGE_RESONANCE)
                    .copied()
                    .unwrap_or(1.0),
                a.get(&eve_sde::attr_ids::ARMOR_KINETIC_DAMAGE_RESONANCE)
                    .copied()
                    .unwrap_or(1.0),
                a.get(&eve_sde::attr_ids::ARMOR_EXPLOSIVE_DAMAGE_RESONANCE)
                    .copied()
                    .unwrap_or(1.0),
            ];
            let struct_res = [
                a.get(&eve_sde::attr_ids::EM_DAMAGE_RESONANCE)
                    .copied()
                    .unwrap_or(1.0),
                a.get(&eve_sde::attr_ids::THERMAL_DAMAGE_RESONANCE)
                    .copied()
                    .unwrap_or(1.0),
                a.get(&eve_sde::attr_ids::KINETIC_DAMAGE_RESONANCE)
                    .copied()
                    .unwrap_or(1.0),
                a.get(&eve_sde::attr_ids::EXPLOSIVE_DAMAGE_RESONANCE)
                    .copied()
                    .unwrap_or(1.0),
            ];
            return eve_destiny::CombatState {
                shield_hp: shield,
                max_shield_hp: shield,
                armor_hp: armor,
                max_armor_hp: armor,
                structure_hp: hull,
                max_structure_hp: hull,
                capacitor: cap,
                max_capacitor: cap,
                signature_radius: sig,
                base_signature_radius: Some(sig),
                shield_recharge_time_ms: shield_rech,
                capacitor_recharge_time_ms: cap_rech,
                shield_resonance: shield_res,
                armor_resonance: armor_res,
                structure_resonance: struct_res,
                base_max_velocity: a.get(&eve_sde::attr_ids::MAX_VELOCITY).copied().filter(|&v| v > 0.0),
                base_mass: a.get(&eve_sde::attr_ids::MASS).copied().filter(|&v| v > 0.0),
                base_inertia: a.get(&eve_sde::attr_ids::AGILITY).copied().filter(|&v| v > 0.0),
                base_shield_resonance: Some(shield_res),
                base_armor_resonance: Some(armor_res),
                base_structure_resonance: Some(struct_res),
                base_max_shield_hp: Some(shield),
                base_max_armor_hp: Some(armor),
                base_max_structure_hp: Some(hull),
                base_max_capacitor: Some(cap),
                base_capacitor_recharge_time_ms: Some(cap_rech),
                rng_state,
                ..eve_destiny::CombatState::default()
            };
        }
        eve_destiny::CombatState {
            rng_state,
            ..eve_destiny::CombatState::default()
        }
    }

    /// Builds a [`eve_destiny::SlimItemMeta`] for a player ship from SDE type metadata and the character's DB record (`P-7`).
    pub fn build_ship_slim_meta(&self, char_id: i64, type_id: u32) -> eve_destiny::SlimItemMeta {
        let (group_id, category_id) = self
            .sde
            .types
            .get(&type_id)
            .map(|t| (t.group_id, t.category_id))
            .unwrap_or((25, 6));
        let (corp_id, alliance_id, war_faction_id, security_status, raw_skin, custom_name) = self.db.with_snapshot(|snap| {
            let Some(ch) = snap.characters.get(&char_id) else {
                return (1_000_009, 0, None, 0.0, None, None);
            };
            let skin = snap.applied_ship_skins.get(&ch.active_ship_id).copied();
            let ship_name = snap
                .item_custom_names
                .get(&ch.active_ship_id)
                .filter(|n| !n.is_empty() && snap.items.get(&ch.active_ship_id).is_some_and(|i| i.type_id == type_id))
                .cloned();
            (
                ch.corporation_id,
                ch.alliance_id.unwrap_or(0),
                ch.war_faction_id,
                ch.security_status,
                skin,
                ship_name,
            )
        });
        let ship_display_name = custom_name
            .or_else(|| self.sde.types.get(&type_id).map(|t| t.name.clone()).filter(|n| !n.is_empty()));
        let skin_id = raw_skin.map(|raw| {
            self.sde
                .skin_licenses
                .get(&raw)
                .map(|l| l.skin_id)
                .unwrap_or(raw)
        });
        let skin_material_set_id = skin_id
            .and_then(|sid| self.sde.skins.get(&sid))
            .and_then(|sk| self.sde.skin_materials.get(&sk.skin_material_id))
            .and_then(|m| m.material_set_id);
        eve_destiny::SlimItemMeta {
            group_id,
            category_id,
            owner_id: char_id,
            char_id: Some(char_id),
            corp_id,
            alliance_id,
            security_status,
            is_cloaked: false,
            skin_id,
            skin_material_set_id,
            jump_target: None,
            war_faction_id,
            name: ship_display_name,
            ..Default::default()
        }
    }

    /// Emits an `OnSkillQueuePausedServer` notification to character (`feat:skills.queue-pause`).
    pub fn notify_skill_queue_paused_server(&self, char_id: i64) -> Vec<PendingMachoNotification> {
        self.emit_typed(
            NotificationTarget::Character(char_id),
            OnSkillQueuePausedServer { paused: None },
        )
    }

    /// `OnNewSkillQueueSaved(newQueue)`; the client stores `newQueue` as its skill queue and
    /// scatters `OnSkillQueueChanged`.
    pub fn notify_new_skill_queue_saved(
        &self,
        char_id: i64,
        new_queue: PyRep,
    ) -> Vec<PendingMachoNotification> {
        self.emit_typed(
            NotificationTarget::Character(char_id),
            OnNewSkillQueueSaved { new_queue },
        )
    }

    /// Wires the module deactivation hook on the `SystemRegistry` to handle automatic cycle deactivations
    /// (e.g. entering warp, target out of range, or target leaving the ballpark).
    /// Emits `OnGodmaShipEffect` (with `isActive = 0`) to the pilot and invalidates the cached ship fitting.
    pub fn wire_module_deactivation_hook(&self) {
        let ctx_w = self.downgrade();

        self.systems.set_module_deactivation_hook(Arc::new(move |char_id, cycle| {
            let Some(step_ctx) = ctx_w.upgrade() else { return; };
            let sde = &step_ctx.sde;
            let db = &step_ctx.db;
            let pending = &step_ctx.pending_notifications;
            let notify_tx = &step_ctx.notify_tx;
            if eve_db::lock_rank::held_lock_ranks().is_empty() {
                step_ctx.invalidate_and_push_fit(cycle.ship_id);
            } else {
                step_ctx.invalidate_ship_fitting_cache_only(cycle.ship_id);
                if let Ok(rt) = tokio::runtime::Handle::try_current() {
                    let ctx_w_spawn = ctx_w.clone();
                    let sid = cycle.ship_id;
                    rt.spawn(async move {
                        if let Some(c) = ctx_w_spawn.upgrade() {
                            c.push_fit_projection_for_ship(sid);
                        }
                    });
                }
            }

            let is_cloak = sde.types.get(&cycle.module_type_id).is_some_and(|t| t.group_id == 330)
                || cycle.effect_name.to_ascii_lowercase().contains("cloak");
            if is_cloak {
                let reactiv_delay_s = if cycle.reactivation_delay_s > 0.0 {
                    cycle.reactivation_delay_s
                } else {
                    sde.types
                        .get(&cycle.module_type_id)
                        .and_then(|t| t.default_attributes.get(&669).copied())
                        .map(|ms| ms / 1000.0)
                        .unwrap_or(0.0)
                };
                if reactiv_delay_s > 0.0 {
                    let now_s = db.clock().now_unix_secs() as f64;
                    let solar_system_id = db.with_snapshot(|snap| {
                        snap.characters.get(&char_id).map(|c| c.solar_system_id)
                    });
                    let sys_sim_s = solar_system_id
                        .map(|sid| step_ctx.systems.get_system_sim_elapsed(sid))
                        .unwrap_or(0.0);
                    step_ctx.module_reactivation_cooldowns.write().unwrap_or_else(|e| e.into_inner()).insert(
                        (cycle.ship_id, cycle.module_id, cycle.module_type_id),
                        super::context::ModuleCooldownEntry {
                            recorded_unix_f64: now_s,
                            delay_s: reactiv_delay_s,
                            solar_system_id,
                            recorded_sys_sim_s: sys_sim_s,
                        },
                    );
                }
            }

            let fanout_modules: Vec<i64> = if cycle.linked_turrets.is_empty() {
                vec![cycle.module_id]
            } else {
                cycle.linked_turrets.clone()
            };
            let (solar_system_id, removed_charge_tid, fanout_types, user_id) = db.with_mut_snapshot(|snap| {
                snap.module_active_states.remove(&cycle.module_id);
                let removed_charge = snap.module_active_charges.remove(&cycle.module_id);
                for &mid in &fanout_modules {
                    if mid != cycle.module_id {
                        snap.module_active_states.remove(&mid);
                        snap.module_active_charges.remove(&mid);
                    }
                }
                let types: Vec<(i64, u32)> = fanout_modules
                    .iter()
                    .map(|&mid| {
                        let tid = snap
                            .items
                            .get(&mid)
                            .map(|i| i.type_id)
                            .unwrap_or(cycle.module_type_id);
                        (mid, tid)
                    })
                    .collect();
                let (sys_id, uid) = snap
                    .characters
                    .get(&char_id)
                    .map(|c| (Some(c.solar_system_id), Some(c.user_id)))
                    .unwrap_or((None, None));
                (sys_id, removed_charge, types, uid)
            });
            let charge_type_id = cycle.ammo_type_id.or(removed_charge_tid);

            let _ = crate::services::notify_emitters_space_dungeons::flush_module_ewar_end(
                &step_ctx,
                char_id,
                solar_system_id,
                cycle.ship_id,
                cycle.module_id,
            );
            crate::services::tactical_modules::remove_projected_remote_assistance(
                &step_ctx,
                cycle.ship_id,
                cycle.module_id,
            );
            if sde
                .types
                .get(&cycle.module_type_id)
                .is_some_and(|t| t.group_id == 1815)
            {
                let cleared =
                    crate::services::inventory_fitting::clear_ship_dbuffs_by_source_module(
                        &step_ctx,
                        cycle.module_id,
                    );
                if !cleared.is_empty() {
                    let sync_now = step_ctx.now_unix_secs();
                    if eve_db::lock_rank::held_lock_ranks().is_empty() {
                        crate::services::inventory_fitting::sync_ship_dbuffs_and_destiny_sync(
                            &step_ctx,
                            &cleared,
                            sync_now,
                        );
                    } else {
                        for &sid in &cleared {
                            step_ctx.invalidate_ship_fitting_cache_only(sid);
                        }
                        let ctx_w_dbuff = ctx_w.clone();
                        step_ctx.systems.enqueue_post_unlock_action(Box::new(move || {
                            if let Some(c) = ctx_w_dbuff.upgrade() {
                                crate::services::inventory_fitting::sync_ship_dbuffs_and_destiny_sync(
                                    &c,
                                    &cleared,
                                    sync_now,
                                );
                            }
                        }));
                    }
                }
            }

            let effect_id = effect_id_for_module_sde(sde, cycle.module_type_id, Some(&cycle.effect_name)).unwrap_or(0);
            let now = step_ctx.now_system_sim_filetime_ticks(solar_system_id);
            let environment = PyRep::Tuple(vec![
                PyRep::Integer(cycle.module_id),
                PyRep::Integer(char_id),
                PyRep::Integer(cycle.ship_id),
                cycle.target_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                PyRep::None,
                PyRep::List(vec![]),
                PyRep::Integer(effect_id),
            ]);
            emit_typed_target_to_char_capped(
                Some(&**pending),
                Some(notify_tx),
                char_id,
                user_id,
                OnGodmaShipEffect::DEFAULT_ID_TYPE,
                now,
                512,
                OnGodmaShipEffect {
                    item_id: cycle.module_id,
                    effect: PyRep::Integer(effect_id),
                    start_or_time: PyRep::Integer(now),
                    start: Some(PyRep::Integer(0)),
                    active: Some(PyRep::Integer(0)),
                    environment: Some(environment),
                    start_time: Some(PyRep::Integer(now)),
                    duration: Some(PyRep::Real(0.0)),
                    repeat: Some(PyRep::Integer(0)),
                    error: Some(PyRep::None),
                    actual_stop_time: None,
                },
            );

            let observers = step_ctx.space_observer_chars_for_ship(cycle.ship_id, char_id);
            for (fan_mod_id, fan_type_id) in fanout_types {
                let fan_effect_id =
                    effect_id_for_module_sde(sde, fan_type_id, Some(&cycle.effect_name)).unwrap_or(effect_id);
                if let Some((sfx_guid, is_offensive)) = special_fx_guid_for_module_sde(
                    sde,
                    fan_type_id,
                    fan_effect_id,
                    Some(&cycle.effect_name),
                ) {
                    let sfx_notif = OnSpecialFX {
                        ship_id: cycle.ship_id,
                        module_id: Some(fan_mod_id),
                        module_type_id: Some(i64::from(fan_type_id)),
                        target_id: cycle.target_id,
                        other_type_id: charge_type_id.map(i64::from),
                        guid: sfx_guid,
                        is_offensive,
                        start: 0,
                        active: 0,
                        duration: cycle.duration_s * 1000.0,
                        repeat: 0,
                        start_time: now,
                        time_from_start: 0.0,
                        graphic_info: None,
                    };
                    let push_sfx = |recip_cid: i64, recip_uid: Option<u64>| {
                        emit_typed_target_to_char_capped(
                            Some(&**pending),
                            Some(notify_tx),
                            recip_cid,
                            recip_uid,
                            OnSpecialFX::DEFAULT_ID_TYPE,
                            now,
                            512,
                            sfx_notif.clone(),
                        );
                    };
                    push_sfx(char_id, user_id);
                    for &(obs_cid, obs_uid) in &observers {
                        push_sfx(obs_cid, Some(obs_uid));
                    }
                }
            }

            let auto_reload_ev: Option<(Vec<i64>, u32, f64)> = db.with_mut_snapshot(|snap| {
                snap.cluster_and_runtime
                    .auto_reloaded_event
                    .remove(&cycle.module_id)
            });
            if let Some((mod_ids, charge_tid, reload_ms)) = auto_reload_ev {
                emit_typed_target_to_char_capped(
                    Some(&**pending),
                    Some(notify_tx),
                    char_id,
                    user_id,
                    OnChargeBeingLoadedToModule::DEFAULT_ID_TYPE,
                    now,
                    512,
                    OnChargeBeingLoadedToModule {
                        item_ids: mod_ids,
                        charge_type_id: i64::from(charge_tid),
                        time_ms: reload_ms,
                    },
                );
            }
        }));

        let db_lock = Arc::downgrade(&self.db);
        let pending_lock = Arc::downgrade(&self.pending_notifications);
        let notify_tx_lock = self.notify_tx.clone();
        self.systems.set_target_lock_completed_hook(Arc::new(move |char_id, target_id| {
            let (Some(db_lock), Some(pending_lock)) = (db_lock.upgrade(), pending_lock.upgrade()) else {
                return;
            };
            let now = db_lock.clock().now_blueos_filetime_ticks();
            let enqueue_target_ev = |rcpt_char: i64, rcpt_user: Option<u64>, sub_ev: &str, item_id: i64| {
                emit_typed_target_to_char_capped(
                    Some(&*pending_lock),
                    Some(&notify_tx_lock),
                    rcpt_char,
                    rcpt_user,
                    OnMultiEvent::DEFAULT_ID_TYPE,
                    now,
                    512,
                    OnMultiEvent {
                        events: vec![PyRep::Tuple(vec![
                            PyRep::Tuple(vec![
                                PyRep::String("OnTarget".into()),
                                PyRep::String(sub_ev.into()),
                                PyRep::Integer(item_id),
                                PyRep::String(String::new()),
                            ]),
                            PyRep::Integer(0),
                        ])],
                    },
                );
            };
            let hook_info = db_lock.with_snapshot(|s| {
                let locker = s.characters.get(&char_id)?;
                if locker.station_id.is_some() { return None; }
                let target_info = s.items.get(&target_id).map(|i| i.owner_id)
                    .filter(|&oid| oid != char_id && s.characters.contains_key(&oid))
                    .and_then(|target_char| {
                        let target = s.characters.get(&target_char)?;
                        if target.station_id.is_some() || locker.solar_system_id != target.solar_system_id {
                            return None;
                        }
                        let locker_ship = crate::services::station_structure::structure_upwell_ops::load_ext_state(s)
                            .active_pilot_by_structure
                            .iter()
                            .find_map(|(&sid, &pid)| (pid == char_id).then_some(sid))
                            .or_else(|| (locker.active_ship_id > 0).then_some(locker.active_ship_id))
                            .unwrap_or(char_id);
                        Some((target_char, Some(target.user_id), locker_ship))
                    });
                Some((Some(locker.user_id), target_info))
            });
            if let Some((locker_user, target_owner_info)) = hook_info {
                enqueue_target_ev(char_id, locker_user, "add", target_id);
                if let Some((target_char, target_user, locker_ship)) = target_owner_info {
                    enqueue_target_ev(target_char, target_user, "otheradd", locker_ship);
                }
            }
        }));
    }

    pub fn upsert_projected_ewar_on_ship(
        &self,
        source_ship_id: i64,
        target_ship_id: i64,
        effect: eve_dogma::ProjectedEwarEffect,
    ) {
        self.projected_ewar_sources
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(effect.source_module_id, source_ship_id);
        {
            let mut guard = self.projected_ewar.write().unwrap_or_else(|e| e.into_inner());
            let list = guard.entry(target_ship_id).or_default();
            if let Some(existing) = list
                .iter_mut()
                .find(|e| e.source_module_id == effect.source_module_id)
            {
                *existing = effect.clone();
            } else {
                list.push(effect.clone());
            }
        }
        {
            let mut fit_guard = self
                .ship_fittings
                .write()
                .unwrap_or_else(|e| e.into_inner());
            if let Some(fit) = fit_guard.get_mut(&target_ship_id) {
                fit.upsert_projected_ewar(effect, &self.sde);
            }
        }
        self.push_fit_projection_for_ship(target_ship_id);
    }

    pub fn remove_projected_ewar_by_module(&self, source_ship_id: i64, source_module_id: i64) -> Vec<i64> {
        let recorded = self
            .projected_ewar_sources
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&source_module_id)
            .copied();
        match recorded {
            Some(recorded_ship) if recorded_ship != source_ship_id => return Vec::new(),
            Some(_) => {
                self.projected_ewar_sources
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&source_module_id);
            }
            None => {
                if !self.db.with_snapshot(|s| {
                    s.items
                        .get(&source_module_id)
                        .is_some_and(|i| i.location_id == source_ship_id)
                }) {
                    return Vec::new();
                }
            }
        }
        let mut affected_ships = Vec::new();
        {
            let mut guard = self.projected_ewar.write().unwrap_or_else(|e| e.into_inner());
            for (&ship_id, list) in guard.iter_mut() {
                let before = list.len();
                list.retain(|e| e.source_module_id != source_module_id);
                if list.len() < before {
                    affected_ships.push(ship_id);
                }
            }
        }
        if !affected_ships.is_empty() {
            {
                let mut fit_guard = self
                    .ship_fittings
                    .write()
                    .unwrap_or_else(|e| e.into_inner());
                for &ship_id in &affected_ships {
                    if let Some(fit) = fit_guard.get_mut(&ship_id) {
                        fit.remove_projected_ewar(source_module_id, &self.sde);
                    }
                }
            }
            for &ship_id in &affected_ships {
                self.push_fit_projection_for_ship(ship_id);
            }
        }
        affected_ships
    }

    /// Computes a target entity's effective signature radius including active Target Painters
    /// (`group 379`, stacking-penalized) and ship fitting modifiers (`feat:combat.target-painter-bloom`).
    pub fn effective_target_signature_radius(&self, target_ball_id: i64, fallback_base_sig: f64) -> f64 {
        let (owner_char_opt, item_base_sig_opt, tp_resist_opt) = self.db.with_snapshot(|snap| {
            let char_id = snap.characters.values().find(|c| c.active_ship_id == target_ball_id).map(|c| c.character_id)
                .or_else(|| snap.items.get(&target_ball_id).map(|i| i.owner_id).filter(|oid| snap.characters.contains_key(oid)));
            let item_t = snap.items.get(&target_ball_id).and_then(|i| self.sde.types.get(&i.type_id));
            let base_sig = item_t
                .and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::SIGNATURE_RADIUS).copied()).filter(|&v| v > 0.0);
            let tp_resist = item_t
                .and_then(|t| t.default_attributes.get(&2114).copied());
            (char_id, base_sig, tp_resist)
        });
        if let Some(cid) = owner_char_opt {
            if let Ok(fit) = self.fit(cid, target_ball_id) {
                let sig = fit.ship.get_attribute(eve_sde::attr_ids::SIGNATURE_RADIUS, &self.sde);
                if sig > 0.0 {
                    return sig;
                }
            }
        }
        let base_sig = item_base_sig_opt.unwrap_or_else(|| fallback_base_sig.max(1.0));
        let tp_resist = tp_resist_opt.unwrap_or(1.0).clamp(0.0, 1.0);
        let bonuses: Vec<f64> = self.projected_ewar.read().unwrap_or_else(|e| e.into_inner())
            .get(&target_ball_id)
            .map(|list| {
                list.iter()
                    .filter(|proj| self.sde.types.get(&proj.source_module_type_id).is_some_and(|t| t.group_id == 379) || proj.effective_module.base_attributes.contains_key(&554))
                    .map(|proj| {
                        let opt = proj.effective_module.get_attribute(eve_sde::attr_ids::MAX_RANGE, &self.sde);
                        let fall = eve_dogma::get_module_falloff(&proj.effective_module, &self.sde);
                        let att = eve_dogma::compute_falloff_attenuation(proj.distance, opt, fall);
                        proj.effective_module.get_attribute(554, &self.sde) * att * tp_resist
                    })
                    .collect()
            })
            .unwrap_or_default();
        eve_dogma::eval_target_painters_sig_radius(base_sig, &bonuses)
    }

    /// Computes ONLY the stacking-penalized Target Painter signature radius multiplier (`>= 1.0`)
    /// from active projected EWAR on `target_ball_id` (`#584`).
    pub fn effective_target_painter_multiplier(&self, target_ball_id: i64) -> f64 {
        let tp_resist = self.db.with_snapshot(|snap| {
            snap.items
                .get(&target_ball_id)
                .and_then(|i| self.sde.types.get(&i.type_id))
                .and_then(|t| t.default_attributes.get(&2114).copied())
        }).unwrap_or(1.0).clamp(0.0, 1.0);
        let bonuses: Vec<f64> = self
            .projected_ewar
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&target_ball_id)
            .map(|list| {
                list.iter()
                    .filter(|proj| {
                        self.sde
                            .types
                            .get(&proj.source_module_type_id)
                            .is_some_and(|t| t.group_id == 379)
                            || proj.effective_module.base_attributes.contains_key(&554)
                    })
                    .map(|proj| {
                        let opt = proj
                            .effective_module
                            .get_attribute(eve_sde::attr_ids::MAX_RANGE, &self.sde);
                        let fall = eve_dogma::get_module_falloff(&proj.effective_module, &self.sde);
                        let att =
                            eve_dogma::compute_falloff_attenuation(proj.distance, opt, fall);
                        proj.effective_module.get_attribute(554, &self.sde) * att * tp_resist
                    })
                    .collect()
            })
            .unwrap_or_default();
        if bonuses.is_empty() {
            1.0
        } else {
            eve_dogma::eval_target_painters_sig_radius(1.0, &bonuses)
        }
    }

    // Covers: ship boarding skill and clone checks (`feat:ships.board-ship-skill-and-clone-check`)
    pub fn check_type_skill_prerequisites_in_snapshot(
        &self,
        snap: &eve_db::WorldStateSnapshot,
        char_id: i64,
        type_id: u32,
    ) -> Result<(), BridgeRpcError> {
        let Some(t) = self.sde.types.get(&type_id) else {
            return Ok(());
        };
        let char_skills = snap.skills.get(&char_id);
        let char_rec = snap.characters.get(&char_id);
        let is_omega = char_rec.is_some_and(|c| {
            let exp_opt = crate::services::account_services::subscription_expiry_for_user(snap, c.user_id);
            c.clone_grade >= 2 && exp_opt.map(|u| u > self.now_unix()).unwrap_or(true)
        });
        let is_alpha_clone = !is_omega;
        let race_id = char_rec.map(|c| c.race_id).unwrap_or(1);
        let req_pairs = [
            (182_u16, 277_u16),
            (183, 278),
            (184, 279),
            (1285, 1286),
            (1289, 1287),
            (1290, 1288),
        ];
        for (req_skill_attr, req_lvl_attr) in req_pairs {
            if let Some(&skill_f) = t.default_attributes.get(&req_skill_attr) {
                let skill_id = skill_f.round() as u32;
                if skill_id > 0 {
                    let req_lvl = t
                        .default_attributes
                        .get(&req_lvl_attr)
                        .copied()
                        .unwrap_or(1.0)
                        .round()
                        .clamp(1.0, 5.0) as u8;
                    let mut raw_trained = char_skills
                        .and_then(|m| m.get(&skill_id))
                        .map(|r| r.level)
                        .unwrap_or(0);
                    if raw_trained < req_lvl && (12180..=12199).contains(&skill_id) && skill_id != 12189 {
                        for alt_skill in [60377_u32, 60378, 60379, 60380, 60381] {
                            let alt_lvl = char_skills
                                .and_then(|m| m.get(&alt_skill))
                                .map(|r| r.level)
                                .unwrap_or(0);
                            raw_trained = raw_trained.max(alt_lvl);
                        }
                    }
                    if raw_trained < req_lvl {
                        let is_ship = t.category_id == 6;
                        let code = if is_ship { "ShipHasSkillPrerequisites" } else { "SkillPrerequisitesNotMet" };
                        return Err(BridgeRpcError::user_error(code));
                    }
                    if is_alpha_clone {
                        let alpha_cap =
                            crate::services::character_skills::alpha_skill_cap_for_character(
                                &self.sde, race_id, skill_id,
                            );
                        let effective_trained = raw_trained.min(alpha_cap);
                        if req_lvl > alpha_cap || effective_trained < req_lvl {
                            let is_ship = t.category_id == 6;
                            let code = if is_ship { "TrialAccountRestriction" } else { "SkillPrerequisitesNotMet" };
                            return Err(BridgeRpcError::user_error(code));
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

pub use crate::router::binding_effects::{
    effect_id_for_module_sde, special_fx_guid_for_module_sde,
};

#[cfg(test)] #[path = "binding_unit_tests.rs"] mod binding_unit_tests;
