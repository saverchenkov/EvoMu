//! `client_login` — services the real V24.01 client calls right after the `GPSTransport` handshake,
//! before a character is selected (T-6 `journey:launch_login`).
//!
//! Each arm mirrors the V24 server-side method the client invokes (from the decompiled client):
//! - `alert.BeanCount(stackIDHash)` / `alert.SendClientStackTraceAlert(stackID, stackTrace, mode,
//!   nextErrorKeyHash)` / `alert.BeanDelivery(beans)` — client error telemetry (`carbon/common/script/sys/alert.pyj`).
//! - `config.GetBlackListedPlanets()` — loaded by `eveCfg.LoadPlanetBlacklist` at login.
//! - `air_npe.is_air_npe_enabled()` — global AIR NPE switch cached by `storylines/client/airnpe.pyj`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;
use tracing::info;

/// `alert.LOGGING_BATCHED`: the client counts repeats and delivers them via `BeanDelivery`.
const LOGGING_BATCHED: &str = "B";

/// Upper bound on entries in each alert registry. Hashes and counts come from clients, so the
/// registries stop growing at this size (a new stack trace still gets an errorID, unregistered).
pub const ALERT_REGISTRY_CAP: usize = 1024;
pub const MAX_ALERT_ENTRIES: usize = ALERT_REGISTRY_CAP;

/// Per-`ServerContext` alert telemetry state (`stackIDHash -> (errorID, logMode)` and `errorID -> count`).
pub struct AlertTelemetryState {
    pub stack_traces: HashMap<i64, (i64, &'static str)>,
    pub bean_counts: HashMap<i64, i64>,
    pub next_error_id: i64,
}

impl Default for AlertTelemetryState {
    fn default() -> Self {
        Self {
            stack_traces: HashMap::new(),
            bean_counts: HashMap::new(),
            next_error_id: 1,
        }
    }
}

/// Returns the delivered occurrence count for `error_id` on `ctx` (0 when never delivered).
pub fn delivered_bean_count(ctx: &ServerContext, error_id: i64) -> i64 {
    let mem = ctx
        .alert_telemetry
        .lock()
        .map(|st| st.bean_counts.get(&error_id).copied().unwrap_or(0))
        .unwrap_or(0);
    if mem > 0 {
        return mem;
    }
    ctx.db.with_snapshot(|snap| {
        snap.cluster_and_runtime
            .alert_bean_counts
            .get(&error_id.to_string())
            .copied()
            .unwrap_or(0)
    })
}

fn log_mode_tuple(entry: Option<(i64, &'static str)>) -> PyRep {
    match entry {
        Some((error_id, mode)) => {
            PyRep::Tuple(vec![PyRep::Integer(error_id), PyRep::ascii_bytes(mode)])
        }
        None => PyRep::Tuple(vec![PyRep::None, PyRep::None]),
    }
}

fn log_mode_override(ctx: &ServerContext, error_id: i64) -> Option<String> {
    ctx.db.with_snapshot(|snap| {
        snap.cluster_and_runtime
            .alert_log_modes
            .get(&error_id.to_string())
            .cloned()
    })
}

fn persist_bean_counts(ctx: &ServerContext, beans: &[(i64, i64)]) {
    if beans.is_empty() {
        return;
    }
    ctx.db.with_mut_snapshot(|snap| {
        let persisted = &mut snap.cluster_and_runtime.alert_bean_counts;
        for &(error_id, n) in beans {
            let key = error_id.to_string();
            if persisted.len() >= ALERT_REGISTRY_CAP && !persisted.contains_key(&key) {
                if let Some(oldest) = persisted.keys().next().cloned() {
                    persisted.remove(&oldest);
                }
            }
            let c = persisted.entry(key).or_insert(0);
            *c = c.saturating_add(n);
        }
    });
}

fn handle_get_character_selection_data(
    ctx: &ServerContext,
    session: &SessionState,
) -> Result<PyRep, BridgeRpcError> {
    crate::services::char_selection::get_character_selection_data(ctx, session)
}

fn handle_get_character_settings(ctx: &ServerContext, char_id: i64) -> PyRep {
    crate::services::cluster_macho_misc_services::get_character_settings_dict(ctx, char_id)
}


/// DERIVED: a marshalled Python 2.7 code object named `SettingsInfo` whose body is
/// `return []` (`BUILD_LIST 0; RETURN_VALUE`). EvoMu collects no client settings statistics, so
/// the script returns an empty list and the client skips `charMgr.LogSettings`.
const SETTINGS_INFO_CODE: &[u8] = b"c\x00\x00\x00\x00\x00\x00\x00\x00\x01\x00\x00\x00\x43\x00\x00\x00s\x04\x00\x00\x00g\x00\x00S(\x01\x00\x00\x00N(\x00\x00\x00\x00(\x00\x00\x00\x00(\x00\x00\x00\x00(\x00\x00\x00\x00s\x00\x00\x00\x00s\x0c\x00\x00\x00SettingsInfo\x01\x00\x00\x00s\x00\x00\x00\x00";

/// `charMgr.GetSettingsInfo()` → `(code, signature)`. `SettingsSvc.UpdateSettingsStatistics`
/// runs `code, verified = Crypto.Verify(...)`, raises unless verified, then
/// `marshal.loads(code)` becomes `SettingsInfo.__code__` and `LogSettings(ret)` is sent only when
/// `len(ret) > 0` (`carbonui/services/settingsSvc.pyj:205-211`). Under the Placebo crypto pack
/// `verify(signedData)` returns `(signedData[0], signedData[1] == 0)` (`evecrypto/placebo.pyj:22`).
pub fn settings_info_blob() -> PyRep {
    PyRep::Tuple(vec![PyRep::Bytes(SETTINGS_INFO_CODE.to_vec()), PyRep::Integer(0)])
}

pub async fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
    caller_trace: Option<&str>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("alert", "BeanCount") => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            handle_alert_bean_count(ctx, args)
        }
        ("alert", "SendClientStackTraceAlert") => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            let (hash, stack_key) = match args.first() {
                Some(PyRep::Tuple(items) | PyRep::List(items)) => (
                    items.first().and_then(|v| v.as_i64()),
                    items.get(1).and_then(|v| v.as_str()).unwrap_or(""),
                ),
                _ => (None, ""),
            };
            let Some(hash) = hash else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArguments".into(),
                    message: "alert.SendClientStackTraceAlert expects (stackID, stackTrace, mode, ...)"
                        .into(),
                }));
            };
            let trace = args.get(1).and_then(|v| v.as_str()).unwrap_or("");
            let mut st = ctx.alert_telemetry.lock().ok()?;
            let mut newly_primed = None;
            let entry = match st.stack_traces.get(&hash) {
                Some(e) => *e,
                None => {
                    let eid = st.next_error_id;
                    st.next_error_id = st.next_error_id.saturating_add(1);
                    let e = (eid, LOGGING_BATCHED);
                    if st.stack_traces.len() >= ALERT_REGISTRY_CAP {
                        if let Some(oldest) = st.stack_traces.keys().next().copied() {
                            st.stack_traces.remove(&oldest);
                        }
                    }
                    st.stack_traces.insert(hash, e);
                    newly_primed = Some(e);
                    e
                }
            };
            drop(st);
            let effective_mode = log_mode_override(ctx, entry.0)
                .unwrap_or_else(|| entry.1.to_string());
            ctx.db.with_mut_snapshot(|snap| {
                let mut traces: Vec<serde_json::Value> = snap
                    .cluster_and_runtime
                    .alert_client_traces
                    .as_ref()
                    .and_then(|v| v.as_array().cloned())
                    .unwrap_or_default();
                traces.push(serde_json::json!({
                    "stack_id": hash,
                    "stack_key": stack_key,
                    "error_id": entry.0,
                    "mode": effective_mode,
                    "user_id": session.user_id,
                    "trace_preview": trace.chars().take(200).collect::<String>(),
                    "timestamp_unix": ctx.now_unix(),
                }));
                if traces.len() > 256 {
                    let trim = traces.len() - 256;
                    traces.drain(0..trim);
                }
                snap.cluster_and_runtime.alert_client_traces =
                    Some(serde_json::Value::Array(traces));
            });
            if let Some((eid, _)) = newly_primed {
                crate::services::notify_emitters_missing::emit_bean_prime(
                    ctx,
                    crate::router::NotificationTarget::All,
                    vec![(hash, eid, effective_mode.clone())],
                );
            }
            // One line only: the client trace is diagnostic text, not a server fault.
            let first_line: String = trace.lines().find(|l| !l.trim().is_empty()).unwrap_or("")
                .chars().take(200).collect();
            let err_line: String = trace.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("")
                .chars().take(200).collect();
            info!(
                user_id = ?session.user_id, error_id = entry.0, stack_hash = hash,
                err = err_line, stack_key = stack_key,
                "client stack trace reported: {first_line}"
            );
            tracing::debug!(error_id = entry.0, stack_hash = hash, "client stack trace body:\n{trace}");
            let bundle = crate::bug_reporting::BugIncidentBundle::build_snapshot(
                ctx,
                session,
                hash,
                stack_key,
                trace,
            );
            let queue_dir = Some(ctx.bug_reporting.effective_queue_dir());
            crate::bug_reporting::schedule_incident_capture(bundle, queue_dir, None);
            Some(Ok(PyRep::Tuple(vec![
                PyRep::Integer(entry.0),
                PyRep::ascii_bytes(&effective_mode),
            ])))
        }
        ("alert", "BeanDelivery") => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            handle_alert_bean_delivery(ctx, args)
        }
        ("alert", "GetLogModeForError") => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            if let Some(PyRep::Dict(entries)) = args.first() {
                let mut modes = Vec::with_capacity(entries.len());
                for (k, _) in entries {
                    modes.push((k.clone(), PyRep::ascii_bytes("M")));
                }
                return Some(Ok(PyRep::Tuple(vec![
                    PyRep::Dict(modes),
                    PyRep::Dict(Vec::new()),
                ])));
            }
            let err_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            if let Some(mode) = ctx.db.with_snapshot(|snap| {
                snap.cluster_and_runtime
                    .alert_log_modes
                    .get(&err_id.to_string())
                    .cloned()
            }) {
                return Some(Ok(PyRep::Tuple(vec![PyRep::Integer(err_id), PyRep::ascii_bytes(&mode)])));
            }
            let entry = ctx
                .alert_telemetry
                .lock()
                .ok()
                .and_then(|st| st.stack_traces.values().find(|(id, _)| *id == err_id).copied());
            Some(Ok(log_mode_tuple(entry)))
        }
        ("alert", "LogModeChanged") => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            let admin_or_qa = eve_db::service_roles::ROLE_ADMIN
                | eve_machonet::service_roles::ROLE_PROGRAMMER
                | eve_db::service_roles::ROLE_GMH
                | eve_db::service_roles::ROLE_GML;
            if !session.has_gm_role() && (session.role & admin_or_qa) == 0 {
                return Some(Err(BridgeRpcError {
                    code: "UserError".into(),
                    message: "NotAuthorized".into(),
                }));
            }
            let (error_id, log_mode) = match args {
                [PyRep::Integer(id), PyRep::String(m)] => (*id, m.clone()),
                [PyRep::Tuple(items) | PyRep::List(items), ..] if items.len() >= 2 => (
                    items[0].as_i64().unwrap_or(0),
                    items[1].as_str().unwrap_or("B").to_string(),
                ),
                _ => (
                    args.first().and_then(|a| a.as_i64()).unwrap_or(0),
                    args.get(1).and_then(|a| a.as_str()).unwrap_or("B").to_string(),
                ),
            };
            if error_id <= 0 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArguments".into(),
                    message: "alert.LogModeChanged expects (errorID, logMode)".into(),
                }));
            }
            ctx.db.with_mut_snapshot(|snap| {
                let modes = &mut snap.cluster_and_runtime.alert_log_modes;
                if modes.len() >= ALERT_REGISTRY_CAP && !modes.contains_key(&error_id.to_string()) {
                    if let Some(oldest) = modes.keys().next().cloned() {
                        modes.remove(&oldest);
                    }
                }
                modes.insert(error_id.to_string(), log_mode);
            });
            Some(Ok(PyRep::None))
        }
        ("alert", "SendProxyStackTraceAlert") => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            let (hash, stack_key) = match args.first() {
                Some(PyRep::Tuple(items) | PyRep::List(items)) => (
                    items.first().and_then(|v| v.as_i64()),
                    items.get(1).and_then(|v| v.as_str()).unwrap_or(""),
                ),
                Some(PyRep::Integer(v)) => (Some(*v), ""),
                _ => (None, ""),
            };
            let Some(hash) = hash else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArguments".into(),
                    message: "alert.SendProxyStackTraceAlert expects (stackID, stackTrace, mode, ...)"
                        .into(),
                }));
            };
            let trace = args.get(1).and_then(|v| v.as_str()).unwrap_or("");
            let mode = args.get(2).and_then(|v| v.as_str()).unwrap_or(LOGGING_BATCHED);
            let mut st = ctx.alert_telemetry.lock().ok()?;
            let entry = match st.stack_traces.get(&hash) {
                Some(e) => *e,
                None => {
                    let eid = st.next_error_id;
                    st.next_error_id = st.next_error_id.saturating_add(1);
                    let e = (eid, LOGGING_BATCHED);
                    if st.stack_traces.len() >= ALERT_REGISTRY_CAP {
                        if let Some(oldest) = st.stack_traces.keys().next().copied() {
                            st.stack_traces.remove(&oldest);
                        }
                    }
                    st.stack_traces.insert(hash, e);
                    e
                }
            };
            drop(st);

            ctx.db.with_mut_snapshot(|snap| {
                let mut traces: Vec<serde_json::Value> = snap
                    .cluster_and_runtime
                    .alert_proxy_traces
                    .as_ref()
                    .and_then(|v| v.as_array().cloned())
                    .unwrap_or_default();
                traces.push(serde_json::json!({
                    "stack_id": hash,
                    "stack_key": stack_key,
                    "error_id": entry.0,
                    "mode": mode,
                    "user_id": session.user_id,
                    "trace_preview": trace.chars().take(200).collect::<String>(),
                    "timestamp_unix": ctx.now_unix(),
                }));
                if traces.len() > 256 {
                    let trim = traces.len() - 256;
                    traces.drain(0..trim);
                }
                snap.cluster_and_runtime.alert_proxy_traces =
                    Some(serde_json::Value::Array(traces));
            });

            info!(
                user_id = ?session.user_id, error_id = entry.0, stack_hash = hash,
                stack_key = stack_key,
                "proxy stack trace reported"
            );
            Some(Ok(log_mode_tuple(Some(entry))))
        }
        ("alert", "GroupBeanDelivery") => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            let mut beans = Vec::new();
            match args.first() {
                Some(PyRep::Dict(entries)) => {
                    for (k, v) in entries {
                        if let (Some(err_id), Some(n)) = (k.as_i64(), v.as_i64()) {
                            if n >= 0 {
                                beans.push((err_id, n));
                            }
                        }
                    }
                }
                Some(PyRep::Bytes(raw)) => {
                    ctx.db.with_mut_snapshot(|snap| {
                        let prev: i64 = snap
                            .cluster_and_runtime
                            .alert_group_bean_bytes
                            .as_ref()
                            .and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse::<i64>().ok())))
                            .unwrap_or(0);
                        snap.cluster_and_runtime.alert_group_bean_bytes =
                            Some(serde_json::json!(prev.saturating_add(raw.len() as i64)));
                    });
                }
                _ => {}
            }
            if !beans.is_empty() {
                if let Ok(mut st) = ctx.alert_telemetry.lock() {
                    for &(err_id, n) in &beans {
                        if st.bean_counts.len() >= ALERT_REGISTRY_CAP && !st.bean_counts.contains_key(&err_id) {
                            if let Some(oldest) = st.bean_counts.keys().next().copied() {
                                st.bean_counts.remove(&oldest);
                            }
                        }
                        let c = st.bean_counts.entry(err_id).or_insert(0);
                        *c = c.saturating_add(n);
                    }
                }
                persist_bean_counts(ctx, &beans);
            }
            Some(Ok(PyRep::None))
        }
        ("config", "GetBlackListedPlanets") => {
            let mut planet_ids: Vec<i64> = ctx.db.with_snapshot(|snap| {
                snap.cluster_and_runtime
                    .blacklisted_planets
                    .clone()
                    .unwrap_or_default()
            });
            planet_ids.sort_unstable();
            planet_ids.dedup();
            Some(Ok(PyRep::List(
                planet_ids.into_iter().map(PyRep::Integer).collect(),
            )))
        }
        ("air_npe", "is_air_npe_enabled") => {
            // DERIVED: the AIR NPE runs on its node graphs, so it is enabled exactly when the
            // imported SDE carries node graphs (nodegraphs.fsdbinary).
            Some(Ok(PyRep::Bool(!ctx.sde.node_graphs.is_empty())))
        }
        ("invbroker", "GetItemDescriptor") => {
            // `invCache.ProcessSessionChange` fetches the inventory row descriptor on the first
            // session change after login, before any character exists. The descriptor is the
            // static `invItems` column layout, so it needs no character (the character-gated arm
            // in `inventory_fitting` is never reached for this method).
            Some(Ok(eve_proto::DBRowDescriptor::inv_item_header().to_pyrep()))
        }
        ("config", "GetAverageMarketPrices") => {
            // `{typeID: KeyVal(typeID, averagePrice, adjustedPrice)}` read by
            // `inventorycommon.typeHelpers.GetAveragePrice/GetAdjustedAveragePrice`.
            // DERIVED: averagePrice is the mean price of the live market orders for the type;
            // adjustedPrice is the SDE basePrice (falls back to averagePrice when it is 0).
            let mut sums: HashMap<u32, (f64, u32)> = HashMap::new();
            ctx.db.with_snapshot(|snap| {
                for ord in snap.market_orders.values().filter(|o| o.vol_remaining > 0) {
                    let e = sums.entry(ord.type_id).or_insert((0.0, 0));
                    e.0 += ord.price.to_wire_f64();
                    e.1 += 1;
                }
            });
            let mut type_ids: Vec<u32> = sums.keys().copied().collect();
            type_ids.sort_unstable();
            let entries = type_ids
                .into_iter()
                .map(|tid| {
                    let (sum, n) = sums[&tid];
                    let avg = sum / f64::from(n);
                    let base = ctx.sde.types.get(&tid).map(|t| t.base_price).unwrap_or(0.0);
                    let adjusted = if base > 0.0 { base } else { avg };
                    (
                        PyRep::Integer(i64::from(tid)),
                        PyRep::keyval([
                            ("typeID", PyRep::Integer(i64::from(tid))),
                            ("averagePrice", PyRep::Real(avg)),
                            ("adjustedPrice", PyRep::Real(adjusted)),
                        ]),
                    )
                })
                .collect();
            Some(Ok(PyRep::Dict(entries)))
        }
        ("charUnboundMgr", "GetCharacterSelectionData") => {
            Some(handle_get_character_selection_data(ctx, session))
        }
        ("charUnboundMgr", "GetCohortsForUser") => {
            let uid = match ServerContext::require_auth(session) {
                Ok(u) => u,
                Err(e) => return Some(Err(e)),
            };
            let now_unix = ctx.now_unix();
            let cohorts = ctx.db.with_snapshot(|snap| {
                if let Some(list) = snap.cluster_and_runtime.user_cohorts.get(&uid) {
                    return list.iter().cloned().map(PyRep::String).collect();
                }
                let user_chars: Vec<&eve_db::CharacterRecord> = snap
                    .characters
                    .values()
                    .filter(|c| c.user_id == uid)
                    .collect();
                let mut out = Vec::new();
                if let Some(oldest) = user_chars.iter().min_by_key(|c| c.create_date_unix) {
                    let year = 1970 + (oldest.create_date_unix / 31_536_000);
                    out.push(PyRep::String(format!("cohort_created_{year}")));
                    let age_secs = now_unix.saturating_sub(oldest.create_date_unix);
                    if age_secs < 30 * 86_400 {
                        out.push(PyRep::String("cohort_new_player".into()));
                    } else {
                        out.push(PyRep::String("cohort_veteran".into()));
                    }
                }
                out
            });
            Some(Ok(PyRep::List(cohorts)))
        }
        ("userSvc", "GetRedeemTokens") => {
            handle_user_redeem_tokens(ctx, session, args, _kwargs).await
        }
        ("charUnboundMgr", "GetCharOmegaDowngradeStatus") => {
            let uid = match ServerContext::require_auth(session) {
                Ok(u) => u,
                Err(e) => return Some(Err(e)),
            };
            let now_unix = ctx.now_unix();
            let downgraded = ctx.db.with_snapshot(|snap| {
                if snap.cluster_and_runtime.omega_downgraded.contains(&uid) {
                    return true;
                }
                if let Some(expiry) =
                    crate::services::account_services::subscription_expiry_for_user(snap, uid)
                {
                    return expiry <= now_unix;
                }
                false
            });
            Some(Ok(PyRep::Bool(downgraded)))
        }
        ("map", "GetSecurityModifiedSystems") => {
            let sec_map = crate::services::security_mgr::compute_modified_systems(ctx);
            let mut entries: Vec<(u32, f64)> = sec_map.into_iter().collect();
            entries.sort_by_key(|(sid, _)| *sid);
            let rows: Vec<Vec<PyRep>> = entries
                .into_iter()
                .map(|(sid, mod_sec)| {
                    let base_sec = ctx
                        .sde
                        .solar_systems
                        .get(&sid)
                        .map(|s| s.security_status)
                        .unwrap_or(0.0);
                    vec![
                        PyRep::Integer(i64::from(sid)),
                        PyRep::Real(base_sec),
                        PyRep::Real(mod_sec),
                    ]
                })
                .collect();
            let desc = eve_proto::DBRowDescriptor::new([
                ("solarSystemID", eve_proto::DbColumnType::I4),
                ("security", eve_proto::DbColumnType::R8),
                ("modifiedSecurity", eve_proto::DbColumnType::R8),
            ]);
            Some(Ok(desc.build_crowset(rows)))
        }
        ("config", "GetOldStationData") => handle_get_old_station_data(ctx, session, args),
        ("populationCap", "GetCharacterLoadSlot") => {
            let user_id = match ServerContext::require_auth(session) {
                Ok(u) => u,
                Err(e) => return Some(Err(e)),
            };
            let Some(char_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArguments".into(),
                    message: "populationCap.GetCharacterLoadSlot expects (charID,)".into(),
                }));
            };
            let Some(ch) = ctx
                .db
                .get_characters_for_user(user_id)
                .into_iter()
                .find(|c| c.character_id == char_id)
            else {
                return Some(Err(BridgeRpcError {
                    code: "CharacterOwnershipError".into(),
                    message: format!("Character {char_id} does not belong to user {user_id}"),
                }));
            };
            let cur_sys = ch.solar_system_id;
            // When character's system has an active population cap restriction, the server offers
            // an alternative destination solar system tagged with a cryptographically secure slotKey.
            // Otherwise, returning {cur_sys: None} permits direct character entry.
            let offered = crate::services::cluster_macho_misc_services::get_or_create_offered_load_slot(ctx, char_id, cur_sys);
            if let Some((slot_key, alt_sys)) = offered {
                Some(Ok(PyRep::Dict(vec![
                    (PyRep::Integer(i64::from(cur_sys)), PyRep::None),
                    (PyRep::Integer(i64::from(alt_sys)), PyRep::String(slot_key)),
                ])))
            } else {
                Some(Ok(PyRep::Dict(vec![(
                    PyRep::Integer(i64::from(cur_sys)),
                    PyRep::None,
                )])))
            }
        }
        // parity: rpc:populationCap.MoveCharacterToNewSystem
        ("populationCap", "MoveCharacterToNewSystem") => {
            let user_id = match ServerContext::require_auth(session) {
                Ok(u) => u,
                Err(e) => return Some(Err(e)),
            };
            let Some(char_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArguments".into(),
                    message: "populationCap.MoveCharacterToNewSystem expects (charID, selectedSolarSystemID, slotKey)".into(),
                }));
            };
            let Some(new_system_id_raw) = args.get(1).and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArguments".into(),
                    message: "populationCap.MoveCharacterToNewSystem requires selectedSolarSystemID".into(),
                }));
            };
            let selected_sys = match u32::try_from(new_system_id_raw) {
                Ok(v) => v,
                Err(_) => {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSolarSystem".into(),
                        message: format!("Invalid solar system ID {new_system_id_raw}"),
                    }));
                }
            };
            let slot_key = args.get(2).and_then(|a| a.as_str());

            let owns_char = ctx
                .db
                .get_characters_for_user(user_id)
                .into_iter()
                .any(|c| c.character_id == char_id);
            if !owns_char {
                return Some(Err(BridgeRpcError {
                    code: "CharacterOwnershipError".into(),
                    message: format!("Character {char_id} does not belong to user {user_id}"),
                }));
            }
            if !ctx.sde.solar_systems.contains_key(&selected_sys) {
                return Some(Err(BridgeRpcError {
                    code: "InvalidSolarSystem".into(),
                    message: format!("Solar system {selected_sys} does not exist in SDE"),
                }));
            }

            // Enforce that the destination system was explicitly offered for this slotKey (R-SC-03)
            let slot_key_str = slot_key.unwrap_or("");
            let is_valid = crate::services::cluster_macho_misc_services::validate_and_consume_offered_load_slot(
                ctx,
                char_id,
                selected_sys,
                slot_key_str,
            );
            if !is_valid {
                return Some(Err(BridgeRpcError {
                    code: "SystemNotOffered".into(),
                    message: format!("Solar system {selected_sys} was not offered for character {char_id} with slot key '{slot_key_str}'"),
                }));
            }

                                    // Consistently relocate character and active ship item
            let _ = ctx.db.set_character_solar_system(char_id, selected_sys, &ctx.sde);
            ctx.db.with_mut_snapshot(|snap| {
                let active_ship_id = snap.characters.get(&char_id).map(|ch| ch.active_ship_id);
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.station_id = None;
                    ch.structure_id = None;
                }
                if let Some(ship_id) = active_ship_id {
                    if let Some(ship) = snap.items.get_mut(&ship_id) {
                        ship.location_id = selected_sys as i64;
                    }
                }
            });
            Some(Ok(PyRep::None))
        }
        ("charUnboundMgr", "GetCharacterLockType") => {
            let user_id = match ServerContext::require_auth(session) {
                Ok(u) => u,
                Err(e) => return Some(Err(e)),
            };
            let Some(char_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArguments".into(),
                    message: "charUnboundMgr.GetCharacterLockType expects (charID,)".into(),
                }));
            };
            let owned = ctx
                .db
                .get_characters_for_user(user_id)
                .iter()
                .any(|c| c.character_id == char_id);
            if !owned {
                return Some(Err(BridgeRpcError {
                    code: "CharacterOwnershipError".into(),
                    message: format!("Character {char_id} does not belong to user {user_id}"),
                }));
            }
            let lock_type = ctx.db.with_snapshot(|s| {
                if let Some(blob) = s.cluster_and_runtime.character_locks.get(&char_id) {
                    if let Ok(v) = blob.parse::<i64>() {
                        return Some(v);
                    }
                    return match blob.to_ascii_lowercase().as_str() {
                        "transfer" => Some(1),
                        "delete" | "delete_pending" => Some(2),
                        "gm" => Some(3),
                        _ => Some(1),
                    };
                }
                if crate::services::character_lifecycle::is_character_delete_pending(s, char_id) {
                    return Some(2);
                }
                None
            });
            match lock_type {
                Some(lt) => Some(Ok(PyRep::Integer(lt))),
                None => Some(Ok(PyRep::None)),
            }
        }
        // rpc-lint: allow legacy Crucible / parity ledger service feat:character.portrait-upload
        ("photoUploadSvc", "Upload") => {
            const MAX_PORTRAIT_UPLOAD_BYTES: usize = 512 * 1024;
            let user_id = match ServerContext::require_auth(session) {
                Ok(u) => u,
                Err(e) => return Some(Err(e)),
            };
            let (char_id, portrait_data) = match args.first() {
                Some(PyRep::Integer(cid)) => {
                    let d = match args.get(1) {
                        Some(PyRep::Bytes(b)) => b.clone(),
                        Some(PyRep::String(s)) => s.as_bytes().to_vec(),
                        _ => Vec::new(),
                    };
                    (*cid, d)
                }
                Some(PyRep::Bytes(b)) => match session.character_id {
                    Some(cid) => (cid, b.clone()),
                    None => {
                        return Some(Err(BridgeRpcError {
                            code: "SessionError".into(),
                            message: "photoUploadSvc.Upload requires an authenticated character"
                                .into(),
                        }));
                    }
                },
                Some(PyRep::String(s)) => match session.character_id {
                    Some(cid) => (cid, s.as_bytes().to_vec()),
                    None => {
                        return Some(Err(BridgeRpcError {
                            code: "SessionError".into(),
                            message: "photoUploadSvc.Upload requires an authenticated character"
                                .into(),
                        }));
                    }
                },
                _ => (0, Vec::new()),
            };
            if char_id <= 0 || portrait_data.is_empty() {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArguments".into(),
                    message: "photoUploadSvc.Upload requires characterID and portrait data".into(),
                }));
            }
            if portrait_data.len() > MAX_PORTRAIT_UPLOAD_BYTES {
                return Some(Err(BridgeRpcError {
                    code: "PortraitTooLarge".into(),
                    message: format!(
                        "Portrait upload size {} exceeds maximum {} bytes",
                        portrait_data.len(),
                        MAX_PORTRAIT_UPLOAD_BYTES
                    ),
                }));
            }
            let is_owner = ctx.db.with_snapshot(|s| {
                s.characters.get(&char_id).is_some_and(|c| c.user_id == user_id)
            }) || session.has_gm_role();
            if !is_owner {
                return Some(Err(BridgeRpcError {
                    code: "CharacterOwnershipError".into(),
                    message: format!("Character {char_id} does not belong to user {user_id}"),
                }));
            }
            let hex_str = portrait_data.iter().map(|b| format!("{b:02x}")).collect::<String>();
            ctx.db.with_mut_snapshot(|snap| {
                for sz in [32, 64, 128, 256, 512, 1024] {
                    snap.cluster_and_runtime.portraits.remove(&format!("{char_id}:{sz}"));
                }
                snap.cluster_and_runtime.portraits.insert(char_id.to_string(), hex_str);
                snap.character_doll_info.entry(char_id).or_default().1 = format!("portrait:{char_id}");
            });
            Some(Ok(PyRep::Bool(true)))
        }
        ("charMgr", "GetCharacterSettings") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(handle_get_character_settings(ctx, char_id)))
        }
        ("charMgr", "GetPaperdollState") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            // `paperdoll.State`: 0 = no_recustomization, 1 = resculpting,
            // 2 = no_existing_customization, 3 = full_recustomizing, 4 = force_recustomize.
            let state = ctx.db.with_snapshot(|snap| {
                if let Some(raw) = snap.cluster_and_runtime.paperdoll_states.get(&char_id) {
                    if let Ok(v) = raw.trim().parse::<i64>() {
                        return v.clamp(0, 4);
                    }
                }
                let is_resculpt_pending = crate::services::account_services::load_store(snap)
                    .resculpt_pending
                    .contains(&char_id)
                    || snap.cluster_and_runtime.resculpt_pending.contains(&char_id);
                if is_resculpt_pending { 1 } else { 0 }
            });
            Some(Ok(PyRep::Integer(state)))
        }
        ("structureDirectory", "GetMyDockableStructures") => {
            Some(crate::services::station_structure::structure_profiles_directory::handle_get_my_dockable_structures(ctx, session, args))
        }
        ("map", "GetStationInfo") => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            let target_station_id = args.first().and_then(|a| a.as_i64()).or_else(|| _kwargs.get("stationID").and_then(|a| a.as_i64()));
            if let Some(sid) = target_station_id {
                if let Some(st) = ctx.sde.stations.get(&sid) {
                    return Some(Ok(PyRep::keyval([
                        ("stationID", PyRep::Integer(st.station_id)),
                        ("solarSystemID", PyRep::Integer(i64::from(st.solar_system_id))),
                        ("constellationID", PyRep::Integer(i64::from(st.constellation_id))),
                        ("regionID", PyRep::Integer(i64::from(st.region_id))),
                        ("operationID", PyRep::Integer(i64::from(st.operation_id))),
                        ("stationTypeID", PyRep::Integer(i64::from(st.station_type_id))),
                        ("ownerID", PyRep::Integer(st.owner_corporation_id)),
                        ("stationName", PyRep::String(st.name.clone())),
                        ("x", PyRep::Real(st.position.x)),
                        ("y", PyRep::Real(st.position.y)),
                        ("z", PyRep::Real(st.position.z)),
                    ])));
                } else {
                    return Some(Err(BridgeRpcError {
                        code: "StationNotFound".into(),
                        message: format!("Station {sid} not found"),
                    }));
                }
            }
            let mut stations: Vec<&eve_sde::StationInfo> = ctx.sde.stations.values().collect();
            stations.sort_by_key(|st| st.station_id);
            let rows: Vec<Vec<PyRep>> = stations
                .into_iter()
                .map(|st| {
                    vec![
                        PyRep::Integer(st.station_id),
                        PyRep::Integer(i64::from(st.solar_system_id)),
                        PyRep::Integer(i64::from(st.constellation_id)),
                        PyRep::Integer(i64::from(st.region_id)),
                        PyRep::Integer(i64::from(st.operation_id)),
                        PyRep::Integer(i64::from(st.station_type_id)),
                        PyRep::Integer(st.owner_corporation_id),
                        PyRep::String(st.name.clone()),
                        PyRep::Real(st.position.x),
                        PyRep::Real(st.position.y),
                        PyRep::Real(st.position.z),
                    ]
                })
                .collect();
            Some(Ok(PyRep::rowset(
                vec![
                    "stationID",
                    "solarSystemID",
                    "constellationID",
                    "regionID",
                    "operationID",
                    "stationTypeID",
                    "ownerID",
                    "stationName",
                    "x",
                    "y",
                    "z",
                ],
                rows,
            )))
        }
        ("skillMgr2", "GetMySkillHandler") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(
                ctx.bind_object(session, "skillMgr2", char_id)
                    .map(|bid| ctx.bound_object_rep(bid, caller_trace)),
            )
        }
        ("skillMgr2", "GetAttributes") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some((cha, int, mem, per, wil)) = ctx.db.with_snapshot(|snap| {
                snap.characters.contains_key(&char_id).then(|| {
                    crate::services::character_skills::effective_character_attributes(
                        &ctx.sde, snap, char_id,
                    )
                })
            }) else {
                return Some(Err(BridgeRpcError {
                    code: "CharacterNotFound".into(),
                    message: format!("character {char_id} not found"),
                }));
            };
            // `{attributeID: value}` for dogma attributes 164..=168 (charisma, intelligence,
            // memory, perception, willpower), including implant (175..=179) and booster bonuses.
            Some(Ok(PyRep::Dict(vec![
                (PyRep::Integer(164), PyRep::Integer(cha as i64)),
                (PyRep::Integer(165), PyRep::Integer(int as i64)),
                (PyRep::Integer(166), PyRep::Integer(mem as i64)),
                (PyRep::Integer(167), PyRep::Integer(per as i64)),
                (PyRep::Integer(168), PyRep::Integer(wil as i64)),
            ])))
        }
        ("skillMgr2", "GetFreeSkillPoints") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Ok(ch) = ctx.db.get_character(char_id) else {
                return Some(Err(BridgeRpcError {
                    code: "CharacterNotFound".into(),
                    message: format!("character {char_id} not found"),
                }));
            };
            Some(Ok(PyRep::Integer(ch.free_skill_points as i64)))
        }
        ("operationsManager", "can_character_play_the_tutorial") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let can_play = ctx.db.with_snapshot(|snap| {
                crate::services::operations_manager::can_character_play_tutorial(snap, char_id)
            });
            Some(Ok(PyRep::Bool(can_play)))
        }
        _ => None,
    }
}

fn handle_get_old_station_data(
    _ctx: &ServerContext,
    _session: &SessionState,
    _args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    // const-ok: `eveCfg._LoadOldStationsInfo` only builds `cfg.oldStations` when the
    // result is truthy. EvoMu's universe has no retired stations (all stations come from
    // the current SDE), so there is no old-station recordset to send.
    Some(Ok(PyRep::None))
}

fn handle_alert_bean_count(
    ctx: &ServerContext,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let Some(hash) = args.first().and_then(|a| a.as_i64()) else {
        return Some(Err(BridgeRpcError {
            code: "InvalidArguments".into(),
            message: "alert.BeanCount expects (stackIDHash,)".into(),
        }));
    };
    let entry = ctx
        .alert_telemetry
        .lock()
        .ok()
        .and_then(|st| st.stack_traces.get(&hash).copied());
    if let Some((eid, default_mode)) = entry {
        let mode = log_mode_override(ctx, eid).unwrap_or_else(|| default_mode.to_string());
        return Some(Ok(PyRep::Tuple(vec![
            PyRep::Integer(eid),
            PyRep::ascii_bytes(&mode),
        ])));
    }
    // (None, None) tells the client "the server wants a trace" (alert.pyj __SendStackTraceAlert).
    Some(Ok(log_mode_tuple(None)))
}

fn handle_alert_bean_delivery(
    ctx: &ServerContext,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    // beans: {errorID: (count, lastWallclockTime)}
    let Some(PyRep::Dict(entries)) = args.first() else {
        return Some(Err(BridgeRpcError {
            code: "InvalidArguments".into(),
            message: "alert.BeanDelivery expects ({errorID: (count, time)},)".into(),
        }));
    };
    let mut beans = Vec::with_capacity(entries.len());
    for (k, v) in entries {
        let (Some(error_id), PyRep::Tuple(t) | PyRep::List(t)) = (k.as_i64(), v) else {
            continue;
        };
        let n = t.first().and_then(|c| c.as_i64()).unwrap_or(0);
        if n < 0 {
            return Some(Err(BridgeRpcError {
                code: "InvalidArguments".into(),
                message: format!("alert.BeanDelivery count for errorID {error_id} is negative"),
            }));
        }
        beans.push((error_id, n));
    }
    if let Ok(mut st) = ctx.alert_telemetry.lock() {
        for &(error_id, n) in &beans {
            if st.bean_counts.len() >= ALERT_REGISTRY_CAP && !st.bean_counts.contains_key(&error_id) {
                if let Some(oldest) = st.bean_counts.keys().next().copied() {
                    st.bean_counts.remove(&oldest);
                }
            }
            let c = st.bean_counts.entry(error_id).or_insert(0);
            *c = c.saturating_add(n);
        }
    }
    persist_bean_counts(ctx, &beans);
    Some(Ok(PyRep::None))
}

async fn handle_user_redeem_tokens(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    crate::services::account_services::handle(
        ctx,
        session,
        "userSvc",
        "GetRedeemTokens",
        args,
        kwargs,
    )
    .await
}

/// `(stack-trace registry size, bean-count registry size)` on `ctx`; both are bounded by `ALERT_REGISTRY_CAP`.
pub fn alert_registry_sizes(ctx: &ServerContext) -> (usize, usize) {
    ctx.alert_telemetry
        .lock()
        .map(|st| (st.stack_traces.len(), st.bean_counts.len()))
        .unwrap_or((0, 0))
}

#[cfg(test)] #[path = "client_login_tests.rs"] mod client_login_tests;

