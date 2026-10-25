//! `agents_missions_helpers` — Helper logic, reward formulas, and resolution for agent missions.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use super::agents_standings_arcs::{load_mission_meta, save_mission_meta};
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
/// // DERIVED: Formulaic mission reward calculation (base ISK = base LP * 1000)
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

    // DERIVED: Formulaic mission reward calculation (base ISK = base LP * 1000)
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

pub fn reclaim_courier_cargo(
    snap: &mut eve_db::WorldStateSnapshot,
    char_id: i64,
    mission_id: u32,
) {
    if let Some(mut meta) = load_mission_meta(snap, char_id, mission_id) {
        // Only run reclaim for courier missions that were actually accepted (R-MSN-RECLAIM-01, R-MSN-MINT-03)
        if meta.granted_courier_item_id.is_some() || meta.granted_courier_qty > 0 {
            let cid = meta.granted_courier_item_id.take();
            let tag = format!("courier_cargo:{char_id}:{mission_id}");
            let mut remaining_to_reclaim = meta.granted_courier_qty.max(1);
            let tagged_items: Vec<(i64, i32)> = snap
                .items
                .iter()
                .filter(|(&iid, it)| {
                    it.owner_id == char_id
                        && (Some(iid) == cid
                            || snap
                                .item_custom_names
                                .get(&iid)
                                .map_or(false, |name| name == &tag))
                })
                .map(|(&iid, it)| (iid, it.quantity))
                .collect();
            for (iid, qty) in tagged_items {
                if qty <= remaining_to_reclaim {
                    snap.items.remove(&iid);
                    snap.item_custom_names.remove(&iid);
                    remaining_to_reclaim -= qty;
                } else {
                    if let Some(it) = snap.items.get_mut(&iid) {
                        it.quantity -= remaining_to_reclaim;
                        it.version += 1;
                    }
                    snap.item_custom_names.remove(&iid);
                    remaining_to_reclaim = 0;
                }
            }
            meta.granted_courier_item_id = None;
            meta.granted_courier_qty = 0;
            save_mission_meta(snap, char_id, &meta);
        }
    }
}

pub async fn abandon_or_decline_mission(
    ctx: &ServerContext,
    session: &SessionState,
    char_id: i64,
    mission_id: u32,
) -> Result<(String, bool, bool), BridgeRpcError> {
    let (m_sys_id, ball_ids, new_state, penalty_applied, was_accepted) =
        ctx.db.try_with_mut_snapshot(|snap| {
            let was_accepted;
            let m_sys_id;
            let m_agent_id;
            let ball_ids;
            let new_state: String;
            {
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
                was_accepted = m.state == "Accepted";
                m_sys_id = m.solar_system_id;
                m_agent_id = m.agent_id;
                new_state = if was_accepted {
                    "Quit".into()
                } else {
                    "Declined".into()
                };
                m.state = new_state.clone();
                m.npcs_remaining = 0;
                ball_ids = std::mem::take(&mut m.spawned_npc_ball_ids);
            }
            let now = ctx.now_unix();
            let mut standing_penalty_applied = false;

            // Reclaim granted courier cargo on quit/decline to prevent commodity minting
            reclaim_courier_cargo(snap, char_id, mission_id);
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
                new_state,
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

#[derive(Debug, Clone)]
pub struct CourierMissionDetails {
    pub cargo_type_id: eve_db::TypeId,
    pub cargo_quantity: i32,
    pub destination_station_id: i64,
    pub origin_station_id: i64,
}

// DERIVED: Courier mission cargo type and quantity formulaically derived from mission title keywords and template ID
pub fn resolve_courier_mission_details(
    ctx: &ServerContext,
    tmpl: &eve_sde::MissionTemplate,
    agent: &eve_sde::AgentInfo,
) -> Result<CourierMissionDetails, BridgeRpcError> {
    let title_lower = tmpl.title.to_lowercase();
    // DERIVED: Cargo type ID and quantity mapped from mission title keywords
    let (cargo_type_id, cargo_quantity) = if title_lower.contains("marine") {
        (3810, 10 + ((tmpl.mission_id % 5) as i32) * 2)
    } else if title_lower.contains("militant") {
        (25373, 10 + ((tmpl.mission_id % 5) as i32) * 2)
    } else if title_lower.contains("arms") || title_lower.contains("weapon") {
        (9844, 10 + ((tmpl.mission_id % 6) as i32) * 2)
    } else if title_lower.contains("drug")
        || title_lower.contains("medicine")
        || title_lower.contains("antibiotic")
    {
        (43, 20 + ((tmpl.mission_id % 10) as i32) * 5)
    } else if title_lower.contains("party") || title_lower.contains("spirits") {
        (9850, 20 + ((tmpl.mission_id % 10) as i32) * 5)
    } else if title_lower.contains("tobacco") {
        (9852, 20 + ((tmpl.mission_id % 10) as i32) * 5)
    } else if title_lower.contains("quafe") {
        (3699, 30 + ((tmpl.mission_id % 10) as i32) * 5)
    } else if title_lower.contains("wheat") || title_lower.contains("food") {
        (3673, 25 + ((tmpl.mission_id % 10) as i32) * 5)
    } else if title_lower.contains("tool") || title_lower.contains("breakdown") {
        (21632, 10 + ((tmpl.mission_id % 5) as i32) * 2)
    } else if title_lower.contains("document")
        || title_lower.contains("report")
        || title_lower.contains("data")
        || title_lower.contains("secret")
        || title_lower.contains("tidings")
        || title_lower.contains("lead")
        || title_lower.contains("outbreak")
        || title_lower.contains("interrogation")
        || title_lower.contains("piracy")
        || title_lower.contains("gift")
    {
        (3814, 5 + ((tmpl.mission_id % 5) as i32))
    } else {
        let fallback_pool: [eve_db::TypeId; 8] = [3814, 3810, 25373, 9844, 43, 9850, 3699, 21632];
        let tid = fallback_pool[(tmpl.mission_id as usize) % fallback_pool.len()];
        let qty = 10 + ((tmpl.mission_id % 7) as i32) * 2;
        (tid, qty)
    };

    let agent_station = agent.station_id.or_else(|| {
        agent.solar_system_id.and_then(|sys| {
            ctx.sde.stations.values().filter(|st| st.solar_system_id == sys).map(|st| st.station_id).min()
        })
    }).ok_or_else(|| BridgeRpcError {
        code: "InvalidMissionLocation".into(),
        message: format!("Agent {} has no station or solar system in SDE", agent.agent_id),
    })?;

    let agent_sys = agent.solar_system_id.or_else(|| {
        ctx.sde.stations.get(&agent_station).map(|st| st.solar_system_id)
    }).ok_or_else(|| BridgeRpcError {
        code: "AgentMissingSolarSystem".into(),
        message: format!("Agent {} has no resolvable solar system in SDE", agent.agent_id),
    })?;
    let agent_constellation = ctx.sde.solar_systems.get(&agent_sys).map(|s| s.constellation_id);

    let mut candidate_stations: Vec<i64> = ctx
        .sde
        .stations
        .values()
        .filter(|st| {
            st.station_id != agent_station
                && ctx
                    .sde
                    .solar_systems
                    .get(&st.solar_system_id)
                    .map(|s| s.constellation_id)
                    == agent_constellation
        })
        .map(|st| st.station_id)
        .collect();
    candidate_stations.sort_unstable();

    // DERIVED: Destination station selected deterministically from reachable candidate stations in constellation or region
    let destination_station_id = if !candidate_stations.is_empty() {
        candidate_stations[(tmpl.mission_id as usize) % candidate_stations.len()]
    } else {
        let agent_region = ctx.sde.solar_systems.get(&agent_sys).map(|s| s.region_id);
        let mut reg_stations: Vec<i64> = ctx
            .sde
            .stations
            .values()
            .filter(|st| {
                st.station_id != agent_station
                    && ctx
                        .sde
                        .solar_systems
                        .get(&st.solar_system_id)
                        .map(|s| s.region_id)
                        == agent_region
            })
            .map(|st| st.station_id)
            .collect();
        reg_stations.sort_unstable();
        if !reg_stations.is_empty() {
            reg_stations[(tmpl.mission_id as usize) % reg_stations.len()]
        } else {
            ctx.sde
                .stations
                .keys()
                .copied()
                .find(|&sid| sid != agent_station)
                .ok_or_else(|| BridgeRpcError {
                    code: "NoDestinationStationAvailable".into(),
                    message: format!("No destination station available for courier mission {}", tmpl.mission_id),
                })?
        }
    };

    Ok(CourierMissionDetails {
        cargo_type_id,
        cargo_quantity,
        destination_station_id,
        origin_station_id: agent_station,
    })
}

#[derive(Debug, Clone)]
pub struct TradeMissionDetails {
    pub trade_type_id: eve_db::TypeId,
    pub trade_quantity: i32,
}

// DERIVED: Trade mission required commodity type and quantity derived from mission title keywords and template ID
pub fn resolve_trade_mission_details(
    _ctx: &ServerContext,
    tmpl: &eve_sde::MissionTemplate,
) -> TradeMissionDetails {
    let title_lower = tmpl.title.to_lowercase();
    // DERIVED: Commodity type ID and quota mapped from title keywords and template ID
    let (trade_type_id, trade_quantity) = if title_lower.contains("trit") {
        (34, 1_000 + ((tmpl.mission_id % 10) as i32) * 100)
    } else if title_lower.contains("entertainment") || title_lower.contains("holoreel") {
        (3647, 10 + ((tmpl.mission_id % 5) as i32) * 5)
    } else if title_lower.contains("microorganism") || title_lower.contains("antibiotic") {
        (43, 25 + ((tmpl.mission_id % 10) as i32) * 5)
    } else if title_lower.contains("oil") || title_lower.contains("synthetic") {
        (3691, 15 + ((tmpl.mission_id % 10) as i32) * 5)
    } else if title_lower.contains("wheat") || title_lower.contains("grain") {
        (3673, 25 + ((tmpl.mission_id % 10) as i32) * 5)
    } else if title_lower.contains("oxygen") || title_lower.contains("gas") {
        (3683, 20 + ((tmpl.mission_id % 10) as i32) * 5)
    } else if title_lower.contains("livestock") || title_lower.contains("cattle") || title_lower.contains("animal") {
        (3725, 10 + ((tmpl.mission_id % 5) as i32) * 5)
    } else if title_lower.contains("food") || title_lower.contains("goods") {
        (3673, 20 + ((tmpl.mission_id % 10) as i32) * 5)
    } else if title_lower.contains("drill") || title_lower.contains("tool") {
        (21632, 10 + ((tmpl.mission_id % 5) as i32) * 2)
    } else if title_lower.contains("laser") || title_lower.contains("electronic") {
        (3687, 15 + ((tmpl.mission_id % 5) as i32) * 5)
    } else if title_lower.contains("wine") || title_lower.contains("spirits") {
        (42, 20 + ((tmpl.mission_id % 10) as i32) * 5)
    } else if title_lower.contains("pyroxeres") {
        (1224, 200 + ((tmpl.mission_id % 5) as i32) * 50)
    } else if title_lower.contains("scordite") {
        (1228, 300 + ((tmpl.mission_id % 5) as i32) * 50)
    } else {
        let pool: [(eve_db::TypeId, i32); 8] = [
            (43, 25),    // Antibiotics
            (3691, 20),  // Synthetic Oil
            (3673, 30),  // Wheat
            (3683, 25),  // Oxygen
            (3725, 10),  // Livestock
            (3647, 15),  // Holoreels
            (34, 1000),  // Tritanium
            (21632, 10), // Tools
        ];
        let (tid, base_qty) = pool[(tmpl.mission_id as usize) % pool.len()];
        let qty = if tid == 34 {
            base_qty + ((tmpl.mission_id % 10) as i32) * 100
        } else {
            base_qty + ((tmpl.mission_id % 5) as i32) * 5
        };
        (tid, qty)
    };

    TradeMissionDetails {
        trade_type_id,
        trade_quantity,
    }
}

#[derive(Debug, Clone)]
pub struct MiningMissionDetails {
    pub ore_type_id: eve_db::TypeId,
    pub required_units: i32,
    pub objective_text: String,
}

// DERIVED: Mining mission target ore type and quantity derived from mission title keywords and template ID
pub fn resolve_mining_mission_details(
    ctx: &ServerContext,
    tmpl: &eve_sde::MissionTemplate,
) -> MiningMissionDetails {
    let title_lower = tmpl.title.to_lowercase();
    // DERIVED: Ore type ID and required units mapped from title keywords
    let (ore_type_id, required_units) = if title_lower.contains("banidine") {
        (28617, 500)
    } else if title_lower.contains("augumene") {
        (28618, 500)
    } else if title_lower.contains("mercium") {
        (28619, 500)
    } else if title_lower.contains("lyavite") {
        (28620, 250)
    } else if title_lower.contains("pithix") {
        (28621, 250)
    } else if title_lower.contains("arisite") {
        (28622, 100)
    } else if title_lower.contains("geodite") {
        (28624, 50)
    } else if title_lower.contains("gneiss") {
        (1229, 100)
    } else if title_lower.contains("omber") {
        (1227, 500)
    } else if title_lower.contains("kernite") {
        (20, 250)
    } else if title_lower.contains("pyroxeres") {
        (1224, 500)
    } else if title_lower.contains("scordite") {
        (1228, 500)
    } else if title_lower.contains("plagioclase") {
        (18, 500)
    } else if title_lower.contains("veldspar") {
        (1230, 1000)
    } else {
        let pool: [(eve_db::TypeId, i32); 4] = [
            (1230, 1000), // Veldspar
            (1228, 750),  // Scordite
            (1224, 500),  // Pyroxeres
            (20, 500),    // Kernite
        ];
        pool[(tmpl.mission_id as usize) % pool.len()]
    };

    let ore_name = ctx
        .sde
        .types
        .get(&ore_type_id)
        .map(|t| t.name.as_str())
        .unwrap_or("ore");
    let objective_text = format!("Mine {required_units} units of {ore_name} and deliver to the agent station hangar.");

    MiningMissionDetails {
        ore_type_id,
        required_units,
        objective_text,
    }
}

#[derive(Debug, Clone)]
pub struct SecurityMissionEncounter {
    pub dungeon_id: u32,
    pub dungeon_name: String,
    pub faction_id: u32,
    pub spawn_group_id: u32,
    pub npc_type_ids: Vec<eve_db::TypeId>,
    pub wave_npc_count: u32,
    pub objective_text: String,
    pub briefing_text: String,
}

// DERIVED: Security mission dungeon encounter, target faction, and wave parameters derived from mission title, agent level, and SDE dungeons
pub fn resolve_security_mission_encounter(
    ctx: &ServerContext,
    tmpl: &eve_sde::MissionTemplate,
    agent: &eve_sde::AgentInfo,
    sys_id: u32,
) -> SecurityMissionEncounter {
    let agent_level = agent.level.clamp(1, 5) as u32;
    let agent_sys_id = agent.solar_system_id.unwrap_or(sys_id);
    let region_id = ctx.sde.solar_systems.get(&agent_sys_id).map(|s| s.region_id);
    let region_name = region_id
        .and_then(|rid| ctx.sde.regions.get(&rid))
        .map(|r| r.name.to_lowercase());
    let sys_name = ctx
        .sde
        .solar_systems
        .get(&agent_sys_id)
        .map(|s| s.name.to_lowercase());
    let region_faction = region_id
        .and_then(|rid| ctx.sde.regions.get(&rid))
        .and_then(|r| r.faction_id);
    let agent_faction = ctx
        .sde
        .npc_corporations
        .get(&agent.corporation_id)
        .and_then(|c| c.faction_id)
        .unwrap_or(0);
    let title_clean = tmpl
        .title
        .to_lowercase()
        .replace(['(', ')', '-', ':', '\''], " ");
    let title_words: Vec<&str> = title_clean
        .split_whitespace()
        .filter(|w| w.len() > 3 && !matches!(*w, "the" | "part" | "level" | "mission"))
        .collect();

    // DERIVED: Dungeon matched from title keywords, system/region, agent faction, archetype 20, or SDE pool
    let matched_dungeon = ctx
        .sde
        .dungeons
        .values()
        .filter(|d| {
            let d_name_low = d.name.to_lowercase();
            tmpl.title.to_lowercase().contains(&d_name_low)
                || d_name_low.contains(&tmpl.title.to_lowercase())
                || title_words.iter().filter(|&&w| d_name_low.contains(w)).count() >= 2
        })
        .min_by_key(|d| {
            let diff_dist = d.difficulty.unwrap_or(agent_level).abs_diff(agent_level);
            (diff_dist, d.dungeon_id)
        })
        .or_else(|| {
            // DERIVED: Filter candidate dungeons by matching system or region
            ctx.sde
                .dungeons
                .values()
                .filter(|d| {
                    let d_name_low = d.name.to_lowercase();
                    let sys_match = sys_name.as_ref().is_some_and(|sn| d_name_low.contains(sn));
                    let reg_match = region_name.as_ref().is_some_and(|rn| d_name_low.contains(rn));
                    let fac_match = region_faction.is_some_and(|rf| d.faction_id == Some(rf));
                    (sys_match || reg_match || fac_match)
                        && d.difficulty.unwrap_or(agent_level) == agent_level
                })
                .min_by_key(|d| (d.dungeon_id as usize + tmpl.mission_id as usize) % 1000)
        })
        .or_else(|| {
            if agent_faction > 0 {
                ctx.sde
                    .dungeons
                    .values()
                    .filter(|d| d.faction_id == Some(agent_faction) && d.difficulty == Some(agent_level))
                    .min_by_key(|d| (d.dungeon_id as usize + tmpl.mission_id as usize) % 1000)
            } else {
                None
            }
        })
        .or_else(|| {
            ctx.sde
                .dungeons
                .values()
                .filter(|d| d.archetype_id == Some(20) && d.difficulty == Some(agent_level))
                .min_by_key(|d| (d.dungeon_id as usize + tmpl.mission_id as usize) % 1000)
        })
        .or_else(|| {
            ctx.sde
                .dungeons
                .values()
                .filter(|d| d.archetype_id == Some(20))
                .min_by_key(|d| d.dungeon_id)
        })
        .or_else(|| {
            ctx.sde.dungeons.values().min_by_key(|d| d.dungeon_id)
        });

    let (dungeon_id, dungeon_name, dungeon_faction, dungeon_difficulty) = match matched_dungeon {
        Some(d) => (
            d.dungeon_id,
            d.name.clone(),
            d.faction_id.unwrap_or(agent_faction),
            d.difficulty.unwrap_or(agent_level),
        ),
        None => (
            0,
            "Encounter Pocket".to_string(),
            agent_faction,
            agent_level,
        ),
    };

    // DERIVED: Identify hostile faction from dungeon faction, mission title keywords, or agent faction
    let target_faction_id = if dungeon_faction > 0 {
        dungeon_faction
    } else if title_clean.contains("guristas") {
        500010
    } else if title_clean.contains("angel") {
        500011
    } else if title_clean.contains("blood") {
        500012
    } else if title_clean.contains("sansha") {
        500019
    } else if title_clean.contains("serpentis") {
        500020
    } else if agent_faction > 0 {
        agent_faction
    } else {
        ctx.sde.npc_spawn_groups.values().map(|sg| sg.faction_id).find(|&f| f > 0).unwrap_or(0)
    };

    let target_faction_name = match target_faction_id {
        500010 => "Guristas",
        500011 => "Angel Cartel",
        500012 => "Blood Raiders",
        500019 => "Sansha's Nation",
        500020 => "Serpentis",
        _ => ctx.sde.factions.get(&target_faction_id).map(|f| f.name.as_str()).unwrap_or("Hostile"),
    };

    // DERIVED: Ship class derived from encounter difficulty level (1 = Frigate, 2 = Cruiser, 3+ = Battleship)
    let target_ship_class = match dungeon_difficulty {
        1 => 1,
        2 => 2,
        3 => 2,
        _ => 3,
    };

    let matching_group = ctx
        .sde
        .npc_spawn_groups
        .values()
        .filter(|sg| {
            let name_low = sg.group_name.to_lowercase();
            (name_low.contains(&target_faction_name.to_lowercase()) || sg.faction_id == target_faction_id)
                && sg.ship_class == target_ship_class
                && !sg.npc_type_ids.is_empty()
        })
        .min_by_key(|sg| sg.spawn_group_id)
        .or_else(|| {
            ctx.sde
                .npc_spawn_groups
                .values()
                .filter(|sg| (sg.faction_id == target_faction_id || sg.group_name.contains("Deadspace")) && !sg.npc_type_ids.is_empty())
                .min_by_key(|sg| (sg.ship_class.abs_diff(target_ship_class), sg.spawn_group_id))
        })
        .or_else(|| {
            ctx.sde
                .npc_spawn_groups
                .values()
                .filter(|sg| !sg.npc_type_ids.is_empty())
                .min_by_key(|sg| (sg.ship_class.abs_diff(target_ship_class), sg.spawn_group_id))
        });

    let (spawn_group_id, npc_type_ids) = match matching_group {
        Some(sg) => (sg.spawn_group_id, sg.npc_type_ids.clone()),
        None => (0, vec![resolve_mission_npc_type(ctx)]),
    };

    // DERIVED: Wave NPC count scaled by dungeon difficulty level (3-6 hostile NPC ships)
    let wave_npc_count = match dungeon_difficulty {
        1 => 3,
        2 => 3,
        3 => 4,
        4 => 5,
        _ => 6,
    };

    let sys_name = ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .map(|s| s.name.clone())
        .unwrap_or_else(|| format!("System {sys_id}"));

    let objective_text = format!(
        "Infiltrate {} in {} and eliminate all hostile {} vessels ({} enemies).",
        dungeon_name, sys_name, target_faction_name, wave_npc_count
    );
    let briefing_text = format!(
        "Agent {} operation: {}. Hostile {} forces detected at {}. Neutralize the encounter and report back.",
        agent.agent_id, tmpl.title, target_faction_name, dungeon_name
    );

    SecurityMissionEncounter {
        dungeon_id,
        dungeon_name,
        faction_id: target_faction_id,
        spawn_group_id,
        npc_type_ids,
        wave_npc_count,
        objective_text,
        briefing_text,
    }
}

/// Handles destruction of an NPC in space, decrementing npcs_remaining for any active mission tracking that NPC ball.
pub fn on_mission_npc_destroyed_core(
    db: &eve_db::WorldRepository,
    sys_id: u32,
    npc_id: i64,
    killer_char_id: Option<i64>,
) {
    db.with_mut_snapshot(|snap| {
        for ((m_char_id, _), m) in snap.active_missions.iter_mut() {
            if (killer_char_id.is_none_or(|cid| *m_char_id == cid) || m.spawned_npc_ball_ids.contains(&npc_id))
                && m.state == "Accepted"
                && m.solar_system_id == sys_id
            {
                if let Some(pos) = m.spawned_npc_ball_ids.iter().position(|&bid| bid == npc_id) {
                    m.spawned_npc_ball_ids.remove(pos);
                    m.npcs_remaining = m.npcs_remaining.saturating_sub(1);
                }
            }
        }
    });
}

