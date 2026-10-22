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
                    if reg.len() >= ALERT_REGISTRY_CAP {
                        if let Some(oldest) = reg.keys().next().copied() {
                            reg.remove(&oldest);
                        }
                    }
                    reg.insert(hash, e);
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
                        if let Some(oldest) = counts.keys().find(|&&k| k != 9999 && k != 0x7000_0000).copied() {
                            counts.remove(&oldest);
                        }
                    }
                    let c = counts.entry(error_id).or_insert(0);
                    *c = c.saturating_add(n);
                }
            }
            Some(Ok(PyRep::None))
        }
        ("alert", "GetLogModeForError") => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            let err_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let entry = stack_trace_registry()
                .lock()
                .ok()
                .and_then(|m| m.values().find(|(id, _)| *id == err_id).copied());
            Some(Ok(log_mode_tuple(entry)))
        }
        ("alert", "GroupBeanDelivery") => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            if let Some(PyRep::Dict(entries)) = args.first() {
                if let Ok(mut counts) = bean_counts().lock() {
                    for (k, v) in entries {
                        if let (Some(err_id), Some(n)) = (k.as_i64(), v.as_i64()) {
                            if counts.len() >= ALERT_REGISTRY_CAP && !counts.contains_key(&err_id) {
                                if let Some(oldest) = counts.keys().find(|&&k| k != 9999).copied() {
                                    counts.remove(&oldest);
                                }
                            }
                            let c = counts.entry(err_id).or_insert(0);
                            *c = c.saturating_add(n);
                        }
                    }
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
        // ---- post-SelectCharacterID station entry bootstrap ----
        ("seasonManager", "get_season_data_for_character") => {
            if let Err(e) = ServerContext::require_character(session) {
                return Some(Err(e));
            }
            // const-ok: `SeasonService._fetch_season_data` checks `if season_data is None: return None`;
            // EvoMu has no active seasonal event running.
            Some(Ok(PyRep::None))
        }
        ("charMgr", "GetCharacterSettings") => {
            if let Err(e) = ServerContext::require_character(session) {
                return Some(Err(e));
            }
            // const-ok: `CharacterSettingsSvc._GetSettings` caches this dict and indexes into it by
            // settingKey (catching KeyError for unset keys).
            Some(Ok(PyRep::Dict(Vec::new())))
        }
        ("charMgr", "GetPaperdollState") => {
            if let Err(e) = ServerContext::require_character(session) {
                return Some(Err(e));
            }
            // const-ok: `const.paperdollStateNoRecustomization = 0` read by `StationSvc.GetPaperdollStateCache`.
            Some(Ok(PyRep::Integer(0)))
        }
        ("structureDirectory", "GetMyDockableStructures") => {
            if let Err(e) = ServerContext::require_character(session) {
                return Some(Err(e));
            }
            let sys_id = args
                .first()
                .and_then(|a| a.as_i64())
                .and_then(|v| u32::try_from(v).ok())
                .or(session.solar_system_id)
                .unwrap_or(0);
            let ids: Vec<PyRep> = ctx.db.with_snapshot(|snap| {
                let mut v: Vec<i64> = snap
                    .structures
                    .values()
                    .filter(|s| s.solar_system_id == sys_id)
                    .map(|s| s.structure_id)
                    .collect();
                v.sort_unstable();
                v.into_iter().map(PyRep::Integer).collect()
            });
            Some(Ok(PyRep::List(ids)))
        }
        ("map", "GetStationInfo") => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            let target_region = session.region_id;
            let target_station = session.station_id;
            let mut stations: Vec<&eve_sde::StationInfo> = ctx
                .sde
                .stations
                .values()
                .filter(|st| {
                    target_station == Some(st.station_id)
                        || target_region.is_none_or(|r| st.region_id == r)
                })
                .collect();
            stations.sort_by_key(|st| st.station_id);
            if target_region.is_none() && stations.len() > 256 {
                stations.truncate(256);
            }
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
            if let Err(e) = ServerContext::require_character(session) {
                return Some(Err(e));
            }
            // const-ok: `operations/common/util.pyj:is_npe_enabled_for_character` returns False when
            // the character is not in the tutorial flow.
            Some(Ok(PyRep::Bool(false)))
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
