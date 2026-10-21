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

/// Upper bound on entries in each alert registry. Hashes and counts come from clients, so the
/// registries stop growing at this size (a new stack trace still gets an errorID, unregistered).
pub const ALERT_REGISTRY_CAP: usize = 1024;

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
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
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
            let mut reg = stack_trace_registry().lock().ok()?;
            let entry = match reg.get(&hash) {
                Some(e) => *e,
                None => {
                    let e = (NEXT_ERROR_ID.fetch_add(1, Ordering::Relaxed), LOGGING_BATCHED);
                    if reg.len() < ALERT_REGISTRY_CAP {
                        reg.insert(hash, e);
                    }
                    e
                }
            };
            drop(reg);
            // One line only: the client trace is diagnostic text, not a server fault.
            let first_line: String = trace.lines().find(|l| !l.trim().is_empty()).unwrap_or("")
                .chars().take(200).collect();
            info!(
                user_id = ?session.user_id, error_id = entry.0, stack_hash = hash,
                stack_key = stack_key,
                "client stack trace reported: {first_line}"
            );
            tracing::debug!(error_id = entry.0, stack_hash = hash, "client stack trace body:\n{trace}");
            Some(Ok(log_mode_tuple(Some(entry))))
        }
        ("alert", "BeanDelivery") => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
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
            if let Ok(mut counts) = bean_counts().lock() {
                for (error_id, n) in beans {
                    if counts.len() >= ALERT_REGISTRY_CAP && !counts.contains_key(&error_id) {
                        continue;
                    }
                    let c = counts.entry(error_id).or_insert(0);
                    *c = c.saturating_add(n);
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
        // ---- character selection screen (eve/client/script/ui/login/charSelection) ----
        ("loginCampaignManager", "get_client_campaign_state") => {
            if session.character_id.is_some() {
                return None;
            }
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            // const-ok: loginCampaignService treats a falsy state as "no active login campaign";
            // EvoMu runs no login-reward campaigns, so there is no campaign state to send.
            Some(Ok(PyRep::None))
        }
        ("seasonalLoginCampaignManager", "get_active_campaign") => {
            if session.character_id.is_some() {
                return None;
            }
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            // const-ok: seasonalLoginCampaignService unpacks (campaign_id, end_date_ordinal,
            // turnover_time_str, num_remaining_rewards) and treats campaign_id None as "no
            // campaign"; EvoMu runs no seasonal login campaigns.
            Some(Ok(PyRep::Tuple(vec![PyRep::None, PyRep::None, PyRep::None, PyRep::None])))
        }
        ("charUnboundMgr", "GetCohortsForUser") => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            // const-ok: gatekeeper.user checks `cohortID in <result>`; EvoMu enrols no account in
            // any feature cohort, so the user's cohort list is empty.
            Some(Ok(PyRep::List(Vec::new())))
        }
        ("userSvc", "GetRedeemTokens") => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            // const-ok: redeemsvc counts/iterates the account's redeem queue; EvoMu has no redeem
            // queue (no gifted or purchased items waiting), so the token list is empty.
            Some(Ok(PyRep::List(Vec::new())))
        }
        ("charUnboundMgr", "GetCharOmegaDowngradeStatus") => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            // const-ok: true only when an Omega subscription has lapsed; EvoMu accounts have no
            // subscription expiry, so no account is ever in the lapsed (downgraded) state.
            Some(Ok(PyRep::Bool(false)))
        }
        ("map", "GetSecurityModifiedSystems") => {
            // characterSlots._GetSecurityModifierText calls `.Index('solarSystemID')` on a CRowset
            // of (solarSystemID, security, modifiedSecurity). EvoMu applies no security modifiers
            // (system security always comes from the SDE), so the rowset has no rows.
            let desc = eve_proto::DBRowDescriptor::new([
                ("solarSystemID", eve_proto::DbColumnType::I4),
                ("security", eve_proto::DbColumnType::R8),
                ("modifiedSecurity", eve_proto::DbColumnType::R8),
            ]);
            Some(Ok(desc.build_crowset(Vec::new())))
        }
        ("config", "GetOldStationData") => {
            // const-ok: `eveCfg._LoadOldStationsInfo` only builds `cfg.oldStations` when the
            // result is truthy. EvoMu's universe has no retired stations (all stations come from
            // the current SDE), so there is no old-station recordset to send.
            Some(Ok(PyRep::None))
        }
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
            // `TryEnterGame` checks `len(locations) > 1` for alternate solar-system load slots when
            // a system is over its population cap; returning `{solar_system_id: None}` (`len == 1`)
            // lets character entry proceed directly into the character's current system.
            Some(Ok(PyRep::Dict(vec![(
                PyRep::Integer(i64::from(ch.solar_system_id)),
                PyRep::None,
            )])))
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
            // const-ok: `CheckCharacterLockStatus` raises when `lockTypeID` is not None (character
            // transfer queue or bazaar sale); EvoMu runs neither, so owned characters are unlocked.
            Some(Ok(PyRep::None))
        }
        _ => None,
    }
}

/// `(stack-trace registry size, bean-count registry size)`; both are bounded by `ALERT_REGISTRY_CAP`.
pub fn alert_registry_sizes() -> (usize, usize) {
    let a = stack_trace_registry().lock().map(|m| m.len()).unwrap_or(0);
    let b = bean_counts().lock().map(|m| m.len()).unwrap_or(0);
    (a, b)
}
