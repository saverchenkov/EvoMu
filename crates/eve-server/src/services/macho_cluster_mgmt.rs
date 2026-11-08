//! `macho_cluster_mgmt` — Cluster management and operational RPCs for `machoNet`.
//!
//! Implements cluster-level management and node coordination RPCs:
//! - `machoNet.GetConnectedClusterMode`
//! - `machoNet.GetConnectedClusterName`
//! - `machoNet.GetConnectionProperties`
//! - `machoNet.GetGlobalConfig`
//! - `machoNet.GetMetrics`
//! - `machoNet.GetNodeFromAddress`
//! - `machoNet.GetPolarisExternalTunnelingAddress`
//! - `machoNet.CheckACL`
//! - `machoNet.SetConnectionProperty`
//! - `machoNet.GracefulShutdown`
//! - `machoNet.GracefulShutdownStarted`
//! - `machoNet.GracefulShutdownComplete`
//! - `machoNet.TerminateAllClients`

#![forbid(unsafe_code)]

use crate::router::context::ServerContext;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

/// Cluster load service state persisted in `WorldStateSnapshot::service_state_blobs`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct LoadServiceState {
    pub active: bool,
    pub config: BTreeMap<String, PyRep>,
    pub runs_started: u64,
    pub total_requests: u64,
    pub systems_stepped: u64,
    pub ticks_executed: u64,
    pub sde_types_sampled: u64,
    pub sde_solar_systems_sampled: u64,
    pub world_items_sampled: u64,
    pub world_characters_sampled: u64,
    pub active_sessions_sampled: u64,
    pub last_started_unix: u64,
    pub last_stopped_unix: u64,
}

/// Cluster management and shutdown state tracked on `ServerContext`.
#[derive(Debug, Clone)]
pub struct MachoClusterState {
    pub shutdown_in_progress: bool,
    pub shutdown_reason: Option<String>,
    pub shutdown_timeout_seconds: Option<i64>,
    pub shutdown_graceful: Option<bool>,
    pub origin_node_id: Option<i64>,
    pub shutdown_complete_nodes: Vec<i64>,
    pub clients_terminated: bool,
    pub termination_reason: Option<String>,
    pub connection_properties: HashMap<String, PyRep>,
    pub cluster_mode: String,
    pub cluster_name: String,
    pub polaris_address: String,
    pub node_addresses: HashMap<String, i64>,
    pub calls_received: HashMap<String, i64>,
    pub calls_sent: HashMap<String, i64>,
    pub notifications_received: HashMap<String, i64>,
    pub notifications_sent: HashMap<String, i64>,
}

impl Default for MachoClusterState {
    fn default() -> Self {
        let mut props = HashMap::new();
        props.insert("internaladdress".to_string(), PyRep::String("127.0.0.1:26000".into()));
        props.insert("vipMode".to_string(), PyRep::Bool(false));

        let mut node_addresses = HashMap::new();
        node_addresses.insert("127.0.0.1:26000".to_string(), 1);
        node_addresses.insert("127.0.0.1".to_string(), 1);
        node_addresses.insert("localhost:26000".to_string(), 1);
        node_addresses.insert("localhost".to_string(), 1);
        node_addresses.insert("127.0.0.1:26001".to_string(), 2);

        let mut calls_received = HashMap::new();
        calls_received.insert("machoNet".to_string(), 1);
        let mut calls_sent = HashMap::new();
        calls_sent.insert("machoNet".to_string(), 1);
        let mut notifications_received = HashMap::new();
        notifications_received.insert("machoNet".to_string(), 1);
        let mut notifications_sent = HashMap::new();
        notifications_sent.insert("machoNet".to_string(), 1);

        Self {
            shutdown_in_progress: false,
            shutdown_reason: None,
            shutdown_timeout_seconds: None,
            shutdown_graceful: None,
            origin_node_id: None,
            shutdown_complete_nodes: Vec::new(),
            clients_terminated: false,
            termination_reason: None,
            connection_properties: props,
            cluster_mode: "LIVE".to_string(),
            cluster_name: "TRANQUILITY".to_string(),
            polaris_address: "127.0.0.1".to_string(),
            node_addresses,
            calls_received,
            calls_sent,
            notifications_received,
            notifications_sent,
        }
    }
}

/// Retrieve a snapshot of cluster state from the server context.
pub fn get_cluster_state(ctx: &ServerContext) -> MachoClusterState {
    ctx.cluster_state.read().unwrap().clone()
}

/// Reset cluster state on the server context (used for testing).
pub fn reset_cluster_state(ctx: &ServerContext) {
    let mut g = ctx.cluster_state.write().unwrap();
    *g = MachoClusterState::default();
}

fn apply_load_setting(
    key: &str,
    val: &PyRep,
    ticks: Option<&mut u64>,
    target_sys: Option<&mut Option<u32>>,
    strict: bool,
) -> Result<(), BridgeRpcError> {
    let err = || BridgeRpcError { code: "UserError".into(), message: "InvalidConfig".into() };
    match key {
        "ticks" | "steps" | "iterations" | "duration" => {
            if let Some(n) = val.as_i64().filter(|&n| n > 0) {
                if let Some(t) = ticks { *t = (n as u64).min(100); }
            } else if let Some(f) = val.as_f64().filter(|f| f.is_finite() && *f > 0.0) {
                if let Some(t) = ticks { *t = (f.ceil() as u64).clamp(1, 100); }
            } else if strict {
                return Err(err());
            }
        }
        "solar_system_id" | "solarSystemID" => {
            if let Some(sys) = val.as_i64().and_then(|s| u32::try_from(s).ok()).filter(|&s| s > 0) {
                if let Some(ts) = target_sys { *ts = Some(sys); }
            } else if strict {
                return Err(err());
            }
        }
        _ => {}
    }
    Ok(())
}

/// Handles cluster management and operational RPCs targeting `machoNet` and cluster infrastructure services.
pub fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let handled = matches!(
        service,
        "machoNet"
            | "cluster"
            | "externalQueueMgr"
            | "tcpRawProxyService"
            | "loadService"
            | "sessionMgr"
            | "raffleMgr"
            | "DB2"
            | "debug"
    );
    if !handled {
        return None;
    }
    if let Ok(mut state) = ctx.cluster_state.write() {
        *state.calls_received.entry(service.to_string()).or_insert(0) += 1;
    }
    if matches!(service, "machoNet" | "cluster") {
        if let Some(res) = crate::services::serverops_cluster::handle_admin_cluster_rpc(
            ctx, session, method, args, kwargs,
        ) {
            return Some(res);
        }
        if service == "cluster" {
            return None;
        }
        return handle_macho_net(ctx, session, method, args, kwargs);
    }

    if service == "sessionMgr" {
        if let Err(e) = ServerContext::require_auth(session) {
            return Some(Err(e));
        }
        if !session.has_gm_role() && (session.role & eve_db::service_roles::ROLE_ADMIN) == 0 {
            return Some(Err(BridgeRpcError {
                code: "InsufficientRole".into(),
                message: "UserError: GM_InsufficientRole".into(),
            }));
        }
    }

    match (service, method) {
        ("externalQueueMgr", "PublishProxyEvent") => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            let event_name = args.first().and_then(|a| a.as_str()).unwrap_or("event");
            let args_json: Vec<serde_json::Value> = args
                .iter()
                .take(7)
                .map(|a| serde_json::Value::String(format!("{a:?}")))
                .collect();
            ctx.db.with_mut_snapshot(|snap| {
                let mut events: Vec<serde_json::Value> = snap
                    .service_state_blobs
                    .get("external_queue_events")
                    .and_then(|s| serde_json::from_str(s).ok())
                    .unwrap_or_default();
                if events.len() >= 256 {
                    events.drain(0..events.len().saturating_sub(255));
                }
                events.push(serde_json::json!({
                    "event_name": event_name,
                    "arguments": args_json,
                    "timestamp_unix": ctx.now_unix(),
                }));
                if let Ok(json) = serde_json::to_string(&events) {
                    snap.service_state_blobs.insert("external_queue_events".into(), json);
                }
            });
            Some(Ok(PyRep::None))
        }

        ("tcpRawProxyService", "GetESPTunnelingAddressByNodeID") => {
            let default_addr = "127.0.0.1:26000".to_string();
            let node_map: Vec<(PyRep, PyRep)> = ctx.db.with_snapshot(|snap| {
                if let Some(blob) = snap.service_state_blobs.get("cluster_node_table") {
                    if let Ok(table) = serde_json::from_str::<HashMap<String, String>>(blob) {
                        return table
                            .into_iter()
                            .filter_map(|(k, v)| k.parse::<i64>().ok().map(|nid| (PyRep::Integer(nid), PyRep::String(v))))
                            .collect();
                    }
                }
                vec![
                    (PyRep::Integer(1), PyRep::String("127.0.0.1:26001".into())),
                    (PyRep::Integer(2), PyRep::String("127.0.0.1:26002".into())),
                ]
            });
            Some(Ok(PyRep::Tuple(vec![
                PyRep::String(default_addr),
                PyRep::Dict(node_map),
            ])))
        }

        ("loadService", "StartLoad") => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            if !session.has_gm_role() && (session.role & eve_db::service_roles::ROLE_ADMIN) == 0 {
                return Some(Err(BridgeRpcError {
                    code: "UserError".into(),
                    message: "NotAuthorized".into(),
                }));
            }
            let load_spec = args.first().cloned().unwrap_or(PyRep::None);
            let mut target_sys = session.solar_system_id;
            let mut ticks: u64 = 5;
            let saved_config = ctx.db.with_snapshot(|snap| {
                snap.service_state_blobs
                    .get("load_service_state")
                    .and_then(|s| serde_json::from_str::<LoadServiceState>(s).ok())
                    .map(|st| st.config)
                    .unwrap_or_default()
            });
            for (k, v) in &saved_config {
                let _ = apply_load_setting(k, v, Some(&mut ticks), Some(&mut target_sys), false);
            }
            match &load_spec {
                PyRep::Real(d) => {
                    if !d.is_finite() || *d <= 0.0 {
                        return Some(Err(BridgeRpcError { code: "UserError".into(), message: "InvalidLoadSpec".into() }));
                    }
                    ticks = (d.ceil() as u64).clamp(1, 100);
                }
                PyRep::Dict(entries) => {
                    for (k, v) in entries {
                        if let Some(ks) = k.as_str() {
                            let _ = apply_load_setting(ks, v, Some(&mut ticks), Some(&mut target_sys), false);
                        }
                    }
                }
                PyRep::KeyVal(entries) => {
                    for (k, v) in entries {
                        let _ = apply_load_setting(k, v, Some(&mut ticks), Some(&mut target_sys), false);
                    }
                }
                other => {
                    if let Some(n) = other.as_i64() {
                        ticks = (n.max(1) as u64).min(100);
                    }
                }
            }

            let target_sys = match target_sys {
                Some(sys) => sys,
                None => {
                    return Some(Err(BridgeRpcError {
                        code: "UserError".into(),
                        message: "MissingSolarSystemID".into(),
                    }));
                }
            };

            if !ctx.sde.solar_systems.contains_key(&target_sys) {
                return Some(Err(BridgeRpcError {
                    code: "UserError".into(),
                    message: "InvalidSolarSystemID".into(),
                }));
            }

            let mut stepped_ticks: u64 = 0;
            if tokio::runtime::Handle::try_current().is_ok() {
                let sys_handle = ctx.systems.get_or_spawn(target_sys);
                for _ in 0..ticks {
                    sys_handle.try_step_manual(1.0);
                    stepped_ticks += 1;
                }
            }
            let sde_types_len = ctx.sde.types.len() as u64;
            let sde_systems_len = ctx.sde.solar_systems.len() as u64;
            let active_sessions_len = ctx
                .session_closers
                .read()
                .map(|g| g.len() as u64)
                .unwrap_or(0);
            let now = ctx.now_unix();
            ctx.db.with_mut_snapshot(|snap| {
                let items_len = snap.items.len() as u64;
                let chars_len = snap.characters.len() as u64;
                let mut state: LoadServiceState = snap
                    .service_state_blobs
                    .get("load_service_state")
                    .and_then(|s| serde_json::from_str(s).ok())
                    .unwrap_or_default();
                state.active = true;
                state.runs_started += 1;
                state.total_requests += 1;
                state.systems_stepped += u64::from(stepped_ticks > 0);
                state.ticks_executed += stepped_ticks;
                state.sde_types_sampled = sde_types_len;
                state.sde_solar_systems_sampled = sde_systems_len;
                state.world_items_sampled = items_len;
                state.world_characters_sampled = chars_len;
                state.active_sessions_sampled = active_sessions_len;
                state.last_started_unix = now;
                state.config.insert("current_load_spec".into(), load_spec);
                state.config.insert(
                    "target_solar_system_id".into(),
                    PyRep::Integer(i64::from(target_sys)),
                );
                if let Ok(json) = serde_json::to_string(&state) {
                    snap.service_state_blobs.insert("load_service_state".into(), json);
                }
            });
            Some(Ok(PyRep::None))
        }

        ("loadService", "StopLoad") => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            if !session.has_gm_role() && (session.role & eve_db::service_roles::ROLE_ADMIN) == 0 {
                return Some(Err(BridgeRpcError {
                    code: "UserError".into(),
                    message: "NotAuthorized".into(),
                }));
            }
            let now = ctx.now_unix();
            ctx.db.with_mut_snapshot(|snap| {
                let mut state: LoadServiceState = snap
                    .service_state_blobs
                    .get("load_service_state")
                    .and_then(|s| serde_json::from_str(s).ok())
                    .unwrap_or_default();
                state.active = false;
                state.total_requests += 1;
                state.last_stopped_unix = now;
                if let Ok(json) = serde_json::to_string(&state) {
                    snap.service_state_blobs.insert("load_service_state".into(), json);
                }
            });
            Some(Ok(PyRep::None))
        }

        ("loadService", "SetConfig") => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            if !session.has_gm_role() && (session.role & eve_db::service_roles::ROLE_ADMIN) == 0 {
                return Some(Err(BridgeRpcError {
                    code: "UserError".into(),
                    message: "NotAuthorized".into(),
                }));
            }
            let invalid_cfg = || BridgeRpcError { code: "UserError".into(), message: "InvalidConfig".into() };
            let mut updates: Vec<(String, PyRep)> = Vec::new();
            match args.first() {
                Some(PyRep::Dict(entries)) => {
                    for (k, v) in entries {
                        let Some(ks) = k.as_str().map(str::trim).filter(|s| !s.is_empty()) else {
                            return Some(Err(invalid_cfg()));
                        };
                        if let Err(e) = apply_load_setting(ks, v, None, None, true) { return Some(Err(e)); }
                        updates.push((ks.to_string(), v.clone()));
                    }
                }
                Some(PyRep::KeyVal(entries)) => {
                    for (k, v) in entries {
                        let ks = k.trim();
                        if ks.is_empty() { return Some(Err(invalid_cfg())); }
                        if let Err(e) = apply_load_setting(ks, v, None, None, true) { return Some(Err(e)); }
                        updates.push((ks.to_string(), v.clone()));
                    }
                }
                Some(PyRep::String(key_str)) if !key_str.trim().is_empty() => {
                    let ks = key_str.trim();
                    let val = args.get(1).cloned().unwrap_or(PyRep::None);
                    if let Err(e) = apply_load_setting(ks, &val, None, None, true) { return Some(Err(e)); }
                    updates.push((ks.to_string(), val));
                }
                None | Some(PyRep::None) if !kwargs.is_empty() => {
                    for (k, v) in kwargs {
                        let ks = k.trim();
                        if ks.is_empty() { continue; }
                        if let Err(e) = apply_load_setting(ks, v, None, None, true) { return Some(Err(e)); }
                        updates.push((ks.to_string(), v.clone()));
                    }
                }
                _ => return Some(Err(invalid_cfg())),
            }
            if updates.is_empty() { return Some(Err(invalid_cfg())); }
            ctx.db.with_mut_snapshot(|snap| {
                let mut state: LoadServiceState = snap
                    .service_state_blobs
                    .get("load_service_state")
                    .and_then(|s| serde_json::from_str(s).ok())
                    .unwrap_or_default();
                for (k, v) in updates {
                    state.config.insert(k, v);
                }
                state.total_requests += 1;
                if let Ok(json) = serde_json::to_string(&state) {
                    snap.service_state_blobs.insert("load_service_state".into(), json);
                }
            });
            Some(Ok(PyRep::None))
        }

        ("loadService", "GetTotalStats") => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            if !session.has_gm_role() && (session.role & eve_db::service_roles::ROLE_ADMIN) == 0 {
                return Some(Err(BridgeRpcError {
                    code: "UserError".into(),
                    message: "NotAuthorized".into(),
                }));
            }
            let sde_types_len = ctx.sde.types.len() as i64;
            let sde_systems_len = ctx.sde.solar_systems.len() as i64;
            let active_systems_len = ctx.systems.active_systems_count() as i64;
            let active_sessions_len = ctx
                .session_closers
                .read()
                .map(|g| g.len() as i64)
                .unwrap_or(0);
            let stats = ctx.db.with_snapshot(|snap| {
                let state: LoadServiceState = snap
                    .service_state_blobs
                    .get("load_service_state")
                    .and_then(|s| serde_json::from_str(s).ok())
                    .unwrap_or_default();
                let config_entries: Vec<(PyRep, PyRep)> = state
                    .config
                    .iter()
                    .map(|(k, v)| (PyRep::String(k.clone()), v.clone()))
                    .collect();
                vec![
                    (PyRep::String("active".into()), PyRep::Bool(state.active)),
                    (PyRep::String("runs_started".into()), PyRep::Integer(state.runs_started as i64)),
                    (PyRep::String("total_requests".into()), PyRep::Integer(state.total_requests as i64)),
                    (PyRep::String("systems_stepped".into()), PyRep::Integer(state.systems_stepped as i64)),
                    (PyRep::String("ticks_executed".into()), PyRep::Integer(state.ticks_executed as i64)),
                    (PyRep::String("sde_types_count".into()), PyRep::Integer(sde_types_len)),
                    (PyRep::String("sde_solar_systems_count".into()), PyRep::Integer(sde_systems_len)),
                    (PyRep::String("world_items_count".into()), PyRep::Integer(snap.items.len() as i64)),
                    (PyRep::String("world_characters_count".into()), PyRep::Integer(snap.characters.len() as i64)),
                    (PyRep::String("active_sessions_count".into()), PyRep::Integer(active_sessions_len)),
                    (PyRep::String("active_systems_count".into()), PyRep::Integer(active_systems_len)),
                    (PyRep::String("last_started_unix".into()), PyRep::Integer(state.last_started_unix as i64)),
                    (PyRep::String("last_stopped_unix".into()), PyRep::Integer(state.last_stopped_unix as i64)),
                    (PyRep::String("config".into()), PyRep::Dict(config_entries)),
                ]
            });
            Some(Ok(PyRep::Dict(stats)))
        }

        ("sessionMgr", "EndAllGameSessions") => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            if !session.has_gm_role() && (session.role & eve_db::service_roles::ROLE_ADMIN) == 0 {
                return Some(Err(BridgeRpcError {
                    code: "InsufficientRole".into(),
                    message: "UserError: GM_InsufficientRole".into(),
                }));
            }
            let active_sessions: Vec<u64> = ctx
                .session_closers
                .read()
                .map(|g| g.keys().copied().collect())
                .unwrap_or_default();
            for sid in active_sessions {
                if sid != session.session_id {
                    ctx.disconnect_session(sid, "EndAllGameSessions", "All game sessions ended by administrator");
                    ctx.release_session_bindings(sid);
                }
            }
            ctx.db.with_mut_snapshot(|snap| {
                let mut active: Vec<i64> = snap
                    .service_state_blobs
                    .get("active_sessions")
                    .and_then(|s| serde_json::from_str(s).ok())
                    .unwrap_or_default();
                active.retain(|&s| s == session.session_id as i64);
                if let Ok(json) = serde_json::to_string(&active) {
                    snap.service_state_blobs.insert("active_sessions".into(), json);
                }
                
                let mut registry: std::collections::HashMap<String, serde_json::Value> = snap
                    .service_state_blobs
                    .get("session_registry")
                    .and_then(|s| serde_json::from_str(s).ok())
                    .unwrap_or_default();
                registry.retain(|k, _| k == &session.session_id.to_string());
                if let Ok(json) = serde_json::to_string(&registry) {
                    snap.service_state_blobs.insert("session_registry".into(), json);
                }
            });
            Some(Ok(PyRep::None))
        }

        ("sessionMgr", "RemoveSessionsFromServer") => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            if !session.has_gm_role() && (session.role & eve_db::service_roles::ROLE_ADMIN) == 0 {
                return Some(Err(BridgeRpcError {
                    code: "InsufficientRole".into(),
                    message: "UserError: GM_InsufficientRole".into(),
                }));
            }
            let node_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            if node_id != 0 && node_id != 1i64 {
                // Not our node
                return Some(Ok(PyRep::None));
            }
            let session_ids: Vec<i64> = match args.get(1) {
                Some(PyRep::List(items) | PyRep::Tuple(items)) => {
                    items.iter().filter_map(|v| v.as_i64()).collect()
                }
                Some(PyRep::Integer(sid)) => vec![*sid],
                _ => Vec::new(),
            };
            for sid in &session_ids {
                if let Ok(u_sid) = u64::try_from(*sid) {
                    if u_sid > 0 {
                        ctx.disconnect_session(u_sid, "RemoveSession", "Session removed from server");
                        ctx.release_session_bindings(u_sid);
                    }
                }
            }
            ctx.db.with_mut_snapshot(|snap| {
                let mut active: Vec<i64> = snap
                    .service_state_blobs
                    .get("active_sessions")
                    .and_then(|s| serde_json::from_str(s).ok())
                    .unwrap_or_default();
                active.retain(|s| !session_ids.contains(s));
                if let Ok(json) = serde_json::to_string(&active) {
                    snap.service_state_blobs.insert("active_sessions".into(), json);
                }
                
                let mut registry: std::collections::HashMap<String, serde_json::Value> = snap
                    .service_state_blobs
                    .get("session_registry")
                    .and_then(|s| serde_json::from_str(s).ok())
                    .unwrap_or_default();
                for sid in &session_ids {
                    registry.remove(&sid.to_string());
                }
                if let Ok(json) = serde_json::to_string(&registry) {
                    snap.service_state_blobs.insert("session_registry".into(), json);
                }
                

            });
            Some(Ok(PyRep::None))
        }

        ("sessionMgr", "ConnectToClientService") => {
            let _ = ctx.db.with_snapshot(|snap| snap.characters.len());
            Some(Ok(PyRep::Integer(1)))
        }

        ("sessionMgr", "PerformHorridSessionAttributeUpdate")
        | ("sessionMgr", "UpdateSessionAttributes") => {
            Some(handle_session_attribute_update(ctx, session, method, args, kwargs))
        }

        ("raffleMgr", "QA_SeedRaffles") => {
            Some(crate::services::hypernet_raffle_ops::handle_qa_seed_raffles(ctx, session, args))
        }

        ("raffleMgr", "character_Seeding") => {
            Some(crate::services::hypernet_raffle_ops::handle_character_seeding(
                ctx, session, args, kwargs,
            ))
        }

        ("DB2", "CallProc") => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            let proc_name = args.first().and_then(|a| a.as_str()).unwrap_or("");
            let is_gm_or_admin =
                session.has_gm_role() || (session.role & eve_db::service_roles::ROLE_ADMIN) != 0;
            if !is_gm_or_admin && proc_name != "zcluster.Attacks_Insert" {
                return Some(Err(BridgeRpcError {
                    code: "UserError".into(),
                    message: "NotAuthorized".into(),
                }));
            }
            let result = match proc_name {
                "zcluster.Nodes_PolarisID" => PyRep::Integer(1),
                "zuser.Users_SelectByRole" => {
                    let role_filter = args.get(1).and_then(|a| a.as_i64()).map(|r| r as u64);
                    let rows = ctx.db.with_snapshot(|snap| {
                        snap.accounts
                            .values()
                            .filter(|acc| match role_filter {
                                Some(mask) => (acc.role & mask) != 0 || (mask == 0 && acc.role == 0),
                                None => true,
                            })
                            .map(|acc| vec![PyRep::Integer(acc.user_id as i64), PyRep::String(acc.username.clone())])
                            .collect()
                    });
                    PyRep::Rowset {
                        columns: vec!["userID".into(), "userName".into()],
                        rows,
                    }
                }
                "zcluster.Attacks_Insert" => {
                    if args.len() > 8 {
                        return Some(Err(BridgeRpcError {
                            code: "UserError".into(),
                            message: "InvalidArguments".into(),
                        }));
                    }
                    let bounded_args: Vec<String> = args
                        .iter()
                        .map(|a| {
                            let s = format!("{a:?}");
                            if s.len() > 256 {
                                s[..256].to_string()
                            } else {
                                s
                            }
                        })
                        .collect();
                    let now_u = ctx.now_unix();
                    let uid = session.user_id;
                    let rate_limited = ctx.db.with_mut_snapshot(|snap| {
                        let mut attacks: Vec<serde_json::Value> = snap
                            .service_state_blobs
                            .get("cluster_attacks")
                            .and_then(|s| serde_json::from_str(s).ok())
                            .unwrap_or_default();
                        let recent_for_user = attacks
                            .iter()
                            .filter(|e| {
                                e.get("user_id").and_then(|v| v.as_u64()) == uid
                                    && now_u.saturating_sub(
                                        e.get("timestamp_unix").and_then(|v| v.as_u64()).unwrap_or(0),
                                    ) < 60
                            })
                            .count();
                        if recent_for_user >= 16 {
                            return true;
                        }
                        if attacks.len() >= 256 {
                            attacks.drain(0..attacks.len().saturating_sub(255));
                        }
                        attacks.push(serde_json::json!({
                            "user_id": uid,
                            "args": bounded_args,
                            "timestamp_unix": now_u,
                        }));
                        if let Ok(json) = serde_json::to_string(&attacks) {
                            snap.service_state_blobs.insert("cluster_attacks".into(), json);
                        }
                        false
                    });
                    if rate_limited {
                        return Some(Err(BridgeRpcError {
                            code: "UserError".into(),
                            message: "RateLimited".into(),
                        }));
                    }
                    PyRep::None
                }
                _ => {
                    return Some(Err(BridgeRpcError {
                        code: "UnimplementedRPC".into(),
                        message: "InvalidProcedure".into(),
                    }));
                }
            };
            Some(Ok(result))
        }

        ("debug", "Exec") => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            let admin_or_prog =
                eve_db::service_roles::ROLE_ADMIN | eve_machonet::service_roles::ROLE_PROGRAMMER;
            if !session.has_gm_role() && (session.role & admin_or_prog) == 0 {
                return Some(Err(BridgeRpcError {
                    code: "UserError".into(),
                    message: "NotAuthorized".into(),
                }));
            }
            let cmd = args.first().and_then(|a| a.as_str()).unwrap_or("");
            if cmd == "status" || cmd == "self.stats()" {
                Some(Err(BridgeRpcError {
                    code: "UnimplementedRPC".into(),
                    message: "debug.Exec is not implemented".into(),
                }))
            } else {
                Some(Ok(PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::String("server".into()),
                    PyRep::Integer(1),
                    PyRep::String("OK".into()),
                ])])))
            }
        }

        _ => None,
    }
}

fn handle_macho_net(
    ctx: &ServerContext,
    session: &SessionState,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match method {
        "GetConnectedClusterMode" => {
            let mode = ctx
                .cluster_state
                .read()
                .map(|s| s.cluster_mode.clone())
                .unwrap_or_else(|_| "LIVE".into());
            Some(Ok(PyRep::String(mode)))
        }

        "GetConnectedClusterName" => {
            let name = ctx
                .cluster_state
                .read()
                .map(|s| s.cluster_name.clone())
                .unwrap_or_else(|_| "TRANQUILITY".into());
            Some(Ok(PyRep::String(name)))
        }

        "GetConnectionProperties" => {
            let props = ctx
                .cluster_state
                .read()
                .map(|s| s.connection_properties.clone())
                .unwrap_or_default();
            let mut entries: Vec<(PyRep, PyRep)> = props
                .into_iter()
                .map(|(k, v)| (PyRep::String(k), v))
                .collect();
            entries.sort_by(|a, b| match (&a.0, &b.0) {
                (PyRep::String(sa), PyRep::String(sb)) => sa.cmp(sb),
                _ => std::cmp::Ordering::Equal,
            });
            Some(Ok(PyRep::Dict(entries)))
        }

        "GetEjabberdAddress" => {
            Some(Ok(PyRep::String("127.0.0.1".into())))
        }

        "GetGlobalConfig" => {
            let (cluster_name, cluster_mode) = ctx
                .cluster_state
                .read()
                .map(|s| (s.cluster_name.clone(), s.cluster_mode.clone()))
                .unwrap_or_else(|_| ("TRANQUILITY".into(), "LIVE".into()));
            let mut merged: std::collections::BTreeMap<String, PyRep> =
                std::collections::BTreeMap::new();
            merged.insert("machoVersion".into(), PyRep::Integer(496));
            merged.insert(
                "build".into(),
                PyRep::Integer(crate::services::auth_cache::V24_01_BUILD),
            );
            merged.insert("clusterName".into(), PyRep::String(cluster_name));
            merged.insert("clusterMode".into(), PyRep::String(cluster_mode));
            for (k, v) in crate::bug_reporting::build_client_global_config(ctx, None) {
                merged.insert(k, v);
            }
            let configs = merged
                .into_iter()
                .map(|(k, v)| (PyRep::String(k), v))
                .collect();
            Some(Ok(PyRep::Dict(configs)))
        }

        "GetMetrics" => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            if !session.has_gm_role() {
                return Some(Err(BridgeRpcError {
                    code: "InsufficientRole".into(),
                    message: "machoNet.GetMetrics requires GM role".into(),
                }));
            }
            let state = ctx.cluster_state.read().unwrap();
            let mut cr_entries: Vec<(PyRep, PyRep)> = state
                .calls_received
                .iter()
                .map(|(k, &v)| (PyRep::String(k.clone()), PyRep::Integer(v)))
                .collect();
            cr_entries.sort_by(|a, b| match (&a.0, &b.0) {
                (PyRep::String(sa), PyRep::String(sb)) => sa.cmp(sb),
                _ => std::cmp::Ordering::Equal,
            });
            let mut cs_entries: Vec<(PyRep, PyRep)> = state
                .calls_sent
                .iter()
                .map(|(k, &v)| (PyRep::String(k.clone()), PyRep::Integer(v)))
                .collect();
            cs_entries.sort_by(|a, b| match (&a.0, &b.0) {
                (PyRep::String(sa), PyRep::String(sb)) => sa.cmp(sb),
                _ => std::cmp::Ordering::Equal,
            });
            let mut nr_entries: Vec<(PyRep, PyRep)> = state
                .notifications_received
                .iter()
                .map(|(k, &v)| (PyRep::String(k.clone()), PyRep::Integer(v)))
                .collect();
            nr_entries.sort_by(|a, b| match (&a.0, &b.0) {
                (PyRep::String(sa), PyRep::String(sb)) => sa.cmp(sb),
                _ => std::cmp::Ordering::Equal,
            });
            let notifs_count = ctx
                .notification_records
                .read()
                .map(|n| n.len() as i64)
                .unwrap_or(0)
                .max(*state.notifications_sent.get("machoNet").unwrap_or(&1));
            let ns_entries = vec![(PyRep::String("machoNet".into()), PyRep::Integer(notifs_count))];

            let metrics_keyval = PyRep::keyval(vec![
                ("CallsReceived", PyRep::Dict(cr_entries)),
                ("CallsSent", PyRep::Dict(cs_entries)),
                ("NotificationsReceived", PyRep::Dict(nr_entries)),
                ("NotificationsSent", PyRep::Dict(ns_entries)),
            ]);
            let node_id = 1i64;
            Some(Ok(PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(node_id),
                PyRep::String("machoNet".into()),
                metrics_keyval,
            ])])))
        }

        "GetNodeFromAddress" => {
            let addr_key = match args.first() {
                Some(PyRep::String(s)) => Some(s.clone()),
                Some(PyRep::Tuple(t)) => {
                    if let (Some(PyRep::String(host)), Some(PyRep::Integer(port))) = (t.first(), t.get(1)) {
                        Some(format!("{host}:{port}"))
                    } else if let Some(PyRep::String(host)) = t.first() {
                        Some(host.clone())
                    } else {
                        None
                    }
                }
                _ => None,
            };
            let fallback = args
                .get(1)
                .and_then(|a| a.as_i64())
                .or_else(|| kwargs.get("default").and_then(|a| a.as_i64()))
                .or_else(|| kwargs.get("default_node_id").and_then(|a| a.as_i64()));

            let resolved = {
                let state = ctx.cluster_state.read().unwrap();
                addr_key.as_ref().and_then(|k| state.node_addresses.get(k).copied())
            };

            let node_id = match resolved {
                Some(nid) if nid > 0 => nid,
                _ => match fallback {
                    Some(fb) if fb > 0 => fb,
                    _ => 1,
                },
            };
            Some(Ok(PyRep::Integer(node_id)))
        }

        "GetPolarisExternalTunnelingAddress" => {
            let addr = ctx
                .cluster_state
                .read()
                .map(|s| s.polaris_address.clone())
                .unwrap_or_else(|_| "127.0.0.1".into());
            Some(Ok(PyRep::String(addr)))
        }

        "CheckACL" => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            let ip_str = args
                .first()
                .or_else(|| kwargs.get("ipAddress"))
                .and_then(|a| a.as_str())
                .unwrap_or("");
            let esp_check = kwargs
                .get("espCheck")
                .and_then(|a| a.as_bool())
                .or_else(|| args.get(1).and_then(|a| a.as_bool()))
                .unwrap_or(false);

            if ip_str.is_empty() || ip_str.parse::<std::net::IpAddr>().is_err() {
                return Some(Ok(PyRep::String("Invalid IP address format".into())));
            }

            if esp_check && !session.has_gm_role() {
                return Some(Ok(PyRep::String("GM role required for ESP access".into())));
            }

            Some(Ok(PyRep::None))
        }

        "SetConnectionProperty" => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            if !session.has_gm_role() {
                return Some(Err(BridgeRpcError {
                    code: "InsufficientRole".into(),
                    message: "machoNet.SetConnectionProperty requires GM role".into(),
                }));
            }
            let key = if let Some(k) = args.first().and_then(|a| a.as_str()) {
                k.to_string()
            } else if let Some(k) = kwargs.get("prop").or_else(|| kwargs.get("key")).and_then(|a| a.as_str()) {
                k.to_string()
            } else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "machoNet.SetConnectionProperty requires property key".into(),
                }));
            };
            const ALLOWED_KEYS: &[&str] = &[
                "limit",
                "vipMode",
                "internaladdress",
                "clusterName",
                "clusterMode",
                "motd",
                "proxyNodeID",
                "solServer",
            ];
            if !ALLOWED_KEYS.contains(&key.as_str()) {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: format!("machoNet.SetConnectionProperty: key '{key}' is not allowed"),
                }));
            }
            let val = args
                .get(1)
                .cloned()
                .or_else(|| kwargs.get("val").cloned())
                .or_else(|| kwargs.get("value").cloned())
                .unwrap_or(PyRep::None);

            let mut state = ctx.cluster_state.write().unwrap();
            if key == "clusterName" {
                if let Some(s) = val.as_str() {
                    state.cluster_name = s.to_string();
                }
            } else if key == "clusterMode" {
                if let Some(s) = val.as_str() {
                    state.cluster_mode = s.to_string();
                }
            } else if key == "internaladdress" {
                if let Some(s) = val.as_str() {
                    state.polaris_address = s.to_string();
                }
            }
            if state.connection_properties.len() < 100 || state.connection_properties.contains_key(&key) {
                state.connection_properties.insert(key, val);
            }
            Some(Ok(PyRep::None))
        }

        "GracefulShutdown" => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            if !session.has_gm_role() {
                return Some(Err(BridgeRpcError {
                    code: "InsufficientRole".into(),
                    message: "machoNet.GracefulShutdown requires GM role".into(),
                }));
            }
            let origin_node_id = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
            let shutdown_reason = args
                .get(1)
                .and_then(|a| a.as_str())
                .unwrap_or("Shutdown initiated")
                .to_string();
            let timeout_seconds = args.get(2).and_then(|a| a.as_i64()).unwrap_or(60);
            let graceful = args.get(3).and_then(|a| a.as_bool()).unwrap_or(true);

            {
                let mut state = ctx.cluster_state.write().unwrap();
                state.shutdown_in_progress = timeout_seconds > 0;
                state.origin_node_id = Some(origin_node_id);
                state.shutdown_reason = Some(shutdown_reason.clone());
                state.shutdown_timeout_seconds = Some(timeout_seconds);
                state.shutdown_graceful = Some(graceful);
            }
            if timeout_seconds <= 0 {
                crate::services::notify_emitters_character_services::emit_cluster_shutdown_cancelled(ctx, &shutdown_reason);
            } else if !shutdown_reason.trim().is_empty() {
                crate::services::notify_emitters_skills_trade_godma::emit_server_message_broadcast(
                    ctx,
                    &shutdown_reason,
                );
            }
            Some(Ok(PyRep::None))
        }

        "GracefulShutdownStarted" => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            if !session.has_gm_role() {
                return Some(Err(BridgeRpcError {
                    code: "InsufficientRole".into(),
                    message: "GM role required for cluster lifecycle operations".into(),
                }));
            }
            ctx.cluster_state.write().unwrap().shutdown_in_progress = true;
            Some(Ok(PyRep::None))
        }

        "GracefulShutdownComplete" => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            if !session.has_gm_role() {
                return Some(Err(BridgeRpcError {
                    code: "InsufficientRole".into(),
                    message: "GM role required for cluster lifecycle operations".into(),
                }));
            }
            let node_id = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
            let mut state = ctx.cluster_state.write().unwrap();
            if !state.shutdown_complete_nodes.contains(&node_id) && state.shutdown_complete_nodes.len() < 1000 {
                state.shutdown_complete_nodes.push(node_id);
            }
            Some(Ok(PyRep::None))
        }

        "TerminateAllClients" => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            if !session.has_gm_role() {
                return Some(Err(BridgeRpcError {
                    code: "InsufficientRole".into(),
                    message: "machoNet.TerminateAllClients requires GM role".into(),
                }));
            }
            let reason = args
                .first()
                .and_then(|a| a.as_str())
                .unwrap_or("The cluster is shutting down")
                .to_string();

            {
                let mut state = ctx.cluster_state.write().unwrap();
                state.clients_terminated = true;
                state.termination_reason = Some(reason.clone());
            }

            let active_sessions: Vec<u64> = ctx
                .session_closers
                .read()
                .map(|g| g.keys().copied().collect())
                .unwrap_or_default();
            for sid in active_sessions {
                if sid != session.session_id {
                    ctx.disconnect_session(sid, "ClusterShutdown", &reason);
                }
            }
            Some(Ok(PyRep::None))
        }

        "ReloadClientCodeHash" => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            if !session.has_gm_role() && (session.role & eve_db::service_roles::ROLE_ADMIN) == 0 {
                return Some(Err(BridgeRpcError {
                    code: "UserError".into(),
                    message: "NotAuthorized".into(),
                }));
            }
            let active_hash = ctx.db.with_mut_snapshot(|snap| {
                let mut data: BTreeMap<String, serde_json::Value> = snap
                    .service_state_blobs
                    .get("macho_client_code_hash")
                    .and_then(|s| serde_json::from_str(s).ok())
                    .unwrap_or_default();
                let hash = args
                    .first()
                    .and_then(|a| a.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "b5d84a7e93c12f00".to_string());
                let reloads = data.get("reload_count").and_then(|v| v.as_u64()).unwrap_or(0) + 1;
                data.insert("hash".into(), serde_json::json!(hash));
                data.insert("reload_count".into(), serde_json::json!(reloads));
                data.insert("timestamp_unix".into(), serde_json::json!(ctx.now_unix()));
                if let Ok(json) = serde_json::to_string(&data) {
                    snap.service_state_blobs.insert("macho_client_code_hash".into(), json);
                }
                hash
            });
            Some(Ok(PyRep::String(active_hash)))
        }

        _ => None,
    }
}

fn session_attr_err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError {
        code: code.to_string(),
        message: message.into(),
    }
}

fn parse_opt_pos_i64_attr(key: &str, val: &PyRep) -> Result<Option<i64>, BridgeRpcError> {
    match val {
        PyRep::None => Ok(None),
        PyRep::Integer(v) if *v > 0 => Ok(Some(*v)),
        PyRep::Integer(0) => Ok(None),
        _ => Err(session_attr_err(
            "InvalidSessionAttribute",
            format!("Session attribute '{key}' expects a positive integer or None"),
        )),
    }
}

fn parse_opt_u64_attr(key: &str, val: &PyRep) -> Result<Option<u64>, BridgeRpcError> {
    match val {
        PyRep::None => Ok(None),
        PyRep::Integer(v) if *v > 0 => Ok(Some(*v as u64)),
        PyRep::Integer(0) => Ok(None),
        _ => Err(session_attr_err(
            "InvalidSessionAttribute",
            format!("Session attribute '{key}' expects a non-negative integer or None"),
        )),
    }
}

fn parse_opt_u32_attr(key: &str, val: &PyRep) -> Result<Option<u32>, BridgeRpcError> {
    match val {
        PyRep::None => Ok(None),
        PyRep::Integer(v) if *v > 0 => u32::try_from(*v).map(Some).map_err(|_| {
            session_attr_err(
                "InvalidSessionAttribute",
                format!("Session attribute '{key}' overflows u32: {v}"),
            )
        }),
        PyRep::Integer(0) => Ok(None),
        _ => Err(session_attr_err(
            "InvalidSessionAttribute",
            format!("Session attribute '{key}' expects a positive 32-bit integer or None"),
        )),
    }
}

fn parse_req_u64_attr(key: &str, val: &PyRep) -> Result<u64, BridgeRpcError> {
    match val {
        PyRep::None => Ok(0),
        PyRep::Integer(v) if *v >= 0 => Ok(*v as u64),
        _ => Err(session_attr_err(
            "InvalidSessionAttribute",
            format!("Session attribute '{key}' expects a non-negative integer or None"),
        )),
    }
}

fn parse_req_i64_attr(key: &str, val: &PyRep) -> Result<i64, BridgeRpcError> {
    match val {
        PyRep::Integer(v) => Ok(*v),
        _ => Err(session_attr_err(
            "InvalidSessionAttribute",
            format!("Session attribute '{key}' expects an integer"),
        )),
    }
}

fn parse_req_str_attr(key: &str, val: &PyRep) -> Result<String, BridgeRpcError> {
    match val {
        PyRep::String(s) => Ok(s.clone()),
        PyRep::Bytes(b) => String::from_utf8(b.clone()).map_err(|_| {
            session_attr_err(
                "InvalidSessionAttribute",
                format!("Session attribute '{key}' contains invalid UTF-8 bytes"),
            )
        }),
        _ => Err(session_attr_err(
            "InvalidSessionAttribute",
            format!("Session attribute '{key}' expects a string"),
        )),
    }
}

fn canonical_session_attr_name(raw: &str) -> Option<&'static str> {
    match raw {
        "userid" => Some("userid"),
        "charid" => Some("charid"),
        "corpid" => Some("corpid"),
        "allianceid" => Some("allianceid"),
        "stationid" | "stationid2" => Some("stationid"),
        "structureid" => Some("structureid"),
        "solarsystemid" => Some("solarsystemid"),
        "solarsystemid2" => Some("solarsystemid2"),
        "constellationid" => Some("constellationid"),
        "regionid" => Some("regionid"),
        "shipid" => Some("shipid"),
        "raceID" | "raceid" => Some("raceID"),
        "corprole" => Some("corprole"),
        "corpAccountKey" | "corpaccountkey" => Some("corpAccountKey"),
        "fleetid" => Some("fleetid"),
        "warfactionid" => Some("warfactionid"),
        "role" => Some("role"),
        "languageID" | "languageid" => Some("languageID"),
        "address" => Some("address"),
        "userType" | "usertype" => Some("userType"),
        "maxSessionTime" | "maxsessiontime" => Some("maxSessionTime"),
        "hqID" | "hqid" => Some("hqID"),
        "baseID" | "baseid" => Some("baseID"),
        "rolesAtAll" | "rolesatall" => Some("rolesAtAll"),
        "rolesAtHQ" | "rolesathq" => Some("rolesAtHQ"),
        "rolesAtBase" | "rolesatbase" => Some("rolesAtBase"),
        "rolesAtOther" | "rolesatother" => Some("rolesAtOther"),
        "fleetrole" => Some("fleetrole"),
        "wingid" => Some("wingid"),
        "squadid" => Some("squadid"),
        "cloneStationID" | "clonestationid" => Some("cloneStationID"),
        "locationid" => Some("locationid"),
        _ => None,
    }
}

fn pyrep_to_json(val: &PyRep) -> serde_json::Value {
    match val {
        PyRep::None => serde_json::Value::Null,
        PyRep::Bool(b) => serde_json::json!(*b),
        PyRep::Integer(i) => serde_json::json!(*i),
        PyRep::Real(f) => serde_json::json!(*f),
        PyRep::String(s) => serde_json::json!(s),
        PyRep::Bytes(b) => serde_json::json!(String::from_utf8_lossy(b).to_string()),
        _ => serde_json::Value::Null,
    }
}

fn session_state_to_keyval(s: &SessionState) -> PyRep {
    let opt_i64 = |v: Option<i64>| v.map(PyRep::Integer).unwrap_or(PyRep::None);
    let opt_u32 = |v: Option<u32>| v.map(|n| PyRep::Integer(i64::from(n))).unwrap_or(PyRep::None);
    let in_space = s.station_id.is_none() && s.structure_id.is_none();
    PyRep::keyval([
        ("userid", s.user_id.map(|u| PyRep::Integer(u as i64)).unwrap_or(PyRep::None)),
        ("charid", opt_i64(s.character_id)),
        ("corpid", opt_i64(s.corporation_id)),
        ("allianceid", opt_i64(s.alliance_id)),
        ("stationid", opt_i64(s.station_id)),
        ("structureid", opt_i64(s.structure_id)),
        ("locationid", opt_i64(s.effective_location_id())),
        ("solarsystemid", if in_space { opt_u32(s.solar_system_id) } else { PyRep::None }),
        ("solarsystemid2", opt_u32(s.solar_system_id)),
        ("constellationid", opt_u32(s.constellation_id)),
        ("regionid", opt_u32(s.region_id)),
        ("shipid", opt_i64(s.ship_id)),
        ("raceID", opt_u32(s.race_id)),
        ("corprole", PyRep::Integer(s.corp_role as i64)),
        ("corpAccountKey", PyRep::Integer(i64::from(s.corp_account_key))),
        ("fleetid", opt_i64(s.fleet_id)),
        ("warfactionid", opt_i64(s.war_faction_id)),
        ("role", PyRep::Integer(s.role as i64)),
        ("languageID", PyRep::String(s.language_id.clone())),
        ("address", PyRep::String(s.address.clone())),
        ("userType", PyRep::Integer(s.user_type)),
        ("sessionID", PyRep::Integer(s.session_id as i64)),
    ])
}

fn apply_session_attr(
    state: &mut SessionState,
    canon_key: &str,
    val: &PyRep,
    has_explicit_station: bool,
    has_explicit_structure: bool,
) -> Result<bool, BridgeRpcError> {
    match canon_key {
        "userid" => state.user_id = parse_opt_u64_attr(canon_key, val)?,
        "charid" => state.character_id = parse_opt_pos_i64_attr(canon_key, val)?,
        "corpid" => state.corporation_id = parse_opt_pos_i64_attr(canon_key, val)?,
        "allianceid" => state.alliance_id = parse_opt_pos_i64_attr(canon_key, val)?,
        "stationid" => {
            state.station_id = parse_opt_pos_i64_attr(canon_key, val)?;
            if state.station_id.is_some() && !has_explicit_structure {
                state.structure_id = None;
            }
        }
        "structureid" => {
            state.structure_id = parse_opt_pos_i64_attr(canon_key, val)?;
            if state.structure_id.is_some() && !has_explicit_station {
                state.station_id = None;
            }
        }
        "solarsystemid" | "solarsystemid2" => {
            state.solar_system_id = parse_opt_u32_attr(canon_key, val)?;
        }
        "constellationid" => state.constellation_id = parse_opt_u32_attr(canon_key, val)?,
        "regionid" => state.region_id = parse_opt_u32_attr(canon_key, val)?,
        "shipid" => state.ship_id = parse_opt_pos_i64_attr(canon_key, val)?,
        "raceID" => state.race_id = parse_opt_u32_attr(canon_key, val)?,
        "corprole" => state.corp_role = parse_req_u64_attr(canon_key, val)?,
        "corpAccountKey" => {
            let v = parse_req_i64_attr(canon_key, val)?;
            state.corp_account_key = i32::try_from(v).map_err(|_| {
                session_attr_err(
                    "InvalidSessionAttribute",
                    format!("Session attribute 'corpAccountKey' overflows i32: {v}"),
                )
            })?;
        }
        "fleetid" => state.fleet_id = parse_opt_pos_i64_attr(canon_key, val)?,
        "warfactionid" => state.war_faction_id = parse_opt_pos_i64_attr(canon_key, val)?,
        "role" => state.role = parse_req_u64_attr(canon_key, val)?,
        "languageID" => state.language_id = parse_req_str_attr(canon_key, val)?,
        "address" => state.address = parse_req_str_attr(canon_key, val)?,
        "userType" => state.user_type = parse_req_i64_attr(canon_key, val)?,
        "maxSessionTime" => {
            state.max_session_time = match val {
                PyRep::None => None,
                PyRep::Integer(v) if *v >= 0 => Some(*v),
                _ => {
                    return Err(session_attr_err(
                        "InvalidSessionAttribute",
                        "Session attribute 'maxSessionTime' expects a non-negative integer or None",
                    ));
                }
            };
        }
        "hqID" | "baseID" | "cloneStationID" | "locationid" | "wingid" | "squadid" => {
            let _ = parse_opt_pos_i64_attr(canon_key, val)?;
            return Ok(false);
        }
        "rolesAtAll" | "rolesAtHQ" | "rolesAtBase" | "rolesAtOther" => {
            let _ = parse_req_u64_attr(canon_key, val)?;
            return Ok(false);
        }
        "fleetrole" => {
            let _ = parse_req_i64_attr(canon_key, val)?;
            return Ok(false);
        }
        _ => {
            return Err(session_attr_err(
                "InvalidSessionAttribute",
                format!("Unknown session attribute: {canon_key}"),
            ));
        }
    }
    Ok(true)
}

fn populate_session_from_registry_json(state: &mut SessionState, entry: &serde_json::Map<String, serde_json::Value>) {
    let opt_i64 = |k: &str| entry.get(k).and_then(|v| v.as_i64()).filter(|&n| n > 0);
    let opt_u32 = |k: &str| opt_i64(k).and_then(|n| u32::try_from(n).ok());
    if let Some(uid) = entry.get("userid").or_else(|| entry.get("user_id")).and_then(|v| v.as_u64()) {
        state.user_id = Some(uid);
    }
    if let Some(cid) = opt_i64("charid").or_else(|| opt_i64("character_id")) {
        state.character_id = Some(cid);
    }
    if let Some(corp) = opt_i64("corpid").or_else(|| opt_i64("corporation_id")) {
        state.corporation_id = Some(corp);
    }
    if let Some(ally) = opt_i64("allianceid").or_else(|| opt_i64("alliance_id")) {
        state.alliance_id = Some(ally);
    }
    if let Some(sta) = opt_i64("stationid").or_else(|| opt_i64("station_id")) {
        state.station_id = Some(sta);
    }
    if let Some(st) = opt_i64("structureid").or_else(|| opt_i64("structure_id")) {
        state.structure_id = Some(st);
    }
    if let Some(sys) = opt_u32("solarsystemid2").or_else(|| opt_u32("solarsystemid")).or_else(|| opt_u32("solar_system_id")) {
        state.solar_system_id = Some(sys);
    }
    if let Some(con) = opt_u32("constellationid").or_else(|| opt_u32("constellation_id")) {
        state.constellation_id = Some(con);
    }
    if let Some(reg) = opt_u32("regionid").or_else(|| opt_u32("region_id")) {
        state.region_id = Some(reg);
    }
    if let Some(ship) = opt_i64("shipid").or_else(|| opt_i64("ship_id")) {
        state.ship_id = Some(ship);
    }
    if let Some(race) = opt_u32("raceID").or_else(|| opt_u32("race_id")) {
        state.race_id = Some(race);
    }
    if let Some(cr) = entry.get("corprole").or_else(|| entry.get("corp_role")).and_then(|v| v.as_u64()) {
        state.corp_role = cr;
    }
    if let Some(key) = entry
        .get("corpAccountKey")
        .and_then(|v| v.as_i64())
        .and_then(|v| i32::try_from(v).ok())
    {
        state.corp_account_key = key;
    }
    if let Some(fl) = opt_i64("fleetid").or_else(|| opt_i64("fleet_id")) {
        state.fleet_id = Some(fl);
    }
    if let Some(wf) = opt_i64("warfactionid").or_else(|| opt_i64("war_faction_id")) {
        state.war_faction_id = Some(wf);
    }
    if let Some(role) = entry.get("role").and_then(|v| v.as_u64()) {
        state.role = role;
    }
    if let Some(lang) = entry.get("languageID").or_else(|| entry.get("language_id")).and_then(|v| v.as_str()) {
        state.language_id = lang.to_string();
    }
    if let Some(addr) = entry.get("address").and_then(|v| v.as_str()) {
        state.address = addr.to_string();
    }
    if let Some(ut) = entry.get("userType").and_then(|v| v.as_i64()) {
        state.user_type = ut;
    }
}

fn handle_session_attribute_update(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    ServerContext::require_auth(session)?;
    if !session.has_gm_role() && (session.role & eve_db::service_roles::ROLE_ADMIN) == 0 {
        return Err(session_attr_err("InsufficientRole", "UserError: GM_InsufficientRole"));
    }

    let (target_sid, rest) = if let Some(PyRep::Integer(sid)) = args.first() {
        let parsed = u64::try_from(*sid).ok().filter(|&v| v > 0).ok_or_else(|| {
            session_attr_err("InvalidSessionId", format!("Invalid target sessionID: {sid}"))
        })?;
        (parsed, &args[1..])
    } else if let Some(sid_rep) = kwargs.get("sessionID") {
        let sid = sid_rep.as_i64().unwrap_or(0);
        let parsed = u64::try_from(sid).ok().filter(|&v| v > 0).ok_or_else(|| {
            session_attr_err("InvalidSessionId", format!("Invalid target sessionID: {sid}"))
        })?;
        (parsed, args)
    } else {
        (session.session_id, args)
    };

    let mut raw_updates: Vec<(String, PyRep)> = Vec::new();
    match rest {
        [] => {}
        [PyRep::Dict(entries)] => {
            for (k, v) in entries {
                let key = k.as_str().ok_or_else(|| {
                    session_attr_err("InvalidArguments", "Session attribute dictionary keys must be strings")
                })?;
                raw_updates.push((key.to_string(), v.clone()));
            }
        }
        [PyRep::String(k), val] => {
            raw_updates.push((k.clone(), val.clone()));
        }
        [PyRep::Bytes(k), val] => {
            let key = String::from_utf8(k.clone()).map_err(|_| {
                session_attr_err("InvalidArguments", "Session attribute name must be valid UTF-8")
            })?;
            raw_updates.push((key, val.clone()));
        }
        _ => {
            return Err(session_attr_err(
                "InvalidArguments",
                format!("{method} expects ([sessionID], attrsDict | key, value)"),
            ));
        }
    }

    for (k, v) in kwargs {
        if k != "sessionID" {
            raw_updates.push((k.clone(), v.clone()));
        }
    }

    if raw_updates.is_empty() {
        return Err(session_attr_err(
            "InvalidArguments",
            format!("{method} requires at least one session attribute to update"),
        ));
    }

    let mut canonical_updates: Vec<(&'static str, PyRep)> = Vec::with_capacity(raw_updates.len());
    for (raw_key, val) in raw_updates {
        let canon = canonical_session_attr_name(&raw_key).ok_or_else(|| {
            session_attr_err(
                "InvalidSessionAttribute",
                format!("Unknown session attribute: {raw_key}"),
            )
        })?;
        canonical_updates.push((canon, val));
    }

    let has_explicit_station = canonical_updates.iter().any(|(k, _)| *k == "stationid");
    let has_explicit_structure = canonical_updates.iter().any(|(k, _)| *k == "structureid");

    let online_match = ctx
        .db
        .online_sessions_snapshot()
        .into_iter()
        .find(|&(_, sid)| sid == target_sid)
        .and_then(|(uid, _)| ctx.db.get_online_session(uid));

    let is_caller = target_sid == session.session_id;
    let now_unix = ctx.now_unix();

    let (target_state_before, target_state_after, extra_diffs, remote_corp_sync): (
        SessionState,
        SessionState,
        Vec<(&'static str, PyRep, PyRep)>,
        Option<(i64, i64, Option<i64>, u64)>,
    ) = ctx.db.try_with_mut_snapshot(|snap| {
        let mut registry: HashMap<String, serde_json::Value> = snap
            .service_state_blobs
            .get("session_registry")
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        let active_sessions: Vec<i64> = snap
            .service_state_blobs
            .get("active_sessions")
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();

        let sid_key = target_sid.to_string();
        let exists = is_caller
            || online_match.is_some()
            || registry.contains_key(&sid_key)
            || active_sessions.contains(&(target_sid as i64));
        if !exists {
            return Err(session_attr_err(
                "SessionNotFound",
                format!("Session {target_sid} not found"),
            ));
        }

        let mut target_state = if is_caller {
            session.clone()
        } else {
            let mut st = SessionState::new(target_sid);
            if let Some(ref online) = online_match {
                st.user_id = Some(online.user_id);
                st.character_id = online.character_id;
            }
            if let Some(serde_json::Value::Object(map)) = registry.get(&sid_key) {
                populate_session_from_registry_json(&mut st, map);
            }
            if let Some(cid) = st.character_id {
                if let Some(ch) = snap.characters.get(&cid) {
                    st.user_id.get_or_insert(ch.user_id);
                    st.corporation_id.get_or_insert(ch.corporation_id);
                    st.alliance_id = st.alliance_id.or(ch.alliance_id);
                    st.war_faction_id = st.war_faction_id.or(ch.war_faction_id);
                    st.station_id = st.station_id.or(ch.station_id);
                    st.structure_id = st.structure_id.or(ch.structure_id);
                    st.solar_system_id.get_or_insert(ch.solar_system_id);
                    st.constellation_id.get_or_insert(ch.constellation_id);
                    st.region_id.get_or_insert(ch.region_id);
                    if st.ship_id.is_none() && ch.active_ship_id > 0 {
                        st.ship_id = Some(ch.active_ship_id);
                    }
                    st.race_id.get_or_insert(ch.race_id);
                }
            }
            st
        };

        let before = target_state.clone();
        let mut reg_map = match registry.get(&sid_key) {
            Some(serde_json::Value::Object(map)) => map.clone(),
            _ => serde_json::Map::new(),
        };

        let mut extra_changes: Vec<(&'static str, PyRep, PyRep)> = Vec::new();
        for (canon, val) in &canonical_updates {
            let handled_by_struct = apply_session_attr(
                &mut target_state,
                canon,
                val,
                has_explicit_station,
                has_explicit_structure,
            )?;
            if !handled_by_struct {
                let old_val = reg_map
                    .get(*canon)
                    .and_then(|v| v.as_i64())
                    .map(PyRep::Integer)
                    .unwrap_or(PyRep::None);
                if &old_val != val {
                    extra_changes.push((*canon, old_val, val.clone()));
                }
            }
            reg_map.insert((*canon).to_string(), pyrep_to_json(val));
        }

        reg_map.insert("session_id".into(), serde_json::json!(target_sid));
        reg_map.insert("user_id".into(), serde_json::json!(target_state.user_id));
        reg_map.insert("character_id".into(), serde_json::json!(target_state.character_id));
        reg_map.insert("corporation_id".into(), serde_json::json!(target_state.corporation_id));
        reg_map.insert("alliance_id".into(), serde_json::json!(target_state.alliance_id));
        reg_map.insert("station_id".into(), serde_json::json!(target_state.station_id));
        reg_map.insert("structure_id".into(), serde_json::json!(target_state.structure_id));
        reg_map.insert("solar_system_id".into(), serde_json::json!(target_state.solar_system_id));
        reg_map.insert("constellation_id".into(), serde_json::json!(target_state.constellation_id));
        reg_map.insert("region_id".into(), serde_json::json!(target_state.region_id));
        reg_map.insert("ship_id".into(), serde_json::json!(target_state.ship_id));
        reg_map.insert("corp_role".into(), serde_json::json!(target_state.corp_role));
        reg_map.insert("fleet_id".into(), serde_json::json!(target_state.fleet_id));
        reg_map.insert("war_faction_id".into(), serde_json::json!(target_state.war_faction_id));
        reg_map.insert("role".into(), serde_json::json!(target_state.role));
        reg_map.insert("language_id".into(), serde_json::json!(target_state.language_id));
        reg_map.insert("updated_at".into(), serde_json::json!(now_unix));
        registry.insert(sid_key, serde_json::Value::Object(reg_map));
        if let Ok(json) = serde_json::to_string(&registry) {
            snap.service_state_blobs.insert("session_registry".into(), json);
        }

        // Sync persisted character & account fields for explicitly updated attributes.
        if let Some(cid) = target_state.character_id {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                for (canon, _) in &canonical_updates {
                    match *canon {
                        "corpid" => {
                            if let Some(corp) = target_state.corporation_id {
                                ch.corporation_id = corp;
                            }
                        }
                        "allianceid" => ch.alliance_id = target_state.alliance_id,
                        "warfactionid" => ch.war_faction_id = target_state.war_faction_id,
                        "stationid" => ch.station_id = target_state.station_id,
                        "structureid" => ch.structure_id = target_state.structure_id,
                        "solarsystemid" | "solarsystemid2" => {
                            if let Some(sys) = target_state.solar_system_id {
                                ch.solar_system_id = sys;
                            }
                        }
                        "constellationid" => {
                            if let Some(con) = target_state.constellation_id {
                                ch.constellation_id = con;
                            }
                        }
                        "regionid" => {
                            if let Some(reg) = target_state.region_id {
                                ch.region_id = reg;
                            }
                        }
                        "shipid" => {
                            if let Some(ship) = target_state.ship_id {
                                ch.active_ship_id = ship;
                            }
                        }
                        "raceID" => {
                            if let Some(race) = target_state.race_id {
                                ch.race_id = race;
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        if canonical_updates.iter().any(|(k, _)| *k == "role") {
            if let Some(uid) = target_state.user_id {
                if let Some(acc) = snap.accounts.values_mut().find(|a| a.user_id == uid) {
                    acc.role = target_state.role;
                }
            }
        }

        let corp_sync = if !is_caller
            && canonical_updates
                .iter()
                .any(|(k, _)| matches!(*k, "corpid" | "allianceid" | "corprole"))
        {
            target_state.character_id.filter(|&cid| cid > 0).map(|cid| {
                (
                    cid,
                    target_state.corporation_id.unwrap_or(1_000_125),
                    target_state.alliance_id,
                    target_state.corp_role,
                )
            })
        } else {
            None
        };

        Ok((before, target_state, extra_changes, corp_sync))
    })?;

    if let Some((cid, corp_id, alliance_id, corp_role)) = remote_corp_sync {
        crate::services::fleet_social::fleet_social_persistence::with_social_state(ctx, |s| {
            s.forced_session_corp_sync
                .insert(cid, (corp_id, alliance_id, corp_role));
        });
    }

    if is_caller {
        *session = target_state_after.clone();
    }

    let mut diff_entries: Vec<(PyRep, PyRep)> = target_state_before
        .compute_diff(&target_state_after)
        .into_iter()
        .map(|(k, (old_v, new_v))| (PyRep::String(k), PyRep::Tuple(vec![old_v, new_v])))
        .collect();
    for (k, old_v, new_v) in extra_diffs {
        if !diff_entries
            .iter()
            .any(|(existing_k, _)| existing_k.as_str() == Some(k))
        {
            diff_entries.push((PyRep::String(k.to_string()), PyRep::Tuple(vec![old_v, new_v])));
        }
    }

    if !diff_entries.is_empty() {
        let sess_kv = session_state_to_keyval(&target_state_after);
        let notify_args = vec![PyRep::Bool(false), sess_kv, PyRep::Dict(diff_entries)];
        if is_caller {
            session.push_notification("OnSessionChanged", notify_args.clone());
        }
        if let Some(cid) = target_state_after.character_id.filter(|&id| id > 0) {
            ctx.notify(
                crate::router::NotificationTarget::Character(cid),
                "OnSessionChanged",
                "charid",
                notify_args,
            );
        }
    }

    Ok(PyRep::Bool(true))
}

#[cfg(test)] #[path = "macho_cluster_mgmt_tests.rs"]
mod macho_cluster_mgmt_tests;

