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
        self.sync_session(session);
        let initial_len = session
            .character_id
            .and_then(|cid| {
                self.pending_notifications
                    .read()
                    .ok()
                    .and_then(|q| q.get(&cid).map(|v| v.len()))
            })
            .unwrap_or(0);

        let prev_station_id = session.station_id;
        let res = Box::pin(self.dispatch_rpc_inner(
            session,
            service,
            bound_id,
            method,
            args,
            kwargs,
            caller_trace,
        ))
        .await;

        if res.is_ok() && matches!(service, "stationSvc" | "ship" | "structureDocking") {
            if let Some(cid) = session.character_id {
                let guest_row = || {
                    self.db.with_snapshot(|snap| {
                        let c = snap.characters.get(&cid);
                        PyRep::Tuple(vec![
                            PyRep::Integer(cid),
                            c.map(|c| PyRep::Integer(c.corporation_id)).unwrap_or(PyRep::None),
                            c.and_then(|c| c.alliance_id).map(PyRep::Integer).unwrap_or(PyRep::None),
                            c.and_then(|c| c.war_faction_id).map(PyRep::Integer).unwrap_or(PyRep::None),
                        ])
                    })
                };
                if method == "Undock" {
                    if let Some(st_id) = prev_station_id.filter(|_| session.station_id.is_none()) {
                        self.notify(
                            crate::router::NotificationTarget::Station(st_id),
                            "OnCharNoLongerInStation",
                            "charid",
                            vec![guest_row()],
                        );
                    }
                } else if method == "Dock" {
                    if let Some(st_id) = session.station_id.filter(|_| prev_station_id.is_none()) {
                        self.notify(
                            crate::router::NotificationTarget::Station(st_id),
                            "OnCharNowInStation",
                            "charid",
                            vec![guest_row()],
                        );
                    }
                }
            }
        }

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
        let is_exempt_petitioner_call =
            bound_id.is_none() && service == "petitioner" && method == "MayPetition";
        if !is_exempt_auth_call && !is_exempt_petitioner_call {
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
            crate::services::fleet_social::fleet_social_notify::sync_session_fleet(self, session, cid);
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
                // rpc-lint: allow "FeatureDisabled" is an error code, not a taleMgr method
                code: "FeatureDisabled".into(),
                message: "Living World Director (taleMgr) is disabled in evomu.toml feature_flags"
                    .into(),
            });
        }

        if effective_service == "publicGatewaySvc" && !self.feature_flags.enable_quasar_gateway {
            return Err(BridgeRpcError {
                // rpc-lint: allow "FeatureDisabled" is an error code, not a publicGatewaySvc method
                code: "FeatureDisabled".into(),
                message: "Quasar publicGatewaySvc is disabled in evomu.toml feature_flags".into(),
            });
        }

        // Central GM vs QA role gates across all GM, slash, sessionMgr, and QA/developer RPC surfaces (`S-9`, `D-4`).
        // rpc-lint: allow prefix check for QA method gating
        let is_qa_surface = effective_service == "publicQaToolsServer"
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
            if !session.has_gm_role() && !slash_subrole {
                return Err(BridgeRpcError {
                    code: "InsufficientRole".into(),
                    message: format!(
                        "UserError: GM_InsufficientRole: {effective_service}.{method} requires a V24.01 GM/Admin service role bitmask"
                    ),
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

        // Handle universal `MachoResolveObject` moniker node lookup (`carbon/common/script/net/moniker.pyj`)
        if method == "MachoResolveObject" {
            let _ = Self::require_auth(session)?;
            return Ok(PyRep::Integer(eve_machonet::MACHO_PROXY_NODE_ID as i64));
        }

        // Handle universal `MachoBindObject` + optional piggybacked RPC call
        if method == "MachoBindObject" {
            if effective_service != "populationCap" {
                let _ = Self::require_character(session)?;
            } else {
                let _ = Self::require_auth(session)?;
            }
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
                    }
                }
            }

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

            // `ObjectCall.GetObjectID` (`carbon/common/script/net/ObjectCallGPCS.pyj`) marshals
            // bound objects as `SubStruct(SubStream((f"N={nodeID}:{boundID}", refID)))`.
            let oid_rep = self.bound_object_rep(bid, caller_trace);
            return Ok(PyRep::Tuple(vec![oid_rep, call_result]));
        }

        // Dispatch across the 12 domain modules in order
        if let Some(res) = Box::pin(services::account_services::handle(self, session, effective_service, method, args, kwargs)).await {
            return res;
        }
        if let Some(res) = Box::pin(services::character_skills::character_contacts_gm::handle_gm_qa_rpcs(self, session, effective_service, method, args)).await {
            return res;
        }
        if let Some(res) = services::character_energy::handle(self, session, effective_service, method, args) {
            return res;
        }
        if let Some(res) = services::project_discovery::handle_project_discovery(
            self,
            session,
            effective_service,
            method,
            args,
            kwargs,
        ) {
            return res;
        }
        if let Some(res) = services::season_manager::handle_season_manager(
            self,
            session,
            effective_service,
            method,
            args,
            kwargs,
        ) {
            return res;
        }
        if let Some(res) = services::macho_cluster_mgmt::handle(
            self,
            session,
            effective_service,
            method,
            args,
            kwargs,
        ) {
            return res;
        }
        if let Some(res) = Box::pin(services::auth_cache::handle(
            self,
            session,
            effective_service,
            method,
            args,
            kwargs,
        ))
        .await
        {
            if (effective_service, method) == ("authentication", "Login") && res.is_ok() {
                if let Some(uid) = session.user_id {
                    services::account_services::on_login(self, uid);
                }
            }
            return res;
        }
        if let Some(res) = Box::pin(services::client_login::handle(
            self,
            session,
            effective_service,
            method,
            args,
            kwargs,
            caller_trace,
        ))
        .await
        {
            return res;
        }
        if let Some(res) = services::inventory_bound::handle(
            self,
            session,
            bound_entry.as_ref(),
            effective_service,
            method,
            args,
            caller_trace,
        ) {
            return res;
        }
        if let Some(res) = Box::pin(services::corp_registry_ops::handle_corp_hangar_invbroker(
            self,
            session,
            bound_entry.as_ref(),
            effective_service,
            method,
            args,
            kwargs,
        ))
        .await
        {
            return res;
        }
        if let Some(res) = services::character_expert_systems::handle(
            self,
            session,
            effective_service,
            method,
            args,
            kwargs,
        ) {
            return res;
        }
        if let Some(res) = services::character_skill_ops::handle(
            self,
            session,
            effective_service,
            method,
            args,
            kwargs,
        ) {
            return res;
        }
        if let Some(res) = services::character_certificates::handle(
            self,
            session,
            effective_service,
            method,
            args,
            kwargs,
        ) {
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
            if effective_service == "charUnboundMgr" && method == "SelectCharacterID" && res.is_ok() {
                if let Some(cid) = session.character_id {
                    services::character_skill_ops::migrate_obsolete_skills_for_character(self, cid);
                    services::pve_special_sites::ensure_abyssal_room_balls_for_ctx(self, cid).await;
                    if let Some(sys) = session.solar_system_id {
                        services::map_service::record_solar_system_visit(self, cid, sys);
                    }
                    services::market_contracts::sweep_and_notify_expired_for_char(self, cid);
                    services::industry_pi::sync_character_pi_colonies_on_login(self, cid);
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
        if let Some(res) = Box::pin(services::inventory_fitting::handle(
            self, session, bound_entry.as_ref(), effective_service, method, args, kwargs,
        )).await {
            if res.is_ok() {
                services::character_skill_ops::post_inventory_fitting_hook(
                    self,
                    session,
                    bound_entry.as_ref(),
                    method,
                    args,
                )
                .await;
            }
            return res;
        }
        if let Some(res) = Box::pin(services::station_structure::handle(
            self, session, effective_service, method, args, kwargs,
        )).await {
            return res;
        }
        if let Some(res) = Box::pin(services::space_entity_mining::handle(
            self, session, effective_service, method, args, bound_entry.as_ref(),
        )).await {
            return res;
        }
        if let Some(res) = Box::pin(services::space_entity_ops::handle(
            self, session, bound_entry.as_ref(), effective_service, method, args, kwargs,
        )).await {
            return res;
        }
        if let Some(res) = Box::pin(services::mining_sites::handle(
            self, session, effective_service, method, args, kwargs,
        )).await {
            return res;
        }
        if let Some(res) = Box::pin(services::space_destiny::handle(
            self, session, bound_entry.as_ref(), effective_service, method, args, kwargs,
        )).await {
            return res;
        }
        if let Some(res) = Box::pin(services::pve_exploration::handle_pve_exploration(
            self, session, effective_service, method, args, kwargs,
        )).await {
            return res;
        }
        if let Some(res) = Box::pin(services::pve_special_sites::handle_pve_special_sites(
            self, session, effective_service, method, args, kwargs,
        )).await {
            return res;
        }
        if let Some(res) = Box::pin(services::pve_dungeon_cache::handle(
            self, session, effective_service, method, args, kwargs,
        )).await {
            return res;
        }
        if let Some(res) = Box::pin(services::abyssal_void_mgr::handle(
            self, session, effective_service, method, args, kwargs,
        )).await {
            return res;
        }
        if let Some(res) = Box::pin(services::combat_crimewatch::handle(
            self, session, effective_service, method, args, kwargs,
        )).await {
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
        if let Some(res) = services::lp_store::handle(self, session, effective_service, method, args, kwargs) {
            return res;
        }
        if let Some(res) = services::market_contracts::handle(self, session, effective_service, method, args, kwargs) {
            return res;
        }
        if let Some(res) = services::fast_checkout_and_activity::handle(self, session, effective_service, method, args, kwargs) {
            return res;
        }
        if let Some(res) = services::wallet_serverops::handle(self, session, effective_service, method, args, kwargs) {
            return res;
        }
        if let Some(res) = Box::pin(services::mining_advanced::handle(
            self,
            session,
            effective_service,
            method,
            args,
            kwargs,
        ))
        .await
        {
            return res;
        }
        if let Some(res) = services::industry_pi::handle(
            self, session, effective_service, method, args, kwargs, bound_entry.as_ref(),
        ) {
            return res;
        }
        if let Some(res) = Box::pin(services::agents_missions::handle(
            self,
            session,
            effective_service,
            method,
            args,
            kwargs,
        ))
        .await
        {
            return res;
        }
        if let Some(res) = Box::pin(services::fleet_social::handle(
            self,
            session,
            effective_service,
            method,
            args,
            kwargs,
        ))
        .await
        {
            return res;
        }
        if let Some(res) = Box::pin(services::social_corp_fleet::handle(
            self, session, effective_service, method, args, kwargs, bound_entry.as_ref().map(|b| b.object_id),
        )).await {
            return res;
        }
        if let Some(res) = Box::pin(services::sov_fw_insurgency::handle(
            self,
            session,
            effective_service,
            method,
            args,
            kwargs,
        ))
        .await
        {
            return res;
        }
        if let Some(res) = Box::pin(services::cosmetics_ui_ops::handle(
            self,
            session,
            effective_service,
            method,
            args,
            kwargs,
        ))
        .await
        {
            return res;
        }
        if let Some(res) = Box::pin(services::campaigns_and_milestones::handle(
            self, session, effective_service, method, args, kwargs,
        )).await {
            return res;
        }
        if let Some(res) = Box::pin(services::cosmetics_events::handle(
            self,
            session,
            effective_service,
            method,
            args,
            kwargs,
        ))
        .await
        {
            return res;
        }
        if let Some(res) = services::operations_manager::handle(
            self,
            session,
            effective_service,
            method,
            args,
            kwargs,
        ) {
            return res;
        }
        if let Some(res) = services::corp_registry_ops::handle(
            self,
            session,
            effective_service,
            method,
            args,
            kwargs,
        ) {
            return res;
        }
        if let Some(res) = services::lookup_svc::handle(
            self,
            session,
            effective_service,
            method,
            args,
            kwargs,
        ) {
            return res;
        }
        if let Some(res) = services::pvp_filament_mgr::handle(
            self,
            session,
            effective_service,
            method,
            args,
            kwargs,
        ).await {
            return res;
        }
        if let Some(res) = services::map_service::handle(self, session, effective_service, method, args, kwargs) {
            return res;
        }
        if let Some(res) = services::tourney_service::handle(self, session, effective_service, method, args, kwargs).await {
            return res;
        }
        if effective_service == "ingamereport" {
            if let Some(res) = services::ingamereport_ops::handle(self, session, method, args, kwargs) {
                return res;
            }
        }
        if let Some(res) = Box::pin(services::cluster_macho_misc_services::handle(
            self, session, effective_service, method, args, kwargs,
        ))
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


use crate::services::mining_sites::{
    asteroid_offset, derive_system_site_position_from_sde, region_name, resolve_gas_site,
    resolve_ore_site, ResolvedGasSite, ResolvedOreSite, SiteKind, COSMIC_ORE_SITES, GAS_SITES,
    SOV_PROSPECTING_DEPOSITS,
};
use crate::system_actor::AsteroidState;
use eve_db::{AsteroidRecord, CosmicAnomalyRecord, CosmicSignatureRecord};
use eve_destiny::{Ball, MovementMode};
use eve_sde::TypeId;
use glam::DVec3;

/// Evaluates server-clock respawn timers for depleted Sovereignty Hub prospecting deposits in `sys_id`.
pub async fn tick_sov_prospecting_deposit_respawn(ctx: &ServerContext, sys_id: u32) {
    let has_sov_anom = ctx.db.with_snapshot(|snap| {
        snap.cosmic_anomalies
            .get(&sys_id)
            .is_some_and(|list| list.iter().any(|a| a.anomaly_type == "SovOre"))
    });
    if !has_sov_anom {
        return;
    }

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let sov_asteroid_ids: Vec<i64> = ctx.db.with_snapshot(|snap| {
        snap.asteroids
            .values()
            .filter(|a| a.solar_system_id == sys_id && a.belt_id == 0)
            .map(|a| a.asteroid_id)
            .collect()
    });
    let mut live_ast_states = Vec::with_capacity(sov_asteroid_ids.len());
    for aid in sov_asteroid_ids {
        if let Ok(Some(st)) = sys_handle.get_asteroid_state(aid).await {
            live_ast_states.push((aid, if st.depleted { 0_i64 } else { st.remaining_units as i64 }));
        }
    }

    let mut modelled: HashMap<TypeId, (u64, ResolvedOreSite)> = HashMap::new();
    for def in SOV_PROSPECTING_DEPOSITS {
        if let Ok(site) = resolve_ore_site(&ctx.sde, &def.site) {
            if site.asteroids.iter().all(|a| ctx.sde.types.contains_key(&a.type_id)) {
                modelled.insert(def.upgrade_type_id, (def.respawn_seconds, site));
            }
        }
    }
    let type_vol_rad = |tid: u32| -> (f64, f64) {
        ctx.sde
            .types
            .get(&tid)
            .map(|t| (t.volume.max(0.001), t.radius.max(50.0)))
            .unwrap_or((0.001, 50.0))
    };
    let sov_pos = derive_system_site_position_from_sde(ctx, sys_id, 3, 5.0);
    let now = ctx.now_unix();

    let newly_spawned = ctx.db.with_mut_snapshot(|snap| {
        for &(aid, rem) in &live_ast_states {
            if let Some(ast) = snap.asteroids.get_mut(&aid) {
                ast.quantity = rem;
            }
        }

        let installed_upgrade = snap
            .structures
            .values()
            .filter(|s| s.solar_system_id == sys_id)
            .find_map(|s| {
                snap.items.values().find_map(|it| {
                    if it.location_id == s.structure_id
                        && it.flag == eve_db::InventoryFlag::SubSystemSlot0
                    {
                        modelled.get(&it.type_id)
                    } else {
                        None
                    }
                })
            });
        let default_respawn_secs = installed_upgrade.map(|(r, _)| *r).unwrap_or(36_000);

        let mut newly_cleared_anom_ids = Vec::new();
        if let Some(anoms) = snap.cosmic_anomalies.get_mut(&sys_id) {
            for anom in anoms.iter_mut().filter(|a| a.anomaly_type == "SovOre") {
                let anom_pos = DVec3::from_array(anom.position);
                let any_remaining = snap.asteroids.values().any(|a| {
                    a.solar_system_id == sys_id
                        && a.quantity > 0
                        && DVec3::new(a.position.0, a.position.1, a.position.2).distance(anom_pos)
                            <= 50_000.0
                });
                if !any_remaining {
                    if !anom.cleared {
                        anom.cleared = true;
                        anom.remaining_volume = 0.0;
                        anom.created_unix = now;
                        anom.lifetime_seconds = default_respawn_secs;
                        newly_cleared_anom_ids.push(anom.anomaly_id.clone());
                    } else if anom.lifetime_seconds == 0 {
                        anom.created_unix = now;
                        anom.lifetime_seconds = default_respawn_secs;
                    }
                }
            }
        }
        if !newly_cleared_anom_ids.is_empty() {
            if let Some(sigs) = snap.cosmic_signatures.get_mut(&sys_id) {
                for s in sigs.iter_mut() {
                    if newly_cleared_anom_ids.contains(&s.signature_id) {
                        s.collapsed = true;
                        s.signal_strength = 0.0;
                    }
                }
            }
        }

        let Some((respawn_secs, site)) = installed_upgrade else {
            return Vec::new();
        };
        let has_uncleared = snap
            .cosmic_anomalies
            .get(&sys_id)
            .is_some_and(|list| list.iter().any(|a| a.anomaly_type == "SovOre" && !a.cleared));
        let ready_to_respawn = !has_uncleared
            && snap.cosmic_anomalies.get(&sys_id).is_some_and(|list| {
                list.iter().any(|a| {
                    a.anomaly_type == "SovOre"
                        && a.cleared
                        && now >= a.created_unix.saturating_add(a.lifetime_seconds.max(*respawn_secs))
                })
            });
        if !ready_to_respawn {
            return Vec::new();
        }

        let sov_pos_vec = DVec3::from_array(sov_pos);
        snap.asteroids.retain(|_, a| {
            !(a.solar_system_id == sys_id
                && a.belt_id == 0
                && DVec3::new(a.position.0, a.position.1, a.position.2).distance(sov_pos_vec)
                    <= 50_000.0)
        });
        if let Some(anoms) = snap.cosmic_anomalies.get_mut(&sys_id) {
            anoms.retain(|a| a.anomaly_type != "SovOre");
        }
        if let Some(sigs) = snap.cosmic_signatures.get_mut(&sys_id) {
            sigs.retain(|s| s.sig_group != "SovOre");
        }

        snap.next_entity_id += 1;
        let anom_id = format!("DNG-{}-{}", site.dungeon_id, snap.next_entity_id);
        let mut volume = 0.0_f64;
        let mut spawned = Vec::with_capacity(site.asteroids.len());
        for (i, a) in site.asteroids.iter().enumerate() {
            let off = asteroid_offset(i);
            let pos = [sov_pos[0] + off[0], sov_pos[1] + off[1], sov_pos[2] + off[2]];
            let (uvol, rad) = type_vol_rad(a.type_id);
            volume += a.units as f64 * uvol;
            snap.next_entity_id += 1;
            let aid = snap.next_entity_id;
            snap.asteroids.insert(
                aid,
                AsteroidRecord {
                    asteroid_id: aid,
                    solar_system_id: sys_id,
                    belt_id: 0,
                    type_id: a.type_id,
                    quantity: a.units,
                    initial_quantity: a.units,
                    position: (pos[0], pos[1], pos[2]),
                    radius: rad,
                },
            );
            spawned.push((aid, a.type_id, a.units as u64, uvol, rad, DVec3::from_array(pos)));
        }
        snap.cosmic_anomalies.entry(sys_id).or_default().push(CosmicAnomalyRecord {
            anomaly_id: anom_id.clone(),
            solar_system_id: sys_id,
            name: site.name.clone(),
            anomaly_type: "SovOre".into(),
            position: sov_pos,
            signal_strength: 1.0,
            ore_types: site.ore_types(),
            remaining_volume: volume,
            created_unix: now,
            lifetime_seconds: 0,
            cleared: false,
        });
        snap.cosmic_signatures.entry(sys_id).or_default().push(CosmicSignatureRecord {
            signature_id: anom_id,
            solar_system_id: sys_id,
            sig_group: "SovOre".into(),
            type_name: site.name.clone(),
            position: sov_pos,
            signal_strength: 1.0,
            wormhole_type_id: None,
            destination_system_id: None,
            created_unix: now,
            lifetime_seconds: 0,
            max_mass_kg: 0.0,
            remaining_mass_kg: 0.0,
            max_jump_mass_kg: 0.0,
            collapsed: false,
        });
        spawned
    });

    for (aid, tid, units, uvol, rad, pos) in newly_spawned {
        let ball = Ball {
            id: aid,
            type_id: tid,
            position: pos,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 80_000.0,
            radius: rad,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        };
        let st = AsteroidState {
            asteroid_id: aid,
            belt_id: 0,
            type_id: tid,
            initial_units: units,
            remaining_units: units,
            unit_volume: uvol,
            depleted: false,
            respawn_at_tick: None,
        };
        let _ = sys_handle.spawn_asteroid(st, ball).await;
    }
}

/// Seeds the cosmic ore anomaly, hidden ore signature and gas site for `sys_id`, once per system.
pub async fn seed_system_mining_sites_from_sde(ctx: &ServerContext, sys_id: u32) {
    tick_sov_prospecting_deposit_respawn(ctx, sys_id).await;

    let Some(sys) = ctx.sde.solar_systems.get(&sys_id) else {
        return;
    };
    let region = region_name(&ctx.sde, sys_id).map(str::to_owned);

    enum Planned {
        Ore(SiteKind, ResolvedOreSite),
        Gas(ResolvedGasSite),
        WhGas(u32, &'static str, Vec<(TypeId, i64)>),
    }
    let mut planned: Vec<Planned> = Vec::new();
    for def in COSMIC_ORE_SITES
        .iter()
        .filter(|d| d.band.matches_system(sys))
    {
        match resolve_ore_site(&ctx.sde, def) {
            Ok(site) if site.asteroids.iter().all(|a| ctx.sde.types.contains_key(&a.type_id)) => {
                planned.push(Planned::Ore(def.kind, site))
            }
            Ok(site) => warn!(
                site = site.name,
                "ore site references an asteroid type missing from sde_types; not spawned"
            ),
            Err(e) => warn!(site = def.dungeon_name, error = e, "ore site not spawned"),
        }
    }
    if let Some(def) = GAS_SITES.iter().find(|d| {
        d.band.matches_system(sys)
            && region
                .as_deref()
                .is_some_and(|r| d.region_names.contains(&r))
    }) {
        match resolve_gas_site(&ctx.sde, def) {
            Ok(site) if ctx.sde.types.contains_key(&site.gas_type_id) => {
                planned.push(Planned::Gas(site))
            }
            Ok(site) => warn!(
                site = site.name,
                "gas site references a gas type missing from sde_types; not spawned"
            ),
            Err(e) => warn!(site = def.dungeon_name, error = e, "gas site not spawned"),
        }
    }
    if sys_id >= 31_000_000 {
        if let Some(wh_def) = crate::services::mining_sites::WORMHOLE_GAS_SITES.first() {
            if let Ok((did, clouds)) =
                crate::services::mining_sites::resolve_wormhole_gas_site(&ctx.sde, wh_def)
            {
                planned.push(Planned::WhGas(did, wh_def.dungeon_name, clouds));
            }
        }
    }
    if planned.is_empty() {
        return;
    }

    let type_vol_rad = |tid: u32| -> (f64, f64) {
        ctx.sde
            .types
            .get(&tid)
            .map(|t| (t.volume.max(0.001), t.radius.max(50.0)))
            .unwrap_or((0.001, 50.0))
    };
    let centre_for = |p: &Planned| -> [f64; 3] {
        match p {
            Planned::Ore(SiteKind::Anomaly, _) => derive_system_site_position_from_sde(ctx, sys_id, 0, 4.0),
            Planned::Ore(SiteKind::Signature, _) => derive_system_site_position_from_sde(ctx, sys_id, 1, 6.5),
            Planned::Gas(_) | Planned::WhGas(_, _, _) => derive_system_site_position_from_sde(ctx, sys_id, 2, 8.5),
        }
    };
    let centres: Vec<[f64; 3]> = planned.iter().map(centre_for).collect();

    let now = ctx.now_unix();
    let newly_spawned = ctx.db.with_mut_snapshot(|snap| {
        let already_seeded = snap
            .cosmic_anomalies
            .get(&sys_id)
            .is_some_and(|list| list.iter().any(|a| a.anomaly_type == "OreAnomaly"))
            || snap.cosmic_signatures.get(&sys_id).is_some_and(|list| {
                list.iter()
                    .any(|s| matches!(s.sig_group.as_str(), "OreAnomaly" | "Ore" | "Gas"))
            });
        if already_seeded {
            return Vec::new();
        }

        let mut spawned = Vec::new();
        for (p, centre) in planned.iter().zip(centres.iter().copied()) {
            snap.next_entity_id += 1;
            let seq = snap.next_entity_id;
            let blank_sig = |id: String, group: &str, name: String, strength: f64| CosmicSignatureRecord {
                signature_id: id,
                solar_system_id: sys_id,
                sig_group: group.into(),
                type_name: name,
                signal_strength: strength,
                position: centre,
                wormhole_type_id: None,
                destination_system_id: None,
                created_unix: now,
                lifetime_seconds: 86_400,
                max_mass_kg: 0.0,
                remaining_mass_kg: 0.0,
                max_jump_mass_kg: 0.0,
                collapsed: false,
            };
            let rocks: Vec<(TypeId, i64)> = match p {
                Planned::Ore(kind, site) => {
                    let site_id = format!("DNG-{}-{seq}", site.dungeon_id);
                    match kind {
                        SiteKind::Anomaly => {
                            let volume: f64 = site.asteroids.iter().map(|a| a.units as f64 * type_vol_rad(a.type_id).0).sum();
                            snap.cosmic_anomalies.entry(sys_id).or_default().push(CosmicAnomalyRecord {
                                anomaly_id: site_id.clone(), solar_system_id: sys_id, name: site.name.clone(), anomaly_type: "OreAnomaly".into(),
                                position: centre, signal_strength: 1.0, ore_types: site.ore_types(), remaining_volume: volume, created_unix: now, lifetime_seconds: 86_400, cleared: false,
                            });
                            snap.cosmic_signatures.entry(sys_id).or_default().push(blank_sig(site_id, "OreAnomaly", site.name.clone(), 1.0));
                        }
                        SiteKind::Signature => {
                            snap.cosmic_signatures.entry(sys_id).or_default().push(blank_sig(site_id, "Ore", site.name.clone(), 0.0));
                        }
                    }
                    site.asteroids.iter().map(|a| (a.type_id, a.units)).collect()
                }
                Planned::Gas(site) => {
                    let site_id = format!("DNG-{}-{seq}", site.dungeon_id);
                    snap.cosmic_signatures.entry(sys_id).or_default().push(blank_sig(site_id, "Gas", site.name.clone(), 0.0));
                    site.cloud_units.iter().map(|&u| (site.gas_type_id, u)).collect()
                }
                Planned::WhGas(did, name, clouds) => {
                    let site_id = format!("DNG-{did}-{seq}");
                    snap.cosmic_signatures.entry(sys_id).or_default().push(blank_sig(site_id, "Gas", (*name).to_string(), 0.0));
                    clouds.clone()
                }
            };

            for (i, (tid, units)) in rocks.into_iter().enumerate() {
                let off = asteroid_offset(i);
                let pos = [centre[0] + off[0], centre[1] + off[1], centre[2] + off[2]];
                let (uvol, rad) = type_vol_rad(tid);
                snap.next_entity_id += 1;
                let aid = snap.next_entity_id;
                snap.asteroids.insert(
                    aid,
                    AsteroidRecord {
                        asteroid_id: aid, solar_system_id: sys_id, belt_id: 0, type_id: tid,
                        quantity: units, initial_quantity: units, position: (pos[0], pos[1], pos[2]), radius: rad,
                    },
                );
                spawned.push((aid, tid, units as u64, uvol, rad, DVec3::from_array(pos)));
            }
        }
        spawned
    });

    if !newly_spawned.is_empty() {
        let handle = ctx.systems.get_or_spawn(sys_id);
        for (aid, tid, units, uvol, rad, pos) in newly_spawned {
            let ball = Ball {
                id: aid, type_id: tid, position: pos, velocity: DVec3::ZERO,
                max_velocity: 0.0, inertia: 1.0, mass: 50_000.0, radius: rad,
                speed_fraction: 0.0, mode: MovementMode::Stop,
            };
            let st = AsteroidState {
                asteroid_id: aid, belt_id: 0, type_id: tid, initial_units: units,
                remaining_units: units, unit_volume: uvol, depleted: false, respawn_at_tick: None,
            };
            let _ = handle.spawn_asteroid(st, ball).await;
        }
    }
}

