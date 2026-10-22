//! `agents_missions_helpers` — Helper logic, reward formulas, and resolution for agent missions.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_db::StandingTransactionRecord;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

pub fn parse_mission_id(args: &[PyRep]) -> Result<u32, BridgeRpcError> {
    let raw = args.first().and_then(|a| a.as_i64()).ok_or_else(|| BridgeRpcError {
        code: "InvalidMissionId".into(),
        message: "mission_id argument is required".into(),
    })?;
    u32::try_from(raw).map_err(|_| BridgeRpcError {
        code: "InvalidMissionId".into(),
        message: format!("mission_id {raw} is out of valid u32 range"),
    })
}

pub fn is_storyline_template(tmpl: &eve_sde::MissionTemplate) -> bool {
    let t = tmpl.title.to_lowercase();
    t.contains("storyline") || t.contains("important") || t.contains("materials for war")
}

pub fn is_storyline_agent_type(agent_type_id: u32) -> bool {
    matches!(agent_type_id, 6 | 7)
}

pub fn is_unsupported_direct_agent_type(agent_type_id: u32) -> bool {
    matches!(agent_type_id, 11 | 12)
}

pub fn is_rnd_agent(agent: &eve_sde::AgentInfo) -> bool {
    agent.agent_type_id == 4 || agent.division_id == 18
}

pub fn is_fw_agent(agent: &eve_sde::AgentInfo) -> bool {
    matches!(
        agent.corporation_id,
        1000121 | 1000147 | 1000085 | 1000009
    )
}

pub fn rnd_tritanium_required_qty(agent_level: u8) -> i32 {
    // Scaled by agent level: Level 1: 300, Level 2: 900, Level 3: 2700, Level 4: 8100, Level 5: 24300
    let lvl = agent_level.clamp(1, 5) as u32;
    300 * 3_i32.pow(lvl - 1)
}

pub fn mission_bonus_duration(agent_level: u8) -> u64 {
    // Bonus timer of 10 min (600s) to 6 h (21,600s)
    let dur = match agent_level {
        1 => 1_800,  // 30 min
        2 => 3_600,  // 1 hour
        3 => 7_200,  // 2 hours
        4 => 14_400, // 4 hours
        _ => 21_600, // 6 hours
    };
    dur.clamp(600, 21_600)
}

pub fn mission_matches_agent_pool(
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
            18 => ct.contains("Research") || tmpl.is_courier,
            22 => ct.contains("BasicCourier"),
            23 => ct.contains("BasicMining") || tmpl.title.to_lowercase().contains("mining"),
            24 => ct.contains("BasicKill"),
            37 => ct.contains("Heraldry") || ct.contains("BasicKill"),
            _ => ct.contains("BasicKill") || ct.contains("BasicCourier"),
        },
    }
}

pub fn resolve_fw_warzone_system(agent_corp_id: i64) -> u32 {
    // Selects a canonical FW warzone system appropriate for militia
    match agent_corp_id {
        1000121 => 30002062, // Federal Defense Union -> Onnamon
        1000147 => 30002097, // 24th Imperial Crusade -> Mehatoor
        1000085 => 30002098, // Tribal Liberation Force -> Amo
        1000009 => 30002061, // State Protectorate -> Pynekoh
        _ => 30002062,
    }
}

pub fn resolve_agent_and_corp_for_system(
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
                ctx.sde.agents.values().filter(|a| {
                    a.corporation_id == primary_corp && is_storyline_agent_type(a.agent_type_id)
                }).min_by_key(|a| a.agent_id)
            })
            .or_else(|| {
                ctx.sde.agents.values().filter(|a| {
                    is_storyline_agent_type(a.agent_type_id)
                        && a.solar_system_id.and_then(|s| ctx.sde.solar_systems.get(&s)).map(|s| s.constellation_id)
                            == cur_constellation
                }).min_by_key(|a| a.agent_id)
            })
            .or_else(|| {
                ctx.sde.agents.values().filter(|a| {
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
        station_id
            .and_then(|sid| {
                ctx.sde.agents.values().filter(|a| {
                    a.solar_system_id == Some(sys_id)
                        && a.station_id == Some(sid)
                        && is_eligible_regular(a)
                }).min_by_key(|a| (a.level, a.agent_id))
            })
            .or_else(|| {
                ctx.sde.agents.values().filter(|a| {
                    a.solar_system_id == Some(sys_id)
                        && a.corporation_id == primary_corp
                        && is_eligible_regular(a)
                }).min_by_key(|a| (a.level, a.agent_id))
            })
            .or_else(|| {
                ctx.sde.agents
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

pub fn resolve_mission_npc_type(ctx: &ServerContext) -> u32 {
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
        (50.0..=450.0).contains(&(shield + armor + hull))
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

/// # feat:pve.mission-lp-isk-reward-formula
/// Mission LP reward = base LP x (1.6288 - the agent system's true security) x (1 + 0.1 x matching division Connections skill: Distribution, Mining or Security).
/// ISK reward is +5% per Negotiation level. Both credited and journaled on completion.
pub fn compute_character_mission_rewards(
    ctx: &ServerContext,
    char_id: i64,
    agent_id: i64,
    mission_id: u32,
    sys_id: u32,
) -> (i64, i64, i64) {
    let tmpl = ctx.sde.missions.get(&mission_id);
    let agent_info = ctx.sde.agents.get(&agent_id);
    let agent_sys_id = agent_info.and_then(|a| a.solar_system_id).unwrap_or(sys_id);
    let true_sec = ctx
        .sde
        .solar_systems
        .get(&agent_sys_id)
        .map(|s| s.security_status)
        .unwrap_or(1.0);

    let (is_alpha, neg_lvl, dist_conn, mining_conn, sec_conn) = ctx.db.with_snapshot(|snap| {
        let alpha = snap
            .characters
            .get(&char_id)
            .is_some_and(|c| c.clone_grade <= 1);
        let sk = snap.skills.get(&char_id);
        let neg = sk.and_then(|m| m.get(&3356)).map_or(0, |s| s.level);
        let dist = sk.and_then(|m| m.get(&3894)).map_or(0, |s| s.level);
        let mining = sk.and_then(|m| m.get(&3893)).map_or(0, |s| s.level);
        let sec = sk.and_then(|m| m.get(&3895)).map_or(0, |s| s.level);
        (alpha, neg, dist, mining, sec)
    });

    let base_lp = tmpl.map_or(100, |t| {
        if is_alpha && t.lp_alpha > 0 {
            t.lp_alpha
        } else {
            t.lp_omega.max(100)
        }
    });

    let matching_conn_lvl = match agent_info.map(|a| a.division_id) {
        Some(27) => dist_conn,  // Distribution
        Some(26) => mining_conn, // Mining
        Some(24) => sec_conn,    // Security
        _ => {
            if tmpl.is_some_and(|t| t.is_courier) {
                dist_conn
            } else if tmpl.is_some_and(|t| t.title.to_lowercase().contains("mining")) {
                mining_conn
            } else {
                sec_conn
            }
        }
    };

    // LP reward = base LP x (1.6288 - true_sec) x (1 + 0.1 x matching division Connections skill)
    let sec_factor = (1.6288 - true_sec).max(0.01);
    let conn_factor = 1.0 + 0.10 * (matching_conn_lvl.min(5) as f64);
    let lp_reward = ((base_lp as f64) * sec_factor * conn_factor).round() as i64;

    // ISK reward = base ISK x (1 + 0.05 x Negotiation level)
    let base_isk = base_lp.saturating_mul(1_000);
    let neg_factor = 1.0 + 0.05 * (neg_lvl.min(5) as f64);
    let isk_reward = ((base_isk as f64) * neg_factor).round() as i64;
    let bonus_reward = isk_reward / 2;

    (isk_reward, lp_reward, bonus_reward)
}

pub fn resolve_target_agent_id(
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
        map.values()
            .filter(|b| {
                b.session_id == session.session_id && b.service == "agentMgr" && b.object_id > 0
            })
            .max_by_key(|b| b.bound_id)
            .map(|b| b.object_id)
    }) {
        return (bound_agent, true);
    }
    let (default_aid, _) =
        resolve_agent_and_corp_for_system(ctx, char_id, sys_id, session.station_id, false);
    (default_aid, false)
}

pub fn resolve_target_mission_id(
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

pub async fn abandon_or_decline_mission(
    ctx: &ServerContext,
    session: &SessionState,
    char_id: i64,
    mission_id: u32,
) -> Result<(String, bool, bool), BridgeRpcError> {
    let (m_sys_id, ball_ids, new_state, penalty_applied, was_accepted) =
        ctx.db.try_with_mut_snapshot(|snap| {
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

            let is_rnd = ctx
                .sde
                .agents
                .get(&m_agent_id)
                .is_some_and(|a| is_rnd_agent(a));

            if was_accepted {
                if ctx.sde.missions.contains_key(&mission_id) {
                    let mut actual_delta = None;
                    if let Some(ch) = snap.characters.get_mut(&char_id) {
                        let cur = ch.standings.get(&corp_id).copied().unwrap_or(0.0);
                        let next = (cur - 0.1).max(-10.0);
                        ch.standings.insert(corp_id, next);
                        let ag_cur = ch.standings.get(&m_agent_id).copied().unwrap_or(0.0);
                        ch.standings.insert(m_agent_id, (ag_cur - 0.1).max(-10.0));
                        actual_delta =
                            Some(if cur - 0.1 >= -10.0 { -0.1 } else { -10.0 - cur });
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
            } else if !is_rnd {
                // R&D agent mission decline has no standing penalty and does not stop research
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
                        let ag_cur = ch.standings.get(&m_agent_id).copied().unwrap_or(0.0);
                        ch.standings.insert(m_agent_id, (ag_cur - 0.05).max(-10.0));
                        actual_delta =
                            Some(if cur - 0.05 >= -10.0 { -0.05 } else { -10.0 - cur });
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
        })?;

    if m_sys_id > 0 && !ball_ids.is_empty() {
        let handle = ctx.systems.get_or_spawn(m_sys_id);
        for bid in ball_ids {
            let _ = handle.remove_ball(bid).await;
        }
    }

    Ok((new_state, penalty_applied, was_accepted))
}
