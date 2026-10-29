//! `agents_standings_arcs` — Submodule of `agents_missions` (`E-5`).
//!
//! Implements:
//! - Effective standing calculation (`Connections`, `Diplomacy`, `Criminal Connections`, `Social`)
//! - Derived faction standings, opposing faction losses, and bipolar Triglavian/EDENCOM standings
//! - `OnStandingSet` and `OnStandingsModified` client notifications
//! - `standingMgr` (`GetCorpStandings`, `GetNPCNPCStandings`, `GetStandingCompositions`)
//! - `missionTrackerMgr` (`GetAllMissionObjectives`, `GetMissionInfoItems`, `IsInActiveDungeonID`, `UpdateAllMissions`)
//! - `epicArcStatus` (`AgentHasEpicMissionsForCharacter`) and `agentMgr` Epic Arc / Career / Plausible / Disabled mission RPCs
//! - R&D agent research (`DoAction` 12/13/14) and Locator agent character location (`DoAction` 15/16/17)
//! - Epic Arc / COSMOS / Data Center choice branching (`DoAction` 18/19)
//! - `keeper.ActivateAccelerationGate` (including Anomic Burner ship restrictions) and AIR Career / NPE handlers


#![forbid(unsafe_code)]

use crate::formulas::apply_standing_increase;
use crate::router::{NotificationTarget, ServerContext};
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, StandingTransactionRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::TypeId;
use std::collections::HashMap;

const SKILL_SOCIAL: TypeId = 3355;
const SKILL_DIPLOMACY: TypeId = 3357;
const SKILL_CONNECTIONS: TypeId = 3359;
const SKILL_CRIMINAL_CONNECTIONS: TypeId = 3361;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct MissionTimingAndCargoMeta {
    pub mission_id: u32,
    pub agent_id: i64,
    pub offered_at_unix: u64,
    pub offer_expiry_unix: u64,
    pub accepted_at_unix: Option<u64>,
    pub accept_expiry_unix: Option<u64>,
    pub bonus_expiry_unix: Option<u64>,
    pub completed_at_unix: Option<u64>,
    pub required_delivery_type_id: Option<TypeId>,
    pub required_delivery_qty: i32,
    pub granted_courier_item_id: Option<i64>,
    #[serde(default)]
    pub granted_courier_qty: i32,
    pub is_anomic_burner: bool,
    pub chosen_branch: Option<String>,
    #[serde(default)]
    pub destination_station_id: Option<i64>,
    #[serde(default)]
    pub origin_station_id: Option<i64>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RndResearchEntry {
    pub agent_id: i64,
    pub research_type_id: TypeId,
    pub points_per_day: f64,
    pub initial_points: f64,
    pub started_unix: u64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EpicArcStateEntry {
    pub epic_arc_id: u32,
    pub agent_id: i64,
    pub faction_id: i64,
    pub chapter: u32,
    pub completed_missions: Vec<u32>,
    pub chosen_branch: Option<String>,
    pub completed_at_unix: Option<u64>,
    pub cooldown_until_unix: Option<u64>,
}

fn mission_meta_note_id(char_id: i64, mission_id: u32) -> i64 {
    let mixed = ((char_id.unsigned_abs() & 0xFFFF_FFFF) << 20) ^ (u64::from(mission_id) & 0xFFFFF);
    -((mixed as i64).max(1) + 5_000_000_000_000)
}

pub fn load_mission_meta(
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
    mission_id: u32,
) -> Option<MissionTimingAndCargoMeta> {
    snap.owner_notes
        .get(&mission_meta_note_id(char_id, mission_id))
        .and_then(|n| serde_json::from_str(&n.note).ok())
}

pub fn save_mission_meta(
    snap: &mut eve_db::WorldStateSnapshot,
    char_id: i64,
    meta: &MissionTimingAndCargoMeta,
) {
    let nid = mission_meta_note_id(char_id, meta.mission_id);
    if let Ok(note) = serde_json::to_string(meta) {
        snap.owner_notes.insert(
            nid,
            eve_db::OwnerNoteRecord {
                note_id: nid,
                character_id: -5,
                item_id: meta.mission_id as i64,
                label: format!("mission_meta:{char_id}:{}", meta.mission_id),
                note,
                timestamp_unix: meta.offered_at_unix,
            },
        );
    }
}

fn rnd_research_note_id(char_id: i64) -> i64 {
    -(5_100_000_000_000_i64.saturating_add(char_id.abs()))
}

pub fn load_rnd_research_entries(
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
) -> Vec<RndResearchEntry> {
    snap.owner_notes
        .get(&rnd_research_note_id(char_id))
        .and_then(|n| serde_json::from_str(&n.note).ok())
        .unwrap_or_default()
}

pub fn save_rnd_research_entries(
    snap: &mut eve_db::WorldStateSnapshot,
    char_id: i64,
    entries: &[RndResearchEntry],
) {
    let nid = rnd_research_note_id(char_id);
    if let Ok(note) = serde_json::to_string(entries) {
        snap.owner_notes.insert(
            nid,
            eve_db::OwnerNoteRecord {
                note_id: nid,
                character_id: -5,
                item_id: char_id,
                label: format!("rnd_research:{char_id}"),
                note,
                timestamp_unix: 0,
            },
        );
    }
    snap.char_research_agents.insert(
        char_id,
        entries
            .iter()
            .map(|e| eve_db::CharacterResearchAgentRecord {
                agent_id: e.agent_id,
                skill_type_id: e.research_type_id as u32,
                started_at_unix: e.started_unix,
                points_per_day: e.points_per_day,
                remainder_points: e.initial_points,
            })
            .collect(),
    );
}

fn epic_arc_note_id(char_id: i64) -> i64 {
    -(5_200_000_000_000_i64.saturating_add(char_id.abs()))
}

pub fn load_epic_arc_states(
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
) -> Vec<EpicArcStateEntry> {
    snap.owner_notes
        .get(&epic_arc_note_id(char_id))
        .and_then(|n| serde_json::from_str(&n.note).ok())
        .unwrap_or_default()
}

pub fn save_epic_arc_states(
    snap: &mut eve_db::WorldStateSnapshot,
    char_id: i64,
    entries: &[EpicArcStateEntry],
) {
    let nid = epic_arc_note_id(char_id);
    if let Ok(note) = serde_json::to_string(entries) {
        snap.owner_notes.insert(
            nid,
            eve_db::OwnerNoteRecord {
                note_id: nid,
                character_id: -5,
                item_id: char_id,
                label: format!("epic_arcs:{char_id}"),
                note,
                timestamp_unix: 0,
            },
        );
    }
}

fn locator_cooldown_note_id(char_id: i64, agent_id: i64) -> i64 {
    let mixed = ((char_id.unsigned_abs() & 0xFFFF_FFFF) << 20) ^ (agent_id.unsigned_abs() & 0xFFFFF);
    -((mixed as i64).max(1) + 5_300_000_000_000)
}

pub fn resolve_epic_arc_id_for_agent(ctx: &ServerContext, agent_id: i64) -> (u32, i64) {
    let corp_id = ctx.sde.agents.get(&agent_id).map_or(1_000_130, |a| a.corporation_id);
    let faction_id = ctx.sde.npc_corporations
        .get(&corp_id)
        .and_then(|c| c.faction_id)
        .map_or(500_003, |f| f as i64);
    let arc_id = match faction_id {
        500_001 => 2,
        500_002 => 3,
        500_003 => 4,
        500_004 => 5,
        500_010 => 6,
        500_011 => 7,
        _ => 1,
    };
    (arc_id, faction_id)
}

pub fn is_pirate_entity(ctx: &ServerContext, entity_id: i64) -> bool {
    let is_pirate_fac = |fid: i64| matches!(fid, 500_010 | 500_011 | 500_012 | 500_019 | 500_020);
    if is_pirate_fac(entity_id) {
        return true;
    }
    if let Some(fid) = ctx.sde.npc_corporations.get(&entity_id).and_then(|c| c.faction_id) {
        return is_pirate_fac(fid as i64);
    }
    if let Some(agent) = ctx.sde.agents.get(&entity_id) {
        if let Some(fid) = ctx.sde.npc_corporations.get(&agent.corporation_id).and_then(|c| c.faction_id) {
            return is_pirate_fac(fid as i64);
        }
    }
    false
}

pub fn compute_effective_standing_breakdown(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
    entity_id: i64,
    raw_standing: f64,
    has_entry: bool,
) -> (f64, f64, f64, f64) {
    let sk = snap.skills.get(&char_id);
    let conn_lvl = sk
        .and_then(|m| m.get(&SKILL_CONNECTIONS))
        .map(|s| s.level.min(5))
        .unwrap_or(0);
    let dip_lvl = sk
        .and_then(|m| m.get(&SKILL_DIPLOMACY))
        .map(|s| s.level.min(5))
        .unwrap_or(0);
    let crim_lvl = sk
        .and_then(|m| m.get(&SKILL_CRIMINAL_CONNECTIONS))
        .map(|s| s.level.min(5))
        .unwrap_or(0);

    if raw_standing < 0.0 {
        let dip_bonus = (10.0 - raw_standing) * 0.04 * (dip_lvl as f64);
        let eff = (raw_standing + dip_bonus).clamp(-10.0, 10.0);
        (eff, 0.0, dip_bonus, 0.0)
    } else if has_entry || raw_standing > 0.0 {
        if is_pirate_entity(ctx, entity_id) {
            let crim_bonus = (10.0 - raw_standing) * 0.04 * (crim_lvl as f64);
            let eff = (raw_standing + crim_bonus).clamp(-10.0, 10.0);
            (eff, 0.0, 0.0, crim_bonus)
        } else {
            let conn_bonus = (10.0 - raw_standing) * 0.04 * (conn_lvl as f64);
            let eff = (raw_standing + conn_bonus).clamp(-10.0, 10.0);
            (eff, conn_bonus, 0.0, 0.0)
        }
    } else {
        (0.0, 0.0, 0.0, 0.0)
    }
}

pub fn compute_effective_agent_standing(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
    agent_id: i64,
    corp_id: i64,
    faction_id: Option<i64>,
) -> f64 {
    let Some(ch) = snap.characters.get(&char_id) else {
        return 0.0;
    };
    let mut vals = Vec::with_capacity(3);
    if let Some(&raw) = ch.standings.get(&agent_id) {
        let (eff, _, _, _) =
            compute_effective_standing_breakdown(ctx, snap, char_id, agent_id, raw, true);
        vals.push(eff);
    }
    if let Some(&raw) = ch.standings.get(&corp_id) {
        let (eff, _, _, _) =
            compute_effective_standing_breakdown(ctx, snap, char_id, corp_id, raw, true);
        vals.push(eff);
    }
    if let Some(fid) = faction_id {
        if let Some(&raw) = ch.standings.get(&fid) {
            let (eff, _, _, _) =
                compute_effective_standing_breakdown(ctx, snap, char_id, fid, raw, true);
            vals.push(eff);
        }
    }
    if vals.is_empty() {
        return 0.0;
    }
    if vals.iter().any(|&v| v <= -2.0) {
        return vals.into_iter().fold(10.0_f64, f64::min);
    }
    vals.into_iter().fold(-10.0_f64, f64::max)
}

pub fn static_npc_faction_standings() -> Vec<(i64, i64, f64)> {
    vec![
        // Caldari State (500001)
        (500_001, 500_003, 0.5),
        (500_001, 500_004, -0.5),
        (500_001, 500_002, -0.3),
        (500_001, 500_007, 0.4),
        (500_001, 500_008, 0.5),
        (500_001, 500_010, -0.8),
        (500_001, 500_019, -1.0),
        // Minmatar Republic (500002)
        (500_002, 500_004, 0.5),
        (500_002, 500_003, -0.5),
        (500_002, 500_001, -0.3),
        (500_002, 500_007, -0.7),
        (500_002, 500_011, -0.8),
        (500_002, 500_015, 0.5),
        (500_002, 500_019, -1.0),
        // Amarr Empire (500003)
        (500_003, 500_001, 0.5),
        (500_003, 500_002, -0.5),
        (500_003, 500_004, -0.3),
        (500_003, 500_007, 0.8),
        (500_003, 500_008, 0.7),
        (500_003, 500_012, -0.8),
        (500_003, 500_019, -1.0),
        // Gallente Federation (500004)
        (500_004, 500_002, 0.5),
        (500_004, 500_001, -0.5),
        (500_004, 500_003, -0.3),
        (500_004, 500_016, 0.4),
        (500_004, 500_020, -0.8),
        (500_004, 500_019, -1.0),
        // CONCORD Assembly (500006)
        (500_006, 500_001, 0.5),
        (500_006, 500_002, 0.5),
        (500_006, 500_003, 0.5),
        (500_006, 500_004, 0.5),
        (500_006, 500_010, -1.0),
        (500_006, 500_011, -1.0),
        (500_006, 500_012, -1.0),
        (500_006, 500_019, -1.0),
        (500_006, 500_020, -1.0),
        // Ammatar Mandate (500007) & Khanid Kingdom (500008)
        (500_007, 500_003, 0.8),
        (500_007, 500_002, -0.7),
        (500_008, 500_003, 0.7),
        (500_008, 500_001, 0.5),
        // Pirate factions (Guristas 500010, Angel 500011, Blood Raiders 500012, Sansha 500019, Serpentis 500020)
        (500_010, 500_001, -0.8),
        (500_011, 500_002, -0.8),
        (500_012, 500_003, -0.8),
        (500_019, 500_001, -1.0),
        (500_019, 500_002, -1.0),
        (500_019, 500_003, -1.0),
        (500_019, 500_004, -1.0),
        (500_020, 500_004, -0.8),
        // Triglavian Collective (500026) <-> EDENCOM (500027)
        (500_026, 500_027, -1.0),
        (500_027, 500_026, -1.0),
    ]
}

pub fn apply_mission_completion_standings(
    ctx: &ServerContext,
    snap: &mut eve_db::WorldStateSnapshot,
    char_id: i64,
    agent_id: i64,
    corp_id: i64,
    base_standing_pct: f64,
    is_storyline_or_special: bool,
) -> (f64, Vec<(i64, f64, f64)>) {
    let social_lvl = snap.skills.get(&char_id)
        .and_then(|m| m.get(&SKILL_SOCIAL))
        .map(|s| s.level.min(5))
        .unwrap_or(0);
    let scaled_pct = base_standing_pct * (1.0 + 0.05 * (social_lvl as f64));
    let mut changes: Vec<(i64, f64, f64)> = Vec::new();
    let mut new_corp_st = 0.0;

    if let Some(ch) = snap.characters.get_mut(&char_id) {
        let cur_corp = ch.standings.get(&corp_id).copied().unwrap_or(0.0);
        new_corp_st = apply_standing_increase(cur_corp, scaled_pct);
        ch.standings.insert(corp_id, new_corp_st);
        let corp_delta = new_corp_st - cur_corp;
        changes.push((corp_id, new_corp_st, corp_delta));

        if agent_id > 0 && scaled_pct > 0.0 {
            let cur_agent = ch.standings.get(&agent_id).copied().unwrap_or(0.0);
            let new_agent = apply_standing_increase(cur_agent, scaled_pct);
            ch.standings.insert(agent_id, new_agent);
            changes.push((agent_id, new_agent, new_agent - cur_agent));
        }
    }

    let faction_id = match corp_id {
        1_000_298 => Some(500_026), // Perun Clade -> Triglavian Collective
        1_000_299 => Some(500_027), // AEGIS -> EDENCOM
        _ => ctx.sde.npc_corporations
            .get(&corp_id)
            .and_then(|c| c.faction_id)
            .map(|f| f as i64),
    };

    if scaled_pct > 0.0 {
        if let Some(fid) = faction_id {
            let fac_pct = if is_storyline_or_special {
                scaled_pct * 0.50
            } else {
                scaled_pct * 0.15
            };
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                let cur_fac = ch.standings.get(&fid).copied().unwrap_or(0.0);
                let new_fac = apply_standing_increase(cur_fac, fac_pct);
                let fac_delta = new_fac - cur_fac;
                ch.standings.insert(fid, new_fac);
                changes.push((fid, new_fac, fac_delta));

                // Propagate derived standings to allied and opposing factions
                for (src_f, dst_f, rel) in static_npc_faction_standings() {
                    if src_f == fid {
                        let cur_other = ch.standings.get(&dst_f).copied().unwrap_or(0.0);
                        let new_other = if rel > 0.0 {
                            apply_standing_increase(cur_other, fac_pct * rel * 0.5)
                        } else {
                            let loss_pct = (fac_pct * rel.abs() * 0.4).min(10.0);
                            (cur_other - (cur_other + 10.0) * (loss_pct / 100.0)).clamp(-10.0, 10.0)
                        };
                        let d_other = new_other - cur_other;
                        if d_other.abs() > 1e-6 {
                            ch.standings.insert(dst_f, new_other);
                            changes.push((dst_f, new_other, d_other));
                        }
                    }
                }
            }
        }
    }

    for &(from_id, _new_val, delta) in &changes {
        if delta.abs() > 1e-9 {
            snap.next_entity_id += 1;
            let ev_id = snap.next_entity_id;
            snap.standing_transactions
                .entry(char_id)
                .or_default()
                .push(StandingTransactionRecord {
                    event_id: ev_id,
                    from_id,
                    to_id: char_id,
                    modification: delta,
                    event_type_id: if delta >= 0.0 { 85 } else { 83 },
                });
        }
    }

    (new_corp_st, changes)
}

pub fn notify_standing_changes(
    ctx: &ServerContext,
    char_id: i64,
    changes: &[(i64, f64, f64)],
) {
    if changes.is_empty() {
        return;
    }
    let mods: Vec<PyRep> = changes
        .iter()
        .map(|&(from_id, _new_val, delta)| {
            PyRep::Tuple(vec![
                PyRep::Integer(from_id),
                PyRep::Integer(char_id),
                PyRep::Real(delta),
                PyRep::Real(-10.0),
                PyRep::Real(10.0),
            ])
        })
        .collect();
    if let Some(&(first_from, first_val, _)) = changes.first() {
        ctx.notify(
            NotificationTarget::Character(char_id),
            "OnStandingSet",
            "character",
            vec![
                PyRep::Integer(first_from),
                PyRep::Integer(char_id),
                PyRep::Real(first_val),
            ],
        );
    }
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnStandingsModified",
        "character",
        vec![PyRep::List(mods)],
    );
}

#[allow(clippy::too_many_arguments)]
pub fn handle_special_dialogue_action(
    ctx: &ServerContext,
    session: &mut SessionState,
    char_id: i64,
    sys_id: u32,
    action_id: i64,
    agent_id: i64,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match action_id {
        // 12 = agentDialogueButtonStartResearch (feat:pve.research-agents)
        12 => {
            let Some(agent_info) = ctx.sde.agents.get(&agent_id) else {
                return Some(Err(BridgeRpcError { code: "UnknownAgent".into(), message: format!("Agent {agent_id} does not exist in SDE") }));
            };
            if agent_info.agent_type_id != 4 {
                return Some(Err(BridgeRpcError { code: "NotResearchAgent".into(), message: format!("Agent {agent_id} is not an R&D research agent") }));
            }
            if agent_info.solar_system_id.is_some_and(|asys| asys != sys_id)
                || (session.station_id.is_some()
                    && agent_info.station_id.is_some()
                    && agent_info.station_id != session.station_id)
            {
                return Some(Err(BridgeRpcError { code: "AgentNotCoLocated".into(), message: format!("Must be co-located with R&D agent {agent_id} to start research") }));
            }
            let req_field_skill = args
                .get(2)
                .and_then(|a| a.as_i64())
                .map(|v| v as TypeId)
                .unwrap_or(11448); // Minmatar Starship Engineering
            if !ctx.sde.types.get(&req_field_skill).is_some_and(|t| t.group_id == 270 && t.type_id != 3402) {
                return Some(Err(BridgeRpcError { code: "InvalidResearchSkill".into(), message: format!("Type {req_field_skill} is not a valid R&D science field skill") }));
            }
            let now_unix = ctx.now_unix();
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let skill_lvl = snap.skills.get(&char_id)
                    .and_then(|m| m.get(&req_field_skill))
                    .map(|s| s.level)
                    .unwrap_or(0);
                if skill_lvl < 1 {
                    return Err(BridgeRpcError { code: "ErrorNoRelevantResearchSkills".into(), message: format!("Starting R&D research with agent {agent_id} requires research skill {req_field_skill} >= 1") });
                }
                let eff_st = compute_effective_agent_standing(ctx, snap, char_id, agent_id, agent_info.corporation_id, None);
                if agent_info.level > 1 {
                    let req_st = ((agent_info.level as f64) - 1.0) * 2.0 - 1.0;
                    if eff_st < req_st {
                        return Err(BridgeRpcError { code: "InsufficientAgentStanding".into(), message: format!("R&D agent {agent_id} (level {}) requires effective standing >= {req_st:.1} (have {eff_st:.2})", agent_info.level) });
                    }
                }
                let rp_per_day =
                    (10.0 + 12.0 * (skill_lvl as f64) + 2.5 * eff_st.max(0.0)) * (agent_info.level as f64);
                let mut entries = load_rnd_research_entries(snap, char_id);
                if let Some(existing) = entries.iter_mut().find(|e| e.agent_id == agent_id) {
                    let elapsed_days = (now_unix.saturating_sub(existing.started_unix) as f64) / 86_400.0;
                    existing.initial_points += existing.points_per_day * elapsed_days;
                    existing.points_per_day = rp_per_day;
                    existing.research_type_id = req_field_skill;
                    existing.started_unix = now_unix;
                } else {
                    entries.push(RndResearchEntry {
                        agent_id,
                        research_type_id: req_field_skill,
                        points_per_day: rp_per_day,
                        initial_points: 0.0,
                        started_unix: now_unix,
                    });
                }
                save_rnd_research_entries(snap, char_id, &entries);
                Ok(rp_per_day)
            });
            match res {
                Ok(rp_per_day) => Some(Ok(PyRep::keyval([
                    ("agentID", PyRep::Integer(agent_id)),
                    ("researchTypeID", PyRep::Integer(req_field_skill as i64)),
                    ("pointsPerDay", PyRep::Real(rp_per_day)),
                    ("researchStarted", PyRep::Bool(true)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        // 13 = agentDialogueButtonCancelResearch
        13 => {
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let mut entries = load_rnd_research_entries(snap, char_id);
                let before = entries.len();
                entries.retain(|e| e.agent_id != agent_id);
                if entries.len() == before {
                    return Err(BridgeRpcError { code: "ResearchNotActive".into(), message: format!("No active R&D research with agent {agent_id}") });
                }
                save_rnd_research_entries(snap, char_id, &entries);
                Ok(())
            });
            match res {
                Ok(()) => Some(Ok(PyRep::keyval([
                    ("agentID", PyRep::Integer(agent_id)),
                    ("researchCancelled", PyRep::Bool(true)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        // 14 = agentDialogueButtonBuyDatacores (feat:pve.research-agents)
        14 => {
            let qty_opt = match args.get(2).and_then(|a| a.as_i64()) {
                Some(raw) => i32::try_from(raw).ok().filter(|&q| (1..=100_000).contains(&q)),
                None => Some(1),
            };
            let Some(qty) = qty_opt else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidDatacoreQuantity".into(),
                    message: "Datacore quantity must be between 1 and 100,000".into(),
                }));
            };
            if let Some(agent_info) = ctx.sde.agents.get(&agent_id) {
                if agent_info.solar_system_id.is_some_and(|asys| asys != sys_id)
                    || (session.station_id.is_some()
                        && agent_info.station_id.is_some()
                        && agent_info.station_id != session.station_id)
                {
                    return Some(Err(BridgeRpcError { code: "AgentNotCoLocated".into(), message: format!("Must be co-located with R&D agent {agent_id} to redeem datacores") }));
                }
            }
            let now_unix = ctx.now_unix();
            let station_id = ctx.sde.agents.get(&agent_id).and_then(|a| a.station_id).or(session.station_id).unwrap_or(60_003_760);
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let mut entries = load_rnd_research_entries(snap, char_id);
                let Some(entry) = entries.iter_mut().find(|e| e.agent_id == agent_id) else {
                    return Err(BridgeRpcError { code: "ResearchNotActive".into(), message: format!("No active R&D research with agent {agent_id}") });
                };
                let elapsed_days = (now_unix.saturating_sub(entry.started_unix) as f64) / 86_400.0;
                let total_rp = entry.initial_points + entry.points_per_day * elapsed_days;
                let rp_cost = 50.0 * (qty as f64);
                if total_rp + 1e-6 < rp_cost {
                    return Err(BridgeRpcError { code: "InsufficientResearchPoints".into(), message: format!("Buying {qty} datacore(s) requires {rp_cost:.1} RP, character has {total_rp:.1} RP") });
                }
                let isk_cost = IskCents::from_isk(10_000 * i64::from(qty));
                let mut bal_after_opt = None;
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    if ch.balance < isk_cost {
                        return Err(BridgeRpcError { code: "InsufficientFunds".into(), message: "Insufficient ISK to redeem R&D datacores".into() });
                    }
                    ch.balance = ch.balance.checked_sub(isk_cost).unwrap_or(IskCents(0));
                    bal_after_opt = Some(ch.balance);
                }
                if let Some(bal_after) = bal_after_opt {
                    snap.append_journal(
                        char_id,
                        "DatacoreFee",
                        IskCents(-isk_cost.0),
                        bal_after,
                        format!("Redeemed {qty} datacore(s) from R&D agent {agent_id}"),
                    );
                }
                let rem_rp = (total_rp - rp_cost).max(0.0);
                entry.initial_points = rem_rp;
                entry.started_unix = now_unix;
                let dc_type_id: TypeId = if entry.research_type_id == 11452 {
                    20172
                } else {
                    20424 // Datacore - Minmatar Starship Engineering
                };
                save_rnd_research_entries(snap, char_id, &entries);
                snap.next_entity_id += 1;
                let item_id = snap.next_entity_id;
                snap.items.insert(
                    item_id,
                    InventoryItemRecord {
                        item_id,
                        type_id: dc_type_id,
                        owner_id: char_id,
                        location_id: station_id,
                        flag: InventoryFlag::Hangar,
                        quantity: qty,
                        singleton: false,
                        version: 1,
                        ..Default::default()
                    },
                );
                Ok((item_id, dc_type_id, rem_rp))
            });
            match res {
                Ok((item_id, dc_type_id, rem_rp)) => Some(Ok(PyRep::keyval([
                    ("agentID", PyRep::Integer(agent_id)),
                    ("datacoreItemID", PyRep::Integer(item_id)),
                    ("datacoreTypeID", PyRep::Integer(dc_type_id as i64)),
                    ("quantity", PyRep::Integer(qty as i64)),
                    ("remainingResearchPoints", PyRep::Real(rem_rp)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        // 15 | 16 | 17 = Locator Agent service (feat:pve.locator-agents)
        15..=17 => {
            if action_id == 17 {
                return Some(Ok(PyRep::keyval([
                    ("agentID", PyRep::Integer(agent_id)),
                    ("locateRejected", PyRep::Bool(true)),
                ])));
            }
            let Some(agent_info) = ctx.sde.agents.get(&agent_id) else {
                return Some(Err(BridgeRpcError { code: "UnknownAgent".into(), message: format!("Agent {agent_id} does not exist in SDE") }));
            };
            if !agent_info.is_locator {
                return Some(Err(BridgeRpcError { code: "AgentHasNoLocatorService".into(), message: format!("Agent {agent_id} does not offer Locator services") }));
            }
            let caller_sys = session.solar_system_id.unwrap_or(30_000_142);
            if agent_info.solar_system_id != Some(caller_sys)
                || (session.station_id.is_some()
                    && agent_info.station_id.is_some()
                    && agent_info.station_id != session.station_id)
            {
                return Some(Err(BridgeRpcError {
                    code: "AgentNotCoLocated".into(),
                    message: format!(
                        "Must be co-located with Locator Agent {agent_id} in system {:?} / station {:?}",
                        agent_info.solar_system_id, agent_info.station_id
                    ),
                }));
            }
            let target_char_id = args
                .get(2)
                .or_else(|| args.get(1))
                .and_then(|a| a.as_i64())
                .filter(|&id| id != agent_id)
                .unwrap_or(90_000_002);
            let now_unix = ctx.now_unix();
            let (isk_fee, cooldown_sec, precision_desc) = match agent_info.level {
                1 => (2_000_i64, 900_u64, "SameConstellation"),
                2 => (8_000, 480, "SameRegion"),
                3 => (25_000, 240, "KnownSpace"),
                4 => (75_000, 120, "AllKnownSpace"),
                _ => (150_000, 60, "AllKnownSpaceImmediate"),
            };
            let cd_nid = locator_cooldown_note_id(char_id, agent_id);
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let eff_st = compute_effective_agent_standing(ctx, snap, char_id, agent_id, agent_info.corporation_id, None);
                if agent_info.level > 1 {
                    let req_st = ((agent_info.level as f64) - 1.0) * 2.0 - 1.0;
                    if eff_st < req_st {
                        return Err(BridgeRpcError { code: "InsufficientAgentStanding".into(), message: format!("Locator agent {agent_id} (level {}) requires effective standing >= {req_st:.1} (have {eff_st:.2})", agent_info.level) });
                    }
                } else if eff_st <= -2.0 {
                    return Err(BridgeRpcError { code: "InsufficientAgentStanding".into(), message: format!("Locator agent {agent_id} (level 1) requires effective standing > -2.0 (have {eff_st:.2})") });
                }
                if let Some(cd_note) = snap.owner_notes.get(&cd_nid) {
                    if now_unix < cd_note.timestamp_unix {
                        return Err(BridgeRpcError { code: "LocatorAgentOnCooldown".into(), message: format!("Locator agent {agent_id} is on cooldown until {}", cd_note.timestamp_unix) });
                    }
                }
                let Some(target_ch) = snap.characters.get(&target_char_id).cloned() else {
                    return Err(BridgeRpcError { code: "TargetCharacterNotFound".into(), message: format!("Target character {target_char_id} not found") });
                };
                if target_ch.solar_system_id >= 31_000_000 {
                    return Err(BridgeRpcError {
                        code: "LocatorTargetInUnknownSpace".into(),
                        message: format!("Locator agents cannot locate targets in wormhole or abyssal space (system {})", target_ch.solar_system_id),
                    });
                }
                let sys_info = ctx.sde.solar_systems.get(&target_ch.solar_system_id);
                let const_id = sys_info.map(|s| s.constellation_id).unwrap_or(20_000_020);
                let reg_id = sys_info.map(|s| s.region_id).unwrap_or(10_000_002);
                if reg_id >= 11_000_000 {
                    return Err(BridgeRpcError {
                        code: "LocatorTargetInUnknownSpace".into(),
                        message: format!("Locator agents cannot locate targets in unknown space region {reg_id}"),
                    });
                }
                let agent_sys_info = agent_info.solar_system_id.and_then(|sid| ctx.sde.solar_systems.get(&sid));
                let agent_const_id = agent_sys_info.map(|s| s.constellation_id).unwrap_or(20_000_020);
                let agent_reg_id = agent_sys_info.map(|s| s.region_id).unwrap_or(10_000_002);
                if agent_info.level == 1 && const_id != agent_const_id {
                    return Err(BridgeRpcError {
                        code: "LocatorTargetOutOfRange".into(),
                        message: format!("Level 1 locator agent {agent_id} can only locate targets in constellation {agent_const_id}"),
                    });
                }
                if agent_info.level == 2 && reg_id != agent_reg_id {
                    return Err(BridgeRpcError {
                        code: "LocatorTargetOutOfRange".into(),
                        message: format!("Level 2 locator agent {agent_id} can only locate targets in region {agent_reg_id}"),
                    });
                }
                let fee_cents = IskCents::from_isk(isk_fee);
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    if ch.balance < fee_cents {
                        return Err(BridgeRpcError { code: "InsufficientFunds".into(), message: format!("Locator service requires {isk_fee} ISK") });
                    }
                    ch.balance = ch.balance.checked_sub(fee_cents).unwrap_or(IskCents(0));
                    let bal_after = ch.balance;
                    snap.append_journal(
                        char_id,
                        "AgentLocationServices",
                        fee_cents,
                        bal_after,
                        format!("Locator agent {agent_id} fee for character {target_char_id}"),
                    );
                }
                snap.owner_notes.insert(
                    cd_nid,
                    eve_db::OwnerNoteRecord {
                        note_id: cd_nid,
                        character_id: -5,
                        item_id: agent_id,
                        label: format!("locator_cd:{char_id}:{agent_id}"),
                        note: target_char_id.to_string(),
                        timestamp_unix: now_unix + cooldown_sec,
                    },
                );
                Ok((
                    target_ch.solar_system_id,
                    const_id,
                    reg_id,
                    target_ch.station_id,
                ))
            });
            match res {
                Ok((t_sys, t_const, t_reg, t_sta)) => Some(Ok(PyRep::keyval([
                    ("agentID", PyRep::Integer(agent_id)),
                    ("targetCharacterID", PyRep::Integer(target_char_id)),
                    ("solarSystemID", PyRep::Integer(t_sys as i64)),
                    ("constellationID", PyRep::Integer(t_const as i64)),
                    ("regionID", PyRep::Integer(t_reg as i64)),
                    ("stationID", t_sta.map(PyRep::Integer).unwrap_or(PyRep::None)),
                    ("agentLevel", PyRep::Integer(agent_info.level as i64)),
                    ("precision", PyRep::String(precision_desc.into())),
                    ("iskFee", PyRep::Integer(isk_fee)),
                    ("cooldownSeconds", PyRep::Integer(cooldown_sec as i64)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        // 18 | 19 = agentDialogueButtonYes / No (Epic Arc choice branching: feat:pve.epic-arcs, feat:pve.storyline-arcs-soe)
        18 | 19 => {
            let branch_name = args
                .get(2)
                .and_then(|a| a.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| {
                    if action_id == 18 {
                        "Amarr".into()
                    } else {
                        "Minmatar".into()
                    }
                });
            let (arc_id, faction_id) = resolve_epic_arc_id_for_agent(ctx, agent_id);
            ctx.db.with_mut_snapshot(|snap| {
                let mut arcs = load_epic_arc_states(snap, char_id);
                if let Some(arc) = arcs.iter_mut().find(|a| a.epic_arc_id == arc_id) {
                    arc.chosen_branch = Some(branch_name.clone());
                    arc.chapter = arc.chapter.max(2);
                } else {
                    arcs.push(EpicArcStateEntry {
                        epic_arc_id: arc_id,
                        agent_id,
                        faction_id,
                        chapter: 2,
                        completed_missions: Vec::new(),
                        chosen_branch: Some(branch_name.clone()),
                        completed_at_unix: None,
                        cooldown_until_unix: None,
                    });
                }
                save_epic_arc_states(snap, char_id, &arcs);
            });
            Some(Ok(PyRep::keyval([
                ("epicArcID", PyRep::Integer(arc_id as i64)),
                ("agentID", PyRep::Integer(agent_id)),
                ("solarSystemID", PyRep::Integer(sys_id as i64)),
                ("chosenBranch", PyRep::String(branch_name)),
                ("repeatCooldownSeconds", PyRep::Integer(7_776_000)),
            ])))
        }

        _ => None,
    }
}

pub async fn handle_extended(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("agentMgr", "agentID")
        | ("agentMgr", "corporationID")
        | ("agentMgr", "factionID")
        | ("agentMgr", "solarsystemID")
        | ("agentMgr", "stationID") => {
            let agent_id = match args.first().and_then(|a| a.as_i64()).or_else(|| {
                ctx.bound_objects.read().ok().and_then(|map| {
                    map.values()
                        .filter(|b| b.session_id == session.session_id && b.service == "agentMgr" && b.object_id > 0)
                        .max_by_key(|b| b.bound_id)
                        .map(|b| b.object_id)
                })
            }) {
                Some(id) => id,
                None => {
                    return Some(Err(BridgeRpcError {
                        code: "AgentNotFound".into(),
                        message: "agentMgr attribute lookup requires agentID or bound agent moniker".into(),
                    }));
                }
            };
            let Some(agent) = ctx.sde.agents.get(&agent_id) else {
                return Some(Err(BridgeRpcError {
                    code: "UnknownAgent".into(),
                    message: format!("Agent {agent_id} does not exist in SDE"),
                }));
            };
            let corp_id = agent.corporation_id;
            let faction_id = ctx.sde.npc_corporations.get(&corp_id).and_then(|c| c.faction_id).unwrap_or(0);
            let val = match method {
                "agentID" => PyRep::Integer(agent.agent_id),
                "corporationID" => PyRep::Integer(corp_id),
                "factionID" => PyRep::Integer(faction_id as i64),
                "solarsystemID" => agent.solar_system_id.map(|s| PyRep::Integer(s as i64)).unwrap_or(PyRep::None),
                "stationID" => agent.station_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                _ => PyRep::None,
            };
            Some(Ok(val))
        }

        ("agentMgr", "GetCompletedCareerAgentIDs") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let ids = ctx.db.with_snapshot(|snap| {
                let mut completed_agents: Vec<i64> = snap.active_missions.values()
                    .filter(|m| m.character_id == char_id && m.state == "Completed")
                    .filter_map(|m| ctx.sde.agents.get(&m.agent_id).filter(|a| a.agent_type_id == 12).map(|a| a.agent_id))
                    .collect();
                completed_agents.sort_unstable();
                completed_agents.dedup();
                completed_agents.into_iter().map(PyRep::Integer).collect::<Vec<_>>()
            });
            Some(Ok(PyRep::List(ids)))
        }

        ("agentMgr", "GetDisabledMissions") => {
            // const-ok: no mission IDs are disabled in EvoMu's SDE mission pool
            let _ = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            Some(Ok(PyRep::List(vec![])))
        }

        ("agentMgr", "GetReplayTimestamp") => {
            // `eve/client/script/ui/station/agents/agents.pyj:485-491` (`GetAgentTimers`):
            // calls `self.GetAgentMoniker(agentID).GetReplayTimestamp(session.charid)` and stores
            // the returned timestamp (or `None` when the career agent is not on a replay timer).
            let _ = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            Some(Ok(PyRep::None))
        }

        ("agentMgr", "GetPlausibleMissionIDs") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let target_aid = args.first().and_then(|a| a.as_i64()).or_else(|| {
                ctx.bound_objects.read().ok().and_then(|map| {
                    map.values()
                        .filter(|b| b.session_id == session.session_id && b.service == "agentMgr" && b.object_id > 0)
                        .max_by_key(|b| b.bound_id)
                        .map(|b| b.object_id)
                })
            });
            let mut plausible: Vec<u32> = Vec::new();
            if let Some(aid) = target_aid {
                if let Some(_agent) = ctx.sde.agents.get(&aid) {
                    for mid in ctx.sde.missions.keys().take(25) {
                        plausible.push(*mid);
                    }
                }
            } else {
                let active_aids = ctx.db.with_snapshot(|snap| {
                    snap.active_missions.iter()
                        .filter(|((cid, _), _)| *cid == char_id)
                        .map(|(_, m)| m.agent_id)
                        .collect::<Vec<_>>()
                });
                for aid in active_aids {
                    if let Some(_agent) = ctx.sde.agents.get(&aid) {
                        for mid in ctx.sde.missions.keys().take(25) {
                            plausible.push(*mid);
                        }
                    }
                    if plausible.len() >= 25 { break; }
                }
                if plausible.is_empty() {
                    for mid in ctx.sde.missions.keys().take(15) { plausible.push(*mid); }
                }
            }
            plausible.sort_unstable();
            plausible.dedup();
            Some(Ok(PyRep::List(plausible.into_iter().map(|m| PyRep::Integer(m as i64)).collect())))
        }

        ("agentMgr", "GetMyEpicArcStatus") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let now_unix = ctx.now_unix();
            let list = ctx.db.with_snapshot(|snap| {
                load_epic_arc_states(snap, char_id)
                    .into_iter()
                    .map(|a| {
                        let on_cd = a.cooldown_until_unix.is_some_and(|until| now_unix < until);
                        let state_str = if a.completed_at_unix.is_some() {
                            if on_cd { "Cooldown" } else { "Completed" }
                        } else {
                            "Active"
                        };
                        PyRep::keyval([
                            ("epicArcID", PyRep::Integer(a.epic_arc_id as i64)),
                            ("agentID", PyRep::Integer(a.agent_id)),
                            ("factionID", PyRep::Integer(a.faction_id)),
                            ("chapter", PyRep::Integer(a.chapter as i64)),
                            ("state", PyRep::String(state_str.into())),
                            ("onCooldown", PyRep::Bool(on_cd)),
                            ("chosenBranch", a.chosen_branch.map(PyRep::String).unwrap_or(PyRep::None)),
                            ("completedMissions", PyRep::List(a.completed_missions.into_iter().map(|m| PyRep::Integer(m as i64)).collect())),
                            ("cooldownUntilUnix", a.cooldown_until_unix.map(|u| PyRep::Integer(u as i64)).unwrap_or(PyRep::None)),
                            ("repeatCooldownSeconds", PyRep::Integer(7_776_000)),
                        ])
                    })
                    .collect::<Vec<_>>()
            });
            Some(Ok(PyRep::List(list)))
        }

        ("agentMgr", "GetMessagesForEpicArcMissions") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let msgs = ctx.db.with_snapshot(|snap| {
                load_epic_arc_states(snap, char_id)
                    .into_iter()
                    .map(|a| {
                        (
                            PyRep::Integer(a.epic_arc_id as i64),
                            PyRep::keyval([
                                ("epicArcID", PyRep::Integer(a.epic_arc_id as i64)),
                                ("agentID", PyRep::Integer(a.agent_id)),
                                (
                                    "message",
                                    PyRep::String(format!(
                                        "Epic Arc {} chapter {} briefing (branch: {:?})",
                                        a.epic_arc_id, a.chapter, a.chosen_branch
                                    )),
                                ),
                            ]),
                        )
                    })
                    .collect::<Vec<_>>()
            });
            Some(Ok(PyRep::Dict(msgs)))
        }

        ("epicArcStatus", "AgentHasEpicMissionsForCharacter") => {
            let (_, session_char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(agent_id) = args.first().and_then(|a| a.as_i64()).filter(|&id| id > 0) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArguments".into(),
                    message: "epicArcStatus.AgentHasEpicMissionsForCharacter requires a positive agentID".into(),
                }));
            };
            let char_id = session_char_id;
            let Some(agent) = ctx.sde.agents.get(&agent_id).filter(|a| a.agent_type_id == 10) else {
                return Some(Ok(PyRep::Bool(false)));
            };
            let has_active = ctx.db.with_snapshot(|snap| {
                snap.active_missions.values().any(|m| {
                    m.character_id == char_id
                        && m.agent_id == agent_id
                        && matches!(m.state.as_str(), "Offered" | "Accepted")
                })
            });
            if has_active {
                return Some(Ok(PyRep::Bool(true)));
            }
            let (arc_id, faction_id) = resolve_epic_arc_id_for_agent(ctx, agent_id);
            let now_unix = ctx.now_unix();
            let available = ctx.db.with_snapshot(|snap| {
                let eff = compute_effective_agent_standing(ctx, snap, char_id, agent_id, agent.corporation_id, Some(faction_id));
                eff > -2.0
                    && !load_epic_arc_states(snap, char_id)
                        .iter()
                        .any(|a| a.epic_arc_id == arc_id && a.cooldown_until_unix.is_some_and(|until| now_unix < until))
            });
            Some(Ok(PyRep::Bool(available)))
        }

        ("missionTrackerMgr", "GetAllMissionObjectives")
        | ("missionTrackerMgr", "UpdateAllMissions")
        | ("missionTrackerMgr", "GetMissionInfoItems") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let filter_mid = args.first().and_then(|a| a.as_i64()).and_then(|v| u32::try_from(v).ok());
            let items = ctx.db.with_snapshot(|snap| {
                let mut list: Vec<PyRep> = snap.active_missions.values()
                    .filter(|m| {
                        m.character_id == char_id
                            && matches!(m.state.as_str(), "Offered" | "Accepted")
                            && filter_mid.is_none_or(|f| f == m.mission_id)
                    })
                    .map(|m| {
                        let meta = load_mission_meta(snap, char_id, m.mission_id);
                        let obj_completed = m.npcs_remaining == 0
                            && meta.as_ref().and_then(|mt| mt.required_delivery_type_id).is_none_or(|req_tid| {
                                let req_q = meta.as_ref().map_or(1, |mt| mt.required_delivery_qty);
                                snap.items
                                    .values()
                                    .filter(|it| it.owner_id == char_id && it.type_id == req_tid)
                                    .map(|it| it.quantity)
                                    .sum::<i32>()
                                    >= req_q
                            });
                        PyRep::keyval([
                            ("missionID", PyRep::Integer(m.mission_id as i64)),
                            ("agentID", PyRep::Integer(m.agent_id)),
                            ("solarSystemID", PyRep::Integer(m.solar_system_id as i64)),
                            ("state", PyRep::String(m.state.clone())),
                            ("currentRoomID", PyRep::Integer(m.current_room_id as i64)),
                            ("npcsRemaining", PyRep::Integer(m.npcs_remaining as i64)),
                            ("objectivesCompleted", PyRep::Bool(obj_completed)),
                            (
                                "info",
                                PyRep::String(format!(
                                    "Mission {} for agent {}",
                                    m.mission_id, m.agent_id
                                )),
                            ),
                        ])
                    })
                    .collect();
                list.sort_by_key(|r| r.dict_get("missionID").and_then(|v| v.as_i64()).unwrap_or(0));
                list
            });
            if method == "UpdateAllMissions" {
                ctx.notify(NotificationTarget::Character(char_id), "OnMissionsUpdated", "charid", vec![PyRep::List(items.clone())]);
            }
            Some(Ok(PyRep::List(items)))
        }

        ("missionTrackerMgr", "IsInActiveDungeonID") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let sys_id = session.solar_system_id.unwrap_or(0);
            let in_dungeon = ctx.db.with_snapshot(|snap| {
                snap.active_missions.values().any(|m| {
                    m.character_id == char_id && m.state == "Accepted" && m.solar_system_id == sys_id
                })
            });
            Some(Ok(PyRep::Bool(in_dungeon)))
        }

        ("standingMgr", "GetCorpStandings") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let rows = ctx.db.with_snapshot(|snap| {
                let corp_id = snap.characters.get(&char_id).map_or(1_000_009, |c| c.corporation_id);
                let mut agg: HashMap<i64, (f64, usize)> = HashMap::new();
                for ch in snap.characters.values().filter(|c| c.corporation_id == corp_id) {
                    for (&from_id, &val) in &ch.standings {
                        let e = agg.entry(from_id).or_insert((0.0, 0));
                        e.0 += val;
                        e.1 += 1;
                    }
                }
                let mut entries: Vec<(i64, f64)> = agg
                    .into_iter()
                    .map(|(fid, (sum, cnt))| (fid, sum / (cnt.max(1) as f64)))
                    .collect();
                entries.sort_by_key(|&(fid, _)| fid);
                entries.into_iter().map(|(from_id, st)| vec![PyRep::Integer(from_id), PyRep::Real(st)]).collect::<Vec<_>>()
            });
            Some(Ok(PyRep::rowset(vec!["fromID", "standing"], rows)))
        }

        ("standingMgr", "GetNPCNPCStandings") => {
            let _ = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let rows: Vec<Vec<PyRep>> = static_npc_faction_standings()
                .into_iter()
                .map(|(from_id, to_id, st)| vec![PyRep::Integer(from_id), PyRep::Integer(to_id), PyRep::Real(st)])
                .collect();
            Some(Ok(PyRep::rowset(vec!["fromID", "toID", "standing"], rows)))
        }

        ("standingMgr", "GetStandingCompositions") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let target_from = args.first().and_then(|a| a.as_i64()).filter(|&id| id > 0);
            let target_to = args
                .get(1)
                .and_then(|a| a.as_i64())
                .filter(|&id| id > 0)
                .unwrap_or(char_id);
            let res = ctx.db.with_snapshot(|snap| {
                let Some(ch) = snap.characters.get(&target_to).or_else(|| snap.characters.get(&char_id)) else {
                    return PyRep::List(vec![]);
                };
                let mut entities: Vec<i64> = if let Some(fid) = target_from {
                    vec![fid]
                } else {
                    ch.standings.keys().copied().collect()
                };
                entities.sort_unstable();
                let list: Vec<PyRep> = entities
                    .into_iter()
                    .map(|from_id| {
                        let raw = ch.standings.get(&from_id).copied().unwrap_or(0.0);
                        let has_entry = ch.standings.contains_key(&from_id);
                        let (eff, conn_b, dip_b, crim_b) = compute_effective_standing_breakdown(
                            ctx, snap, ch.character_id, from_id, raw, has_entry,
                        );
                        PyRep::keyval([
                            ("fromID", PyRep::Integer(from_id)),
                            ("toID", PyRep::Integer(target_to)),
                            ("rawStanding", PyRep::Real(raw)),
                            ("standing", PyRep::Real(raw)),
                            ("effectiveStanding", PyRep::Real(eff)),
                            ("connectionsBonus", PyRep::Real(conn_b)),
                            ("diplomacyBonus", PyRep::Real(dip_b)),
                            ("criminalConnectionsBonus", PyRep::Real(crim_b)),
                        ])
                    })
                    .collect();
                PyRep::List(list)
            });
            Some(Ok(res))
        }

        ("keeper", "ActivateAccelerationGate") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            if session.station_id.is_some() || session.structure_id.is_some() {
                return Some(Err(BridgeRpcError { code: "ShipDocked".into(), message: "Cannot activate dungeon acceleration gate while docked".into() }));
            }
            let is_mission = args.first().and_then(|a| a.as_i64()).and_then(|raw_id| u32::try_from(raw_id).ok()).is_some_and(|mid| {
                ctx.db
                    .with_snapshot(|snap| snap.active_missions.contains_key(&(char_id, mid)))
            });
            if !is_mission {
                return None;
            }
            let Some(raw) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError { code: "InvalidMissionId".into(), message: "mission_id argument is required".into() }));
            };
            let Ok(mission_id) = u32::try_from(raw) else {
                return Some(Err(BridgeRpcError { code: "InvalidMissionId".into(), message: format!("mission_id {raw} is out of valid u32 range") }));
            };
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                // Enforce Anomic Burner frigate/small-hull gate restriction (feat:pve.anomic-burner-missions)
                let is_burner = load_mission_meta(snap, char_id, mission_id)
                    .is_some_and(|mt| mt.is_anomic_burner)
                    || ctx.sde.missions
                        .get(&mission_id)
                        .is_some_and(|t| t.title.contains("Anomic"));
                if is_burner {
                    if let Some(ship_id) = session.ship_id {
                        if let Some(ship_item) = snap.items.get(&ship_id) {
                            if let Some(ship_type) = ctx.sde.types.get(&ship_item.type_id) {
                                // Battleship (group 27), Battlecruiser (419), Capital (30, 485, 547) rejected
                                if matches!(ship_type.group_id, 27 | 30 | 419 | 485 | 547 | 659 | 898 | 900)
                                    || ship_type.mass >= 50_000_000.0
                                {
                                    return Err(BridgeRpcError { code: "ShipRestrictedByGate".into(), message: format!("Ship {} (group {}) is too large for this Anomic Burner acceleration gate", ship_type.name, ship_type.group_id) });
                                }
                            }
                        }
                    }
                }
                let Some(m) = snap.active_missions.get_mut(&(char_id, mission_id)) else {
                    return Err(BridgeRpcError { code: "MissionNotActive".into(), message: format!("Mission {mission_id} is not active for character {char_id}") });
                };
                if m.state != "Accepted" {
                    return Err(BridgeRpcError { code: "InvalidMissionState".into(), message: format!("Mission {mission_id} is in state '{}', not Accepted", m.state) });
                }
                if m.npcs_remaining > 0 {
                    return Err(BridgeRpcError { code: "RoomNpcsRemaining".into(), message: format!("Cannot activate acceleration gate while {} room NPC(s) remain", m.npcs_remaining) });
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

        ("air_npe", "skip_air_npe") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let now = ctx.now_unix();
            match crate::services::character_expert_systems::skip_air_npe_and_stop_graph(ctx, char_id, now) {
                Ok((graph_id, curr_node, completed)) => Some(Ok(PyRep::keyval([
                    ("nodeGraphID", PyRep::Integer(i64::from(graph_id))),
                    ("currentNodeID", PyRep::Integer(i64::from(curr_node))),
                    ("completed", PyRep::Bool(completed)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("air_npe", "get_air_npe_state") | ("air_npe", "is_air_npe_enabled") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
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

        _ => None,
    }
}
