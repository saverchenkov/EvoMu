//! Authenticated RPC and Quasar protobuf message dispatching.

use crate::router::context::ServerContext;
use crate::services;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;
use tracing::{info, warn};

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

impl ServerContext {
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
        let initial_len = session
            .character_id
            .and_then(|cid| {
                self.pending_notifications
                    .read()
                    .ok()
                    .and_then(|q| q.get(&cid).map(|v| v.len()))
            })
            .unwrap_or(0);

        let res = self
            .dispatch_rpc_inner(
                session,
                service,
                bound_id,
                method,
                args,
                kwargs,
                caller_trace,
            )
            .await;

        if let Some(cid) = session.character_id {
            if let Some(notifs) = self
                .pending_notifications
                .read()
                .ok()
                .and_then(|q| q.get(&cid).cloned())
            {
                for notif in notifs.into_iter().skip(initial_len) {
                    session.push_notification(notif.event, notif.args);
                }
            }
        }

        res
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
        if !is_exempt_auth_call {
            if let Some(uid) = session.user_id {
                if let Some((code, msg)) =
                    self.db
                        .check_session_revoked(uid, session.session_id, self.now_unix())
                {
                    self.release_session_bindings(session.session_id);
                    self.quasar_gateway.unsubscribe_session(session.session_id);
                    if code == "MultiLoginBlocked" {
                        if !is_multi_login_blocker_call {
                            return Err(BridgeRpcError { code, message: msg });
                        }
                    } else {
                        let _ = self.db.release_online_lock(uid, session.session_id);
                        session.user_id = None;
                        session.role = 0;
                        session.character_id = None;
                        session.corporation_id = None;
                        session.alliance_id = None;
                        session.station_id = None;
                        session.structure_id = None;
                        session.solar_system_id = None;
                        session.constellation_id = None;
                        session.region_id = None;
                        session.ship_id = None;
                        return Err(BridgeRpcError { code, message: msg });
                    }
                }
            }
        }

        // Synchronize active character's ship/station/structure/solar-system state from DB
        if let Some(cid) = session.character_id {
            if let Ok(ch) = self.db.get_character(cid) {
                session.ship_id = Some(ch.active_ship_id);
                session.station_id = ch.station_id;
                session.structure_id = ch.structure_id;
                session.solar_system_id = Some(ch.solar_system_id);
                session.constellation_id = Some(ch.constellation_id);
                session.region_id = Some(ch.region_id);
            }
        }
        let now_unix = self.now_unix();
        let _ = self.quasar_gateway.on_session_changed(session, now_unix);

        let bound_entry = if let Some(bid) = bound_id {
            Some(self.verify_bound_object(session, bid)?)
        } else {
            None
        };

        let effective_service = bound_entry
            .as_ref()
            .map(|b| b.service.as_str())
            .unwrap_or(service);

        if effective_service == "taleMgr" && !self.feature_flags.enable_living_world {
            return Err(BridgeRpcError {
                code: "FeatureDisabled".into(),
                message: "Living World Director (taleMgr) is disabled in evomu.toml feature_flags"
                    .into(),
            });
        }

        if effective_service == "publicGatewaySvc" && !self.feature_flags.enable_quasar_gateway {
            return Err(BridgeRpcError {
                code: "FeatureDisabled".into(),
                message: "Quasar publicGatewaySvc is disabled in evomu.toml feature_flags".into(),
            });
        }

        // Central GM/QA role gate across all GM, slash, and QA/developer RPC surfaces (`S-9`, `D-4`).
        let is_gm_or_qa_surface = matches!(
            effective_service,
            "slash" | "publicQaToolsServer" | "devToolsProvider"
        ) || (effective_service == "taleMgr"
            && method == "MaterializeSystemLOD2")
            || (effective_service == "keeper" && method == "GetLevelEditor")
            || method.starts_with("GM_")
            || method.starts_with("QA_")
            || method.starts_with("qa_")
            || method.ends_with("QA");
        if is_gm_or_qa_surface {
            let _ = Self::require_auth(session)?;
            if !session.has_gm_role() && (session.role & eve_machonet::service_roles::ROLE_QA) == 0
            {
                return Err(BridgeRpcError {
                    code: "InsufficientRole".into(),
                    message: format!(
                        "{effective_service}.{method} requires a V24.01 GM/QA/Admin service role bitmask"
                    ),
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
            if matches!(method, "SendRequest" | "PublishEvent" | "CallQuasar") {
                let domain = args
                    .first()
                    .and_then(|a| a.as_str())
                    .unwrap_or("eve_public.character");
                if !self.quasar_domains.contains(domain) {
                    return Err(BridgeRpcError {
                        code: "UnknownQuasarDomain".into(),
                        message: format!(
                            "Quasar domain '{domain}' is not in quasar_protobuf_catalog.json"
                        ),
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
                });
            }
        }

        // Handle universal `MachoBindObject` + optional piggybacked RPC call
        if method == "MachoBindObject" {
            let _ = Self::require_character(session)?;
            let obj_id = match args.first() {
                Some(PyRep::Integer(id)) => *id,
                Some(PyRep::String(s)) | Some(PyRep::Token(s)) if !s.is_empty() => s
                    .bytes()
                    .fold(1_000i64, |acc, b| {
                        acc.wrapping_mul(31).wrapping_add(b as i64)
                    })
                    .abs()
                    .max(1),
                Some(PyRep::Tuple(items)) => {
                    if let Some(id) = items.first().and_then(|v| v.as_i64()) {
                        id
                    } else if let Some(s) = items
                        .first()
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                    {
                        s.bytes()
                            .fold(1_000i64, |acc, b| {
                                acc.wrapping_mul(31).wrapping_add(b as i64)
                            })
                            .abs()
                            .max(1)
                    } else {
                        return Err(BridgeRpcError {
                            code: "InvalidArgument".into(),
                            message: "MachoBindObject bind tuple missing objectID".into(),
                        });
                    }
                }
                _ => {
                    return Err(BridgeRpcError {
                        code: "InvalidArgument".into(),
                        message: "MachoBindObject requires an explicit integer or string objectID"
                            .into(),
                    })
                }
            };
            let bid = self.bind_object(session, effective_service, obj_id)?;

            let call_result = match args.get(1) {
                Some(PyRep::Tuple(call_tuple)) if !call_tuple.is_empty() => {
                    let sub_method =
                        call_tuple.first().and_then(|m| m.as_str()).ok_or_else(|| {
                            BridgeRpcError {
                            code: "InvalidArgument".into(),
                            message:
                                "Piggybacked call in MachoBindObject requires string method name"
                                    .into(),
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
                    Box::pin(self.dispatch_rpc(
                        session,
                        effective_service,
                        Some(bid),
                        sub_method,
                        &sub_args,
                        &sub_kwargs,
                        caller_trace,
                    ))
                    .await?
                }
                _ => PyRep::None,
            };

            return Ok(PyRep::Tuple(vec![PyRep::Integer(bid as i64), call_result]));
        }

        // Dispatch across the 12 domain modules in order
        if let Some(res) =
            services::auth_cache::handle(self, session, effective_service, method, args, kwargs)
                .await
        {
            return res;
        }
        if let Some(res) = services::character_skills::handle(
            self,
            session,
            effective_service,
            method,
            args,
            kwargs,
        ) {
            return res;
        }
        if let Some(res) = services::inventory_fitting::handle(
            self,
            session,
            bound_entry.as_ref(),
            effective_service,
            method,
            args,
            kwargs,
        )
        .await
        {
            return res;
        }
        if let Some(res) = services::station_structure::handle(
            self,
            session,
            effective_service,
            method,
            args,
            kwargs,
        )
        .await
        {
            return res;
        }
        if let Some(res) = services::space_destiny::handle(
            self,
            session,
            bound_entry.as_ref(),
            effective_service,
            method,
            args,
            kwargs,
        )
        .await
        {
            return res;
        }
        if let Some(res) = services::combat_crimewatch::handle(
            self,
            session,
            effective_service,
            method,
            args,
            kwargs,
        )
        .await
        {
            return res;
        }
        if let Some(res) = services::market_trading::handle(
            self,
            session,
            effective_service,
            method,
            args,
            kwargs,
        ) {
            return res;
        }
        if let Some(res) = services::contracts_service::handle(
            self,
            session,
            effective_service,
            method,
            args,
            kwargs,
        ) {
            return res;
        }
        if let Some(res) = services::lp_store::handle(
            self,
            session,
            effective_service,
            method,
            args,
            kwargs,
        ) {
            return res;
        }
        if let Some(res) = services::market_contracts::handle(
            self,
            session,
            effective_service,
            method,
            args,
            kwargs,
        ) {
            return res;
        }
        if let Some(res) =
            services::industry_pi::handle(self, session, effective_service, method, args, kwargs)
        {
            return res;
        }
        if let Some(res) = services::agents_missions::handle(
            self,
            session,
            effective_service,
            method,
            args,
            kwargs,
        )
        .await
        {
            return res;
        }
        if let Some(res) = services::fleet_social::handle(
            self,
            session,
            effective_service,
            method,
            args,
            kwargs,
        )
        .await
        {
            return res;
        }
        if let Some(res) = services::social_corp_fleet::handle(
            self,
            session,
            effective_service,
            method,
            args,
            kwargs,
        )
        .await
        {
            return res;
        }
        if let Some(res) = services::sov_fw_insurgency::handle(
            self,
            session,
            effective_service,
            method,
            args,
            kwargs,
        )
        .await
        {
            return res;
        }
        if let Some(res) = services::cosmetics_events::handle(
            self,
            session,
            effective_service,
            method,
            args,
            kwargs,
        )
        .await
        {
            return res;
        }

        warn!(
            service = effective_service,
            method,
            arg_count = args.len(),
            caller = caller_trace.unwrap_or("unknown"),
            "Unimplemented RPC intercepted — returning explicit UnimplementedRPC error"
        );
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
        })
    }
}

const MAX_UNIMPLEMENTED_LOG_BYTES: u64 = 10 * 1024 * 1024; // 10 MiB cap
