//! `auth_cache` — Domain 1: Session, Authentication, Object Caching, Config, & Quasar Gateway (`§3.1`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_db::IskCents;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;
const V24_01_VERSION: f64 = 24.01;
const V24_01_BUILD: i64 = 3_619_879;

pub fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("machoNet", "GetInitVals") => {
            let mut sorted_svcs: Vec<&String> = ctx.cataloged_services.iter().collect();
            sorted_svcs.sort();
            let service_entries: Vec<(PyRep, PyRep)> = sorted_svcs
                .into_iter()
                .map(|svc| (PyRep::String(svc.clone()), PyRep::String("sol".into())))
                .collect();
            let cluster_info = vec![(
                PyRep::String("catalogedServices".into()),
                PyRep::Integer(ctx.cataloged_services.len() as i64),
            )];
            Some(Ok(PyRep::Tuple(vec![
                PyRep::Dict(service_entries),
                PyRep::Dict(cluster_info),
            ])))
        }

        ("machoNet", "GetServiceInfo") => {
            let mut sorted_svcs: Vec<&String> = ctx.cataloged_services.iter().collect();
            sorted_svcs.sort();
            let service_entries: Vec<(PyRep, PyRep)> = sorted_svcs
                .into_iter()
                .map(|svc| (PyRep::String(svc.clone()), PyRep::String("sol".into())))
                .collect();
            Some(Ok(PyRep::Dict(service_entries)))
        }

        ("machoNet", "GetTime") => {
            let ticks = ctx.now_blueos_filetime_ticks();
            Some(Ok(PyRep::Integer(ticks)))
        }

        ("cache", "GetServerVersionAndBuild") => {
            let ticks = ctx.now_blueos_filetime_ticks();
            Some(Ok(PyRep::keyval([
                ("version", PyRep::Real(V24_01_VERSION)),
                ("build", PyRep::Integer(V24_01_BUILD)),
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

        ("subscriptionMgr", "GetCloneGrade") => {
            let uid = match ServerContext::require_auth(session) {
                Ok(u) => u,
                Err(e) => return Some(Err(e)),
            };
            let clone_grade_u8 = session
                .character_id
                .and_then(|cid| ctx.db.get_character(cid).ok())
                .map(|c| c.clone_grade)
                .unwrap_or(1);
            let (max_sp, speed_mult) = ctx
                .sde
                .clone_grades
                .get(&clone_grade_u8)
                .map(|g| (g.max_skill_points, g.training_speed_multiplier))
                .unwrap_or((5_000_000, 0.5));
            Some(Ok(PyRep::keyval([
                ("userID", PyRep::Integer(uid as i64)),
                ("cloneGrade", PyRep::Integer(clone_grade_u8 as i64)),
                ("maxSkillPoints", PyRep::Integer(max_sp as i64)),
                ("trainingSpeedMultiplier", PyRep::Real(speed_mult)),
            ])))
        }

        ("loginRewardFacilities", "get_my_clone_grade") => {
            let _ = match ServerContext::require_auth(session) {
                Ok(u) => u,
                Err(e) => return Some(Err(e)),
            };
            let clone_grade_u8 = session
                .character_id
                .and_then(|cid| ctx.db.get_character(cid).ok())
                .map(|c| c.clone_grade)
                .unwrap_or(1);
            Some(Ok(PyRep::Integer(clone_grade_u8 as i64)))
        }

        ("slash", "SlashCmd") => {
            let (_, cid) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if !session.has_gm_role() {
                return Some(Err(BridgeRpcError {
                    code: "InsufficientRole".into(),
                    message: "slash.SlashCmd requires GM/Admin role (ROLE_GMS/ROLE_GML/ROLE_GMH/ROLE_ADMIN)".into(),
                }));
            }
            let cmd = args.first().and_then(|a| a.as_str()).unwrap_or("").trim();
            if let Some(rest) = cmd.strip_prefix("/giveisk ") {
                let Ok(amount) = rest.trim().parse::<i64>() else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Usage: /giveisk <positive_integer_amount>".into(),
                    }));
                };
                if amount <= 0 {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Amount for /giveisk must be positive".into(),
                    }));
                }
                let new_bal = ctx.db.try_with_mut_snapshot(|snap| {
                    let ch = snap.characters.get_mut(&cid).ok_or_else(|| BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {cid} not found"),
                    })?;
                    let add = IskCents::from_isk(amount);
                    ch.balance = ch.balance.checked_add(add).ok_or_else(|| BridgeRpcError {
                        code: "BalanceOverflow".into(),
                        message: "Adding ISK would overflow balance".into(),
                    })?;
                    Ok(ch.balance.to_wire_f64())
                });
                match new_bal {
                    Ok(bal) => Some(Ok(PyRep::keyval([
                        ("command", PyRep::String(cmd.to_string())),
                        ("newBalanceISK", PyRep::Real(bal)),
                    ]))),
                    Err(e) => Some(Err(e)),
                }
            } else {
                Some(Err(BridgeRpcError {
                    code: "UnknownSlashCommand".into(),
                    message: format!("Unknown or unsupported slash command '{cmd}'"),
                }))
            }
        }

        _ => None,
    }
}
