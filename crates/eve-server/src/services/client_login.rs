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

const SETTINGS_INFO_MARSHAL_CODE: &[u8] = b"c\x00\x00\x00\x00\x00\x00\x00\x00\x01\x00\x00\x00\x43\x00\x00\x00s\x04\x00\x00\x00g\x00\x00S(\x01\x00\x00\x00N(\x00\x00\x00\x00(\x00\x00\x00\x00(\x00\x00\x00\x00(\x00\x00\x00\x00s\x00\x00\x00\x00s\x0c\x00\x00\x00SettingsInfo\x01\x00\x00\x00s\x00\x00\x00\x00";

fn unix_to_filetime(unix_secs: u64) -> i64 {
    (unix_secs as i64 + 11_644_473_600).saturating_mul(10_000_000)
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
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            // const-ok: loginCampaignService treats a falsy state as "no active login campaign";
            // EvoMu runs no login-reward campaigns, so there is no campaign state to send.
            Some(Ok(PyRep::None))
        }
        ("seasonalLoginCampaignManager", "get_active_campaign") => {
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
        ("dogmaIM", "GetAllInfo") => Some(
            crate::services::char_selection::handle_dogma_get_all_info(ctx, session),
        ),
        ("crimewatch", "GetClientStates") => Some(
            crate::services::char_selection::handle_crimewatch_get_client_states(ctx, session),
        ),
        ("standingMgr", "GetNPCNPCStandings") => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            let mut faction_ids: Vec<u32> = ctx.sde.factions.keys().copied().collect();
            faction_ids.sort_unstable();
            // DERIVED: symmetric positive standing (5.0) for each faction toward itself so
            // `standingsvc.__RefreshStandings` (`tmp.Filter('toID')` / `tmp.Filter('fromID')`)
            // primes a non-empty Rowset of SDE factions.
            let rows: Vec<Vec<PyRep>> = faction_ids
                .into_iter()
                .map(|fid| {
                    vec![
                        PyRep::Integer(i64::from(fid)),
                        PyRep::Integer(i64::from(fid)),
                        PyRep::Real(5.0),
                    ]
                })
                .collect();
            Some(Ok(PyRep::rowset(
                vec!["fromID", "toID", "standing"],
                rows,
            )))
        }
        ("seasonManager", "get_season_data_for_character") => {
            if let Err(e) = ServerContext::require_character(session) {
                return Some(Err(e));
            }
            // const-ok: `SeasonService._fetch_season_data` checks `if season_data is None: return None`;
            // EvoMu has no active seasonal event running.
            Some(Ok(PyRep::None))
        }
        ("securityMgr", "get_modified_systems") => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            // const-ok: `SecurityService.Initialize` iterates `modified_system_rows_by_solar_system_id.iteritems()`;
            // EvoMu does not apply dynamic CONCORD security modifiers over SDE system security.
            Some(Ok(PyRep::Dict(Vec::new())))
        }
        ("monolithTaskTrackerMgr", "GetCompletedAchievementsAndClientEventCount") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let completed_entries = ctx.db.with_snapshot(|snap| {
                snap.npe_progress
                    .get(&char_id)
                    .filter(|n| n.completed)
                    .map(|n| vec![(PyRep::Integer(i64::from(n.current_node_id)), PyRep::Integer(1))])
                    .unwrap_or_default()
            });
            Some(Ok(PyRep::Dict(vec![
                (
                    PyRep::String("completedDict".into()),
                    PyRep::Dict(completed_entries),
                ),
                (
                    PyRep::String("eventDict".into()),
                    PyRep::Dict(Vec::new()),
                ),
            ])))
        }
        ("charMgr", "GetSettingsInfo") => {
            if let Err(e) = ServerContext::require_character(session) {
                return Some(Err(e));
            }
            // `SettingsSvc.UpdateSettingsStatistics` calls `Crypto.Verify(sm.RemoteSvc('charMgr').GetSettingsInfo())`,
            // which under Placebo crypto (`evecrypto/placebo.pyj:verify`) returns `(signedData[0], signedData[1] == 0)`
            // and passes `signedData[0]` to `marshal.loads` to execute `SettingsInfo()`.
            Some(Ok(PyRep::Tuple(vec![
                PyRep::Bytes(SETTINGS_INFO_MARSHAL_CODE.to_vec()),
                PyRep::Integer(0),
            ])))
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
        ("accessGroupBookmarkMgr", "GetMyActiveBookmarks") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let now_ft = unix_to_filetime(ctx.now_unix());
            let bookmarks: Vec<PyRep> = ctx.db.with_snapshot(|snap| {
                let mut list: Vec<_> = snap
                    .bookmarks
                    .values()
                    .filter(|b| b.owner_id == char_id)
                    .cloned()
                    .collect();
                list.sort_by_key(|b| b.bookmark_id);
                list.into_iter()
                    .map(|b| {
                        PyRep::keyval([
                            ("bookmarkID", PyRep::Integer(b.bookmark_id)),
                            ("ownerID", PyRep::Integer(b.owner_id)),
                            ("itemID", PyRep::Integer(i64::from(b.solar_system_id))),
                            ("typeID", PyRep::Integer(5)),
                            ("flag", PyRep::Integer(4)),
                            ("memo", PyRep::String(format!("{}\t{}", b.label, b.memo))),
                            ("created", PyRep::Integer(now_ft)),
                            ("x", PyRep::Real(b.x)),
                            ("y", PyRep::Real(b.y)),
                            ("z", PyRep::Real(b.z)),
                            ("locationID", PyRep::Integer(i64::from(b.solar_system_id))),
                            ("creatorID", PyRep::Integer(b.owner_id)),
                            ("folderID", PyRep::Integer(1)),
                            ("subfolderID", PyRep::None),
                        ])
                    })
                    .collect()
            });
            let folders = vec![PyRep::keyval([
                ("folderID", PyRep::Integer(1)),
                ("folderName", PyRep::String("Personal Locations".into())),
                ("creatorID", PyRep::Integer(char_id)),
                ("isPersonal", PyRep::Bool(true)),
                ("accessLevel", PyRep::Integer(4)),
            ])];
            // `BookmarkSvc.ACLPrimeBookmarks` unpacks `(folders, bookmarks, subfolders)`.
            Some(Ok(PyRep::Tuple(vec![
                PyRep::List(folders),
                PyRep::List(bookmarks),
                PyRep::List(Vec::new()),
            ])))
        }
        ("ProjectDiscovery", "initialize_tutorial_status") => {
            if let Err(e) = ServerContext::require_character(session) {
                return Some(Err(e));
            }
            // const-ok: `CovidClientService.OnCharacterSessionChanged` stores this boolean into
            // `_is_tutorial_complete`; returning true marks the minigame tutorial complete.
            Some(Ok(PyRep::Bool(true)))
        }
        ("milestoneMgr", "ProcessCharacterLogon") => {
            if let Err(e) = ServerContext::require_character(session) {
                return Some(Err(e));
            }
            // const-ok: `MilestoneService.OnCharacterSessionChanged` discards the return value (`POP_TOP`).
            Some(Ok(PyRep::None))
        }
        ("XmppChatMgr", "Hostname") => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            // const-ok: `XmppChatService._CreateConnection` stores `self.hostname = self.XmppChatMgr.Hostname()`.
            Some(Ok(PyRep::String("127.0.0.1".into())))
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
        ("officeManager", "GetMyCorporationsOffices") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let corp_id = session.corporation_id.unwrap_or_else(|| {
                ctx.db
                    .get_character(char_id)
                    .map(|c| c.corporation_id)
                    .unwrap_or(1_000_009)
            });
            let rows: Vec<Vec<PyRep>> = ctx.db.with_snapshot(|snap| {
                let mut offices: Vec<_> = snap
                    .items
                    .values()
                    .filter(|i| i.owner_id == corp_id && i.type_id == 27)
                    .cloned()
                    .collect();
                offices.sort_by_key(|i| i.item_id);
                offices
                    .into_iter()
                    .map(|i| {
                        vec![
                            PyRep::Integer(i.item_id),
                            PyRep::Integer(i.location_id),
                            PyRep::Integer(i64::from(i.type_id)),
                            PyRep::Integer(i.item_id),
                        ]
                    })
                    .collect()
            });
            Some(Ok(PyRep::rowset(
                vec!["officeID", "stationID", "typeID", "officeFolderID"],
                rows,
            )))
        }
        ("fwWarzoneSolarsystem", "GetLocalOccupationState") => {
            if let Err(e) = ServerContext::require_character(session) {
                return Some(Err(e));
            }
            let sys_id = session.solar_system_id.unwrap_or(30_000_142);
            let occ_state = ctx.db.with_snapshot(|snap| {
                snap.fw_system_occupations.get(&sys_id).map(|fw| {
                    let frac = if fw.victory_points_threshold > 0 {
                        (fw.victory_points as f64 / fw.victory_points_threshold as f64).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    PyRep::keyval([
                        ("occupierID", PyRep::Integer(fw.occupier_faction_id)),
                        ("attackerID", PyRep::Integer(fw.owner_faction_id)),
                        ("contestedFraction", PyRep::Real(frac)),
                        ("adjacencyState", PyRep::Integer(1)),
                    ])
                })
            });
            // `FwWarzoneSvc.GetLocalOccupationState` unpacks `(solarsystemID, occupationState)`
            // and verifies `solarsystemID == session.solarsystemid2`.
            Some(Ok(PyRep::Tuple(vec![
                PyRep::Integer(i64::from(sys_id)),
                occ_state.unwrap_or(PyRep::None),
            ])))
        }
        ("corpRegistry", "GetEveOwners") => Some(
            crate::services::char_selection::handle_corp_get_eve_owners(ctx, session),
        ),
        ("corpRegistry", "GetCorporation") | ("corpmgr", "GetCorporations") => Some(
            crate::services::char_selection::handle_corp_get_corporation(ctx, session, args),
        ),
        ("corpRegistry", "GetMember") => Some(
            crate::services::char_selection::handle_corp_get_member(ctx, session, args),
        ),
        ("jumpTimers", "GetTimers") => {
            if let Err(e) = ServerContext::require_character(session) {
                return Some(Err(e));
            }
            // const-ok: `CrimewatchService.GetJumpTimers` / `TimerContainer.OnJumpTimersUpdated`
            // unpacks `(jumpActivation, jumpFatigue, lastUpdated)` and deletes both timers when None.
            Some(Ok(PyRep::Tuple(vec![PyRep::None, PyRep::None, PyRep::None])))
        }
        ("eventLog", "LogClientStats") => {
            if let Err(e) = ServerContext::require_character(session) {
                return Some(Err(e));
            }
            // const-ok: `ClientStatsSvc.GatherStats` sends telemetry and discards the return value.
            Some(Ok(PyRep::None))
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
            let bid = match ctx.bind_object(session, "skillMgr2", char_id) {
                Ok(b) => b,
                Err(e) => return Some(Err(e)),
            };
            let oid_str = format!("N={}:{bid}", eve_machonet::MACHO_PROXY_NODE_ID);
            let ref_id = unix_to_filetime(ctx.now_unix());
            Some(Ok(PyRep::SubStruct(Box::new(PyRep::SubStream(Box::new(
                PyRep::Tuple(vec![PyRep::ascii_bytes(oid_str), PyRep::Integer(ref_id)]),
            ))))))
        }
        ("skillMgr2", "GetSkills") | ("skillMgr2", "GetAllSkills") => Some(
            crate::services::char_selection::handle_skill_get_skills(ctx, session),
        ),
        ("skillMgr2", "GetSkillQueueAndFreePoints") => Some(
            crate::services::char_selection::handle_skill_get_queue_and_free_points(ctx, session),
        ),
        ("skillMgr2", "GetAttributes") => {
            if let Err(e) = ServerContext::require_character(session) {
                return Some(Err(e));
            }
            // DERIVED: standard base character attributes (164=charisma, 165=intelligence,
            // 166=memory, 167=perception, 168=willpower) read by `SkillsSvc.GetCharacterAttributes`.
            Some(Ok(PyRep::Dict(vec![
                (PyRep::Integer(164), PyRep::Integer(20)),
                (PyRep::Integer(165), PyRep::Integer(20)),
                (PyRep::Integer(166), PyRep::Integer(20)),
                (PyRep::Integer(167), PyRep::Integer(20)),
                (PyRep::Integer(168), PyRep::Integer(20)),
            ])))
        }
        ("skillMgr2", "GetBoosters") => {
            if let Err(e) = ServerContext::require_character(session) {
                return Some(Err(e));
            }
            // const-ok: `SkillsSvc.GetBoosters` caches a dict of active combat boosters keyed by slot.
            Some(Ok(PyRep::Dict(Vec::new())))
        }
        ("skillMgr2", "GetImplants") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let entries: Vec<(PyRep, PyRep)> = ctx.db.with_snapshot(|snap| {
                snap.active_implants
                    .get(&char_id)
                    .map(|implants| {
                        implants
                            .iter()
                            .enumerate()
                            .map(|(idx, &type_id)| {
                                let slot = ctx
                                    .sde
                                    .types
                                    .get(&type_id)
                                    .and_then(|t| t.default_attributes.get(&331).copied())
                                    .map(|v| v as i64)
                                    .unwrap_or((idx as i64) + 1);
                                (
                                    PyRep::Integer(slot),
                                    PyRep::keyval([
                                        ("implantSlot", PyRep::Integer(slot)),
                                        ("typeID", PyRep::Integer(i64::from(type_id))),
                                    ]),
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            });
            Some(Ok(PyRep::Dict(entries)))
        }
        ("skillMgr2", "GetFreeSkillPoints") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let free_sp = ctx
                .db
                .get_character(char_id)
                .map(|c| c.free_skill_points as i64)
                .unwrap_or(0);
            Some(Ok(PyRep::Integer(free_sp)))
        }
        ("skillMgr2", "GetRespecInfo") => {
            if let Err(e) = ServerContext::require_character(session) {
                return Some(Err(e));
            }
            let now_ft = unix_to_filetime(ctx.now_unix());
            Some(Ok(PyRep::Dict(vec![
                (PyRep::String("nextTimedRespec".into()), PyRep::Integer(now_ft)),
                (PyRep::String("freeRespecs".into()), PyRep::Integer(2)),
            ])))
        }
        ("operationsManager", "can_character_play_the_tutorial") => {
            if let Err(e) = ServerContext::require_character(session) {
                return Some(Err(e));
            }
            // const-ok: `operations/common/util.pyj:is_npe_enabled_for_character` returns False when
            // the character is not in the tutorial flow.
            Some(Ok(PyRep::Bool(false)))
        }
        ("insurgencySolarsystem", "GetAllVisibleCampaigns") => {
            if let Err(e) = ServerContext::require_character(session) {
                return Some(Err(e));
            }
            let campaigns: Vec<PyRep> = ctx.db.with_snapshot(|snap| {
                let mut sys_ids: Vec<u32> = snap.system_states.keys().copied().collect();
                sys_ids.sort_unstable();
                sys_ids
                    .into_iter()
                    .filter_map(|sid| {
                        let st = snap.system_states.get(&sid)?;
                        if st.insurgency_corruption_stage == 0
                            && st.insurgency_suppression_stage == 0
                        {
                            return None;
                        }
                        Some(PyRep::keyval([
                            ("originSolarsystemID", PyRep::Integer(i64::from(st.solar_system_id))),
                            ("pirateFactionID", PyRep::Integer(500_010)),
                            (
                                "coveredSolarsystemIDs",
                                PyRep::List(vec![PyRep::Integer(i64::from(st.solar_system_id))]),
                            ),
                            ("fsmState", PyRep::Integer(2)),
                            ("structureID", PyRep::None),
                        ]))
                    })
                    .collect()
            });
            Some(Ok(PyRep::List(campaigns)))
        }
        ("warRegistry", "GetWars") => {
            if let Err(e) = ServerContext::require_character(session) {
                return Some(Err(e));
            }
            let corp_id = args
                .first()
                .and_then(|a| a.as_i64())
                .or(session.corporation_id)
                .unwrap_or(0);
            let now_ft = unix_to_filetime(ctx.now_unix());
            let entries: Vec<(PyRep, PyRep)> = ctx.db.with_snapshot(|snap| {
                let mut wars: Vec<_> = snap
                    .wars
                    .values()
                    .filter(|w| {
                        corp_id == 0
                            || w.aggressor_corp_id == corp_id
                            || w.defender_corp_id == corp_id
                    })
                    .cloned()
                    .collect();
                wars.sort_by_key(|w| w.war_id);
                wars.into_iter()
                    .map(|w| {
                        (
                            PyRep::Integer(w.war_id),
                            PyRep::keyval([
                                ("warID", PyRep::Integer(w.war_id)),
                                ("declaredByID", PyRep::Integer(w.aggressor_corp_id)),
                                ("againstID", PyRep::Integer(w.defender_corp_id)),
                                ("timeDeclared", PyRep::Integer(now_ft)),
                                ("timeStarted", PyRep::Integer(now_ft)),
                                ("timeFinished", if w.active { PyRep::None } else { PyRep::Integer(now_ft) }),
                                ("retracted", PyRep::None),
                                ("retractedBy", PyRep::None),
                                ("billID", PyRep::None),
                                ("mutual", PyRep::Bool(w.mutual)),
                                ("openForAllies", PyRep::Bool(false)),
                                ("allies", PyRep::Dict(Vec::new())),
                            ]),
                        )
                    })
                    .collect()
            });
            // `war_cso.PopulateCacheAndCleanupWars` iterates `wars.itervalues()`.
            Some(Ok(PyRep::Dict(entries)))
        }
        ("air_npe", "get_air_npe_state") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            // `storylines/common/airnpeconstants.pyj:AirNpeState`: ACTIVE=1, COMPLETED=2, SKIPPED=3.
            // Returning COMPLETED (2) or SKIPPED (3) ensures `AirNpeController.OnAirNpeStateChanged`
            // reveals the full station UI instead of locking camera/UI for an unstarted tutorial.
            let state_id = ctx.db.with_snapshot(|snap| {
                match snap.npe_progress.get(&char_id) {
                    Some(n) if n.completed => 2_i64,
                    _ => 3_i64,
                }
            });
            Some(Ok(PyRep::Integer(state_id)))
        }
        ("fighterMgr", "GetFightersForShip") if caller_trace == Some("machoNet:26000") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let ship_id = session.ship_id.unwrap_or(0);
            let tubes: Vec<PyRep> = ctx.db.with_snapshot(|snap| {
                snap.items
                    .values()
                    .filter(|i| {
                        i.owner_id == char_id
                            && i.location_id == ship_id
                            && i.flag == eve_db::InventoryFlag::FighterBay
                    })
                    .enumerate()
                    .map(|(idx, i)| {
                        // `(tubeFlagID, fighterItemID, fighterTypeID, squadronSize)` unpacked by
                        // `ShipFighterState._UpdateStateForShip`.
                        PyRep::Tuple(vec![
                            PyRep::Integer(133 + idx as i64),
                            PyRep::Integer(i.item_id),
                            PyRep::Integer(i64::from(i.type_id)),
                            PyRep::Integer(i64::from(i.quantity)),
                        ])
                    })
                    .collect()
            });
            Some(Ok(PyRep::Tuple(vec![
                PyRep::List(tubes),
                PyRep::List(Vec::new()),
                PyRep::Dict(Vec::new()),
            ])))
        }
        ("agentMgr", "GetAgents") if args.is_empty() => {
            let target_station = session.station_id;
            let target_system = session.solar_system_id;
            let mut matching: Vec<&eve_sde::AgentInfo> = ctx
                .sde
                .agents
                .values()
                .filter(|a| {
                    target_station == a.station_id
                        || target_system == a.solar_system_id
                        || a.level == 1
                })
                .collect();
            matching.sort_by_key(|a| a.agent_id);
            if matching.len() > 200 {
                matching.truncate(200);
            }
            let rows: Vec<Vec<PyRep>> = matching
                .into_iter()
                .map(|a| {
                    let corp_faction = ctx
                        .sde
                        .npc_corporations
                        .get(&a.corporation_id)
                        .and_then(|c| c.faction_id)
                        .map(i64::from);
                    let sys_rep = a
                        .solar_system_id
                        .map(|s| PyRep::Integer(i64::from(s)))
                        .unwrap_or(PyRep::None);
                    vec![
                        PyRep::Integer(a.agent_id),
                        PyRep::Integer(i64::from(a.agent_type_id)),
                        PyRep::Integer(i64::from(a.division_id)),
                        PyRep::Integer(a.corporation_id),
                        corp_faction.map(PyRep::Integer).unwrap_or(PyRep::None),
                        sys_rep.clone(),
                        sys_rep,
                        a.station_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                        PyRep::Integer(i64::from(a.level)),
                        PyRep::Integer(20),
                        PyRep::Integer(1),
                        PyRep::Bool(true),
                    ]
                })
                .collect();
            // `Agents.__GetAllAgents` calls `agents.Filter('solarsystemID')` (lowercase 's').
            Some(Ok(PyRep::rowset(
                vec![
                    "agentID",
                    "agentTypeID",
                    "divisionID",
                    "corporationID",
                    "factionID",
                    "solarsystemID",
                    "solarSystemID",
                    "stationID",
                    "level",
                    "quality",
                    "bloodlineID",
                    "gender",
                ],
                rows,
            )))
        }
        ("station", "GetGuests") | ("stationSvc", "GetGuests") => {
            if let Err(e) = ServerContext::require_character(session) {
                return Some(Err(e));
            }
            let station_id = session.station_id;
            let guests: Vec<PyRep> = ctx.db.with_snapshot(|snap| {
                let mut chars: Vec<_> = snap
                    .characters
                    .values()
                    .filter(|c| station_id.is_some() && c.station_id == station_id)
                    .cloned()
                    .collect();
                chars.sort_by_key(|c| c.character_id);
                chars
                    .into_iter()
                    .map(|c| {
                        // `StationSvc.GetGuests` unpacks `(charID, corpID, allianceID, warFactionID)`.
                        PyRep::Tuple(vec![
                            PyRep::Integer(c.character_id),
                            PyRep::Integer(c.corporation_id),
                            c.alliance_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                            c.war_faction_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                        ])
                    })
                    .collect()
            });
            Some(Ok(PyRep::List(guests)))
        }
        ("stationSvc", "GetStationItemBits") | ("station", "GetStationItemBits") => {
            if let Err(e) = ServerContext::require_character(session) {
                return Some(Err(e));
            }
            let station_id = session.station_id.unwrap_or(60_003_760);
            let (owner_id, op_id, st_type_id) = ctx
                .sde
                .stations
                .get(&station_id)
                .map(|st| {
                    (
                        st.owner_corporation_id,
                        i64::from(st.operation_id),
                        i64::from(st.station_type_id),
                    )
                })
                .unwrap_or((1_000_035, 26, 1529));
            // `StationSvc.SetStationItemBits` builds `Row(['ownerID', 'itemID', 'operationID', 'stationTypeID'], bits)`.
            Some(Ok(PyRep::Tuple(vec![
                PyRep::Integer(owner_id),
                PyRep::Integer(station_id),
                PyRep::Integer(op_id),
                PyRep::Integer(st_type_id),
            ])))
        }
        ("corpStationMgr", "DoStandingCheckForStationService") => {
            if let Err(e) = ServerContext::require_character(session) {
                return Some(Err(e));
            }
            // const-ok: `StationController._DoStandingCheckForServices` returns None when station
            // service access is permitted.
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
