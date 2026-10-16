//! `auth_cache` — Domain 1: Session, Authentication, Object Caching, Config, & Quasar Gateway (`§3.1`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_db::IskCents;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

const V24_01_VERSION: f64 = 24.01;
const V24_01_BUILD: i64 = 3_619_879;
const WIN32_FILETIME_EPOCH_DIFF_TICKS: i64 = 116_444_736_000_000_000;

fn current_blueos_filetime_ticks() -> i64 {
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let unix_ticks = (dur.as_secs() as i64)
        .saturating_mul(10_000_000)
        .saturating_add((dur.subsec_nanos() / 100) as i64);
    WIN32_FILETIME_EPOCH_DIFF_TICKS.saturating_add(unix_ticks)
}

pub fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("machoNet", "GetInitVals") | ("machoNet", "GetServiceInfo") => {
            let service_entries = vec![
                (
                    PyRep::String("stationSvc".into()),
                    PyRep::String("station".into()),
                ),
                (
                    PyRep::String("charUnboundMgr".into()),
                    PyRep::String("character".into()),
                ),
                (
                    PyRep::String("dogmaIM".into()),
                    PyRep::String("dogma".into()),
                ),
            ];
            let cluster_info = vec![(
                PyRep::String("catalogedServices".into()),
                PyRep::Integer(ctx.cataloged_services.len() as i64),
            )];
            Some(Ok(PyRep::Tuple(vec![
                PyRep::Dict(service_entries),
                PyRep::Dict(cluster_info),
            ])))
        }

        ("machoNet", "GetTime") => {
            let sde_systems = ctx.sde.solar_systems.len() as i64;
            let ticks = current_blueos_filetime_ticks().saturating_add(sde_systems);
            Some(Ok(PyRep::Integer(ticks)))
        }

        ("cache", "GetServerVersionAndBuild") => {
            let ticks = current_blueos_filetime_ticks();
            Some(Ok(PyRep::keyval([
                ("version", PyRep::Real(V24_01_VERSION)),
                ("build", PyRep::Integer(V24_01_BUILD as i64)),
                ("sdeTypes", PyRep::Integer(ctx.sde.types.len() as i64)),
                ("timestamp", PyRep::Integer(ticks)),
            ])))
        }

        ("authentication", "Login") => {
            let username = args.first().and_then(|a| a.as_str()).unwrap_or("");
            let token = args.get(1).and_then(|a| a.as_str()).unwrap_or("");
            let res = ctx
                .db
                .authenticate(username, token)
                .map_err(|e| BridgeRpcError {
                    code: "AuthenticationFailed".into(),
                    message: e.to_string(),
                });
            let acc = match res {
                Ok(a) => a,
                Err(e) => return Some(Err(e)),
            };

            ctx.release_session_bindings(session.session_id);
            session.user_id = Some(acc.user_id);
            session.role = acc.role;
            session.character_id = None;
            session.corporation_id = None;
            session.alliance_id = None;
            session.station_id = None;
            session.structure_id = None;
            session.solar_system_id = None;
            session.constellation_id = None;
            session.region_id = None;
            session.ship_id = None;
            session.race_id = None;
            session.corp_role = 0;
            session.fleet_id = None;
            session.war_faction_id = None;

            Some(Ok(PyRep::keyval([
                ("userID", PyRep::Integer(acc.user_id as i64)),
                ("role", PyRep::Integer(acc.role as i64)),
            ])))
        }

        ("authentication", "Logout") => {
            ctx.release_session_bindings(session.session_id);
            session.user_id = None;
            session.character_id = None;
            session.corporation_id = None;
            session.alliance_id = None;
            session.station_id = None;
            session.structure_id = None;
            session.solar_system_id = None;
            session.constellation_id = None;
            session.region_id = None;
            session.ship_id = None;
            Some(Ok(PyRep::keyval([("loggedOut", PyRep::Bool(true))])))
        }

        ("objectCaching", "GetCachableObject") | ("objectCaching", "GetCachedObject") => {
            let _ = match ServerContext::require_auth(session) {
                Ok(u) => u,
                Err(e) => return Some(Err(e)),
            };
            let db_version = ctx.db.with_snapshot(|s| s.next_entity_id);
            Some(Ok(PyRep::keyval([
                ("objectID", args.first().cloned().unwrap_or(PyRep::None)),
                ("version", PyRep::Integer(db_version)),
                ("nodeID", PyRep::Integer(888444)),
            ])))
        }

        ("config", "GetMultiOwnersEx") => {
            let _ = match ServerContext::require_auth(session) {
                Ok(u) => u,
                Err(e) => return Some(Err(e)),
            };
            let filter_ids: Vec<i64> = match args.first() {
                Some(PyRep::List(items)) | Some(PyRep::Tuple(items)) => {
                    items.iter().filter_map(|v| v.as_i64()).collect()
                }
                Some(PyRep::Integer(id)) => vec![*id],
                _ => Vec::new(),
            };
            let rows = ctx.db.with_snapshot(|snap| {
                snap.characters
                    .values()
                    .filter(|c| filter_ids.is_empty() || filter_ids.contains(&c.character_id))
                    .map(|c| {
                        vec![
                            PyRep::Integer(c.character_id),
                            PyRep::String(c.name.clone()),
                            PyRep::Integer(c.corporation_id),
                        ]
                    })
                    .collect()
            });
            Some(Ok(PyRep::rowset(
                vec!["ownerID", "ownerName", "corporationID"],
                rows,
            )))
        }

        ("config", "GetMap") | ("config", "GetMapObjects") => {
            let _ = match ServerContext::require_auth(session) {
                Ok(u) => u,
                Err(e) => return Some(Err(e)),
            };
            let rows = ctx
                .sde
                .solar_systems
                .values()
                .map(|s| {
                    vec![
                        PyRep::Integer(s.solar_system_id as i64),
                        PyRep::String(s.name.clone()),
                        PyRep::Real(s.security_status),
                    ]
                })
                .collect();
            Some(Ok(PyRep::rowset(
                vec!["solarSystemID", "solarSystemName", "security"],
                rows,
            )))
        }

        ("sessionMgr", "PerformSessionChange") | ("sessionMgr", "RemoveSessionsFromServer") => {
            let _ = match ServerContext::require_auth(session) {
                Ok(u) => u,
                Err(e) => return Some(Err(e)),
            };
            if method == "RemoveSessionsFromServer" {
                ctx.release_session_bindings(session.session_id);
            }
            Some(Ok(PyRep::keyval([
                ("sessionID", PyRep::Integer(session.session_id as i64)),
                (
                    "solarSystemID",
                    session
                        .solar_system_id
                        .map(|s| PyRep::Integer(s as i64))
                        .unwrap_or(PyRep::None),
                ),
            ])))
        }

        ("onlineStatus", "GetInitialState") | ("onlineStatus", "GetOnlineStatus") => {
            let uid = match ServerContext::require_auth(session) {
                Ok(u) => u,
                Err(e) => return Some(Err(e)),
            };
            let target_char = args
                .first()
                .and_then(|a| a.as_i64())
                .or(session.character_id)
                .unwrap_or(0);
            let exists = ctx
                .db
                .with_snapshot(|snap| snap.characters.contains_key(&target_char));
            Some(Ok(PyRep::keyval([
                ("userID", PyRep::Integer(uid as i64)),
                ("characterID", PyRep::Integer(target_char)),
                ("online", PyRep::Bool(exists)),
            ])))
        }

        ("subscriptionMgr", "GetCloneGrade") | ("loginRewardFacilities", "get_my_clone_grade") => {
            let uid = match ServerContext::require_auth(session) {
                Ok(u) => u,
                Err(e) => return Some(Err(e)),
            };
            let clone_grade = session
                .character_id
                .and_then(|cid| ctx.db.get_character(cid).ok())
                .map(|c| c.clone_grade as i64)
                .unwrap_or(2);
            Some(Ok(PyRep::keyval([
                ("userID", PyRep::Integer(uid as i64)),
                ("cloneGrade", PyRep::Integer(clone_grade)),
                ("subscriptionDaysRemaining", PyRep::Integer(30)),
            ])))
        }

        ("slash", "SlashCmd") => {
            let _ = match ServerContext::require_auth(session) {
                Ok(u) => u,
                Err(e) => return Some(Err(e)),
            };
            if (session.role & 0x02) == 0 {
                return Some(Err(BridgeRpcError {
                    code: "InsufficientRole".into(),
                    message: "slash.SlashCmd requires GM/Admin role bit 0x02".into(),
                }));
            }
            let cmd = args.first().and_then(|a| a.as_str()).unwrap_or("");
            if let Some(rest) = cmd.strip_prefix("/giveisk ") {
                let amount = rest.trim().parse::<i64>().unwrap_or(0).max(0);
                if let Some(cid) = session.character_id {
                    ctx.db.with_mut_snapshot(|snap| {
                        if let Some(ch) = snap.characters.get_mut(&cid) {
                            let add = IskCents::from_isk(amount);
                            ch.balance = ch.balance.checked_add(add).unwrap_or(ch.balance);
                        }
                    });
                }
            }
            let total_chars = ctx.db.with_snapshot(|s| s.characters.len());
            Some(Ok(PyRep::String(format!(
                "Executed GM command '{cmd}' (characters={total_chars})"
            ))))
        }

        _ => None,
    }
}
