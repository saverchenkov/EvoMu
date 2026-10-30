//! Open `feat:pve.*` implementations for Batch 36:
//! - `feat:pve.research-agent-access-requirements`
//! - `feat:pve.research-agent-rp-formula`
//! - `feat:pve.datacore-purchase`
//! - `feat:pve.storyline-offer-counter`
//! - `feat:pve.ess-grid-warp-mwd-restrictions`
//! - `feat:pve.ess-main-bank-autopayout`
//! - `feat:pve.sleeper-sentry-towers`
//! - `feat:pve.wormhole-site-downtime-despawn`

use std::collections::HashMap;

use eve_db::{IskCents, OwnerNoteRecord, SolarSystemId, WorldStateSnapshot};
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{AgentInfo, SdeRegistry, TypeId};
use glam::DVec3;
use serde::{Deserialize, Serialize};

use crate::router::ServerContext;
use crate::services::agents_missions::agents_missions_helpers::is_storyline_agent_type;
use crate::services::agents_missions::agents_standings_arcs::{
    compute_effective_agent_standing, load_rnd_research_entries,
};

pub type CharacterId = i64;
pub type ItemId = i64;

pub const SKILL_SCIENCE: TypeId = 3402;
pub const SKILL_MECHANICS: TypeId = 3392;
pub const SKILL_POWER_GRID_MANAGEMENT: TypeId = 3413;
pub const SKILL_CPU_MANAGEMENT: TypeId = 3426;
pub const SKILL_RESEARCH_PROJECT_MANAGEMENT: TypeId = 12179;
pub const SKILL_NEGOTIATION: TypeId = 3356;
pub const SKILL_CONNECTIONS: TypeId = 3359;

pub const ESS_INTRUSION_BUBBLE_RADIUS_M: f64 = 75_000.0;
pub const ESS_MAIN_BANK_AUTOPAYOUT_SECS: u64 = 9_900; // 2h45m

pub const SLEEPER_SENTRY_VIGILANT: TypeId = 30460;
pub const SLEEPER_SENTRY_WAKEFUL: TypeId = 30461;
pub const SLEEPER_SENTRY_RESTLESS: TypeId = 30462;

fn skill_level(snap: &WorldStateSnapshot, char_id: CharacterId, type_id: TypeId) -> u8 {
    snap.skills
        .get(&char_id)
        .and_then(|m| m.get(&type_id))
        .map(|s| s.level)
        .unwrap_or(0)
}

fn agent_mission_history_note_id(char_id: CharacterId) -> i64 {
    -(5_300_000_000_000_i64.saturating_add(char_id.abs()))
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
    let nid = agent_mission_history_note_id(char_id);
    snap.owner_notes
        .get(&nid)
        .and_then(|n| serde_json::from_str::<Vec<i64>>(&n.note).ok())
        .is_some_and(|v| v.contains(&agent_id))
}

/// Validates all R&D agent access requirements (`feat:pve.research-agent-access-requirements`):
/// - Docked in the agent's station
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
    req_field_skill: TypeId,
) -> Result<u8, BridgeRpcError> {
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
        1 => -10.0,
        2 => 1.0,
        3 => 3.0,
        4 => 5.0,
        _ => 7.0,
    };
    let eff_any = compute_effective_agent_standing(
        ctx,
        snap,
        char_id,
        agent_info.agent_id,
        agent_info.corporation_id,
        None,
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
            code: "InsufficientCorporationStanding".into(),
            message: format!(
                "R&D agent {} (level {}) requires effective corporation standing >= {min_corp_st:.1} (have {eff_corp:.2})",
                agent_info.agent_id, agent_info.level
            ),
        });
    }

    Ok(field_lvl)
}

/// Computes R&D RP/day (`feat:pve.research-agent-rp-formula`):
/// `RP/day = (1 + (20 + 5*Negotiation + agentStanding)/100) * (fieldSkill + agentLevel)^2`
/// where `agentStanding` is personal standing with the agent, raised by Connections only after
/// at least one mission has been completed for that agent.
pub fn compute_rnd_rp_per_day(
    snap: &WorldStateSnapshot,
    char_id: CharacterId,
    agent_id: i64,
    agent_level: u8,
    field_skill_lvl: u8,
) -> f64 {
    let neg_lvl = skill_level(snap, char_id, SKILL_NEGOTIATION).min(5) as f64;
    let conn_lvl = skill_level(snap, char_id, SKILL_CONNECTIONS).min(5) as f64;
    let raw_agent_st = snap
        .characters
        .get(&char_id)
        .and_then(|c| c.standings.get(&agent_id))
        .copied()
        .unwrap_or(0.0);
    let agent_st = if has_completed_mission_with_agent(snap, char_id, agent_id) && raw_agent_st > 0.0
    {
        (raw_agent_st + (10.0 - raw_agent_st) * 0.04 * conn_lvl).min(10.0)
    } else {
        raw_agent_st
    };
    let mult = 1.0 + (20.0 + 5.0 * neg_lvl + agent_st) / 100.0;
    let base = (field_skill_lvl.min(5) as f64) + (agent_level as f64);
    mult * base.powi(2)
}

/// Resolves the datacore `TypeId` matching an R&D field skill (`feat:pve.datacore-purchase`).
pub fn resolve_datacore_for_research_skill(sde: &SdeRegistry, research_type_id: TypeId) -> TypeId {
    if research_type_id == 11452 {
        return 20172;
    }
    if research_type_id == 11448 {
        return 20424;
    }
    if let Some(skill_type) = sde.types.get(&research_type_id) {
        let target_name = format!("Datacore - {}", skill_type.name);
        if let Some(dc) = sde.types.values().find(|t| t.name == target_name) {
            return dc.type_id;
        }
    }
    20424
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
    -(5_400_000_000_000_i64.saturating_add(char_id.abs()))
}

pub fn is_interbus_or_militia_corp(corp_id: i64) -> bool {
    matches!(
        corp_id,
        1000127 // InterBus
            | 1000179 // 24th Imperial Crusade
            | 1000180 // State Protectorate
            | 1000181 // Federal Defense Union
            | 1000182 // Tribal Liberation Force
            | 1000436 // Malakim Zealots
            | 1000437 // Guristas Commando
    )
}

/// Increments the `(faction_id, level)` mission counter and, on every 16th completion,
/// selects the nearest qualifying Storyline agent (`feat:pve.storyline-offer-counter`).
pub fn record_mission_completion_and_resolve_storyline(
    ctx: &ServerContext,
    snap: &mut WorldStateSnapshot,
    char_id: CharacterId,
    completed_agent: &AgentInfo,
) -> Option<i64> {
    record_agent_mission_completed(snap, char_id, completed_agent.agent_id);
    if is_interbus_or_militia_corp(completed_agent.corporation_id) {
        return None;
    }
    let faction_id = ctx
        .sde
        .npc_corporations
        .get(&completed_agent.corporation_id)
        .and_then(|c| c.faction_id)
        .unwrap_or(500_001);
    let level = completed_agent.level.max(1);
    let nid = storyline_counter_note_id(char_id);
    let mut state: StorylineCounterState = snap
        .owner_notes
        .get(&nid)
        .and_then(|n| serde_json::from_str(&n.note).ok())
        .unwrap_or_default();
    let key = format!("{faction_id}:{level}");
    let entry = state.counts.entry(key).or_insert(0);
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

    let origin_sys_id = completed_agent.solar_system_id.unwrap_or(30_000_142);
    let origin_sys = ctx.sde.solar_systems.get(&origin_sys_id);
    let require_highsec = origin_sys.is_some_and(|s| s.security_status >= 0.45);
    let origin_pos = origin_sys
        .map(|s| DVec3::from_array(s.galactic_pos_ly))
        .unwrap_or(DVec3::ZERO);

    let mut candidates: Vec<(&AgentInfo, f64)> = ctx
        .sde
        .agents
        .values()
        .filter(|a| is_storyline_agent_type(a.agent_type_id))
        .filter(|a| !is_interbus_or_militia_corp(a.corporation_id))
        .filter(|a| {
            let af = ctx
                .sde
                .npc_corporations
                .get(&a.corporation_id)
                .and_then(|c| c.faction_id);
            af == Some(faction_id)
        })
        .filter(|a| {
            if !require_highsec {
                return true;
            }
            a.solar_system_id
                .and_then(|sid| ctx.sde.solar_systems.get(&sid))
                .is_some_and(|s| s.security_status >= 0.45)
        })
        .map(|a| {
            let dist = a
                .solar_system_id
                .and_then(|sid| ctx.sde.solar_systems.get(&sid))
                .map(|s| DVec3::from_array(s.galactic_pos_ly).distance(origin_pos))
                .unwrap_or(f64::MAX);
            (a, dist)
        })
        .collect();

    candidates.sort_by(|a, b| a.1.total_cmp(&b.1).then_with(|| a.0.agent_id.cmp(&b.0.agent_id)));

    let has_pending_offer = |agent_id: i64| -> bool {
        snap.active_missions.values().any(|m| {
            m.character_id == char_id
                && m.agent_id == agent_id
                && matches!(m.state.as_str(), "Offered" | "Accepted")
        })
    };

    candidates
        .iter()
        .find(|(a, _)| !has_pending_offer(a.agent_id))
        .or_else(|| candidates.first())
        .map(|(a, _)| a.agent_id)
}

/// Checks if a ship is inside the 75 km ESS intrusion bubble (`feat:pve.ess-grid-warp-mwd-restrictions`),
/// which blocks warping, Microwarpdrives, Micro Jump Drives, cloaking, and cyno lighting.
pub async fn check_ess_intrusion_bubble_restriction(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    ship_id: ItemId,
    action: &str,
) -> Result<(), BridgeRpcError> {
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let Some((_, ship_ball)) = sys_handle.get_ball_snapshot(ship_id).await.ok().flatten() else {
        return Ok(());
    };
    let balls = sys_handle.list_balls().await.unwrap_or_default();
    let in_bubble = balls.iter().any(|b| {
        (b.type_id == 32882 || b.type_id == 56002 || b.id == 820_000_000 + sys_id as i64)
            && b.position.distance(ship_ball.position) <= ESS_INTRUSION_BUBBLE_RADIUS_M
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
            "Cloaking is disabled inside the 75 km ESS intrusion bubble",
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
) -> Result<(), BridgeRpcError> {
    let action = if mod_type.group_id == 330 {
        Some("cloak")
    } else if mod_type.group_id == 658 || mod_type.name.contains("Cynosural") {
        Some("cyno")
    } else if matches!(mod_type.group_id, 1189 | 1310) || mod_type.name.contains("Micro Jump") {
        Some("mjd")
    } else if mod_type.group_id == 46 && mod_type.name.contains("Microwarpdrive") {
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
    *st.ess_main_bank_contributors.entry(char_id).or_insert(0) += added_cents;
}

/// Distributes accumulated ESS Main Bank ISK every 2h45m (`9_900` s) when not currently being hacked
/// (`feat:pve.ess-main-bank-autopayout`).
pub fn tick_ess_main_bank_autopayout(ctx: &ServerContext, now_unix: u64) -> usize {
    let paid_events: Vec<(SolarSystemId, f64)> = ctx.db.with_mut_snapshot(|snap| {
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
        for sys_id in sys_ids {
            let Some((pool, post_reserve, total_contrib, contribs)) =
                snap.system_states.get_mut(&sys_id).and_then(|st| {
                    if st.ess_linked_character_id.is_some() || st.ess_main_bank.0 <= 0 {
                        return None;
                    }
                    let total_contrib: i64 = st.ess_main_bank_contributors.values().sum();
                    if total_contrib <= 0 {
                        st.ess_cycle_started_unix = now_unix;
                        return None;
                    }
                    let pool = st.ess_main_bank.0;
                    st.ess_main_bank = IskCents(0);
                    let post_reserve = st.ess_reserve_bank.to_wire_f64();
                    let contribs: Vec<(CharacterId, i64)> = st
                        .ess_main_bank_contributors
                        .iter()
                        .map(|(&cid, &c)| (cid, c))
                        .collect();
                    st.ess_main_bank_contributors.clear();
                    st.ess_cycle_started_unix = now_unix;
                    Some((pool, post_reserve, total_contrib, contribs))
                })
            else {
                continue;
            };
            for &(cid, contrib) in &contribs {
                let share = ((pool as i128 * contrib as i128) / total_contrib as i128) as i64;
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
                    }
                }
            }
            events.push((sys_id, post_reserve));
        }
        events
    });
    for &(sys_id, post_reserve) in &paid_events {
        let ess_data = PyRep::keyval([
            ("solarSystemID", PyRep::Integer(sys_id as i64)),
            ("mainBank", PyRep::Real(0.0)),
            ("reserveBank", PyRep::Real(post_reserve)),
        ]);
        super::notify_emitters_batch6::emit_system_ess_data_changed(
            ctx,
            sys_id as i64,
            ess_data,
        );
    }
    paid_events.len()
}

/// Marks a W-space site as activated on first warp-in (`feat:pve.wormhole-site-downtime-despawn`).
pub fn mark_wormhole_site_activated(ctx: &ServerContext, sys_id: SolarSystemId, site_id: &str) {
    crate::services::pve_exploration::with_pve_exploration_reg(ctx, |reg| {
        if let Some(sites) = reg.sites_by_system.get_mut(&sys_id) {
            if let Some(site) = sites.iter_mut().find(|s| s.site_id == site_id && s.wormhole_class.is_some()) {
                site.activated = true;
            }
        }
    });
}

/// Advances downtime counters for activated W-space sites (`feat:pve.wormhole-site-downtime-despawn`):
/// despawns at the 3rd downtime after activation if completed, and at the 4th if left unfinished.
pub fn tick_wormhole_sites_downtime(ctx: &ServerContext) -> usize {
    crate::services::pve_exploration::with_pve_exploration_reg(ctx, |reg| {
        let mut despawned = 0usize;
        for sites in reg.sites_by_system.values_mut() {
            let before = sites.len();
            sites.retain_mut(|site| {
                if site.wormhole_class.is_none() || !site.activated {
                    return true;
                }
                site.downtimes_since_activation += 1;
                let should_despawn = if site.cleared {
                    site.downtimes_since_activation >= 3
                } else {
                    site.downtimes_since_activation >= 4
                };
                !should_despawn
            });
            despawned += before.saturating_sub(sites.len());
        }
        despawned
    })
}
