//! `auth_cache` — Domain 1: Session, Authentication, Object Caching, Config, & Quasar Gateway (`§3.1`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

pub fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("machoNet", "GetInitVals")
        | ("machoNet", "GetServiceInfo")
        | ("machoNet", "GetClusterGameStatisticsForClient")
        | ("machoNet", "NoBlock")
        | ("machoNet", "ForwardCharacterNotification") => {
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

        ("machoNet", "GetTime") | ("cache", "GetServerVersionAndBuild") => {
            let base_ticks: i64 = 133_500_000_000_000_000;
            let offset = (session.session_id as i64).saturating_mul(10_000_000);
            if service == "cache" {
                Some(Ok(PyRep::keyval([
                    ("version", PyRep::Real(24.01)),
                    ("build", PyRep::Integer(2600000)),
                    ("timestamp", PyRep::Integer(base_ticks + offset)),
                ])))
            } else {
                Some(Ok(PyRep::Integer(base_ticks + offset)))
            }
        }

        ("authentication", "Login")
        | ("authentication", "Logout")
        | ("authentication", "SetLanguageID")
        | ("authentication", "AccruedTime")
        | ("authentication", "AmUnderage")
        | ("authentication", "Ping")
        | ("pingService", "Ping") => {
            if method == "Ping" {
                return Some(Ok(PyRep::String("pong".into())));
            }
            if method == "Logout" {
                ctx.release_session_bindings(session.session_id);
                session.user_id = None;
                session.character_id = None;
                return Some(Ok(PyRep::keyval([(
                    "loggedOut",
                    PyRep::Bool(true),
                )])));
            }
            if method == "SetLanguageID" || method == "AccruedTime" || method == "AmUnderage" {
                let uid = match ServerContext::require_auth(session) {
                    Ok(u) => u,
                    Err(e) => return Some(Err(e)),
                };
                return Some(Ok(PyRep::keyval([
                    ("userID", PyRep::Integer(uid as i64)),
                    ("underage", PyRep::Bool(false)),
                    ("accruedTime", PyRep::Integer(0)),
                ])));
            }
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

        ("objectCaching", "GetCachableObject")
        | ("objectCaching", "GetCachedObject")
        | ("machoNet", "GetCachedObject") => {
            let _ = match ServerContext::require_auth(session) {
                Ok(u) => u,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(PyRep::keyval([
                ("objectID", args.first().cloned().unwrap_or(PyRep::None)),
                ("version", PyRep::Integer(1)),
                ("nodeID", PyRep::Integer(888444)),
            ])))
        }

        ("config", "GetMultiOwnersEx")
        | ("config", "GetMultiLocationsEx")
        | ("config", "GetMultiAllianceShortNamesEx")
        | ("config", "GetMultiCorpTickerNamesEx")
        | ("config", "GetStationSolarSystemsByOwner") => {
            let _ = match ServerContext::require_auth(session) {
                Ok(u) => u,
                Err(e) => return Some(Err(e)),
            };
            let rows = ctx.db.with_snapshot(|snap| {
                snap.characters
                    .values()
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

        ("alert", "BeanCount")
        | ("alert", "BeanDelivery")
        | ("alert", "GetLogModeForError")
        | ("alert", "GroupBeanDelivery")
        | ("alert", "SendClientStackTraceAlert") => {
            let _ = match ServerContext::require_auth(session) {
                Ok(u) => u,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(PyRep::keyval([
                ("logged", PyRep::Bool(true)),
                ("sessionID", PyRep::Integer(session.session_id as i64)),
                ("method", PyRep::String(method.to_string())),
            ])))
        }

        ("clientStatsMgr", "SubmitStats")
        | ("infoGatheringMgr", "GetStateAndConfig")
        | ("infoGatheringMgr", "LogInfoEventsFromClient")
        | ("eventLog", "LogClientEvent")
        | ("eventLog", "LogClientStats")
        | ("eventLog", "LogPlayerRequestedDisconnect") => {
            let _ = match ServerContext::require_auth(session) {
                Ok(u) => u,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(PyRep::keyval([
                ("accepted", PyRep::Bool(true)),
                ("sessionID", PyRep::Integer(session.session_id as i64)),
                ("argCount", PyRep::Integer(args.len() as i64)),
            ])))
        }

        ("sessionMgr", "PerformSessionChange")
        | ("sessionMgr", "GetSessionStatistics")
        | ("sessionMgr", "RemoveSessionsFromServer") => {
            let _ = match ServerContext::require_auth(session) {
                Ok(u) => u,
                Err(e) => return Some(Err(e)),
            };
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

        ("userSvc", "ClaimRedeemTokens")
        | ("userSvc", "ActivateMultiTraining")
        | ("userSvc", "ActivateSoulboundMultiTraining")
        | ("userSvc", "ActivateCharacterReSculpt")
        | ("userSvc", "CastUpvote")
        | ("userSvc", "CastDownvote")
        | ("subscriptionMgr", "GetCloneGrade")
        | ("subscriptionMgr", "GetSubscriptionTime")
        | ("loginRewardFacilities", "get_my_clone_grade") => {
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

        ("slash", "SlashCmd")
        | ("publicQaToolsServer", "SlashCmd")
        | ("publicQaToolsServer", "MoveMeTo")
        | ("publicQaToolsServer", "CanGiveItemForMultifit") => {
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
            Some(Ok(PyRep::String(format!("Executed GM command: {cmd}"))))
        }

        _ => None,
    }
}
