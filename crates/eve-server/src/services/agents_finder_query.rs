//! `agents_finder_query` — `agentMgr.GetAgents` SDE agent finder query helper.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

use super::compute_effective_agent_standing;

pub(super) fn handle_get_agents(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let caller_char = session.character_id;
    let filter_level = args
        .first()
        .and_then(|a| a.as_i64())
        .map(|v| v as u8)
        .or_else(|| kwargs.get("level").and_then(|v| v.as_i64()).map(|v| v as u8));
    let filter_division = args
        .get(1)
        .and_then(|a| a.as_i64())
        .map(|v| v as u32)
        .or_else(|| {
            kwargs
                .get("divisionID")
                .or_else(|| kwargs.get("division"))
                .and_then(|v| v.as_i64())
                .map(|v| v as u32)
        });
    let filter_faction = args.get(2).and_then(|a| a.as_i64()).or_else(|| {
        kwargs
            .get("factionID")
            .or_else(|| kwargs.get("faction"))
            .and_then(|v| v.as_i64())
    });
    let filter_corp = args.get(3).and_then(|a| a.as_i64()).or_else(|| {
        kwargs
            .get("corporationID")
            .or_else(|| kwargs.get("corporation"))
            .and_then(|v| v.as_i64())
    });
    let filter_agent_type = args
        .get(4)
        .and_then(|a| a.as_i64())
        .map(|v| v as u32)
        .or_else(|| {
            kwargs
                .get("agentTypeID")
                .or_else(|| kwargs.get("agentType"))
                .and_then(|v| v.as_i64())
                .map(|v| v as u32)
        });
    let filter_sec = kwargs
        .get("securityStatus")
        .or_else(|| kwargs.get("security"))
        .or_else(|| args.get(5));

    let mut matching_agents: Vec<&eve_sde::AgentInfo> = ctx
        .sde
        .agents
        .values()
        .filter(|a| {
            if filter_level.is_some_and(|lvl| a.level != lvl)
                || filter_division.is_some_and(|div| a.division_id != div)
                || filter_corp.is_some_and(|cid| a.corporation_id != cid)
                || filter_agent_type.is_some_and(|tid| a.agent_type_id != tid)
            {
                return false;
            }
            if let Some(fid) = filter_faction {
                if ctx
                    .sde
                    .npc_corporations
                    .get(&a.corporation_id)
                    .and_then(|c| c.faction_id)
                    .map(|f| f as i64)
                    != Some(fid)
                {
                    return false;
                }
            }
            if let Some(sec_rep) = filter_sec {
                let sys_sec = a
                    .solar_system_id
                    .and_then(|sid| ctx.sde.solar_systems.get(&sid))
                    .map(|s| s.security_status)
                    .unwrap_or(1.0);
                let matches_sec = match sec_rep {
                    PyRep::String(s) => match s.to_lowercase().as_str() {
                        "highsec" | "high" => sys_sec >= 0.45,
                        "lowsec" | "low" => (0.05..0.45).contains(&sys_sec),
                        "nullsec" | "null" => sys_sec < 0.05,
                        _ => s.parse::<f64>().map(|v| sys_sec >= v - 0.05).unwrap_or(true),
                    },
                    PyRep::Real(v) => sys_sec >= *v - 0.05,
                    PyRep::Integer(v) => sys_sec >= (*v as f64) - 0.05,
                    _ => true,
                };
                if !matches_sec {
                    return false;
                }
            }
            true
        })
        .collect();
    matching_agents.sort_by_key(|a| a.agent_id);

    let rows = ctx.db.with_snapshot(|snap| {
        matching_agents
            .into_iter()
            .map(|a| {
                let corp_faction = ctx
                    .sde
                    .npc_corporations
                    .get(&a.corporation_id)
                    .and_then(|c| c.faction_id)
                    .map(|f| f as i64);
                let has_access = if let Some(cid) = caller_char {
                    let eff = compute_effective_agent_standing(
                        ctx,
                        snap,
                        cid,
                        a.agent_id,
                        a.corporation_id,
                        corp_faction,
                    );
                    let req = if a.level <= 1 {
                        -2.0 + 1e-6
                    } else {
                        ((a.level as f64) - 1.0) * 2.0 - 1.0
                    };
                    eff >= req
                } else {
                    a.level <= 1
                };
                vec![
                    PyRep::Integer(a.agent_id),
                    PyRep::Integer(a.agent_type_id as i64),
                    PyRep::Integer(a.division_id as i64),
                    PyRep::Integer(a.corporation_id),
                    corp_faction.map(PyRep::Integer).unwrap_or(PyRep::None),
                    a.solar_system_id
                        .map(|s| PyRep::Integer(s as i64))
                        .unwrap_or(PyRep::None),
                    a.station_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                    PyRep::Integer(a.level as i64),
                    PyRep::Integer(20),
                    PyRep::Bool(has_access),
                    PyRep::Bool(a.is_locator),
                ]
            })
            .collect::<Vec<_>>()
    });

    // Column names follow the client: `Agents.__GetAllAgents` indexes the rowset with
    // `Filter('stationID')`, `Filter('solarsystemID')`, `Filter('corporationID')`,
    // `Filter('factionID')` and `Filter('agentTypeID')`
    // (`eve/client/script/ui/station/agents/agents.pyj:110-117`), and Agency
    // `agentContentPiece.py:109` / `contentProviderLocatorAgents.py:10` read
    // `agent.isLocatorAgent`.
    Ok(PyRep::rowset(
        vec![
            "agentID",
            "agentTypeID",
            "divisionID",
            "corporationID",
            "factionID",
            "solarsystemID",
            "stationID",
            "level",
            "quality",
            "hasAccess",
            "isLocatorAgent",
        ],
        rows,
    ))
}
