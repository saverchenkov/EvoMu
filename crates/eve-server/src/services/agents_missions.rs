//! `agents_missions` — Domain 9: Agent Dialogue, Missions, 16-Mission Storyline Tally, Epic Arcs, & AIR NPE (`§3.8`).

// parity: plan:E-5
// parity: feat:pve.mission-accept
// parity: feat:pve.mission-complete, feat:pve.mission-decline, feat:pve.mission-quit
// parity: feat:pve.mission-standing-gate, feat:pve.mission-faction-standing-loss

#![forbid(unsafe_code)]

#[path = "agents_standings_arcs.rs"]
pub mod agents_standings_arcs;
pub use agents_standings_arcs::*;

use crate::formulas::compute_mission_reward_with_skills;
use crate::router::ServerContext;
use eve_db::{
    ActiveMissionRecord, InventoryFlag, InventoryItemRecord, IskCents, StandingTransactionRecord,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;
use std::collections::HashMap;

fn parse_mission_id(args: &[PyRep]) -> Result<u32, BridgeRpcError> {
    let Some(raw) = args.first().and_then(|a| a.as_i64()) else {
        return Err(BridgeRpcError { code: "InvalidMissionId".into(), message: "mission_id argument is required".into() });
    };
    u32::try_from(raw).map_err(|_| BridgeRpcError {
        code: "InvalidMissionId".into(),
        message: format!("mission_id {raw} is out of valid u32 range"),
    })
}

fn is_storyline_template(tmpl: &eve_sde::MissionTemplate) -> bool {
    tmpl.content_template
        .as_deref()
        .is_some_and(|ct| ct.contains("Storyline"))
}

fn is_storyline_agent_type(agent_type_id: u32) -> bool {
    matches!(agent_type_id, 6 | 7)
}

fn is_unsupported_direct_agent_type(agent_type_id: u32) -> bool {
    // EVE agentType enum (EVE_Agent.h:53): 11 = Aura, 12 = CareerAgent (excluded from implicit default selection)
    matches!(agent_type_id, 11 | 12)
}

fn mission_matches_agent_pool(
    tmpl: &eve_sde::MissionTemplate,
    agent: &eve_sde::AgentInfo,
) -> bool {
    if is_storyline_template(tmpl) {
        return false;
    }
    let ct = tmpl.content_template.as_deref().unwrap_or_default();
    match agent.agent_type_id {
        10 => ct.contains("EpicArc"),
        4 => ct.contains("Research"),
        13 => ct.contains("Heraldry") || ct.contains("BasicKill"),
        12 => ct.contains("BasicKill") || ct.contains("BasicCourier") || ct.contains("BasicMining"),
        5 | 8 => {
            ct.contains("BasicKill") || ct.contains("BasicCourier") || ct.contains("BasicTrade")
        }
        11 => false,
        _ => match agent.division_id {
            18 => ct.contains("Research"),
            22 => ct.contains("BasicCourier"),
            23 => ct.contains("BasicMining") || tmpl.title.to_lowercase().contains("mining"),
            24 => ct.contains("BasicKill"),
            37 => ct.contains("Heraldry") || ct.contains("BasicKill"),
            _ => ct.contains("BasicKill") || ct.contains("BasicCourier"),
        },
    }
}

fn resolve_agent_and_corp_for_system(
    ctx: &ServerContext,
    char_id: i64,
    sys_id: u32,
    station_id: Option<i64>,
    is_storyline: bool,
) -> (i64, i64) {
    let preferred_station = station_id.or_else(|| {
        ctx.db
            .get_character(char_id)
            .ok().map(|ch| ch.home_station_id)
    });
    let cur_sys = ctx.sde.solar_systems.get(&sys_id);
    let cur_constellation = cur_sys.map(|s| s.constellation_id);
    let cur_region = cur_sys.map(|s| s.region_id);
    let primary_corp = cur_sys.and_then(|s| {
            preferred_station.and_then(|sid| s.stations.iter().find(|st| st.station_id == sid)).or_else(|| s.stations.first())
        }).map(|st| st.owner_corporation_id).or_else(|| ctx.sde.npc_corporations.keys().copied().min()).unwrap_or(0);

    let agent = if is_storyline {
        ctx.sde.agents
            .values().filter(|a| a.solar_system_id == Some(sys_id) && is_storyline_agent_type(a.agent_type_id)).min_by_key(|a| a.agent_id).or_else(|| {
                ctx.sde.agents
                    .values().filter(|a| {
                        a.corporation_id == primary_corp && is_storyline_agent_type(a.agent_type_id)
                    }).min_by_key(|a| a.agent_id)
            }).or_else(|| {
                ctx.sde.agents
                    .values().filter(|a| {
                        is_storyline_agent_type(a.agent_type_id)
                            && a.solar_system_id.and_then(|s| ctx.sde.solar_systems.get(&s)).map(|s| s.constellation_id)
                                == cur_constellation
                    }).min_by_key(|a| a.agent_id)
            }).or_else(|| {
                ctx.sde.agents
                    .values().filter(|a| {
                        is_storyline_agent_type(a.agent_type_id)
                            && a.solar_system_id.and_then(|s| ctx.sde.solar_systems.get(&s)).map(|s| s.region_id)
                                == cur_region
                    }).min_by_key(|a| a.agent_id)
            })
    } else {
        let is_eligible_regular = |a: &eve_sde::AgentInfo| -> bool {
            !is_storyline_agent_type(a.agent_type_id)
                && !is_unsupported_direct_agent_type(a.agent_type_id)
        };
        station_id.and_then(|sid| {
                ctx.sde.agents
                    .values().filter(|a| {
                        a.solar_system_id == Some(sys_id)
                            && a.station_id == Some(sid)
                            && is_eligible_regular(a)
                    }).min_by_key(|a| (a.level, a.agent_id))
            }).or_else(|| {
                ctx.sde.agents
                    .values().filter(|a| {
                        a.solar_system_id == Some(sys_id)
                            && a.corporation_id == primary_corp
                            && is_eligible_regular(a)
                    }).min_by_key(|a| (a.level, a.agent_id))
            }).or_else(|| {
                ctx.sde.agents
                    .values().filter(|a| a.solar_system_id == Some(sys_id) && is_eligible_regular(a)).min_by_key(|a| (a.level, a.agent_id))
            })
    };

    match agent {
        Some(a) => (a.agent_id, a.corporation_id),
        None => (0, primary_corp),
    }
}

fn resolve_mission_npc_type(ctx: &ServerContext) -> u32 {
    let is_l1_frigate_rat = |inv: &eve_sde::InvType| -> bool {
        if !inv
            .default_attributes
            .contains_key(&eve_sde::attr_ids::ENTITY_KILL_BOUNTY)
            || inv.radius < 25.0
        {
            return false;
        }
        let shield = inv
            .default_attributes.get(&eve_sde::attr_ids::SHIELD_CAPACITY).copied().unwrap_or(0.0);
        let armor = inv
            .default_attributes.get(&eve_sde::attr_ids::ARMOR_HP).copied().unwrap_or(0.0);
        let hull = inv
            .default_attributes.get(&eve_sde::attr_ids::HP_STRUCT).copied().unwrap_or(0.0);
        (50.0..=450.0).contains(&(shield + armor + hull))
    };

    let mut group_ids: Vec<u32> = ctx.sde.npc_spawn_groups
        .values().filter(|sg| sg.ship_class == 1).map(|sg| sg.group_id)
        .collect();
    group_ids.sort_unstable();
    for gid in group_ids {
        if let Some(sg) = ctx.sde.npc_spawn_groups.get(&gid) {
            for &tid in &sg.npc_type_ids {
                if let Some(inv) = ctx.sde.types.get(&tid) {
                    if is_l1_frigate_rat(inv) {
                        return tid;
                    }
                }
            }
        }
    }
    ctx.sde.types
        .values().filter(|t| t.category_id == 11 && is_l1_frigate_rat(t)).map(|t| t.type_id)
        .min().unwrap_or(0)
}

fn compute_character_mission_rewards(
    ctx: &ServerContext,
    char_id: i64,
    mission_id: u32,
) -> (i64, i64, i64) {
    let tmpl = ctx.sde.missions.get(&mission_id);
    let (is_alpha, neg_lvl, sec_conn_lvl) = ctx.db.with_snapshot(|snap| {
        let alpha = snap.characters.get(&char_id)
            .is_some_and(|c| c.clone_grade <= 1);
        let sk = snap.skills.get(&char_id);
        let neg = sk.and_then(|m| m.get(&3356)).map_or(0, |s| s.level);
        let sec = sk.and_then(|m| m.get(&3893)).map_or(0, |s| s.level);
        (alpha, neg, sec)
    });
    let base_lp = tmpl.map_or(100, |t| {
        if is_alpha && t.lp_alpha > 0 { t.lp_alpha } else { t.lp_omega.max(100) }
    });
    let base_isk = base_lp.saturating_mul(1_000);
    let (isk_reward, lp_reward) =
        compute_mission_reward_with_skills(base_isk, base_lp, neg_lvl, sec_conn_lvl, 1);
    (isk_reward, lp_reward, isk_reward / 2)
}

fn resolve_target_agent_id(
    ctx: &ServerContext,
    session: &SessionState,
    char_id: i64,
    sys_id: u32,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> (i64, bool) {
    if let Some(first_val) = args.first().and_then(|a| a.as_i64()) {
        if ctx.sde.agents.contains_key(&first_val) {
            return (first_val, true);
        }
    }
    if let Some(second_val) = args.get(1).and_then(|a| a.as_i64()) {
        if ctx.sde.agents.contains_key(&second_val) {
            return (second_val, true);
        }
    }
    if let Some(bound_agent) = ctx.bound_objects.read().ok().and_then(|map| {
        map.values().filter(|b| {
                b.session_id == session.session_id && b.service == "agentMgr" && b.object_id > 0
            })
            .max_by_key(|b| b.bound_id).map(|b| b.object_id)
    }) {
        return (bound_agent, true);
    }
    let (default_aid, _) =
        resolve_agent_and_corp_for_system(ctx, char_id, sys_id, session.station_id, false);
    (default_aid, false)
}

fn resolve_target_mission_id(
    ctx: &ServerContext,
    char_id: i64,
    agent_id: i64,
    expected_state: Option<&str>,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Result<u32, BridgeRpcError> {
    if let Some(second_arg) = args.get(1).and_then(|a| a.as_i64()) {
        if let Ok(mid) = u32::try_from(second_arg) {
            if ctx.sde.missions.contains_key(&mid)
                || ctx
                    .db
                    .with_snapshot(|snap| snap.active_missions.contains_key(&(char_id, mid)))
            {
                return Ok(mid);
            }
        }
    }
    if let Some(first_arg) = args.first().and_then(|a| a.as_i64()) {
        if first_arg > 19 {
            if let Ok(mid) = u32::try_from(first_arg) {
                if ctx.sde.missions.contains_key(&mid)
                    || ctx
                        .db
                        .with_snapshot(|snap| snap.active_missions.contains_key(&(char_id, mid)))
                {
                    return Ok(mid);
                }
            }
        }
    }
    let found = ctx.db.with_snapshot(|snap| {
        snap.active_missions
            .values().filter(|m| {
                m.character_id == char_id
                    && (agent_id == 0 || m.agent_id == agent_id)
                    && expected_state.is_none_or(|st| m.state == st)
            }).map(|m| m.mission_id)
            .min().or_else(|| {
                snap.active_missions
                    .values().filter(|m| {
                        m.character_id == char_id && expected_state.is_none_or(|st| m.state == st)
                    }).map(|m| m.mission_id)
                    .min()
            }).or_else(|| {
                snap.active_missions
                    .values().filter(|m| {
                        m.character_id == char_id && (agent_id == 0 || m.agent_id == agent_id)
                    }).map(|m| m.mission_id)
                    .min()
            })
    });
    found.ok_or_else(|| BridgeRpcError {
        code: "MissionNotActive".into(),
        message: format!(
            "No matching active mission found for character {char_id} and agent {agent_id}"
        ),
    })
}

async fn abandon_or_decline_mission(
    ctx: &ServerContext,
    session: &SessionState,
    char_id: i64,
    mission_id: u32,
) -> Result<(String, bool, bool), BridgeRpcError> {
    let (m_sys_id, ball_ids, new_state, penalty_applied, was_accepted) =
        ctx.db.try_with_mut_snapshot(|snap| {
            let Some(m) = snap.active_missions.get_mut(&(char_id, mission_id)) else {
                return Err(BridgeRpcError { code: "MissionNotActive".into(), message: format!("Mission {mission_id} is not active or offered for character {char_id}") });
            };
            if !matches!(m.state.as_str(), "Offered" | "Accepted") {
                return Err(BridgeRpcError { code: "MissionNotActive".into(), message: format!("Mission {mission_id} is in terminal state '{}' and cannot be abandoned", m.state) });
            }
            let was_accepted = m.state == "Accepted";
            let m_sys_id = m.solar_system_id;
            let m_agent_id = m.agent_id;
            let now = ctx.now_unix();
            let mut standing_penalty_applied = false;

            m.state = if was_accepted { "Quit".into() } else { "Declined".into() };
            m.npcs_remaining = 0;
            let ball_ids = std::mem::take(&mut m.spawned_npc_ball_ids);
            let corp_id = ctx.sde.agents.get(&m_agent_id).map(|a| a.corporation_id)
                .unwrap_or_else(|| {
                    resolve_agent_and_corp_for_system(ctx, char_id, m_sys_id, session.station_id, false)
                    .1
                });

            if was_accepted {
                if ctx.sde.missions.contains_key(&mission_id) {
                    let mut actual_delta = None;
                    if let Some(ch) = snap.characters.get_mut(&char_id) {
                        let cur = ch.standings.get(&corp_id).copied().unwrap_or(0.0);
                        let next = (cur - 0.1).max(-10.0);
                        ch.standings.insert(corp_id, next);
                        let ag_cur = ch.standings.get(&m_agent_id).copied().unwrap_or(0.0);
                        ch.standings.insert(m_agent_id, (ag_cur - 0.1).max(-10.0));
                        actual_delta = Some(if cur - 0.1 >= -10.0 { -0.1 } else { -10.0 - cur });
                    }
                    if let Some(delta) = actual_delta {
                        snap.next_entity_id += 1;
                        let ev_id = snap.next_entity_id;
                        snap.standing_transactions
                            .entry(char_id)
                            .or_default()
                            .push(StandingTransactionRecord { event_id: ev_id, from_id: corp_id, to_id: char_id, modification: delta, event_type_id: 83 });
                    }
                    standing_penalty_applied = true;
                }
                snap.bookmarks.retain(|_, b| {
                    !(b.owner_id == char_id
                        && b.solar_system_id == m_sys_id
                        && b.memo.contains("Mission Pocket"))
                });
            } else {
                let last_decline = snap.last_mission_decline_times.get(&char_id).copied().unwrap_or(0);
                if now.saturating_sub(last_decline) < 14_400 {
                    let mut actual_delta = None;
                    if let Some(ch) = snap.characters.get_mut(&char_id) {
                        let cur = ch.standings.get(&corp_id).copied().unwrap_or(0.0);
                        let next = (cur - 0.05).max(-10.0);
                        ch.standings.insert(corp_id, next);
                        let ag_cur = ch.standings.get(&m_agent_id).copied().unwrap_or(0.0);
                        ch.standings.insert(m_agent_id, (ag_cur - 0.05).max(-10.0));
                        actual_delta = Some(if cur - 0.05 >= -10.0 { -0.05 } else { -10.0 - cur });
                    }
                    if let Some(delta) = actual_delta {
                        snap.next_entity_id += 1;
                        let ev_id = snap.next_entity_id;
                        snap.standing_transactions
                            .entry(char_id)
                            .or_default()
                            .push(StandingTransactionRecord { event_id: ev_id, from_id: corp_id, to_id: char_id, modification: delta, event_type_id: 83 });
                    }
                    standing_penalty_applied = true;
                }
                snap.last_mission_decline_times.insert(char_id, now);
            }
            Ok((
                m_sys_id,
                ball_ids,
                m.state.clone(),
                standing_penalty_applied,
                was_accepted,
            ))
        })?;

    if m_sys_id > 0 && !ball_ids.is_empty() {
        let handle = ctx.systems.get_or_spawn(m_sys_id);
        for bid in ball_ids {
            let _ = handle.remove_ball(bid).await;
        }
    }
    Ok((new_state, penalty_applied, was_accepted))
}

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
            let (agent_id, _) =
                resolve_target_agent_id(ctx, session, char_id, sys_id, args, _kwargs);
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
                        } else if st == "Accepted"
                            && meta.accept_expiry_unix.is_some_and(|exp| exp > 0 && now > exp)
                        {
                            if let Some(m) = snap.active_missions.get_mut(&(char_id, mid)) {
                                m.state = "Failed".into();
                                m.npcs_remaining = 0;
                            }
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
            let ct = tmpl.and_then(|t| t.content_template.as_deref()).unwrap_or_default();
            let is_mining = tmpl.is_some_and(|t| t.title.to_lowercase().contains("mining"))
                || ct.contains("BasicMining");
            let objectives = if is_mining {
                "Mine 500 units of Veldspar and deliver to the agent station hangar."
            } else if ct.contains("BasicCourier") {
                "Transport 10 units of Mission Cargo (type 3683) to the station hangar."
            } else if ct.contains("BasicTrade") { "Acquire and deliver 10 units of trade goods (type 3683) to the agent station hangar." } else { "Fly to encounter location, neutralize hostile pirate vessel, and report back." };
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
                compute_character_mission_rewards(ctx, char_id, mission_id);
            let req_type = meta
                .as_ref().and_then(|mt| mt.required_delivery_type_id).map(|t| PyRep::Integer(t as i64)).unwrap_or(PyRep::None);
            let req_qty = meta.as_ref().map_or(0_i64, |mt| mt.required_delivery_qty as i64);
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
                            return Ok((mid, title));
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
                        let is_mining = ct.contains("BasicMining")
                            || tmpl.title.to_lowercase().contains("mining")
                            || agent_info.division_id == 23;
                        let is_courier = ct.contains("BasicCourier");
                        let is_trade = ct.contains("BasicTrade");
                        let is_data_center = agent_info.agent_type_id == 5;
                        let is_heraldry = ct.contains("Heraldry");
                        let npcs_remaining =
                            if is_mining || is_courier || is_trade || is_data_center || is_heraldry { 0 } else { 1 };
                        let (req_tid, req_qty) = if is_data_center {
                            (Some(17202), 1)
                        } else if is_courier || is_trade { (Some(3683), 10) } else { (None, 0) };
                        let is_burner = title.contains("Anomic")
                            || (agent_info.level >= 4 && agent_info.division_id == 24);
                        save_mission_meta(
                            snap,
                            char_id,
                            &MissionTimingAndCargoMeta { mission_id, agent_id, offered_at_unix: now, offer_expiry_unix: now + offer_dur, accepted_at_unix: None, accept_expiry_unix: None, bonus_expiry_unix: None, completed_at_unix: None, required_delivery_type_id: req_tid, required_delivery_qty: req_qty, granted_courier_item_id: None, is_anomic_burner: is_burner, chosen_branch: None, },
                        );
                        snap.active_missions.insert(
                            (char_id, mission_id),
                            ActiveMissionRecord { mission_id, character_id: char_id, agent_id, solar_system_id: sys_id, state: "Offered".into(), current_room_id: 1, npcs_remaining, spawned_npc_ball_ids: Vec::new() },
                        );
                        Ok((mission_id, title))
                    });

                    match res {
                        Ok((mission_id, title)) => {
                            let tmpl = ctx.sde.missions.get(&mission_id);
                            let briefing = tmpl.map(|t| {
                                    format!(
                                        "Agent {} operation: {}. Execute objectives in system {} and report back.",
                                        agent_id, t.title, sys_id
                                    )
                                })
                                .unwrap_or_else(|| format!("Mission assignment {mission_id} for agent {agent_id}."));
                            let is_mining = tmpl.is_some_and(|t| {
                                t.title.to_lowercase().contains("mining")
                                    || t.content_template
                                        .as_deref().unwrap_or_default()
                                        .contains("BasicMining")
                            }) || agent_info.division_id == 23;
                            let objectives = if is_mining { "Mine 500 units of Veldspar and deliver to the agent station hangar." } else { "Fly to encounter location, neutralize hostile pirate vessel, and report back." };
                            let expiry_unix = now + offer_dur;
                            let dialogue = PyRep::List(vec![
                                PyRep::Tuple(vec![PyRep::Integer(3), PyRep::String("Accept".into())]),
                                PyRep::Tuple(vec![PyRep::Integer(9), PyRep::String("Decline".into())]),
                            ]);
                            let (isk_reward, lp_reward, bonus_reward) =
                                compute_character_mission_rewards(ctx, char_id, mission_id);

                            Some(Ok(PyRep::keyval([
                                ("missionID", PyRep::Integer(mission_id as i64)),
                                ("contentID", PyRep::Integer(mission_id as i64)),
                                ("agentID", PyRep::Integer(agent_id)),
                                ("title", PyRep::String(title)),
                                ("state", PyRep::String("Offered".into())),
                                ("missionState", PyRep::Integer(1)),
                                ("briefing", PyRep::String(briefing.clone())),
                                ("agentSays", PyRep::String(briefing)),
                                ("objectives", PyRep::String(objectives.into())),
                                ("dialogue", dialogue),
                                ("iskReward", PyRep::Integer(isk_reward)),
                                ("lpReward", PyRep::Integer(lp_reward)),
                                ("bonusReward", PyRep::Integer(bonus_reward)),
                                ("expiryUnix", PyRep::Integer(expiry_unix as i64)),
                            ])))
                        }
                        Err(e) => Some(Err(e)),
                    }
                }

                // 3 = Accept, 4 = AcceptChoice, 5 = AcceptRemotely
                3..=5 => {
                    let mission_id = match resolve_target_mission_id(ctx, char_id, agent_id, Some("Offered"), args, _kwargs) { Ok(v) => v, Err(e) => return Some(Err(e)) };
                    let npc_type_id = resolve_mission_npc_type(ctx);
                    let Some(npc_inv) = ctx.sde.types.get(&npc_type_id) else {
                        return Some(Err(BridgeRpcError { code: "MissingNpcSdeType".into(), message: format!("Mission NPC type {npc_type_id} missing from SDE") }));
                    };
                    let Some(&max_velocity) = npc_inv
                        .default_attributes.get(&eve_sde::attr_ids::MAX_VELOCITY)
                    else {
                        return Some(Err(BridgeRpcError { code: "MissingNpcSdeAttributes".into(), message: format!("Mission NPC type {npc_type_id} missing maxVelocity attribute in SDE") }));
                    };
                    let mass = npc_inv.mass.max(1.0);
                    let radius = npc_inv.radius.max(1.0);
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
                    let accept_dur = 86_400_u64;
                    let bonus_dur = 14_400_u64;

                    let res = ctx.db.try_with_mut_snapshot(|snap| {
                        if let Some(meta) = load_mission_meta(snap, char_id, mission_id) {
                            if meta.offer_expiry_unix > 0 && now > meta.offer_expiry_unix {
                                if let Some(m) = snap.active_missions.get_mut(&(char_id, mission_id))
                                {
                                    m.state = "Expired".into();
                                }
                                return Err(BridgeRpcError { code: "MissionOfferExpired".into(), message: format!("Mission {mission_id} offer has expired") });
                            }
                        }
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
                        let mut npc_id_opt = None;
                        let mut need_npc = false;
                        if let Some(m) = snap.active_missions.get_mut(&(char_id, mission_id)) {
                            m.state = "Accepted".into();
                            m.solar_system_id = sys_id;
                            need_npc = m.npcs_remaining > 0;
                        }
                        if need_npc {
                            snap.next_entity_id += 1;
                            let npc_id = 700_000_000_i64.saturating_add(snap.next_entity_id);
                            if let Some(m) = snap.active_missions.get_mut(&(char_id, mission_id)) {
                                m.spawned_npc_ball_ids = vec![npc_id];
                            }
                            npc_id_opt = Some(npc_id);
                        }
                        if let Some(mut meta) = load_mission_meta(snap, char_id, mission_id) {
                            meta.accepted_at_unix = Some(now);
                            meta.accept_expiry_unix = Some(now + accept_dur);
                            meta.bonus_expiry_unix = Some(now + bonus_dur);
                            let is_courier = ctx.sde.missions.get(&mission_id).and_then(|t| t.content_template.as_deref())
                                .is_some_and(|ct| ct.contains("BasicCourier"));
                            if is_courier && meta.granted_courier_item_id.is_none() {
                                snap.next_entity_id += 1;
                                let cid = snap.next_entity_id;
                                let loc = session.ship_id.unwrap_or(char_id);
                                snap.items.insert(
                                    cid,
                                    InventoryItemRecord { item_id: cid, type_id: 3683, owner_id: char_id, location_id: loc, flag: InventoryFlag::Cargo, quantity: meta.required_delivery_qty.max(10), singleton: false, version: 1, ..Default::default() },
                                );
                                meta.granted_courier_item_id = Some(cid);
                            }
                            save_mission_meta(snap, char_id, &meta);
                        }
                        Ok(npc_id_opt)
                    });
                    let npc_id_opt = match res { Ok(v) => v, Err(e) => return Some(Err(e)) };

                    if let Some(npc_id) = npc_id_opt {
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
                        let pocket_offset = ((npc_id % 40) as f64) * 35.0;
                        let _ = handle
                            .spawn_ball_with_combat(
                                Ball { id: npc_id, type_id: npc_type_id, position: origin + DVec3::new(800.0 + pocket_offset, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity, inertia: 2.5, mass, radius, speed_fraction: 0.0, mode: MovementMode::Stop, },
                                ctx.build_type_combat_state(npc_type_id),
                            )
                            .await;
                    }

                    let is_mining_mission = ctx.sde.missions.get(&mission_id).is_some_and(|m| {
                        let t = m.title.to_lowercase();
                        let c = m
                            .content_template
                            .as_deref().unwrap_or_default()
                            .to_lowercase();
                        c.contains("mining")
                            || t.contains("mining")
                            || t.contains("mineral")
                            || t.contains("ore")
                    });
                    if is_mining_mission {
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
                                    type_id: 1230,
                                    quantity: 5_000,
                                    initial_quantity: 5_000,
                                    position: (ast_pos.x, ast_pos.y, ast_pos.z),
                                    radius: 50.0,
                                },
                            );
                        });
                        let ast_ball = Ball { id: ast_id, type_id: 1230, position: ast_pos, velocity: DVec3::ZERO, max_velocity: 0.0, inertia: 1.0, mass: 50_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop, };
                        let ast_state = crate::system_actor::AsteroidState { asteroid_id: ast_id, belt_id: 0, type_id: 1230, initial_units: 5_000, remaining_units: 5_000, unit_volume: 0.1, depleted: false, respawn_at_tick: None, };
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
                    Some(Ok(PyRep::keyval([
                        ("missionID", PyRep::Integer(mission_id as i64)),
                        ("contentID", PyRep::Integer(mission_id as i64)),
                        ("agentID", PyRep::Integer(agent_id)),
                        ("state", PyRep::String("Accepted".into())),
                        ("missionState", PyRep::Integer(2)),
                        ("dialogue", dialogue),
                        ("dungeonID", PyRep::None),
                        ("currentRoomID", PyRep::Integer(1)),
                        ("bookmarkID", PyRep::Integer(bookmark_id)),
                        ("expiryUnix", PyRep::Integer(expiry_unix as i64)),
                        ("bonusExpiryUnix", PyRep::Integer(bonus_expiry_unix as i64)),
                        ("spawnedNpcBallID", npc_id_opt.map(PyRep::Integer).unwrap_or(PyRep::None)),
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
                        compute_character_mission_rewards(ctx, char_id, mission_id);
                    let now = ctx.now_unix();

                    let res = ctx.db.try_with_mut_snapshot(|snap| {
                        let meta_opt = load_mission_meta(snap, char_id, mission_id);
                        if let Some(ref meta) = meta_opt {
                            if meta.accept_expiry_unix.is_some_and(|exp| exp > 0 && now > exp) {
                                if let Some(m) = snap.active_missions.get_mut(&(char_id, mission_id))
                                {
                                    m.state = "Failed".into();
                                    m.npcs_remaining = 0;
                                }
                                return Err(BridgeRpcError { code: "MissionExpired".into(), message: format!("Mission {mission_id} has expired and failed") });
                            }
                        }
                        let Some(m) = snap.active_missions.get_mut(&(char_id, mission_id)) else {
                            return Err(BridgeRpcError { code: "MissionNotActive".into(), message: format!("Mission {mission_id} is not active for character {char_id}") });
                        };
                        let is_mining_mission = {
                            let t = tmpl.title.to_lowercase();
                            let c = tmpl
                                .content_template
                                .as_deref().unwrap_or_default()
                                .to_lowercase();
                            c.contains("mining")
                                || t.split(|ch: char| !ch.is_alphanumeric()).any(|w| matches!(w, "mining" | "mineral" | "minerals" | "ore" | "ores"))
                        };

                        if m.state != "Accepted" || (m.npcs_remaining > 0 && !is_mining_mission) {
                            return Err(BridgeRpcError { code: "MissionIncomplete".into(), message: format!("Mission {mission_id} cannot be completed (state='{}', npcs_remaining={})", m.state, m.npcs_remaining) });
                        }

                        let req_delivery = if is_mining_mission {
                            Some((1230_u32, 500_i32))
                        } else {
                            meta_opt.as_ref().and_then(|mt| {
                                mt.required_delivery_type_id.map(|tid| (tid, mt.required_delivery_qty.max(1)))
                            })
                        };
                        if let Some((req_tid, req_units)) = req_delivery {
                            let agent_station_id = ctx.sde.agents.get(&m.agent_id).and_then(|a| a.station_id)
                                .or(session.station_id);
                            let active_ship_id = snap.characters.get(&char_id).map(|c| c.active_ship_id).or(session.ship_id);
                            let is_valid_delivery_item = |item: &eve_db::InventoryItemRecord| -> bool {
                                item.owner_id == char_id
                                    && item.type_id == req_tid
                                    && ((item.flag == InventoryFlag::Hangar
                                        && agent_station_id.is_none_or(|st_id| item.location_id == st_id))
                                        || (is_mining_mission
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

                        let m_agent_id = m.agent_id;
                        m.state = "Completed".into();
                        let corp_id = ctx.sde.agents.get(&m_agent_id).map(|a| a.corporation_id)
                            .unwrap_or_else(|| {
                                resolve_agent_and_corp_for_system(ctx, char_id, sys_id, session.station_id, is_storyline)
                                .1
                            });

                        let bonus_granted = meta_opt
                            .as_ref().and_then(|mt| mt.bonus_expiry_unix)
                            .is_none_or(|bexp| now <= bexp);
                        let bonus_paid = if bonus_granted { bonus_reward } else { 0 };

                        if let Some(ch) = snap.characters.get_mut(&char_id) {
                            let reward_cents = IskCents::from_isk(isk_reward);
                            ch.balance = ch.balance.checked_add(reward_cents).unwrap_or(ch.balance);
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

                        let (t, storyline) = if !is_storyline && tmpl.has_standing_rewards {
                            let count = snap.mission_tally.entry(char_id).or_insert(0);
                            *count += 1;
                            let t = *count;
                            (t, t % 16 == 0)
                        } else {
                            let t = snap.mission_tally.get(&char_id).copied().unwrap_or(0);
                            (t, false)
                        };
                        if storyline {
                            if let Some(storyline_mission_id) = ctx.sde.missions
                                .values().filter(|sm| {
                                    is_storyline_template(sm)
                                        && !snap.active_missions.contains_key(&(char_id, sm.mission_id))
                                }).map(|sm| sm.mission_id)
                                .min()
                            {
                                snap.active_missions
                                    .entry((char_id, storyline_mission_id))
                                    .or_insert(ActiveMissionRecord { mission_id: storyline_mission_id, character_id: char_id, agent_id: storyline_agent_id, solar_system_id: sys_id, state: "Offered".into(), current_room_id: 1, npcs_remaining: 0, spawned_npc_ball_ids: Vec::new() });
                            }
                        }
                        Ok((new_st, t, storyline, bonus_paid, st_changes))
                    });

                    match res {
                        Ok((new_standing, tally, storyline_offered, bonus_paid, st_changes)) => {
                            notify_standing_changes(ctx, char_id, &st_changes);
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
                    match abandon_or_decline_mission(ctx, session, char_id, mission_id).await {
                        Ok((new_state, penalty_applied, was_accepted)) => {
                            let mission_state = if was_accepted { 6_i64 } else { 5_i64 };
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
