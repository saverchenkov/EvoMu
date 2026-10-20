//! `agents_missions` — Domain 9: Agent Dialogue, Missions, 16-Mission Storyline Tally, Epic Arcs, & AIR NPE (`§3.8`).

#![forbid(unsafe_code)]

use crate::formulas::{apply_standing_increase, compute_mission_reward_with_skills};
use crate::router::ServerContext;
use eve_db::{ActiveMissionRecord, InventoryFlag, IskCents, StandingTransactionRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;
use std::collections::HashMap;

fn parse_mission_id(args: &[PyRep]) -> Result<u32, BridgeRpcError> {
    let Some(raw) = args.first().and_then(|a| a.as_i64()) else {
        return Err(BridgeRpcError {
            code: "InvalidMissionId".into(),
            message: "mission_id argument is required".into(),
        });
    };
    u32::try_from(raw).map_err(|_| BridgeRpcError {
        code: "InvalidMissionId".into(),
        message: format!("mission_id {raw} is out of valid u32 range"),
    })
}

fn is_storyline_template(tmpl: &eve_sde::MissionTemplate) -> bool {
    tmpl.content_template
        .as_deref()
        .map(|ct| ct.contains("Storyline"))
        .unwrap_or(false)
}

fn is_storyline_agent_type(agent_type_id: u32) -> bool {
    // EVE agentType enum (EVE_Agent.h:48-49): 6 = GenericStoryLine, 7 = StoryLine (3 = Tutorial)
    matches!(agent_type_id, 6 | 7)
}

fn is_unsupported_direct_agent_type(agent_type_id: u32) -> bool {
    // EVE agentType enum (EVE_Agent.h:53): 11 = Aura, 12 = CareerAgent (divisions 25..=29)
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
        // EVE_Agent.h:52: EpicArc = 10
        10 => ct.contains("EpicArc"),
        // EVE_Agent.h:47: Research = 4
        4 => ct.contains("Research"),
        // Heraldry (Paragon) = 13
        13 => ct.contains("Heraldry") || ct.contains("BasicKill"),
        // Aura = 11, Career = 12 have no standard missions.fsdbinary pool
        11 | 12 => false,
        _ => match agent.division_id {
            // EVE_Agent.h:126, 137-139: RnD = 18, DistributionNew = 22, MiningNew = 23, SecurityNew = 24
            18 => ct.contains("Research"),
            22 => ct.contains("BasicCourier") || ct.contains("BasicTrade"),
            23 => ct.contains("BasicMining"),
            24 => ct.contains("BasicKill"),
            37 => ct.contains("Heraldry") || ct.contains("BasicKill"),
            _ => false,
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
            .ok()
            .map(|ch| ch.home_station_id)
    });
    let cur_sys = ctx.sde.solar_systems.get(&sys_id);
    let cur_constellation = cur_sys.map(|s| s.constellation_id);
    let cur_region = cur_sys.map(|s| s.region_id);
    let primary_corp = cur_sys
        .and_then(|s| {
            preferred_station
                .and_then(|sid| s.stations.iter().find(|st| st.station_id == sid))
                .or_else(|| s.stations.first())
        })
        .map(|st| st.owner_corporation_id)
        .or_else(|| ctx.sde.npc_corporations.keys().copied().min())
        .unwrap_or(0);

    let agent = if is_storyline {
        ctx.sde
            .agents
            .values()
            .filter(|a| a.solar_system_id == Some(sys_id) && is_storyline_agent_type(a.agent_type_id))
            .min_by_key(|a| a.agent_id)
            .or_else(|| {
                ctx.sde
                    .agents
                    .values()
                    .filter(|a| {
                        a.corporation_id == primary_corp && is_storyline_agent_type(a.agent_type_id)
                    })
                    .min_by_key(|a| a.agent_id)
            })
            .or_else(|| {
                ctx.sde
                    .agents
                    .values()
                    .filter(|a| {
                        is_storyline_agent_type(a.agent_type_id)
                            && a.solar_system_id
                                .and_then(|s| ctx.sde.solar_systems.get(&s))
                                .map(|s| s.constellation_id)
                                == cur_constellation
                    })
                    .min_by_key(|a| a.agent_id)
            })
            .or_else(|| {
                ctx.sde
                    .agents
                    .values()
                    .filter(|a| {
                        is_storyline_agent_type(a.agent_type_id)
                            && a.solar_system_id
                                .and_then(|s| ctx.sde.solar_systems.get(&s))
                                .map(|s| s.region_id)
                                == cur_region
                    })
                    .min_by_key(|a| a.agent_id)
            })
    } else {
        // Non-storyline default agent resolution strictly requires co-location in `sys_id`
        let is_eligible_regular = |a: &eve_sde::AgentInfo| -> bool {
            !is_storyline_agent_type(a.agent_type_id)
                && !is_unsupported_direct_agent_type(a.agent_type_id)
        };
        station_id
            .and_then(|sid| {
                ctx.sde
                    .agents
                    .values()
                    .filter(|a| {
                        a.solar_system_id == Some(sys_id)
                            && a.station_id == Some(sid)
                            && is_eligible_regular(a)
                    })
                    .min_by_key(|a| (a.level, a.agent_id))
            })
            .or_else(|| {
                ctx.sde
                    .agents
                    .values()
                    .filter(|a| {
                        a.solar_system_id == Some(sys_id)
                            && a.corporation_id == primary_corp
                            && is_eligible_regular(a)
                    })
                    .min_by_key(|a| (a.level, a.agent_id))
            })
            .or_else(|| {
                ctx.sde
                    .agents
                    .values()
                    .filter(|a| a.solar_system_id == Some(sys_id) && is_eligible_regular(a))
                    .min_by_key(|a| (a.level, a.agent_id))
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
            .default_attributes
            .get(&eve_sde::attr_ids::SHIELD_CAPACITY)
            .copied()
            .unwrap_or(0.0);
        let armor = inv
            .default_attributes
            .get(&eve_sde::attr_ids::ARMOR_HP)
            .copied()
            .unwrap_or(0.0);
        let hull = inv
            .default_attributes
            .get(&eve_sde::attr_ids::HP_STRUCT)
            .copied()
            .unwrap_or(0.0);
        let total_hp = shield + armor + hull;
        (50.0..=450.0).contains(&total_hp)
    };

    let mut group_ids: Vec<u32> = ctx
        .sde
        .npc_spawn_groups
        .values()
        .filter(|sg| sg.ship_class == 1)
        .map(|sg| sg.group_id)
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
    ctx.sde
        .types
        .values()
        .filter(|t| t.category_id == 11 && is_l1_frigate_rat(t))
        .map(|t| t.type_id)
        .min()
        .unwrap_or(0)
}

fn resolve_career_goal_def(ctx: &ServerContext, goal_id: u32) -> Option<(u64, f64)> {
    if goal_id == 2 {
        // MN-18: Mining Career Goal: "Mine 500 units of Ore", 50,000 SP reward
        return Some((500, 50_000.0));
    }
    ctx.sde
        .career_goals
        .get(&goal_id)
        .map(|g| (g.target_progress, g.sp_reward as f64))
}

/// Computes `(isk_reward, lp_reward, bonus_isk_reward)` for a character and SDE mission template
/// using the character's Negotiation (`3356`) and Security Connections (`3893`) skills so that
/// `GetMissionBriefingInfo`, `DoAction` (RequestMission), and `DoAction` (CompleteMission) report
/// and pay identical values.
///
/// DERIVED: V24 client `missions.fsdbinary` stores `lp_alpha` and `lp_omega` per mission template
/// while cash ISK payouts reside on CCP's server DB; `base_isk` is derived deterministically from
/// the SDE mission template's `base_lp` (`base_lp * 1_000`).
fn compute_character_mission_rewards(
    ctx: &ServerContext,
    char_id: i64,
    mission_id: u32,
) -> (i64, i64, i64) {
    let tmpl = ctx.sde.missions.get(&mission_id);
    let (is_alpha, neg_lvl, sec_conn_lvl) = ctx.db.with_snapshot(|snap| {
        let alpha = snap
            .characters
            .get(&char_id)
            .map(|c| c.clone_grade <= 1)
            .unwrap_or(false);
        let sk = snap.skills.get(&char_id);
        let neg = sk.and_then(|m| m.get(&3356)).map(|s| s.level).unwrap_or(0);
        let sec = sk.and_then(|m| m.get(&3893)).map(|s| s.level).unwrap_or(0);
        (alpha, neg, sec)
    });
    let base_lp = tmpl
        .map(|t| {
            if is_alpha && t.lp_alpha > 0 {
                t.lp_alpha
            } else {
                t.lp_omega.max(100)
            }
        })
        .unwrap_or(100);
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
    if let Some(bound_agent) = ctx
        .bound_objects
        .read()
        .ok()
        .and_then(|map| {
            map.values()
                .filter(|b| {
                    b.session_id == session.session_id
                        && b.service == "agentMgr"
                        && b.object_id > 0
                })
                .max_by_key(|b| b.bound_id)
                .map(|b| b.object_id)
        })
    {
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
        if first_arg > 11 {
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
            .values()
            .filter(|m| {
                m.character_id == char_id
                    && (agent_id == 0 || m.agent_id == agent_id)
                    && expected_state.is_none_or(|st| m.state == st)
            })
            .map(|m| m.mission_id)
            .min()
            .or_else(|| {
                snap.active_missions
                    .values()
                    .filter(|m| {
                        m.character_id == char_id && expected_state.is_none_or(|st| m.state == st)
                    })
                    .map(|m| m.mission_id)
                    .min()
            })
            .or_else(|| {
                snap.active_missions
                    .values()
                    .filter(|m| {
                        m.character_id == char_id && (agent_id == 0 || m.agent_id == agent_id)
                    })
                    .map(|m| m.mission_id)
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

pub async fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("agentMgr", "GetAgents") => {
            let caller_char = session.character_id;
            let standings = caller_char
                .and_then(|cid| {
                    ctx.db
                        .with_snapshot(|snap| snap.characters.get(&cid).map(|c| c.standings.clone()))
                })
                .unwrap_or_default();

            let filter_level = args
                .first()
                .and_then(|a| a.as_i64())
                .map(|v| v as u8)
                .or_else(|| _kwargs.get("level").and_then(|v| v.as_i64()).map(|v| v as u8));
            let filter_division = args
                .get(1)
                .and_then(|a| a.as_i64())
                .map(|v| v as u32)
                .or_else(|| {
                    _kwargs
                        .get("divisionID")
                        .or_else(|| _kwargs.get("division"))
                        .and_then(|v| v.as_i64())
                        .map(|v| v as u32)
                });
            let filter_faction = args
                .get(2)
                .and_then(|a| a.as_i64())
                .or_else(|| {
                    _kwargs
                        .get("factionID")
                        .or_else(|| _kwargs.get("faction"))
                        .and_then(|v| v.as_i64())
                });
            let filter_corp = args
                .get(3)
                .and_then(|a| a.as_i64())
                .or_else(|| {
                    _kwargs
                        .get("corporationID")
                        .or_else(|| _kwargs.get("corporation"))
                        .and_then(|v| v.as_i64())
                });

            let mut matching_agents: Vec<&eve_sde::AgentInfo> = ctx
                .sde
                .agents
                .values()
                .filter(|a| {
                    if let Some(lvl) = filter_level {
                        if a.level != lvl {
                            return false;
                        }
                    }
                    if let Some(div) = filter_division {
                        if a.division_id != div {
                            return false;
                        }
                    }
                    if let Some(cid) = filter_corp {
                        if a.corporation_id != cid {
                            return false;
                        }
                    }
                    if let Some(fid) = filter_faction {
                        let corp_faction = ctx
                            .sde
                            .npc_corporations
                            .get(&a.corporation_id)
                            .and_then(|c| c.faction_id)
                            .map(|f| f as i64);
                        if corp_faction != Some(fid) {
                            return false;
                        }
                    }
                    true
                })
                .collect();

            matching_agents.sort_by_key(|a| a.agent_id);

            let rows: Vec<Vec<PyRep>> = matching_agents
                .into_iter()
                .map(|a| {
                    let corp_faction = ctx
                        .sde
                        .npc_corporations
                        .get(&a.corporation_id)
                        .and_then(|c| c.faction_id)
                        .map(|f| f as i64);

                    let has_access = if a.level <= 1 {
                        true
                    } else {
                        let req_standing = ((a.level as f64) - 1.0) * 2.0 - 1.0;
                        let agent_st = standings.get(&a.agent_id).copied();
                        let corp_st = standings.get(&a.corporation_id).copied();
                        let fac_st = corp_faction.and_then(|fid| standings.get(&fid).copied());
                        let max_st = [agent_st, corp_st, fac_st]
                            .into_iter()
                            .flatten()
                            .fold(0.0_f64, f64::max);
                        max_st >= req_standing
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
                        PyRep::Integer(20), // Standard agent quality post-Incursion
                        PyRep::Bool(has_access),
                    ]
                })
                .collect();

            Some(Ok(PyRep::rowset(
                vec![
                    "agentID",
                    "agentTypeID",
                    "divisionID",
                    "corporationID",
                    "factionID",
                    "solarSystemID",
                    "stationID",
                    "level",
                    "quality",
                    "hasAccess",
                ],
                rows,
            )))
        }

        ("agentMgr", "GetInfoServiceDetails") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
                }));
            };
            let (agent_id, _) = resolve_target_agent_id(ctx, session, char_id, sys_id, args, _kwargs);
            let Some(a) = ctx.sde.agents.get(&agent_id) else {
                return Some(Err(BridgeRpcError {
                    code: "UnknownAgent".into(),
                    message: format!("Agent {agent_id} does not exist in SDE"),
                }));
            };
            Some(Ok(PyRep::keyval([
                ("agentID", PyRep::Integer(a.agent_id)),
                ("agentTypeID", PyRep::Integer(a.agent_type_id as i64)),
                ("divisionID", PyRep::Integer(a.division_id as i64)),
                ("corporationID", PyRep::Integer(a.corporation_id)),
                (
                    "solarSystemID",
                    a.solar_system_id
                        .map(|s| PyRep::Integer(s as i64))
                        .unwrap_or(PyRep::None),
                ),
                ("stationID", a.station_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
                ("level", PyRep::Integer(a.level as i64)),
                ("quality", PyRep::Integer(20)),
                ("services", PyRep::List(vec![PyRep::String("mission".into())])),
            ])))
        }

        ("agentMgr", "GetSolarSystemOfAgent") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
                }));
            };
            let (agent_id, _) = resolve_target_agent_id(ctx, session, char_id, sys_id, args, _kwargs);
            let Some(a) = ctx.sde.agents.get(&agent_id) else {
                return Some(Err(BridgeRpcError {
                    code: "UnknownAgent".into(),
                    message: format!("Agent {agent_id} does not exist in SDE"),
                }));
            };
            Some(Ok(
                a.solar_system_id
                    .map(|s| PyRep::Integer(s as i64))
                    .unwrap_or(PyRep::None),
            ))
        }

        ("agentMgr", "GetMyJournalDetails") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let now = ctx.now_unix();
            let missions = ctx.db.with_snapshot(|snap| {
                let mut active: Vec<ActiveMissionRecord> = snap
                    .active_missions
                    .values()
                    .filter(|m| {
                        m.character_id == char_id
                            && matches!(m.state.as_str(), "Offered" | "Accepted")
                    })
                    .cloned()
                    .collect();
                active.sort_by_key(|m| m.mission_id);
                active
                    .into_iter()
                    .map(|m| {
                        let tmpl = ctx.sde.missions.get(&m.mission_id);
                        let title = tmpl
                            .map(|t| t.title.clone())
                            .unwrap_or_else(|| format!("Mission {}", m.mission_id));
                        let important = tmpl.is_some_and(is_storyline_template);
                        let mission_state = if m.state == "Accepted" { 2_i64 } else { 1_i64 };
                        let bms: Vec<PyRep> = snap
                            .bookmarks
                            .values()
                            .filter(|b| {
                                b.owner_id == char_id
                                    && b.solar_system_id == m.solar_system_id
                                    && b.memo.contains("Mission Pocket")
                            })
                            .map(|b| {
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
                            ("expirationTime", PyRep::Integer((now + 86400) as i64)),
                            ("bookmarks", PyRep::List(bms)),
                        ])
                    })
                    .collect::<Vec<PyRep>>()
            });
            Some(Ok(PyRep::Tuple(vec![
                PyRep::List(missions),
                PyRep::List(vec![]),
            ])))
        }

        ("agentMgr", "GetMissionBriefingInfo")
        | ("agentMgr", "GetMissionObjectiveInfo")
        | ("agentMgr", "GetMissionJournalInfo") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
                }));
            };
            let (agent_id, _) = resolve_target_agent_id(ctx, session, char_id, sys_id, args, _kwargs);
            let mission_id = match resolve_target_mission_id(ctx, char_id, agent_id, None, args, _kwargs) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(m) = ctx.db.with_snapshot(|snap| snap.active_missions.get(&(char_id, mission_id)).cloned()) else {
                return Some(Err(BridgeRpcError {
                    code: "MissionNotActive".into(),
                    message: format!("Mission {mission_id} is not active for character {char_id}"),
                }));
            };
            let tmpl = ctx.sde.missions.get(&mission_id);
            let title = tmpl
                .map(|t| t.title.clone())
                .unwrap_or_else(|| format!("Mission {mission_id}"));
            let briefing = format!(
                "Agent {} operation: {}. Execute objectives in system {} and report back.",
                m.agent_id, title, m.solar_system_id
            );
            let is_mining = tmpl
                .map(|t| {
                    t.title.to_lowercase().contains("mining")
                        || t.content_template
                            .as_deref()
                            .unwrap_or_default()
                            .contains("BasicMining")
                })
                .unwrap_or(false);
            let objectives = if is_mining {
                "Mine 500 units of Veldspar and deliver to the agent station hangar."
            } else {
                "Fly to encounter location, neutralize hostile pirate vessel, and report back."
            };
            let mission_state = match m.state.as_str() {
                "Offered" => 1_i64,
                "Accepted" => 2_i64,
                "Failed" => 3_i64,
                "Completed" => 4_i64,
                "Declined" => 5_i64,
                "Quit" => 6_i64,
                _ => 0_i64,
            };
            let (isk_reward, lp_reward, bonus_reward) =
                compute_character_mission_rewards(ctx, char_id, mission_id);
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
                ("iskReward", PyRep::Integer(isk_reward)),
                ("lpReward", PyRep::Integer(lp_reward)),
                ("bonusReward", PyRep::Integer(bonus_reward)),
            ])))
        }

        ("agentMgr", "DoAction") | ("agentMgr", "GetOffer") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
                }));
            };
            let action_id = if method == "GetOffer" {
                2_i64
            } else if let Some(raw_action) = args.first().and_then(|a| a.as_i64()) {
                if (1..=11).contains(&raw_action) {
                    raw_action
                } else {
                    2_i64
                }
            } else {
                2_i64
            };

            let (mut agent_id, mut explicit_agent) =
                resolve_target_agent_id(ctx, session, char_id, sys_id, args, _kwargs);

            match action_id {
                // 1 = agentDialogueButtonViewMission, 2 = agentDialogueButtonRequestMission, 8 = agentDialogueButtonContinue
                1 | 2 | 8 => {
                    let supplied_id = if args
                        .first()
                        .and_then(|a| a.as_i64())
                        .is_some_and(|v| (1..=11).contains(&v))
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
                        return Some(Err(BridgeRpcError {
                            code: "UnknownAgent".into(),
                            message: format!(
                                "Agent {agent_id} does not exist in SDE"
                            ),
                        }));
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
                        return Some(Err(BridgeRpcError {
                            code: "AgentNotCoLocated".into(),
                            message: format!(
                                "Agent {agent_id} is in system {:?} / station {:?}, not character's current location (system {sys_id}, station {:?})",
                                agent_info.solar_system_id, agent_info.station_id, session.station_id
                            ),
                        }));
                    }
                    if is_storyline_agent_type(agent_info.agent_type_id) {
                        return Some(Err(BridgeRpcError {
                            code: "StorylineAgentRequiresReferral".into(),
                            message: format!(
                                "Storyline agent {agent_id} (agentType {}) cannot be requested directly without a 16-mission referral",
                                agent_info.agent_type_id
                            ),
                        }));
                    }
                    if is_unsupported_direct_agent_type(agent_info.agent_type_id) {
                        return Some(Err(BridgeRpcError {
                            code: "UnimplementedRPC".into(),
                            message: format!(
                                "Agent {agent_id} (agentType {}) uses Aura/Career node-graph missions which are not yet implemented",
                                agent_info.agent_type_id
                            ),
                        }));
                    }
                    if agent_info.level > 1 {
                        let required_standing = ((agent_info.level as f64) - 1.0) * 2.0 - 1.0;
                        let faction_id = ctx
                            .sde
                            .npc_corporations
                            .get(&agent_info.corporation_id)
                            .and_then(|c| c.faction_id)
                            .map(|fid| fid as i64);
                        let effective_standing = ctx.db.with_snapshot(|snap| {
                            snap.characters
                                .get(&char_id)
                                .map(|ch| {
                                    let agent_st = ch.standings.get(&agent_id).copied();
                                    let corp_st = ch.standings.get(&agent_info.corporation_id).copied();
                                    let fac_st = faction_id.and_then(|fid| ch.standings.get(&fid).copied());
                                    [agent_st, corp_st, fac_st]
                                        .into_iter()
                                        .flatten()
                                        .fold(0.0_f64, f64::max)
                                })
                                .unwrap_or(0.0)
                        });
                        if effective_standing < required_standing {
                            return Some(Err(BridgeRpcError {
                                code: "InsufficientAgentStanding".into(),
                                message: format!(
                                    "Agent {agent_id} (level {}) requires agent/corporation/faction standing >= {required_standing:.1}, character has {effective_standing:.2}",
                                    agent_info.level
                                ),
                            }));
                        }
                    }

                    let res = ctx.db.try_with_mut_snapshot(|snap| {
                        if let Some(existing_active) = snap.active_missions.values().find(|m| {
                            m.character_id == char_id
                                && m.agent_id == agent_id
                                && m.state == "Offered"
                        }) {
                            let mid = existing_active.mission_id;
                            let title = ctx
                                .sde
                                .missions
                                .get(&mid)
                                .map(|t| t.title.clone())
                                .unwrap_or_default();
                            return Ok((mid, title));
                        }
                        let next_tmpl = ctx
                            .sde
                            .missions
                            .values()
                            .filter(|m| {
                                mission_matches_agent_pool(m, agent_info)
                                    && m.has_standing_rewards
                                    && !snap.active_missions.contains_key(&(char_id, m.mission_id))
                            })
                            .min_by_key(|m| m.mission_id)
                            .or_else(|| {
                                ctx.sde
                                    .missions
                                    .values()
                                    .filter(|m| {
                                        mission_matches_agent_pool(m, agent_info)
                                            && !snap.active_missions.contains_key(&(char_id, m.mission_id))
                                    })
                                    .min_by_key(|m| m.mission_id)
                            });
                        let Some(tmpl) = next_tmpl else {
                            return Err(BridgeRpcError {
                                code: "NoMissionsAvailable".into(),
                                message: format!(
                                    "No unoffered missions remaining in SDE pool for agent {agent_id} and character {char_id}"
                                ),
                            });
                        };
                        let mission_id = tmpl.mission_id;
                        let title = tmpl.title.clone();
                        let is_mining = tmpl
                            .content_template
                            .as_deref()
                            .unwrap_or_default()
                            .contains("BasicMining")
                            || tmpl.title.to_lowercase().contains("mining")
                            || agent_info.division_id == 23;
                        let npcs_remaining = if is_mining { 0 } else { 1 };
                        snap.active_missions.insert(
                            (char_id, mission_id),
                            ActiveMissionRecord {
                                mission_id,
                                character_id: char_id,
                                agent_id,
                                solar_system_id: sys_id,
                                state: "Offered".into(),
                                current_room_id: 1,
                                npcs_remaining,
                                spawned_npc_ball_ids: Vec::new(),
                            },
                        );
                        Ok((mission_id, title))
                    });

                    match res {
                        Ok((mission_id, title)) => {
                            let tmpl = ctx.sde.missions.get(&mission_id);
                            let briefing = tmpl
                                .map(|t| {
                                    format!(
                                        "Agent {} operation: {}. Execute objectives in system {} and report back.",
                                        agent_id, t.title, sys_id
                                    )
                                })
                                .unwrap_or_else(|| format!("Mission assignment {mission_id} for agent {agent_id}."));
                            let is_mining = tmpl
                                .map(|t| {
                                    t.title.to_lowercase().contains("mining")
                                        || t.content_template
                                            .as_deref()
                                            .unwrap_or_default()
                                            .contains("BasicMining")
                                })
                                .unwrap_or(false)
                                || agent_info.division_id == 23;
                            let objectives = if is_mining {
                                "Mine 500 units of Veldspar and deliver to the agent station hangar."
                            } else {
                                "Fly to encounter location, neutralize hostile pirate vessel, and report back."
                            };
                            let now = ctx.now_unix();
                            let expiry_unix = now + 604_800; // 7 days offer expiry
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

                // 3 = agentDialogueButtonAccept, 4 = AcceptChoice, 5 = AcceptRemotely
                3..=5 => {
                    let mission_id = match resolve_target_mission_id(
                        ctx,
                        char_id,
                        agent_id,
                        Some("Offered"),
                        args,
                        _kwargs,
                    ) {
                        Ok(v) => v,
                        Err(e) => return Some(Err(e)),
                    };
                    let npc_type_id = resolve_mission_npc_type(ctx);
                    let Some(npc_inv) = ctx.sde.types.get(&npc_type_id) else {
                        return Some(Err(BridgeRpcError {
                            code: "MissingNpcSdeType".into(),
                            message: format!("Mission NPC type {npc_type_id} missing from SDE"),
                        }));
                    };
                    let Some(&max_velocity) = npc_inv
                        .default_attributes
                        .get(&eve_sde::attr_ids::MAX_VELOCITY)
                    else {
                        return Some(Err(BridgeRpcError {
                            code: "MissingNpcSdeAttributes".into(),
                            message: format!(
                                "Mission NPC type {npc_type_id} missing maxVelocity attribute in SDE"
                            ),
                        }));
                    };
                    let mass = npc_inv.mass.max(1.0);
                    let radius = npc_inv.radius.max(1.0);

                    let handle = ctx.systems.get_or_spawn(sys_id);
                    let existing_accepted = ctx.db.with_snapshot(|snap| {
                        snap.active_missions
                            .get(&(char_id, mission_id))
                            .filter(|m| m.state == "Accepted" && m.npcs_remaining > 0)
                            .map(|m| (m.solar_system_id, m.spawned_npc_ball_ids.clone()))
                    });
                    let can_respawn_missing = if let Some((m_sys_id, prev_balls)) = existing_accepted {
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
                    let res = ctx.db.try_with_mut_snapshot(|snap| {
                        let Some(m_check) = snap.active_missions.get(&(char_id, mission_id)) else {
                            return Err(BridgeRpcError {
                                code: "MissionNotOffered".into(),
                                message: format!(
                                    "Mission {mission_id} has not been offered to character {char_id}"
                                ),
                            });
                        };
                        if m_check.state != "Offered"
                            && !(m_check.state == "Accepted"
                                && m_check.solar_system_id == sys_id
                                && can_respawn_missing)
                        {
                            return Err(BridgeRpcError {
                                code: "InvalidMissionState".into(),
                                message: format!(
                                    "Mission {mission_id} is in state '{}' (system {}), not Offered",
                                    m_check.state, m_check.solar_system_id
                                ),
                            });
                        };
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
                        Ok(npc_id_opt)
                    });
                    let npc_id_opt = match res {
                        Ok(id) => id,
                        Err(e) => return Some(Err(e)),
                    };

                    if let Some(npc_id) = npc_id_opt {
                        let origin = if let Some(ship_id) = session.ship_id {
                            handle
                                .get_ball_snapshot(ship_id)
                                .await
                                .ok()
                                .flatten()
                                .map(|(_, b)| b.position)
                                .unwrap_or_else(|| {
                                    ctx.sde
                                        .solar_systems
                                        .get(&sys_id)
                                        .and_then(|s| {
                                            s.stations
                                                .first()
                                                .map(|st| st.position + DVec3::new(1_500.0, 0.0, 0.0))
                                        })
                                        .unwrap_or(DVec3::ZERO)
                                })
                        } else {
                            DVec3::ZERO
                        };
                        let pocket_offset = ((npc_id % 40) as f64) * 35.0;
                        let _ = handle
                            .spawn_ball_with_combat(
                                Ball {
                                    id: npc_id,
                                    type_id: npc_type_id,
                                    position: origin + DVec3::new(800.0 + pocket_offset, 0.0, 0.0),
                                    velocity: DVec3::ZERO,
                                    max_velocity,
                                    inertia: 2.5,
                                    mass,
                                    radius,
                                    speed_fraction: 0.0,
                                    mode: MovementMode::Stop,
                                },
                                ctx.build_type_combat_state(npc_type_id),
                            )
                            .await;
                    }

                    let is_mining_mission = ctx
                        .sde
                        .missions
                        .get(&mission_id)
                        .map(|m| {
                            let t = m.title.to_lowercase();
                            let c = m
                                .content_template
                                .as_deref()
                                .unwrap_or_default()
                                .to_lowercase();
                            c.contains("mining")
                                || t.contains("mining")
                                || t.contains("mineral")
                                || t.contains("ore")
                        })
                        .unwrap_or(false);

                    if is_mining_mission {
                        let origin = if let Some(ship_id) = session.ship_id {
                            handle
                                .get_ball_snapshot(ship_id)
                                .await
                                .ok()
                                .flatten()
                                .map(|(_, b)| b.position)
                                .unwrap_or(DVec3::ZERO)
                        } else {
                            DVec3::ZERO
                        };
                        let ast_id = 910_000_000 + (mission_id as i64) * 1000 + 1;
                        let ast_ball = Ball {
                            id: ast_id,
                            type_id: 1230, // Veldspar
                            position: origin + DVec3::new(8_000.0, 1_000.0, -5_000.0),
                            velocity: DVec3::ZERO,
                            max_velocity: 0.0,
                            inertia: 1.0,
                            mass: 50_000.0,
                            radius: 50.0,
                            speed_fraction: 0.0,
                            mode: MovementMode::Stop,
                        };
                        let ast_state = crate::system_actor::AsteroidState {
                            asteroid_id: ast_id,
                            belt_id: 0,
                            type_id: 1230,
                            initial_units: 5_000,
                            remaining_units: 5_000,
                            unit_volume: 0.1,
                            depleted: false,
                            respawn_at_tick: None,
                        };
                        let _ = handle.spawn_asteroid(ast_state, ast_ball).await;
                    }

                    let now = ctx.now_unix();
                    let expiry_unix = now + 86400; // 24h
                    let bonus_expiry_unix = now + 14400; // 4h
                    let mission_title = ctx
                        .sde
                        .missions
                        .get(&mission_id)
                        .map(|m| m.title.clone())
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
                        PyRep::Tuple(vec![
                            PyRep::Integer(6),
                            PyRep::String("Complete Mission".into()),
                        ]),
                        PyRep::Tuple(vec![
                            PyRep::Integer(11),
                            PyRep::String("Quit Mission".into()),
                        ]),
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
                        (
                            "spawnedNpcBallID",
                            npc_id_opt.map(PyRep::Integer).unwrap_or(PyRep::None),
                        ),
                    ])))
                }

                // 6 = agentDialogueButtonComplete, 7 = CompleteRemotely
                6 | 7 => {
                    let mission_id = match resolve_target_mission_id(
                        ctx,
                        char_id,
                        agent_id,
                        Some("Accepted"),
                        args,
                        _kwargs,
                    ) {
                        Ok(v) => v,
                        Err(e) => return Some(Err(e)),
                    };
                    let Some(tmpl) = ctx.sde.missions.get(&mission_id).cloned() else {
                        return Some(Err(BridgeRpcError {
                            code: "UnknownMission".into(),
                            message: format!("Mission {mission_id} does not exist in SDE"),
                        }));
                    };
                    let is_storyline = is_storyline_template(&tmpl);
                    let active_agent_id = ctx.db.with_snapshot(|snap| {
                        snap.active_missions
                            .get(&(char_id, mission_id))
                            .map(|m| m.agent_id)
                            .unwrap_or(agent_id)
                    });
                    let agent_level = ctx
                        .sde
                        .agents
                        .get(&active_agent_id)
                        .map(|a| a.level.max(1) as f64)
                        .unwrap_or(1.0);
                    let storyline_mult = if is_storyline { 3.0 } else { 1.0 };
                    let standing_pct = if tmpl.has_standing_rewards {
                        1.5 * agent_level * storyline_mult
                    } else {
                        0.0
                    };
                    let (storyline_agent_id, _) = resolve_agent_and_corp_for_system(
                        ctx,
                        char_id,
                        sys_id,
                        session.station_id,
                        true,
                    );
                    let (isk_reward, lp_reward, _) =
                        compute_character_mission_rewards(ctx, char_id, mission_id);

                    let res = ctx.db.try_with_mut_snapshot(|snap| {
                        let Some(m) = snap.active_missions.get_mut(&(char_id, mission_id)) else {
                            return Err(BridgeRpcError {
                                code: "MissionNotActive".into(),
                                message: format!(
                                    "Mission {mission_id} is not active for character {char_id}"
                                ),
                            });
                        };
                        let is_mining_mission = {
                            let t = tmpl.title.to_lowercase();
                            let c = tmpl
                                .content_template
                                .as_deref()
                                .unwrap_or_default()
                                .to_lowercase();
                            c.contains("mining")
                                || t.contains("mining")
                                || t.contains("mineral")
                                || t.contains("ore")
                        };

                        if m.state != "Accepted" || (m.npcs_remaining > 0 && !is_mining_mission) {
                            return Err(BridgeRpcError {
                                code: "MissionIncomplete".into(),
                                message: format!(
                                    "Mission {mission_id} cannot be completed (state='{}', npcs_remaining={})",
                                    m.state, m.npcs_remaining
                                ),
                            });
                        }

                        if is_mining_mission {
                            let req_ore_tid = 1230; // Veldspar
                            let req_units = 500_i32;
                            let agent_station_id = ctx
                                .sde
                                .agents
                                .get(&m.agent_id)
                                .and_then(|a| a.station_id)
                                .or(session.station_id);
                            let mut found_qty = 0_i32;
                            for item in snap.items.values() {
                                if item.owner_id == char_id
                                    && item.type_id == req_ore_tid
                                    && item.flag == InventoryFlag::Hangar
                                    && agent_station_id
                                        .map_or(true, |st_id| item.location_id == st_id)
                                {
                                    found_qty += item.quantity;
                                }
                            }
                            if found_qty < req_units {
                                return Err(BridgeRpcError {
                                    code: "MissionIncomplete".into(),
                                    message: format!(
                                        "Mission {mission_id} requires delivering {req_units} units of type {req_ore_tid} in agent station hangar, character only has {found_qty}"
                                    ),
                                });
                            }

                            let mut remaining_to_consume = req_units;
                            let mut to_remove = Vec::new();
                            for item in snap.items.values_mut() {
                                if item.owner_id == char_id
                                    && item.type_id == req_ore_tid
                                    && item.flag == InventoryFlag::Hangar
                                    && agent_station_id
                                        .map_or(true, |st_id| item.location_id == st_id)
                                    && remaining_to_consume > 0
                                {
                                    if item.quantity <= remaining_to_consume {
                                        remaining_to_consume -= item.quantity;
                                        to_remove.push(item.item_id);
                                    } else {
                                        item.quantity -= remaining_to_consume;
                                        remaining_to_consume = 0;
                                    }
                                }
                            }
                            for iid in to_remove {
                                snap.items.remove(&iid);
                            }
                        }
                        let m_agent_id = m.agent_id;
                        m.state = "Completed".into();
                        let corp_id = ctx
                            .sde
                            .agents
                            .get(&m_agent_id)
                            .map(|a| a.corporation_id)
                            .unwrap_or_else(|| {
                                resolve_agent_and_corp_for_system(
                                    ctx,
                                    char_id,
                                    sys_id,
                                    session.station_id,
                                    is_storyline,
                                )
                                .1
                            });

                        let mut new_st = 2.0;
                        let mut actual_delta = None;
                        if let Some(ch) = snap.characters.get_mut(&char_id) {
                            let reward_cents = IskCents::from_isk(isk_reward);
                            ch.balance = ch.balance.checked_add(reward_cents).unwrap_or(ch.balance);
                            let lp_entry = ch.loyalty_points.entry(corp_id).or_insert(0);
                            *lp_entry = lp_entry.saturating_add(lp_reward.max(0));
                            let cur_st = ch.standings.get(&corp_id).copied().unwrap_or(0.0);
                            new_st = apply_standing_increase(cur_st, standing_pct);
                            ch.standings.insert(corp_id, new_st);
                            actual_delta = Some(new_st - cur_st);
                        }
                        if let Some(delta) = actual_delta {
                            snap.next_entity_id += 1;
                            let ev_id = snap.next_entity_id;
                            snap.standing_transactions
                                .entry(char_id)
                                .or_default()
                                .push(StandingTransactionRecord {
                                    event_id: ev_id,
                                    from_id: corp_id,
                                    to_id: char_id,
                                    modification: delta,
                                    event_type_id: 85,
                                });
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
                            if let Some(storyline_mission_id) = ctx
                                .sde
                                .missions
                                .values()
                                .filter(|sm| {
                                    is_storyline_template(sm)
                                        && !snap
                                            .active_missions
                                            .contains_key(&(char_id, sm.mission_id))
                                })
                                .map(|sm| sm.mission_id)
                                .min()
                            {
                                snap.active_missions
                                    .entry((char_id, storyline_mission_id))
                                    .or_insert(ActiveMissionRecord {
                                        mission_id: storyline_mission_id,
                                        character_id: char_id,
                                        agent_id: storyline_agent_id,
                                        solar_system_id: sys_id,
                                        state: "Offered".into(),
                                        current_room_id: 1,
                                        npcs_remaining: 0,
                                        spawned_npc_ball_ids: Vec::new(),
                                    });
                            }
                        }
                        Ok((new_st, t, storyline))
                    });

                    match res {
                        Ok((new_standing, tally, storyline_offered)) => Some(Ok(PyRep::keyval([
                            ("missionID", PyRep::Integer(mission_id as i64)),
                            ("contentID", PyRep::Integer(mission_id as i64)),
                            ("state", PyRep::String("Completed".into())),
                            ("missionState", PyRep::Integer(4)),
                            ("iskReward", PyRep::Integer(isk_reward)),
                            ("lpReward", PyRep::Integer(lp_reward)),
                            ("newStanding", PyRep::Real(new_standing)),
                            ("missionTally", PyRep::Integer(tally as i64)),
                            ("storylineTriggered", PyRep::Bool(storyline_offered)),
                        ]))),
                        Err(e) => Some(Err(e)),
                    }
                }

                // 9 = agentDialogueButtonDecline, 10 = Defer, 11 = agentDialogueButtonQuit
                9..=11 => {
                    let expected_state = if action_id == 11 {
                        Some("Accepted")
                    } else {
                        Some("Offered")
                    };
                    let mission_id = match resolve_target_mission_id(
                        ctx,
                        char_id,
                        agent_id,
                        expected_state,
                        args,
                        _kwargs,
                    ) {
                        Ok(v) => v,
                        Err(e) => return Some(Err(e)),
                    };
                    let res = ctx.db.try_with_mut_snapshot(|snap| {
                        let Some(m) = snap.active_missions.get_mut(&(char_id, mission_id)) else {
                            return Err(BridgeRpcError {
                                code: "MissionNotActive".into(),
                                message: format!(
                                    "Mission {mission_id} is not active or offered for character {char_id}"
                                ),
                            });
                        };
                        if !matches!(m.state.as_str(), "Offered" | "Accepted") {
                            return Err(BridgeRpcError {
                                code: "MissionNotActive".into(),
                                message: format!(
                                    "Mission {mission_id} is in terminal state '{}' and cannot be abandoned",
                                    m.state
                                ),
                            });
                        }
                        let was_accepted = m.state == "Accepted";
                        let m_sys_id = m.solar_system_id;
                        let m_agent_id = m.agent_id;
                        let now = ctx.now_unix();
                        let mut standing_penalty_applied = false;

                        m.state = if was_accepted {
                            "Quit".into()
                        } else {
                            "Declined".into()
                        };
                        m.npcs_remaining = 0;
                        let ball_ids = std::mem::take(&mut m.spawned_npc_ball_ids);

                        let corp_id = ctx
                            .sde
                            .agents
                            .get(&m_agent_id)
                            .map(|a| a.corporation_id)
                            .unwrap_or_else(|| {
                                resolve_agent_and_corp_for_system(
                                    ctx,
                                    char_id,
                                    m_sys_id,
                                    session.station_id,
                                    false,
                                )
                                .1
                            });

                        if was_accepted {
                            if ctx.sde.missions.contains_key(&mission_id) {
                                let mut actual_delta = None;
                                if let Some(ch) = snap.characters.get_mut(&char_id) {
                                    let cur = ch.standings.get(&corp_id).copied().unwrap_or(0.0);
                                    let next = (cur - 0.1).max(-10.0);
                                    ch.standings.insert(corp_id, next);
                                    actual_delta = Some(if cur - 0.1 >= -10.0 {
                                        -0.1
                                    } else {
                                        -10.0 - cur
                                    });
                                }
                                if let Some(delta) = actual_delta {
                                    snap.next_entity_id += 1;
                                    let ev_id = snap.next_entity_id;
                                    snap.standing_transactions
                                        .entry(char_id)
                                        .or_default()
                                        .push(StandingTransactionRecord {
                                            event_id: ev_id,
                                            from_id: corp_id,
                                            to_id: char_id,
                                            modification: delta,
                                            event_type_id: 83,
                                        });
                                }
                                standing_penalty_applied = true;
                            }
                            snap.bookmarks.retain(|_, b| {
                                !(b.owner_id == char_id
                                    && b.solar_system_id == m_sys_id
                                    && b.memo.contains("Mission Pocket"))
                            });
                        } else {
                            let last_decline = snap
                                .last_mission_decline_times
                                .get(&char_id)
                                .copied()
                                .unwrap_or(0);
                            if now.saturating_sub(last_decline) < 14_400 {
                                let mut actual_delta = None;
                                if let Some(ch) = snap.characters.get_mut(&char_id) {
                                    let cur = ch.standings.get(&corp_id).copied().unwrap_or(0.0);
                                    let next = (cur - 0.05).max(-10.0);
                                    ch.standings.insert(corp_id, next);
                                    actual_delta = Some(if cur - 0.05 >= -10.0 {
                                        -0.05
                                    } else {
                                        -10.0 - cur
                                    });
                                }
                                if let Some(delta) = actual_delta {
                                    snap.next_entity_id += 1;
                                    let ev_id = snap.next_entity_id;
                                    snap.standing_transactions
                                        .entry(char_id)
                                        .or_default()
                                        .push(StandingTransactionRecord {
                                            event_id: ev_id,
                                            from_id: corp_id,
                                            to_id: char_id,
                                            modification: delta,
                                            event_type_id: 83,
                                        });
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
                    });
                    match res {
                        Ok((m_sys_id, ball_ids, new_state, penalty_applied, was_accepted)) => {
                            if m_sys_id > 0 && !ball_ids.is_empty() {
                                let handle = ctx.systems.get_or_spawn(m_sys_id);
                                for bid in ball_ids {
                                    let _ = handle.remove_ball(bid).await;
                                }
                            }
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

                _ => Some(Err(BridgeRpcError {
                    code: "InvalidDialogueAction".into(),
                    message: format!("Unknown agent dialogue action {action_id}"),
                })),
            }
        }

        ("agentMgr", "RemoveOfferFromJournal") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
                }));
            };
            let (agent_id, _) = resolve_target_agent_id(ctx, session, char_id, sys_id, args, _kwargs);
            let mission_id = match parse_mission_id(args).or_else(|_| {
                resolve_target_mission_id(ctx, char_id, agent_id, None, args, _kwargs)
            }) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(m) = snap.active_missions.get_mut(&(char_id, mission_id)) else {
                    return Err(BridgeRpcError {
                        code: "MissionNotActive".into(),
                        message: format!(
                            "Mission {mission_id} is not active or offered for character {char_id}"
                        ),
                    });
                };
                if !matches!(m.state.as_str(), "Offered" | "Accepted") {
                    return Err(BridgeRpcError {
                        code: "MissionNotActive".into(),
                        message: format!(
                            "Mission {mission_id} is in terminal state '{}' and cannot be abandoned",
                            m.state
                        ),
                    });
                }
                let was_accepted = m.state == "Accepted";
                let m_sys_id = m.solar_system_id;
                let m_agent_id = m.agent_id;
                let now = ctx.now_unix();
                let mut standing_penalty_applied = false;

                m.state = if was_accepted {
                    "Quit".into()
                } else {
                    "Declined".into()
                };
                m.npcs_remaining = 0;
                let ball_ids = std::mem::take(&mut m.spawned_npc_ball_ids);

                let corp_id = ctx
                    .sde
                    .agents
                    .get(&m_agent_id)
                    .map(|a| a.corporation_id)
                    .unwrap_or_else(|| {
                        resolve_agent_and_corp_for_system(
                            ctx,
                            char_id,
                            m_sys_id,
                            session.station_id,
                            false,
                        )
                        .1
                    });

                if was_accepted {
                    if ctx.sde.missions.contains_key(&mission_id) {
                        let mut actual_delta = None;
                        if let Some(ch) = snap.characters.get_mut(&char_id) {
                            let cur = ch.standings.get(&corp_id).copied().unwrap_or(0.0);
                            let next = (cur - 0.1).max(-10.0);
                            ch.standings.insert(corp_id, next);
                            actual_delta = Some(if cur - 0.1 >= -10.0 { -0.1 } else { -10.0 - cur });
                        }
                        if let Some(delta) = actual_delta {
                            snap.next_entity_id += 1;
                            let ev_id = snap.next_entity_id;
                            snap.standing_transactions
                                .entry(char_id)
                                .or_default()
                                .push(StandingTransactionRecord {
                                    event_id: ev_id,
                                    from_id: corp_id,
                                    to_id: char_id,
                                    modification: delta,
                                    event_type_id: 83,
                                });
                        }
                        standing_penalty_applied = true;
                    }
                    snap.bookmarks.retain(|_, b| {
                        !(b.owner_id == char_id
                            && b.solar_system_id == m_sys_id
                            && b.memo.contains("Mission Pocket"))
                    });
                } else {
                    let last_decline = snap
                        .last_mission_decline_times
                        .get(&char_id)
                        .copied()
                        .unwrap_or(0);
                    if now.saturating_sub(last_decline) < 14_400 {
                        let mut actual_delta = None;
                        if let Some(ch) = snap.characters.get_mut(&char_id) {
                            let cur = ch.standings.get(&corp_id).copied().unwrap_or(0.0);
                            let next = (cur - 0.05).max(-10.0);
                            ch.standings.insert(corp_id, next);
                            actual_delta = Some(if cur - 0.05 >= -10.0 { -0.05 } else { -10.0 - cur });
                        }
                        if let Some(delta) = actual_delta {
                            snap.next_entity_id += 1;
                            let ev_id = snap.next_entity_id;
                            snap.standing_transactions
                                .entry(char_id)
                                .or_default()
                                .push(StandingTransactionRecord {
                                    event_id: ev_id,
                                    from_id: corp_id,
                                    to_id: char_id,
                                    modification: delta,
                                    event_type_id: 83,
                                });
                        }
                        standing_penalty_applied = true;
                    }
                    snap.last_mission_decline_times.insert(char_id, now);
                }

                Ok((m_sys_id, ball_ids, m.state.clone(), standing_penalty_applied, was_accepted))
            });
            match res {
                Ok((m_sys_id, ball_ids, new_state, penalty_applied, was_accepted)) => {
                    if m_sys_id > 0 && !ball_ids.is_empty() {
                        let handle = ctx.systems.get_or_spawn(m_sys_id);
                        for bid in ball_ids {
                            let _ = handle.remove_ball(bid).await;
                        }
                    }
                    Some(Ok(PyRep::keyval([
                        ("missionID", PyRep::Integer(mission_id as i64)),
                        ("state", PyRep::String(new_state)),
                        ("standingPenaltyApplied", PyRep::Bool(penalty_applied)),
                        ("dungeonCleaned", PyRep::Bool(was_accepted)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("keeper", "ActivateAccelerationGate") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if session.station_id.is_some() || session.structure_id.is_some() {
                return Some(Err(BridgeRpcError {
                    code: "ShipDocked".into(),
                    message: "Cannot activate dungeon acceleration gate while docked".into(),
                }));
            }
            if let Some(raw_id) = args.first().and_then(|a| a.as_i64()) {
                if raw_id >= 100_000_000 {
                    let is_mission = u32::try_from(raw_id)
                        .ok()
                        .is_some_and(|mid| ctx.db.with_snapshot(|snap| snap.active_missions.contains_key(&(char_id, mid))));
                    if !is_mission {
                        return None;
                    }
                }
            }
            let mission_id = match parse_mission_id(args) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(m) = snap.active_missions.get_mut(&(char_id, mission_id)) else {
                    return Err(BridgeRpcError {
                        code: "MissionNotActive".into(),
                        message: format!(
                            "Mission {mission_id} is not active for character {char_id}"
                        ),
                    });
                };
                if m.state != "Accepted" {
                    return Err(BridgeRpcError {
                        code: "InvalidMissionState".into(),
                        message: format!(
                            "Mission {mission_id} is in state '{}', not Accepted",
                            m.state
                        ),
                    });
                }
                if m.npcs_remaining > 0 {
                    return Err(BridgeRpcError {
                        code: "RoomNpcsRemaining".into(),
                        message: format!(
                            "Cannot activate acceleration gate while {} room NPC(s) remain",
                            m.npcs_remaining
                        ),
                    });
                }
                m.current_room_id += 1;
                Ok(m.current_room_id)
            });
            match res {
                Ok(next_room) => Some(Ok(PyRep::keyval([
                    ("missionID", PyRep::Integer(mission_id as i64)),
                    ("currentRoomID", PyRep::Integer(next_room as i64)),
                    ("gateUnlocked", PyRep::Bool(true)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        // AIR NPE Node Graph `473` & AIR Career Program (`air_npe` / `seasonManager`, `E2E-16`)
        ("seasonManager", "process_client_event") | ("air_npe", "skip_air_npe") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let npe = snap.npe_progress.entry(char_id).or_insert_with(|| {
                    eve_db::NpeProgressRecord {
                        character_id: char_id,
                        node_graph_id: 473,
                        current_node_id: 1,
                        completed: false,
                        career_goal_progress: Default::default(),
                        claimed_goals: Default::default(),
                    }
                });
                if npe.completed {
                    return Err(BridgeRpcError {
                        code: "NodeGraphAlreadyCompleted".into(),
                        message: "AIR NPE node graph 473 is already completed".into(),
                    });
                }
                // Node 3 ("DestroySeekers") requires at least 1 NPC kill recorded in career_goal_progress[1]
                if npe.current_node_id == 3
                    && npe.career_goal_progress.get(&1).copied().unwrap_or(0) < 1
                {
                    return Err(BridgeRpcError {
                        code: "NodeObjectiveNotMet".into(),
                        message:
                            "Node 3 (DestroySeekers) requires destroying at least 1 NPC before advancing"
                                .into(),
                    });
                }
                if npe.current_node_id < 4 {
                    npe.current_node_id += 1;
                }
                if npe.current_node_id >= 4 {
                    npe.completed = true;
                }
                Ok((npe.current_node_id, npe.completed))
            });
            match res {
                Ok((curr_node, completed)) => Some(Ok(PyRep::keyval([
                    ("nodeGraphID", PyRep::Integer(473)),
                    ("currentNodeID", PyRep::Integer(curr_node as i64)),
                    ("completed", PyRep::Bool(completed)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("air_npe", "get_air_npe_state") | ("air_npe", "is_air_npe_enabled") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let (curr_node, completed) = ctx.db.with_snapshot(|snap| {
                snap.npe_progress
                    .get(&char_id)
                    .map(|n| (n.current_node_id, n.completed))
                    .unwrap_or((1, false))
            });
            Some(Ok(PyRep::keyval([
                ("nodeGraphID", PyRep::Integer(473)),
                ("currentNodeID", PyRep::Integer(curr_node as i64)),
                ("completed", PyRep::Bool(completed)),
                ("enabled", PyRep::Bool(true)),
            ])))
        }

        ("seasonManager", "get_character_progress_by_character_id") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let raw_goal_id = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
            let Ok(goal_id) = u32::try_from(raw_goal_id) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidGoalId".into(),
                    message: format!("goal_id {raw_goal_id} is out of valid u32 range"),
                }));
            };
            if resolve_career_goal_def(ctx, goal_id).is_none() {
                return Some(Err(BridgeRpcError {
                    code: "UnimplementedRPC".into(),
                    message: format!(
                        "Career goal {goal_id} is not present in V24.01 goals.fsdbinary"
                    ),
                }));
            }
            let prog = ctx.db.with_snapshot(|snap| {
                if goal_id == 2 {
                    let total_mined: u64 = snap
                        .mining_ledger
                        .iter()
                        .filter(|r| r.character_id == char_id)
                        .map(|r| r.quantity.max(0) as u64)
                        .sum();
                    total_mined
                } else {
                    snap.npe_progress
                        .get(&char_id)
                        .and_then(|n| n.career_goal_progress.get(&goal_id).copied())
                        .unwrap_or(0)
                }
            });
            Some(Ok(PyRep::keyval([
                ("goalID", PyRep::Integer(goal_id as i64)),
                ("currentProgress", PyRep::Integer(prog as i64)),
                ("verifiedFromLedger", PyRep::Bool(goal_id == 2)),
            ])))
        }

        ("seasonManager", "claim_goal_reward") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let raw_goal_id = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
            let Ok(goal_id) = u32::try_from(raw_goal_id) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidGoalId".into(),
                    message: format!("goal_id {raw_goal_id} is out of valid u32 range"),
                }));
            };
            let Some((target_prog, sp_reward)) = resolve_career_goal_def(ctx, goal_id) else {
                return Some(Err(BridgeRpcError {
                    code: "UnimplementedRPC".into(),
                    message: format!(
                        "Career goal {goal_id} is not present in V24.01 goals.fsdbinary"
                    ),
                }));
            };

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let npe = snap.npe_progress.entry(char_id).or_insert_with(|| {
                    eve_db::NpeProgressRecord {
                        character_id: char_id,
                        node_graph_id: 473,
                        current_node_id: 1,
                        completed: false,
                        career_goal_progress: Default::default(),
                        claimed_goals: Default::default(),
                    }
                });
                let cur_prog = if goal_id == 2 {
                    let total_mined: u64 = snap
                        .mining_ledger
                        .iter()
                        .filter(|r| r.character_id == char_id)
                        .map(|r| r.quantity.max(0) as u64)
                        .sum();
                    npe.career_goal_progress.insert(goal_id, total_mined);
                    total_mined
                } else {
                    npe.career_goal_progress.get(&goal_id).copied().unwrap_or(0)
                };
                if cur_prog < target_prog {
                    return Err(BridgeRpcError {
                        code: "GoalProgressIncomplete".into(),
                        message: format!(
                            "Career goal {goal_id} requires progress {target_prog}, current is {cur_prog}"
                        ),
                    });
                }
                if !npe.claimed_goals.insert(goal_id) {
                    return Err(BridgeRpcError {
                        code: "GoalAlreadyClaimed".into(),
                        message: format!("Career goal {goal_id} has already been claimed"),
                    });
                }
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.free_skill_points += sp_reward;
                    Ok(ch.free_skill_points)
                } else {
                    Err(BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {char_id} not found"),
                    })
                }
            });

            match res {
                Ok(new_free_sp) => Some(Ok(PyRep::keyval([
                    ("goalID", PyRep::Integer(goal_id as i64)),
                    ("spReward", PyRep::Real(sp_reward)),
                    ("freeSkillPoints", PyRep::Real(new_free_sp)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        _ => None,
    }
}
