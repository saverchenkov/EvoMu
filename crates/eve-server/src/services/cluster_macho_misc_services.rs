//! Cluster Miscellaneous and Character Setting RPC Service Handlers.
//!
//! Provides solid implementations for:
//! - `charMgr.SaveCharacterSetting` and `charMgr.DeleteCharacterSetting`
//!
//! All unhandled, hollow, or unverified RPC arms return `None` to fall through to `UnimplementedRPC`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_db::{OwnerNoteRecord, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

const MISC_STATE_NOTE_ID: i64 = -88_000_001;
const MISC_STATE_OWNER_ID: i64 = -88;

const MAX_SETTINGS_PER_CHAR: usize = 500;
const MAX_SETTING_KEY_LEN: usize = 128;

#[derive(Default, Clone, Serialize, Deserialize)]
struct PersistedClusterMiscState {
    #[serde(default)]
    char_settings: HashMap<i64, BTreeMap<String, PyRep>>,
}

#[derive(Default)]
pub struct ClusterMiscRuntimeState {
    pub char_settings: HashMap<i64, BTreeMap<String, PyRep>>,
}

impl ClusterMiscRuntimeState {
    pub fn from_db(db: &Arc<WorldRepository>) -> Self {
        let mut st = Self::default();
        let persisted: Option<PersistedClusterMiscState> = db.with_snapshot(|snap| {
            snap.owner_notes
                .get(&MISC_STATE_NOTE_ID)
                .and_then(|n| serde_json::from_str(&n.note).ok())
        });
        if let Some(p) = persisted {
            st.char_settings = p.char_settings;
        }
        st
    }
}

pub(crate) fn with_misc_state<R>(
    ctx: &ServerContext,
    f: impl FnOnce(&mut ClusterMiscRuntimeState) -> R,
) -> R {
    let mut guard = ctx
        .cluster_misc_state
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    f(&mut guard)
}

pub(crate) fn persist_misc_state(ctx: &ServerContext) {
    // Clone state under misc lock, release misc lock BEFORE acquiring DB lock (prevents lock-order inversion).
    let p = with_misc_state(ctx, |st| PersistedClusterMiscState {
        char_settings: st.char_settings.clone(),
    });
    if let Ok(json) = serde_json::to_string(&p) {
        let _ = ctx.db.try_with_mut_snapshot(|snap| {
            snap.owner_notes.insert(
                MISC_STATE_NOTE_ID,
                OwnerNoteRecord {
                    note_id: MISC_STATE_NOTE_ID,
                    character_id: MISC_STATE_OWNER_ID,
                    item_id: 0,
                    label: "__cluster_misc_state__".into(),
                    note: json,
                    timestamp_unix: 0,
                },
            );
            Ok::<(), String>(())
        });
    }
}

pub(crate) fn get_character_settings_dict(ctx: &ServerContext, char_id: i64) -> PyRep {
    with_misc_state(ctx, |st| {
        let settings = st.char_settings.get(&char_id).cloned().unwrap_or_default();
        let entries: Vec<(PyRep, PyRep)> = settings
            .into_iter()
            .map(|(k, v)| (PyRep::String(k), v))
            .collect();
        PyRep::Dict(entries)
    })
}

fn rpc_err(code: impl Into<String>, msg: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError {
        code: code.into(),
        message: msg.into(),
    }
}

/// Dispatches requests for miscellaneous cluster RPC services.
///
/// Only solid, accepted rows are implemented here. Any other RPC returns `None`
/// to fall through to the global `UnimplementedRPC` handler.
pub async fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &std::collections::HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        // --------------------------------------------------------------------
        // charMgr: Client UI settings persistence
        // --------------------------------------------------------------------
        ("charMgr", "SaveCharacterSetting") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(key) = args.first().and_then(|v| v.as_str()).filter(|s| !s.is_empty()) else {
                return Some(Err(rpc_err("CustomError", "SaveCharacterSetting requires a non-empty setting key")));
            };
            if key.len() > MAX_SETTING_KEY_LEN {
                return Some(Err(rpc_err("CustomError", "Setting key exceeds maximum length")));
            }
            let val = args.get(1).cloned().unwrap_or(PyRep::Bool(true));
            with_misc_state(ctx, |st| {
                let user_settings = st.char_settings.entry(char_id).or_default();
                if user_settings.len() < MAX_SETTINGS_PER_CHAR || user_settings.contains_key(key) {
                    user_settings.insert(key.to_owned(), val);
                }
            });
            persist_misc_state(ctx);
            Some(Ok(PyRep::Bool(true)))
        }

        ("charMgr", "DeleteCharacterSetting") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(key) = args.first().and_then(|v| v.as_str()).filter(|s| !s.is_empty()) else {
                return Some(Err(rpc_err("CustomError", "DeleteCharacterSetting requires a non-empty setting key")));
            };
            let _ = with_misc_state(ctx, |st| {
                st.char_settings
                    .get_mut(&char_id)
                    .and_then(|m| m.remove(key))
            });
            persist_misc_state(ctx);
            Some(Ok(PyRep::Bool(true)))
        }

        _ => None,
    }
}
