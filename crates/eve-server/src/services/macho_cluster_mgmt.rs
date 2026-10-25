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
use std::collections::HashMap;

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

/// Handles cluster management and operational RPCs targeting `machoNet`.
pub fn handle(
    ctx: &ServerContext,
    session: &SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if service != "machoNet" {
        return None;
    }

    if let Ok(mut state) = ctx.cluster_state.write() {
        *state.calls_received.entry("machoNet".to_string()).or_insert(0) += 1;
    }

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
            if let Ok(extra) = crate::services::auth_cache::GLOBAL_CONFIG_VALUES.read() {
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
                state.shutdown_in_progress = true;
                state.origin_node_id = Some(origin_node_id);
                state.shutdown_reason = Some(shutdown_reason);
                state.shutdown_timeout_seconds = Some(timeout_seconds);
                state.shutdown_graceful = Some(graceful);
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

        _ => None,
    }
}
