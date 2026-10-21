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
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Mutex, OnceLock};
use tracing::info;

/// `alert.LOGGING_BATCHED`: the client counts repeats and delivers them via `BeanDelivery`.
const LOGGING_BATCHED: &str = "B";

/// Per-process registry `stackIDHash -> (errorID, logMode)`, like the alert service's
/// `stacktraceLogMode` on a real node. One alert service per server process.
fn stack_trace_registry() -> &'static Mutex<HashMap<i64, (i64, &'static str)>> {
    static REG: OnceLock<Mutex<HashMap<i64, (i64, &'static str)>>> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(HashMap::new()))
}

static NEXT_ERROR_ID: AtomicI64 = AtomicI64::new(1);

/// Total error occurrences delivered by clients through `alert.BeanDelivery`, per errorID.
fn bean_counts() -> &'static Mutex<HashMap<i64, i64>> {
    static COUNTS: OnceLock<Mutex<HashMap<i64, i64>>> = OnceLock::new();
    COUNTS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Returns the delivered occurrence count for `error_id` (0 when never delivered).
pub fn delivered_bean_count(error_id: i64) -> i64 {
    bean_counts().lock().map(|m| m.get(&error_id).copied().unwrap_or(0)).unwrap_or(0)
}

fn log_mode_tuple(entry: Option<(i64, &'static str)>) -> PyRep {
    match entry {
        Some((error_id, mode)) => {
            PyRep::Tuple(vec![PyRep::Integer(error_id), PyRep::ascii_bytes(mode)])
        }
        None => PyRep::Tuple(vec![PyRep::None, PyRep::None]),
    }
}

pub async fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("alert", "BeanCount") => {
            let Some(hash) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArguments".into(),
                    message: "alert.BeanCount expects (stackIDHash,)".into(),
                }));
            };
            let entry = stack_trace_registry().lock().ok().and_then(|m| m.get(&hash).copied());
            // (None, None) tells the client "the server wants a trace" (alert.pyj __SendStackTraceAlert).
            Some(Ok(log_mode_tuple(entry)))
        }
        ("alert", "SendClientStackTraceAlert") => {
            let hash = match args.first() {
                Some(PyRep::Tuple(items) | PyRep::List(items)) => {
                    items.first().and_then(|v| v.as_i64())
                }
                _ => None,
            };
            let Some(hash) = hash else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArguments".into(),
                    message: "alert.SendClientStackTraceAlert expects (stackID, stackTrace, mode, ...)"
                        .into(),
                }));
            };
            let trace = args.get(1).and_then(|v| v.as_str()).unwrap_or("");
            let mut reg = stack_trace_registry().lock().ok()?;
            let entry = *reg.entry(hash).or_insert_with(|| {
                (NEXT_ERROR_ID.fetch_add(1, Ordering::Relaxed), LOGGING_BATCHED)
            });
            drop(reg);
            // One line only: the client trace is diagnostic text, not a server fault.
            let first_line: String = trace.lines().find(|l| !l.trim().is_empty()).unwrap_or("")
                .chars().take(200).collect();
            info!(
                user_id = ?session.user_id, error_id = entry.0, stack_hash = hash,
                "client stack trace reported: {first_line}"
            );
            Some(Ok(log_mode_tuple(Some(entry))))
        }
        ("alert", "BeanDelivery") => {
            // beans: {errorID: (count, lastWallclockTime)}
            let Some(PyRep::Dict(entries)) = args.first() else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArguments".into(),
                    message: "alert.BeanDelivery expects ({errorID: (count, time)},)".into(),
                }));
            };
            if let Ok(mut counts) = bean_counts().lock() {
                for (k, v) in entries {
                    let (Some(error_id), PyRep::Tuple(t) | PyRep::List(t)) = (k.as_i64(), v) else {
                        continue;
                    };
                    let n = t.first().and_then(|c| c.as_i64()).unwrap_or(0);
                    *counts.entry(error_id).or_insert(0) += n;
                }
            }
            Some(Ok(PyRep::None))
        }
        ("config", "GetBlackListedPlanets") => {
            // const-ok: no planet is excluded from planetary interaction in EvoMu's world and the
            // client static data has no blacklist source, so the client receives an empty list.
            Some(Ok(PyRep::List(Vec::new())))
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
        ("charUnboundMgr", "GetCharacterSelectionData") => Some(
            crate::services::char_selection::get_character_selection_data(ctx, session),
        ),
        ("config", "GetOldStationData") => {
            // const-ok: `eveCfg._LoadOldStationsInfo` only builds `cfg.oldStations` when the
            // result is truthy. EvoMu's universe has no retired stations (all stations come from
            // the current SDE), so there is no old-station recordset to send.
            Some(Ok(PyRep::None))
        }
        _ => None,
    }
}
