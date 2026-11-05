//! Open `feat:pve.*` implementations for Batch 36:
//! - `feat:pve.research-agent-access-requirements`
//! - `feat:pve.research-agent-rp-formula`
//! - `feat:pve.datacore-purchase`
//! - `feat:pve.storyline-offer-counter`
//! - `feat:pve.ess-grid-warp-mwd-restrictions`
//! - `feat:pve.ess-main-bank-autopayout`

use std::cmp::Reverse;
use std::collections::{HashMap, VecDeque};

use eve_db::{ActiveMissionRecord, IskCents, OwnerNoteRecord, SolarSystemId, WorldStateSnapshot};
use eve_destiny::{Ball, MovementMode};
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{AgentInfo, MissionTemplate, SdeRegistry, TypeId};
use glam::DVec3;
use serde::{Deserialize, Serialize};

use crate::router::ServerContext;
use crate::services::agents_missions::agents_missions_helpers::{
    is_storyline_agent_type, is_storyline_template,
};
use crate::services::agents_missions::agents_standings_arcs::{
    compute_effective_agent_standing, compute_effective_standing_breakdown,
    load_rnd_research_entries,
};

pub type CharacterId = i64;
pub type ItemId = i64;

pub const SKILL_SCIENCE: TypeId = 3402;
pub const SKILL_MECHANICS: TypeId = 3392;
pub const SKILL_POWER_GRID_MANAGEMENT: TypeId = 3413;
pub const SKILL_CPU_MANAGEMENT: TypeId = 3426;
pub const SKILL_RESEARCH_PROJECT_MANAGEMENT: TypeId = 12179;
pub const SKILL_NEGOTIATION: TypeId = 3356;

pub const ESS_INTRUSION_BUBBLE_RADIUS_M: f64 = 75_000.0;
pub const ESS_NO_CLOAK_RADIUS_M: f64 = 500_000.0;
pub const ESS_MAIN_BANK_AUTOPAYOUT_SECS: u64 = 9_900; // 2h45m
pub const ESS_BEACON_ID_BASE: i64 = 820_000_000;
pub const ESS_GATE_ID_BASE: i64 = 821_000_000;
pub const ESS_BEACON_TYPE_ID: TypeId = 32882;
pub const ESS_GATE_TYPE_ID: TypeId = 17831;

/// SDE combat ship `group_id`s eligible for the null-sec ESS acceleration gate:
/// Tactical Destroyers (`1305`), Cruisers/HACs/Logistics/Recons/HICs/T3Cs/Flag Cruisers
/// (`26, 358, 832, 833, 894, 906, 963, 1972`), Battlecruisers/Command Ships (`419, 540, 1201`),
/// and Battleships (`27`). Excludes Marauders (`900`), Black Ops (`898`), Mining Barges/Exhumers (`463, 543`),
/// Hauler/Transport industrials (`28, 380, 1202`), Corvettes/Frigates/non-T3 Destroyers, and Capitals.
pub const ESS_ALLOWED_SHIP_GROUP_IDS: &[u32] = &[
    26, 27, 358, 419, 540, 832, 833, 894, 906, 963, 1201, 1305, 1972,
];

pub fn is_ess_gate_eligible_hull(sde: &SdeRegistry, ship_type_id: TypeId) -> bool {
    sde.types
        .get(&ship_type_id)
        .is_some_and(|t| ESS_ALLOWED_SHIP_GROUP_IDS.contains(&t.group_id))
}

/// Returns `true` only for regular Security/Distribution/Mining (`2`), R&D (`4`), and Epic Arc (`10`, `11`)
/// agents that increment the 16-mission Storyline counter (`feat:pve.storyline-offer-counter`),
/// excluding Tutorial (`3`), Data Center (`5`), Storyline (`6`, `7`), CONCORD/COSMOS (`8`), FW (`9`), and Career (`12`).
pub fn is_storyline_qualifying_agent_type(agent_type_id: u32) -> bool {
    matches!(agent_type_id, 2 | 4 | 10 | 11)
}

fn skill_level(snap: &WorldStateSnapshot, char_id: CharacterId, type_id: TypeId) -> u8 {
    snap.skills
        .get(&char_id)
        .and_then(|m| m.get(&type_id))
        .map(|s| s.level)
        .unwrap_or(0)
}

fn agent_mission_history_note_id(char_id: CharacterId) -> i64 {
    -(8_300_000_000_000_i64.saturating_add(char_id.abs()))
}

pub fn record_agent_mission_completed(
    snap: &mut WorldStateSnapshot,
    char_id: CharacterId,
    agent_id: i64,
) {
    let nid = agent_mission_history_note_id(char_id);
    let mut completed: Vec<i64> = snap
        .owner_notes
        .get(&nid)
        .and_then(|n| serde_json::from_str(&n.note).ok())
        .unwrap_or_default();
    if !completed.contains(&agent_id) {
        completed.push(agent_id);
        if let Ok(note) = serde_json::to_string(&completed) {
            snap.owner_notes.insert(
                nid,
                OwnerNoteRecord {
                    note_id: nid,
                    character_id: -5,
                    item_id: char_id,
                    label: format!("agent_completed:{char_id}"),
                    note,
                    timestamp_unix: 0,
                },
            );
        }
    }
}

pub fn has_completed_mission_with_agent(
    snap: &WorldStateSnapshot,
    char_id: CharacterId,
    agent_id: i64,
) -> bool {
    if snap
        .active_missions
        .values()
        .any(|m| m.character_id == char_id && m.agent_id == agent_id && m.state == "Completed")
    {
        return true;
    }
    let nid = agent_mission_history_note_id(char_id);
    snap.owner_notes
        .get(&nid)
        .and_then(|n| serde_json::from_str::<Vec<i64>>(&n.note).ok())
        .is_some_and(|v| v.contains(&agent_id))
}

/// Resolves the group-333 Datacore `TypeId` matching a group-270 R&D science field skill via SDE
/// dogma attribute `182` (`requiredSkill1`). Returns `None` for non-research skills and for science
/// skills that have no corresponding Datacore in the V24 SDE (`feat:pve.datacore-purchase`).
pub fn resolve_datacore_for_research_skill(
    sde: &SdeRegistry,
    research_type_id: TypeId,
) -> Option<TypeId> {
    if research_type_id == SKILL_SCIENCE || research_type_id == SKILL_RESEARCH_PROJECT_MANAGEMENT {
        return None;
    }
    if !sde
        .types
        .get(&research_type_id)
        .is_some_and(|t| t.group_id == 270)
    {
        return None;
    }
    sde.types
        .values()
        .filter(|t| {
            t.group_id == 333
                && t.published
                && t.default_attributes
                    .get(&182)
                    .is_some_and(|&req_sk| req_sk as u32 == research_type_id)
        })
        .min_by_key(|t| t.type_id)
        .map(|t| t.type_id)
}

/// Validates all R&D agent access requirements (`feat:pve.research-agent-access-requirements`):
/// - Docked in the agent's station
/// - Requested science field skill is offered by this R&D agent (`agent_info.research_skills`) and maps to a valid SDE Datacore
/// - Science V
/// - Mechanics V, Power Grid Management V, or CPU Management V
/// - Agent's field skill >= 1
/// - Concurrent R&D agents <= 1 + Research Project Management level (max 6)
/// - Normal effective standing for agent level toward agent, corp, or faction (5.0 for level 4)
/// - Effective corporation standing >= (required standing - 2.0) (3.0 for level 4)
pub fn validate_rnd_research_access(
    ctx: &ServerContext,
    station_id_opt: Option<i64>,
    sys_id: SolarSystemId,
    snap: &WorldStateSnapshot,
    char_id: CharacterId,
    agent_info: &AgentInfo,
    requested_field_skill: Option<TypeId>,
) -> Result<(TypeId, u8), BridgeRpcError> {
    if station_id_opt.is_none()
        || agent_info.solar_system_id.is_some_and(|asys| asys != sys_id)
        || agent_info.station_id.is_some_and(|ast| Some(ast) != station_id_opt)
    {
        return Err(BridgeRpcError {
            code: "AgentNotCoLocated".into(),
            message: format!(
                "Must be docked in R&D agent {}'s station to start research",
                agent_info.agent_id
            ),
        });
    }

    let req_field_skill = match requested_field_skill {
        Some(sk) => {
            if resolve_datacore_for_research_skill(&ctx.sde, sk).is_none() {
                return Err(BridgeRpcError {
                    code: "InvalidResearchSkill".into(),
                    message: format!("Type {sk} is not a valid Datacore-producing R&D science field skill"),
                });
            }
            if !agent_info.research_skills.is_empty() && !agent_info.research_skills.contains(&sk) {
                return Err(BridgeRpcError {
                    code: "InvalidResearchSkill".into(),
                    message: format!(
                        "R&D agent {} does not offer research in skill {sk}",
                        agent_info.agent_id
                    ),
                });
            }
            sk
        }
        None => agent_info
            .research_skills
            .iter()
            .copied()
            .find(|&sk| resolve_datacore_for_research_skill(&ctx.sde, sk).is_some())
            .ok_or_else(|| BridgeRpcError {
                code: "InvalidResearchSkill".into(),
                message: format!(
                    "R&D agent {} has no Datacore-producing research field skills",
                    agent_info.agent_id
                ),
            })?,
    };

    if skill_level(snap, char_id, SKILL_SCIENCE) < 5 {
        return Err(BridgeRpcError {
            code: "ErrorNoRelevantResearchSkills".into(),
            message: "Starting R&D research requires Science V".into(),
        });
    }

    let has_eng_5 = skill_level(snap, char_id, SKILL_MECHANICS) >= 5
        || skill_level(snap, char_id, SKILL_POWER_GRID_MANAGEMENT) >= 5
        || skill_level(snap, char_id, SKILL_CPU_MANAGEMENT) >= 5;
    if !has_eng_5 {
        return Err(BridgeRpcError {
            code: "ErrorNoRelevantResearchSkills".into(),
            message: "Starting R&D research requires Mechanics V, Power Grid Management V, or CPU Management V".into(),
        });
    }

    let field_lvl = skill_level(snap, char_id, req_field_skill);
    if field_lvl < 1 {
        return Err(BridgeRpcError {
            code: "ErrorNoRelevantResearchSkills".into(),
            message: format!(
                "Starting R&D research with agent {} requires field skill {req_field_skill} >= 1",
                agent_info.agent_id
            ),
        });
    }

    let entries = load_rnd_research_entries(snap, char_id);
    let already_active = entries.iter().any(|e| e.agent_id == agent_info.agent_id);
    if !already_active {
        let rpm_lvl = skill_level(snap, char_id, SKILL_RESEARCH_PROJECT_MANAGEMENT).min(5);
        let max_agents = (1 + rpm_lvl as usize).min(6);
        if entries.len() >= max_agents {
            return Err(BridgeRpcError {
                code: "MaxResearchAgentsReached".into(),
                message: format!(
                    "Character already has {} active R&D agents (maximum {max_agents} with Research Project Management {rpm_lvl})",
                    entries.len()
                ),
            });
        }
    }

    let req_st = match agent_info.level {
        1 => -1.99,
        2 => 1.0,
        3 => 3.0,
        4 => 5.0,
        _ => 7.0,
    };
    let fac_id = ctx
        .sde
        .npc_corporations
        .get(&agent_info.corporation_id)
        .and_then(|c| c.faction_id)
        .map(i64::from);
    let eff_any = compute_effective_agent_standing(
        ctx,
        snap,
        char_id,
        agent_info.agent_id,
        agent_info.corporation_id,
        fac_id,
    );
    if eff_any < req_st {
        return Err(BridgeRpcError {
            code: "InsufficientAgentStanding".into(),
            message: format!(
                "R&D agent {} (level {}) requires effective standing >= {req_st:.1} (have {eff_any:.2})",
                agent_info.agent_id, agent_info.level
            ),
        });
    }
    if agent_info.level > 1 {
        let eff_corp = compute_effective_agent_standing(
            ctx,
            snap,
            char_id,
            0,
            agent_info.corporation_id,
            None,
        );
        let min_corp_st = req_st - 2.0;
        if eff_corp < min_corp_st {
            return Err(BridgeRpcError {
                code: "InsufficientAgentStanding".into(),
                message: format!(
                    "R&D agent {} (level {}) requires effective corporation standing >= {min_corp_st:.1} (have {eff_corp:.2})",
                    agent_info.agent_id, agent_info.level
                ),
            });
        }
    }

    Ok((req_field_skill, field_lvl))
}

/// Computes R&D RP/day (`feat:pve.research-agent-rp-formula`):
/// `RP/day = (1 + (20 + 5*Negotiation + agentStanding)/100) * (fieldSkill + agentLevel)^2`
/// where `agentStanding` is personal standing with the agent, modified by Connections only after
/// at least one mission has been completed for that agent (and by Diplomacy when negative).
pub fn compute_rnd_rp_per_day(
    ctx: &ServerContext,
    snap: &WorldStateSnapshot,
    char_id: CharacterId,
    agent_id: i64,
    agent_level: u8,
    field_skill_lvl: u8,
) -> f64 {
    let neg_lvl = skill_level(snap, char_id, SKILL_NEGOTIATION).min(5) as f64;
    let raw_agent_st = snap
        .characters
        .get(&char_id)
        .and_then(|c| c.standings.get(&agent_id))
        .copied()
        .unwrap_or(0.0);
    let agent_st = if has_completed_mission_with_agent(snap, char_id, agent_id) || raw_agent_st < 0.0
    {
        compute_effective_standing_breakdown(ctx, snap, char_id, agent_id, raw_agent_st, true).0
    } else {
        raw_agent_st.clamp(-10.0, 10.0)
    };
    let mult = 1.0 + (20.0 + 5.0 * neg_lvl + agent_st) / 100.0;
    let base = (field_skill_lvl.min(5) as f64) + (agent_level as f64);
    mult * base.powi(2)
}

pub fn max_affordable_datacores(total_rp: f64) -> i32 {
    if total_rp <= 0.0 {
        0
    } else {
        (total_rp / 100.0).floor() as i32
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct StorylineCounterState {
    counts: HashMap<String, u32>,
}

fn storyline_counter_note_id(char_id: CharacterId) -> i64 {
    -(8_400_000_000_000_i64.saturating_add(char_id.abs()))
}

/// Identifies InterBus and all Factional Warfare militia corporations purely from SDE faction and
/// NPC corporation records (`feat:pve.storyline-offer-counter`).
pub fn is_interbus_or_militia_corp(sde: &SdeRegistry, corp_id: i64) -> bool {
    if sde
        .factions
        .values()
        .any(|f| f.militia_corporation_id == Some(corp_id))
    {
        return true;
    }
    let Some(corp) = sde.npc_corporations.get(&corp_id) else {
        return false;
    };
    if corp.name.eq_ignore_ascii_case("InterBus") {
        return true;
    }
    corp.faction_id
        .and_then(|fid| sde.factions.get(&fid))
        .is_some_and(|f| f.name.eq_ignore_ascii_case("InterBus"))
}

fn stargate_jump_distances(sde: &SdeRegistry, origin: SolarSystemId) -> HashMap<SolarSystemId, u32> {
    let mut adj: HashMap<SolarSystemId, Vec<SolarSystemId>> = HashMap::new();
    for j in sde.jumps.values() {
        adj.entry(j.from_solar_system_id)
            .or_default()
            .push(j.to_solar_system_id);
    }
    let mut dist = HashMap::new();
    let mut q = VecDeque::new();
    dist.insert(origin, 0);
    q.push_back(origin);
    for _ in 0..sde.solar_systems.len().max(1) {
        let Some(cur) = q.pop_front() else {
            break;
        };
        let next_d = dist[&cur] + 1;
        if let Some(neighbors) = adj.get(&cur) {
            for &nb in neighbors {
                if !dist.contains_key(&nb) {
                    dist.insert(nb, next_d);
                    q.push_back(nb);
                }
            }
        }
    }
    dist
}

/// Increments the `(faction_id, level)` mission counter and, on every 16th completion,
/// selects the nearest qualifying Storyline agent and returns `(storyline_agent_id, level)`
/// (`feat:pve.storyline-offer-counter`).
pub fn record_mission_completion_and_resolve_storyline(
    ctx: &ServerContext,
    snap: &mut WorldStateSnapshot,
    char_id: CharacterId,
    completed_agent: &AgentInfo,
) -> Option<(i64, u8)> {
    record_agent_mission_completed(snap, char_id, completed_agent.agent_id);
    if !is_storyline_qualifying_agent_type(completed_agent.agent_type_id)
        || is_interbus_or_militia_corp(&ctx.sde, completed_agent.corporation_id)
    {
        return None;
    }
    let faction_id = ctx
        .sde
        .npc_corporations
        .get(&completed_agent.corporation_id)
        .and_then(|c| c.faction_id)?;
    let origin_sys_id = completed_agent.solar_system_id?;
    let level = completed_agent.level.clamp(1, 5);

    let nid = storyline_counter_note_id(char_id);
    let legacy_seed = if snap.owner_notes.contains_key(&nid) {
        0
    } else {
        snap.mission_tally.get(&char_id).copied().unwrap_or(0)
    };
    let mut state: StorylineCounterState = snap
        .owner_notes
        .get(&nid)
        .and_then(|n| serde_json::from_str(&n.note).ok())
        .unwrap_or_default();
    let key = format!("{faction_id}:{level}");
    let entry = state.counts.entry(key).or_insert(legacy_seed);
    *entry += 1;
    let count = *entry;
    if let Ok(note) = serde_json::to_string(&state) {
        snap.owner_notes.insert(
            nid,
            OwnerNoteRecord {
                note_id: nid,
                character_id: -5,
                item_id: char_id,
                label: format!("storyline_counter:{char_id}"),
                note,
                timestamp_unix: 0,
            },
        );
    }
    if count % 16 != 0 {
        return None;
    }

    let origin_sys = ctx.sde.solar_systems.get(&origin_sys_id)?;
    let require_highsec = origin_sys.security_status >= 0.45;
    let origin_pos = DVec3::from_array(origin_sys.galactic_pos_ly);
    let jump_dists = stargate_jump_distances(&ctx.sde, origin_sys_id);

    let eligible: Vec<&AgentInfo> = ctx
        .sde
        .agents
        .values()
        .filter(|a| is_storyline_agent_type(a.agent_type_id))
        .filter(|a| !is_interbus_or_militia_corp(&ctx.sde, a.corporation_id))
        .filter(|a| {
            ctx.sde
                .npc_corporations
                .get(&a.corporation_id)
                .and_then(|c| c.faction_id)
                == Some(faction_id)
        })
        .filter(|a| {
            let Some(sid) = a.solar_system_id else {
                return false;
            };
            let Some(sys) = ctx.sde.solar_systems.get(&sid) else {
                return false;
            };
            !require_highsec || sys.security_status >= 0.45
        })
        .collect();

    let has_exact_level = eligible.iter().any(|a| a.level == level);
    let mut candidates: Vec<(&AgentInfo, u32, f64)> = eligible
        .into_iter()
        .filter(|a| {
            if has_exact_level {
                a.level == level
            } else {
                a.agent_type_id == 6 || a.level == level
            }
        })
        .map(|a| {
            let sid = a.solar_system_id.unwrap_or(origin_sys_id);
            let jumps = jump_dists.get(&sid).copied().unwrap_or(u32::MAX);
            let ly = ctx
                .sde
                .solar_systems
                .get(&sid)
                .map(|s| DVec3::from_array(s.galactic_pos_ly).distance(origin_pos))
                .unwrap_or(f64::MAX);
            (a, jumps, ly)
        })
        .collect();

    candidates.sort_by(|a, b| {
        a.1.cmp(&b.1)
            .then_with(|| a.2.total_cmp(&b.2))
            .then_with(|| a.0.agent_id.cmp(&b.0.agent_id))
    });

    let has_pending_offer = |agent_id: i64| -> bool {
        snap.active_missions.values().any(|m| {
            m.character_id == char_id
                && m.agent_id == agent_id
                && matches!(m.state.as_str(), "Offered" | "Accepted")
        })
    };

    candidates
        .iter()
        .find(|(a, _, _)| !has_pending_offer(a.agent_id))
        .or_else(|| candidates.first())
        .map(|(a, _, _)| (a.agent_id, level))
}

/// Selects a Storyline `MissionTemplate` matching `level` (`1..=5`) via SDE dungeon difficulty.
pub fn select_storyline_mission_for_level<'a>(
    sde: &'a SdeRegistry,
    active_missions: &HashMap<(CharacterId, u32), ActiveMissionRecord>,
    char_id: CharacterId,
    level: u8,
) -> Option<&'a MissionTemplate> {
    let target_diff = u32::from(level.clamp(1, 5));
    let matches_level = sde
        .missions
        .values()
        .filter(|sm| {
            is_storyline_template(sm)
                && !active_missions.contains_key(&(char_id, sm.mission_id))
                && sm
                    .dungeon_id
                    .and_then(|did| sde.dungeons.get(&did))
                    .and_then(|d| d.difficulty)
                    == Some(target_diff)
        })
        .min_by_key(|sm| sm.mission_id);
    matches_level.or_else(|| {
        sde.missions
            .values()
            .filter(|sm| {
                is_storyline_template(sm)
                    && !active_missions.contains_key(&(char_id, sm.mission_id))
            })
            .min_by_key(|sm| sm.mission_id)
    })
}

fn is_kspace_nullsec(sde: &SdeRegistry, sys_id: SolarSystemId) -> bool {
    sys_id < 31_000_000
        && sde
            .solar_systems
            .get(&sys_id)
            .is_some_and(|s| s.security_status <= 0.0)
}

fn is_ess_beacon_ball(b: &Ball, sys_id: SolarSystemId) -> bool {
    matches!(b.type_id, 32882 | 55914 | 56002) || b.id == ESS_BEACON_ID_BASE + i64::from(sys_id)
}

/// Seeds the system's ESS beacon (`820_000_000 + sys_id`, if none exists yet) and its exterior
/// Acceleration Gate (`821_000_000 + sys_id`, 200 km outside the 75 km warp disruption bubble)
/// in K-space null-sec systems (`feat:pve.ess-grid-warp-mwd-restrictions`).
pub async fn ensure_ess_intrusion_site_balls(ctx: &ServerContext, sys_id: SolarSystemId) {
    if !is_kspace_nullsec(&ctx.sde, sys_id) {
        return;
    }
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let balls = sys_handle.list_balls().await.unwrap_or_default();
    let ess_pos = if let Some(existing) = balls.iter().find(|b| is_ess_beacon_ball(b, sys_id)) {
        existing.position
    } else {
        let pos = DVec3::new(50_000_000_000.0, 50_000_000_000.0, 0.0);
        let ess_id = ESS_BEACON_ID_BASE + i64::from(sys_id);
        let _ = sys_handle
            .spawn_ball_with_combat(
                Ball {
                    id: ess_id,
                    type_id: ESS_BEACON_TYPE_ID,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 0.0,
                    inertia: 1.0,
                    mass: 50_000_000.0,
                    radius: 500.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Rigid,
                },
                ctx.build_type_combat_state(ESS_BEACON_TYPE_ID),
            )
            .await;
        pos
    };

    let gate_id = ESS_GATE_ID_BASE + i64::from(sys_id);
    if !balls.iter().any(|b| b.id == gate_id) {
        let gate_pos = ess_pos + DVec3::new(200_000.0, 0.0, 0.0);
        let _ = sys_handle
            .spawn_ball_with_combat(
                Ball {
                    id: gate_id,
                    type_id: ESS_GATE_TYPE_ID,
                    position: gate_pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 0.0,
                    inertia: 1.0,
                    mass: 50_000_000.0,
                    radius: 500.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Rigid,
                },
                ctx.build_type_combat_state(ESS_GATE_TYPE_ID),
            )
            .await;
    }
}

/// Handles `ActivateAccelerationGate` for a null-sec ESS intrusion site gate (`821_000_000 + sys_id`),
/// enforcing tackle/immobilization/warp/session-change checks, `<= 2,500 m` gate proximity, and
/// combat hull group eligibility (`ESS_ALLOWED_SHIP_GROUP_IDS`), decloaking the ship and warping it
/// onto the ESS grid (`feat:pve.ess-grid-warp-mwd-restrictions`).
pub async fn try_activate_ess_acceleration_gate(
    ctx: &ServerContext,
    char_id: CharacterId,
    sys_id: SolarSystemId,
    ship_id: ItemId,
    gate_id: ItemId,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if !is_kspace_nullsec(&ctx.sde, sys_id) || gate_id != ESS_GATE_ID_BASE + i64::from(sys_id) {
        return None;
    }
    ensure_ess_intrusion_site_balls(ctx, sys_id).await;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let Some((current_tick, ship_ball)) = sys_handle.get_ball_snapshot(ship_id).await.ok().flatten() else {
        return Some(Err(BridgeRpcError {
            code: "ShipNotInSpace".into(),
            message: "Ship is not in space".into(),
        }));
    };
    if matches!(ship_ball.mode, MovementMode::Warp { .. }) {
        return Some(Err(BridgeRpcError {
            code: "ShipInWarp".into(),
            message: "Cannot activate ESS acceleration gate while ship is in warp".into(),
        }));
    }
    if crate::services::space_destiny::check_session_change_timer_active(
        ctx,
        char_id,
        ctx.now_unix(),
        Some((sys_id, current_tick)),
    )
    .is_some()
    {
        return Some(Err(BridgeRpcError::user_error("SessionChangeInProgress")));
    }
    if let Err(e) = crate::services::tactical_modules::check_ship_navigation_lock(ctx, ship_id, "warp") {
        return Some(Err(e));
    }
    if let Err(e) = crate::services::space_navigation_advanced::check_immobilizing_module_active(ctx, ship_id, "warp") {
        return Some(Err(e));
    }
    if sys_handle.is_ball_scrambled(ship_id).await.unwrap_or(false) {
        return Some(Err(BridgeRpcError {
            code: "WarpScrambled".into(),
            message: "Cannot activate ESS acceleration gate while warp scrambled".into(),
        }));
    }
    let Some((_, gate_ball)) = sys_handle.get_ball_snapshot(gate_id).await.ok().flatten() else {
        return Some(Err(BridgeRpcError {
            code: "GateNotFound".into(),
            message: format!("ESS acceleration gate {gate_id} is not present in system {sys_id}"),
        }));
    };
    let dist = ship_ball.position.distance(gate_ball.position);
    if dist > 2_500.0 {
        return Some(Err(BridgeRpcError {
            code: "OutsideGateActivationRange".into(),
            message: format!("Ship is {dist:.1} m from ESS gate (must be <= 2500 m)"),
        }));
    }
    if !is_ess_gate_eligible_hull(&ctx.sde, ship_ball.type_id) {
        return Some(Err(BridgeRpcError {
            code: "ShipRestrictedByEssGate".into(),
            message: "ESS acceleration gate admits only eligible combat Tactical Destroyer, Cruiser, Battlecruiser, and Battleship hulls".into(),
        }));
    }
    if let Ok(Some(mut cs)) = sys_handle.get_combat_state(ship_id).await {
        if cs.is_cloaked {
            cs.is_cloaked = false;
            cs.is_covert_cloak = false;
            cs.cloak_stabilization_remaining_s = 0.0;
            let _ = sys_handle.set_combat_state(ship_id, cs).await;
            crate::services::notify_emitters_space_dungeons::emit_cloak_module_force_deactivated(
                ctx,
                char_id,
                "DecloakProximityOrDeactivation",
                2000.0,
            );
        }
    }
    let balls = sys_handle.list_balls().await.unwrap_or_default();
    let ess_pos = balls
        .iter()
        .find(|b| is_ess_beacon_ball(b, sys_id))
        .map(|b| b.position)
        .unwrap_or_else(|| gate_ball.position - DVec3::new(200_000.0, 0.0, 0.0));
    let landing_pos = ess_pos + DVec3::new(1_500.0, 0.0, 0.0);
    let mut warp_mode = MovementMode::new_warp(ship_ball.position, landing_pos, 3.0);
    if let MovementMode::Warp { ref mut phase, .. } = warp_mode {
        *phase = eve_destiny::WarpPhase::Accelerating;
    }
    let _ = sys_handle.set_movement_mode(ship_id, warp_mode).await;
    Some(Ok(PyRep::keyval([
        ("success", PyRep::Bool(true)),
        ("gateID", PyRep::Integer(gate_id)),
        ("destinationX", PyRep::Real(landing_pos.x)),
        ("destinationY", PyRep::Real(landing_pos.y)),
        ("destinationZ", PyRep::Real(landing_pos.z)),
    ])))
}

/// Blocks direct warps from outside onto the 75 km ESS intrusion grid without using the ESS
/// acceleration gate (`feat:pve.ess-grid-warp-mwd-restrictions`).
pub async fn check_ess_warp_destination_allowed(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    anchor_pos: DVec3,
    landing_pos: DVec3,
) -> Result<(), BridgeRpcError> {
    if !is_kspace_nullsec(&ctx.sde, sys_id) {
        return Ok(());
    }
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let balls = sys_handle.list_balls().await.unwrap_or_default();
    let hits_bubble = balls.iter().any(|b| {
        is_ess_beacon_ball(b, sys_id)
            && (b.position.distance(anchor_pos) <= ESS_INTRUSION_BUBBLE_RADIUS_M
                || b.position.distance(landing_pos) <= ESS_INTRUSION_BUBBLE_RADIUS_M)
    });
    if hits_bubble {
        return Err(BridgeRpcError {
            code: "EssIntrusionBubbleWarpBlocked".into(),
            message: "Cannot warp directly onto the 75 km ESS intrusion grid; use the ESS acceleration gate".into(),
        });
    }
    Ok(())
}

/// Checks if a ship is inside the ESS intrusion bubble (`75 km` for warp/MWD/MJD/cyno/filament,
/// `500 km` for cloaking; `feat:pve.ess-grid-warp-mwd-restrictions`).
pub async fn check_ess_intrusion_bubble_restriction(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    ship_id: ItemId,
    action: &str,
) -> Result<(), BridgeRpcError> {
    if !is_kspace_nullsec(&ctx.sde, sys_id) {
        return Ok(());
    }
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let Some((_, ship_ball)) = sys_handle.get_ball_snapshot(ship_id).await.ok().flatten() else {
        return Ok(());
    };
    let radius = if action == "cloak" {
        ESS_NO_CLOAK_RADIUS_M
    } else {
        ESS_INTRUSION_BUBBLE_RADIUS_M
    };
    let balls = sys_handle.list_balls().await.unwrap_or_default();
    let in_bubble = balls.iter().any(|b| {
        is_ess_beacon_ball(b, sys_id) && b.position.distance(ship_ball.position) <= radius
    });
    if !in_bubble {
        return Ok(());
    }
    let (code, msg) = match action {
        "warp" => (
            "EssIntrusionBubbleWarpBlocked",
            "Warping is disabled inside the 75 km ESS intrusion bubble",
        ),
        "mwd" => (
            "EssIntrusionBubbleMwdBlocked",
            "Microwarpdrives are disabled inside the 75 km ESS intrusion bubble",
        ),
        "mjd" => (
            "EssIntrusionBubbleMjdBlocked",
            "Micro Jump Drives are disabled inside the 75 km ESS intrusion bubble",
        ),
        "cloak" => (
            "EssIntrusionBubbleCloakBlocked",
            "Cloaking is disabled within 500 km of the ESS beacon",
        ),
        "cyno" => (
            "EssIntrusionBubbleCynoBlocked",
            "Cynosural fields cannot be lit inside the 75 km ESS intrusion bubble",
        ),
        _ => (
            "EssIntrusionBubbleRestricted",
            "Action is disabled inside the 75 km ESS intrusion bubble",
        ),
    };
    Err(BridgeRpcError {
        code: code.into(),
        message: msg.into(),
    })
}

pub async fn check_ess_module_activation_allowed(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    ship_id: ItemId,
    mod_type: &eve_sde::InvType,
    effect_name: &str,
) -> Result<(), BridgeRpcError> {
    let is_mwd = effect_name == "moduleBonusMicrowarpdrive"
        || (mod_type.group_id == 46
            && (mod_type.effect_ids.contains(&6730)
                || mod_type.default_attributes.get(&554).copied().unwrap_or(0.0) > 0.0
                || mod_type.default_attributes.get(&986).copied().unwrap_or(0.0) > 0.0
                || mod_type.name.contains("Microwarpdrive")));
    let action = if mod_type.group_id == 330 {
        Some("cloak")
    } else if mod_type.group_id == 658 || mod_type.name.contains("Cynosural") {
        Some("cyno")
    } else if matches!(mod_type.group_id, 1189 | 1310) || mod_type.name.contains("Micro Jump") {
        Some("mjd")
    } else if is_mwd {
        Some("mwd")
    } else {
        None
    };
    if let Some(act) = action {
        check_ess_intrusion_bubble_restriction(ctx, sys_id, ship_id, act).await?;
    }
    Ok(())
}

pub fn record_ess_main_bank_contribution(
    st: &mut eve_db::SystemDynamicState,
    char_id: CharacterId,
    added_cents: i64,
    now_unix: u64,
) {
    if added_cents <= 0 {
        return;
    }
    if st.ess_cycle_started_unix == 0 {
        st.ess_cycle_started_unix = now_unix;
    }
    let entry = st.ess_main_bank_contributors.entry(char_id).or_insert(0);
    *entry = entry.saturating_add(added_cents);
}

/// Distributes accumulated ESS Main Bank ISK every 2h45m (`9_900` s) when not currently being hacked
/// (`feat:pve.ess-main-bank-autopayout`), conserving 100% of ISK cents across valid contributors.
pub fn tick_ess_main_bank_autopayout(ctx: &ServerContext, now_unix: u64) -> usize {
    let (paid_events, acct_changes): (Vec<(SolarSystemId, f64, f64)>, Vec<(CharacterId, f64)>) =
        ctx.db.with_mut_snapshot(|snap| {
            let sys_ids: Vec<SolarSystemId> = snap
                .system_states
                .iter()
                .filter(|(_, st)| {
                    !st.ess_main_bank_contributors.is_empty()
                        && st.ess_cycle_started_unix > 0
                        && now_unix.saturating_sub(st.ess_cycle_started_unix) >= ESS_MAIN_BANK_AUTOPAYOUT_SECS
                })
                .map(|(&sid, _)| sid)
                .collect();
            let mut events = Vec::new();
            let mut changes = Vec::new();
            for sys_id in sys_ids {
                let valid_chars = |cid: CharacterId| snap.characters.contains_key(&cid);
                let existing_contribs: Vec<(CharacterId, i64)> = snap
                    .system_states
                    .get(&sys_id)
                    .map(|st| {
                        st.ess_main_bank_contributors
                            .iter()
                            .filter(|(&cid, &c)| c > 0 && valid_chars(cid))
                            .map(|(&cid, &c)| (cid, c))
                            .collect()
                    })
                    .unwrap_or_default();
                let total_contrib: i128 = existing_contribs.iter().map(|&(_, c)| i128::from(c)).sum();
                let Some((pool, post_reserve)) = snap.system_states.get_mut(&sys_id).and_then(|st| {
                    if st.ess_linked_character_id.is_some() || st.ess_main_bank.0 <= 0 {
                        return None;
                    }
                    if total_contrib <= 0 {
                        st.ess_main_bank_contributors.clear();
                        st.ess_cycle_started_unix = 0;
                        return None;
                    }
                    let pool = st.ess_main_bank.0;
                    st.ess_main_bank = IskCents(0);
                    let post_reserve = st.ess_reserve_bank.to_wire_f64();
                    st.ess_main_bank_contributors.clear();
                    st.ess_cycle_started_unix = 0;
                    Some((pool, post_reserve))
                }) else {
                    continue;
                };

                let mut payouts: Vec<(CharacterId, i64)> = existing_contribs
                    .iter()
                    .map(|&(cid, contrib)| {
                        let share = ((i128::from(pool) * i128::from(contrib)) / total_contrib) as i64;
                        (cid, share)
                    })
                    .collect();
                let distributed: i64 = payouts.iter().map(|&(_, s)| s).sum();
                let rem = pool.saturating_sub(distributed);
                if rem > 0 {
                    if let Some((top_idx, _)) = existing_contribs
                        .iter()
                        .enumerate()
                        .max_by_key(|&(_, &(cid, c))| (c, Reverse(cid)))
                    {
                        payouts[top_idx].1 = payouts[top_idx].1.saturating_add(rem);
                    }
                }

                for (cid, share) in payouts {
                    if share > 0 {
                        if let Some(ch) = snap.characters.get_mut(&cid) {
                            ch.balance = ch.balance.checked_add(IskCents(share)).unwrap_or(ch.balance);
                            let bal_after = ch.balance;
                            snap.append_journal(
                                cid,
                                "ESSMainBankPayout",
                                IskCents(share),
                                bal_after,
                                format!("ESS Main Bank 2h45m autopayout in system {sys_id}"),
                            );
                            changes.push((cid, bal_after.to_wire_f64()));
                        }
                    }
                }
                events.push((sys_id, 0.0, post_reserve));
            }
            (events, changes)
        });
    for &(cid, bal_f64) in &acct_changes {
        ctx.notify_account_change(cid, "cash", cid, bal_f64);
    }
    for &(sys_id, post_main, post_reserve) in &paid_events {
        let ess_data = PyRep::keyval([
            ("solarSystemID", PyRep::Integer(sys_id as i64)),
            ("mainBank", PyRep::Real(post_main)),
            ("reserveBank", PyRep::Real(post_reserve)),
        ]);
        super::notify_emitters_combat_ess_progression::emit_system_ess_data_changed(
            ctx,
            sys_id as i64,
            ess_data,
        );
    }
    paid_events.len()
}

/// Rejects `essMgr.RequestUnlockReserveBank` while an unlock window or active Reserve Bank link is open.
pub fn ensure_ess_reserve_bank_not_already_unlocked(
    snap: &WorldStateSnapshot,
    sys_id: SolarSystemId,
    now_unix: u64,
    rt_until: u64,
) -> Result<(), BridgeRpcError> {
    let pulse_st = super::space_destiny::get_ess_reserve_pulses(snap, sys_id);
    let eff_until = pulse_st.unlocked_until_unix.max(rt_until);
    if pulse_st.reserve_linked_character_id.is_some()
        || (eff_until > now_unix && pulse_st.pulses_remaining > 0)
        || (rt_until > now_unix && pulse_st.unlocked_until_unix == 0)
    {
        return Err(BridgeRpcError {
            code: "EssReserveBankAlreadyUnlocked".into(),
            message: "ESS Reserve Bank is already unlocked or has an active link".into(),
        });
    }
    Ok(())
}

// DERIVED: EVE Uni Wiki (`Encounter_Surveillance_System.wiki`) states Reserve Bank ISK is
// "paid out over time at a rate that spools up and then drops down" without publishing exact
// per-minute curve weights. Weight `w(i, N)` ramps linearly from `100` at pulse `1` up to `300`
// at `peak = (4 * N) / 5` (80% through the window) and tapers down to `150` at pulse `N`.
// Telescoping ratio `available * w(k, N) / sum_{j=k..=N} w(j, N)` spools up (`pulse_2 > pulse_1`),
// drops down after peak, and conserves 100% of remaining integer `IskCents` on pulse `N`.
pub fn ess_reserve_pulse_weight(pulse_idx_1based: u32, pulses_total: u32) -> u64 {
    let n = pulses_total.max(1) as u64;
    let i = pulse_idx_1based.clamp(1, pulses_total.max(1)) as u64;
    let peak = ((4 * n) / 5).max(1);
    if i <= peak {
        if peak <= 1 {
            300
        } else {
            100 + (200 * (i - 1)) / (peak - 1)
        }
    } else {
        let tail_len = (n - peak).max(1);
        300 - (150 * (i - peak)) / tail_len
    }
}

pub fn compute_ess_reserve_pulse_payout_cents(
    available_cents: i64,
    pulses_total: u32,
    pulses_remaining: u32,
) -> i64 {
    if available_cents <= 0 || pulses_remaining == 0 {
        return 0;
    }
    let total = pulses_total.max(pulses_remaining).max(1);
    let current_idx = total.saturating_sub(pulses_remaining).saturating_add(1);
    let cur_w = ess_reserve_pulse_weight(current_idx, total) as i128;
    let rem_sum: i128 = (current_idx..=total)
        .map(|idx| ess_reserve_pulse_weight(idx, total) as i128)
        .sum();
    if rem_sum <= 0 {
        return available_cents;
    }
    let raw = ((available_cents as i128) * cur_w) / rem_sum;
    (raw as i64).clamp(1, available_cents)
}

/// Streams 60-second ESS Reserve Bank pulses from `tick_background_cluster` while verifying
/// active `<= 2,500 m` ship proximity to the ESS beacon and emitting `OnReserveBankPlayerLinkDisconnected`
/// strictly upon link termination (`feat:pve.ess-reserve-bank-key-tiers`).
pub async fn tick_ess_reserve_bank_links(ctx: &ServerContext, now_unix: u64) -> usize {
    let active_systems: Vec<(SolarSystemId, CharacterId)> = ctx.db.with_snapshot(|snap| {
        snap.service_state_blobs
            .iter()
            .filter_map(|(k, v)| {
                let sid_str = k.strip_prefix("ess_reserve_pulses:")?;
                let sid: SolarSystemId = sid_str.parse().ok()?;
                let st: super::space_destiny::EssReservePulseState = serde_json::from_str(v).ok()?;
                st.reserve_linked_character_id.map(|cid| (sid, cid))
            })
            .collect()
    });
    let mut processed = 0usize;
    for (sys_id, char_id) in active_systems {
        let ship_id_opt = ctx.db.with_snapshot(|snap| {
            let ch = snap.characters.get(&char_id)?;
            if ch.solar_system_id != sys_id || ch.station_id.is_some() || ch.structure_id.is_some() || ch.active_ship_id <= 0 {
                return None;
            }
            Some(ch.active_ship_id)
        });
        let mut range_ok = false;
        if let Some(ship_id) = ship_id_opt {
            let sys_h = ctx.systems.get_or_spawn(sys_id);
            if let Some((_, ship_ball)) = sys_h.get_ball_snapshot(ship_id).await.ok().flatten() {
                if !matches!(ship_ball.mode, MovementMode::Warp { .. }) {
                    let balls = sys_h.list_balls().await.unwrap_or_default();
                    if let Some(ess) = balls.into_iter().find(|b| {
                        b.type_id == ESS_BEACON_TYPE_ID || b.type_id == 56002 || b.id == ESS_BEACON_ID_BASE + sys_id as i64
                    }) {
                        range_ok = ship_ball.position.distance(ess.position) <= 2_500.0;
                    }
                }
            }
        }
        if !range_ok {
            let severed = ctx.db.with_mut_snapshot(|snap| {
                let mut pulse_st = super::space_destiny::get_ess_reserve_pulses(snap, sys_id);
                if pulse_st.reserve_linked_character_id != Some(char_id) {
                    return false;
                }
                pulse_st.reserve_linked_character_id = None;
                pulse_st.reserve_link_started_tick = None;
                pulse_st.link_started_unix = None;
                super::space_destiny::set_ess_reserve_pulses(snap, sys_id, &pulse_st);
                if let Some(st) = snap.system_states.get_mut(&sys_id) {
                    if st.ess_linked_character_id == Some(char_id) {
                        st.ess_linked_character_id = None;
                        st.ess_link_started_tick = None;
                    }
                }
                true
            });
            if severed {
                super::notify_emitters_combat_ess_progression::emit_reserve_bank_player_link_disconnected(ctx, char_id, sys_id, 1);
            }
            continue;
        }

        let rt_until = super::combat_crimewatch::with_combat_runtime(ctx, |rt| {
            rt.ess_reserve_unlocked_until.get(&sys_id).copied().unwrap_or(0)
        });
        let (pulse_payouts, bal_wire, post_banks, terminated, clear_rt_unlock) =
            ctx.db.with_mut_snapshot(|snap| {
                let mut pulse_st = super::space_destiny::get_ess_reserve_pulses(snap, sys_id);
                if pulse_st.reserve_linked_character_id != Some(char_id) {
                    return (Vec::new(), None, None, false, false);
                }
                let eff_until = pulse_st.unlocked_until_unix.max(rt_until);
                let link_start = pulse_st.link_started_unix.unwrap_or(now_unix);
                let mut anchor = pulse_st
                    .last_pulse_sec
                    .map(|l| l.max(link_start))
                    .unwrap_or(link_start);
                let cap_now = if eff_until > 0 { now_unix.min(eff_until) } else { now_unix };
                let due_count = ((cap_now.saturating_sub(anchor)) / 60)
                    .min(u64::from(pulse_st.pulses_remaining))
                    .min(45) as u32;
                let mut payouts = Vec::new();
                let mut last_bal_wire = None;
                for _ in 0..due_count {
                    let avail = snap
                        .system_states
                        .get(&sys_id)
                        .map(|s| s.ess_reserve_bank.0)
                        .unwrap_or(0);
                    if avail <= 0 || pulse_st.pulses_remaining == 0 {
                        break;
                    }
                    let p = compute_ess_reserve_pulse_payout_cents(
                        avail,
                        pulse_st.pulses_total,
                        pulse_st.pulses_remaining,
                    );
                    if let Some(st) = snap.system_states.get_mut(&sys_id) {
                        st.ess_reserve_bank = IskCents(st.ess_reserve_bank.0.saturating_sub(p));
                    }
                    pulse_st.pulses_remaining = pulse_st.pulses_remaining.saturating_sub(1);
                    anchor = anchor.saturating_add(60);
                    pulse_st.last_pulse_sec = Some(anchor);
                    if let Some(ch) = snap.characters.get_mut(&char_id) {
                        ch.balance = ch.balance.checked_add(IskCents(p)).unwrap_or(ch.balance);
                        let bal_after = ch.balance;
                        last_bal_wire = Some(bal_after.to_wire_f64());
                        snap.append_journal(
                            char_id,
                            "ESSReserveBankPayout",
                            IskCents(p),
                            bal_after,
                            format!("ESS Reserve Bank pulse payout in system {sys_id}"),
                        );
                    }
                    payouts.push((IskCents(p).to_wire_f64(), anchor));
                }
                let rem_bank = snap
                    .system_states
                    .get(&sys_id)
                    .map(|s| s.ess_reserve_bank.0)
                    .unwrap_or(0);
                let window_expired = eff_until > 0 && now_unix >= eff_until;
                let term = pulse_st.pulses_remaining == 0 || rem_bank <= 0 || window_expired;
                let mut clear_rt = false;
                if term {
                    pulse_st.reserve_linked_character_id = None;
                    pulse_st.reserve_link_started_tick = None;
                    pulse_st.link_started_unix = None;
                    if let Some(st) = snap.system_states.get_mut(&sys_id) {
                        if st.ess_linked_character_id == Some(char_id) {
                            st.ess_linked_character_id = None;
                            st.ess_link_started_tick = None;
                        }
                    }
                    if pulse_st.pulses_remaining == 0 || window_expired {
                        pulse_st.unlocked_until_unix = 0;
                        clear_rt = true;
                    }
                }
                super::space_destiny::set_ess_reserve_pulses(snap, sys_id, &pulse_st);
                let banks = snap
                    .system_states
                    .get(&sys_id)
                    .map(|s| (s.ess_main_bank.to_wire_f64(), s.ess_reserve_bank.to_wire_f64()));
                (payouts, last_bal_wire, banks, term, clear_rt)
            });

        if clear_rt_unlock {
            super::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
                rt.ess_reserve_unlocked_until.remove(&sys_id);
            });
        }
        if !pulse_payouts.is_empty() {
            processed += pulse_payouts.len();
            let suspect_until = now_unix.saturating_add(900);
            let weapon_until = now_unix.saturating_add(60);
            {
                let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
                let st = cw.get_or_create(char_id);
                st.suspect_flag_until_unix = st.suspect_flag_until_unix.max(suspect_until);
                st.pvp_timer_until_unix = st.pvp_timer_until_unix.max(suspect_until);
                st.weapon_timer_until_unix = st.weapon_timer_until_unix.max(weapon_until);
            }
            super::combat_crimewatch::emit_crimewatch_timer_notifications(ctx, char_id, now_unix);
            if let Some(bal_f64) = bal_wire {
                ctx.notify_account_change(char_id, "cash", char_id, bal_f64);
            }
            super::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
                let list = rt.ess_reserve_bank_thefts.entry(sys_id).or_default();
                for &(amt, ts) in &pulse_payouts {
                    list.push((char_id, amt, ts));
                }
            });
            if let Some((post_main, post_reserve)) = post_banks {
                let ess_data = PyRep::keyval([
                    ("solarSystemID", PyRep::Integer(sys_id as i64)),
                    ("mainBank", PyRep::Real(post_main)),
                    ("reserveBank", PyRep::Real(post_reserve)),
                ]);
                super::notify_emitters_combat_ess_progression::emit_system_ess_data_changed(
                    ctx,
                    sys_id as i64,
                    ess_data,
                );
            }
        }
        if terminated {
            super::notify_emitters_combat_ess_progression::emit_reserve_bank_player_link_disconnected(ctx, char_id, sys_id, 0);
        }
    }
    processed
}

/// Applies the canonical K-space null-sec DBS & ESS Main/Reserve Bank bounty split for 1 Hz
/// `NpcSpawnManager` ballpark NPC kills while preserving direct full-bounty payout in non-nullsec space.
pub fn credit_ballpark_npc_bounty_with_ess_split(
    sde: &SdeRegistry,
    snap: &mut WorldStateSnapshot,
    sys_id: SolarSystemId,
    killer_char_id: CharacterId,
    raw_bounty_isk: f64,
    base_entity_bounty_isk: f64,
    supp_mult: f64,
    npc_type_id: TypeId,
    now_unix: u64,
) {
    let sys_sec = sde
        .solar_systems
        .get(&sys_id)
        .map(|s| s.security_status)
        .unwrap_or(0.5);
    let apply_sec_gain = |snap: &mut WorldStateSnapshot| {
        let Some(old_sec) = snap.characters.get(&killer_char_id).map(|c| c.security_status) else {
            return;
        };
        if old_sec < 5.0 {
            let gain = crate::combat::compute_sec_status_gain(old_sec, base_entity_bounty_isk) * supp_mult;
            if gain > 0.0 {
                let new_sec = (old_sec + gain).min(5.0).clamp(-10.0, 10.0);
                if let Some(ch) = snap.characters.get_mut(&killer_char_id) {
                    ch.security_status = new_sec;
                }
                snap.combat_crimewatch_state.sec_status_transactions.entry(killer_char_id).or_default().push(
                    eve_db::PersistedSecStatusTransaction {
                        timestamp_unix: now_unix, old_sec, new_sec, delta: new_sec - old_sec,
                        reason: "NPC Pirate Bounty".into(), solar_system_id: sys_id,
                        event_type_id: 378, other_owner_id: None, other_type_id: Some(npc_type_id),
                    },
                );
            }
        }
    };
    let is_kspace_nullsec = sys_sec <= 0.0 && sys_id < 31_000_000;
    if !is_kspace_nullsec {
        let bounty_cents = IskCents::from_f64_isk(raw_bounty_isk);
        if let Some(ch) = snap.characters.get_mut(&killer_char_id) {
            ch.balance = ch.balance.checked_add(bounty_cents).unwrap_or(ch.balance);
        }
        apply_sec_gain(snap);
        let bal_after = snap
            .characters
            .get(&killer_char_id)
            .map(|c| c.balance)
            .unwrap_or(bounty_cents);
        snap.append_journal(
            killer_char_id,
            "bounty_payout",
            bounty_cents,
            bal_after,
            format!("NPC bounty award for destruction of {npc_type_id}"),
        );
        return;
    }

    let dbs_mult = snap
        .system_states
        .get(&sys_id)
        .map(|st| st.dbs_multiplier)
        .unwrap_or(1.35);
    let char_corp_id = snap
        .characters
        .get(&killer_char_id)
        .map(|c| c.corporation_id)
        .unwrap_or(0);
    let (is_player_corp, resolved_tax_rate) = match snap.corporations.get(&char_corp_id) {
        Some(c) => (true, c.tax_rate.clamp(0.0, 1.0)),
        None => (false, 0.11),
    };
    let modified_gross = raw_bounty_isk * dbs_mult;
    let effective_tax_rate = if !is_player_corp && modified_gross < 100_000.0 {
        0.0
    } else {
        resolved_tax_rate
    };
    let (pilot_isk, tax_isk, main_ess, reserve_ess) =
        crate::formulas::compute_dbs_and_ess_bounty_split(raw_bounty_isk, dbs_mult, effective_tax_rate);
    let pilot_cents = IskCents::from_f64_isk(pilot_isk);
    let tax_cents = IskCents::from_f64_isk(tax_isk);
    let main_cents = IskCents::from_f64_isk(main_ess);
    let reserve_cents = IskCents::from_f64_isk(reserve_ess);

    if let Some(ch) = snap.characters.get_mut(&killer_char_id) {
        ch.balance = ch.balance.checked_add(pilot_cents).unwrap_or(ch.balance);
    }
    apply_sec_gain(snap);
    if is_player_corp && tax_cents.raw_cents() > 0 {
        if let Some(corp) = snap.corporations.get_mut(&char_corp_id) {
            corp.ensure_wallet_divisions();
            corp.master_wallet = corp.master_wallet.checked_add(tax_cents).unwrap_or(corp.master_wallet);
            if let Some(d0) = corp.wallet_divisions.first_mut() {
                d0.balance = d0.balance.checked_add(tax_cents).unwrap_or(d0.balance);
            }
        }
    }
    let st = snap.system_states.entry(sys_id).or_default();
    st.solar_system_id = sys_id;
    st.ess_main_bank = st.ess_main_bank.checked_add(main_cents).unwrap_or(st.ess_main_bank);
    record_ess_main_bank_contribution(st, killer_char_id, main_cents.0, now_unix);
    st.ess_reserve_bank = st.ess_reserve_bank.checked_add(reserve_cents).unwrap_or(st.ess_reserve_bank);

    let bal_after = snap
        .characters
        .get(&killer_char_id)
        .map(|c| c.balance)
        .unwrap_or(pilot_cents);
    snap.append_journal(
        killer_char_id,
        "bounty_prizes",
        pilot_cents,
        bal_after,
        format!("NPC bounty award for destruction of {npc_type_id} in system {sys_id} (DBS {dbs_mult:.2}x)"),
    );
    if tax_cents.raw_cents() > 0 {
        snap.append_journal(
            killer_char_id,
            "bounty_prize_corporation_tax",
            IskCents::from_raw_cents(-tax_cents.raw_cents()),
            bal_after,
            format!("Corporation tax ({:.0}%) on NPC bounty in {sys_id}", effective_tax_rate * 100.0),
        );
    }
}
