//! V24 SDE-backed Escalation & Expedition chain resolution and localized journal templates
//! (`feat:pve.escalations`, `feat:pve.expeditions`, `notify:OnDungeonInstanceAdded`,
//! `notify:OnDungeonInstanceRemoved`).

#![forbid(unsafe_code)]

use eve_sde::{SdeRegistry, SolarSystemId};
use std::collections::BTreeMap;

/// Resolved multi-step V24 SDE escalation chain for an anomaly or signature clear.
#[derive(Debug, Clone)]
pub struct SdeEscalationChain {
    #[allow(dead_code)]
    pub faction_id: u32,
    pub step_dungeon_ids: Vec<u32>,
    pub expedition_name: String,
    pub total_steps: u8,
    pub journal_template_id: u32,
}

impl SdeEscalationChain {
    /// Returns the SDE `dungeon_id` for 1-based `step_index`.
    pub fn dungeon_for_step(&self, step_index: u8) -> u32 {
        let idx = usize::from(step_index.saturating_sub(1));
        self.step_dungeon_ids
            .get(idx)
            .copied()
            .or_else(|| self.step_dungeon_ids.last().copied())
            .unwrap_or(1422)
    }
}

fn infer_faction_id(sde: &SdeRegistry, source_dungeon_id: u32, origin_sys_id: SolarSystemId) -> u32 {
    if let Some(d) = sde.dungeons.get(&source_dungeon_id) {
        if let Some(fid) = d.faction_id {
            if fid != 500_021 && fid != 500_026 && fid != 500_027 {
                return fid;
            }
            if sde
                .dungeons
                .values()
                .any(|cand| cand.archetype_id == Some(31) && cand.faction_id == Some(fid))
            {
                return fid;
            }
        }
        let lower = d.name.to_ascii_lowercase();
        if lower.contains("angel") || lower.contains("domination") || lower.contains("cartel") {
            return 500_011;
        }
        if lower.contains("blood") || lower.contains("crimson") || lower.contains("corpii") {
            return 500_012;
        }
        if lower.contains("sansha") || lower.contains("centi") || lower.contains("nation") {
            return 500_019;
        }
        if lower.contains("serpentis") || lower.contains("coreli") || lower.contains("shadow") {
            return 500_020;
        }
        if lower.contains("gurista") || lower.contains("pith") {
            return 500_010;
        }
        if lower.contains("drone") || lower.contains("hive") || lower.contains("alvu") {
            return 500_021;
        }
    }

    match crate::services::pve_exploration_seeding::pirate_faction_for_system(sde, origin_sys_id) {
        crate::services::pve_exploration_seeding::PirateFaction::AngelCartel => 500_011,
        crate::services::pve_exploration_seeding::PirateFaction::BloodRaiders => 500_012,
        crate::services::pve_exploration_seeding::PirateFaction::Sansha => 500_019,
        crate::services::pve_exploration_seeding::PirateFaction::Serpentis => 500_020,
        crate::services::pve_exploration_seeding::PirateFaction::Guristas => 500_010,
    }
}

fn is_valid_escalation_template(d: &eve_sde::DungeonTemplate) -> bool {
    d.archetype_id == Some(31)
        && !d.name.contains("QA Test")
        && !d.name.contains("DO NOT TRANSLATE")
        && !d.name.contains("Test ")
}

/// Counts rooms in `res:/staticdata/dungeonobjectives.fsdbinary` for `dungeon_id`, if present.
pub fn sde_dungeon_room_count(sde: &SdeRegistry, dungeon_id: u32) -> u8 {
    let Ok(objectives) = sde.dungeon_objectives() else {
        return 2;
    };
    let mut max_room: u8 = 0;
    for val in objectives.values() {
        let did = val
            .get("dungeonID")
            .and_then(|v| v.as_u64())
            .or_else(|| val.get("dungeonId").and_then(|v| v.as_u64()));
        if did == Some(u64::from(dungeon_id)) {
            if let Some(room) = val
                .get("roomNumber")
                .or_else(|| val.get("roomIndex"))
                .and_then(|v| v.as_u64())
            {
                max_room = max_room.max(room.min(8) as u8);
            } else {
                max_room = max_room.max(1);
            }
        }
    }
    max_room.max(1)
}

/// Resolves a localized journal template ID and objective hint from V24 `dungeonobjectives.fsdbinary`
/// and `localization_fsd_en-us.pickle` for `dungeon_id`.
pub fn resolve_escalation_journal_template_and_hint(
    sde: &SdeRegistry,
    dungeon_id: u32,
) -> (u32, Option<String>) {
    if let Ok(objectives) = sde.dungeon_objectives() {
        for val in objectives.values() {
            let did = val
                .get("dungeonID")
                .and_then(|v| v.as_u64())
                .or_else(|| val.get("dungeonId").and_then(|v| v.as_u64()));
            if did != Some(u64::from(dungeon_id)) {
                continue;
            }
            for field in ["objectiveTextID", "titleID", "descriptionID", "messageID"] {
                if let Some(msg_id) = val.get(field).and_then(|v| v.as_u64()) {
                    let msg_u32 = msg_id as u32;
                    let text = sde
                        .localized_message_en_us(msg_u32)
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty());
                    return (msg_u32, text);
                }
            }
        }
    }
    (dungeon_id.max(1), None)
}

/// Resolves a multi-step Escalation chain from V24 `sde.dungeons` (`archetype_id == 31`)
/// matching `source_dungeon_id`'s faction and difficulty tier.
pub fn resolve_sde_escalation_chain(
    sde: &SdeRegistry,
    source_dungeon_id: u32,
    origin_sys_id: SolarSystemId,
) -> SdeEscalationChain {
    let faction_id = infer_faction_id(sde, source_dungeon_id, origin_sys_id);
    let src = sde.dungeons.get(&source_dungeon_id);
    let src_diff = src.and_then(|d| d.difficulty).unwrap_or(2);
    let total_steps: u8 = if src_diff <= 2 { 3 } else { 4 };

    let mut by_diff: BTreeMap<u32, Vec<(u32, String)>> = BTreeMap::new();
    for d in sde.dungeons.values() {
        if d.faction_id == Some(faction_id) && is_valid_escalation_template(d) {
            let diff = d.difficulty.unwrap_or(3).max(3);
            by_diff
                .entry(diff)
                .or_default()
                .push((d.dungeon_id, d.name.trim().to_string()));
        }
    }
    for list in by_diff.values_mut() {
        list.sort_by_key(|(id, _)| *id);
    }

    // If `by_diff` is empty (e.g., sub-faction without dedicated arch=31 entries), fallback to any arch=31 dungeon.
    if by_diff.is_empty() {
        for d in sde.dungeons.values().filter(|d| is_valid_escalation_template(d)) {
            let diff = d.difficulty.unwrap_or(3).max(3);
            by_diff
                .entry(diff)
                .or_default()
                .push((d.dungeon_id, d.name.trim().to_string()));
        }
        for list in by_diff.values_mut() {
            list.sort_by_key(|(id, _)| *id);
        }
    }

    if let Some(d) = src.filter(|d| is_valid_escalation_template(d)) {
        let mut chain = vec![d.dungeon_id];
        let mut cur_diff = d.difficulty.unwrap_or(3).max(3);
        let mut prev_id = d.dungeon_id;
        for step in 2..=total_steps {
            let next_diff = if step == total_steps { 5 } else { (cur_diff + 1).min(5) };
            let chosen = by_diff
                .range(next_diff..)
                .flat_map(|(_, list)| list.iter())
                .find(|(id, _)| *id != prev_id)
                .map(|(id, _)| *id)
                .unwrap_or(d.dungeon_id);
            chain.push(chosen);
            prev_id = chosen;
            cur_diff = next_diff;
        }
        let (tpl_id, _) = resolve_escalation_journal_template_and_hint(sde, d.dungeon_id);
        return SdeEscalationChain {
            faction_id,
            step_dungeon_ids: chain,
            expedition_name: d.name.trim().to_string(),
            total_steps,
            journal_template_id: tpl_id,
        };
    }

    let seed = usize::from(source_dungeon_id as u16);
    let mut chain = Vec::with_capacity(usize::from(total_steps));
    let mut primary_name = String::new();
    let start_diff = src_diff.clamp(3, 4);

    for step_idx in 0..total_steps {
        let target_diff = if step_idx == 0 {
            start_diff
        } else if step_idx + 1 == total_steps {
            5
        } else {
            (start_diff + 1).min(5)
        };
        let pool = by_diff
            .range(target_diff..)
            .next()
            .or_else(|| by_diff.iter().next_back())
            .map(|(_, v)| v.as_slice())
            .unwrap_or(&[]);
        if let Some((did, name)) = pool
            .get((seed.wrapping_add(usize::from(step_idx))) % pool.len().max(1))
            .or_else(|| pool.first())
        {
            if step_idx == 0 {
                primary_name = name.clone();
            }
            chain.push(*did);
        }
    }

    if chain.is_empty() {
        let (fallback_id, fallback_name, fallback_steps) =
            crate::services::pve_expeditions::expedition_dungeon_chain(source_dungeon_id);
        let (tpl_id, _) = resolve_escalation_journal_template_and_hint(sde, fallback_id);
        return SdeEscalationChain {
            faction_id,
            step_dungeon_ids: (0..fallback_steps)
                .map(|i| crate::services::pve_expeditions::expedition_dungeon_for_step(fallback_id, i + 1))
                .collect(),
            expedition_name: fallback_name.to_string(),
            total_steps: fallback_steps,
            journal_template_id: tpl_id,
        };
    }

    let first_did = chain[0];
    let (tpl_id, _) = resolve_escalation_journal_template_and_hint(sde, first_did);
    SdeEscalationChain {
        faction_id,
        step_dungeon_ids: chain,
        expedition_name: primary_name,
        total_steps,
        journal_template_id: tpl_id,
    }
}

/// Formats localized journal text for an escalation step using V24 SDE dungeon & objective strings.
pub fn format_escalation_journal_text(
    sde: &SdeRegistry,
    exp_name: &str,
    step_index: u8,
    total_steps: u8,
    dungeon_id: u32,
    dest_sys_id: SolarSystemId,
) -> String {
    let sys_name = sde
        .solar_systems
        .get(&dest_sys_id)
        .map(|s| s.name.clone())
        .unwrap_or_else(|| dest_sys_id.to_string());
    let (_, hint) = resolve_escalation_journal_template_and_hint(sde, dungeon_id);
    if let Some(objective_hint) = hint {
        format!(
            "{objective_hint} — Target coordinates locked for '{exp_name}' (Step {step_index}/{total_steps}) in {sys_name} ({dest_sys_id})."
        )
    } else {
        format!(
            "Navigational logs point to '{exp_name}' (Step {step_index}/{total_steps}) in {sys_name} ({dest_sys_id})."
        )
    }
}
