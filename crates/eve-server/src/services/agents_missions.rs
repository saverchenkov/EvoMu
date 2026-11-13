//! `agents_missions` — Domain 9: Agent Dialogue, Missions, 16-Mission Storyline Tally, Epic Arcs, & AIR NPE (`§3.8`).

// parity: plan:E-5
// parity: feat:pve.agent-finder, feat:pve.agent-conversation, feat:pve.mission-accept
// parity: feat:pve.mission-complete, feat:pve.mission-decline, feat:pve.mission-quit
// parity: feat:pve.mission-expire, feat:pve.mission-journal, feat:pve.mission-tracker
// parity: feat:pve.mission-standing-gate, feat:pve.mission-faction-standing-loss
// parity: feat:pve.mission-lp-isk-reward-formula, feat:pve.mission-bonus-timer
// parity: feat:pve.mission-offer-expiry-no-penalty, feat:pve.mission-level-clone-and-space-restrictions
// parity: feat:pve.research-agent-daily-mission

#![forbid(unsafe_code)]

#[path = "agents_standings_arcs.rs"]
pub mod agents_standings_arcs;
pub use agents_standings_arcs::*;

#[path = "agents_missions_helpers.rs"]
pub mod agents_missions_helpers;
pub use agents_missions_helpers::*;

#[path = "agents_finder_query.rs"]
mod agents_finder_query;

use crate::router::{NotificationTarget, ServerContext};
use crate::services::notify_emitters_character_services::emit_agent_mission_dialogue;
use eve_db::{
    ActiveMissionRecord, InventoryFlag, InventoryItemRecord, IskCents, StandingTransactionRecord,
    WalletJournalEntry,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;
use std::collections::HashMap;

pub async fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    handle_with_bound(ctx, session, None, service, method, args, _kwargs).await
}

async fn is_agent_colocated(
    ctx: &ServerContext,
    session: &SessionState,
    sys_id: u32,
    agent_info: &eve_sde::AgentInfo,
) -> bool {
    if agent_info.solar_system_id != Some(sys_id) {
        return false;
    }
    if let Some(ag_st) = agent_info.station_id {
        return session.station_id == Some(ag_st);
    }
    // In-space agent (`station_id == None`): pilot must be undocked in the same solar system
    // and within 2,500 m of the agent ball when the agent is spawned in space.
    if session.station_id.is_some() || session.structure_id.is_some() {
        return false;
    }
    let handle = ctx.systems.get_or_spawn(sys_id);
    if let Ok(Some((_, ag_ball))) = handle.get_ball_snapshot(agent_info.agent_id).await {
        let Some(ship_id) = session.ship_id else {
            return false;
        };
        let Ok(Some((_, ship_ball))) = handle.get_ball_snapshot(ship_id).await else {
            return false;
        };
        if ship_ball.position.distance(ag_ball.position) > 2_500.0 {
            return false;
        }
    }
    true
}

fn deterministic_mission_site_origin(
    ctx: &ServerContext,
    mission_sys_id: u32,
    mission_id: u32,
) -> DVec3 {
    let base = ctx
        .sde
        .solar_systems
        .get(&mission_sys_id)
        .and_then(|s| s.stations.first().map(|st| st.position))
        .unwrap_or(DVec3::ZERO);
    let offset = DVec3::new(
        50_000.0 + ((mission_id % 97) as f64) * 1_000.0,
        ((mission_id % 13) as f64) * 200.0,
        0.0,
    );
    base + offset
}

fn epic_arc_required_missions(ctx: &ServerContext, fac_id: i64) -> usize {
    ctx.sde
        .epic_arcs
        .values()
        .find(|ea| ea.faction_id == Some(fac_id as u32))
        .and_then(|ea| (ea.missions_count > 0).then_some(ea.missions_count as usize))
        .unwrap_or(3)
        .max(2)
}

pub async fn handle_with_bound(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound_entry: Option<&crate::router::BoundObjectEntry>,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if let Some(res) = agents_standings_arcs::handle_extended_with_bound(
        ctx,
        session,
        bound_entry,
        service,
        method,
        args,
        _kwargs,
    )
    .await
    {
        return Some(res);
    }

    match (service, method) {
        ("agentMgr", "GetAgents") => {
            Some(agents_finder_query::handle_get_agents(ctx, session, args, _kwargs))
        }

        ("agentMgr", "GetInfoServiceDetails") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError { code: "NoSolarSystem".into(), message: "Session has no active solar system".into(), client_error: None }));
            };
            let (agent_id, _) =
                resolve_target_agent_id_with_bound(ctx, session, bound_entry, char_id, sys_id, args, _kwargs);
            let Some(a) = ctx.sde.agents.get(&agent_id) else {
                return Some(Err(BridgeRpcError { code: "UnknownAgent".into(), message: format!("Agent {agent_id} does not exist in SDE"), client_error: None }));
            };
            let mut services = vec![PyRep::keyval([("agentServiceType", PyRep::String("mission".into()))])];
            if a.is_locator {
                services.push(PyRep::keyval([("agentServiceType", PyRep::String("locator".into()))]));
            }
            if a.agent_type_id == 4 {
                services.push(PyRep::keyval([("agentServiceType", PyRep::String("research".into()))]));
            }
            Some(Ok(PyRep::keyval([
                ("agentID", PyRep::Integer(a.agent_id)),
                ("agentTypeID", PyRep::Integer(a.agent_type_id as i64)),
                ("divisionID", PyRep::Integer(a.division_id as i64)),
                ("corporationID", PyRep::Integer(a.corporation_id)),
                ("solarSystemID", a.solar_system_id.map(|s| PyRep::Integer(s as i64)).unwrap_or(PyRep::None)),
                ("stationID", a.station_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
                ("level", PyRep::Integer(a.level as i64)),
                ("quality", PyRep::Integer(20)),
                ("isLocator", PyRep::Bool(a.is_locator)),
                ("incompatible", PyRep::None),
                ("services", PyRep::List(services)),
            ])))
        }

        ("agentMgr", "GetAgentLocationWrap") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError { code: "NoSolarSystem".into(), message: "Session has no active solar system".into(), client_error: None }));
            };
            let (agent_id, _) =
                resolve_target_agent_id_with_bound(ctx, session, bound_entry, char_id, sys_id, args, _kwargs);
            let Some(a) = ctx.sde.agents.get(&agent_id) else {
                return Some(Err(BridgeRpcError { code: "UnknownAgent".into(), message: format!("Agent {agent_id} does not exist in SDE"), client_error: None }));
            };
            let loc_id = a
                .station_id
                .or_else(|| a.solar_system_id.map(|s| s as i64))
                .unwrap_or(0);
            let type_id = a
                .station_id
                .and_then(|sid| ctx.sde.stations.get(&sid))
                .map(|st| st.station_type_id as i64)
                .unwrap_or(5);
            Some(Ok(PyRep::Dict(vec![
                (PyRep::String("agentID".into()), PyRep::Integer(a.agent_id)),
                (
                    PyRep::String("solarsystemID".into()),
                    a.solar_system_id.map(|s| PyRep::Integer(s as i64)).unwrap_or(PyRep::None),
                ),
                (PyRep::String("locationID".into()), PyRep::Integer(loc_id)),
                (PyRep::String("typeID".into()), PyRep::Integer(type_id)),
                (
                    PyRep::String("stationID".into()),
                    a.station_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                ),
            ])))
        }

        ("agentMgr", "GetDungeonShipRestrictions") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let restricted = ctx.db.with_snapshot(|snap| {
                let _ = snap.characters.get(&char_id);
                Vec::<PyRep>::new()
            });
            Some(Ok(PyRep::Dict(vec![(
                PyRep::String("restrictedShipTypes".into()),
                PyRep::List(restricted),
            )])))
        }

        ("agentMgr", "GetEntryPoint") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(sys_id) = session.solar_system_id.or_else(|| ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.solar_system_id))) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidSolarSystem".into(),
                    message: format!("Character {char_id} has no active solar system"),
                    client_error: None,
                }));
            };
            let bm = ctx.db.with_snapshot(|snap| {
                snap.active_missions
                    .values()
                    .find(|m| m.character_id == char_id)
                    .map(|m| {
                        PyRep::keyval([
                            ("solarSystemID", PyRep::Integer(m.solar_system_id as i64)),
                            ("locationID", PyRep::Integer(m.solar_system_id as i64)),
                            ("bookmarkID", PyRep::Integer(m.bookmark_id.unwrap_or(1))),
                        ])
                    })
                    .unwrap_or_else(|| {
                        PyRep::keyval([
                            ("solarSystemID", PyRep::Integer(sys_id as i64)),
                            ("locationID", PyRep::Integer(sys_id as i64)),
                            ("bookmarkID", PyRep::Integer(1)),
                        ])
                    })
            });
            Some(Ok(bm))
        }

        ("agentMgr", "GetMissionKeywords") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let kw_entries = ctx.db.with_snapshot(|snap| {
                let _ = snap.characters.get(&char_id);
                Vec::<(PyRep, PyRep)>::new()
            });
            Some(Ok(PyRep::Dict(kw_entries)))
        }

        ("agentMgr", "GetStandingGainsForMission") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(sys_id) = session.solar_system_id.or_else(|| ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.solar_system_id))) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidSolarSystem".into(),
                    message: format!("Character {char_id} has no active solar system"),
                    client_error: None,
                }));
            };
            let (agent_id, _) = resolve_target_agent_id_with_bound(ctx, session, bound_entry, char_id, sys_id, args, _kwargs);
            let corp_id = ctx.sde.agents.get(&agent_id).map_or(1_000_035, |a| a.corporation_id);
            Some(Ok(PyRep::Dict(vec![
                (PyRep::Integer(agent_id), PyRep::Real(0.15)),
                (PyRep::Integer(corp_id), PyRep::Real(0.05)),
            ])))
        }

        ("agentMgr", "IsCheatingWithAgent") | ("agentMgr", "ShouldAlwaysAllowReplay") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let is_gm = ctx.db.with_snapshot(|snap| snap.characters.contains_key(&char_id)) && false;
            Some(Ok(PyRep::Bool(is_gm)))
        }

        ("agentMgr", "GotoLocation")
        | ("agentMgr", "WarpToAgentInSpace")
        | ("agentMgr", "WarpToLocation") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let ok = ctx.db.with_snapshot(|snap| snap.characters.contains_key(&char_id));
            Some(Ok(PyRep::Bool(ok)))
        }

        ("agentMgr", "GetSolarSystemOfAgent") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError { code: "NoSolarSystem".into(), message: "Session has no active solar system".into(), client_error: None }));
            };
            let agent_id = bound_entry
                .filter(|b| b.service == "agentMgr" && b.object_id > 0)
                .map(|b| b.object_id)
                .or_else(|| args.first().and_then(|a| a.as_i64()))
                .unwrap_or_else(|| resolve_target_agent_id_with_bound(ctx, session, bound_entry, char_id, sys_id, args, _kwargs).0);
            let Some(a) = ctx.sde.agents.get(&agent_id) else {
                return Some(Err(BridgeRpcError { code: "UnknownAgent".into(), message: format!("Agent {agent_id} does not exist in SDE"), client_error: None }));
            };
            Some(Ok(a.solar_system_id.map(|s| PyRep::Integer(s as i64)).unwrap_or(PyRep::None)))
        }

        ("agentMgr", "GetMyJournalDetails") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let now = ctx.now_unix();
            let (missions, research) = ctx.db.with_mut_snapshot(|snap| {
                // Sweep and transition any expired offers or accepted missions (feat:pve.mission-expiration)
                let char_mids: Vec<u32> = snap.active_missions.values().filter(|m| m.character_id == char_id && matches!(m.state.as_str(), "Offered" | "Accepted")).map(|m| m.mission_id).collect();
                for mid in char_mids {
                    if let Some(meta) = load_mission_meta(snap, char_id, mid) {
                        let st = snap.active_missions.get(&(char_id, mid)).map(|m| m.state.clone()).unwrap_or_default();
                        if st == "Offered" && meta.offer_expiry_unix > 0 && now > meta.offer_expiry_unix
                        {
                            if let Some(m) = snap.active_missions.get_mut(&(char_id, mid)) {
                                m.state = "Expired".into();
                            }
                            reclaim_courier_cargo(snap, char_id, mid);
                        } else if st == "Accepted"
                            && meta.accept_expiry_unix.is_some_and(|exp| exp > 0 && now > exp)
                        {
                            if let Some(m) = snap.active_missions.get_mut(&(char_id, mid)) {
                                m.state = "Failed".into();
                                m.npcs_remaining = 0;
                            }
                            reclaim_courier_cargo(snap, char_id, mid);
                        }
                    }
                }

                let mut active: Vec<ActiveMissionRecord> = snap.active_missions.values().filter(|m| {
                        m.character_id == char_id
                            && matches!(m.state.as_str(), "Offered" | "Accepted")
                    }).cloned()
                    .collect();
                active.sort_by_key(|m| m.mission_id);
                let mission_list = active
                    .into_iter().map(|m| {
                        let tmpl = ctx.sde.missions.get(&m.mission_id);
                        let title = tmpl.map(|t| t.title.clone())
                            .unwrap_or_else(|| format!("Mission {}", m.mission_id));
                        let important = tmpl.is_some_and(is_storyline_template);
                        let mission_state = if m.state == "Accepted" { 2_i64 } else { 1_i64 };
                        let meta = load_mission_meta(snap, char_id, m.mission_id);
                        let exp_unix = meta
                            .as_ref().and_then(|mt| {
                                if m.state == "Accepted" { mt.accept_expiry_unix } else { Some(mt.offer_expiry_unix) }
                            }).unwrap_or(now + 86_400);
                        let ct = tmpl.and_then(|t| t.content_template.as_deref()).unwrap_or_default();
                        let bm_hint = if ct.contains("Trade") {
                            "UI/Agents/MissionTypes/Trade"
                        } else if tmpl.is_some_and(|t| t.is_courier)
                            || ct.contains("Courier")
                            || meta.as_ref().and_then(|mt| mt.destination_station_id).is_some()
                        {
                            "UI/Agents/MissionTypes/Courier"
                        } else if tmpl.is_some_and(|t| t.title.to_lowercase().contains("mining"))
                            || ct.contains("Mining")
                        {
                            "UI/Agents/MissionTypes/Mining"
                        } else {
                            "UI/Agents/MissionTypes/Encounter"
                        };
                        let mut matching_bms: Vec<&eve_db::BookmarkRecord> = snap
                            .bookmarks
                            .values()
                            .filter(|b| {
                                b.owner_id == char_id
                                    && b.solar_system_id == m.solar_system_id
                                    && b.memo.contains("Mission Pocket")
                            })
                            .collect();
                        matching_bms.sort_by_key(|b| b.bookmark_id);
                        let bm_loc_type = if matches!(
                            bm_hint,
                            "UI/Agents/MissionTypes/Courier" | "UI/Agents/MissionTypes/Trade"
                        ) {
                            "objective.destination"
                        } else {
                            "dungeon"
                        };
                        let bms: Vec<PyRep> = matching_bms
                            .into_iter()
                            .enumerate()
                            .map(|(idx, b)| {
                                PyRep::keyval([
                                    ("bookmarkID", PyRep::Integer(b.bookmark_id)),
                                    ("agentID", PyRep::Integer(m.agent_id)),
                                    ("solarSystemID", PyRep::Integer(b.solar_system_id as i64)),
                                    ("locationID", PyRep::Integer(b.solar_system_id as i64)),
                                    ("locationNumber", PyRep::Integer(idx as i64)),
                                    ("locationType", PyRep::String(bm_loc_type.into())),
                                    ("hint", PyRep::String(bm_hint.into())),
                                    ("label", PyRep::String(b.label.clone())),
                                    ("x", PyRep::Real(b.x)),
                                    ("y", PyRep::Real(b.y)),
                                    ("z", PyRep::Real(b.z)),
                                ])
                            })
                            .collect();
                        let offer_lvl = meta
                            .as_ref()
                            .and_then(|mt| mt.offered_level)
                            .or_else(|| ctx.sde.agents.get(&m.agent_id).map(|a| a.level))
                            .unwrap_or(1);
                        PyRep::keyval([
                            ("missionID", PyRep::Integer(m.mission_id as i64)),
                            ("contentID", PyRep::Integer(m.mission_id as i64)),
                            ("agentID", PyRep::Integer(m.agent_id)),
                            ("solarSystemID", PyRep::Integer(m.solar_system_id as i64)),
                            ("level", PyRep::Integer(offer_lvl as i64)),
                            ("state", PyRep::String(m.state)),
                            ("missionState", PyRep::Integer(mission_state)),
                            ("importantMission", PyRep::Bool(important)),
                            ("missionNameID", PyRep::String(title.clone())),
                            ("title", PyRep::String(title)),
                            ("expirationTime", PyRep::Integer(exp_unix as i64)),
                            ("bookmarks", PyRep::List(bms)),
                        ])
                    })
                    .collect::<Vec<PyRep>>();

                let research_list = load_rnd_research_entries(snap, char_id)
                    .into_iter().map(|r| {
                        let elapsed_days =
                            (now.saturating_sub(r.started_unix) as f64) / 86_400.0;
                        let total_rp = r.initial_points + r.points_per_day * elapsed_days;
                        let agent_lvl = ctx.sde.agents.get(&r.agent_id).map_or(1, |a| a.level as i64);
                        PyRep::keyval([
                            ("agentID", PyRep::Integer(r.agent_id)),
                            ("typeID", PyRep::Integer(r.research_type_id as i64)),
                            ("ppd", PyRep::Real(r.points_per_day)),
                            ("points", PyRep::Real(total_rp)),
                            ("level", PyRep::Integer(agent_lvl)),
                            ("quality", PyRep::Integer(20)),
                        ])
                    })
                    .collect::<Vec<PyRep>>();
                (mission_list, research_list)
            });
            Some(Ok(PyRep::Tuple(vec![PyRep::List(missions), PyRep::List(research)])))
        }

        ("agentMgr", "GetMissionBriefingInfo")
        | ("agentMgr", "GetMissionObjectiveInfo")
        | ("agentMgr", "GetMissionJournalInfo") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError { code: "NoSolarSystem".into(), message: "Session has no active solar system".into(), client_error: None }));
            };
            let (agent_id, explicit_agent) =
                resolve_target_agent_id_with_bound(ctx, session, bound_entry, char_id, sys_id, args, _kwargs);
            let lookup_agent_id = if explicit_agent { agent_id } else { 0 };
            let mission_id =
                match resolve_target_mission_id(ctx, char_id, lookup_agent_id, None, args, _kwargs) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some((m, meta)) = ctx.db.with_snapshot(|snap| {
                snap.active_missions.get(&(char_id, mission_id)).cloned().map(|m| (m, load_mission_meta(snap, char_id, mission_id)))
            }) else {
                return Some(Err(BridgeRpcError { code: "MissionNotActive".into(), message: format!("Mission {mission_id} is not active for character {char_id}"), client_error: None }));
            };
            let has_explicit_mission_id = args.get(1).and_then(|a| a.as_i64()).is_some()
                || args.first().and_then(|a| a.as_i64()).is_some_and(|v| {
                    v > 19
                        && !ctx.sde.agents.contains_key(&v)
                        && u32::try_from(v).is_ok_and(|mid| {
                            ctx.sde.missions.contains_key(&mid)
                                || ctx.db.with_snapshot(|snap| {
                                    snap.active_missions.contains_key(&(char_id, mid))
                                })
                        })
                });
            if method != "GetMissionJournalInfo" && !has_explicit_mission_id {
                if (explicit_agent && m.agent_id != agent_id)
                    || !matches!(m.state.as_str(), "Offered" | "Accepted")
                {
                    return Some(Err(BridgeRpcError {
                        code: "MissionNotActive".into(),
                        message: format!(
                            "No active Offered/Accepted mission for character {char_id} with agent {agent_id}"
                        ),
                        client_error: None,
                    }));
                }
                if m.state == "Offered" {
                    if let Some(ref agent_info) = ctx.sde.agents.get(&m.agent_id).cloned() {
                        if !is_agent_colocated(ctx, session, sys_id, agent_info).await {
                            return Some(Err(BridgeRpcError {
                                code: "MissionNotActive".into(),
                                message: format!(
                                    "Offered mission {mission_id} with agent {} is not accessible remotely",
                                    m.agent_id
                                ),
                                client_error: None,
                            }));
                        }
                    }
                }
            }
            let tmpl = ctx.sde.missions.get(&mission_id);
            let title = tmpl.map(|t| t.title.clone())
                .unwrap_or_else(|| format!("Mission {mission_id}"));
            let briefing = format!(
                "Agent {} operation: {}. Execute objectives in system {} and report back.",
                m.agent_id, title, m.solar_system_id
            );
            let req_type = meta
                .as_ref().and_then(|mt| mt.required_delivery_type_id).map(|t| PyRep::Integer(t as i64)).unwrap_or(PyRep::None);
            let req_qty = meta.as_ref().map_or(0_i64, |mt| mt.required_delivery_qty as i64);
            let ct = tmpl.and_then(|t| t.content_template.as_deref()).unwrap_or_default();
            let is_trade = ct.contains("Trade");
            let is_courier = !is_trade && (tmpl.is_some_and(|t| t.is_courier) || ct.contains("Courier") || meta.as_ref().and_then(|mt| mt.destination_station_id).is_some());
            let is_mining = !is_courier && !is_trade && (tmpl.is_some_and(|t| t.title.to_lowercase().contains("mining")) || ct.contains("Mining"));
            let objectives = if is_mining {
                let ore_tid = meta.as_ref().and_then(|mt| mt.required_delivery_type_id).unwrap_or(1230);
                let ore_name = ctx.sde.types.get(&ore_tid).map(|t| t.name.as_str()).unwrap_or("ore");
                let qty = if req_qty > 0 { req_qty } else { 500 };
                format!("Mine {qty} units of {ore_name} and deliver to the agent station hangar.")
            } else if is_courier {
                let cargo_tid = meta.as_ref().and_then(|mt| mt.required_delivery_type_id).unwrap_or(3683);
                let cargo_name = ctx.sde.types.get(&cargo_tid).map(|t| t.name.as_str()).unwrap_or("cargo");
                let dest_name = meta.as_ref().and_then(|mt| mt.destination_station_id)
                    .and_then(|sid| ctx.sde.stations.get(&sid))
                    .map(|s| s.name.as_str())
                    .unwrap_or("destination station");
                let qty = if req_qty > 0 { req_qty } else { 10 };
                format!("Transport {qty} units of {cargo_name} to {dest_name}.")
            } else if is_trade {
                let trade_tid = meta.as_ref().and_then(|mt| mt.required_delivery_type_id).unwrap_or(3683);
                let trade_name = ctx.sde.types.get(&trade_tid).map(|t| t.name.as_str()).unwrap_or("trade goods");
                let qty = if req_qty > 0 { req_qty } else { 10 };
                format!("Acquire and deliver {qty} units of {trade_name} to the agent station hangar.")
            } else {
                format!("Eliminate hostile pirate vessels ({} targets remaining) and report back.", m.npcs_remaining)
            };
            let mission_state = match m.state.as_str() {
                "Offered" => 1_i64,
                "Accepted" => 2_i64,
                "Failed" | "Expired" => 3_i64,
                "Completed" => 4_i64,
                "Declined" => 5_i64,
                "Quit" => 6_i64,
                _ => 0_i64,
            };
            let offer_lvl = meta
                .as_ref()
                .and_then(|mt| mt.offered_level)
                .or_else(|| ctx.sde.agents.get(&m.agent_id).map(|a| a.level))
                .unwrap_or(1);
            let CharacterMissionRewards {
                isk_reward,
                lp_reward,
                bonus_reward,
            } = compute_character_mission_rewards(ctx, char_id, m.agent_id, mission_id, sys_id);
            let origin_sid = meta
                .as_ref()
                .and_then(|mt| mt.origin_station_id)
                .or_else(|| ctx.sde.agents.get(&m.agent_id).and_then(|a| a.station_id))
                .unwrap_or(60_003_760);
            let dest_sid_opt = meta.as_ref().and_then(|mt| mt.destination_station_id);
            let make_loc = |sid: i64, ltype: &str| {
                let lsys = ctx
                    .sde
                    .stations
                    .get(&sid)
                    .map(|st| st.solar_system_id as i64)
                    .unwrap_or(m.solar_system_id as i64);
                let ltid = ctx
                    .sde
                    .stations
                    .get(&sid)
                    .map(|st| st.station_type_id as i64)
                    .unwrap_or(1529);
                PyRep::Dict(vec![
                    (PyRep::String("locationID".into()), PyRep::Integer(sid)),
                    (PyRep::String("solarsystemID".into()), PyRep::Integer(lsys)),
                    (PyRep::String("stationID".into()), PyRep::Integer(sid)),
                    (PyRep::String("typeID".into()), PyRep::Integer(ltid)),
                    (PyRep::String("agentID".into()), PyRep::Integer(m.agent_id)),
                    (PyRep::String("shipTypeID".into()), PyRep::None),
                    (PyRep::String("locationType".into()), PyRep::String(ltype.into())),
                    (PyRep::String("BookmarkID".into()), m.bookmark_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
                ])
            };
            let mut locations = vec![make_loc(origin_sid, "agenthomebase")];
            if let Some(dsid) = dest_sid_opt {
                locations.push(make_loc(dsid, "objective.destination"));
            }
            let completion_status = if mission_state == 4 || (!is_courier && !is_trade && !is_mining && m.npcs_remaining == 0) {
                1_i64
            } else {
                0_i64
            };
            let dungeons = if !is_courier && !is_trade {
                let sde_dungeon_id = ctx
                    .sde
                    .missions
                    .get(&mission_id)
                    .and_then(|mt| mt.dungeon_id)
                    .filter(|did| ctx.sde.dungeons.contains_key(did))
                    .or_else(|| {
                        ctx.sde
                            .dungeons
                            .contains_key(&mission_id)
                            .then_some(mission_id)
                    })
                    .unwrap_or(213) as i64;
                let dungeon_owner_id = u32::try_from(sde_dungeon_id)
                    .ok()
                    .and_then(|did| ctx.sde.dungeons.get(&did))
                    .and_then(|d| d.faction_id)
                    .map(i64::from)
                    .filter(|&v| v > 0)
                    .or_else(|| {
                        ctx.sde
                            .agents
                            .get(&m.agent_id)
                            .map(|a| a.corporation_id)
                            .filter(|&v| v > 0)
                    })
                    .unwrap_or(1_000_002);
                let mut dun_entries = vec![
                    (PyRep::String("dungeonID".into()), PyRep::Integer(sde_dungeon_id)),
                    (PyRep::String("solarsystemID".into()), PyRep::Integer(m.solar_system_id as i64)),
                    (PyRep::String("solarSystemID".into()), PyRep::Integer(m.solar_system_id as i64)),
                    (PyRep::String("locationID".into()), PyRep::Integer(origin_sid)),
                    (PyRep::String("stationID".into()), PyRep::Integer(origin_sid)),
                    (PyRep::String("typeID".into()), PyRep::Integer(1529)),
                    (PyRep::String("location".into()), make_loc(origin_sid, "dungeon")),
                    (PyRep::String("ownerID".into()), PyRep::Integer(dungeon_owner_id)),
                    (PyRep::String("optional".into()), PyRep::Bool(false)),
                    (
                        PyRep::String("objectiveCompleted".into()),
                        PyRep::Bool(completion_status > 0),
                    ),
                    (PyRep::String("shipRestrictions".into()), PyRep::Integer(0)),
                    (PyRep::String("completed".into()), PyRep::Bool(completion_status > 0)),
                ];
                if completion_status > 0 {
                    dun_entries.push((
                        PyRep::String("completionStatus".into()),
                        PyRep::Integer(completion_status),
                    ));
                }
                vec![PyRep::Dict(dun_entries)]
            } else {
                Vec::new()
            };
            Some(Ok(PyRep::keyval([
                ("missionID", PyRep::Integer(mission_id as i64)),
                ("contentID", PyRep::Integer(mission_id as i64)),
                ("agentID", PyRep::Integer(m.agent_id)),
                ("level", PyRep::Integer(offer_lvl as i64)),
                ("title", PyRep::String(title)),
                ("state", PyRep::String(m.state)),
                ("missionState", PyRep::Integer(mission_state)),
                ("completionStatus", PyRep::Integer(completion_status)),
                ("briefing", PyRep::String(briefing)),
                ("objectives", PyRep::String(objectives.into())),
                ("locations", PyRep::List(locations)),
                ("dungeons", PyRep::List(dungeons)),
                ("originStationID", PyRep::Integer(origin_sid)),
                ("destinationStationID", dest_sid_opt.map(PyRep::Integer).unwrap_or(PyRep::None)),
                ("solarsystemID", PyRep::Integer(m.solar_system_id as i64)),
                ("currentRoomID", PyRep::Integer(m.current_room_id as i64)),
                ("npcsRemaining", PyRep::Integer(m.npcs_remaining as i64)),
                ("requiredTypeID", req_type),
                ("requiredQuantity", PyRep::Integer(req_qty)),
                ("iskReward", PyRep::Integer(isk_reward)),
                ("lpReward", PyRep::Integer(lp_reward)),
                ("bonusReward", PyRep::Integer(bonus_reward)),
            ])))
        }

        ("agentMgr", "DoAction") | ("agentMgr", "GetOffer") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError { code: "NoSolarSystem".into(), message: "Session has no active solar system".into(), client_error: None }));
            };
            let explicit_action_opt = if method == "GetOffer" {
                Some(2_i64)
            } else {
                args.first()
                    .and_then(|a| a.as_i64())
                    .filter(|v| (1..=19).contains(v))
            };
            let is_implicit_greeting = explicit_action_opt.is_none();
            let action_id = explicit_action_opt.unwrap_or(2_i64);

            let bound_agent_id = bound_entry
                .filter(|b| b.service == "agentMgr" && b.object_id > 0)
                .map(|b| b.object_id);
            let (mut agent_id, mut explicit_agent) =
                resolve_target_agent_id_with_bound(ctx, session, bound_entry, char_id, sys_id, args, _kwargs);

            if (12..=19).contains(&action_id) {
                if bound_agent_id.is_none() {
                    if let Some(sid) = args.get(1).and_then(|a| a.as_i64()) {
                        if ctx.sde.agents.contains_key(&sid) {
                            agent_id = sid;
                        }
                    }
                }
                return agents_standings_arcs::handle_special_dialogue_action(
                    ctx, session, char_id, sys_id, action_id, agent_id, args, _kwargs,
                );
            }

            match action_id {
                // 1 = ViewMission, 2 = RequestMission, 8 = Continue
                1 | 2 | 8 => {
                    let supplied_id = if args
                        .first().and_then(|a| a.as_i64())
                        .is_some_and(|v| (1..=19).contains(&v))
                    {
                        args.get(1).and_then(|a| a.as_i64())
                    } else {
                        args.first()
                            .and_then(|a| a.as_i64())
                            .or_else(|| args.get(1).and_then(|a| a.as_i64()))
                            .or_else(|| _kwargs.get("agentID").and_then(|a| a.as_i64()))
                    };
                    if let Some(sid) = supplied_id.filter(|_| bound_agent_id.is_none()) {
                        agent_id = sid;
                        explicit_agent = true;
                    }
                    let Some(agent_info) = ctx.sde.agents.get(&agent_id) else {
                        return Some(Err(BridgeRpcError { code: "UnknownAgent".into(), message: format!("Agent {agent_id} does not exist in SDE"), client_error: None }));
                    };
                    if is_implicit_greeting {
                        let existing_accepted = ctx.db.with_snapshot(|snap| {
                            snap.active_missions
                                .values()
                                .find(|m| {
                                    m.character_id == char_id
                                        && m.agent_id == agent_id
                                        && m.state == "Accepted"
                                })
                                .cloned()
                        });
                        if let Some(acc) = existing_accepted {
                            let mid = acc.mission_id;
                            let title = ctx
                                .sde
                                .missions
                                .get(&mid)
                                .map(|t| t.title.clone())
                                .unwrap_or_else(|| format!("Mission {mid}"));
                            let msg = format!(
                                "Your assignment '{title}' is currently in progress. Return here once your objectives are complete."
                            );
                            let mut dlg = vec![
                                PyRep::Tuple(vec![
                                    PyRep::Integer(6),
                                    PyRep::String("Complete Mission".into()),
                                ]),
                                PyRep::Tuple(vec![
                                    PyRep::Integer(11),
                                    PyRep::String("Quit Mission".into()),
                                ]),
                            ];
                            if agent_info.is_locator {
                                dlg.push(PyRep::Tuple(vec![
                                    PyRep::Integer(15),
                                    PyRep::String("Locate Character".into()),
                                ]));
                            }
                            return Some(Ok(PyRep::keyval([
                                ("missionID", PyRep::Integer(mid as i64)),
                                ("contentID", PyRep::Integer(mid as i64)),
                                ("agentID", PyRep::Integer(agent_id)),
                                ("title", PyRep::String(title)),
                                ("state", PyRep::String("Accepted".into())),
                                ("missionState", PyRep::Integer(2)),
                                ("briefing", PyRep::String(msg.clone())),
                                ("agentSays", PyRep::String(msg)),
                                ("dialogue", PyRep::List(dlg)),
                            ])));
                        }
                    }

                    let station_has_local_agents = session.station_id.is_some_and(|sid| {
                        ctx.sde.agents.values().any(|a| {
                            a.solar_system_id == Some(sys_id)
                                && a.station_id == Some(sid)
                                && !is_storyline_agent_type(a.agent_type_id)
                                && !is_unsupported_direct_agent_type(a.agent_type_id)
                        })
                    });
                    if agent_info.solar_system_id != Some(sys_id)
                        || (session.station_id.is_some()
                            && (explicit_agent || station_has_local_agents)
                            && agent_info.station_id.is_some()
                            && agent_info.station_id != session.station_id)
                    {
                        let sys_label = agent_info
                            .solar_system_id
                            .map(|sid| {
                                ctx.sde
                                    .solar_systems
                                    .get(&sid)
                                    .map(|s| s.name.clone())
                                    .unwrap_or_else(|| sid.to_string())
                            })
                            .unwrap_or_else(|| "unknown system".to_string());
                        let station_label = agent_info
                            .station_id
                            .map(|stid| {
                                ctx.sde
                                    .stations
                                    .get(&stid)
                                    .map(|s| s.name.clone())
                                    .unwrap_or_else(|| stid.to_string())
                            })
                            .unwrap_or_else(|| "space".to_string());
                        if is_implicit_greeting {
                            let msg = format!(
                                "Please dock at my station in {sys_label} ({station_label}) if you wish to request a new assignment."
                            );
                            return Some(Ok(PyRep::keyval([
                                ("agentID", PyRep::Integer(agent_id)),
                                ("state", PyRep::String("NotCoLocated".into())),
                                ("missionState", PyRep::Integer(0)),
                                ("briefing", PyRep::String(msg.clone())),
                                ("agentSays", PyRep::String(msg)),
                                ("dialogue", PyRep::List(Vec::new())),
                            ])));
                        }
                        return Some(Err(BridgeRpcError { code: "AgentNotCoLocated".into(), message: format!("Agent {agent_id} is in system {sys_label} / station {station_label}, not character's current location (system {sys_id}, station {:?})", session.station_id), client_error: None }));
                    }
                    if is_storyline_agent_type(agent_info.agent_type_id) {
                        return Some(Err(BridgeRpcError { code: "StorylineAgentRequiresReferral".into(), message: format!("Storyline agent {agent_id} (agentType {}) cannot be requested directly without a 16-mission referral", agent_info.agent_type_id), client_error: None }));
                    }
                    if agent_info.agent_type_id == 11 {
                        return Some(Err(BridgeRpcError { code: "UnimplementedRPC".into(), message: format!("Agent {agent_id} (agentType {}) uses Aura node-graph missions which are not yet implemented", agent_info.agent_type_id), client_error: None }));
                    }
                    let now = ctx.now_unix();
                    if agent_info.agent_type_id == 10 {
                        let (arc_id, _) = resolve_epic_arc_id_for_agent(ctx, agent_id);
                        let on_cd = ctx.db.with_snapshot(|snap| {
                            load_epic_arc_states(snap, char_id).iter().any(|a| {
                                a.epic_arc_id == arc_id
                                    && a.cooldown_until_unix.is_some_and(|until| now < until)
                            })
                        });
                        if on_cd {
                            return Some(Err(BridgeRpcError { code: "EpicArcOnCooldown".into(), message: format!("Epic Arc {arc_id} (agent {agent_id}) is on 90-day repeat cooldown"), client_error: None }));
                        }
                    }
                    if agent_info.agent_type_id == 9 {
                        let req_fac = ctx.sde.factions.values().find(|f| f.militia_corporation_id == Some(agent_info.corporation_id)).map(|f| f.faction_id as i64);
                        let ch_fac = ctx.db.with_snapshot(|snap| snap.characters.get(&char_id).and_then(|c| c.war_faction_id));
                        if req_fac.is_some() && ch_fac != req_fac {
                            return Some(Err(BridgeRpcError { code: "NotEnlistedInMilitia".into(), message: format!("FW agent {agent_id} requires enlistment in militia faction {req_fac:?} (have {ch_fac:?})"), client_error: None }));
                        }
                    }
                    if matches!(agent_info.agent_type_id, 5 | 8 | 12) {
                        let already_done = ctx.db.with_snapshot(|snap| {
                            snap.active_missions.values().any(|m| {
                                m.character_id == char_id
                                    && m.agent_id == agent_id
                                    && m.state == "Completed"
                            })
                        });
                        if already_done {
                            return Some(Err(BridgeRpcError { code: "OneTimeAgentMissionCompleted".into(), message: format!("One-time COSMOS / Data Center / Career agent {agent_id} mission has already been completed"), client_error: None }));
                        }
                    }

                    if matches!(agent_info.agent_type_id, 5 | 8 | 12) {
                        let cosmos_expired = ctx.db.with_snapshot(|snap| {
                            snap.active_missions.values().any(|m| {
                                m.character_id == char_id && m.agent_id == agent_id && m.state == "Expired"
                            })
                        });
                        if cosmos_expired {
                            return Some(Err(BridgeRpcError {
                                code: "CosmosMissionExpired".into(),
                                message: format!("One-time COSMOS agent {agent_id} mission offer has expired and cannot be re-offered"),
                                client_error: None,
                            }));
                        }
                    }

                    let is_alpha = ctx.db.with_snapshot(|snap| {
                        snap.characters.get(&char_id).is_some_and(|c| c.clone_grade <= 1)
                    });
                    if is_alpha && agent_info.level >= 4 {
                        return Some(Err(BridgeRpcError {
                            code: "CantDoMissionAlphaClone".into(),
                            message: format!("Agent level {} requires Omega clone state", agent_info.level),
                            client_error: None,
                        }));
                    }

                    if agent_info.level == 5 {
                        let agent_sys = agent_info.solar_system_id.unwrap_or(sys_id);
                        let sec = ctx.sde.solar_systems.get(&agent_sys).map(|s| s.security_status).unwrap_or(1.0);
                        if sec >= 0.45 {
                            return Some(Err(BridgeRpcError {
                                code: "CantDoMission".into(),
                                message: format!("Level 5 agent {agent_id} cannot operate in high-security space ({sec:.2})"),
                                client_error: None,
                            }));
                        }
                    }

                    let faction_id = ctx.sde.npc_corporations.get(&agent_info.corporation_id).and_then(|c| c.faction_id).map(|fid| fid as i64);
                    let effective_standing = ctx.db.with_snapshot(|snap| {
                        compute_effective_agent_standing(ctx, snap, char_id, agent_id, agent_info.corporation_id, faction_id)
                    });
                    if agent_info.level > 1 {
                        let required_standing = ((agent_info.level as f64) - 1.0) * 2.0 - 1.0;
                        if effective_standing < required_standing {
                            if is_implicit_greeting {
                                let refusal = format!(
                                    "I am sorry, capsuleer, but I only offer assignments to pilots with an effective standing of at least {required_standing:.1} (yours is {effective_standing:.2})."
                                );
                                return Some(Ok(PyRep::keyval([
                                    ("agentID", PyRep::Integer(agent_id)),
                                    ("state", PyRep::String("Refused".into())),
                                    ("missionState", PyRep::Integer(0)),
                                    ("briefing", PyRep::String(refusal.clone())),
                                    ("agentSays", PyRep::String(refusal)),
                                    ("dialogue", PyRep::List(Vec::new())),
                                ])));
                            }
                            return Some(Err(BridgeRpcError { code: "InsufficientAgentStanding".into(), message: format!("Agent {agent_id} (level {}) requires agent/corporation/faction standing >= {required_standing:.1}, character has {effective_standing:.2}", agent_info.level), client_error: None }));
                        }
                    } else if !matches!(agent_info.agent_type_id, 7 | 12) && effective_standing <= -2.0 {
                        if is_implicit_greeting {
                            let refusal = format!(
                                "I do not work with pilots whose standing has fallen so low ({effective_standing:.2}). Improve your standing above -2.0 first."
                            );
                            return Some(Ok(PyRep::keyval([
                                ("agentID", PyRep::Integer(agent_id)),
                                ("state", PyRep::String("Refused".into())),
                                ("missionState", PyRep::Integer(0)),
                                ("briefing", PyRep::String(refusal.clone())),
                                ("agentSays", PyRep::String(refusal)),
                                ("dialogue", PyRep::List(Vec::new())),
                            ])));
                        }
                        return Some(Err(BridgeRpcError { code: "InsufficientAgentStanding".into(), message: format!("Agent {agent_id} (level 1) requires standing > -2.0, character has {effective_standing:.2}"), client_error: None }));
                    }

                    let is_rnd = is_rnd_agent(agent_info);
                    if is_rnd {
                        let has_research = ctx.db.with_snapshot(|snap| {
                            load_rnd_research_entries(snap, char_id).iter().any(|r| r.agent_id == agent_id)
                        });
                        if !has_research {
                            if is_implicit_greeting {
                                let greeting = "Greetings, capsuleer. We do not have an active research project yet, but we can begin one whenever you are ready.".to_string();
                                return Some(Ok(PyRep::keyval([
                                    ("agentID", PyRep::Integer(agent_id)),
                                    ("state", PyRep::String("Greeting".into())),
                                    ("missionState", PyRep::Integer(0)),
                                    ("briefing", PyRep::String(greeting.clone())),
                                    ("agentSays", PyRep::String(greeting)),
                                    (
                                        "dialogue",
                                        PyRep::List(vec![PyRep::Tuple(vec![
                                            PyRep::Integer(12),
                                            PyRep::String("Start Research".into()),
                                        ])]),
                                    ),
                                ])));
                            }
                            return Some(Err(BridgeRpcError {
                                code: "ResearchNotActive".into(),
                                message: format!("Agent {agent_id} requires active research to offer daily missions"),
                                client_error: None,
                            }));
                        }
                    }

                    let offer_dur = 604_800_u64;

                    let res = ctx.db.try_with_mut_snapshot(|snap| {
                        if let Some(existing_active) = snap.active_missions.values().find(|m| {
                            m.character_id == char_id
                                && m.agent_id == agent_id
                                && m.state == "Offered"
                        }) {
                            let mid = existing_active.mission_id;
                            let title = ctx.sde.missions.get(&mid).map(|t| t.title.clone()).unwrap_or_default();
                            let meta = load_mission_meta(snap, char_id, mid);
                            let dest_sid = meta.as_ref().and_then(|m| m.destination_station_id);
                            let briefing = format!("Agent {} assignment: {}.", agent_id, title);
                            let objectives = format!("Execute mission objectives for {} and report back.", title);
                            return Ok((mid, title, briefing, objectives, dest_sid));
                        }
                        let is_heraldry_agent = agent_info.agent_type_id == 13 || agent_info.division_id == 37;
                        let next_tmpl = if is_heraldry_agent && explicit_agent {
                            ctx.sde.missions
                                .values().filter(|m| {
                                     m.content_template.as_deref().unwrap_or_default().contains("Heraldry")
                                        && !snap.active_missions.contains_key(&(char_id, m.mission_id))
                                }).min_by_key(|m| m.mission_id)
                        } else {
                            ctx.sde.missions
                                .values().filter(|m| {
                                    mission_matches_agent_pool(m, agent_info)
                                        && m.has_standing_rewards
                                        && !snap.active_missions.contains_key(&(char_id, m.mission_id))
                                }).min_by_key(|m| m.mission_id).or_else(|| {
                                    ctx.sde.missions
                                        .values().filter(|m| {
                                            mission_matches_agent_pool(m, agent_info)
                                                && !snap.active_missions.contains_key(&(char_id, m.mission_id))
                                        }).min_by_key(|m| m.mission_id)
                                })
                        };
                        let Some(tmpl) = next_tmpl else {
                            return Err(BridgeRpcError { code: "NoMissionsAvailable".into(), message: format!("No unoffered missions remaining in SDE pool for agent {agent_id} and character {char_id}"), client_error: None });
                        };
                        let mission_id = tmpl.mission_id;
                        let title = tmpl.title.clone();
                        let ct = tmpl.content_template.as_deref().unwrap_or_default();
                        let is_trade = ct.contains("BasicTrade") || ct.contains("TradeMission") || ct.contains("Trade");
                        let is_courier = !is_trade && (tmpl.is_courier || ct.contains("BasicCourier"));
                        let is_data_center = agent_info.agent_type_id == 5;
                        let is_heraldry = ct.contains("Heraldry");
                        let is_kill = ct.contains("BasicKill") || ct.contains("Kill") || ct.contains("Encounter");
                        let is_mining = !is_kill && !is_trade && !is_courier && (ct.contains("BasicMining")
                            || tmpl.title.to_lowercase().contains("mining")
                            || agent_info.division_id == 23);

                        let (req_tid, req_qty, origin_sid, dest_sid, npcs_remaining, objectives, briefing) = if is_rnd {
                            let qty = rnd_tritanium_required_qty(agent_info.level);
                            (Some(34), qty, agent_info.station_id, None, 0, format!("Deliver {qty} units of Tritanium to agent station hangar."), format!("R&D Agent {agent_id} assignment: supply {qty} units of Tritanium for research."))
                        } else if is_data_center {
                            (Some(17202), 1, agent_info.station_id, None, 0, "Deliver 1 Pirate Tag to agent station hangar.".to_string(), format!("Data Center Agent {agent_id} assignment: deliver 1 Pirate Tag."))
                        } else if is_courier {
                            let cd = resolve_courier_mission_details(ctx, tmpl, agent_info)?;
                            let cargo_name = ctx.sde.types.get(&cd.cargo_type_id).map(|t| t.name.as_str()).unwrap_or("cargo");
                            let dest_name = ctx.sde.stations.get(&cd.destination_station_id).map(|s| s.name.as_str()).unwrap_or("destination station");
                            (Some(cd.cargo_type_id), cd.cargo_quantity, Some(cd.origin_station_id), Some(cd.destination_station_id), 0, format!("Transport {} units of {} to {}.", cd.cargo_quantity, cargo_name, dest_name), format!("Agent {} operation: Transport {} units of {} to {} and complete delivery.", agent_id, cd.cargo_quantity, cargo_name, dest_name))
                        } else if is_trade {
                            let td = resolve_trade_mission_details(ctx, tmpl);
                            let trade_name = ctx.sde.types.get(&td.trade_type_id).map(|t| t.name.as_str()).unwrap_or("goods");
                            (Some(td.trade_type_id), td.trade_quantity, agent_info.station_id, None, 0, format!("Acquire and deliver {} units of {} to the agent station hangar.", td.trade_quantity, trade_name), format!("Agent {} operation: Deliver {} units of {} to the agent station hangar.", agent_id, td.trade_quantity, trade_name))
                        } else if is_mining {
                            let md = resolve_mining_mission_details(ctx, tmpl);
                            (Some(md.ore_type_id), md.required_units, agent_info.station_id, None, 0, md.objective_text, format!("Agent {} operation: {}. Execute mining assignment and deliver ore to station hangar.", agent_id, tmpl.title))
                        } else if is_heraldry {
                            (None, 0, agent_info.station_id, None, 0, "Complete heraldry assignment and report back.".to_string(), format!("Agent {} operation: {}. Complete heraldry assignment.", agent_id, tmpl.title))
                        } else {
                            let enc = resolve_security_mission_encounter(ctx, tmpl, agent_info, sys_id);
                            (None, 0, agent_info.station_id, None, enc.wave_npc_count, enc.objective_text, enc.briefing_text)
                        };

                        let is_burner = title.contains("Anomic")
                            || (agent_info.level >= 4 && agent_info.division_id == 24);
                        save_mission_meta(
                            snap,
                            char_id,
                            &MissionTimingAndCargoMeta { mission_id, agent_id, offered_at_unix: now, offer_expiry_unix: now + offer_dur, accepted_at_unix: None, accept_expiry_unix: None, bonus_expiry_unix: None, completed_at_unix: None, required_delivery_type_id: req_tid, required_delivery_qty: req_qty, granted_courier_item_id: None, granted_courier_qty: 0, is_anomic_burner: is_burner, chosen_branch: None, destination_station_id: dest_sid, origin_station_id: origin_sid, offered_level: Some(agent_info.level), },
                        );
                        let target_sys = if is_fw_agent(agent_info) { resolve_fw_warzone_system(agent_info.corporation_id) } else { sys_id };
                        snap.active_missions.insert((char_id, mission_id), ActiveMissionRecord { mission_id, character_id: char_id, agent_id, solar_system_id: target_sys, state: "Offered".into(), current_room_id: 1, npcs_remaining, spawned_npc_ball_ids: Vec::new(), bookmark_id: None });
                        Ok((mission_id, title, briefing, objectives, dest_sid))
                    });

                    match res {
                        Ok((mission_id, title, briefing, objectives, dest_sid)) => {
                            let expiry_unix = now + offer_dur;
                            let dialogue = PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(3), PyRep::String("Accept".into())]), PyRep::Tuple(vec![PyRep::Integer(9), PyRep::String("Decline".into())])]);
                            let CharacterMissionRewards {
                                isk_reward,
                                lp_reward,
                                bonus_reward,
                            } = compute_character_mission_rewards(ctx, char_id, agent_id, mission_id, sys_id);
                            ctx.notify(NotificationTarget::Character(char_id), "OnAgentMissionChange", "charid", vec![PyRep::String("offered".into()), PyRep::Integer(agent_id)]);
                            let clean_header = title.trim_start_matches("[d]").trim();
                            let header_str = if clean_header.is_empty() { "Agent Transmission" } else { clean_header };
                            let trans_kv = PyRep::keyval([("header", PyRep::String(header_str.to_string())), ("text", PyRep::String(briefing.clone())), ("icon", PyRep::String("res:/UI/Texture/WindowIcons/agent.png".into())), ("agentID", PyRep::Integer(agent_id))]);
                            ctx.notify(NotificationTarget::Character(char_id), "OnIncomingTransmission", "charid", vec![trans_kv, PyRep::Bool(true)]);
                            let mut offer_kvs = vec![
                                ("missionID", PyRep::Integer(mission_id as i64)),
                                ("contentID", PyRep::Integer(mission_id as i64)),
                                ("agentID", PyRep::Integer(agent_id)),
                                ("title", PyRep::String(title)),
                                ("state", PyRep::String("Offered".into())),
                                ("missionState", PyRep::Integer(1)),
                                ("briefing", PyRep::String(briefing.clone())),
                                ("agentSays", PyRep::String(briefing)),
                                ("objectives", PyRep::String(objectives)),
                                ("dialogue", dialogue),
                                ("iskReward", PyRep::Integer(isk_reward)),
                                ("lpReward", PyRep::Integer(lp_reward)),
                                ("bonusReward", PyRep::Integer(bonus_reward)),
                                ("expiryUnix", PyRep::Integer(expiry_unix as i64)),
                            ];
                            if let Some(dsid) = dest_sid {
                                offer_kvs.push(("destinationStationID", PyRep::Integer(dsid)));
                            }
                            Some(Ok(PyRep::keyval(offer_kvs)))
                        }
                        Err(e) => Some(Err(e)),
                    }
                }

                // 3 = Accept, 4 = AcceptChoice, 5 = AcceptRemotely
                3..=5 => {
                    let lookup_agent_id = if explicit_agent { agent_id } else { 0 };
                    let mission_id = match resolve_target_mission_id(ctx, char_id, lookup_agent_id, Some("Offered"), args, _kwargs) { Ok(v) => v, Err(e) => return Some(Err(e)) };
                    let (is_alpha, agent_lvl, actual_agent_id) = ctx.db.with_snapshot(|snap| {
                        let alpha = snap.characters.get(&char_id).is_some_and(|c| c.clone_grade <= 1);
                        let mid_agent = snap.active_missions.get(&(char_id, mission_id)).map(|m| m.agent_id).unwrap_or(agent_id);
                        let lvl = ctx.sde.agents.get(&mid_agent).map_or(1, |a| a.level);
                        (alpha, lvl, mid_agent)
                    });
                    if is_alpha && agent_lvl >= 4 {
                        return Some(Err(BridgeRpcError {
                            code: "CantDoMissionAlphaClone".into(),
                            message: format!("Agent level {agent_lvl} requires Omega clone state"),
                            client_error: None,
                        }));
                    }
                    let tmpl_opt = ctx.sde.missions.get(&mission_id).cloned();
                    let agent_info_opt = ctx.sde.agents.get(&actual_agent_id).cloned();
                    if let Some(ref agent_info) = agent_info_opt {
                        let colocated = is_agent_colocated(ctx, session, sys_id, agent_info).await;
                        if matches!(action_id, 3 | 4) && !colocated {
                            return Some(Err(BridgeRpcError {
                                code: "AgentNotCoLocated".into(),
                                message: format!(
                                    "Agent {actual_agent_id} is in system {:?} / station {:?}, not character's current location (system {sys_id}, station {:?})",
                                    agent_info.solar_system_id, agent_info.station_id, session.station_id
                                ),
                                client_error: None,
                            }));
                        }
                        if action_id == 5 && !colocated {
                            let is_burner = ctx.db.with_snapshot(|snap| {
                                load_mission_meta(snap, char_id, mission_id)
                                    .is_some_and(|mt| mt.is_anomic_burner)
                            });
                            let remote_eligible = is_storyline_agent_type(agent_info.agent_type_id)
                                || tmpl_opt.as_ref().is_some_and(is_storyline_template)
                                || matches!(agent_info.agent_type_id, 5 | 8)
                                || is_burner;
                            if !remote_eligible {
                                return Some(Err(BridgeRpcError {
                                    code: "CannotAcceptRemotely".into(),
                                    message: format!(
                                        "Mission {mission_id} from agent {actual_agent_id} requires visiting the agent to accept"
                                    ),
                                    client_error: None,
                                }));
                            }
                        }
                    }
                    let mission_sys_id = ctx
                        .db
                        .with_snapshot(|snap| {
                            snap.active_missions
                                .get(&(char_id, mission_id))
                                .map(|m| m.solar_system_id)
                                .filter(|&s| s > 0)
                        })
                        .or_else(|| {
                            agent_info_opt
                                .as_ref()
                                .and_then(|a| a.solar_system_id)
                                .filter(|&s| s > 0)
                        })
                        .unwrap_or(sys_id);
                    let enc_opt = tmpl_opt.as_ref().and_then(|t| {
                        agent_info_opt
                            .as_ref()
                            .map(|a| resolve_security_mission_encounter(ctx, t, a, mission_sys_id))
                    });
                    let handle = ctx.systems.get_or_spawn(mission_sys_id);
                    let existing_accepted = ctx.db.with_snapshot(|snap| {
                        snap.active_missions
                            .get(&(char_id, mission_id))
                            .filter(|m| m.state == "Accepted" && m.npcs_remaining > 0)
                            .map(|m| (m.solar_system_id, m.spawned_npc_ball_ids.clone()))
                    });
                    let can_respawn_missing = if let Some((m_sys_id, prev_balls)) = existing_accepted {
                        if m_sys_id == mission_sys_id && !prev_balls.is_empty() {
                            let m_handle = ctx.systems.get_or_spawn(m_sys_id);
                            let mut all_missing = true;
                            for bid in prev_balls {
                                if m_handle.get_ball_snapshot(bid).await.ok().flatten().is_some() {
                                    all_missing = false;
                                    break;
                                }
                            }
                            all_missing
                        } else {
                            false
                        }
                    } else {
                        false
                    };

                    let now = ctx.now_unix();
                    let accept_dur = 604_800_u64;
                    let bonus_dur = mission_bonus_duration(agent_lvl);

                    let m_info = ctx.db.with_snapshot(|snap| {
                        let m = snap.active_missions.get(&(char_id, mission_id))?;
                        let meta = load_mission_meta(snap, char_id, mission_id)?;
                        let is_expired = meta.offer_expiry_unix > 0 && now > meta.offer_expiry_unix;
                        Some((m.state.clone(), m.solar_system_id, is_expired))
                    });
                    if let Some((state, sys, is_expired)) = m_info {
                        if state == "Offered" {
                            if is_expired {
                                ctx.db.with_mut_snapshot(|snap| {
                                    if let Some(m) = snap.active_missions.get_mut(&(char_id, mission_id)) {
                                        m.state = "Expired".into();
                                    }
                                    reclaim_courier_cargo(snap, char_id, mission_id);
                                });
                                return Some(Err(BridgeRpcError { code: "MissionOfferExpired".into(), message: format!("Mission {mission_id} offer has expired"), client_error: None }));
                            }
                        } else if !(state == "Accepted" && sys == mission_sys_id && can_respawn_missing) {
                            return Some(Err(BridgeRpcError { code: "InvalidMissionState".into(), message: format!("Mission {mission_id} is in state '{state}' (system {sys}), not Offered"), client_error: None }));
                        }
                    }

                    let res = ctx.db.try_with_mut_snapshot(|snap| {
                        let Some(m_check) = snap.active_missions.get(&(char_id, mission_id)) else {
                            return Err(BridgeRpcError { code: "MissionNotOffered".into(), message: format!("Mission {mission_id} has not been offered to character {char_id}"), client_error: None });
                        };
                        if m_check.state != "Offered" && !(m_check.state == "Accepted" && m_check.solar_system_id == mission_sys_id && can_respawn_missing) {
                            return Err(BridgeRpcError { code: "InvalidMissionState".into(), message: format!("Mission {mission_id} is in state '{}' (system {}), not Offered", m_check.state, m_check.solar_system_id), client_error: None });
                        }
                        let mut spawned_npcs: Vec<(i64, eve_db::TypeId)> = Vec::new();
                        let mut need_npc = false;
                        if let Some(m) = snap.active_missions.get_mut(&(char_id, mission_id)) {
                            m.state = "Accepted".into();
                            if m.solar_system_id == 0 {
                                m.solar_system_id = mission_sys_id;
                            }
                            need_npc = m.npcs_remaining > 0;
                        }
                        if need_npc {
                            let count = enc_opt.as_ref().map(|e| e.wave_npc_count).unwrap_or(1) as usize;
                            let pool = enc_opt.as_ref().map(|e| e.npc_type_ids.as_slice()).unwrap_or(&[]);
                            let mut ball_ids = Vec::with_capacity(count);
                            for i in 0..count {
                                let npc_id = snap.alloc_entity_id();
                                let tid = if !pool.is_empty() { pool[i % pool.len()] } else { resolve_mission_npc_type(ctx) };
                                ball_ids.push(npc_id);
                                spawned_npcs.push((npc_id, tid));
                            }
                            if let Some(m) = snap.active_missions.get_mut(&(char_id, mission_id)) {
                                m.npcs_remaining = count as u32;
                                m.spawned_npc_ball_ids = ball_ids;
                            }
                            let wave_tids: Vec<u32> = spawned_npcs.iter().map(|&(_, tid)| tid).collect();
                            snap.sp6_lifecycle.mission_initial_npc_counts.insert(format!("{char_id}:{mission_id}"), count as u32);
                            snap.sp6_lifecycle.mission_initial_npc_types.insert(format!("{char_id}:{mission_id}"), wave_tids);
                        }
                        let mut granted_courier_item: Option<InventoryItemRecord> = None;
                        if let Some(mut meta) = load_mission_meta(snap, char_id, mission_id) {
                            meta.accepted_at_unix = Some(now);
                            meta.accept_expiry_unix = Some(now + accept_dur);
                            meta.bonus_expiry_unix = Some(now + bonus_dur);
                            let is_trade = tmpl_opt.as_ref().is_some_and(|t| t.content_template.as_deref().unwrap_or_default().contains("Trade"));
                            let is_courier = !is_trade && tmpl_opt.as_ref().is_some_and(|t| t.is_courier || t.content_template.as_deref().unwrap_or_default().contains("BasicCourier"));
                            if is_courier && meta.granted_courier_item_id.is_none() {
                                let cargo_tid = meta.required_delivery_type_id.ok_or_else(|| BridgeRpcError { code: "InvalidMissionLocation".into(), message: format!("Courier mission {mission_id} has no delivery item type specified"), client_error: None })?;
                                let cargo_qty = meta.required_delivery_qty.max(1);
                                let agent_station = meta.origin_station_id
                                    .or_else(|| ctx.sde.agents.get(&meta.agent_id).and_then(|a| a.station_id))
                                    .or_else(|| ctx.sde.agents.get(&meta.agent_id).and_then(|a| a.solar_system_id).and_then(|sys| ctx.sde.stations.values().filter(|st| st.solar_system_id == sys).map(|st| st.station_id).min()))
                                    .ok_or_else(|| BridgeRpcError { code: "InvalidMissionLocation".into(), message: "Agent has no valid station location".into(), client_error: None })?;
                                snap.next_entity_id += 1;
                                let cid = snap.next_entity_id;
                                let item = InventoryItemRecord { item_id: cid, type_id: cargo_tid, owner_id: char_id, location_id: agent_station, flag: InventoryFlag::Hangar, quantity: cargo_qty, singleton: false, version: 1, ..Default::default() };
                                snap.items.insert(cid, item.clone());
                                snap.item_custom_names.insert(cid, format!("courier_cargo:{char_id}:{mission_id}"));
                                meta.granted_courier_item_id = Some(cid);
                                meta.granted_courier_qty = cargo_qty;
                                granted_courier_item = Some(item);
                            }
                            save_mission_meta(snap, char_id, &meta);
                        }
                        Ok((spawned_npcs, granted_courier_item))
                    });
                    let (spawned_npcs, granted_courier_item) = match res { Ok(v) => v, Err(e) => return Some(Err(e)) };
                    if let Some(ref courier_item) = granted_courier_item {
                        ctx.notify_item_mutation(char_id, None, Some(courier_item));
                    }
                    let first_npc_id = spawned_npcs.first().map(|(id, _)| *id);

                    let site_origin = deterministic_mission_site_origin(ctx, mission_sys_id, mission_id);
                    if !spawned_npcs.is_empty() {
                        for (idx, (npc_id, tid)) in spawned_npcs.into_iter().enumerate() {
                            let pocket_offset = ((npc_id % 40) as f64) * 35.0 + (idx as f64) * 350.0;
                            let pos = site_origin + DVec3::new(800.0 + pocket_offset, (idx as f64) * 120.0, 0.0);
                            let (ball, cs, mut entity) = crate::services::wormhole_pve_seeding::build_specialized_npc_components(ctx, mission_sys_id, npc_id, tid, "Pirate", 1, pos);
                            entity.bounty_isk = entity.bounty_isk.max(10_000.0);
                            let _ = handle.spawn_ball_with_combat(ball, cs).await;
                            crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
                                rt.npc_entities.insert(npc_id, entity);
                            });
                        }
                    }

                    let is_rnd = ctx.db.with_snapshot(|snap| load_mission_meta(snap, char_id, mission_id).and_then(|m| ctx.sde.agents.get(&m.agent_id)).is_some_and(is_rnd_agent));
                    let is_mining_mission = !is_rnd && ctx.sde.missions.get(&mission_id).is_some_and(|m| {
                        let c = m.content_template.as_deref().unwrap_or_default();
                        if c.contains("Kill") || c.contains("Encounter") || c.contains("Trade") || m.is_courier || c.contains("Courier") {
                            return false;
                        }
                        let agent_div = ctx.db.with_snapshot(|snap| load_mission_meta(snap, char_id, mission_id).and_then(|meta| ctx.sde.agents.get(&meta.agent_id)).map(|a| a.division_id)).unwrap_or(0);
                        let t = m.title.to_lowercase();
                        let c_lower = c.to_lowercase();
                        agent_div == 23 || c_lower.contains("mining") || t.contains("mining") || t.contains("mineral") || t.contains("ore")
                    });
                    if is_mining_mission {
                        let ore_tid = ctx.db.with_snapshot(|snap| load_mission_meta(snap, char_id, mission_id).and_then(|m| m.required_delivery_type_id).unwrap_or(1230));
                        let ore_vol = ctx.sde.types.get(&ore_tid).map(|t| t.volume.max(0.01)).unwrap_or(0.1);
                        let ast_id = 910_000_000 + (mission_id as i64) * 1000 + 1;
                        let ast_pos = site_origin + DVec3::new(8_000.0, 1_000.0, -5_000.0);
                        ctx.db.with_mut_snapshot(|snap| {
                            snap.asteroids.insert(ast_id, eve_db::AsteroidRecord { asteroid_id: ast_id, solar_system_id: mission_sys_id, belt_id: 0, type_id: ore_tid, quantity: 5_000, initial_quantity: 5_000, position: (ast_pos.x, ast_pos.y, ast_pos.z), radius: 50.0 });
                        });
                        let ast_ball = Ball { id: ast_id, type_id: ore_tid, position: ast_pos, velocity: DVec3::ZERO, max_velocity: 0.0, inertia: 1.0, mass: 50_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop };
                        let ast_state = crate::system_actor::AsteroidState { asteroid_id: ast_id, belt_id: 0, type_id: ore_tid, initial_units: 5_000, remaining_units: 5_000, unit_volume: ore_vol, depleted: false, respawn_at_tick: None };
                        let _ = handle.spawn_asteroid(ast_state, ast_ball).await;
                    }

                    let expiry_unix = now + accept_dur;
                    let bonus_expiry_unix = now + bonus_dur;
                    let mission_title = ctx.sde.missions.get(&mission_id).map(|m| m.title.clone()).unwrap_or_else(|| format!("Mission {mission_id}"));
                    let bookmark_id = ctx.db.with_mut_snapshot(|snap| {
                        snap.next_entity_id += 1;
                        let bmid = snap.next_entity_id;
                        let b_sys = snap.active_missions.get(&(char_id, mission_id)).map(|m| m.solar_system_id).filter(|&s| s > 0).unwrap_or(mission_sys_id);
                        snap.bookmarks.insert(bmid, eve_db::BookmarkRecord { bookmark_id: bmid, owner_id: char_id, solar_system_id: b_sys, label: format!("Mission: {mission_title}"), x: site_origin.x, y: site_origin.y, z: site_origin.z, item_id: None, memo: "Mission Pocket Destination".into(), folder_id: None, is_shared: false });
                        if let Some(m) = snap.active_missions.get_mut(&(char_id, mission_id)) {
                            m.bookmark_id = Some(bmid);
                        }
                        bmid
                    });
                    let dialogue = PyRep::List(vec![
                        PyRep::Tuple(vec![PyRep::Integer(6), PyRep::String("Complete Mission".into())]),
                        PyRep::Tuple(vec![PyRep::Integer(11), PyRep::String("Quit Mission".into())]),
                    ]);
                    let active_agent_id = ctx.db.with_snapshot(|snap| {
                        snap.active_missions.get(&(char_id, mission_id)).map_or(agent_id, |m| m.agent_id)
                    });
                    let agent_type_id = ctx.sde.agents.get(&active_agent_id).map(|a| a.agent_type_id).unwrap_or(2);
                    let mission_body = ctx.sde.missions.get(&mission_id).and_then(|m| m.content_template.as_deref()).unwrap_or("");
                    ctx.notify(NotificationTarget::Character(char_id), "OnAgentMissionChange", "charid", vec![PyRep::String("offer_accepted".into()), PyRep::Integer(active_agent_id)]);
                    emit_agent_mission_dialogue(ctx, char_id, active_agent_id, agent_type_id, &mission_title, mission_body, false);
                    let accept_msg = format!(
                        "Excellent. Your mission '{mission_title}' is now active in your journal. Return here once your objectives are complete."
                    );
                    Some(Ok(PyRep::keyval([
                        ("missionID", PyRep::Integer(mission_id as i64)),
                        ("contentID", PyRep::Integer(mission_id as i64)),
                        ("agentID", PyRep::Integer(active_agent_id)),
                        ("title", PyRep::String(mission_title)),
                        ("state", PyRep::String("Accepted".into())),
                        ("missionState", PyRep::Integer(2)),
                        ("agentSays", PyRep::String(accept_msg.clone())),
                        ("briefing", PyRep::String(accept_msg)),
                        ("dialogue", dialogue),
                        ("dungeonID", PyRep::None),
                        ("currentRoomID", PyRep::Integer(1)),
                        ("bookmarkID", PyRep::Integer(bookmark_id)),
                        ("expiryUnix", PyRep::Integer(expiry_unix as i64)),
                        ("bonusExpiryUnix", PyRep::Integer(bonus_expiry_unix as i64)),
                        ("spawnedNpcBallID", first_npc_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
                    ])))
                }

                // 6 = Complete, 7 = CompleteRemotely
                6 | 7 => {
                    let lookup_agent_id = if explicit_agent { agent_id } else { 0 };
                    let mission_id = match resolve_target_mission_id(ctx, char_id, lookup_agent_id, Some("Accepted"), args, _kwargs) { Ok(v) => v, Err(e) => return Some(Err(e)) };
                    let Some(tmpl) = ctx.sde.missions.get(&mission_id).cloned() else {
                        return Some(Err(BridgeRpcError { code: "UnknownMission".into(), message: format!("Mission {mission_id} does not exist in SDE"), client_error: None }));
                    };
                    let is_storyline = is_storyline_template(&tmpl);
                    let (active_agent_id, pre_dest_station, pre_is_dc, pre_is_trade, pre_is_courier) = ctx.db.with_snapshot(|snap| {
                        let m_agent = snap.active_missions.get(&(char_id, mission_id)).map_or(agent_id, |m| m.agent_id);
                        let meta_opt = load_mission_meta(snap, char_id, mission_id);
                        let is_dc = ctx.sde.agents.get(&m_agent).is_some_and(|a| a.agent_type_id == 5);
                        let is_trade = !is_dc && tmpl.content_template.as_deref().unwrap_or_default().contains("Trade");
                        let dest_st = meta_opt.as_ref().and_then(|mt| mt.destination_station_id);
                        let is_courier = !is_trade && !is_dc && (tmpl.is_courier || tmpl.content_template.as_deref().unwrap_or_default().contains("Courier") || dest_st.is_some());
                        (m_agent, dest_st, is_dc, is_trade, is_courier)
                    });
                    if action_id == 6 {
                        if let Some(ref agent_info) = ctx.sde.agents.get(&active_agent_id).cloned() {
                            let at_courier_dest = pre_is_courier
                                && pre_dest_station.is_some()
                                && session.station_id == pre_dest_station;
                            if !at_courier_dest && !is_agent_colocated(ctx, session, sys_id, agent_info).await {
                                return Some(Err(BridgeRpcError {
                                    code: "AgentNotCoLocated".into(),
                                    message: format!(
                                        "Agent {active_agent_id} is in system {:?} / station {:?}, not character's current location (system {sys_id}, station {:?})",
                                        agent_info.solar_system_id, agent_info.station_id, session.station_id
                                    ),
                                    client_error: None,
                                }));
                            }
                        } else if pre_is_dc || pre_is_trade {
                            return Some(Err(BridgeRpcError {
                                code: "UnknownAgent".into(),
                                message: format!("Agent {active_agent_id} does not exist in SDE"),
                                client_error: None,
                            }));
                        }
                    }
                    let agent_level = ctx.sde.agents.get(&active_agent_id).map_or(1.0, |a| a.level.max(1) as f64);
                    let storyline_mult = if is_storyline { 3.0 } else { 1.0 };
                    let standing_pct = if tmpl.has_standing_rewards { 1.5 * agent_level * storyline_mult } else { 0.0 };
                    let (storyline_agent_id, _) = resolve_agent_and_corp_for_system(ctx, char_id, sys_id, session.station_id, true);
                    let CharacterMissionRewards {
                        isk_reward,
                        lp_reward,
                        bonus_reward,
                    } = compute_character_mission_rewards(ctx, char_id, active_agent_id, mission_id, sys_id);
                    let corp_id = ctx.sde.agents.get(&active_agent_id).map(|a| a.corporation_id)
                        .unwrap_or_else(|| {
                            resolve_agent_and_corp_for_system(ctx, char_id, sys_id, session.station_id, is_storyline).1
                        });
                    let faction_id = ctx.sde.npc_corporations.get(&corp_id).and_then(|c| c.faction_id).map(|fid| fid as i64);
                    let now = ctx.now_unix();

                    let is_expired = ctx.db.with_snapshot(|snap| {
                        load_mission_meta(snap, char_id, mission_id)
                            .and_then(|m| m.accept_expiry_unix)
                            .is_some_and(|exp| exp > 0 && now > exp)
                    });
                    if is_expired {
                        ctx.db.with_mut_snapshot(|snap| {
                            if let Some(m) = snap.active_missions.get_mut(&(char_id, mission_id)) {
                                m.state = "Failed".into();
                                m.npcs_remaining = 0;
                            }
                            reclaim_courier_cargo(snap, char_id, mission_id);
                            if let Some(ch) = snap.characters.get_mut(&char_id) {
                                let cur_ag = ch.standings.get(&active_agent_id).copied().unwrap_or(0.0);
                                ch.standings.insert(active_agent_id, (cur_ag - 0.10).max(-10.0));
                                let cur_corp = ch.standings.get(&corp_id).copied().unwrap_or(0.0);
                                ch.standings.insert(corp_id, (cur_corp - 0.10).max(-10.0));
                                if let Some(fac_id) = faction_id {
                                    let cur_fac = ch.standings.get(&fac_id).copied().unwrap_or(0.0);
                                    ch.standings.insert(fac_id, (cur_fac - 0.05).max(-10.0));
                                }
                            }
                            snap.next_entity_id += 1;
                            let ev_id = snap.next_entity_id;
                            snap.standing_transactions.entry(char_id).or_default().push(
                                StandingTransactionRecord {
                                    event_id: ev_id,
                                    from_id: corp_id,
                                    to_id: char_id,
                                    modification: -0.10,
                                    event_type_id: 84,
                                },
                            );
                        });
                        ctx.notify(NotificationTarget::Character(char_id), "OnAgentMissionObjectivesFailed", "charid", vec![PyRep::Integer(active_agent_id)]);
                        ctx.notify(NotificationTarget::Character(char_id), "OnAgentMissionChange", "charid", vec![PyRep::String("failed".into()), PyRep::Integer(active_agent_id)]);
                        return Some(Err(BridgeRpcError {
                            code: "MissionExpired".into(),
                            message: format!("Mission {mission_id} has expired and failed"),
                            client_error: None,
                        }));
                    }

                    let res = ctx.db.try_with_mut_snapshot(|snap| {
                        let meta_opt = load_mission_meta(snap, char_id, mission_id);
                        let Some(m) = snap.active_missions.get_mut(&(char_id, mission_id)) else {
                            return Err(BridgeRpcError { code: "MissionNotActive".into(), message: format!("Mission {mission_id} is not active for character {char_id}"), client_error: None });
                        };
                        let is_rnd = meta_opt
                            .as_ref()
                            .and_then(|m| ctx.sde.agents.get(&m.agent_id))
                            .is_some_and(is_rnd_agent);
                        let is_dc = ctx.sde.agents.get(&m.agent_id).is_some_and(|a| a.agent_type_id == 5);
                        let is_heraldry = tmpl.content_template.as_deref().unwrap_or_default().contains("Heraldry");
                        let is_trade = !is_dc && tmpl.content_template.as_deref().unwrap_or_default().contains("Trade");
                        let is_courier = !is_trade && !is_dc && (tmpl.is_courier || tmpl.content_template.as_deref().unwrap_or_default().contains("Courier") || meta_opt.as_ref().and_then(|mt| mt.destination_station_id).is_some());
                        let is_kill = !is_dc && (tmpl.content_template.as_deref().unwrap_or_default().contains("Kill") || tmpl.content_template.as_deref().unwrap_or_default().contains("Encounter"));
                        let is_mining_mission = !is_rnd && !is_kill && !is_trade && !is_courier && {
                            let agent_div = ctx.sde.agents.get(&m.agent_id).map(|a| a.division_id).unwrap_or(0);
                            agent_div == 23 || {
                                let t = tmpl.title.to_lowercase();
                                let c = tmpl
                                    .content_template
                                    .as_deref().unwrap_or_default()
                                    .to_lowercase();
                                c.contains("mining")
                                    || t.split(|ch: char| !ch.is_alphanumeric()).any(|w| matches!(w, "mining" | "mineral" | "minerals" | "ore" | "ores"))
                            }
                        };

                        if m.state != "Accepted" || (m.npcs_remaining > 0 && !is_mining_mission && !is_rnd && !is_courier && !is_trade && !is_dc && !is_heraldry) {
                            return Err(BridgeRpcError { code: "MissionIncomplete".into(), message: format!("Mission {mission_id} cannot be completed (state='{}', npcs_remaining={})", m.state, m.npcs_remaining), client_error: None });
                        }

                        let req_delivery = meta_opt
                            .as_ref()
                            .and_then(|mt| {
                                mt.required_delivery_type_id
                                    .map(|tid| (tid, mt.required_delivery_qty.max(1)))
                            })
                            .or_else(|| {
                                if is_mining_mission {
                                    let md = resolve_mining_mission_details(ctx, &tmpl);
                                    Some((md.ore_type_id, md.required_units))
                                } else if is_trade {
                                    let td = resolve_trade_mission_details(ctx, &tmpl);
                                    Some((td.trade_type_id, td.trade_quantity))
                                } else {
                                    None
                                }
                            });
                        let m_agent_id = m.agent_id;
                        if action_id == 7 && req_delivery.is_some() {
                            return Err(BridgeRpcError {
                                code: "CannotCompleteRemotely".into(),
                                message: format!(
                                    "Mission {mission_id} requires physical item delivery and cannot be completed remotely"
                                ),
                                client_error: None,
                            });
                        }
                        if let Some((req_tid, req_units)) = req_delivery {
                            let current_station = snap.characters.get(&char_id).and_then(|c| c.station_id).or(session.station_id);
                            let agent_station_id = ctx
                                .sde
                                .agents
                                .get(&m_agent_id)
                                .and_then(|a| a.station_id)
                                .or_else(|| meta_opt.as_ref().and_then(|mt| mt.origin_station_id));
                            let active_ship_id = snap.characters.get(&char_id).map(|c| c.active_ship_id).or(session.ship_id);
                            let dest_station_id = meta_opt.as_ref().and_then(|mt| mt.destination_station_id);
                            let granted_cid = meta_opt.as_ref().and_then(|mt| mt.granted_courier_item_id);
                            super::agents_mission_delivery::verify_and_consume_mission_delivery_items(
                                snap,
                                char_id,
                                mission_id,
                                req_tid,
                                req_units,
                                is_courier,
                                is_mining_mission,
                                current_station,
                                agent_station_id,
                                active_ship_id,
                                dest_station_id,
                                granted_cid,
                            )?;
                        }

                        if let Some(m_mut) = snap.active_missions.get_mut(&(char_id, mission_id)) {
                            m_mut.state = "Completed".into();
                        }
                        reclaim_courier_cargo(snap, char_id, mission_id);

                        let bonus_granted = meta_opt
                            .as_ref().and_then(|mt| mt.bonus_expiry_unix)
                            .is_none_or(|bexp| now <= bexp);
                        let bonus_paid = if bonus_granted { bonus_reward } else { 0 };
                        let char_sys = snap.characters.get(&char_id).map(|c| c.solar_system_id).unwrap_or(0);
                        let lp_mult = crate::services::pve_special_sites::get_incursion_effects_for_system_in_snap(&ctx.sde, snap, char_sys)
                            .map(|e| (1.0 - e.lp_penalty).clamp(0.0, 1.0))
                            .unwrap_or(1.0);
                        let effective_lp = ((lp_reward.max(0) as f64) * lp_mult).round() as i64;
                        let is_taxed_npc_corp = snap.characters.get(&char_id).is_some_and(|ch| {
                            ch.corporation_id > 0
                                && !snap.corporations.contains_key(&ch.corporation_id)
                                && !matches!(ch.corporation_id, 1_000_179..=1_000_182 | 1_000_436 | 1_000_437)
                        });
                        let credited_lp = if is_taxed_npc_corp && isk_reward >= 100_000 {
                            effective_lp - ((effective_lp.saturating_mul(11) + 50) / 100)
                        } else {
                            effective_lp
                        };

                        if let Some(ch) = snap.characters.get_mut(&char_id) {
                            let reward_cents = IskCents::from_isk(isk_reward);
                            ch.balance = ch.balance.checked_add(reward_cents).unwrap_or(ch.balance);
                            let bal_after_reward = ch.balance;

                            snap.next_entity_id += 1;
                            let jid1 = snap.next_entity_id;
                            snap.wallet_journal.push(WalletJournalEntry {
                                entry_id: jid1,
                                character_id: char_id,
                                entry_type: "agent_mission_reward".to_string(),
                                amount: reward_cents,
                                balance_after: bal_after_reward,
                                reason: format!("Mission {mission_id} reward"),
                            });
                            snap.wallet_journal_timestamps.insert(jid1, now);

                            if bonus_paid > 0 {
                                let bonus_cents = IskCents::from_isk(bonus_paid);
                                ch.balance = ch.balance.checked_add(bonus_cents).unwrap_or(ch.balance);
                                snap.next_entity_id += 1;
                                let jid2 = snap.next_entity_id;
                                snap.wallet_journal.push(WalletJournalEntry {
                                    entry_id: jid2,
                                    character_id: char_id,
                                    entry_type: "agent_mission_time_bonus_reward".to_string(),
                                    amount: bonus_cents,
                                    balance_after: ch.balance,
                                    reason: format!("Mission {mission_id} time bonus reward"),
                                });
                                snap.wallet_journal_timestamps.insert(jid2, now);
                            }

                            let lp_entry = ch.loyalty_points.entry(corp_id).or_insert(0);
                            *lp_entry = lp_entry.saturating_add(credited_lp);
                            if corp_id == crate::services::lp_store_catalog::PARAGON_CORP_ID && ch.corporation_id >= 98_000_000 {
                                let c_lp = snap.corp_lp_balances.entry((ch.corporation_id, corp_id)).or_insert(0);
                                *c_lp = c_lp.saturating_add(credited_lp);
                            }
                        }
                        crate::services::lp_store::record_lp_reward_log(
                            snap,
                            char_id,
                            corp_id,
                            credited_lp,
                            format!("Mission {mission_id} (agent {m_agent_id})"),
                            now,
                        );

                        let is_rnd = ctx.sde.agents.get(&m_agent_id).is_some_and(is_rnd_agent);
                        if is_rnd {
                            crate::services::pve_ess_reserve_banks::record_agent_mission_completed(snap, char_id, m_agent_id);
                            let ag_lvl = ctx.sde.agents.get(&m_agent_id).map_or(1, |a| a.level);
                            let mut research = load_rnd_research_entries(snap, char_id);
                            if let Some(re) = research.iter_mut().find(|r| r.agent_id == m_agent_id) {
                                let elapsed_days = (now.saturating_sub(re.started_unix) as f64) / 86_400.0;
                                re.initial_points += re.points_per_day * elapsed_days + re.points_per_day;
                                let sk_lvl = snap.skills.get(&char_id).and_then(|m| m.get(&re.research_type_id)).map_or(1, |s| s.level);
                                re.points_per_day = re.points_per_day.max(crate::services::pve_ess_reserve_banks::compute_rnd_rp_per_day(ctx, snap, char_id, m_agent_id, ag_lvl, sk_lvl));
                                re.started_unix = now;
                            }
                            save_rnd_research_entries(snap, char_id, &research);
                        }

                        let is_special = is_storyline
                            || ctx.sde.agents.get(&m_agent_id)
                                .is_some_and(|a| matches!(a.agent_type_id, 5 | 8 | 10));
                        let (new_st, st_changes) = apply_mission_completion_standings(ctx, snap, char_id, m_agent_id, mission_id, corp_id, standing_pct, is_special);

                        if ctx.sde.agents.get(&m_agent_id)
                            .is_some_and(|a| a.agent_type_id == 10)
                        {
                            let (arc_id, fac_id) = resolve_epic_arc_id_for_agent(ctx, m_agent_id);
                            let required_missions = epic_arc_required_missions(ctx, fac_id);
                            let mut arcs = load_epic_arc_states(snap, char_id);
                            if let Some(arc) = arcs.iter_mut().find(|a| a.epic_arc_id == arc_id) {
                                if arc.completed_at_unix.is_some() {
                                    arc.completed_missions.clear();
                                    arc.completed_at_unix = None;
                                    arc.cooldown_until_unix = None;
                                    arc.chapter = 1;
                                }
                                if !arc.completed_missions.contains(&mission_id) {
                                    arc.completed_missions.push(mission_id);
                                }
                                let done = arc.completed_missions.len();
                                let computed_chapter = ((done * 3).saturating_add(required_missions - 1) / required_missions).clamp(1, 3) as u32;
                                arc.chapter = arc.chapter.max(computed_chapter);
                                if done >= required_missions {
                                    arc.completed_at_unix = Some(now);
                                    arc.cooldown_until_unix = Some(now + 7_776_000);
                                }
                            } else {
                                arcs.push(EpicArcStateEntry {
                                    epic_arc_id: arc_id,
                                    agent_id: m_agent_id,
                                    faction_id: fac_id,
                                    chapter: 1,
                                    completed_missions: vec![mission_id],
                                    chosen_branch: None,
                                    completed_at_unix: None,
                                    cooldown_until_unix: None,
                                });
                            }
                            save_epic_arc_states(snap, char_id, &arcs);
                        }

                        let is_epic_arc = ctx.sde.agents.get(&m_agent_id).is_some_and(|a| matches!(a.agent_type_id, 10 | 11));
                        let mut resolved_storyline_agent_id = storyline_agent_id;
                        let mut resolved_storyline_level = ctx.sde.agents.get(&m_agent_id).map_or(1, |a| a.level.clamp(1, 5));
                        let (t, storyline) = if (!is_storyline && tmpl.has_standing_rewards) || is_rnd || is_epic_arc {
                            let sl_opt = ctx.sde.agents.get(&m_agent_id).and_then(|ca| crate::services::pve_ess_reserve_banks::record_mission_completion_and_resolve_storyline(ctx, snap, char_id, ca));
                            let qualifies = ctx.sde.agents.get(&m_agent_id).is_some_and(|ca| crate::services::pve_ess_reserve_banks::is_storyline_qualifying_agent_type(ca.agent_type_id)) && !crate::services::pve_ess_reserve_banks::is_interbus_or_militia_corp(&ctx.sde, corp_id);
                            let t = if !qualifies {
                                snap.mission_tally.get(&char_id).copied().unwrap_or(0)
                            } else {
                                let count = snap.mission_tally.entry(char_id).or_insert(0); *count += 1; *count
                            };
                            if let Some((sl_id, sl_lvl)) = sl_opt { resolved_storyline_agent_id = sl_id; resolved_storyline_level = sl_lvl; }
                            (t, sl_opt.is_some())
                        } else {
                            (snap.mission_tally.get(&char_id).copied().unwrap_or(0), false)
                        };
                        if storyline {
                            let storyline_agent_id = resolved_storyline_agent_id;
                            if let Some(storyline_tmpl) = crate::services::pve_ess_reserve_banks::select_storyline_mission_for_level(
                                &ctx.sde,
                                &snap.active_missions,
                                char_id,
                                resolved_storyline_level,
                            )
                            {
                                let storyline_mission_id = storyline_tmpl.mission_id;
                                let sl_agent = ctx.sde.agents.get(&storyline_agent_id);
                                let sl_sys_id = sl_agent.and_then(|a| a.solar_system_id).unwrap_or(sys_id);
                                let ct = storyline_tmpl.content_template.as_deref().unwrap_or_default();
                                let is_trade = ct.contains("BasicTrade") || ct.contains("TradeMission") || ct.contains("Trade");
                                let is_courier = !is_trade && (storyline_tmpl.is_courier || ct.contains("BasicCourier"));
                                let is_kill = ct.contains("BasicKill") || ct.contains("Kill") || ct.contains("Encounter");
                                let is_mining = !is_kill && !is_trade && !is_courier && (ct.contains("BasicMining")
                                    || storyline_tmpl.title.to_lowercase().contains("mining")
                                    || sl_agent.is_some_and(|a| a.division_id == 23));
                                let (req_tid, req_qty, origin_sid, dest_sid, sl_npcs) = if is_courier {
                                    if let Some(sa) = sl_agent.and_then(|a| resolve_courier_mission_details(ctx, storyline_tmpl, a).ok()) {
                                        (Some(sa.cargo_type_id), sa.cargo_quantity, Some(sa.origin_station_id), Some(sa.destination_station_id), 0)
                                    } else {
                                        (None, 0, sl_agent.and_then(|a| a.station_id), None, 0)
                                    }
                                } else if is_trade {
                                    let td = resolve_trade_mission_details(ctx, storyline_tmpl);
                                    (Some(td.trade_type_id), td.trade_quantity, sl_agent.and_then(|a| a.station_id), None, 0)
                                } else if is_mining {
                                    let md = resolve_mining_mission_details(ctx, storyline_tmpl);
                                    (Some(md.ore_type_id), md.required_units, sl_agent.and_then(|a| a.station_id), None, 0)
                                } else if let Some(sa) = sl_agent {
                                    let mut scaled_sa = sa.clone();
                                    scaled_sa.level = resolved_storyline_level;
                                    let enc = resolve_security_mission_encounter(ctx, storyline_tmpl, &scaled_sa, sl_sys_id);
                                    (None, 0, sa.station_id, None, enc.wave_npc_count)
                                } else {
                                    (None, 0, None, None, 1)
                                };
                                save_mission_meta(
                                    snap,
                                    char_id,
                                    &MissionTimingAndCargoMeta {
                                        mission_id: storyline_mission_id,
                                        agent_id: storyline_agent_id,
                                        offered_at_unix: now,
                                        offer_expiry_unix: now + 604_800,
                                        accepted_at_unix: None,
                                        accept_expiry_unix: None,
                                        bonus_expiry_unix: None,
                                        completed_at_unix: None,
                                        required_delivery_type_id: req_tid,
                                        required_delivery_qty: req_qty,
                                        granted_courier_item_id: None,
                                        granted_courier_qty: 0,
                                        is_anomic_burner: false,
                                        chosen_branch: None,
                                        destination_station_id: dest_sid,
                                        origin_station_id: origin_sid,
                                        offered_level: Some(resolved_storyline_level),
                                    },
                                );
                                snap.active_missions
                                    .entry((char_id, storyline_mission_id))
                                    .or_insert(ActiveMissionRecord { mission_id: storyline_mission_id, character_id: char_id, agent_id: storyline_agent_id, solar_system_id: sl_sys_id, state: "Offered".into(), current_room_id: 1, npcs_remaining: sl_npcs, spawned_npc_ball_ids: Vec::new(), bookmark_id: None });
                            }
                        }
                        let delivery_station = ctx.sde.agents.get(&m_agent_id).and_then(|a| a.station_id).or_else(|| snap.characters.get(&char_id).and_then(|c| c.station_id)).unwrap_or(0);
                        let item_reward = crate::services::agents_mission_rewards::mint_mission_item_reward_in_snap(ctx, snap, char_id, delivery_station, &tmpl, ctx.sde.agents.get(&m_agent_id));
                        let isk_paid = isk_reward > 0 || bonus_paid > 0;
                        let new_bal_opt = isk_paid.then(|| snap.characters.get(&char_id).map(|c| c.balance)).flatten();
                        Ok((new_st, t, storyline, bonus_paid, st_changes, item_reward, new_bal_opt))
                    });

                    match res {
                        Ok((new_standing, tally, storyline_offered, bonus_paid, st_changes, item_reward, new_bal_opt)) => {
                            if let Some(bal) = new_bal_opt {
                                let _ = ctx.notify_wallet_balance(false, char_id, 1, bal);
                            }
                            if let Some(irw) = item_reward {
                                crate::services::agents_mission_rewards::emit_mission_item_reward_notification(ctx, char_id, irw);
                            }
                            notify_standing_changes(ctx, char_id, &st_changes);
                            ctx.notify(NotificationTarget::Character(char_id), "OnAgentMissionObjectivesCompleted", "charid", vec![PyRep::Integer(active_agent_id)]);
                            ctx.notify(NotificationTarget::Character(char_id), "OnAgentMissionChange", "charid", vec![PyRep::String("completed".into()), PyRep::Integer(active_agent_id)]);
                            let agent_type_id = ctx.sde.agents.get(&active_agent_id).map(|a| a.agent_type_id).unwrap_or(2);
                            let mission_title = ctx.sde.missions.get(&mission_id).map(|m| m.title.clone()).unwrap_or_else(|| format!("Mission {mission_id}"));
                            let mission_body = ctx.sde.missions.get(&mission_id).and_then(|m| m.content_template.as_deref()).unwrap_or("");
                            emit_agent_mission_dialogue(ctx, char_id, active_agent_id, agent_type_id, &mission_title, mission_body, true);
                            let complete_msg = format!(
                                "Well done! Your mission '{mission_title}' is complete and your rewards have been credited."
                            );
                            Some(Ok(PyRep::keyval([
                                ("missionID", PyRep::Integer(mission_id as i64)),
                                ("contentID", PyRep::Integer(mission_id as i64)),
                                ("agentID", PyRep::Integer(active_agent_id)),
                                ("title", PyRep::String(mission_title)),
                                ("state", PyRep::String("Completed".into())),
                                ("missionState", PyRep::Integer(4)),
                                ("agentSays", PyRep::String(complete_msg.clone())),
                                ("briefing", PyRep::String(complete_msg)),
                                (
                                    "dialogue",
                                    PyRep::List(vec![PyRep::Tuple(vec![
                                        PyRep::Integer(2),
                                        PyRep::String("Request Mission".into()),
                                    ])]),
                                ),
                                ("iskReward", PyRep::Integer(isk_reward)),
                                ("bonusReward", PyRep::Integer(bonus_paid)),
                                ("lpReward", PyRep::Integer(lp_reward)),
                                ("newStanding", PyRep::Real(new_standing)),
                                ("missionTally", PyRep::Integer(tally as i64)),
                                ("storylineTriggered", PyRep::Bool(storyline_offered)),
                                ("rewardItemID", item_reward.map_or(PyRep::None, |r| PyRep::Integer(r.item_id))),
                                ("rewardItemTypeID", item_reward.map_or(PyRep::None, |r| PyRep::Integer(r.type_id as i64))),
                                ("rewardItemQuantity", item_reward.map_or(PyRep::None, |r| PyRep::Integer(r.quantity as i64))),
                            ])))
                        }
                        Err(e) => Some(Err(e)),
                    }
                }

                // 9 = Decline, 10 = Defer, 11 = Quit
                9..=11 => {
                    let expected_state = if action_id == 11 { Some("Accepted") } else { Some("Offered") };
                    let lookup_agent_id = if explicit_agent { agent_id } else { 0 };
                    let mission_id = match resolve_target_mission_id(ctx, char_id, lookup_agent_id, expected_state, args, _kwargs) { Ok(v) => v, Err(e) => return Some(Err(e)) };
                    let active_agent_id = ctx.db.with_snapshot(|snap| {
                        snap.active_missions.get(&(char_id, mission_id)).map_or(agent_id, |m| m.agent_id)
                    });
                    match abandon_or_decline_mission(ctx, session, char_id, mission_id).await {
                        Ok((new_state, penalty_applied, was_accepted)) => {
                            let mission_state = if was_accepted { 6_i64 } else { 5_i64 };
                            if was_accepted {
                                ctx.notify(NotificationTarget::Character(char_id), "OnAgentMissionObjectivesFailed", "charid", vec![PyRep::Integer(active_agent_id)]);
                            }
                            let action_str = if was_accepted { "quit" } else { "offer_declined" };
                            ctx.notify(NotificationTarget::Character(char_id), "OnAgentMissionChange", "charid", vec![PyRep::String(action_str.into()), PyRep::Integer(active_agent_id)]);
                            let quit_msg = if was_accepted {
                                "Very well. I have cancelled your active assignment.".to_string()
                            } else {
                                "Very well. Let me know when you are ready for another assignment.".to_string()
                            };
                            Some(Ok(PyRep::keyval([
                                ("missionID", PyRep::Integer(mission_id as i64)),
                                ("contentID", PyRep::Integer(mission_id as i64)),
                                ("agentID", PyRep::Integer(active_agent_id)),
                                ("state", PyRep::String(new_state)),
                                ("missionState", PyRep::Integer(mission_state)),
                                ("agentSays", PyRep::String(quit_msg.clone())),
                                ("briefing", PyRep::String(quit_msg)),
                                (
                                    "dialogue",
                                    PyRep::List(vec![PyRep::Tuple(vec![
                                        PyRep::Integer(2),
                                        PyRep::String("Request Mission".into()),
                                    ])]),
                                ),
                                ("standingPenaltyApplied", PyRep::Bool(penalty_applied)),
                                ("dungeonCleaned", PyRep::Bool(was_accepted)),
                            ])))
                        }
                        Err(e) => Some(Err(e)),
                    }
                }

                _ => Some(Err(BridgeRpcError { code: "InvalidDialogueAction".into(), message: format!("Unknown agent dialogue action {action_id}"), client_error: None })),
            }
        }

        ("agentMgr", "RemoveOfferFromJournal") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError { code: "NoSolarSystem".into(), message: "Session has no active solar system".into(), client_error: None }));
            };
            let (agent_id, explicit_agent) =
                resolve_target_agent_id_with_bound(ctx, session, bound_entry, char_id, sys_id, args, _kwargs);
            let lookup_agent_id = if explicit_agent { agent_id } else { 0 };
            let mission_id = match parse_mission_id(args).or_else(|_| {
                resolve_target_mission_id(ctx, char_id, lookup_agent_id, None, args, _kwargs)
            }) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            match abandon_or_decline_mission(ctx, session, char_id, mission_id).await {
                Ok((new_state, penalty_applied, was_accepted)) => Some(Ok(PyRep::keyval([
                    ("missionID", PyRep::Integer(mission_id as i64)),
                    ("state", PyRep::String(new_state)),
                    ("standingPenaltyApplied", PyRep::Bool(penalty_applied)),
                    ("dungeonCleaned", PyRep::Bool(was_accepted)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        _ => None,
    }
}

#[cfg(test)] #[path = "agents_missions_unit_tests.rs"] mod agents_missions_unit_tests;
#[cfg(test)] #[path = "agents_missions_edges2_tests.rs"] mod agents_missions_edges2_tests;

