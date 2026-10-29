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
pub struct LoadServiceState {
    pub active: bool,
    pub config: BTreeMap<String, PyRep>,
    pub runs_started: u64,
    pub total_requests: u64,
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

/// Handles cluster management and operational RPCs targeting `machoNet` and cluster infrastructure services.
pub fn handle(
    ctx: &ServerContext,
    session: &SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let handled = matches!(
        service,
        "machoNet"
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

    if service == "machoNet" {
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
            ctx.db.with_mut_snapshot(|snap| {
                let mut state: LoadServiceState = snap
                    .service_state_blobs
                    .get("load_service_state")
                    .and_then(|s| serde_json::from_str(s).ok())
                    .unwrap_or_default();
                state.active = true;
                state.runs_started += 1;
                state.total_requests += 1;
                state.config.insert("current_load_spec".into(), load_spec);
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
            ctx.db.with_mut_snapshot(|snap| {
                let mut state: LoadServiceState = snap
                    .service_state_blobs
                    .get("load_service_state")
                    .and_then(|s| serde_json::from_str(s).ok())
                    .unwrap_or_default();
                state.active = false;
                state.total_requests += 1;
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
            ctx.db.with_mut_snapshot(|snap| {
                let mut state: LoadServiceState = snap
                    .service_state_blobs
                    .get("load_service_state")
                    .and_then(|s| serde_json::from_str(s).ok())
                    .unwrap_or_default();
                if let Some(PyRep::Dict(entries)) = args.first() {
                    for (k, v) in entries {
                        if let Some(key_str) = k.as_str() {
                            state.config.insert(key_str.to_string(), v.clone());
                        }
                    }
                } else if let Some(key_str) = args.first().and_then(|a| a.as_str()) {
                    let val = args.get(1).cloned().unwrap_or(PyRep::None);
                    state.config.insert(key_str.to_string(), val);
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
                snap.service_state_blobs.insert("active_sessions".into(), "[]".into());
                snap.service_state_blobs.insert("session_registry".into(), "{}".into());
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
            let session_ids: Vec<i64> = match args.get(1) {
                Some(PyRep::List(items) | PyRep::Tuple(items)) => {
                    items.iter().filter_map(|v| v.as_i64()).collect()
                }
                Some(PyRep::Integer(sid)) => vec![*sid],
                _ => Vec::new(),
            };
            for sid in &session_ids {
                ctx.disconnect_session(*sid as u64, "RemoveSession", "Session removed from server");
                ctx.release_session_bindings(*sid as u64);
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
            });
            Some(Ok(PyRep::None))
        }

        ("raffleMgr", "QA_SeedRaffles") => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            let qa_or_gm = eve_machonet::service_roles::ROLE_QA | eve_db::service_roles::ROLE_ADMIN;
            if !session.has_gm_role() && (session.role & qa_or_gm) == 0 {
                return Some(Err(BridgeRpcError {
                    code: "UserError".into(),
                    message: "NotAuthorized".into(),
                }));
            }
            let count = args.first().and_then(|a| a.as_i64()).unwrap_or(1).clamp(1, 100) as usize;
            let now = ctx.now_unix();
            let mut seeded = Vec::new();
            ctx.db.with_mut_snapshot(|snap| {
                for _ in 0..count {
                    let raffle_id = snap.next_entity_id;
                    snap.next_entity_id += 1;
                    // Negative item ID in dedicated keyspace to avoid colliding with player items (100_000_001+)
                    let item_id = -9_000_000_000_i64.saturating_sub(raffle_id.abs());
                    snap.items.insert(
                        item_id,
                        eve_db::InventoryItemRecord {
                            item_id,
                            type_id: 587,
                            owner_id: session.character_id.unwrap_or(90000001),
                            location_id: 60003760,
                            flag: eve_db::InventoryFlag::Hangar,
                            quantity: 1,
                            singleton: true,
                            version: 1,
                            ..Default::default()
                        },
                    );
                    let note_id = -(7_400_000_000_000_i64.saturating_add(raffle_id.abs()));
                    let rec_json = serde_json::json!({
                        "raffle_id": raffle_id,
                        "creator_id": session.character_id.unwrap_or(90000001),
                        "station_id": 60003760_i64,
                        "item_id": item_id,
                        "type_id": 587,
                        "ticket_count": 100,
                        "ticket_price_cents": 100_000,
                        "hypercores_used": 10,
                        "created_unix": now,
                        "expires_unix": now + 86400 * 3,
                        "sold_tickets": {},
                        "winning_ticket": null,
                        "winner_id": null,
                        "item_awarded": false,
                        "expired": false,
                    });
                    snap.owner_notes.insert(
                        note_id,
                        eve_db::OwnerNoteRecord {
                            note_id,
                            character_id: -74,
                            item_id: raffle_id,
                            label: format!("raffle:{raffle_id}"),
                            note: rec_json.to_string(),
                            timestamp_unix: now,
                        },
                    );
                    seeded.push(PyRep::keyval([
                        ("raffle_id", PyRep::Integer(raffle_id)),
                        ("raffleID", PyRep::Integer(raffle_id)),
                        ("owner_id", PyRep::Integer(session.character_id.unwrap_or(90000001))),
                        ("location_id", PyRep::Integer(60003760)),
                        ("item_id", PyRep::Integer(item_id)),
                        ("type_id", PyRep::Integer(587)),
                        ("ticket_count", PyRep::Integer(100)),
                        ("tickets_remaining", PyRep::Integer(100)),
                        ("ticket_price", PyRep::Real(1000.0)),
                        ("hypercores_used", PyRep::Integer(10)),
                        ("expiration_time", PyRep::Integer((now + 86400 * 3) as i64)),
                        ("winning_ticket", PyRep::None),
                        ("winner_id", PyRep::None),
                        ("is_expired", PyRep::Bool(false)),
                        ("is_finished", PyRep::Bool(false)),
                        ("item_awarded", PyRep::Bool(false)),
                    ]));
                }
            });
            Some(Ok(PyRep::List(seeded)))
        }

        ("DB2", "CallProc") => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            if !session.has_gm_role() && (session.role & eve_db::service_roles::ROLE_ADMIN) == 0 {
                return Some(Err(BridgeRpcError {
                    code: "UserError".into(),
                    message: "NotAuthorized".into(),
                }));
            }
            let proc_name = args.first().and_then(|a| a.as_str()).unwrap_or("");
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
                    ctx.db.with_mut_snapshot(|snap| {
                        let mut attacks: Vec<serde_json::Value> = snap
                            .service_state_blobs
                            .get("cluster_attacks")
                            .and_then(|s| serde_json::from_str(s).ok())
                            .unwrap_or_default();
                        if attacks.len() >= 256 {
                            attacks.drain(0..attacks.len().saturating_sub(255));
                        }
                        attacks.push(serde_json::json!({
                            "args": format!("{args:?}"),
                            "timestamp_unix": ctx.now_unix(),
                        }));
                        if let Ok(json) = serde_json::to_string(&attacks) {
                            snap.service_state_blobs.insert("cluster_attacks".into(), json);
                        }
                    });
                    PyRep::None
                }
                _ => {
                    ctx.db.with_mut_snapshot(|snap| {
                        let mut log: Vec<String> = snap
                            .service_state_blobs
                            .get("db2_proc_log")
                            .and_then(|s| serde_json::from_str(s).ok())
                            .unwrap_or_default();
                        if log.len() >= 256 {
                            log.drain(0..log.len().saturating_sub(255));
                        }
                        log.push(format!("{proc_name} called at {}", ctx.now_unix()));
                        if let Ok(json) = serde_json::to_string(&log) {
                            snap.service_state_blobs.insert("db2_proc_log".into(), json);
                        }
                    });
                    PyRep::None
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
            let pos_args: Vec<String> = args.iter().skip(1).take(4).map(|a| format!("{a:?}")).collect();
            let kw_map: Vec<(String, String)> = kwargs
                .iter()
                .map(|(k, v)| (k.clone(), format!("{v:?}")))
                .collect();
            ctx.db.with_mut_snapshot(|snap| {
                let mut log: Vec<serde_json::Value> = snap
                    .service_state_blobs
                    .get("debug_exec_log")
                    .and_then(|s| serde_json::from_str(s).ok())
                    .unwrap_or_default();
                if log.len() >= 256 {
                    log.drain(0..log.len().saturating_sub(255));
                }
                log.push(serde_json::json!({
                    "command": cmd,
                    "pos_args": pos_args,
                    "kwargs": kw_map,
                    "user_id": session.user_id,
                    "timestamp_unix": ctx.now_unix(),
                }));
                if let Ok(json) = serde_json::to_string(&log) {
                    snap.service_state_blobs.insert("debug_exec_log".into(), json);
                }
            });
            Some(Ok(PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(0),
                PyRep::String("ok".into()),
                PyRep::String(format!("executed: {cmd}")),
            ])])))
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

        "GetGlobalConfig" => {
            let (cluster_name, cluster_mode) = ctx
                .cluster_state
                .read()
                .map(|s| (s.cluster_name.clone(), s.cluster_mode.clone()))
                .unwrap_or_else(|_| ("TRANQUILITY".into(), "LIVE".into()));
            let mut configs = vec![
                (PyRep::String("machoVersion".into()), PyRep::Integer(496)),
                (PyRep::String("build".into()), PyRep::Integer(crate::services::auth_cache::V24_01_BUILD)),
                (PyRep::String("clusterName".into()), PyRep::String(cluster_name)),
                (PyRep::String("clusterMode".into()), PyRep::String(cluster_mode)),
            ];
            if let Ok(extra) = ctx.global_config_values.read() {
                for (k, v) in extra.iter() {
                    configs.push((PyRep::String(k.clone()), v.clone()));
                }
            }
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
                crate::services::notify_emitters_batch3::emit_cluster_shutdown_cancelled(ctx, &shutdown_reason);
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
