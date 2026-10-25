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
use std::sync::{LazyLock, RwLock};

/// Cluster management and shutdown state tracked across `machoNet` RPC calls.
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
}

impl Default for MachoClusterState {
    fn default() -> Self {
        let mut props = HashMap::new();
        props.insert("internaladdress".to_string(), PyRep::String("127.0.0.1:26000".into()));
        props.insert("vipMode".to_string(), PyRep::Bool(false));
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
        }
    }
}

pub static CLUSTER_STATE: LazyLock<RwLock<MachoClusterState>> =
    LazyLock::new(|| RwLock::new(MachoClusterState::default()));

/// Retrieve a snapshot of cluster state.
pub fn get_cluster_state() -> MachoClusterState {
    CLUSTER_STATE.read().unwrap().clone()
}

/// Reset cluster state (used for testing).
pub fn reset_cluster_state() {
    let mut g = CLUSTER_STATE.write().unwrap();
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

    match method {
        "GetConnectedClusterMode" => Some(Ok(PyRep::String("LIVE".into()))),

        "GetConnectedClusterName" => Some(Ok(PyRep::String("TRANQUILITY".into()))),

        "GetConnectionProperties" => {
            let props = CLUSTER_STATE.read().unwrap().connection_properties.clone();
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
            let mut configs = vec![
                (PyRep::String("machoVersion".into()), PyRep::Integer(496)),
                (PyRep::String("build".into()), PyRep::Integer(crate::services::auth_cache::V24_01_BUILD)),
                (PyRep::String("clusterName".into()), PyRep::String("TRANQUILITY".into())),
                (PyRep::String("clusterMode".into()), PyRep::String("LIVE".into())),
            ];
            if let Ok(extra) = crate::services::auth_cache::GLOBAL_CONFIG_VALUES.read() {
                for (k, v) in extra.iter() {
                    configs.push((PyRep::String(k.clone()), v.clone()));
                }
            }
            Some(Ok(PyRep::Dict(configs)))
        }

        "GetMetrics" => {
            let service_metrics = PyRep::Dict(vec![
                (
                    PyRep::String("CallsReceived".into()),
                    PyRep::Dict(vec![(PyRep::String("machoNet".into()), PyRep::Integer(10))]),
                ),
                (
                    PyRep::String("CallsSent".into()),
                    PyRep::Dict(vec![(PyRep::String("machoNet".into()), PyRep::Integer(5))]),
                ),
                (
                    PyRep::String("NotificationsReceived".into()),
                    PyRep::Dict(vec![(PyRep::String("machoNet".into()), PyRep::Integer(20))]),
                ),
                (
                    PyRep::String("NotificationsSent".into()),
                    PyRep::Dict(vec![(PyRep::String("machoNet".into()), PyRep::Integer(15))]),
                ),
            ]);
            Some(Ok(PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(1),
                PyRep::String("machoNet".into()),
                service_metrics,
            ])])))
        }

        "GetNodeFromAddress" => {
            let node_id = args
                .get(1)
                .and_then(|a| a.as_i64())
                .or_else(|| kwargs.get("default").and_then(|a| a.as_i64()))
                .or_else(|| kwargs.get("default_node_id").and_then(|a| a.as_i64()))
                .unwrap_or(1);
            Some(Ok(PyRep::Integer(node_id)))
        }

        "GetPolarisExternalTunnelingAddress" => Some(Ok(PyRep::String("127.0.0.1".into()))),

        "CheckACL" => {
            if ServerContext::require_auth(session).is_err() {
                return Some(Err(BridgeRpcError {
                    code: "InsufficientRole".into(),
                    message: "Authentication required for machoNet.CheckACL".into(),
                }));
            }
            let esp_check = kwargs
                .get("espCheck")
                .and_then(|a| a.as_bool())
                .or_else(|| args.get(1).and_then(|a| a.as_bool()))
                .unwrap_or(false);
            let service_name = args.first().and_then(|a| a.as_str()).unwrap_or("");
            let is_restricted = esp_check
                || matches!(
                    service_name,
                    "esp" | "admin" | "gm" | "internal"
                );
            if is_restricted && !session.has_gm_role() {
                // rpc-lint: allow error code
                return Some(Err(BridgeRpcError {
                    code: "InsufficientRole".into(),
                    message: "GM role required for CheckACL on restricted service".into(),
                }));
            }
            Some(Ok(PyRep::Bool(true)))
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

            let mut state = CLUSTER_STATE.write().unwrap();
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
                let mut state = CLUSTER_STATE.write().unwrap();
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
            CLUSTER_STATE.write().unwrap().shutdown_in_progress = true;
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
            let mut state = CLUSTER_STATE.write().unwrap();
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
                let mut state = CLUSTER_STATE.write().unwrap();
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
