//! Authenticated RPC and Quasar protobuf message dispatching.

use crate::router::context::ServerContext;
use crate::services;
use crate::warn_limiter::{check_global_warn, WarnCategory};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;
use tracing::{info, warn};

/// Slow-work warning threshold (ms) for RPC handlers and `after_call` hooks (`#1087`).
pub const SLOW_RPC_THRESHOLD_MS: u64 = 100;

tokio::task_local! {
    static ACTIVE_RPC_CALL_ID: u64;
}

tokio::task_local! {
    static SUPPRESS_QUASAR_SESSION_CHANGE: bool;
}

/// Runs `fut` with `call_id` bound for `rpc.slow` diagnostics.
pub async fn with_active_rpc_call_id<F, R>(call_id: u64, fut: F) -> R
where
    F: std::future::Future<Output = R>,
{
    ACTIVE_RPC_CALL_ID.scope(call_id, Box::pin(fut)).await
}

/// Runs `fut` with `quasar_gateway.on_session_changed` suppressed inside `dispatch_rpc_inner`.
///
/// Used by `SafeLogoffTeardownGrace` read-only RPC dispatch (`#1434`) so a temporary cloned
/// `SessionState` with `character_id = Some(grace_char_id)` never transiently re-attaches
/// character tenancy on the live charsel `GatewaySubscriber` or drains `NoticeRoutingCache`.
pub async fn with_quasar_session_change_suppressed<F, R>(fut: F) -> R
where
    F: std::future::Future<Output = R>,
{
    SUPPRESS_QUASAR_SESSION_CHANGE
        .scope(true, Box::pin(fut))
        .await
}

#[inline]
fn current_rpc_call_id() -> u64 {
    ACTIVE_RPC_CALL_ID.try_with(|id| *id).unwrap_or(0)
}

#[inline]
fn is_quasar_session_change_suppressed() -> bool {
    SUPPRESS_QUASAR_SESSION_CHANGE
        .try_with(|suppressed| *suppressed)
        .unwrap_or(false)
}

#[inline(never)]
pub fn box_fut<'a, F, Fut, T>(
    f: F,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = T> + Send + 'a,
{
    Box::pin(f())
}

#[inline(never)]
fn dispatch_rpc_boxed<'a>(
    ctx: &'a ServerContext,
    session: &'a mut SessionState,
    service: &'a str,
    bound_id: Option<u64>,
    method: &'a str,
    args: &'a [PyRep],
    kwargs: &'a HashMap<String, PyRep>,
    caller_trace: Option<&'a str>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<PyRep, BridgeRpcError>> + Send + 'a>> {
    Box::pin(ctx.dispatch_rpc(session, service, bound_id, method, args, kwargs, caller_trace))
}

#[inline(never)]
fn dispatch_rpc_inner_boxed<'a>(
    ctx: &'a ServerContext,
    session: &'a mut SessionState,
    service: &'a str,
    bound_id: Option<u64>,
    method: &'a str,
    args: &'a [PyRep],
    kwargs: &'a HashMap<String, PyRep>,
    caller_trace: Option<&'a str>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<PyRep, BridgeRpcError>> + Send + 'a>> {
    Box::pin(ctx.dispatch_rpc_inner(session, service, bound_id, method, args, kwargs, caller_trace))
}

struct CatchUnwindFuture<F>(F);

impl<F: std::future::Future + Unpin> std::future::Future for CatchUnwindFuture<F> {
    type Output = Result<F::Output, Box<dyn std::any::Any + Send>>;
    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let inner = std::pin::Pin::into_inner(self);
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            std::pin::Pin::new(&mut inner.0).poll(cx)
        })) {
            Ok(std::task::Poll::Ready(v)) => std::task::Poll::Ready(Ok(v)),
            Ok(std::task::Poll::Pending) => std::task::Poll::Pending,
            Err(e) => std::task::Poll::Ready(Err(e)),
        }
    }
}

fn format_panic_payload(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unexpected panic in RPC handler".to_string()
    }
}

fn append_unimplemented_rpc_log(
    session_id: u64,
    service: &str,
    method: &str,
    arg_count: usize,
    kwarg_keys: Vec<&str>,
    caller_trace: Option<&str>,
) {
    use std::io::Write;
    let _ = std::fs::create_dir_all("logs");
    let path = "logs/unimplemented_rpcs.jsonl";
    if let Ok(meta) = std::fs::metadata(path) {
        if meta.len() >= MAX_UNIMPLEMENTED_LOG_BYTES {
            return;
        }
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let sanitized_trace: String = caller_trace
            .unwrap_or("unknown")
            .chars()
            .filter(|c| !c.is_control())
            .take(256)
            .collect();
        let record = serde_json::json!({
            "session_id": session_id,
            "service": service,
            "method": method,
            "arg_count": arg_count,
            "kwarg_keys": kwarg_keys,
            "caller_trace": sanitized_trace,
        });
        let _ = writeln!(file, "{record}");
    }
}

fn format_pyrep_summary(rep: &PyRep) -> String {
    match rep {
        PyRep::None => "None".to_string(),
        PyRep::Bool(b) => format!("Bool({b})"),
        PyRep::Integer(i) => format!("Int({i})"),
        PyRep::Real(f) => format!("Real({f:.2})"),
        PyRep::String(s) => format!("Str[{}]", s.len()),
        PyRep::List(l) => format!("List[{}]", l.len()),
        PyRep::Tuple(t) => format!("Tuple[{}]", t.len()),
        PyRep::Dict(d) => format!("Dict[{}]", d.len()),
        PyRep::Object { type_name, .. } => format!("Obj({type_name})"),
        _ => "PyRep".to_string(),
    }
}

tokio::task_local! {
    static DISPATCH_TX_ERROR: std::cell::RefCell<Option<(String, String)>>;
}

static TX_HOOK_ONCE: std::sync::Once = std::sync::Once::new();

fn dispatch_record_tx_error(err: &eve_db::TxError) {
    let (code, msg) = match err {
        eve_db::TxError::NonFinite { .. } | eve_db::TxError::RowLimit { .. } => {
            ("ValidationError".to_string(), err.to_string())
        }
        eve_db::TxError::Persist(_) | eve_db::TxError::Panicked { .. } => {
            ("PersistenceFailure".to_string(), err.to_string())
        }
    };
    let _ = DISPATCH_TX_ERROR.try_with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            *slot = Some((code, msg));
        }
    });
}

fn dispatch_check_tx_poison() -> Option<String> {
    DISPATCH_TX_ERROR
        .try_with(|cell| cell.borrow().as_ref().map(|(_, msg)| msg.clone()))
        .ok()
        .flatten()
}

pub(crate) fn ensure_dispatch_tx_hooks_installed() {
    TX_HOOK_ONCE.call_once(|| {
        eve_db::set_tx_error_hook(Some(dispatch_record_tx_error));
        eve_db::set_tx_poison_check_hook(Some(dispatch_check_tx_poison));
    });
}

async fn maybe_with_dispatch_tx_error<F, R>(outer: bool, fut: F) -> R
where
    F: std::future::Future<Output = R>,
{
    let fut = Box::pin(fut);
    if outer {
        DISPATCH_TX_ERROR
            .scope(std::cell::RefCell::new(None), fut)
            .await
    } else {
        fut.await
    }
}

async fn maybe_with_staged_dispatch_notifications<F, R>(
    outer: bool,
    pend: &dyn eve_db::lock_rank::AnyRwLock<
        HashMap<i64, Vec<crate::router::cache_notifications::PendingMachoNotification>>,
    >,
    notify_tx: &tokio::sync::broadcast::Sender<(
        i64,
        crate::router::cache_notifications::PendingMachoNotification,
    )>,
    fut: F,
) -> R
where
    F: std::future::Future<Output = R>,
{
    let fut = Box::pin(fut);
    if outer {
        crate::router::cache_notifications::with_staged_dispatch_notifications(
            pend, notify_tx, fut,
        )
        .await
    } else {
        fut.await
    }
}

impl ServerContext {
    fn parse_macho_bind_piggyback(
        arg: Option<&PyRep>,
    ) -> Result<Option<(&str, Vec<PyRep>, HashMap<String, PyRep>)>, BridgeRpcError> {
        let Some(PyRep::Tuple(call_tuple)) = arg else {
            return Ok(None);
        };
        if call_tuple.is_empty() {
            return Ok(None);
        }
        let sub_method = call_tuple.first().and_then(|m| m.as_str()).ok_or_else(|| {
            BridgeRpcError {
                code: "InvalidArgument".into(),
                message: "Piggybacked call in MachoBindObject requires string method name".into(),
                client_error: None,
            }
        })?;
        let sub_args: Vec<PyRep> = match call_tuple.get(1) {
            Some(PyRep::Tuple(items)) | Some(PyRep::List(items)) => items.clone(),
            _ => Vec::new(),
        };
        let mut sub_kwargs = HashMap::new();
        if let Some(PyRep::Dict(entries)) = call_tuple.get(2) {
            for (k, v) in entries {
                if let Some(ks) = k.as_str() {
                    sub_kwargs.insert(ks.to_owned(), v.clone());
                }
            }
        }
        Ok(Some((sub_method, sub_args, sub_kwargs)))
    }

    /// Dispatches an incoming RPC call `(service, bound_id, method, args, kwargs)` across the 12
    /// modular domain handlers, enforcing authentication and ownership checks.
    #[allow(clippy::too_many_arguments)]
    pub async fn dispatch_rpc(
        &self,
        session: &mut SessionState,
        service: &str,
        bound_id: Option<u64>,
        method: &str,
        args: &[PyRep],
        kwargs: &HashMap<String, PyRep>,
        caller_trace: Option<&str>,
    ) -> Result<PyRep, BridgeRpcError> {
        ensure_dispatch_tx_hooks_installed();

        let is_outer_tx_scope = DISPATCH_TX_ERROR.try_with(|_| ()).is_err();
        if is_outer_tx_scope {
            if let Err(rate_err) =
                self.enforce_session_rpc_rate_budget(session, service, bound_id, method)
            {
                return Err(rate_err);
            }
        }

        #[cfg(any(test, feature = "testkit"))]
        self.record_fixture_census_on_first_rpc(session, service, bound_id, method);

        let pre_sync_char_id = session.character_id;
        let pre_sync_loc = (
            session.solar_system_id,
            session.station_id,
            session.structure_id,
        );
        self.sync_session(session);
        let post_sync_loc = (
            session.solar_system_id,
            session.station_id,
            session.structure_id,
        );
        if pre_sync_loc.0.is_some() && post_sync_loc != pre_sync_loc {
            if let (Some(cid), Some(sys_id)) = (session.character_id, session.solar_system_id) {
                self.invalidate_bound_objects_for_session_transition(
                    cid,
                    sys_id,
                    session.station_id.or(session.structure_id),
                    "session_location_change",
                );
            }
        }
        if pre_sync_char_id.is_some() && session.character_id != pre_sync_char_id {
            self.purge_transition_grace_for_session(session.session_id);
        } else if pre_sync_char_id == session.character_id && session.character_id.is_some() {
            self.update_transition_departed_locations(
                session,
                pre_sync_loc.0,
                pre_sync_loc.1.or(pre_sync_loc.2),
                post_sync_loc.0,
                post_sync_loc.1.or(post_sync_loc.2),
            );
        }
        let pre_seq = session
            .character_id
            .map(|cid| self.latest_notification_seq_for_char(cid))
            .unwrap_or(0);
        let pre_session_notif_len = session.notifications.len();

        let prev_station_id = session.station_id;
        let initial_active_char_id = session.character_id;
        let pre_dispatch_user_id = session.user_id;
        let pre_dispatch_was_docked = session.station_id.is_some() || session.structure_id.is_some();
        let is_outer_notify_scope =
            !crate::router::cache_notifications::has_staged_dispatch_notifications_scope();
        let dispatch_service =
            bound_id.and_then(|bid| self.owned_bound_service_for_metrics(session, bid));
        let dispatch_rpc_label = crate::router::delta_item_compare::normalize_dispatch_method_label(
            dispatch_service.as_deref().unwrap_or(service),
            method,
        );
        let start_time = std::time::Instant::now();
        let (mut res, tx_poisoned) = maybe_with_dispatch_tx_error(
            is_outer_tx_scope,
            maybe_with_staged_dispatch_notifications(
                is_outer_notify_scope,
                &*self.pending_notifications,
                &self.notify_tx,
                crate::router::cache_notifications::with_active_dispatch_char(
                    initial_active_char_id,
                    Box::pin(async {
                        let _rpc_method_guard =
                            crate::router::delta_item_compare::push_dispatch_rpc_method_guard(
                                &*self.pending_notifications,
                                &self.notify_tx,
                                dispatch_rpc_label,
                            );
                        let mut res = match CatchUnwindFuture(dispatch_rpc_inner_boxed(
                            self,
                            session,
                            service,
                            bound_id,
                            method,
                            args,
                            kwargs,
                            caller_trace,
                        ))
                        .await
                        {
                            Ok(r) => r,
                            Err(panic_payload) => {
                                let panic_msg = format_panic_payload(panic_payload.as_ref());
                                warn!(
                                    session_id = session.session_id,
                                    service,
                                    method,
                                    panic_msg = %panic_msg,
                                    "Caught panic in RPC handler; converting to ErrorResponse (ORD-2)"
                                );
                                Err(BridgeRpcError {
                                    code: "InternalError".into(),
                                    message: format!(
                                        "RPC handler panicked in {service}.{method}: {panic_msg}"
                                    ),
                                    client_error: None,
                                })
                            }
                        };

                        if res.is_ok()
                            && dispatch_check_tx_poison().is_none()
                            && matches!(
                                service,
                                "stationSvc" | "ship" | "structureDocking" | "beyonce"
                            )
                        {
                            if let Some(cid) = session.character_id {
                                let guest_row = || {
                                    self.db.with_snapshot(|snap| {
                                        let c = snap.characters.get(&cid);
                                        PyRep::Tuple(vec![
                                            PyRep::Integer(cid),
                                            c.map(|c| PyRep::Integer(c.corporation_id))
                                                .unwrap_or(PyRep::None),
                                            c.and_then(|c| c.alliance_id)
                                                .map(PyRep::Integer)
                                                .unwrap_or(PyRep::None),
                                            c.and_then(|c| c.war_faction_id)
                                                .map(PyRep::Integer)
                                                .unwrap_or(PyRep::None),
                                        ])
                                    })
                                };
                                if method == "Undock" {
                                    if let Some(st_id) =
                                        prev_station_id.filter(|_| session.station_id.is_none())
                                    {
                                        let row = guest_row();
                                        for (peer_cid, _) in self.resolve_notification_recipients(
                                            &crate::router::NotificationTarget::Station(st_id),
                                        ) {
                                            if peer_cid != cid {
                                                self.notify(
                                                    crate::router::NotificationTarget::Character(
                                                        peer_cid,
                                                    ),
                                                    "OnCharNoLongerInStation",
                                                    "charid",
                                                    vec![row.clone()],
                                                );
                                            }
                                        }
                                    }
                                } else if matches!(method, "Dock" | "CmdDock") {
                                    if let Some(st_id) =
                                        session.station_id.filter(|_| prev_station_id.is_none())
                                    {
                                        let row = guest_row();
                                        for (peer_cid, _) in self.resolve_notification_recipients(
                                            &crate::router::NotificationTarget::Station(st_id),
                                        ) {
                                            if peer_cid != cid {
                                                self.notify(
                                                    crate::router::NotificationTarget::Character(
                                                        peer_cid,
                                                    ),
                                                    "OnCharNowInStation",
                                                    "charid",
                                                    vec![row.clone()],
                                                );
                                            }
                                        }
                                    }
                                    crate::services::fleet_social::execute_pending_auto_kicks_for_docked_char(
                                        self, session, cid,
                                    );
                                }
                            }
                        } else if res.is_ok()
                            && dispatch_check_tx_poison().is_none()
                            && matches!(method, "Dock" | "CmdDock")
                        {
                            if let Some(cid) = session.character_id {
                                crate::services::fleet_social::execute_pending_auto_kicks_for_docked_char(
                                    self, session, cid,
                                );
                            }
                        }

                        let mut tx_poisoned = false;
                        if is_outer_tx_scope {
                            if let Some((code, msg)) = DISPATCH_TX_ERROR
                                .try_with(|cell| cell.borrow_mut().take())
                                .ok()
                                .flatten()
                            {
                                tx_poisoned = true;
                                let preserve_existing_err = matches!(
                                    &res,
                                    Err(e) if matches!(
                                        e.code.as_str(),
                                        "PersistenceFailure"
                                            | "DatabasePersistenceError"
                                            | "ValidationError"
                                    )
                                );
                                if !preserve_existing_err {
                                    res = Err(BridgeRpcError {
                                        code,
                                        message: msg,
                                        client_error: None,
                                    });
                                }
                            }
                        }
                        if matches!(
                            &res,
                            Err(e) if matches!(
                                e.code.as_str(),
                                "PersistenceFailure"
                                    | "DatabasePersistenceError"
                                    | "ValidationError"
                                    | "InternalError"
                            )
                        ) {
                            tx_poisoned = true;
                        }

                        if tx_poisoned {
                            session.notifications.truncate(pre_session_notif_len);
                        } else if is_outer_notify_scope {
                            crate::router::cache_notifications::set_active_dispatch_char(
                                session.character_id,
                            );
                            crate::router::cache_notifications::flush_staged_dispatch_notifications(
                                &*self.pending_notifications,
                                &self.notify_tx,
                            );
                        }

                        (res, tx_poisoned)
                    }),
                ),
            ),
        )
        .await;
        let duration = start_time.elapsed();
        let resolved_service =
            bound_id.and_then(|bid| self.owned_bound_service_for_metrics(session, bid));
        let metric_service = resolved_service.as_deref().unwrap_or(service);
        let genuine_bootstrap_transition = match (metric_service, method) {
            ("authentication", "Login") => {
                session.user_id.is_some() && pre_dispatch_user_id != session.user_id
            }
            ("charUnboundMgr", "SelectCharacterID") => {
                session.character_id.is_some() && initial_active_char_id != session.character_id
            }
            ("stationSvc", "Undock") | ("structureDocking", "Undock") | ("ship", "Undock") => {
                pre_dispatch_was_docked
                    && session.station_id.is_none()
                    && session.structure_id.is_none()
            }
            _ => false,
        };
        if is_outer_tx_scope && !tx_poisoned && res.is_ok() && genuine_bootstrap_transition {
            self.grant_session_bootstrap_burst(session.session_id);
        }
        let track_individual =
            self.should_track_rpc_latency_individually(session, metric_service, method, &res);
        self.flight_recorder.record_rpc_latency_classified(
            metric_service,
            method,
            track_individual,
            duration,
        );
        if duration > std::time::Duration::from_millis(crate::flight_recorder::PACKET_PERF_WARN_THRESHOLD_MS)
        {
            let arg_size = crate::flight_recorder::encoded_args_byte_size(args, kwargs);
            let (perf_service, perf_method) = if track_individual {
                (metric_service, method)
            } else {
                (
                    crate::flight_recorder::UNKNOWN_RPC_LABEL,
                    crate::flight_recorder::UNKNOWN_RPC_LABEL,
                )
            };
            let is_bootstrap = matches!(
                (perf_service, perf_method),
                ("charUnboundMgr", "SelectCharacterID")
                    | ("charUnboundMgr", "GetCharacterInfo")
                    | ("character", "GetCharacterSelectionInfo")
                    | ("stationSvc", "Undock")
                    | ("structureDocking", "Undock")
                    | ("ship", "Undock")
                    | ("beyonce", "MachoBindObject")
                    | ("beyonce", "GetFormations")
                    | ("beyonce", "UpdateStateRequest")
            );
            tracing::warn!(
                target: "packet_perf",
                service = %perf_service,
                method = %perf_method,
                arg_size,
                duration_ms = duration.as_millis() as u64,
                session_id = session.session_id,
                bootstrap = is_bootstrap,
                "[PacketPerf] slow RPC handler"
            );
            self.flight_recorder.record_packet_perf_warning(
                session.session_id,
                perf_service,
                perf_method,
                arg_size,
                duration,
                is_bootstrap,
            );
        }
        let call_id = current_rpc_call_id();
        let handler_ms = duration.as_millis() as u64;
        if handler_ms >= SLOW_RPC_THRESHOLD_MS {
            warn!(
                target: "rpc.slow",
                service,
                method,
                call_id,
                ms = handler_ms,
                "RPC handler exceeded slow-work threshold"
            );
        }

        let status_desc = match &res {
            Ok(rep) => format!("Ok({})", format_pyrep_summary(rep)),
            Err(e) => format!("Err({}: {})", e.code, e.message),
        };
        self.flight_recorder.record(
            session.session_id,
            service,
            bound_id,
            method,
            args,
            kwargs,
            duration,
            &status_desc,
        );

        let after_call_start = std::time::Instant::now();

        if !tx_poisoned {
            if initial_active_char_id.is_some() && session.character_id != initial_active_char_id {
                self.purge_transition_grace_for_session(session.session_id);
            }
            if let Some(cid) = session.character_id {
                let post_rpc_loc = (
                    session.solar_system_id,
                    session.station_id,
                    session.structure_id,
                );
                if res.is_ok() && session.character_id == initial_active_char_id {
                    self.update_transition_departed_locations(
                        session,
                        post_sync_loc.0,
                        post_sync_loc.1.or(post_sync_loc.2),
                        post_rpc_loc.0,
                        post_rpc_loc.1.or(post_rpc_loc.2),
                    );
                }
                let is_undock_rpc = matches!(
                    (metric_service, method),
                    ("stationSvc" | "structureDocking" | "ship", "Undock")
                );
                if res.is_ok()
                    && !is_undock_rpc
                    && post_sync_loc.0.is_some()
                    && post_rpc_loc != post_sync_loc
                {
                    if let Some(sys_id) = session.solar_system_id {
                        self.invalidate_bound_objects_for_session_transition(
                            cid,
                            sys_id,
                            session.station_id.or(session.structure_id),
                            "session_location_change",
                        );
                    }
                }
                if !session.live_notify_listener {
                    if let Some(notifs) = self.pending_notifications.read().ok().and_then(|q| {
                        q.get(&cid).map(|v| {
                            v.iter()
                                .filter(|n| n.seq_id > pre_seq)
                                .cloned()
                                .collect::<Vec<_>>()
                        })
                    }) {
                        for notif in notifs {
                            session.push_notification(notif.event, notif.args);
                        }
                    }
                }
            }
        }

        if caller_trace == Some("machoNet:26000") {
            let effective_service = bound_id
                .and_then(|bid| self.owned_bound_service_for_metrics(session, bid))
                .unwrap_or_else(|| service.to_string());
            if effective_service == "scanMgr" && method == "GetSystemScanMgr" {
                if let Ok(ref rep) = res {
                    if let Some(bid) = rep.dict_get("boundID").and_then(|v| v.as_i64()) {
                        res = Ok(self.bound_object_rep(bid as u64, caller_trace));
                    }
                }
            }
            if effective_service == "fleetObjectHandler" && method == "CreateFleet" {
                if let Ok(PyRep::Integer(fleet_id)) = res {
                    res = self
                        .bind_verified_sub_object(session, "fleetObjectHandler", fleet_id)
                        .map(|bid| self.bound_object_rep(bid, caller_trace));
                }
            }
            res = crate::router::client_compat::normalize_macho_rpc_reply_at(
                &effective_service,
                method,
                res,
                Some(self.now_unix()),
            );
            if effective_service == "marketProxy"
                && matches!(method, "GetOrders" | "GetPlexOrders")
            {
                if let Ok(rep) = res {
                    let already_cached = matches!(
                        &rep,
                        PyRep::Object { type_name, .. }
                            if type_name == eve_proto::marshal::V24_CACHED_METHOD_CALL_RESULT_CLASS
                    );
                    res = Ok(if already_cached {
                        rep
                    } else {
                        self.cache_method_call_result("marketProxy", method, rep, "always", None)
                    });
                }
            } else if effective_service == "agentMgr"
                && method == "GetMessagesForEpicArcMissions"
            {
                if let Ok(rep) = res {
                    let already_cached = matches!(
                        &rep,
                        PyRep::Object { type_name, .. }
                            if type_name == eve_proto::marshal::V24_CACHED_METHOD_CALL_RESULT_CLASS
                    );
                    res = Ok(if already_cached {
                        rep
                    } else {
                        self.cache_method_call_result("agentMgr", method, rep, "never", None)
                    });
                }
            }
        }

        let after_call_ms = after_call_start.elapsed().as_millis() as u64;
        if after_call_ms >= SLOW_RPC_THRESHOLD_MS {
            warn!(
                target: "rpc.slow",
                service,
                method,
                call_id,
                hook = "after_call",
                ms = after_call_ms,
                "RPC after_call hooks exceeded slow-work threshold"
            );
        }

        res
    }

    /// Internal, test/admin, and Quasar gateway services implemented outside `rpc_service_catalog.json`.
    const INTERNAL_LATENCY_TRACKED_SERVICES: &'static [&'static str] = &[
        "cluster",
        "contractMgr",
        "fleetSvc",
        "harness",
        "incursion",
        "officeManager",
        "ownerGroupManager",
        "publicGatewaySvc",
        "scanSvc",
        "sovHubSvc",
        "structureProfiles",
        "timeControl",
    ];

    pub(crate) fn owned_bound_service_for_metrics(
        &self,
        session: &SessionState,
        bound_id: u64,
    ) -> Option<String> {
        self.bound_objects
            .read()
            .ok()
            .and_then(|m| {
                m.get(&bound_id)
                    .filter(|e| {
                        e.session_id == session.session_id && Some(e.user_id) == session.user_id
                    })
                    .map(|e| e.service.clone())
            })
            .or_else(|| self.transition_grace_service_for_bound_id(session, bound_id))
    }

    pub(crate) fn has_bound_service(&self, session_id: u64, service: &str) -> bool {
        self.bound_objects
            .read()
            .ok()
            .is_some_and(|m| {
                m.values()
                    .any(|e| e.session_id == session_id && e.service == service)
            })
    }

    fn is_trackable_latency_service(&self, effective_service: &str) -> bool {
        self.cataloged_services.contains(effective_service)
            || Self::INTERNAL_LATENCY_TRACKED_SERVICES.contains(&effective_service)
    }

    fn is_preauth_individual_rpc(effective_service: &str, method: &str) -> bool {
        matches!(
            (effective_service, method),
            ("authentication", "Login" | "Logout" | "Ping")
                | ("machoNet", "GetTime" | "GetServiceInfo" | "NoBlock")
                | ("cache", "GetServerVersionAndBuild" | "Rowset" | "Setting")
        )
    }

    fn is_genuine_preauth_rpc(effective_service: &str, method: &str) -> bool {
        Self::is_preauth_individual_rpc(effective_service, method)
            || matches!(
                (effective_service, method),
                (
                    "machoNet",
                    "ClientHasReleasedTheseObjects" | "GetGlobalConfig"
                )
            )
    }

    fn extract_moniker_object_id(args: &[PyRep], rpc_name: &str) -> Result<i64, BridgeRpcError> {
        match args.first() {
            Some(PyRep::Integer(id)) => Ok(*id),
            Some(PyRep::String(s)) | Some(PyRep::Token(s)) if !s.is_empty() => Ok(s
                .bytes()
                .fold(1_000i64, |acc, b| {
                    acc.wrapping_mul(31).wrapping_add(b as i64)
                })
                .saturating_abs()
                .max(1)),
            Some(PyRep::Tuple(items)) => {
                if let Some(id) = items.first().and_then(|v| v.as_i64()) {
                    Ok(id)
                } else if let Some(s) = items
                    .first()
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                {
                    Ok(s.bytes()
                        .fold(1_000i64, |acc, b| {
                            acc.wrapping_mul(31).wrapping_add(b as i64)
                        })
                        .saturating_abs()
                        .max(1))
                } else {
                    Err(BridgeRpcError {
                        code: "InvalidArgument".into(),
                        message: format!("{rpc_name} bind tuple missing objectID"),
                        client_error: None,
                    })
                }
            }
            _ => Err(BridgeRpcError {
                code: "InvalidArgument".into(),
                message: format!(
                    "{rpc_name} requires an explicit integer or string objectID"
                ),
                client_error: None,
            }),
        }
    }

    /// Returns `true` for bootstrap RPCs expected during login/character-select/undock bursts.
    pub(crate) fn is_bootstrap_burst_rpc(service: &str, method: &str) -> bool {
        matches!(
            (service, method),
            ("authentication", "Logout" | "Ping")
                | (
                    "machoNet",
                    "GetTime"
                        | "GetGlobalConfig"
                        | "GetServiceInfo"
                        | "NoBlock"
                        | "ClientHasReleasedTheseObjects"
                        | "MachoBindObject"
                )
                | ("cache", "Rowset" | "Setting" | "GetServerVersionAndBuild")
                | ("objectCaching", "GetCachableObject")
                | ("charUnboundMgr", "GetCharactersToSelect" | "GetCharacterInfo")
                | (
                    "character",
                    "GetCharacterSelectionInfo" | "GetCharacterToSelect"
                )
                | (
                    "config",
                    "GetBlackListedPlanets"
                        | "GetOldStationData"
                        | "GetMultiOwnersEx"
                        | "GetLocationsEx"
                        | "GetMultiLocationsEx"
                        | "GetMultiAllianceShortNamesEx"
                        | "GetMultiCorpTickerNamesEx"
                        | "GetStationSolarSystemsByOwner"
                )
                | (
                    "stationSvc",
                    "Undock" | "GetStation" | "GetSolarSystem" | "GetGuests"
                )
                | ("structureDocking", "Undock")
                | (
                    "beyonce",
                    "MachoBindObject" | "GetFormations" | "UpdateStateRequest"
                )
                | (
                    "dogmaIM",
                    "MachoBindObject" | "GetAllInfo" | "GetDynamicItemAttributes"
                )
                | (
                    "skillMgr2",
                    "MachoBindObject" | "GetSkills" | "GetSkillQueueAndFreePoints"
                )
                | ("invbroker", "MachoBindObject" | "List")
                | (
                    "ship",
                    "Undock" | "MachoBindObject" | "GetShipConfiguration"
                )
                | ("publicGatewaySvc", "MachoBindObject")
        )
    }

    fn enforce_session_rpc_rate_budget(
        &self,
        session: &SessionState,
        service: &str,
        bound_id: Option<u64>,
        method: &str,
    ) -> Result<(), BridgeRpcError> {
        if self.is_gm_ops_enabled() && session.has_gm_role() {
            return Ok(());
        }
        let resolved_service =
            bound_id.and_then(|bid| self.owned_bound_service_for_metrics(session, bid));
        let effective_service = resolved_service.as_deref().unwrap_or(service);
        let is_bootstrap = Self::is_bootstrap_burst_rpc(effective_service, method);
        if self.try_consume_rpc_token_at(
            session.session_id,
            std::time::Instant::now(),
            is_bootstrap,
        ) {
            return Ok(());
        }
        self.flight_recorder.record_rpc_latency_classified(
            effective_service,
            method,
            false,
            std::time::Duration::ZERO,
        );
        Err(BridgeRpcError {
            code: "RateLimited".into(),
            message: format!(
                "Per-session RPC rate budget exceeded for {effective_service}.{method}"
            ),
            client_error: None,
        })
    }

    /// Returns `true` only when `(effective_service, method)` represents a matched/known RPC
    /// eligible for its own latency bucket rather than collapsing into `<unknown>`.
    fn should_track_rpc_latency_individually(
        &self,
        session: &SessionState,
        effective_service: &str,
        method: &str,
        res: &Result<PyRep, BridgeRpcError>,
    ) -> bool {
        if let Err(err) = res {
            if matches!(
                err.code.as_str(),
                "UnimplementedRPC"
                    | "RateLimited"
                    | "Unauthorized"
                    | "AuthenticationRequired"
                    | "AuthenticationFailed"
                    | "InvalidLoginArguments"
                    | "CharacterNotSelected"
                    | "InsufficientRole"
                    | "ClusterShuttingDown"
                    | "UserBanned"
                    | "AccountSuspended"
                    | "MultiLoginBlocked"
                    | "SessionKicked"
                    | "InvalidBoundObject"
                    | "BindAuthorizationDenied"
                    | super::binding::ERR_BOUND_OWNERSHIP
                    | "FeatureDisabled"
                    | "UnknownQuasarDomain"
            ) {
                return false;
            }
        }
        if !self.is_trackable_latency_service(effective_service) {
            return false;
        }
        if !session.is_authenticated() {
            return res.is_ok() && Self::is_preauth_individual_rpc(effective_service, method);
        }
        if method == "MachoBindObject" {
            return res.is_ok();
        }
        true
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn dispatch_rpc_inner(
        &self,
        session: &mut SessionState,
        service: &str,
        bound_id: Option<u64>,
        method: &str,
        args: &[PyRep],
        kwargs: &HashMap<String, PyRep>,
        caller_trace: Option<&str>,
    ) -> Result<PyRep, BridgeRpcError> {
        info!(
            session_id = session.session_id,
            user_id = ?session.user_id,
            char_id = ?session.character_id,
            service,
            ?bound_id,
            method,
            "Dispatching RPC call"
        );

        // Enforce session revocation (ban, kick, or multi-login displacement) on EVERY call
        // except un-bound authentication.Login/Logout/Ping (`R35-P1-3`, `R36-P1-3`).
        let is_exempt_auth_call = bound_id.is_none()
            && service == "authentication"
            && matches!(method, "Login" | "Logout" | "Ping");
        let is_multi_login_blocker_call =
            bound_id.is_none() && service == "multiLoginBlocker" && method == "Login";
        let is_exempt_petitioner_call =
            bound_id.is_none() && service == "petitioner" && method == "MayPetition";

        if !is_exempt_auth_call
            && !is_exempt_petitioner_call
            && !session.has_gm_role()
            && (self.is_shutting_down()
                || crate::services::serverops_cluster::is_downtime_active_in_db(self))
        {
            self.release_session_bindings(session.session_id);
            self.clear_pending_structure_view_outside_set_state_for_session(session.session_id);
            self.quasar_gateway.unsubscribe_session(session.session_id);
            if let Some(uid) = session.user_id {
                let _ = self.db.release_online_lock(uid, session.session_id);
            }
            if let Some(cid) = session.character_id {
                self.logoff_character_sync(cid, Some(session));
            }
            self.disconnect_session(
                session.session_id,
                "ClusterShuttingDown",
                "Cluster is undergoing scheduled daily downtime",
            );
            session.user_id = None;
            session.role = 0;
            session.character_id = None;
            session.corporation_id = None;
            session.alliance_id = None;
            crate::router::clear_session_character_state(session);
            return Err(BridgeRpcError {
                code: "ClusterShuttingDown".into(),
                message: "Cluster is undergoing scheduled daily downtime; non-GM gameplay RPCs are rejected".into(),
                client_error: None,
            });
        }

        if !is_exempt_auth_call && !is_exempt_petitioner_call {
            if let Some(uid) = session.user_id {
                if let Some((code, msg)) =
                    self.db
                        .check_session_revoked(uid, session.session_id, self.now_unix())
                {
                    self.release_session_bindings(session.session_id);
                    self.clear_pending_structure_view_outside_set_state_for_session(
                        session.session_id,
                    );
                    self.quasar_gateway.unsubscribe_session(session.session_id);
                    if code == "MultiLoginBlocked" {
                        if !is_multi_login_blocker_call {
                            return Err(BridgeRpcError { code, message: msg, client_error: None });
                        }
                    } else {
                        let _ = self.db.release_online_lock(uid, session.session_id);
                        if let Some(cid) = session.character_id {
                            if !self.is_character_online(cid, uid)
                                && !crate::sp6_lifecycle::has_active_disconnected_ship(
                                    &self.db, cid,
                                )
                            {
                                self.logoff_character_sync(cid, Some(session));
                            } else {
                                crate::quasar_gateway::isolate_side_effect(
                                    "dispatch.disconnect.pause_milestone_playtime",
                                    || {
                                        services::campaigns_and_milestones::playtime::pause_milestone_playtime(
                                            &self.db,
                                            cid,
                                            self.now_unix(),
                                        );
                                    },
                                );
                            }
                        }
                        session.user_id = None;
                        session.role = 0;
                        session.character_id = None;
                        session.corporation_id = None;
                        session.alliance_id = None;
                        crate::router::clear_session_character_state(session);
                        return Err(BridgeRpcError { code, message: msg, client_error: None });
                    }
                }
            }
        }

        if method == "ClientHasReleasedTheseObjects"
            && bound_id.is_none()
            && (service == "machoNet" || session.user_id.is_some())
        {
            let had_beyonce = self.has_bound_service(session.session_id, "beyonce");
            self.release_bound_objects_from_pyrep(session.session_id, &PyRep::List(args.to_vec()));
            if had_beyonce
                && !self.has_bound_service(session.session_id, "beyonce")
                && session.character_id.is_some()
                && session.station_id.is_none()
                && session.structure_id.is_some()
            {
                if let Some(cid) = session.character_id {
                    self.clear_space_ballpark_bootstrapped(cid);
                    crate::services::space_destiny::reset_update_state_rate_limit(
                        self,
                        session.session_id,
                        cid,
                    );
                }
            }
            return Ok(PyRep::None);
        }

        // Synchronize active character's ship/station/structure/solar-system state from DB
        if let Some(cid) = session.character_id {
            crate::services::fleet_social::fleet_social_notify::sync_session_fleet(self, session, cid);
            if let Ok(ch) = self.db.get_character(cid) {
                crate::router::apply_session_from_character(session, &ch, None, Some(&self.sde));
            }
        }
        let now_unix = self.now_unix();
        if !is_quasar_session_change_suppressed() {
            let _ = self.quasar_gateway.on_session_changed(session, now_unix);
        }

        let bound_entry = if let Some(bid) = bound_id {
            match self.verify_bound_object(session, bid) {
                Ok(entry) => Some(entry),
                Err(err) if err.code == "InvalidBoundObject" => {
                    match self.resolve_late_call_with_policy(session, bid, method, args, kwargs) {
                        crate::router::binding::LateCallResolution::DispatchWithBinding(entry) => {
                            tracing::debug!(
                                session_id = session.session_id,
                                char_id = ?session.character_id,
                                bound_id = bid,
                                object_id = entry.object_id,
                                service = %entry.service,
                                method,
                                "Dispatching late RPC via scene-epoch policy (#1146, #1150, #1190)"
                            );
                            Some(entry)
                        }
                        crate::router::binding::LateCallResolution::RespondImmediate(rep) => {
                            return Ok(rep);
                        }
                        crate::router::binding::LateCallResolution::RejectStale => return Err(err),
                    }
                }
                Err(err) => return Err(err),
            }
        } else {
            None
        };

        let effective_service = bound_entry
            .as_ref()
            .map(|b| b.service.as_str())
            .unwrap_or(service);

        let (enable_living_world, enable_quasar_gateway) = {
            let flags = self.feature_flags.read().unwrap_or_else(|e| e.into_inner());
            (flags.enable_living_world, flags.enable_quasar_gateway)
        };
        if effective_service == "taleMgr" && !enable_living_world {
            return Err(BridgeRpcError {
                // rpc-lint: allow "FeatureDisabled" is an error code, not a taleMgr method
                code: "FeatureDisabled".into(),
                message: "Living World Director (taleMgr) is disabled in evomu.toml feature_flags"
                    .into(),
                client_error: None,
            });
        }

        if effective_service == "publicGatewaySvc" && !enable_quasar_gateway {
            return Err(BridgeRpcError {
                // rpc-lint: allow "FeatureDisabled" is an error code, not a publicGatewaySvc method
                code: "FeatureDisabled".into(),
                message: "Quasar publicGatewaySvc is disabled in evomu.toml feature_flags".into(),
                client_error: None,
            });
        }

        // Framed Quasar gateway requests carry wire-encoded protobuf envelopes that return
        // structured EvePublicResponse STATUS_UNAUTHORIZED (401) rather than MachoNet exceptions;
        // empty/non-framed calls and notice/event RPCs remain hard-gated by require_auth.
        let is_framed_quasar_request = effective_service == "publicGatewaySvc"
            && matches!(method, "SendRequest" | "Send")
            && matches!(
                args.iter().next().or_else(|| kwargs.get("request")),
                Some(PyRep::Bytes(_) | PyRep::Buffer(_))
            );
        if session.user_id.is_none()
            && !Self::is_genuine_preauth_rpc(effective_service, method)
            && !is_framed_quasar_request
        {
            let _ = Self::require_auth(session)?;
        }

        if effective_service == "wormholeMgr" && method == "GetWormholePolarization" {
            if let Some(res) = services::wormhole_lifecycle_ops::resolve_cross_system_wormhole_polarization(self, session, args) {
                return res;
            }
        }

        if effective_service == "timeControl" && method == "AdvanceClock" {
            let _ = Self::require_auth(session)?;
            if !session.has_gm_role() {
                return Err(BridgeRpcError {
                    code: "InsufficientRole".into(),
                    message: format!(
                        "UserError: GM_InsufficientRole: {effective_service}.{method} requires a V24.01 GM/Admin service role bitmask"
                    ),
                    client_error: None,
                });
            }
            let secs = match args.first() {
                Some(PyRep::Integer(i)) if *i < 0 => {
                    return Err(BridgeRpcError {
                        code: "InvalidArgument".into(),
                        message: "AdvanceClock seconds must be non-negative".into(),
                        client_error: None,
                    });
                }
                Some(PyRep::Integer(i)) => *i as u64,
                _ => 12,
            };
            let explicit_target_cid = match args.get(1) {
                None | Some(PyRep::None) => None,
                Some(PyRep::Integer(id)) if *id > 0 => Some(*id),
                _ => {
                    return Err(BridgeRpcError {
                        code: "InvalidArgument".into(),
                        message: "AdvanceClock target_char_id must be a positive integer".into(),
                        client_error: None,
                    });
                }
            };
            let target_space = if let Some(target_cid) = explicit_target_cid {
                let resolved = self.db.with_snapshot(|snap| {
                    snap.characters.get(&target_cid).map(|ch| {
                        if ch.station_id.is_none()
                            && ch.structure_id.is_none()
                            && ch.solar_system_id > 0
                            && ch.active_ship_id > 0
                        {
                            Some((ch.solar_system_id, ch.active_ship_id))
                        } else {
                            None
                        }
                    })
                });
                resolved.ok_or_else(|| BridgeRpcError {
                    code: "CharacterNotFound".into(),
                    message: format!("Character {target_cid} not found"),
                    client_error: None,
                })?
            } else {
                match (session.solar_system_id, session.ship_id) {
                    (Some(sys_id), Some(ship_id)) => Some((sys_id, ship_id)),
                    _ => None,
                }
            };
            self.advance_secs_without_sim_clock_rebase(secs);
            if secs > 0 {
                let target_cid = explicit_target_cid.or(session.character_id).unwrap_or(0);
                crate::services::notify_emitters_character_services::fast_forward_and_sync_space_clock(
                    self,
                    target_cid,
                    target_space,
                    secs,
                )
                .await;
            }
            return Ok(PyRep::Bool(true));
        }

        if (effective_service == "timeControl" || effective_service == "harness")
            && (method == "FastForwardSessionTimer" || method == "ClearSessionTimer")
        {
            let _ = Self::require_auth(session)?;
            if !session.has_gm_role() {
                return Err(BridgeRpcError {
                    code: "InsufficientRole".into(),
                    message: format!(
                        "UserError: GM_InsufficientRole: {effective_service}.{method} requires a V24.01 GM/Admin service role bitmask"
                    ),
                    client_error: None,
                });
            }
            let target_cid = match args.first() {
                None | Some(PyRep::None) => session.character_id,
                Some(PyRep::Integer(id)) if *id > 0 => {
                    let cid = *id;
                    if !self.db.with_snapshot(|snap| snap.characters.contains_key(&cid)) {
                        return Err(BridgeRpcError {
                            code: "CharacterNotFound".into(),
                            message: format!("Character {cid} not found"),
                            client_error: None,
                        });
                    }
                    Some(cid)
                }
                _ => {
                    return Err(BridgeRpcError {
                        code: "InvalidArgument".into(),
                        message: format!("{method} target_char_id must be a positive integer"),
                        client_error: None,
                    });
                }
            };
            if let Some(cid) = target_cid {
                crate::services::space_destiny::clear_session_change_timer(self, cid);
            }
            return Ok(PyRep::Bool(true));
        }

        // Explicit camelCase GM / QA RPC gate (`#1350`): must hold `ROLE_GMH | ROLE_GML | ROLE_ADMIN` (`ROLE_QA` alone is denied).
        let is_privileged_gm_camel_rpc = matches!(
            (effective_service, method),
            (
                "warRegistry",
                "GMActivateDefender"
                    | "GMDeactivateDefender"
                    | "GMExtendAllyContract"
                    | "GMJoinDefender"
                    | "GMSetDeclareTime"
                    | "GMSetWarFinishTime"
            ) | (
                "charUnboundMgr",
                "ToggleValidation" | "GetQAStarterSystemIDs"
            )
        );
        if is_privileged_gm_camel_rpc {
            const ERR_INSUFFICIENT_ROLE: &str = "InsufficientRole";
            let _ = Self::require_auth(session)?;
            if (session.role
                & (eve_machonet::service_roles::ROLE_GMH
                    | eve_machonet::service_roles::ROLE_GML
                    | eve_machonet::service_roles::ROLE_GMS
                    | eve_machonet::service_roles::ROLE_ADMIN))
                == 0
            {
                return Err(BridgeRpcError {
                    code: ERR_INSUFFICIENT_ROLE.into(),
                    message: format!(
                        "UserError: GM_InsufficientRole: {effective_service}.{method} requires ROLE_GMH, ROLE_GML, ROLE_GMS, or ROLE_ADMIN"
                    ),
                    client_error: None,
                });
            }
        }

        // Central GM vs QA role gates across all GM, slash, sessionMgr, and QA/developer RPC surfaces (`S-9`, `D-4`).
        // V24.02 Multifit UI calls `publicQaToolsServer.CanGiveItemForMultifit()` on normal player
        // sessions (`role = 0`) expecting `False` rather than `InsufficientRole` so it can hide
        // QA item-spawning controls; `jump_clone_names::handle_public_qa_tools` enforces
        // `require_character` and returns `Bool(char_exists && has_gm_or_qa)` while keeping
        // `MoveMeTo`, `SlashCmd`, and all other `publicQaToolsServer` RPCs gated here.
        let is_public_qa_multifit_check = matches!(
            (effective_service, method),
            ("publicQaToolsServer", "CanGiveItemForMultifit")
        );
        // rpc-lint: allow prefix check for QA method gating
        let is_qa_surface = (effective_service == "publicQaToolsServer" && !is_public_qa_multifit_check)
            || method.starts_with("QA_")
            || method.starts_with("qa_")
            || method.ends_with("QA");
        let is_gm_only_surface = matches!(
            effective_service,
            "slash" | "devToolsProvider" | "sessionMgr"
        ) || (effective_service == "taleMgr"
            // rpc-lint: allow pre-existing GM gate for a server-internal name; refusing it is the point
            && method == "MaterializeSystemLOD2")
            || (effective_service == "keeper" && method == "GetLevelEditor")
            || (effective_service == "dungeon"
                && matches!(
                    method,
                    "DEGetDungeons"
                        | "DEGetRooms"
                        | "DEGetArchetypes"
                        | "DEGetFactions"
                        | "DEGetRoomObjectPaletteData"
                ))
            || method.starts_with("GM_");
        if is_gm_only_surface {
            let _ = Self::require_auth(session)?;
            let slash_subrole = effective_service == "slash"
                && (session.role
                    & (eve_machonet::service_roles::ROLE_QA
                        | eve_machonet::service_roles::ROLE_SPAWN
                        | eve_machonet::service_roles::ROLE_ROLEADMIN))
                    != 0;
            // V24.01 `svc_slash.FetchRemoteCommandList` calls `slash.SlashCmd("/")` for all
            // `/localdevtools` sessions; denying it with `InsufficientRole` triggers `eval()`
            // failure and a modal error popup, whereas `SlashError` `"Commands: []"` hides
            // privileged commands cleanly without exposing any GM command names.
            let slash_fetch_command_list = effective_service == "slash"
                && method == "SlashCmd"
                && args
                    .first()
                    .and_then(|a| a.as_str())
                    .map(str::trim)
                    == Some("/");
            if !session.has_gm_role() && !slash_subrole && !slash_fetch_command_list {
                return Err(BridgeRpcError {
                    code: "InsufficientRole".into(),
                    message: format!(
                        "UserError: GM_InsufficientRole: {effective_service}.{method} requires a V24.01 GM/Admin service role bitmask"
                    ),
                    client_error: None,
                });
            }
        } else if is_qa_surface {
            let _ = Self::require_auth(session)?;
            if !session.has_gm_role() && (session.role & eve_machonet::service_roles::ROLE_QA) == 0
            {
                return Err(BridgeRpcError {
                    code: "InsufficientRole".into(),
                    message: format!(
                        "UserError: GM_InsufficientRole: {effective_service}.{method} requires a V24.01 GM/QA/Admin service role bitmask"
                    ),
                    client_error: None,
                });
            }
        }

        if effective_service == "serviceGatewayMgr" {
            if let Some(res) = self
                .quasar_gateway
                .dispatch_service_gateway_mgr_rpc(method, args, kwargs)
            {
                return res;
            }
        }

        // Dispatch Quasar Protobuf Public Gateway (`publicGatewaySvc`) requests, subscriptions, notices, and events (`Q-2`)
        if effective_service == "publicGatewaySvc" {
            if let Some(res) = self.quasar_gateway.dispatch_public_gateway_rpc(
                session,
                method,
                args,
                kwargs,
                now_unix,
            ) {
                return res;
            }
            let _ = Self::require_character(session)?;
            if matches!(method, "SendRequest" | "PublishEvent") {
                let domain = args
                    .first()
                    .and_then(|a| a.as_str())
                    .unwrap_or("eve_public.character");
                if !self.quasar_domains.contains(domain) {
                    let dec = check_global_warn(
                        WarnCategory::NotInCatalog,
                        format!("quasar_domain:{domain}"),
                    );
                    if dec.should_log {
                        warn!(
                            domain,
                            count = dec.count,
                            suppressed = dec.suppressed_for_key,
                            "NOT_IN_CATALOG: Quasar domain not in quasar_protobuf_catalog.json"
                        );
                    }
                    return Err(BridgeRpcError {
                        code: "UnknownQuasarDomain".into(),
                        message: format!(
                            "Quasar domain '{domain}' is not in quasar_protobuf_catalog.json"
                        ),
                        client_error: None,
                    });
                }
                let msg_type = args.get(1).and_then(|a| a.as_str()).unwrap_or("GetRequest");
                append_unimplemented_rpc_log(
                    session.session_id,
                    effective_service,
                    method,
                    args.len(),
                    kwargs.keys().map(|s| s.as_str()).collect(),
                    caller_trace,
                );
                return Err(BridgeRpcError {
                    code: "UnimplementedRPC".into(),
                    message: format!(
                        "publicGatewaySvc.{method}({domain} / {msg_type}) is not yet implemented"
                    ),
                    client_error: None,
                });
            }
        }

        // Handle universal `MachoResolveObject` moniker node lookup (`carbon/common/script/net/moniker.pyj`)
        if method == "MachoResolveObject" {
            if matches!(effective_service, "keeper" | "tourneyMgr" | "planetMgr") {
                let (_, char_id) = Self::require_character(session)?;
                let obj_id = Self::extract_moniker_object_id(args, "MachoResolveObject")?;
                if !crate::router::binding_auth::authorize_service_binding(
                    self,
                    session,
                    char_id,
                    effective_service,
                    obj_id,
                ) {
                    const ERR_BIND_AUTH_DENIED: &str = "BindAuthorizationDenied";
                    return Err(BridgeRpcError {
                        code: ERR_BIND_AUTH_DENIED.into(),
                        message: format!(
                            "MachoResolveObject denied for {effective_service}({obj_id})"
                        ),
                        client_error: None,
                    });
                }
            } else {
                let _ = Self::require_auth(session)?;
            }
            return Ok(PyRep::Integer(eve_machonet::MACHO_PROXY_NODE_ID as i64));
        }

        // Handle universal `MachoBindObject` + optional piggybacked RPC call
        if method == "MachoBindObject" {
            if effective_service != "populationCap" {
                let _ = Self::require_character(session)?;
            } else {
                let _ = Self::require_auth(session)?;
            }
            let obj_id = Self::extract_moniker_object_id(args, "MachoBindObject")?;
            let parsed_piggyback = Self::parse_macho_bind_piggyback(args.get(1));
            let (bid, bound_in_map) = match self.bind_object(session, effective_service, obj_id) {
                Ok(bid) => (bid, true),
                Err(err) if err.code == "BindAuthorizationDenied" => {
                    let grace_bid = match &parsed_piggyback {
                        Ok(Some((sub_method, sub_args, sub_kwargs))) => self
                            .allocate_late_call_grace_binding(
                                session,
                                effective_service,
                                obj_id,
                                sub_method,
                                sub_args,
                                sub_kwargs,
                            ),
                        _ => None,
                    };
                    if let Some(grace_bid) = grace_bid {
                        (grace_bid, false)
                    } else {
                        return Err(err);
                    }
                }
                Err(err) => return Err(err),
            };
            if effective_service == "beyonce" {
                let requested_sys = u32::try_from(obj_id)
                    .ok()
                    .filter(|&id| self.sde.solar_systems.contains_key(&id));
                if let Some(sys_id) = session.solar_system_id {
                    if requested_sys.is_none_or(|req_id| req_id == sys_id) {
                        crate::services::mining_sites::seed_system_mining_sites_from_sde(
                            self, sys_id,
                        )
                        .await;
                        crate::services::combat_crimewatch::combat_npc_ai::ensure_zarzakh_patrols_and_gate_guns(
                            self, sys_id,
                        )
                        .await;
                        if self
                            .sde
                            .solar_systems
                            .get(&sys_id)
                            .is_some_and(|s| s.region_id == 10_000_070)
                        {
                            crate::services::pve_exploration::ensure_system_pve_sites_seeded(
                                self, sys_id,
                            )
                            .await;
                        }
                    }
                }
                let has_piggybacked_update_state = matches!(
                    args.get(1),
                    Some(PyRep::Tuple(call_tuple))
                        if call_tuple.first().and_then(|m| m.as_str()) == Some("UpdateStateRequest")
                );
                if !has_piggybacked_update_state && session.station_id.is_none() {
                    if let (Some(cid), Some(sys_id)) =
                        (session.character_id, session.solar_system_id)
                    {
                        if !self.is_space_ballpark_bootstrapped(session.session_id, cid, sys_id) {
                            let _ = crate::services::space_destiny::handle_update_state_request(
                                self,
                                session,
                                None,
                            )
                            .await;
                        }
                    }
                }
            }

            let call_result = match parsed_piggyback {
                Err(err) => Err(err),
                Ok(None) => Ok(PyRep::None),
                Ok(Some((sub_method, sub_args, sub_kwargs))) => {
                    dispatch_rpc_boxed(
                        self,
                        session,
                        effective_service,
                        Some(bid),
                        sub_method,
                        &sub_args,
                        &sub_kwargs,
                        caller_trace,
                    )
                    .await
                }
            };
            let call_result = match call_result {
                Ok(rep) => rep,
                Err(err) => {
                    if bound_in_map {
                        self.remove_bound_object_without_notify(session.session_id, bid);
                    } else {
                        self.purge_transition_grace_entry(session.session_id, bid);
                    }
                    return Err(err);
                }
            };

            // `ObjectCall.GetObjectID` (`carbon/common/script/net/ObjectCallGPCS.pyj`) marshals
            // bound objects as `SubStruct(SubStream((f"N={nodeID}:{boundID}", refID)))`.
            let oid_rep = self.bound_object_rep(bid, caller_trace);
            return Ok(PyRep::Tuple(vec![oid_rep, call_result]));
        }

        // Dispatch across the 12 domain modules in order
        if let Some(res) = box_fut(|| services::account_services::handle(self, session, effective_service, method, args, kwargs)).await {
            if effective_service == "userSvc" && method == "UserLogOffCharacter" && res.is_ok() {
                self.clear_pending_structure_view_outside_set_state_for_session(
                    session.session_id,
                );
            }
            return res;
        }
        if let Some(res) = box_fut(|| services::character_skills::character_contacts_gm::handle_gm_qa_rpcs(self, session, effective_service, method, args)).await {
            return res;
        }
        if let Some(entry) = self.rpc_registry.lookup(effective_service, method).copied() {
            use crate::router::rpc_registry::RpcDispatchHandler;
            let registry_res = match entry.handler {
                RpcDispatchHandler::Sync(f) => {
                    f(self, session, effective_service, method, args, kwargs)
                }
                RpcDispatchHandler::SyncReadOnlySession(f) => {
                    f(self, session, effective_service, method, args, kwargs)
                }
                RpcDispatchHandler::SyncNoKwargs(f) => {
                    f(self, session, effective_service, method, args)
                }
                RpcDispatchHandler::SyncNoService(f) => f(self, session, method, args, kwargs),
                RpcDispatchHandler::SyncCallerTrace(f) => {
                    f(self, session, effective_service, method, args, kwargs, caller_trace)
                }
                RpcDispatchHandler::Async(f) => {
                    f(self, session, effective_service, method, args, kwargs).await
                }
                RpcDispatchHandler::AsyncCallerTrace(f) => {
                    f(self, session, effective_service, method, args, kwargs, caller_trace).await
                }
                RpcDispatchHandler::BoundAsync(f) => {
                    f(
                        self,
                        session,
                        bound_entry.as_ref(),
                        effective_service,
                        method,
                        args,
                        kwargs,
                    )
                    .await
                }
            };
            match registry_res {
                Some(res) => return res,
                None => {
                    debug_assert!(
                        false,
                        "registered RPC handler for {effective_service}.{method} returned None"
                    );
                    return self.unimplemented_rpc_error(
                        session,
                        effective_service,
                        method,
                        args,
                        kwargs,
                        caller_trace,
                    );
                }
            }
        }
        if let Some(res) = services::macho_cluster_mgmt::handle(self, session, effective_service, method, args, kwargs) {
            return res;
        }
        if let Some(res) = box_fut(|| services::auth_cache::handle(self, session, effective_service, method, args, kwargs)).await {
            if (effective_service, method) == ("authentication", "Login") && res.is_ok() {
                if let Some(uid) = session.user_id {
                    services::account_services::on_login(self, uid);
                }
            } else if (effective_service, method) == ("authentication", "Logout") && res.is_ok() {
                self.clear_pending_structure_view_outside_set_state_for_session(
                    session.session_id,
                );
            }
            return res;
        }
        if let Some(res) = box_fut(|| services::client_login::handle(self, session, effective_service, method, args, kwargs, caller_trace)).await {
            return res;
        }
        if let Some(res) = services::inventory_bound::handle(self, session, bound_entry.as_ref(), effective_service, method, args, caller_trace) {
            return res;
        }
        if let Some(res) = box_fut(|| services::corp_registry_ops::handle_corp_hangar_invbroker(self, session, bound_entry.as_ref(), effective_service, method, args, kwargs)).await {
            return res;
        }
        let prev_cid_before_select = (effective_service == "charUnboundMgr"
            && method == "SelectCharacterID")
            .then_some(session.character_id)
            .flatten();
        if let Some(res) = services::character_skills::handle(self, session, effective_service, method, args, kwargs) {
            if effective_service == "charUnboundMgr" && method == "SelectCharacterID" && res.is_ok() {
                crate::quasar_gateway::isolate_side_effect(
                    "dispatch.SelectCharacterID.secondary_hooks",
                    || {
                        if let Some(old_cid) = prev_cid_before_select {
                            services::season_manager::clear_season_session_initialized(self, old_cid);
                        }
                        if let (Some(old_cid), Some(new_cid)) = (prev_cid_before_select, session.character_id) {
                            if old_cid != new_cid {
                                services::campaigns_and_milestones::playtime::pause_milestone_playtime(
                                    &self.db,
                                    old_cid,
                                    self.now_unix(),
                                );
                            }
                        }
                        if let Some(cid) = session.character_id {
                            services::season_manager::clear_season_session_initialized(self, cid);
                        }
                    },
                );
                if let Some(old_cid) = prev_cid_before_select {
                    self.clear_space_ballpark_bootstrapped(old_cid);
                    self.clear_brain_primed_for_char(old_cid);
                }
                if let Some(cid) = session.character_id {
                    self.clear_space_ballpark_bootstrapped(cid);
                    self.clear_brain_primed_for_char(cid);
                    crate::sp6_lifecycle::clear_safe_logoff(&self.db, cid);
                    services::character_skill_ops::migrate_obsolete_skills_on_login(self, cid);
                    services::pve_special_sites::ensure_abyssal_room_balls_for_ctx(self, cid).await;
                    if let Some(sys) = session.solar_system_id {
                        services::map_service::record_solar_system_visit(self, cid, sys);
                        if session.station_id.is_none() && session.structure_id.is_none() {
                            maybe_emit_login_sim_clock_rebase(self, cid, sys);
                        }
                    }
                    services::market_contracts::sweep_and_notify_expired_for_char(self, cid);
                    services::industry_pi::sync_character_pi_colonies_on_login(self, cid);
                    services::nav_warp_interdiction::sync_waypoints_on_login(self, cid);
                    if let Some(st_id) = session.station_id {
                        services::notify_emitters_combat_ess_progression::emit_char_station_notification(self, cid, st_id, true);
                    }
                }
            }
            if effective_service == "skillMgr2"
                && method == "GetSkills"
                && (caller_trace == Some("machoNet:26000") || bound_id.is_some())
            {
                if let Ok(PyRep::Rowset { rows, .. }) = res {
                    let entries = rows
                        .into_iter()
                        .filter_map(|r| {
                            let tid = r.first()?.as_i64()?;
                            let lvl = r.get(1)?.as_i64().unwrap_or(0);
                            let sp = r.get(2)?.as_f64().unwrap_or(0.0);
                            let active_lvl = r.get(3).and_then(|v| v.as_i64()).unwrap_or(lvl);
                            let rank = self
                                .sde
                                .types
                                .get(&(tid as u32))
                                .and_then(|t| {
                                    t.default_attributes
                                        .get(&eve_sde::attr_ids::SKILL_TIME_CONSTANT)
                                        .copied()
                                })
                                .unwrap_or(1.0);
                            Some((
                                PyRep::Integer(tid),
                                PyRep::keyval([
                                    ("typeID", PyRep::Integer(tid)),
                                    ("skillLevel", PyRep::Integer(lvl)),
                                    ("trainedSkillLevel", PyRep::Integer(lvl)),
                                    ("effectiveSkillLevel", PyRep::Integer(active_lvl)),
                                    ("activeSkillLevel", PyRep::Integer(active_lvl)),
                                    ("virtualSkillLevel", PyRep::Integer(active_lvl)),
                                    ("skillPoints", PyRep::Real(sp)),
                                    ("trainedSkillPoints", PyRep::Real(sp)),
                                    ("skillRank", PyRep::Real(rank)),
                                ]),
                            ))
                        })
                        .collect();
                    return Ok(PyRep::Dict(entries));
                }
            }
            return res;
        }
        if matches!(
            (effective_service, method),
            ("invbroker", "GetAllItems")
                | ("corpmgr", "GetAssetInventory" | "GetAssetInventoryForLocation" | "SearchAssets")
        ) {
            services::station_structure::structure_lifecycle::sync_asset_safety_wraps(self);
        }
        if let Some(res) = box_fut(|| services::inventory_fitting::handle(
            self, session, bound_entry.as_ref(), effective_service, method, args, kwargs,
        )).await {
            if res.is_ok() {
                box_fut(|| services::character_skill_ops::post_inventory_fitting_hook(
                    self,
                    session,
                    bound_entry.as_ref(),
                    method,
                    args,
                ))
                .await;
            }
            return res;
        }
        if let Some(res) = box_fut(|| services::station_structure::handle(
            self, session, effective_service, method, args, kwargs,
        )).await {
            return res;
        }
        if let Some(res) = box_fut(|| services::space_entity_ops::handle(
            self, session, bound_entry.as_ref(), effective_service, method, args, kwargs,
        )).await {
            return res;
        }
        if let Some(res) = box_fut(|| services::mining_sites::handle(
            self, session, effective_service, method, args, kwargs,
        )).await {
            return res;
        }
        if let Some(res) = box_fut(|| services::space_destiny::handle(
            self, session, bound_entry.as_ref(), effective_service, method, args, kwargs,
        )).await {
            return res;
        }
        if let Some(res) = box_fut(|| services::pve_exploration::handle_pve_exploration(
            self, session, effective_service, method, args, kwargs,
        )).await {
            return res;
        }
        if let Some(res) = box_fut(|| services::pve_special_sites::handle_pve_special_sites(
            self, session, effective_service, method, args, kwargs,
        )).await {
            return res;
        }
        if let Some(res) = box_fut(|| services::pve_dungeon_cache::handle(
            self, session, effective_service, method, args, kwargs,
        )).await {
            return res;
        }
        // Preserve abyssal_void_mgr::handle's cross-service Abyssal in-pocket RPC guard ahead of
        // combat_crimewatch, fleet_social (XmppChatMgr/LSC/scanMgr), sov_fw_insurgency (map.GetCurrentSovData),
        // and fallback scanSvc handlers.
        if let Some(res) = services::pve_abyssal_hazards::check_abyssal_in_pocket_rpc_restrictions(
            session, effective_service, method, args,
        ) {
            return res;
        }
        if let Some(res) = box_fut(|| services::combat_crimewatch::handle(
            self, session, effective_service, method, args, kwargs,
        )).await {
            return res;
        }
        if let Some(res) = services::market_trading::handle(
            self, session, effective_service, method, args, kwargs,
        ) {
            return res;
        }
        if let Some(res) = services::market_contracts::handle(self, session, effective_service, method, args, kwargs) {
            return res;
        }
        if let Some(res) = services::wallet_serverops::handle(self, session, effective_service, method, args, kwargs) {
            return res;
        }
        if let Some(res) = services::industry_pi::handle(self, session, effective_service, method, args, kwargs, bound_entry.as_ref()) {
            return res;
        }
        if let Some(res) = box_fut(|| services::agents_missions::handle_with_bound(self, session, bound_entry.as_ref(), effective_service, method, args, kwargs)).await {
            return res;
        }
        if let Some(res) = box_fut(|| services::fleet_social::handle_with_bound(self, session, bound_entry.as_ref(), effective_service, method, args, kwargs)).await {
            if effective_service == "beyonce"
                && method == "GetFormations"
                && res.is_ok()
                && session.station_id.is_none()
            {
                if let (Some(cid), Some(sys_id)) =
                    (session.character_id, session.solar_system_id)
                {
                    let has_observer = session.structure_id.is_some()
                        || session.ship_id.is_some_and(|ship_id| ship_id > 0);
                    if has_observer
                        && !self.is_space_ballpark_bootstrapped(session.session_id, cid, sys_id)
                    {
                        let _ = crate::services::space_destiny::handle_update_state_request(
                            self,
                            session,
                            bound_entry.as_ref(),
                        )
                        .await;
                    }
                }
            }
            return res;
        }
        if let Some(res) = box_fut(|| services::social_corp_fleet::handle(
            self, session, effective_service, method, args, kwargs, bound_entry.as_ref().map(|b| b.object_id),
        )).await {
            return res;
        }
        if let Some(res) = box_fut(|| services::sov_fw_insurgency::handle(self, session, effective_service, method, args, kwargs)).await {
            return res;
        }
        if let Some(res) = box_fut(|| services::cluster_macho_misc_services::handle(self, session, effective_service, method, args, kwargs)).await {
            return res;
        }

        self.unimplemented_rpc_error(session, effective_service, method, args, kwargs, caller_trace)
    }

    fn unimplemented_rpc_error(
        &self,
        session: &SessionState,
        effective_service: &str,
        method: &str,
        args: &[PyRep],
        kwargs: &HashMap<String, PyRep>,
        caller_trace: Option<&str>,
    ) -> Result<PyRep, BridgeRpcError> {
        let in_catalog = self.cataloged_services.contains(effective_service);
        let warn_cat = if in_catalog {
            WarnCategory::UnknownRpc
        } else {
            WarnCategory::NotInCatalog
        };
        let dec = check_global_warn(warn_cat, format!("{effective_service}.{method}"));
        if dec.should_log {
            if in_catalog {
                warn!(
                    service = effective_service,
                    method,
                    arg_count = args.len(),
                    caller = caller_trace.unwrap_or("unknown"),
                    count = dec.count,
                    suppressed = dec.suppressed_for_key,
                    "Unknown RPC intercepted — returning explicit UnimplementedRPC error"
                );
            } else {
                warn!(
                    service = effective_service,
                    method,
                    arg_count = args.len(),
                    caller = caller_trace.unwrap_or("unknown"),
                    count = dec.count,
                    suppressed = dec.suppressed_for_key,
                    "NOT_IN_CATALOG: unknown service/RPC intercepted — returning explicit UnimplementedRPC error"
                );
            }
        }
        if session.is_authenticated() {
            append_unimplemented_rpc_log(
                session.session_id,
                effective_service,
                method,
                args.len(),
                kwargs.keys().map(|s| s.as_str()).collect(),
                caller_trace,
            );
        }
        Err(BridgeRpcError {
            code: "UnimplementedRPC".into(),
            message: format!("{effective_service}.{method} is not yet implemented"),
            client_error: None,
        })
    }
}

const MAX_UNIMPLEMENTED_LOG_BYTES: u64 = 10 * 1024 * 1024; // 10 MiB cap


pub use crate::services::mining_sites::{
    seed_system_mining_sites_from_sde, tick_sov_prospecting_deposit_respawn,
};

/// Emits a single `DoSimClockRebase` before ballpark bootstrap when logging into a solar
/// system whose simulation clock lags behind wallclock (`#1083`, `#1085`).
fn maybe_emit_login_sim_clock_rebase(
    ctx: &ServerContext,
    char_id: i64,
    solar_system_id: u32,
) {
    let already_rebased = {
        let cn = ctx
            .systems
            .cluster_nodes
            .read()
            .unwrap_or_else(|e| e.into_inner());
        let gen = cn
            .rebase_generation_by_system
            .get(&solar_system_id)
            .copied()
            .unwrap_or(1);
        cn.char_rebased_system_gen.get(&char_id).copied() == Some((solar_system_id, gen))
    };
    if already_rebased {
        return;
    }
    let now_wall_ft = ctx.now_machonet_wallclock_filetime_ticks();
    let cur_sim_ft_opt = {
        let cn = ctx
            .systems
            .cluster_nodes
            .read()
            .unwrap_or_else(|e| e.into_inner());
        cn.sim_clock_anchoring
            .get(&solar_system_id)
            .copied()
            .map(|(anchor_wall_ft, anchor_sim_ft, factor)| {
                let elapsed = now_wall_ft.saturating_sub(anchor_wall_ft).max(0);
                anchor_sim_ft.saturating_add(((elapsed as f64) * factor).round() as i64)
            })
    }
    .or_else(|| {
        let handle = ctx.systems.get_handle(solar_system_id)?;
        let bp_unix = handle
            .ballpark
            .read()
            .map(|bp| bp.sim_unix_secs)
            .unwrap_or(0);
        let client_now_unix = ctx
            .now_unix()
            .saturating_sub(ctx.unrebased_clock_offset_secs());
        (bp_unix > 0 && bp_unix < client_now_unix)
            .then(|| ((bp_unix as i64) + 11_644_473_600).saturating_mul(10_000_000))
    });
    if let Some(cur_sim_ft) = cur_sim_ft_opt {
        if cur_sim_ft < now_wall_ft {
            {
                let mut cn = ctx
                    .systems
                    .cluster_nodes
                    .write()
                    .unwrap_or_else(|e| e.into_inner());
                let gen = *cn
                    .rebase_generation_by_system
                    .entry(solar_system_id)
                    .or_insert(1);
                if cn.char_rebased_system_gen.get(&char_id).copied()
                    == Some((solar_system_id, gen))
                {
                    return;
                }
                cn.char_rebased_system_gen
                    .insert(char_id, (solar_system_id, gen));
            }
            // parity: notify:DoSimClockRebase
            ctx.emit_typed(
                crate::router::NotificationTarget::Character(char_id),
                crate::router::notify_registry::DoSimClockRebase {
                    times: PyRep::Tuple(vec![
                        PyRep::Integer(now_wall_ft),
                        PyRep::Integer(cur_sim_ft),
                    ]),
                },
            );
        }
    }
}

#[cfg(test)]
#[path = "dispatch_wal_tests.rs"]
mod wal_poison_tests;

#[cfg(test)]
#[path = "dispatch_unit_tests.rs"]
mod dispatch_unit_tests;
#[cfg(test)]
#[path = "dispatch_edges_tests.rs"]
mod dispatch_edges_tests;
