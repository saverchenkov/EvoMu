//! `agents_missions` — Domain 9: Agent Dialogue, Missions, 16-Mission Storyline Tally, Epic Arcs, & AIR NPE (`§3.8`).

// parity: plan:E-5
// parity: feat:pve.mission-accept
// parity: feat:pve.mission-complete, feat:pve.mission-decline, feat:pve.mission-quit
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

use crate::router::{NotificationTarget, ServerContext};
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
    if let Some(res) =
        agents_standings_arcs::handle_extended(ctx, session, service, method, args, _kwargs).await
    {
        return Some(res);
    }

    match (service, method) {
        ("agentMgr", "GetAgents") => {
            let caller_char = session.character_id;
            let filter_level = args
                .first().and_then(|a| a.as_i64()).map(|v| v as u8).or_else(|| _kwargs.get("level").and_then(|v| v.as_i64()).map(|v| v as u8));
            let filter_division = args.get(1).and_then(|a| a.as_i64()).map(|v| v as u32).or_else(|| {
                    _kwargs.get("divisionID").or_else(|| _kwargs.get("division")).and_then(|v| v.as_i64()).map(|v| v as u32)
                });
            let filter_faction = args.get(2).and_then(|a| a.as_i64()).or_else(|| {
                _kwargs.get("factionID").or_else(|| _kwargs.get("faction")).and_then(|v| v.as_i64())
            });
            let filter_corp = args.get(3).and_then(|a| a.as_i64()).or_else(|| {
                _kwargs.get("corporationID").or_else(|| _kwargs.get("corporation")).and_then(|v| v.as_i64())
            });

            let mut matching_agents: Vec<&eve_sde::AgentInfo> = ctx.sde.agents
                .values().filter(|a| {
                    if filter_level.is_some_and(|lvl| a.level != lvl) {
                        return false;
                    }
                    if filter_division.is_some_and(|div| a.division_id != div) {
                        return false;
                    }
                    if filter_corp.is_some_and(|cid| a.corporation_id != cid) {
                        return false;
                    }
                    if let Some(fid) = filter_faction {
                        let corp_faction = ctx.sde.npc_corporations.get(&a.corporation_id).and_then(|c| c.faction_id).map(|f| f as i64);
                        if corp_faction != Some(fid) {
                            return false;
                        }
                    }
                    true
                })
                .collect();
            matching_agents.sort_by_key(|a| a.agent_id);

            let rows = ctx.db.with_snapshot(|snap| {
                matching_agents
                    .into_iter().map(|a| {
                        let corp_faction = ctx.sde.npc_corporations.get(&a.corporation_id).and_then(|c| c.faction_id).map(|f| f as i64);
                        let has_access = if a.level <= 1 {
                            true
                        } else if let Some(cid) = caller_char {
                            let req_standing = ((a.level as f64) - 1.0) * 2.0 - 1.0;
                            let eff = compute_effective_agent_standing(ctx, snap, cid, a.agent_id, a.corporation_id, corp_faction);
                            eff >= req_standing
                        } else {
                            false
                        };
                        vec![
                            PyRep::Integer(a.agent_id),
                            PyRep::Integer(a.agent_type_id as i64),
                            PyRep::Integer(a.division_id as i64),
                            PyRep::Integer(a.corporation_id),
                            corp_faction.map(PyRep::Integer).unwrap_or(PyRep::None),
                            a.solar_system_id.map(|s| PyRep::Integer(s as i64)).unwrap_or(PyRep::None),
                            a.station_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                            PyRep::Integer(a.level as i64),
                            PyRep::Integer(20),
                            PyRep::Bool(has_access),
                        ]
                    })
                    .collect::<Vec<_>>()
            });

            // Column names follow the client: `Agents.__GetAllAgents` indexes the rowset with
            // `Filter('stationID')`, `Filter('solarsystemID')`, `Filter('corporationID')`,
            // `Filter('factionID')` and `Filter('agentTypeID')`
            // (`eve/client/script/ui/station/agents/agents.pyj:110-117`).
            Some(Ok(PyRep::rowset(
                vec!["agentID", "agentTypeID", "divisionID", "corporationID", "factionID", "solarsystemID", "stationID", "level", "quality", "hasAccess",],
                rows,
            )))
        }

        ("agentMgr", "GetInfoServiceDetails") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError { code: "NoSolarSystem".into(), message: "Session has no active solar system".into() }));
            };
            let (agent_id, _) =
                resolve_target_agent_id(ctx, session, char_id, sys_id, args, _kwargs);
            let Some(a) = ctx.sde.agents.get(&agent_id) else {
                return Some(Err(BridgeRpcError { code: "UnknownAgent".into(), message: format!("Agent {agent_id} does not exist in SDE") }));
            };
            let mut services = vec![PyRep::String("mission".into())];
            if a.is_locator {
                services.push(PyRep::String("locator".into()));
            }
            if a.agent_type_id == 4 || a.division_id == 18 {
                services.push(PyRep::String("research".into()));
            }
            Some(Ok(PyRep::keyval([
                ("agentID", PyRep::Integer(a.agent_id)),
                ("agentTypeID", PyRep::Integer(a.agent_type_id as i64)),
                ("divisionID", PyRep::Integer(a.division_id as i64)),
                ("corporationID", PyRep::Integer(a.corporation_id)),
                (
                    "solarSystemID",
                    a.solar_system_id.map(|s| PyRep::Integer(s as i64)).unwrap_or(PyRep::None),
                ),
                ("stationID", a.station_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
                ("level", PyRep::Integer(a.level as i64)),
                ("quality", PyRep::Integer(20)),
                ("isLocator", PyRep::Bool(a.is_locator)),
                ("services", PyRep::List(services)),
            ])))
        }

        ("agentMgr", "GetSolarSystemOfAgent") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError { code: "NoSolarSystem".into(), message: "Session has no active solar system".into() }));
            };
            let agent_id = if let Some(first) = args.first().and_then(|a| a.as_i64()) {
                first
            } else {
                resolve_target_agent_id(ctx, session, char_id, sys_id, args, _kwargs).0
            };
            let Some(a) = ctx.sde.agents.get(&agent_id) else {
                return Some(Err(BridgeRpcError { code: "UnknownAgent".into(), message: format!("Agent {agent_id} does not exist in SDE") }));
            };
            Some(Ok(
                a.solar_system_id.map(|s| PyRep::Integer(s as i64)).unwrap_or(PyRep::None),
            ))
        }

        ("agentMgr", "GetMyJournalDetails") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let now = ctx.now_unix();
            let (missions, research) = ctx.db.with_mut_snapshot(|snap| {
                // Sweep and transition any expired offers or accepted missions (feat:pve.mission-expiration)
                let char_mids: Vec<u32> = snap.active_missions.values().filter(|m| {
                        m.character_id == char_id
                            && matches!(m.state.as_str(), "Offered" | "Accepted")
                    }).map(|m| m.mission_id)
                    .collect();
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
                        let bms: Vec<PyRep> = snap.bookmarks.values().filter(|b| {
                                b.owner_id == char_id
                                    && b.solar_system_id == m.solar_system_id
                                    && b.memo.contains("Mission Pocket")
                            }).map(|b| {
                                PyRep::keyval([
                                    ("bookmarkID", PyRep::Integer(b.bookmark_id)),
                                    ("solarSystemID", PyRep::Integer(b.solar_system_id as i64)),
                                    ("label", PyRep::String(b.label.clone())),
                                    ("x", PyRep::Real(b.x)),
                                    ("y", PyRep::Real(b.y)),
                                    ("z", PyRep::Real(b.z)),
                                ])
                            })
                            .collect();
                        PyRep::keyval([
                            ("missionID", PyRep::Integer(m.mission_id as i64)),
                            ("contentID", PyRep::Integer(m.mission_id as i64)),
                            ("agentID", PyRep::Integer(m.agent_id)),
                            ("solarSystemID", PyRep::Integer(m.solar_system_id as i64)),
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
                return Some(Err(BridgeRpcError { code: "NoSolarSystem".into(), message: "Session has no active solar system".into() }));
            };
            let (agent_id, _) =
                resolve_target_agent_id(ctx, session, char_id, sys_id, args, _kwargs);
            let mission_id =
                match resolve_target_mission_id(ctx, char_id, agent_id, None, args, _kwargs) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some((m, meta)) = ctx.db.with_snapshot(|snap| {
                snap.active_missions.get(&(char_id, mission_id)).cloned().map(|m| (m, load_mission_meta(snap, char_id, mission_id)))
            }) else {
                return Some(Err(BridgeRpcError { code: "MissionNotActive".into(), message: format!("Mission {mission_id} is not active for character {char_id}") }));
            };
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
            let (isk_reward, lp_reward, bonus_reward) =
                compute_character_mission_rewards(ctx, char_id, m.agent_id, mission_id, sys_id);
            Some(Ok(PyRep::keyval([
                ("missionID", PyRep::Integer(mission_id as i64)),
                ("contentID", PyRep::Integer(mission_id as i64)),
                ("agentID", PyRep::Integer(m.agent_id)),
                ("title", PyRep::String(title)),
                ("state", PyRep::String(m.state)),
                ("missionState", PyRep::Integer(mission_state)),
                ("briefing", PyRep::String(briefing)),
                ("objectives", PyRep::String(objectives.into())),
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
                return Some(Err(BridgeRpcError { code: "NoSolarSystem".into(), message: "Session has no active solar system".into() }));
            };
            let action_id = if method == "GetOffer" {
                2_i64
            } else if let Some(raw_action) = args.first().and_then(|a| a.as_i64()) {
                if (1..=19).contains(&raw_action) { raw_action } else { 2_i64 }
            } else {
                2_i64
            };

            let (mut agent_id, mut explicit_agent) =
                resolve_target_agent_id(ctx, session, char_id, sys_id, args, _kwargs);

            if (12..=19).contains(&action_id) {
                if let Some(sid) = args.get(1).and_then(|a| a.as_i64()) {
                    if ctx.sde.agents.contains_key(&sid) {
                        agent_id = sid;
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
                        args.first().and_then(|a| a.as_i64())
                    };
                    if let Some(sid) = supplied_id {
                        agent_id = sid;
                        explicit_agent = true;
                    }
                    let Some(agent_info) = ctx.sde.agents.get(&agent_id) else {
                        return Some(Err(BridgeRpcError { code: "UnknownAgent".into(), message: format!("Agent {agent_id} does not exist in SDE") }));
                    };
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
                        return Some(Err(BridgeRpcError { code: "AgentNotCoLocated".into(), message: format!("Agent {agent_id} is in system {:?} / station {:?}, not character's current location (system {sys_id}, station {:?})", agent_info.solar_system_id, agent_info.station_id, session.station_id) }));
                    }
                    if is_storyline_agent_type(agent_info.agent_type_id) {
                        return Some(Err(BridgeRpcError { code: "StorylineAgentRequiresReferral".into(), message: format!("Storyline agent {agent_id} (agentType {}) cannot be requested directly without a 16-mission referral", agent_info.agent_type_id) }));
                    }
                    if agent_info.agent_type_id == 11 {
                        return Some(Err(BridgeRpcError { code: "UnimplementedRPC".into(), message: format!("Agent {agent_id} (agentType {}) uses Aura node-graph missions which are not yet implemented", agent_info.agent_type_id) }));
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
                            return Some(Err(BridgeRpcError { code: "EpicArcOnCooldown".into(), message: format!("Epic Arc {arc_id} (agent {agent_id}) is on 90-day repeat cooldown") }));
                        }
                    }
                    if matches!(agent_info.agent_type_id, 5 | 8 | 9 | 12) {
                        let already_done = ctx.db.with_snapshot(|snap| {
                            snap.active_missions.values().any(|m| {
                                m.character_id == char_id
                                    && m.agent_id == agent_id
                                    && m.state == "Completed"
                            })
                        });
                        if already_done {
                            return Some(Err(BridgeRpcError { code: "OneTimeAgentMissionCompleted".into(), message: format!("One-time COSMOS / Data Center / Career agent {agent_id} mission has already been completed") }));
                        }
                    }

                    if matches!(agent_info.agent_type_id, 5 | 8 | 9 | 12) {
                        let cosmos_expired = ctx.db.with_snapshot(|snap| {
                            snap.active_missions.values().any(|m| {
                                m.character_id == char_id && m.agent_id == agent_id && m.state == "Expired"
                            })
                        });
                        if cosmos_expired {
                            return Some(Err(BridgeRpcError {
                                code: "CosmosMissionExpired".into(),
                                message: format!("One-time COSMOS agent {agent_id} mission offer has expired and cannot be re-offered"),
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
                        }));
                    }

                    if agent_info.level == 5 {
                        let agent_sys = agent_info.solar_system_id.unwrap_or(sys_id);
                        let sec = ctx.sde.solar_systems.get(&agent_sys).map(|s| s.security_status).unwrap_or(1.0);
                        if sec >= 0.45 {
                            return Some(Err(BridgeRpcError {
                                code: "CantDoMission".into(),
                                message: format!("Level 5 agent {agent_id} cannot operate in high-security space ({sec:.2})"),
                            }));
                        }
                    }

                    let is_rnd = is_rnd_agent(agent_info);
                    if is_rnd {
                        let has_research = ctx.db.with_snapshot(|snap| {
                            load_rnd_research_entries(snap, char_id).iter().any(|r| r.agent_id == agent_id)
                        });
                        if !has_research {
                            return Some(Err(BridgeRpcError {
                                code: "ResearchNotActive".into(),
                                message: format!("Agent {agent_id} requires active research to offer daily missions"),
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
                            return Some(Err(BridgeRpcError { code: "InsufficientAgentStanding".into(), message: format!("Agent {agent_id} (level {}) requires agent/corporation/faction standing >= {required_standing:.1}, character has {effective_standing:.2}", agent_info.level) }));
                        }
                    } else if !matches!(agent_info.agent_type_id, 7 | 12) && effective_standing <= -2.0 {
                        return Some(Err(BridgeRpcError { code: "InsufficientAgentStanding".into(), message: format!("Agent {agent_id} (level 1) requires standing > -2.0, character has {effective_standing:.2}") }));
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
                            return Err(BridgeRpcError { code: "NoMissionsAvailable".into(), message: format!("No unoffered missions remaining in SDE pool for agent {agent_id} and character {char_id}") });
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
                            &MissionTimingAndCargoMeta { mission_id, agent_id, offered_at_unix: now, offer_expiry_unix: now + offer_dur, accepted_at_unix: None, accept_expiry_unix: None, bonus_expiry_unix: None, completed_at_unix: None, required_delivery_type_id: req_tid, required_delivery_qty: req_qty, granted_courier_item_id: None, granted_courier_qty: 0, is_anomic_burner: is_burner, chosen_branch: None, destination_station_id: dest_sid, origin_station_id: origin_sid, },
                        );
                        let target_sys = if is_fw_agent(agent_info) {
                            resolve_fw_warzone_system(agent_info.corporation_id)
                        } else {
                            sys_id
                        };
                        snap.active_missions.insert(
                            (char_id, mission_id),
                            ActiveMissionRecord { mission_id, character_id: char_id, agent_id, solar_system_id: target_sys, state: "Offered".into(), current_room_id: 1, npcs_remaining, spawned_npc_ball_ids: Vec::new() },
                        );
                        Ok((mission_id, title, briefing, objectives, dest_sid))
                    });

                    match res {
                        Ok((mission_id, title, briefing, objectives, dest_sid)) => {
                            let expiry_unix = now + offer_dur;
                            let dialogue = PyRep::List(vec![
                                PyRep::Tuple(vec![PyRep::Integer(3), PyRep::String("Accept".into())]),
                                PyRep::Tuple(vec![PyRep::Integer(9), PyRep::String("Decline".into())]),
                            ]);
                            let (isk_reward, lp_reward, bonus_reward) =
                                compute_character_mission_rewards(ctx, char_id, agent_id, mission_id, sys_id);

                            ctx.notify(NotificationTarget::Character(char_id), "OnAgentMissionChange", "charid", vec![PyRep::String("offered".into()), PyRep::Integer(agent_id)]);
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
                    let mission_id = match resolve_target_mission_id(ctx, char_id, agent_id, Some("Offered"), args, _kwargs) { Ok(v) => v, Err(e) => return Some(Err(e)) };
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
                        }));
                    }
                    let tmpl_opt = ctx.sde.missions.get(&mission_id).cloned();
                    let agent_info_opt = ctx.sde.agents.get(&actual_agent_id).cloned();
                    if action_id == 3 {
                        if let Some(ref agent_info) = agent_info_opt {
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
                                return Some(Err(BridgeRpcError {
                                    code: "AgentNotCoLocated".into(),
                                    message: format!(
                                        "Agent {actual_agent_id} is in system {:?} / station {:?}, not character's current location (system {sys_id}, station {:?})",
                                        agent_info.solar_system_id, agent_info.station_id, session.station_id
                                    ),
                                }));
                            }
                        }
                    }
                    let enc_opt = tmpl_opt.as_ref().and_then(|t| {
                        agent_info_opt
                            .as_ref()
                            .map(|a| resolve_security_mission_encounter(ctx, t, a, sys_id))
                    });

                    let handle = ctx.systems.get_or_spawn(sys_id);
                    let existing_accepted = ctx.db.with_snapshot(|snap| {
                        snap.active_missions.get(&(char_id, mission_id)).filter(|m| m.state == "Accepted" && m.npcs_remaining > 0).map(|m| (m.solar_system_id, m.spawned_npc_ball_ids.clone()))
                    });
                    let can_respawn_missing =
                        if let Some((m_sys_id, prev_balls)) = existing_accepted {
                            if m_sys_id == sys_id && !prev_balls.is_empty() {
                                let m_handle = ctx.systems.get_or_spawn(m_sys_id);
                                let mut all_missing = true;
                                for bid in prev_balls {
                                    if m_handle
                                        .get_ball_snapshot(bid)
                                        .await
                                        .ok()
                                        .flatten()
                                        .is_some()
                                    {
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
                                return Some(Err(BridgeRpcError {
                                    code: "MissionOfferExpired".into(),
                                    message: format!("Mission {mission_id} offer has expired"),
                                }));
                            }
                        } else if !(state == "Accepted" && sys == sys_id && can_respawn_missing) {
                            return Some(Err(BridgeRpcError {
                                code: "InvalidMissionState".into(),
                                message: format!("Mission {mission_id} is in state '{state}' (system {sys}), not Offered"),
                            }));
                        }
                    }

                    let res = ctx.db.try_with_mut_snapshot(|snap| {
                        let Some(m_check) = snap.active_missions.get(&(char_id, mission_id)) else {
                            return Err(BridgeRpcError { code: "MissionNotOffered".into(), message: format!("Mission {mission_id} has not been offered to character {char_id}") });
                        };
                        if m_check.state != "Offered"
                            && !(m_check.state == "Accepted"
                                && m_check.solar_system_id == sys_id
                                && can_respawn_missing)
                        {
                            return Err(BridgeRpcError { code: "InvalidMissionState".into(), message: format!("Mission {mission_id} is in state '{}' (system {}), not Offered", m_check.state, m_check.solar_system_id) });
                        }
                        let mut spawned_npcs: Vec<(i64, eve_db::TypeId)> = Vec::new();
                        let mut need_npc = false;
                        if let Some(m) = snap.active_missions.get_mut(&(char_id, mission_id)) {
                            m.state = "Accepted".into();
                            if m.solar_system_id == 0 {
                                m.solar_system_id = sys_id;
                            }
                            need_npc = m.npcs_remaining > 0;
                        }
                        if need_npc {
                            let count = enc_opt.as_ref().map(|e| e.wave_npc_count).unwrap_or(1) as usize;
                            let pool = enc_opt.as_ref().map(|e| e.npc_type_ids.as_slice()).unwrap_or(&[]);
                            let mut ball_ids = Vec::with_capacity(count);
                            for i in 0..count {
                                snap.next_entity_id += 1;
                                let npc_id = 700_000_000_i64.saturating_add(snap.next_entity_id);
                                let tid = if !pool.is_empty() {
                                    pool[i % pool.len()]
                                } else {
                                    resolve_mission_npc_type(ctx)
                                };
                                ball_ids.push(npc_id);
                                spawned_npcs.push((npc_id, tid));
                            }
                            if let Some(m) = snap.active_missions.get_mut(&(char_id, mission_id)) {
                                m.npcs_remaining = count as u32;
                                m.spawned_npc_ball_ids = ball_ids;
                            }
                        }
                        if let Some(mut meta) = load_mission_meta(snap, char_id, mission_id) {
                            meta.accepted_at_unix = Some(now);
                            meta.accept_expiry_unix = Some(now + accept_dur);
                            meta.bonus_expiry_unix = Some(now + bonus_dur);
                            let is_trade = tmpl_opt.as_ref().is_some_and(|t| t.content_template.as_deref().unwrap_or_default().contains("Trade"));
                            let is_courier = !is_trade && tmpl_opt.as_ref().is_some_and(|t| t.is_courier || t.content_template.as_deref().unwrap_or_default().contains("BasicCourier"));
                            if is_courier && meta.granted_courier_item_id.is_none() {
                                let cargo_tid = meta.required_delivery_type_id.ok_or_else(|| BridgeRpcError {
                                    code: "InvalidMissionLocation".into(),
                                    message: format!("Courier mission {mission_id} has no delivery item type specified"),
                                })?;
                                let cargo_qty = meta.required_delivery_qty.max(1);
                                let agent_station = meta.origin_station_id
                                    .or_else(|| ctx.sde.agents.get(&meta.agent_id).and_then(|a| a.station_id))
                                    .or_else(|| {
                                        ctx.sde.agents.get(&meta.agent_id).and_then(|a| a.solar_system_id).and_then(|sys| {
                                            ctx.sde.stations.values().filter(|st| st.solar_system_id == sys).map(|st| st.station_id).min()
                                        })
                                    })
                                    .ok_or_else(|| BridgeRpcError {
                                        code: "InvalidMissionLocation".into(),
                                        message: "Agent has no valid station location".into(),
                                    })?;
                                snap.next_entity_id += 1;
                                let cid = snap.next_entity_id;
                                snap.items.insert(
                                    cid,
                                    InventoryItemRecord {
                                        item_id: cid,
                                        type_id: cargo_tid,
                                        owner_id: char_id,
                                        location_id: agent_station,
                                        flag: InventoryFlag::Hangar,
                                        quantity: cargo_qty,
                                        singleton: false,
                                        version: 1,
                                        ..Default::default()
                                    },
                                );
                                snap.item_custom_names.insert(cid, format!("courier_cargo:{char_id}:{mission_id}"));
                                meta.granted_courier_item_id = Some(cid);
                                meta.granted_courier_qty = cargo_qty;
                            }
                            save_mission_meta(snap, char_id, &meta);
                        }
                        Ok(spawned_npcs)
                    });
                    let spawned_npcs = match res { Ok(v) => v, Err(e) => return Some(Err(e)) };
                    let first_npc_id = spawned_npcs.first().map(|(id, _)| *id);

                    if !spawned_npcs.is_empty() {
                        let origin = if let Some(ship_id) = session.ship_id {
                            handle
                                .get_ball_snapshot(ship_id)
                                .await
                                .ok()
                                .flatten().map(|(_, b)| b.position)
                                .unwrap_or_else(|| {
                                    ctx.sde.solar_systems.get(&sys_id).and_then(|s| {
                                            s.stations
                                                .first().map(|st| st.position + DVec3::new(1_500.0, 0.0, 0.0))
                                        }).unwrap_or(DVec3::ZERO)
                                })
                        } else {
                            DVec3::ZERO
                        };
                        for (idx, (npc_id, tid)) in spawned_npcs.into_iter().enumerate() {
                            let (max_velocity, mass, radius) = ctx.sde.types.get(&tid).map(|inv| {
                                let mv = inv.default_attributes.get(&eve_sde::attr_ids::MAX_VELOCITY).copied().unwrap_or(300.0);
                                (mv, inv.mass.max(1.0), inv.radius.max(1.0))
                            }).unwrap_or((300.0, 100_000.0, 30.0));
                            let pocket_offset = ((npc_id % 40) as f64) * 35.0 + (idx as f64) * 350.0;
                            let _ = handle
                                .spawn_ball_with_combat(
                                    Ball { id: npc_id, type_id: tid, position: origin + DVec3::new(800.0 + pocket_offset, (idx as f64) * 120.0, 0.0), velocity: DVec3::ZERO, max_velocity, inertia: 2.5, mass, radius, speed_fraction: 0.0, mode: MovementMode::Stop, },
                                    ctx.build_type_combat_state(tid),
                                )
                                .await;
                        }
                    }

                    let is_rnd = ctx.db.with_snapshot(|snap| {
                        load_mission_meta(snap, char_id, mission_id)
                            .and_then(|m| ctx.sde.agents.get(&m.agent_id))
                            .is_some_and(is_rnd_agent)
                    });
                    let is_mining_mission = !is_rnd && ctx.sde.missions.get(&mission_id).map_or(false, |m| {
                        let c = m
                            .content_template
                            .as_deref().unwrap_or_default();
                        let is_kill = c.contains("Kill") || c.contains("Encounter");
                        let is_trade = c.contains("Trade");
                        let is_courier = m.is_courier || c.contains("Courier");
                        if is_kill || is_trade || is_courier {
                            return false;
                        }
                        let agent_div = ctx.db.with_snapshot(|snap| {
                            load_mission_meta(snap, char_id, mission_id)
                                .and_then(|meta| ctx.sde.agents.get(&meta.agent_id))
                                .map(|a| a.division_id)
                        }).unwrap_or(0);
                        agent_div == 23 || {
                            let t = m.title.to_lowercase();
                            let c_lower = c.to_lowercase();
                            c_lower.contains("mining")
                                || t.contains("mining")
                                || t.contains("mineral")
                                || t.contains("ore")
                        }
                    });
                    if is_mining_mission {
                        let ore_tid = ctx.db.with_snapshot(|snap| {
                            load_mission_meta(snap, char_id, mission_id)
                                .and_then(|m| m.required_delivery_type_id)
                                .unwrap_or(1230)
                        });
                        let ore_vol = ctx.sde.types.get(&ore_tid).map(|t| t.volume.max(0.01)).unwrap_or(0.1);
                        let origin = if let Some(ship_id) = session.ship_id {
                            handle
                                .get_ball_snapshot(ship_id)
                                .await
                                .ok()
                                .flatten().map(|(_, b)| b.position).unwrap_or(DVec3::ZERO)
                        } else {
                            DVec3::ZERO
                        };
                        let ast_id = 910_000_000 + (mission_id as i64) * 1000 + 1;
                        let ast_pos = origin + DVec3::new(8_000.0, 1_000.0, -5_000.0);
                        ctx.db.with_mut_snapshot(|snap| {
                            snap.asteroids.insert(
                                ast_id,
                                eve_db::AsteroidRecord {
                                    asteroid_id: ast_id,
                                    solar_system_id: sys_id,
                                    belt_id: 0,
                                    type_id: ore_tid,
                                    quantity: 5_000,
                                    initial_quantity: 5_000,
                                    position: (ast_pos.x, ast_pos.y, ast_pos.z),
                                    radius: 50.0,
                                },
                            );
                        });
                        let ast_ball = Ball { id: ast_id, type_id: ore_tid, position: ast_pos, velocity: DVec3::ZERO, max_velocity: 0.0, inertia: 1.0, mass: 50_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop, };
                        let ast_state = crate::system_actor::AsteroidState { asteroid_id: ast_id, belt_id: 0, type_id: ore_tid, initial_units: 5_000, remaining_units: 5_000, unit_volume: ore_vol, depleted: false, respawn_at_tick: None, };
                        let _ = handle.spawn_asteroid(ast_state, ast_ball).await;
                    }

                    let expiry_unix = now + accept_dur;
                    let bonus_expiry_unix = now + bonus_dur;
                    let mission_title = ctx.sde.missions.get(&mission_id).map(|m| m.title.clone())
                        .unwrap_or_else(|| format!("Mission {mission_id}"));
                    let bookmark_id = ctx.db.with_mut_snapshot(|snap| {
                        snap.next_entity_id += 1;
                        let bmid = snap.next_entity_id;
                        snap.bookmarks.insert(
                            bmid,
                            eve_db::BookmarkRecord {
                                bookmark_id: bmid,
                                owner_id: char_id,
                                solar_system_id: sys_id,
                                label: format!("Mission: {mission_title}"),
                                x: 50_000.0,
                                y: 0.0,
                                z: 0.0,
                                item_id: None,
                                memo: "Mission Pocket Destination".into(),
                                folder_id: None,
                                is_shared: false,
                            },
                        );
                        bmid
                    });
                    let dialogue = PyRep::List(vec![
                        PyRep::Tuple(vec![PyRep::Integer(6), PyRep::String("Complete Mission".into())]),
                        PyRep::Tuple(vec![PyRep::Integer(11), PyRep::String("Quit Mission".into())]),
                    ]);
                    let active_agent_id = ctx.db.with_snapshot(|snap| {
                        snap.active_missions.get(&(char_id, mission_id)).map_or(agent_id, |m| m.agent_id)
                    });
                    ctx.notify(NotificationTarget::Character(char_id), "OnAgentMissionChange", "charid", vec![PyRep::String("offer_accepted".into()), PyRep::Integer(active_agent_id)]);
                    Some(Ok(PyRep::keyval([
                        ("missionID", PyRep::Integer(mission_id as i64)),
                        ("contentID", PyRep::Integer(mission_id as i64)),
                        ("agentID", PyRep::Integer(active_agent_id)),
                        ("state", PyRep::String("Accepted".into())),
                        ("missionState", PyRep::Integer(2)),
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
                    let mission_id = match resolve_target_mission_id(ctx, char_id, agent_id, Some("Accepted"), args, _kwargs) { Ok(v) => v, Err(e) => return Some(Err(e)) };
                    let Some(tmpl) = ctx.sde.missions.get(&mission_id).cloned() else {
                        return Some(Err(BridgeRpcError { code: "UnknownMission".into(), message: format!("Mission {mission_id} does not exist in SDE") }));
                    };
                    let is_storyline = is_storyline_template(&tmpl);
                    let active_agent_id = ctx.db.with_snapshot(|snap| {
                        snap.active_missions.get(&(char_id, mission_id)).map_or(agent_id, |m| m.agent_id)
                    });
                    let agent_level = ctx.sde.agents.get(&active_agent_id).map_or(1.0, |a| a.level.max(1) as f64);
                    let storyline_mult = if is_storyline { 3.0 } else { 1.0 };
                    let standing_pct = if tmpl.has_standing_rewards { 1.5 * agent_level * storyline_mult } else { 0.0 };
                    let (storyline_agent_id, _) = resolve_agent_and_corp_for_system(ctx, char_id, sys_id, session.station_id, true);
                    let (isk_reward, lp_reward, bonus_reward) =
                        compute_character_mission_rewards(ctx, char_id, active_agent_id, mission_id, sys_id);
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
                        }));
                    }

                    let res = ctx.db.try_with_mut_snapshot(|snap| {
                        let meta_opt = load_mission_meta(snap, char_id, mission_id);
                        let Some(m) = snap.active_missions.get_mut(&(char_id, mission_id)) else {
                            return Err(BridgeRpcError { code: "MissionNotActive".into(), message: format!("Mission {mission_id} is not active for character {char_id}") });
                        };
                        let is_rnd = meta_opt
                            .as_ref()
                            .and_then(|m| ctx.sde.agents.get(&m.agent_id))
                            .is_some_and(is_rnd_agent);
                        let is_trade = tmpl.content_template.as_deref().unwrap_or_default().contains("Trade");
                        let is_courier = !is_trade && (tmpl.is_courier || tmpl.content_template.as_deref().unwrap_or_default().contains("Courier") || meta_opt.as_ref().and_then(|mt| mt.destination_station_id).is_some());
                        let is_kill = tmpl.content_template.as_deref().unwrap_or_default().contains("Kill")
                            || tmpl.content_template.as_deref().unwrap_or_default().contains("Encounter");
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

                        if m.state != "Accepted" || (m.npcs_remaining > 0 && !is_mining_mission && !is_rnd && !is_courier && !is_trade) {
                            return Err(BridgeRpcError { code: "MissionIncomplete".into(), message: format!("Mission {mission_id} cannot be completed (state='{}', npcs_remaining={})", m.state, m.npcs_remaining) });
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
                        if let Some((req_tid, req_units)) = req_delivery {
                            let current_station = snap.characters.get(&char_id).and_then(|c| c.station_id).or(session.station_id);
                            let agent_station_id = ctx.sde.agents.get(&m.agent_id).and_then(|a| a.station_id)
                                .or(session.station_id);
                            let active_ship_id = snap.characters.get(&char_id).map(|c| c.active_ship_id).or(session.ship_id);

                            if is_courier {
                                let dest_station = meta_opt
                                    .as_ref()
                                    .and_then(|mt| mt.destination_station_id)
                                    .ok_or_else(|| BridgeRpcError {
                                        code: "MissingDestinationStation".into(),
                                        message: format!("Courier mission {mission_id} has no valid destination station"),
                                    })?;
                                if current_station != Some(dest_station) {
                                    return Err(BridgeRpcError {
                                        code: "CourierNotAtDestinationStation".into(),
                                        message: format!("Courier mission {mission_id} requires delivery to destination station {dest_station}, character is currently docked at {current_station:?}"),
                                    });
                                }
                                let courier_tag = format!("courier_cargo:{char_id}:{mission_id}");
                                let custom_names = &snap.item_custom_names;
                                let granted_cid = meta_opt.as_ref().and_then(|mt| mt.granted_courier_item_id);
                                let is_valid_delivery_item = |item: &eve_db::InventoryItemRecord| -> bool {
                                    item.owner_id == char_id
                                        && item.type_id == req_tid
                                        && ((active_ship_id == Some(item.location_id) && item.flag == InventoryFlag::Cargo)
                                            || (item.location_id == dest_station && item.flag == InventoryFlag::Hangar))
                                        && custom_names.get(&item.item_id).map_or(false, |s| s == &courier_tag)
                                        && granted_cid.is_some_and(|gid| item.item_id >= gid)
                                };
                                let found_qty: i32 = snap
                                    .items
                                    .values()
                                    .filter(|it| is_valid_delivery_item(it))
                                    .map(|it| it.quantity)
                                    .sum();
                                if found_qty < req_units {
                                    return Err(BridgeRpcError { code: "MissionIncomplete".into(), message: format!("Courier mission {mission_id} requires delivering {req_units} units of type {req_tid} from ship cargo at destination station, character only has {found_qty}") });
                                }
                                let mut rem = req_units;
                                let mut to_remove = Vec::new();
                                let matching_ids: Vec<i64> = snap.items.values().filter(|it| is_valid_delivery_item(it)).map(|it| it.item_id).collect();
                                for iid in matching_ids {
                                    if rem <= 0 { break; }
                                    if let Some(item) = snap.items.get_mut(&iid) {
                                        if item.quantity <= rem { rem -= item.quantity; to_remove.push(iid); }
                                        else { item.quantity -= rem; rem = 0; }
                                    }
                                }
                                for iid in to_remove { snap.items.remove(&iid); snap.item_custom_names.remove(&iid); }
                            } else {
                                if let Some(ast_id) = agent_station_id {
                                    if current_station != Some(ast_id) {
                                        return Err(BridgeRpcError {
                                            code: "MissionIncomplete".into(),
                                            message: format!("Mission {mission_id} delivery requires character to be docked at agent station {ast_id}, currently at {current_station:?}"),
                                        });
                                    }
                                }
                                let is_valid_delivery_item = |item: &eve_db::InventoryItemRecord| -> bool {
                                    item.owner_id == char_id
                                        && item.type_id == req_tid
                                        && ((item.flag == InventoryFlag::Hangar
                                            && agent_station_id.is_none_or(|st_id| item.location_id == st_id))
                                            || ((is_mining_mission || req_tid == 34)
                                                && active_ship_id == Some(item.location_id)
                                                && matches!(item.flag, InventoryFlag::SpecializedOreHold | InventoryFlag::Cargo)))
                                };
                                let found_qty: i32 = snap
                                    .items
                                    .values()
                                    .filter(|it| is_valid_delivery_item(it))
                                    .map(|it| it.quantity)
                                    .sum();
                                if found_qty < req_units {
                                    return Err(BridgeRpcError { code: "MissionIncomplete".into(), message: format!("Mission {mission_id} requires delivering {req_units} units of type {req_tid} in agent station hangar, character only has {found_qty}") });
                                }
                                let mut rem = req_units;
                                let mut to_remove = Vec::new();
                                let matching_ids: Vec<i64> = snap
                                    .items
                                    .values()
                                    .filter(|it| is_valid_delivery_item(it))
                                    .map(|it| it.item_id)
                                    .collect();
                                for iid in matching_ids {
                                    if rem <= 0 {
                                        break;
                                    }
                                    if let Some(item) = snap.items.get_mut(&iid) {
                                        if item.quantity <= rem {
                                            rem -= item.quantity;
                                            to_remove.push(iid);
                                        } else {
                                            item.quantity -= rem;
                                            rem = 0;
                                        }
                                    }
                                }
                                for iid in to_remove {
                                    snap.items.remove(&iid);
                                }
                            }
                        }

                        let m_agent_id = m.agent_id;
                        m.state = "Completed".into();
                        reclaim_courier_cargo(snap, char_id, mission_id);

                        let bonus_granted = meta_opt
                            .as_ref().and_then(|mt| mt.bonus_expiry_unix)
                            .is_none_or(|bexp| now <= bexp);
                        let bonus_paid = if bonus_granted { bonus_reward } else { 0 };

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
                            *lp_entry = lp_entry.saturating_add(lp_reward.max(0));
                        }
                        crate::services::lp_store::record_lp_reward_log(
                            snap,
                            char_id,
                            corp_id,
                            lp_reward.max(0),
                            format!("Mission {mission_id} (agent {m_agent_id})"),
                            now,
                        );

                        let is_rnd = ctx.sde.agents.get(&m_agent_id).is_some_and(is_rnd_agent);
                        if is_rnd {
                            let mut research = load_rnd_research_entries(snap, char_id);
                            if let Some(re) = research.iter_mut().find(|r| r.agent_id == m_agent_id) {
                                re.initial_points += re.points_per_day;
                            }
                            save_rnd_research_entries(snap, char_id, &research);
                        }

                        let is_special = is_storyline
                            || ctx.sde.agents.get(&m_agent_id)
                                .is_some_and(|a| matches!(a.agent_type_id, 5 | 8 | 10));
                        let (new_st, st_changes) = apply_mission_completion_standings(ctx, snap, char_id, m_agent_id, corp_id, standing_pct, is_special);

                        if ctx.sde.agents.get(&m_agent_id)
                            .is_some_and(|a| a.agent_type_id == 10)
                        {
                            let (arc_id, fac_id) = resolve_epic_arc_id_for_agent(ctx, m_agent_id);
                            let mut arcs = load_epic_arc_states(snap, char_id);
                            if let Some(arc) = arcs.iter_mut().find(|a| a.epic_arc_id == arc_id) {
                                if !arc.completed_missions.contains(&mission_id) {
                                    arc.completed_missions.push(mission_id);
                                }
                                arc.completed_at_unix = Some(now);
                                arc.cooldown_until_unix = Some(now + 7_776_000);
                            } else {
                                arcs.push(EpicArcStateEntry { epic_arc_id: arc_id, agent_id: m_agent_id, faction_id: fac_id, chapter: 3, completed_missions: vec![mission_id], chosen_branch: None, completed_at_unix: Some(now), cooldown_until_unix: Some(now + 7_776_000), });
                            }
                            save_epic_arc_states(snap, char_id, &arcs);
                        }

                        let (t, storyline) = if (!is_storyline && tmpl.has_standing_rewards) || is_rnd {
                            let count = snap.mission_tally.entry(char_id).or_insert(0);
                            *count += 1;
                            let t = *count;
                            (t, t % 16 == 0)
                        } else {
                            let t = snap.mission_tally.get(&char_id).copied().unwrap_or(0);
                            (t, false)
                        };
                        if storyline {
                            if let Some(storyline_tmpl) = ctx.sde.missions
                                .values().filter(|sm| {
                                    is_storyline_template(sm)
                                        && !snap.active_missions.contains_key(&(char_id, sm.mission_id))
                                }).min_by_key(|sm| sm.mission_id)
                            {
                                let storyline_mission_id = storyline_tmpl.mission_id;
                                let sl_agent = ctx.sde.agents.get(&storyline_agent_id);
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
                                    let enc = resolve_security_mission_encounter(ctx, storyline_tmpl, sa, sys_id);
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
                                    },
                                );
                                snap.active_missions
                                    .entry((char_id, storyline_mission_id))
                                    .or_insert(ActiveMissionRecord { mission_id: storyline_mission_id, character_id: char_id, agent_id: storyline_agent_id, solar_system_id: sys_id, state: "Offered".into(), current_room_id: 1, npcs_remaining: sl_npcs, spawned_npc_ball_ids: Vec::new() });
                            }
                        }
                        Ok((new_st, t, storyline, bonus_paid, st_changes))
                    });

                    match res {
                        Ok((new_standing, tally, storyline_offered, bonus_paid, st_changes)) => {
                            notify_standing_changes(ctx, char_id, &st_changes);
                            ctx.notify(NotificationTarget::Character(char_id), "OnAgentMissionObjectivesCompleted", "charid", vec![PyRep::Integer(active_agent_id)]);
                            ctx.notify(NotificationTarget::Character(char_id), "OnAgentMissionChange", "charid", vec![PyRep::String("completed".into()), PyRep::Integer(active_agent_id)]);
                            Some(Ok(PyRep::keyval([
                                ("missionID", PyRep::Integer(mission_id as i64)),
                                ("contentID", PyRep::Integer(mission_id as i64)),
                                ("state", PyRep::String("Completed".into())),
                                ("missionState", PyRep::Integer(4)),
                                ("iskReward", PyRep::Integer(isk_reward)),
                                ("bonusReward", PyRep::Integer(bonus_paid)),
                                ("lpReward", PyRep::Integer(lp_reward)),
                                ("newStanding", PyRep::Real(new_standing)),
                                ("missionTally", PyRep::Integer(tally as i64)),
                                ("storylineTriggered", PyRep::Bool(storyline_offered)),
                            ])))
                        }
                        Err(e) => Some(Err(e)),
                    }
                }

                // 9 = Decline, 10 = Defer, 11 = Quit
                9..=11 => {
                    let expected_state = if action_id == 11 { Some("Accepted") } else { Some("Offered") };
                    let mission_id = match resolve_target_mission_id(ctx, char_id, agent_id, expected_state, args, _kwargs) { Ok(v) => v, Err(e) => return Some(Err(e)) };
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
                            Some(Ok(PyRep::keyval([
                                ("missionID", PyRep::Integer(mission_id as i64)),
                                ("contentID", PyRep::Integer(mission_id as i64)),
                                ("state", PyRep::String(new_state)),
                                ("missionState", PyRep::Integer(mission_state)),
                                ("standingPenaltyApplied", PyRep::Bool(penalty_applied)),
                                ("dungeonCleaned", PyRep::Bool(was_accepted)),
                            ])))
                        }
                        Err(e) => Some(Err(e)),
                    }
                }

                _ => Some(Err(BridgeRpcError { code: "InvalidDialogueAction".into(), message: format!("Unknown agent dialogue action {action_id}") })),
            }
        }

        ("agentMgr", "RemoveOfferFromJournal") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError { code: "NoSolarSystem".into(), message: "Session has no active solar system".into() }));
            };
            let (agent_id, _) =
                resolve_target_agent_id(ctx, session, char_id, sys_id, args, _kwargs);
            let mission_id = match parse_mission_id(args).or_else(|_| {
                resolve_target_mission_id(ctx, char_id, agent_id, None, args, _kwargs)
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
