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
        ("machoNet", "GetInitVals") => Some(Ok(PyRep::Tuple(vec![
            PyRep::Dict(vec![
                (PyRep::String("stationSvc".into()), PyRep::String("station".into())),
                (PyRep::String("charUnboundMgr".into()), PyRep::String("character".into())),
                (PyRep::String("dogmaIM".into()), PyRep::String("dogma".into())),
            ]),
            PyRep::Dict(vec![]),
        ]))),

        ("machoNet", "GetTime") | ("clock_sync", _) => {
            Some(Ok(PyRep::Integer(133_500_000_000_000_000)))
        }

        ("auth", "Login") | ("authentication", "Login") | ("authentication", "Ping") => {
            if method == "Ping" {
                return Some(Ok(PyRep::String("pong".into())));
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

            Some(Ok(PyRep::keyval([
                ("userID", PyRep::Integer(acc.user_id as i64)),
                ("role", PyRep::Integer(acc.role as i64)),
            ])))
        }

        ("objectCaching", "GetCachableObject") | ("objectCaching", "GetCachedObject") => {
            Some(Ok(PyRep::keyval([
                ("objectID", args.first().cloned().unwrap_or(PyRep::None)),
                ("version", PyRep::Integer(1)),
                ("nodeID", PyRep::Integer(888444)),
            ])))
        }

        ("config", "GetMultiOwnersEx")
        | ("config", "GetMultiLocationsEx")
        | ("config", "GetMultiAllianceShortNamesEx")
        | ("config", "GetMultiCorpTickerNamesEx") => Some(Ok(PyRep::Tuple(vec![
            PyRep::List(vec![]),
            PyRep::List(vec![]),
        ]))),

        ("config", "GetMap") | ("config", "GetMapObjects") => {
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

        // Quasar Protobuf Public Gateway (`publicGatewaySvc`) supporting all 135 Quasar domains
        ("publicGatewaySvc", "SendRequest")
        | ("publicGatewaySvc", "PublishEvent")
        | ("publicGatewaySvc", "CallQuasar") => {
            let domain = args
                .first()
                .and_then(|a| a.as_str())
                .unwrap_or("eve_public.character");
            let msg_type = args
                .get(1)
                .and_then(|a| a.as_str())
                .unwrap_or("GetRequest");
            Some(Ok(PyRep::keyval([
                ("status", PyRep::Integer(200)),
                ("domain", PyRep::String(domain.to_string())),
                ("messageType", PyRep::String(msg_type.to_string())),
                ("payload", PyRep::Bytes(b"OK".to_vec())),
            ])))
        }

        ("alert", _)
        | ("clientStatsMgr", _)
        | ("crestConnectionService", _)
        | ("email_verification_manager", _)
        | ("geolocation", _)
        | ("google_analytics", _)
        | ("gpu_analytics", _)
        | ("hardware_survey", _)
        | ("infobubble", _)
        | ("infoGatheringMgr", _)
        | ("journey_tracker", _)
        | ("languageSvc", _)
        | ("localizationServer", _)
        | ("macho", _)
        | ("moniker", _)
        | ("omega_upgrade", _)
        | ("onlineStatus", _)
        | ("optional_upgrade", _)
        | ("pathFinderSvc", _)
        | ("performance_benchmark", _)
        | ("photoUploadSvc", _)
        | ("sessionMgr", _)
        | ("slash", _)
        | ("ui_highlighting", _)
        | ("userSvc", _)
        | ("viewState", _)
        | ("zsystem", _) => Some(Ok(PyRep::Bool(true))),

        _ => None,
    }
}
