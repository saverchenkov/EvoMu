//! Compiled RPC registry table for extended agent, standings, epic-arc, and NPE handlers.

#![forbid(unsafe_code)]

use std::collections::{BTreeSet, HashMap, HashSet};

use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, ClientError, PyRep};

use crate::router::rpc_registry::{rpc_entry, RpcBoxFuture, RpcDispatchHandler, RpcEntry};
use crate::router::{BoundObjectEntry, ServerContext};

fn invalid_parameter_error(message: &str) -> BridgeRpcError {
    BridgeRpcError::with_client_error(
        "InvalidParameter",
        message,
        ClientError::CustomInfo(format!("InvalidParameter: {message}")),
    )
}

fn handle_get_completed_career_agent_ids(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let mut requested_ids = Vec::new();
    let mut seen_ids = HashSet::new();
    if let Some(arg0) = crate::router::RpcArgs::new(args, kwargs).get_raw(0, "agentIDs") {
        match arg0 {
            PyRep::None => {}
            PyRep::List(items)
            | PyRep::Tuple(items)
            | PyRep::Set(items)
            | PyRep::PySet(items) => {
                for item in items {
                    let Some(aid) = item.as_i64().filter(|&id| id > 0) else {
                        return Some(Err(invalid_parameter_error(
                            "agentIDs must contain positive integer agent IDs",
                        )));
                    };
                    if seen_ids.insert(aid) {
                        requested_ids.push(aid);
                    }
                }
            }
            PyRep::Integer(aid) if *aid > 0 => {
                requested_ids.push(*aid);
            }
            _ => {
                return Some(Err(invalid_parameter_error(
                    "agentIDs must be a list, tuple, set, or positive integer",
                )));
            }
        }
    }
    // V24.01 `agents.pyj:217-223` calls `self._careerAgentsCompletedByID.update(...)`
    // and indexes `self._careerAgentsCompletedByID[agentID]` directly, so every queried
    // `agentID` must be mapped to `PyRep::Bool(completed)` in the returned `PyRep::Dict`.
    let completed_set: BTreeSet<i64> = ctx.db.with_snapshot(|snap| {
        snap.missions
            .iter_history_by_character(char_id)
            .filter(|e| e.final_state == eve_db::MissionState::Completed)
            .filter_map(|e| {
                ctx.sde
                    .agents
                    .get(&e.agent_id)
                    .filter(|a| a.agent_type_id == 12)
                    .map(|a| a.agent_id)
            })
            .collect()
    });
    let entries: Vec<(PyRep, PyRep)> = if requested_ids.is_empty() {
        completed_set
            .into_iter()
            .map(|aid| (PyRep::Integer(aid), PyRep::Bool(true)))
            .collect()
    } else {
        requested_ids
            .into_iter()
            .map(|aid| (PyRep::Integer(aid), PyRep::Bool(completed_set.contains(&aid))))
            .collect()
    };
    Some(Ok(PyRep::Dict(entries)))
}

pub(crate) async fn handle_extended_with_bound(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    use crate::services::agents_missions::agents_standings_arcs as asa;
    match (service, method) {
        ("agentMgr", "agentID") => {
            asa::handle_agent_mgr_attr(ctx, session, bound_entry, service, "agentID", args)
        }
        ("agentMgr", "corporationID") => {
            asa::handle_agent_mgr_attr(ctx, session, bound_entry, service, "corporationID", args)
        }
        ("agentMgr", "factionID") => {
            asa::handle_agent_mgr_attr(ctx, session, bound_entry, service, "factionID", args)
        }
        ("agentMgr", "solarsystemID") => {
            asa::handle_agent_mgr_attr(ctx, session, bound_entry, service, "solarsystemID", args)
        }
        ("agentMgr", "stationID") => {
            asa::handle_agent_mgr_attr(ctx, session, bound_entry, service, "stationID", args)
        }
        ("agentMgr", "GetCompletedCareerAgentIDs") => {
            handle_get_completed_career_agent_ids(ctx, session, args, kwargs)
        }
        // parity: rpc:agentMgr.GetDisabledMissions
        ("agentMgr", "GetDisabledMissions") => asa::handle_get_disabled_missions(ctx, session),
        ("agentMgr", "GetReplayTimestamp") => {
            asa::handle_get_replay_timestamp(ctx, session, bound_entry, args)
        }
        ("agentMgr", "GetPlausibleMissionIDs") => {
            asa::handle_get_plausible_mission_ids(ctx, session, bound_entry, args)
        }
        ("agentMgr", "GetMyEpicArcStatus") => asa::handle_get_my_epic_arc_status(ctx, session),
        ("agentMgr", "GetMessagesForEpicArcMissions") => {
            asa::handle_get_messages_for_epic_arc_missions(session)
        }
        ("epicArcStatus", "AgentHasEpicMissionsForCharacter") => {
            asa::handle_agent_has_epic_missions_for_character(ctx, session, args)
        }
        ("missionTrackerMgr", "GetAllMissionObjectives")
        | ("missionTrackerMgr", "UpdateAllMissions")
        | ("missionTrackerMgr", "GetMissionInfoItems") => {
            asa::handle_mission_tracker_list(ctx, session, method, args)
        }
        ("missionTrackerMgr", "IsInActiveDungeonID") => {
            asa::handle_is_in_active_dungeon_id(ctx, session)
        }
        ("standingMgr", "GetCorpStandings") => asa::handle_get_corp_standings(ctx, session),
        ("standingMgr", "GetNPCNPCStandings") => asa::handle_get_npc_npc_standings(ctx, session),
        ("standingMgr", "GetStandingCompositions") => {
            asa::handle_get_standing_compositions(ctx, session, args)
        }
        ("keeper", "ActivateAccelerationGate") => {
            asa::handle_activate_acceleration_gate(ctx, session, args).await
        }
        ("air_npe", "skip_air_npe") => asa::handle_skip_air_npe(ctx, session),
        ("air_npe", "get_air_npe_state") => asa::handle_get_air_npe_state(ctx, session),
        _ => None,
    }
}

fn dispatch_rpc<'a>(
    ctx: &'a ServerContext,
    session: &'a mut SessionState,
    bound_entry: Option<&'a BoundObjectEntry>,
    service: &'a str,
    method: &'a str,
    args: &'a [PyRep],
    kwargs: &'a HashMap<String, PyRep>,
) -> RpcBoxFuture<'a> {
    Box::pin(handle_extended_with_bound(
        ctx,
        session,
        bound_entry,
        service,
        method,
        args,
        kwargs,
    ))
}

pub const RPCS: &[RpcEntry] = &[
    rpc_entry("agentMgr", "agentID", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("agentMgr", "corporationID", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("agentMgr", "factionID", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("agentMgr", "solarsystemID", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("agentMgr", "stationID", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("agentMgr", "GetCompletedCareerAgentIDs", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("agentMgr", "GetDisabledMissions", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("agentMgr", "GetReplayTimestamp", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("agentMgr", "GetPlausibleMissionIDs", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("agentMgr", "GetMyEpicArcStatus", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("agentMgr", "GetMessagesForEpicArcMissions", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("epicArcStatus", "AgentHasEpicMissionsForCharacter", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("missionTrackerMgr", "GetAllMissionObjectives", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("missionTrackerMgr", "UpdateAllMissions", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("missionTrackerMgr", "GetMissionInfoItems", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("missionTrackerMgr", "IsInActiveDungeonID", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("standingMgr", "GetCorpStandings", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("standingMgr", "GetNPCNPCStandings", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("standingMgr", "GetStandingCompositions", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("air_npe", "skip_air_npe", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("air_npe", "get_air_npe_state", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
];
