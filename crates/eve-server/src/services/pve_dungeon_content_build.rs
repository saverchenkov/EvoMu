//! `pve_dungeon_content_build` — Translation and runtime room-building for `pve_dungeon_content`.
//!
//! Separated from schema parsing/lookup (`pve_dungeon_content.rs`) to keep both modules compact
//! while preserving Track 2a trigger, action, gate-link, spawn-tag, and objective semantics.

#![forbid(unsafe_code)]

use super::{
    bind_spawn_object_indices_if_omitted, classify_dungeon_object_spawn,
    get_active_wiki_pve_catalog, has_omitted_object_trigger, inline_spawn_object_trigger_hint,
    is_non_empty_object_or_structure_class, rebind_auto_object_targets,
    select_entry_index_for_dungeon, trigger_target_object_hint, wiki_faction_name_to_id,
    ObjectTriggerHint, WikiBuildStats, WikiDungeonBuild, WikiLootItemSpec, WikiObjective,
    WikiPveEntry, WikiSpawn, WikiTrigger, WikiTriggerActionSpec, WikiWave, WikiWaveDistance,
};
use crate::router::ServerContext;
use crate::services::combat_crimewatch::combat_legacy_ess::{
    is_invulnerable_npc_authority_group, is_non_combat_npc_prop_group,
    is_sleeper_or_drifter_npc_group,
};
use eve_db::service_domains::{
    DungeonGateLink, DungeonNpcGroup, DungeonNpcSpawnEntry, DungeonObjective,
    DungeonObjectiveStatus, DungeonRuntimeObject, DungeonRuntimeRoom, DungeonTemplateId,
    DungeonTrigger, DungeonTriggerAction, DungeonTriggerKind,
};
use std::collections::{BTreeSet, HashMap, HashSet};

pub const MAX_TRIGGER_COMPOUND_DEPTH: usize = 4;

#[derive(Debug, Clone)]
enum TriggerTranslationError {
    UnsupportedKind(String),
    InvalidSpec(String),
}

fn resolve_chance_bps(
    chance_bps: Option<u16>,
    fraction: Option<f64>,
    default_bps: u16,
    field_name: &str,
) -> Result<u16, String> {
    let from_fraction = match fraction {
        Some(p) => {
            if !p.is_finite() || !(0.0..=1.0).contains(&p) {
                return Err(format!(
                    "invalid {field_name} {p}: expected finite probability in 0.0..=1.0"
                ));
            }
            Some(((p * 10_000.0).round() as u32).min(10_000) as u16)
        }
        None => None,
    };
    if let Some(bps) = chance_bps {
        return Ok(bps.min(10_000));
    }
    Ok(from_fraction.unwrap_or(default_bps.min(10_000)))
}

/// Resolves authored escalation trigger/action chance in basis points (`0..=10_000`).
/// Prefers explicit `chance_bps`, otherwise scales finite `0.0..=1.0` `escalation_chance`,
/// defaulting to `10_000` (100%) only when both fields are omitted.
pub(crate) fn resolve_escalation_chance_bps(
    chance_bps: Option<u16>,
    escalation_chance: Option<f64>,
) -> Result<u16, String> {
    resolve_chance_bps(chance_bps, escalation_chance, 10_000, "escalation_chance")
}

/// Resolves optional wave spawn probability in basis points (`0..=10_000`).
pub(crate) fn effective_wave_spawn_chance_bps(wave: &WikiWave) -> Result<u16, String> {
    resolve_chance_bps(
        wave.spawn_chance_bps,
        wave.spawn_chance,
        10_000,
        "spawn_chance",
    )
}

fn validate_user_timer_id<'a>(raw: Option<&'a str>, context: &str) -> Result<&'a str, String> {
    let trimmed = raw.map(str::trim).unwrap_or("");
    if trimmed.is_empty() {
        return Err(format!("{context}: timer_id must be non-empty"));
    }
    if trimmed.starts_with("__") {
        return Err(format!(
            "{context}: reserved '__'-prefixed timer_id '{trimmed}' is forbidden"
        ));
    }
    Ok(trimmed)
}

fn validate_opt_time_seconds(raw: Option<f64>) -> Result<Option<u64>, String> {
    match raw {
        Some(ts) => {
            if !ts.is_finite() || ts < 0.0 {
                return Err(format!("invalid trigger time_seconds {ts}"));
            }
            Ok(Some(ts.round() as u64))
        }
        None => Ok(None),
    }
}

#[derive(Clone, Copy)]
enum GroupLookup<'a> {
    ParseOnly,
    Materialized {
        wave_to_group_id: &'a HashMap<usize, u8>,
        all_room_wave_indices: Option<&'a HashSet<usize>>,
    },
}

fn resolve_target_group_id_for_room(
    spec: &WikiTriggerActionSpec,
    lookup: GroupLookup<'_>,
) -> Result<Option<u8>, String> {
    if let Some(gid) = spec.target_group_id.filter(|&g| g > 0) {
        return Ok(Some(gid));
    }
    if let Some(wi) = spec.target_wave_index {
        return match lookup {
            GroupLookup::Materialized {
                wave_to_group_id,
                all_room_wave_indices,
            } => {
                if let Some(&gid) = wave_to_group_id.get(&wi) {
                    Ok(Some(gid))
                } else if all_room_wave_indices.is_some_and(|waves| waves.contains(&wi)) {
                    // Valid wave in the room that did not materialize (e.g. spawn_chance_bps miss
                    // or container-only wave): skip this dead action rather than aborting room build.
                    Ok(None)
                } else {
                    Err(format!(
                        "action '{}' references unknown target_wave_index {wi}",
                        spec.kind
                    ))
                }
            }
            GroupLookup::ParseOnly => u8::try_from(wi)
                .ok()
                .filter(|&g| g > 0)
                .map(Some)
                .ok_or_else(|| format!("action '{}' has invalid target_wave_index {wi}", spec.kind)),
        };
    }
    Err(format!(
        "action '{}' requires target_group_id or target_wave_index",
        spec.kind
    ))
}

fn translate_wiki_action_spec_inner(
    spec: &WikiTriggerActionSpec,
    lookup: GroupLookup<'_>,
    fallback_escalation_chance_bps: Option<u16>,
) -> Result<Option<DungeonTriggerAction>, String> {
    let kind_raw = spec.kind.trim();
    if kind_raw.is_empty() {
        return Err("empty trigger action kind".to_string());
    }
    let kind_lc = kind_raw.to_ascii_lowercase();
    match kind_lc.as_str() {
        "unlock_gate" | "unlockgate" => Ok(Some(DungeonTriggerAction::UnlockGate)),
        "lock_gate" | "lockgate" => Ok(Some(DungeonTriggerAction::LockGate)),
        "unlock_entry_gate" | "unlockentrygate" => Ok(Some(DungeonTriggerAction::UnlockEntryGate)),
        "spawn_npc_group" | "spawnnpcgroup" | "spawn_wave" | "spawn_group" => {
            Ok(resolve_target_group_id_for_room(spec, lookup)?.map(DungeonTriggerAction::SpawnNpcGroup))
        }
        "despawn_npc_group" | "despawnnpcgroup" | "despawn_wave" | "despawn_group" => {
            Ok(resolve_target_group_id_for_room(spec, lookup)?.map(DungeonTriggerAction::DespawnNpcGroup))
        }
        "spawn_object" | "spawnobject" => {
            let object_index = spec
                .object_index
                .or(spec.target_object_index)
                .ok_or_else(|| format!("action '{kind_raw}' requires object_index"))?;
            Ok(Some(DungeonTriggerAction::SpawnObject { object_index }))
        }
        "grant_loot" | "grantloot" => {
            let items: Vec<(u32, i32)> = spec
                .items
                .iter()
                .copied()
                .map(WikiLootItemSpec::into_pair)
                .filter(|&(tid, qty)| tid > 0 && qty > 0)
                .collect();
            if items.is_empty() {
                return Err(format!(
                    "action '{kind_raw}' requires at least one valid (type_id, quantity) item"
                ));
            }
            Ok(Some(DungeonTriggerAction::GrantLoot {
                items,
                target_object_index: spec.target_object_index.or(spec.object_index),
            }))
        }
        "grant_reward" | "grantreward" => Ok(Some(DungeonTriggerAction::GrantReward {
            isk: spec.isk.unwrap_or(0),
            lp: spec.lp.unwrap_or(0),
            corp_id: spec.corp_id.unwrap_or(0),
            faction_id: spec.faction_id.unwrap_or(0),
            standing_milli: spec.standing_milli.unwrap_or(0),
        })),
        "complete_dungeon" | "completedungeon" => Ok(Some(DungeonTriggerAction::CompleteDungeon)),
        "fail_dungeon" | "faildungeon" => Ok(Some(DungeonTriggerAction::FailDungeon {
            reason: spec.reason.clone().unwrap_or_default(),
        })),
        "start_timer" | "starttimer" => {
            let timer_id =
                validate_user_timer_id(spec.timer_id.as_deref(), "start_timer action")?.to_string();
            let ts_secs = validate_opt_time_seconds(spec.time_seconds)?;
            let delay_secs = spec.delay_secs.or(ts_secs).unwrap_or(0);
            Ok(Some(DungeonTriggerAction::StartTimer {
                timer_id,
                delay_secs,
            }))
        }
        "change_objective" | "changeobjective" | "set_objective" | "setobjective" => {
            let objective_id = spec.objective_id.unwrap_or(0);
            let objective_key = spec
                .objective_key
                .as_deref()
                .map(str::trim)
                .unwrap_or("")
                .to_string();
            let title = spec
                .title
                .as_deref()
                .map(str::trim)
                .unwrap_or("")
                .to_string();
            if objective_id <= 0 && objective_key.is_empty() && title.is_empty() {
                return Err(format!(
                    "action '{kind_raw}' requires objective_id, objective_key, or title"
                ));
            }
            Ok(Some(DungeonTriggerAction::SetObjective {
                objective_id,
                objective_key,
                title,
                completed: spec.completed.unwrap_or(false),
            }))
        }
        "start_escalation" | "startescalation" => {
            let chance_bps = if spec.chance_bps.is_none() && spec.escalation_chance.is_none() {
                fallback_escalation_chance_bps.unwrap_or(10_000)
            } else {
                resolve_escalation_chance_bps(spec.chance_bps, spec.escalation_chance)?
            };
            Ok(Some(DungeonTriggerAction::StartEscalation {
                target_dungeon_id: spec.target_dungeon_id,
                chance_bps,
                target_title: spec.target_title.clone(),
            }))
        }
        "trigger_message" | "triggermessage" => Ok(Some(DungeonTriggerAction::TriggerMessage {
            message_type: spec.message_type.unwrap_or(0),
            message_id: spec.message_id.unwrap_or(0),
        })),
        _ => Err(format!("unknown trigger action kind '{kind_raw}'")),
    }
}

/// Translates a declarative wiki `WikiTriggerActionSpec` into a runtime `DungeonTriggerAction`.
/// Rejects unknown action kinds and reserved `__`-prefixed or empty `start_timer` IDs.
pub(crate) fn translate_wiki_action_spec(
    spec: &WikiTriggerActionSpec,
    wave_to_group_id: Option<&HashMap<usize, u8>>,
    fallback_escalation_chance_bps: Option<u16>,
) -> Result<DungeonTriggerAction, String> {
    let lookup = match wave_to_group_id {
        Some(map) => GroupLookup::Materialized {
            wave_to_group_id: map,
            all_room_wave_indices: None,
        },
        None => GroupLookup::ParseOnly,
    };
    translate_wiki_action_spec_inner(spec, lookup, fallback_escalation_chance_bps)?
        .ok_or_else(|| format!("action '{}' references an unmaterialized wave", spec.kind))
}

fn resolve_source_group_for_trigger(
    tr: &WikiTrigger,
    wave_to_group_id: &HashMap<usize, u8>,
    default_source_group: Option<u8>,
) -> Option<u8> {
    tr.source_wave_index
        .and_then(|sw| wave_to_group_id.get(&sw).copied())
        .or(default_source_group)
}

fn build_kill_trigger_kind(
    tr: &WikiTrigger,
    wave_to_group_id: &HashMap<usize, u8>,
    default_source_group: Option<u8>,
    allow_tagged_kill: bool,
) -> DungeonTriggerKind {
    let explicit_tag = tr
        .target_tag
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let chosen_tag = explicit_tag.or_else(|| {
        allow_tagged_kill
            .then(|| tr.trigger_ship_tag.as_deref().map(str::trim))
            .flatten()
            .filter(|s| !s.is_empty())
    });
    if let Some(tag) = chosen_tag {
        return DungeonTriggerKind::OnKillTagged {
            target_tag: tag.to_string(),
            required_kills: tr.required_kills.filter(|&k| k > 0),
            type_ids: tr.trigger_type_ids.clone(),
        };
    }
    DungeonTriggerKind::OnKill {
        target_npc_group: resolve_source_group_for_trigger(
            tr,
            wave_to_group_id,
            default_source_group,
        ),
        target_ball_id: None,
    }
}

fn attach_resolved_tag_meta(
    tr: &WikiTrigger,
    kind: &mut DungeonTriggerKind,
    type_ids_by_tag: &HashMap<String, Vec<u32>>,
    counts_by_tag: &HashMap<String, u32>,
) {
    match kind {
        DungeonTriggerKind::OnKillTagged {
            target_tag,
            required_kills,
            type_ids,
        } => {
            let tag_key = target_tag.trim().to_ascii_lowercase();
            if !type_ids.is_empty() {
                if let Some(spawned_tids) = type_ids_by_tag.get(&tag_key) {
                    for &stid in spawned_tids {
                        if stid > 0 && !type_ids.contains(&stid) {
                            type_ids.push(stid);
                        }
                    }
                }
            }
            let is_class_clear = matches!(
                tr.kind
                    .as_deref()
                    .map(|s| s.trim().to_ascii_lowercase())
                    .as_deref(),
                Some("last_of_class_destroyed")
            );
            if is_class_clear && required_kills.is_none() {
                if let Some(&cnt) = counts_by_tag.get(&tag_key) {
                    if cnt > 0 {
                        *required_kills = Some(cnt);
                    }
                }
            }
        }
        DungeonTriggerKind::OnNpcDamaged { target_tag, .. } => {
            if target_tag.as_deref().is_some_and(|t| {
                counts_by_tag
                    .get(&t.trim().to_ascii_lowercase())
                    .copied()
                    .unwrap_or(0)
                    == 0
            }) {
                *target_tag = None;
            }
        }
        DungeonTriggerKind::CompoundAnd { clauses }
        | DungeonTriggerKind::CompoundOr { clauses } => {
            for (clause_tr, clause_kind) in tr.clauses.iter().zip(clauses.iter_mut()) {
                attach_resolved_tag_meta(clause_tr, clause_kind, type_ids_by_tag, counts_by_tag);
            }
        }
        _ => {}
    }
}

fn collect_wave_backref_tags(tr: &WikiTrigger, out: &mut HashMap<(usize, usize), String>) {
    if let (Some(sw), Some(ss), Some(raw_tag)) = (
        tr.source_wave_index,
        tr.source_spawn_index,
        tr.trigger_ship_tag.as_deref(),
    ) {
        let trimmed = raw_tag.trim();
        if sw >= 1 && ss >= 1 && !trimmed.is_empty() {
            out.entry((sw, ss)).or_insert_with(|| trimmed.to_string());
        }
    }
    for clause in &tr.clauses {
        collect_wave_backref_tags(clause, out);
    }
}

fn build_npc_damaged_kind(
    tr: &WikiTrigger,
    wave_to_group_id: &HashMap<usize, u8>,
    default_source_group: Option<u8>,
    kind_lc: &str,
) -> Result<DungeonTriggerKind, TriggerTranslationError> {
    if let Some(pct) = tr.hull_below_pct {
        if pct > 100 {
            return Err(TriggerTranslationError::InvalidSpec(format!(
                "invalid hull_below_pct {pct} (must be 0..=100)"
            )));
        }
    }
    let cond_lc = tr
        .condition
        .as_deref()
        .map(str::trim)
        .map(str::to_ascii_lowercase);
    let on_first_attack = tr
        .on_first_attack
        .unwrap_or_else(|| kind_lc == "on_attack" || cond_lc.as_deref() == Some("on_attack"));
    let hull_or_shield_below_pct = tr
        .hull_below_pct
        .unwrap_or(if on_first_attack { 100 } else { 50 });
    let target_tag = trimmed_opt(tr.target_tag.as_deref().or(tr.trigger_ship_tag.as_deref()));
    Ok(DungeonTriggerKind::OnNpcDamaged {
        target_npc_group: resolve_source_group_for_trigger(
            tr,
            wave_to_group_id,
            default_source_group,
        ),
        target_tag,
        target_ball_id: None,
        hull_or_shield_below_pct,
        on_first_attack,
    })
}

#[allow(clippy::too_many_arguments)]
fn translate_trigger_kind_inner(
    tr: &WikiTrigger,
    wave_distance: Option<WikiWaveDistance>,
    room_pos: [f64; 3],
    objects: &[DungeonRuntimeObject],
    wave_to_group_id: &HashMap<usize, u8>,
    room_index_to_vec_idx: &HashMap<usize, usize>,
    default_source_group: Option<u8>,
    allow_tagged_kill: bool,
    depth: usize,
    collect_unsupported: Option<&mut BTreeSet<String>>,
) -> Result<DungeonTriggerKind, TriggerTranslationError> {
    let ts_secs = validate_opt_time_seconds(tr.time_seconds)
        .map_err(TriggerTranslationError::InvalidSpec)?;
    if let Some(ri) = tr.target_room_index {
        if ri == 0 || !room_index_to_vec_idx.contains_key(&ri) {
            return Err(TriggerTranslationError::InvalidSpec(format!(
                "target_room_index {ri} does not match any authored room"
            )));
        }
    }

    if tr
        .classification_state
        .as_deref()
        .map(str::trim)
        .is_some_and(|s| s.eq_ignore_ascii_case("unclassified"))
    {
        return Err(TriggerTranslationError::UnsupportedKind(
            "unclassified".to_string(),
        ));
    }

    let raw_kind = tr.kind.as_deref().map(str::trim).unwrap_or("");
    let kind_lc = raw_kind.to_ascii_lowercase();
    let is_compound = matches!(
        kind_lc.as_str(),
        "compound_and" | "compoundand" | "compound_or" | "compoundor"
    );
    if !is_compound && !tr.clauses.is_empty() {
        return Err(TriggerTranslationError::InvalidSpec(format!(
            "trigger kind '{}' must not specify compound clauses",
            if raw_kind.is_empty() { "<none>" } else { raw_kind }
        )));
    }

    let cond_lc = tr
        .condition
        .as_deref()
        .map(str::trim)
        .map(str::to_ascii_lowercase);

    if is_compound {
        if depth >= MAX_TRIGGER_COMPOUND_DEPTH {
            return Err(TriggerTranslationError::InvalidSpec(format!(
                "compound trigger nesting exceeds MAX_TRIGGER_COMPOUND_DEPTH ({MAX_TRIGGER_COMPOUND_DEPTH})"
            )));
        }
        if tr.clauses.is_empty() {
            return Err(TriggerTranslationError::InvalidSpec(format!(
                "compound trigger '{raw_kind}' must contain at least one clause"
            )));
        }
        let mut translated = Vec::with_capacity(tr.clauses.len());
        match collect_unsupported {
            Some(bucket) => {
                for clause in &tr.clauses {
                    match translate_trigger_kind_inner(
                        clause,
                        wave_distance,
                        room_pos,
                        objects,
                        wave_to_group_id,
                        room_index_to_vec_idx,
                        default_source_group,
                        allow_tagged_kill,
                        depth + 1,
                        Some(bucket),
                    ) {
                        Ok(k) => translated.push(k),
                        Err(TriggerTranslationError::UnsupportedKind(k)) => {
                            bucket.insert(k);
                        }
                        Err(e @ TriggerTranslationError::InvalidSpec(_)) => return Err(e),
                    }
                }
            }
            None => {
                for clause in &tr.clauses {
                    translated.push(translate_trigger_kind_inner(
                        clause,
                        wave_distance,
                        room_pos,
                        objects,
                        wave_to_group_id,
                        room_index_to_vec_idx,
                        default_source_group,
                        allow_tagged_kill,
                        depth + 1,
                        None,
                    )?);
                }
            }
        }
        return Ok(
            if matches!(kind_lc.as_str(), "compound_and" | "compoundand") {
                DungeonTriggerKind::CompoundAnd {
                    clauses: translated,
                }
            } else {
                DungeonTriggerKind::CompoundOr {
                    clauses: translated,
                }
            },
        );
    }

    match kind_lc.as_str() {
        "" => match cond_lc.as_deref() {
            Some("on_damage" | "on_attack") => {
                build_npc_damaged_kind(tr, wave_to_group_id, default_source_group, &kind_lc)
            }
            Some("on_destroy") => {
                if tr.hull_below_pct.is_some() || tr.on_first_attack == Some(true) {
                    build_npc_damaged_kind(tr, wave_to_group_id, default_source_group, &kind_lc)
                } else {
                    Ok(build_kill_trigger_kind(
                        tr,
                        wave_to_group_id,
                        default_source_group,
                        allow_tagged_kill,
                    ))
                }
            }
            None | Some("") => {
                if tr.hull_below_pct.is_some() || tr.on_first_attack == Some(true) {
                    build_npc_damaged_kind(tr, wave_to_group_id, default_source_group, &kind_lc)
                } else {
                    let has_non_empty =
                        |opt: &Option<String>| opt.as_deref().is_some_and(|s| !s.trim().is_empty());
                    let has_explicit_kill_target = has_non_empty(&tr.trigger_ship_tag)
                        || has_non_empty(&tr.trigger_ship_name)
                        || has_non_empty(&tr.target_tag)
                        || tr.source_wave_index.is_some()
                        || tr.source_spawn_index.is_some()
                        || !tr.trigger_type_ids.is_empty();
                    let has_original_text = tr
                        .original_text
                        .as_deref()
                        .is_some_and(|s| !s.trim().is_empty());
                    if has_explicit_kill_target && !has_original_text {
                        Ok(build_kill_trigger_kind(
                            tr,
                            wave_to_group_id,
                            default_source_group,
                            allow_tagged_kill,
                        ))
                    } else {
                        Err(TriggerTranslationError::UnsupportedKind(
                            "unclassified".to_string(),
                        ))
                    }
                }
            }
            Some(other) => Err(TriggerTranslationError::UnsupportedKind(other.to_string())),
        },
        "last_in_group_destroyed" | "group_destroyed" | "wave_cleared" => {
            if matches!(cond_lc.as_deref(), Some("on_damage" | "on_attack"))
                || tr.hull_below_pct.is_some()
                || tr.on_first_attack == Some(true)
            {
                build_npc_damaged_kind(tr, wave_to_group_id, default_source_group, &kind_lc)
            } else {
                // Group-clear triggers must wait for the entire group rather than collapsing
                // onto a 1-kill OnKillTagged via importer-populated trigger_ship_tag.
                Ok(DungeonTriggerKind::OnKill {
                    target_npc_group: resolve_source_group_for_trigger(
                        tr,
                        wave_to_group_id,
                        default_source_group,
                    ),
                    target_ball_id: None,
                })
            }
        }
        "this_ship_destroyed"
        | "last_of_class_destroyed"
        | "objective_complete"
        | "kill"
        | "on_kill" => {
            if matches!(cond_lc.as_deref(), Some("on_damage" | "on_attack"))
                || tr.hull_below_pct.is_some()
                || tr.on_first_attack == Some(true)
            {
                build_npc_damaged_kind(tr, wave_to_group_id, default_source_group, &kind_lc)
            } else {
                Ok(build_kill_trigger_kind(
                    tr,
                    wave_to_group_id,
                    default_source_group,
                    allow_tagged_kill,
                ))
            }
        }
        "kill_tagged" | "on_kill_tagged" | "onkilltagged" => {
            let tag = tr
                .target_tag
                .as_deref()
                .or(tr.trigger_ship_tag.as_deref())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    TriggerTranslationError::InvalidSpec(format!(
                        "trigger kind '{raw_kind}' requires non-empty target_tag"
                    ))
                })?;
            Ok(DungeonTriggerKind::OnKillTagged {
                target_tag: tag.to_string(),
                required_kills: tr.required_kills.filter(|&k| k > 0),
                type_ids: tr.trigger_type_ids.clone(),
            })
        }
        "kill_count" | "on_kill_count" | "onkillcount" => {
            let required_kills = match tr.required_kills {
                Some(0) => {
                    return Err(TriggerTranslationError::InvalidSpec(
                        "kill_count trigger required_kills must be >= 1".to_string(),
                    ));
                }
                Some(n) => n,
                None => 1,
            };
            Ok(DungeonTriggerKind::OnKillCount {
                target_npc_group: resolve_source_group_for_trigger(
                    tr,
                    wave_to_group_id,
                    default_source_group,
                ),
                required_kills,
            })
        }
        "ship_approached" | "approach" | "on_approach" | "onapproach" => {
            if let Some(r) = tr.radius_m {
                if !r.is_finite() || r <= 0.0 {
                    return Err(TriggerTranslationError::InvalidSpec(format!(
                        "invalid trigger radius_m {r}"
                    )));
                }
            }
            let radius_m = tr
                .radius_m
                .or_else(|| wave_distance.map(WikiWaveDistance::effective_m))
                .unwrap_or(3_500.0)
                .max(2_500.0);
            let target_object_index = tr
                .target_object_index
                .or_else(|| objects.iter().position(|o| !o.spawn_on_trigger));
            let anchor_pos = target_object_index
                .and_then(|oi| objects.get(oi))
                .map(|o| o.position)
                .unwrap_or(room_pos);
            Ok(DungeonTriggerKind::OnApproach {
                target_ball_id: None,
                target_object_index,
                anchor_pos,
                radius_m,
            })
        }
        "timed" | "timer" | "on_timer" | "ontimer" => {
            let timer_id = match tr.timer_id.as_deref() {
                Some(raw) => Some(
                    validate_user_timer_id(Some(raw), "timed trigger")
                        .map_err(TriggerTranslationError::InvalidSpec)?
                        .to_string(),
                ),
                None => None,
            };
            let delay_secs = tr.delay_secs.or(ts_secs).unwrap_or(0);
            let after_group_cleared = tr.after_group_cleared.or_else(|| {
                tr.after_wave_cleared
                    .and_then(|w| wave_to_group_id.get(&w).copied())
            });
            Ok(DungeonTriggerKind::OnTimer {
                delay_secs,
                timer_id,
                after_group_cleared,
            })
        }
        "room_entered" | "on_room_entered" | "onroomentered" => {
            let target_room_index = match tr.target_room_index {
                None => None,
                Some(ri) => {
                    let Some(&vec_idx) = room_index_to_vec_idx.get(&ri) else {
                        return Err(TriggerTranslationError::InvalidSpec(format!(
                            "target_room_index {ri} does not match any authored room"
                        )));
                    };
                    Some(vec_idx)
                }
            };
            Ok(DungeonTriggerKind::OnRoomEntered { target_room_index })
        }
        "npc_damaged" | "on_npc_damaged" | "onnpcdamaged" | "on_damage" | "on_attack" => {
            build_npc_damaged_kind(tr, wave_to_group_id, default_source_group, &kind_lc)
        }
        "loot_taken" | "on_loot" | "onloot" => {
            let target_object_index = tr
                .target_object_index
                .or_else(|| objects.iter().position(|o| o.is_container));
            Ok(DungeonTriggerKind::OnLoot {
                container_ball_id: None,
                target_object_index,
                item_type_id: tr.item_type_id.filter(|&id| id > 0),
            })
        }
        "cargo_delivered" | "on_cargo_delivered" | "oncargodelivered" | "deliver_cargo" => {
            let target_object_index = tr
                .target_object_index
                .or_else(|| objects.iter().position(|o| o.is_container));
            let required_quantity = tr.required_quantity.unwrap_or(1).max(1);
            Ok(DungeonTriggerKind::OnCargoDelivered {
                container_ball_id: None,
                target_object_index,
                item_type_id: tr.item_type_id.filter(|&id| id > 0),
                required_quantity,
            })
        }
        "structure_destroyed"
        | "on_structure_destroyed"
        | "onstructuredestroyed"
        | "destroy_structure"
        | "object_destroyed" => {
            let target_tag = trimmed_opt(tr.target_tag.as_deref());
            let target_type_id = tr.target_type_id.filter(|&id| id > 0);
            let target_object_index = tr.target_object_index.or_else(|| {
                (target_tag.is_none() && target_type_id.is_none())
                    .then(|| objects.iter().position(|o| o.destructible))
                    .flatten()
            });
            Ok(DungeonTriggerKind::OnStructureDestroyed {
                target_ball_id: None,
                target_object_index,
                target_tag,
                target_type_id,
            })
        }
        // Fail closed on unimplemented Track 2c trigger kinds ("can_hacked", "ore_depleted")
        // and any unknown trigger kind.
        _ => Err(TriggerTranslationError::UnsupportedKind(kind_lc)),
    }
}

/// Strictly translates a `WikiTrigger` into a runtime `DungeonTriggerKind`.
/// Returns `Err(...)` for unimplemented or unknown trigger kinds (never degrading to `OnKill`),
/// reserved `__`-prefixed timer IDs, empty compound clauses, or compound nesting depth > 4.
#[allow(clippy::too_many_arguments)]
pub(crate) fn translate_wiki_trigger_kind(
    tr: &WikiTrigger,
    wave_distance: Option<WikiWaveDistance>,
    room_pos: [f64; 3],
    objects: &[DungeonRuntimeObject],
    wave_to_group_id: &HashMap<usize, u8>,
    room_index_to_vec_idx: &HashMap<usize, usize>,
    default_source_group: Option<u8>,
    allow_tagged_kill: bool,
) -> Result<DungeonTriggerKind, String> {
    translate_trigger_kind_inner(
        tr,
        wave_distance,
        room_pos,
        objects,
        wave_to_group_id,
        room_index_to_vec_idx,
        default_source_group,
        allow_tagged_kill,
        0,
        None,
    )
    .map_err(|e| match e {
        TriggerTranslationError::UnsupportedKind(k) => format!("unsupported trigger kind '{k}'"),
        TriggerTranslationError::InvalidSpec(msg) => msg,
    })
}

/// Validates a `WikiTrigger` structurally at JSON parse time while collecting any unsupported
/// or unimplemented trigger kinds (including nested compound clauses) into `unsupported_kinds`.
pub(super) fn validate_trigger_and_collect_unsupported(
    tr: &WikiTrigger,
    context: &str,
    room_index_to_vec_idx: &HashMap<usize, usize>,
    allow_tagged_kill: bool,
    unsupported_kinds: &mut BTreeSet<String>,
) -> Result<(), String> {
    resolve_escalation_chance_bps(tr.chance_bps, tr.escalation_chance)
        .map_err(|e| format!("{context}: {e}"))?;
    if let Some(ref action) = tr.action {
        translate_wiki_action_spec(action, None, None)
            .map_err(|e| format!("{context}: {e}"))?;
    }
    for extra in &tr.extra_actions {
        translate_wiki_action_spec(extra, None, None).map_err(|e| format!("{context}: {e}"))?;
    }

    let empty_waves = HashMap::new();
    match translate_trigger_kind_inner(
        tr,
        None,
        [0.0; 3],
        &[],
        &empty_waves,
        room_index_to_vec_idx,
        None,
        allow_tagged_kill,
        0,
        Some(unsupported_kinds),
    ) {
        Ok(_) => Ok(()),
        Err(TriggerTranslationError::UnsupportedKind(k)) => {
            unsupported_kinds.insert(k);
            Ok(())
        }
        Err(TriggerTranslationError::InvalidSpec(msg)) => Err(format!("{context}: {msg}")),
    }
}

fn translate_objective_item(
    wobj: &WikiObjective,
    idx: usize,
    entry: &WikiPveEntry,
    dungeon_id: DungeonTemplateId,
) -> Option<DungeonObjective> {
    let raw_key = wobj.key.as_deref().map(str::trim).unwrap_or("");
    let raw_title = wobj.title.as_deref().map(str::trim).unwrap_or("");
    let raw_orig = wobj.original_text.as_deref().map(str::trim).unwrap_or("");
    let raw_target = wobj.target_name.as_deref().map(str::trim).unwrap_or("");
    let raw_kind = wobj
        .kind
        .as_deref()
        .or(entry.objective_kind.as_deref())
        .map(str::trim)
        .unwrap_or("");
    let target_type_id = wobj
        .target_type_id
        .or_else(|| wobj.target_type_ids.first().copied())
        .filter(|&t| t > 0);
    if raw_key.is_empty()
        && raw_title.is_empty()
        && raw_orig.is_empty()
        && raw_target.is_empty()
        && raw_kind.is_empty()
        && target_type_id.is_none()
    {
        return None;
    }

    let objective_id = wobj
        .objective_id
        .filter(|&id| id > 0)
        .unwrap_or_else(|| i64::from(dungeon_id.as_u32()) * 100 + (idx as i64) + 1);
    let key = if !raw_key.is_empty() {
        raw_key.to_string()
    } else {
        format!("wiki_obj_{}", idx + 1)
    };
    let title = [
        raw_title,
        raw_orig,
        raw_target,
        raw_kind,
        entry.base_title.trim(),
    ]
    .into_iter()
    .find(|s| !s.is_empty())
    .unwrap_or("Complete Site")
    .to_string();
    let is_unclassified = wobj
        .classification_state
        .as_deref()
        .map(str::trim)
        .is_some_and(|s| s.eq_ignore_ascii_case("unclassified"));
    let kind = if !raw_kind.is_empty() {
        raw_kind.to_string()
    } else if is_unclassified {
        "unclassified".to_string()
    } else {
        "kill_all".to_string()
    };

    Some(DungeonObjective {
        objective_id,
        key,
        title,
        kind,
        room_index: wobj.room_index.filter(|&r| r > 0),
        target_type_id,
        target_count: wobj.target_count.or(wobj.item_quantity).unwrap_or(1).max(1),
        progress_count: 0,
        status: DungeonObjectiveStatus::Active,
    })
}

/// Builds Track 2a `DungeonObjective` records from authored wiki entry objectives.
pub(crate) fn build_wiki_objectives(
    entry: &WikiPveEntry,
    dungeon_id: DungeonTemplateId,
) -> Vec<DungeonObjective> {
    let mut out = Vec::new();
    let source_list: Vec<&WikiObjective> = if !entry.objectives.is_empty() {
        entry.objectives.iter().collect()
    } else {
        entry.objective.iter().collect()
    };
    for (idx, wobj) in source_list.into_iter().enumerate() {
        if let Some(obj) = translate_objective_item(wobj, idx, entry, dungeon_id) {
            out.push(obj);
        }
    }
    if out.is_empty() {
        if let Some(kind) = entry
            .objective_kind
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let title = if entry.base_title.trim().is_empty() {
                kind.to_string()
            } else {
                entry.base_title.trim().to_string()
            };
            out.push(DungeonObjective {
                objective_id: i64::from(dungeon_id.as_u32()) * 100 + 1,
                key: "wiki_obj_1".to_string(),
                title,
                kind: kind.to_string(),
                room_index: None,
                target_type_id: None,
                target_count: 1,
                progress_count: 0,
                status: DungeonObjectiveStatus::Active,
            });
        }
    }
    out
}

fn raw_class_to_standin_ship_class(raw_class: &str) -> Option<u32> {
    let norm = raw_class.trim().to_lowercase();
    match norm.as_str() {
        "frigate"
        | "frigates"
        | "corvette"
        | "shuttle"
        | "rookie"
        | "rookie ship"
        | "interceptor"
        | "assault frigate"
        | "covert ops"
        | "electronic attack frigate"
        | "logistics frigate"
        | "drone"
        | "fighter" => Some(1),
        "destroyer"
        | "destroyers"
        | "tactical destroyer"
        | "command destroyer"
        | "interdictor"
        | "cruiser"
        | "cruisers"
        | "heavy assault cruiser"
        | "heavy interdictor"
        | "recon"
        | "recon ship"
        | "combat recon"
        | "force recon"
        | "logistics cruiser"
        | "strategic cruiser" => Some(2),
        "battlecruiser"
        | "battlecruisers"
        | "command ship"
        | "attack battlecruiser"
        | "battleship"
        | "battleships"
        | "marauder"
        | "black ops"
        | "commander"
        | "boss"
        | "overseer"
        | "dreadnought"
        | "carrier"
        | "supercarrier"
        | "titan"
        | "capital" => Some(3),
        _ => None,
    }
}

pub(crate) fn is_valid_combat_npc_type(sde: &eve_sde::SdeRegistry, tid: u32) -> bool {
    if tid == 0 {
        return false;
    }
    let Some(t) = sde.types.get(&tid) else {
        // Minimal unit-test SDE registries with empty types skip external lookup.
        return sde.types.is_empty();
    };
    t.category_id == 11
        && !is_non_combat_npc_prop_group(t.group_id)
        && !is_invulnerable_npc_authority_group(t.group_id)
}

fn resolve_standin_npc_type(
    ctx: &ServerContext,
    entry: &WikiPveEntry,
    sys_id: u32,
    dungeon_id: DungeonTemplateId,
    raw_class: &str,
) -> Option<u32> {
    let target_ship_class = raw_class_to_standin_ship_class(raw_class)?;
    let desired_faction_id = entry
        .factions
        .iter()
        .find_map(|f| wiki_faction_name_to_id(f))
        .or_else(|| ctx.sde.dungeons.get(&dungeon_id.as_u32()).and_then(|d| d.faction_id))
        .unwrap_or_else(|| {
            match crate::services::pve_exploration_seeding::pirate_faction_for_system(
                &ctx.sde, sys_id,
            ) {
                crate::services::pve_exploration_seeding::PirateFaction::AngelCartel => 500_011,
                crate::services::pve_exploration_seeding::PirateFaction::BloodRaiders => 500_012,
                crate::services::pve_exploration_seeding::PirateFaction::Sansha => 500_019,
                crate::services::pve_exploration_seeding::PirateFaction::Serpentis => 500_020,
                crate::services::pve_exploration_seeding::PirateFaction::Guristas => 500_010,
            }
        });
    let explicit_sleeper_drifter = desired_faction_id == 500_024
        || entry
            .factions
            .iter()
            .any(|f| matches!(f.trim(), "Drifters" | "Sleepers"));

    ctx.sde
        .npc_spawn_groups
        .values()
        .filter(|sg| {
            !sg.npc_type_ids.is_empty()
                && !is_non_combat_npc_prop_group(sg.group_id)
                && !is_invulnerable_npc_authority_group(sg.group_id)
                && (explicit_sleeper_drifter || !is_sleeper_or_drifter_npc_group(sg.group_id))
                && (desired_faction_id == 0 || sg.faction_id == desired_faction_id)
        })
        .min_by_key(|sg| {
            (
                sg.ship_class.abs_diff(target_ship_class),
                sg.group_id,
                sg.spawn_group_id,
            )
        })
        .and_then(|sg| sg.npc_type_ids.first().copied())
}

fn is_zero_spawn_count(min_count: Option<usize>, max_count: Option<usize>) -> bool {
    matches!(
        (min_count, max_count),
        (Some(0), Some(0)) | (Some(0), None) | (None, Some(0))
    )
}

fn pick_spawn_count(
    ctx: &ServerContext,
    min_count: Option<usize>,
    max_count: Option<usize>,
) -> usize {
    if is_zero_spawn_count(min_count, max_count) {
        return 0;
    }
    let (lo, hi) = match (min_count, max_count) {
        (None, None) => return 1,
        (Some(lo), None) => {
            let v = lo.max(1);
            (v, v)
        }
        (None, Some(hi)) => (1, hi.max(1)),
        (Some(lo), Some(hi)) => {
            let l = lo.max(1);
            (l, hi.max(l))
        }
    };
    if lo == hi {
        return lo;
    }
    let span = hi - lo + 1;
    let offset = ctx
        .mechanic_rng
        .try_next_u64()
        .map(|v| (v as usize) % span)
        .unwrap_or(0);
    lo + offset
}

enum ResolvedSpawn {
    CombatNpc {
        type_id: u32,
        count: usize,
        is_standin: bool,
    },
    DungeonObject {
        type_id: u32,
        name: String,
        count: usize,
        is_concrete: bool,
        is_container: bool,
        destructible: bool,
    },
    Skipped,
}

fn spawn_has_valid_combat_npc_type(sde: &eve_sde::SdeRegistry, sp: &WikiSpawn) -> bool {
    !sde.types.is_empty()
        && sp
            .resolution
            .type_ids
            .iter()
            .chain(sp.resolution.candidate_type_ids.iter())
            .copied()
            .any(|tid| is_valid_combat_npc_type(sde, tid))
}

fn resolve_spawn_entry(
    ctx: &ServerContext,
    entry: &WikiPveEntry,
    sys_id: u32,
    dungeon_id: DungeonTemplateId,
    sp: &WikiSpawn,
    trigger_hint: ObjectTriggerHint,
) -> ResolvedSpawn {
    let count = pick_spawn_count(ctx, sp.count.min, sp.count.max);
    if count == 0 {
        return ResolvedSpawn::Skipped;
    }

    let has_valid_combat_npc_type = spawn_has_valid_combat_npc_type(&ctx.sde, sp);
    let force_object_for_npc =
        trigger_hint.requires_destructible || trigger_hint.requires_container;

    if !has_valid_combat_npc_type || force_object_for_npc {
        if let Some(flags) = classify_dungeon_object_spawn(sp, trigger_hint) {
            let concrete_tid = sp
                .resolution
                .type_ids
                .iter()
                .chain(sp.resolution.candidate_type_ids.iter())
                .copied()
                .find(|&tid| tid > 0);
            let type_id = concrete_tid.unwrap_or(flags.default_type_id);
            let name = if sp.raw_name.trim().is_empty() {
                flags.default_name.to_string()
            } else {
                sp.raw_name.trim().to_string()
            };
            return ResolvedSpawn::DungeonObject {
                type_id,
                name,
                count,
                is_concrete: concrete_tid.is_some(),
                is_container: flags.is_container,
                destructible: flags.destructible,
            };
        }
    }

    let concrete_tids: Vec<u32> = sp
        .resolution
        .type_ids
        .iter()
        .copied()
        .filter(|&tid| is_valid_combat_npc_type(&ctx.sde, tid))
        .collect();
    if !concrete_tids.is_empty() {
        let pick_idx = if concrete_tids.len() > 1 {
            ctx.mechanic_rng
                .try_next_u64()
                .map(|v| (v as usize) % concrete_tids.len())
                .unwrap_or(0)
        } else {
            0
        };
        return ResolvedSpawn::CombatNpc {
            type_id: concrete_tids[pick_idx],
            count,
            is_standin: false,
        };
    }

    let candidate_tids: Vec<u32> = sp
        .resolution
        .candidate_type_ids
        .iter()
        .copied()
        .filter(|&tid| is_valid_combat_npc_type(&ctx.sde, tid))
        .collect();
    if !candidate_tids.is_empty() {
        let pick_idx = if candidate_tids.len() > 1 {
            ctx.mechanic_rng
                .try_next_u64()
                .map(|v| (v as usize) % candidate_tids.len())
                .unwrap_or(0)
        } else {
            0
        };
        return ResolvedSpawn::CombatNpc {
            type_id: candidate_tids[pick_idx],
            count,
            is_standin: false,
        };
    }

    if let Some(standin_tid) =
        resolve_standin_npc_type(ctx, entry, sys_id, dungeon_id, &sp.raw_class)
    {
        return ResolvedSpawn::CombatNpc {
            type_id: standin_tid,
            count,
            is_standin: true,
        };
    }

    tracing::debug!(
        entry_id = %entry.entry_id,
        dungeon_id = dungeon_id.as_u32(),
        raw_class = %sp.raw_class,
        raw_name = %sp.raw_name,
        status = %sp.resolution.status,
        "Skipping unresolvable wiki spawn row (no concrete type_id or matching SDE stand-in)"
    );
    ResolvedSpawn::Skipped
}

fn validate_entry_triggers_for_build(entry: &WikiPveEntry) -> Result<(), String> {
    let room_index_to_vec_idx: HashMap<usize, usize> = entry
        .rooms
        .iter()
        .enumerate()
        .map(|(pos_idx, r)| (r.room_index, pos_idx))
        .collect();
    let mut unsupported = BTreeSet::new();
    for room in &entry.rooms {
        for wave in &room.waves {
            effective_wave_spawn_chance_bps(wave)?;
            if let Some(ref wtr) = wave.wave_trigger {
                validate_trigger_and_collect_unsupported(
                    wtr,
                    &format!("entry '{}' room {} wave {}", entry.entry_id, room.room_index, wave.wave_index),
                    &room_index_to_vec_idx,
                    false,
                    &mut unsupported,
                )?;
            }
            for (sp_idx, sp) in wave.spawns.iter().enumerate() {
                if let Some(ref strig) = sp.trigger {
                    validate_trigger_and_collect_unsupported(
                        strig,
                        &format!(
                            "entry '{}' room {} wave {} spawn[{sp_idx}]",
                            entry.entry_id, room.room_index, wave.wave_index
                        ),
                        &room_index_to_vec_idx,
                        true,
                        &mut unsupported,
                    )?;
                }
            }
        }
    }
    if !unsupported.is_empty() {
        return Err(format!(
            "entry '{}' references unsupported trigger kind(s): {:?}",
            entry.entry_id, unsupported
        ));
    }
    Ok(())
}

fn collect_explicit_actions(
    tr: &WikiTrigger,
    wave_to_group_id: &HashMap<usize, u8>,
    all_room_wave_indices: &HashSet<usize>,
) -> Result<Vec<DungeonTriggerAction>, String> {
    let trig_esc_chance = resolve_escalation_chance_bps(tr.chance_bps, tr.escalation_chance)?;
    let lookup = GroupLookup::Materialized {
        wave_to_group_id,
        all_room_wave_indices: Some(all_room_wave_indices),
    };
    let mut actions = Vec::new();
    if let Some(ref primary) = tr.action {
        if let Some(act) = translate_wiki_action_spec_inner(primary, lookup, Some(trig_esc_chance))? {
            actions.push(act);
        }
    }
    for extra in &tr.extra_actions {
        if let Some(act) = translate_wiki_action_spec_inner(extra, lookup, Some(trig_esc_chance))? {
            actions.push(act);
        }
    }
    Ok(actions)
}

fn is_non_kill_or_compound_wave_trigger(tr: &WikiTrigger) -> bool {
    let kind_lc = tr
        .kind
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .to_ascii_lowercase();
    matches!(
        kind_lc.as_str(),
        "ship_approached"
            | "approach"
            | "on_approach"
            | "onapproach"
            | "timed"
            | "timer"
            | "on_timer"
            | "ontimer"
            | "room_entered"
            | "on_room_entered"
            | "onroomentered"
            | "loot_taken"
            | "on_loot"
            | "onloot"
            | "cargo_delivered"
            | "on_cargo_delivered"
            | "oncargodelivered"
            | "deliver_cargo"
            | "structure_destroyed"
            | "on_structure_destroyed"
            | "onstructuredestroyed"
            | "destroy_structure"
            | "object_destroyed"
            | "compound_and"
            | "compoundand"
            | "compound_or"
            | "compoundor"
    )
}

fn resolve_action_target_room_index(
    tr: &WikiTrigger,
    trig_kind: &DungeonTriggerKind,
    room_index_to_vec_idx: &HashMap<usize, usize>,
) -> Result<Option<usize>, String> {
    if matches!(trig_kind, DungeonTriggerKind::OnRoomEntered { .. }) {
        Ok(None)
    } else if let Some(ri) = tr.target_room_index {
        room_index_to_vec_idx
            .get(&ri)
            .copied()
            .map(Some)
            .ok_or_else(|| format!("target_room_index {ri} does not match any authored room"))
    } else {
        Ok(None)
    }
}

fn trigger_delay_wrapper_secs(tr: &WikiTrigger, kind: &DungeonTriggerKind) -> u64 {
    if let Some(d) = tr.trigger_delay_secs {
        return d;
    }
    if !matches!(kind, DungeonTriggerKind::OnTimer { .. }) {
        return tr.delay_secs.unwrap_or(0);
    }
    0
}

fn append_target_wave_spawn_actions(
    tr: &WikiTrigger,
    wave_to_group_id: &HashMap<usize, u8>,
    actions: &mut Vec<DungeonTriggerAction>,
) {
    let mut target_waves: Vec<usize> = Vec::new();
    if let Some(tw) = tr.target_wave_index {
        target_waves.push(tw);
    }
    for &tw in &tr.target_wave_indices {
        if !target_waves.contains(&tw) {
            target_waves.push(tw);
        }
    }
    for tw in target_waves {
        if let Some(&target_gid) = wave_to_group_id.get(&tw) {
            let spawn_act = DungeonTriggerAction::SpawnNpcGroup(target_gid);
            if !actions.contains(&spawn_act) {
                actions.push(spawn_act);
            }
        }
    }
}

fn append_escalation_action_if_missing(
    tr: &WikiTrigger,
    actions: &mut Vec<DungeonTriggerAction>,
) -> Result<(), String> {
    if tr.is_escalation == Some(true)
        && !actions
            .iter()
            .any(|a| matches!(a, DungeonTriggerAction::StartEscalation { .. }))
    {
        let chance_bps = resolve_escalation_chance_bps(tr.chance_bps, tr.escalation_chance)?;
        actions.push(DungeonTriggerAction::StartEscalation {
            target_dungeon_id: tr.escalation_dungeon_id,
            chance_bps,
            target_title: tr.escalation_title.clone(),
        });
    }
    Ok(())
}

fn build_gate_links_for_room(
    entry: &WikiPveEntry,
    pos_idx: usize,
    room_positions: &[[f64; 3]],
    room_index_to_vec_idx: &HashMap<usize, usize>,
    dungeon_name: &str,
    default_locked: bool,
) -> Vec<DungeonGateLink> {
    let wroom = &entry.rooms[pos_idx];
    let room_pos = room_positions[pos_idx];
    let total_rooms = entry.rooms.len();
    let mut gate_links = Vec::new();

    if !wroom.gates.is_empty() {
        for gate in &wroom.gates {
            let Some(&target_vec_idx) = room_index_to_vec_idx.get(&gate.target_room_index) else {
                tracing::warn!(
                    entry_id = %entry.entry_id,
                    room_index = wroom.room_index,
                    target_room_index = gate.target_room_index,
                    "Ignoring out-of-bounds wiki room gate link"
                );
                continue;
            };
            if target_vec_idx == pos_idx {
                tracing::warn!(
                    entry_id = %entry.entry_id,
                    room_index = wroom.room_index,
                    "Ignoring self-looping wiki room gate link"
                );
                continue;
            }
            let target_pos = room_positions[target_vec_idx];
            let dest_pos = gate.destination_pos.unwrap_or([
                target_pos[0] - 10_000.0,
                target_pos[1],
                target_pos[2],
            ]);
            let gate_pos = gate.gate_position.unwrap_or([
                room_pos[0] + 5_000.0 + (gate_links.len() as f64) * 1_500.0,
                room_pos[1],
                room_pos[2],
            ]);
            let label = if gate.label.trim().is_empty() {
                let target_wroom = &entry.rooms[target_vec_idx];
                if target_wroom.room_name.trim().is_empty() {
                    format!("To {dungeon_name} - Room {}", target_wroom.room_index)
                } else {
                    format!("To {}", target_wroom.room_name.trim())
                }
            } else {
                gate.label.trim().to_string()
            };
            gate_links.push(DungeonGateLink {
                gate_ball_id: None,
                gate_position: gate_pos,
                target_room_index: target_vec_idx,
                destination_pos: dest_pos,
                locked: gate.locked.unwrap_or(default_locked),
                label,
                gate_type_id: gate.gate_type_id,
                spawned: false,
            });
        }
        return gate_links;
    }

    if pos_idx + 1 < total_rooms {
        let next_pos = room_positions[pos_idx + 1];
        gate_links.push(DungeonGateLink {
            gate_ball_id: None,
            gate_position: [room_pos[0] + 5_000.0, room_pos[1], room_pos[2]],
            target_room_index: pos_idx + 1,
            destination_pos: [next_pos[0] - 10_000.0, next_pos[1], next_pos[2]],
            locked: default_locked,
            label: format!(
                "Acceleration Gate to Room {}",
                entry.rooms[pos_idx + 1].room_index
            ),
            gate_type_id: None,
            spawned: false,
        });
    }

    gate_links
}

fn trimmed_opt(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn room_has_targeted_action(
    scratches: &[BuiltRoomScratch],
    target_idx: usize,
    pred: impl Fn(&DungeonTriggerAction) -> bool,
) -> bool {
    scratches.iter().enumerate().any(|(src_idx, sc)| {
        sc.triggers.iter().any(|t| {
            t.target_room_index.unwrap_or(src_idx) == target_idx
                && t.all_actions().any(&pred)
        })
    })
}

struct BuiltRoomScratch {
    room_index: usize,
    room_id: i64,
    room_name: String,
    room_pos: [f64; 3],
    disallow_mwd: Option<bool>,
    objects: Vec<DungeonRuntimeObject>,
    npc_groups: Vec<DungeonNpcGroup>,
    triggers: Vec<DungeonTrigger>,
    wave_to_group_id: HashMap<usize, u8>,
    all_room_wave_indices: HashSet<usize>,
    spawn_tag_type_ids: HashMap<String, Vec<u32>>,
    spawn_tag_counts: HashMap<String, u32>,
    materialized_spawns: HashSet<(usize, usize)>,
    spawn_object_indices: HashMap<(usize, usize), Vec<usize>>,
    group_unlock_flags: HashMap<u8, bool>,
    spawn_immediate_by_wave: HashMap<usize, bool>,
    active_waves: HashSet<usize>,
}

#[allow(clippy::too_many_arguments)]
fn push_translated_wiki_trigger(
    tr: &WikiTrigger,
    mut trig_kind: DungeonTriggerKind,
    src_idx: usize,
    prepend_spawn_group: Option<u8>,
    inline_unlock: bool,
    record_auto_obj: bool,
    room_scratches: &[BuiltRoomScratch],
    room_index_to_vec_idx: &HashMap<usize, usize>,
    triggers: &mut Vec<DungeonTrigger>,
    auto_obj_ref_trigs: &mut Vec<(usize, usize, WikiTrigger)>,
) -> Option<()> {
    let src = &room_scratches[src_idx];
    attach_resolved_tag_meta(tr, &mut trig_kind, &src.spawn_tag_type_ids, &src.spawn_tag_counts);
    let target_room_index =
        resolve_action_target_room_index(tr, &trig_kind, room_index_to_vec_idx).ok()?;
    let eff_target_idx = target_room_index.unwrap_or(src_idx);
    let target_scratch = &room_scratches[eff_target_idx];
    let mut actions = collect_explicit_actions(
        tr,
        &target_scratch.wave_to_group_id,
        &target_scratch.all_room_wave_indices,
    )
    .ok()?;
    if eff_target_idx == src_idx {
        if let Some(gid) = prepend_spawn_group {
            if !actions.iter().any(|a| matches!(a, DungeonTriggerAction::SpawnNpcGroup(x) if *x == gid)) {
                actions.insert(0, DungeonTriggerAction::SpawnNpcGroup(gid));
            }
        }
    }
    append_target_wave_spawn_actions(tr, &target_scratch.wave_to_group_id, &mut actions);
    append_escalation_action_if_missing(tr, &mut actions).ok()?;
    if inline_unlock {
        let unlock_act = if eff_target_idx + 1 >= room_scratches.len() {
            DungeonTriggerAction::CompleteDungeon
        } else {
            DungeonTriggerAction::UnlockGate
        };
        if !actions.contains(&unlock_act) {
            actions.push(unlock_act);
        }
    }
    if let Some((first_action, rest)) = actions.split_first() {
        let delay_secs = trigger_delay_wrapper_secs(tr, &trig_kind);
        let trig_idx = triggers.len();
        triggers.push(DungeonTrigger {
            trigger_id: (trig_idx + 1) as u32,
            kind: trig_kind,
            action: first_action.clone(),
            extra_actions: rest.to_vec(),
            fired: false,
            repeatable: tr.repeatable.unwrap_or(false),
            delay_secs,
            target_room_index,
            ..Default::default()
        });
        if record_auto_obj && has_omitted_object_trigger(tr) {
            auto_obj_ref_trigs.push((src_idx, trig_idx, tr.clone()));
        }
    }
    Some(())
}

/// Resolves authored deadspace dungeon rooms from the loaded EVE University Wiki (`CC BY-SA 4.0`)
/// content catalog for `(dungeon_id, dungeon_name)`. Returns `None` when no matching wiki entry
/// exists or when any trigger fails strict validation so callers fall back cleanly.
pub(crate) fn build_wiki_deadspace_rooms(
    ctx: &ServerContext,
    sys_id: u32,
    dungeon_id: DungeonTemplateId,
    dungeon_name: &str,
    base_pos: [f64; 3],
    has_entry_gate: bool,
) -> Option<WikiDungeonBuild> {
    let catalog = get_active_wiki_pve_catalog(ctx)?;
    let entry_idx =
        select_entry_index_for_dungeon(&catalog, &ctx.sde, sys_id, dungeon_id, dungeon_name)?;
    let entry = &catalog.entries[entry_idx];
    if entry.rooms.is_empty() {
        return None;
    }

    if let Err(reason) = validate_entry_triggers_for_build(entry) {
        tracing::warn!(
            entry_id = %entry.entry_id,
            dungeon_id = dungeon_id.as_u32(),
            error = %reason,
            "Skipping wiki PvE entry with unimplemented/invalid trigger configuration"
        );
        return None;
    }

    let total_rooms = entry.rooms.len();
    let mut room_positions: Vec<[f64; 3]> = Vec::with_capacity(total_rooms);
    let mut room_index_to_vec_idx: HashMap<usize, usize> = HashMap::with_capacity(total_rooms);
    for (pos_idx, wroom) in entry.rooms.iter().enumerate() {
        let room_step = pos_idx + usize::from(has_entry_gate);
        let room_pos = if room_step == 0 {
            base_pos
        } else {
            [
                base_pos[0] + (room_step as f64) * 600_000.0,
                base_pos[1],
                base_pos[2],
            ]
        };
        room_positions.push(room_pos);
        room_index_to_vec_idx.insert(wroom.room_index, pos_idx);
    }

    let mut stats = WikiBuildStats::default();
    let mut room_scratches: Vec<BuiltRoomScratch> = Vec::with_capacity(total_rooms);

    // Pass 1: Materialize objects and NPC groups across all rooms so cross-room and forward
    // wave references resolve deterministically in Pass 2.
    for (pos_idx, wroom) in entry.rooms.iter().enumerate() {
        let room_pos = room_positions[pos_idx];
        let room_id = i64::from(dungeon_id.as_u32()) * 100 + (wroom.room_index as i64);
        let room_name = if wroom.room_name.trim().is_empty() {
            format!("{dungeon_name} - Room {}", wroom.room_index)
        } else {
            wroom.room_name.trim().to_string()
        };
        let disallow_mwd = crate::services::pve_dungeon_editor::get_dungeon_or_room_mwd_override(
            ctx,
            i64::from(dungeon_id.as_u32()),
            Some(room_id),
        );

        let mut objects: Vec<DungeonRuntimeObject> = Vec::new();
        let mut npc_groups: Vec<DungeonNpcGroup> = Vec::new();
        let mut wave_to_group_id: HashMap<usize, u8> = HashMap::new();
        let all_room_wave_indices: HashSet<usize> =
            wroom.waves.iter().map(|w| w.wave_index).collect();
        let mut spawn_tag_type_ids: HashMap<String, Vec<u32>> = HashMap::new();
        let mut spawn_tag_counts: HashMap<String, u32> = HashMap::new();
        let mut materialized_spawns: HashSet<(usize, usize)> = HashSet::new();
        let mut spawn_object_indices: HashMap<(usize, usize), Vec<usize>> = HashMap::new();
        let mut group_unlock_flags: HashMap<u8, bool> = HashMap::new();
        let mut spawn_immediate_by_wave: HashMap<usize, bool> = HashMap::new();
        let mut active_waves: HashSet<usize> = HashSet::new();
        let mut backref_tags: HashMap<(usize, usize), String> = HashMap::new();
        for w in &wroom.waves {
            if let Some(ref wtr) = w.wave_trigger {
                collect_wave_backref_tags(wtr, &mut backref_tags);
            }
        }
        let mut next_group_id = 1u8;

        for (wwave_idx, wwave) in wroom.waves.iter().enumerate() {
            let spawn_chance_bps = effective_wave_spawn_chance_bps(wwave).ok()?;
            if spawn_chance_bps == 0 {
                continue;
            }
            if spawn_chance_bps < 10_000 {
                let rolled = ctx
                    .mechanic_rng
                    .try_next_u64()
                    .map(|v| (v % 10_000) as u16)
                    .unwrap_or(0);
                if rolled >= spawn_chance_bps {
                    continue;
                }
            }
            active_waves.insert(wwave.wave_index);

            let group_id = next_group_id;
            let mut npc_specs: Vec<(u32, usize)> = Vec::new();
            let mut npc_entries: Vec<DungeonNpcSpawnEntry> = Vec::new();

            for (sp_idx, sp) in wwave.spawns.iter().enumerate() {
                let tag = trimmed_opt(sp.tag.as_deref())
                    .or_else(|| trimmed_opt(sp.trigger.as_ref().and_then(|t| t.trigger_ship_tag.as_deref())))
                    .or_else(|| backref_tags.get(&(wwave.wave_index, sp_idx + 1)).cloned());
                let custom_name = trimmed_opt(Some(sp.raw_name.as_str()));
                let spawn_tag_lc = tag.as_deref().map(str::to_ascii_lowercase);
                let spawn_tids: Vec<u32> = sp
                    .resolution
                    .type_ids
                    .iter()
                    .chain(sp.resolution.candidate_type_ids.iter())
                    .copied()
                    .filter(|&tid| tid > 0)
                    .collect();
                let has_valid_combat_npc_type = spawn_has_valid_combat_npc_type(&ctx.sde, sp);
                let is_non_npc_struct_cls = is_non_empty_object_or_structure_class(&sp.raw_class)
                    && !has_valid_combat_npc_type;
                let mut trigger_hint = sp
                    .trigger
                    .as_ref()
                    .map(|strig| {
                        inline_spawn_object_trigger_hint(
                            strig,
                            spawn_tag_lc.as_deref(),
                            &spawn_tids,
                            has_valid_combat_npc_type,
                        )
                    })
                    .unwrap_or_default();
                for (w_idx, w) in wroom.waves.iter().enumerate() {
                    if let Some(ref wtr) = w.wave_trigger {
                        let include_unbound = is_non_npc_struct_cls
                            && (w_idx == wwave_idx
                                || wtr.source_wave_index == Some(wwave.wave_index)
                                || wtr.source_wave_index.is_none());
                        trigger_hint.merge(trigger_target_object_hint(
                            wtr,
                            spawn_tag_lc.as_deref(),
                            &spawn_tids,
                            include_unbound,
                        ));
                    }
                    for osp in &w.spawns {
                        if let Some(ref strig) = osp.trigger {
                            trigger_hint.merge(trigger_target_object_hint(
                                strig,
                                spawn_tag_lc.as_deref(),
                                &spawn_tids,
                                false,
                            ));
                        }
                    }
                }

                match resolve_spawn_entry(ctx, entry, sys_id, dungeon_id, sp, trigger_hint) {
                    ResolvedSpawn::CombatNpc {
                        type_id,
                        count,
                        is_standin,
                    } => {
                        materialized_spawns.insert((wwave.wave_index, sp_idx));
                        if is_standin {
                            stats.standin_rows += 1;
                        } else {
                            stats.concrete_rows += 1;
                        }
                        if let Some(ref t) = tag {
                            let tag_key = t.to_ascii_lowercase();
                            let list = spawn_tag_type_ids.entry(tag_key.clone()).or_default();
                            if !list.contains(&type_id) {
                                list.push(type_id);
                            }
                            let cnt_entry = spawn_tag_counts.entry(tag_key).or_insert(0);
                            *cnt_entry = cnt_entry.saturating_add(u32::try_from(count).unwrap_or(u32::MAX));
                        }
                        if let Some(existing) = npc_specs.iter_mut().find(|(tid, _)| *tid == type_id) {
                            existing.1 += count;
                        } else {
                            npc_specs.push((type_id, count));
                        }
                        // Combat NPCs tracked in `grp.npc_ball_ids` must never inherit wiki
                        // `destructible: "No"` (`is_tethered = true`), which makes them
                        // invulnerable and permanently deadlocks wave/room clear triggers.
                        if let Some(existing) = npc_entries.iter_mut().find(|e| {
                            e.type_id == type_id
                                && e.tag == tag
                                && e.custom_name == custom_name
                                && e.destructible.is_none()
                        }) {
                            existing.count += count;
                        } else {
                            npc_entries.push(DungeonNpcSpawnEntry {
                                type_id,
                                count,
                                tag,
                                custom_name,
                                destructible: None,
                                ..Default::default()
                            });
                        }
                    }
                    ResolvedSpawn::DungeonObject {
                        type_id,
                        name,
                        count,
                        is_concrete,
                        is_container,
                        destructible,
                    } => {
                        materialized_spawns.insert((wwave.wave_index, sp_idx));
                        if is_concrete {
                            stats.concrete_rows += 1;
                        }
                        for _dup_i in 0..count {
                            if is_container {
                                stats.container_objects += 1;
                            }
                            let obj_idx = objects.len();
                            spawn_object_indices
                                .entry((wwave.wave_index, sp_idx))
                                .or_default()
                                .push(obj_idx);
                            let dist = wwave
                                .distance_m
                                .map(WikiWaveDistance::effective_m)
                                .unwrap_or(2_200.0 + (obj_idx as f64) * 500.0);
                            let angle = (obj_idx as f64) * 1.1;
                            objects.push(DungeonRuntimeObject {
                                ball_id: 0,
                                type_id,
                                name: name.clone(),
                                position: [
                                    room_pos[0] + angle.cos() * dist,
                                    room_pos[1],
                                    room_pos[2] + angle.sin() * dist,
                                ],
                                radius: 350.0,
                                is_container,
                                loot_items: Vec::new(),
                                spawned: false,
                                destructible,
                                tag: tag.clone(),
                                ..Default::default()
                            });
                        }
                    }
                    ResolvedSpawn::Skipped => {
                        stats.skipped_rows += 1;
                    }
                }
            }

            if npc_specs.is_empty() {
                continue;
            }

            let has_non_kill_or_compound_trigger = wwave
                .wave_trigger
                .as_ref()
                .is_some_and(is_non_kill_or_compound_wave_trigger);
            let spawn_immediately = matches!(
                wwave.timing.as_str(),
                "initial_spawn" | "simultaneous_initial_group" | "static_structures"
            ) || (npc_groups.is_empty() && !has_non_kill_or_compound_trigger);

            let wtr_is_room_entered = wwave
                .wave_trigger
                .as_ref()
                .and_then(|t| t.kind.as_deref())
                .is_some_and(|k| {
                    matches!(
                        k.trim().to_ascii_lowercase().as_str(),
                        "room_entered" | "on_room_entered" | "onroomentered"
                    )
                });
            let wtr_targets_other_room = !wtr_is_room_entered
                && wwave
                    .wave_trigger
                    .as_ref()
                    .and_then(|t| t.target_room_index)
                    .and_then(|ri| room_index_to_vec_idx.get(&ri).copied())
                    .is_some_and(|ti| ti != pos_idx);
            if !spawn_immediately
                && !wtr_targets_other_room
                && wwave.wave_trigger.as_ref().and_then(|t| t.unlocks_gate) == Some(true)
            {
                group_unlock_flags.insert(group_id, true);
            }

            wave_to_group_id.insert(wwave.wave_index, group_id);
            spawn_immediate_by_wave.insert(wwave.wave_index, spawn_immediately);
            npc_groups.push(DungeonNpcGroup {
                group_id,
                wave: u8::try_from(wwave.wave_index).unwrap_or(u8::MAX),
                npc_specs,
                npc_entries,
                npc_ball_ids: Vec::new(),
                spawn_immediately,
                spawned: false,
                cleared: false,
                ..Default::default()
            });
            next_group_id = next_group_id.saturating_add(1);
        }

        room_scratches.push(BuiltRoomScratch {
            room_index: wroom.room_index,
            room_id,
            room_name,
            room_pos,
            disallow_mwd,
            objects,
            npc_groups,
            triggers: Vec::new(),
            wave_to_group_id,
            all_room_wave_indices,
            spawn_tag_type_ids,
            spawn_tag_counts,
            materialized_spawns,
            spawn_object_indices,
            group_unlock_flags,
            spawn_immediate_by_wave,
            active_waves,
        });
    }

    // Pass 2: Translate wave and spawn triggers across all rooms using precomputed target wave maps.
    let mut auto_obj_ref_trigs: Vec<(usize, usize, WikiTrigger)> = Vec::new();
    for pos_idx in 0..total_rooms {
        let wroom = &entry.rooms[pos_idx];
        let mut triggers = Vec::new();
        for wwave in &wroom.waves {
            let src = &room_scratches[pos_idx];
            if !src.active_waves.contains(&wwave.wave_index) {
                continue;
            }
            let group_id = src.wave_to_group_id.get(&wwave.wave_index).copied();
            let spawn_immediately = src
                .spawn_immediate_by_wave
                .get(&wwave.wave_index)
                .copied()
                .unwrap_or(false);

            let mut wtr_targets_other_room = false;
            if let Some(ref wtr) = wwave.wave_trigger {
                let default_src = if spawn_immediately {
                    group_id
                } else {
                    group_id.and_then(|g| g.checked_sub(1).filter(|&p| p >= 1))
                };
                let pre_kind = translate_wiki_trigger_kind(
                    wtr,
                    wwave.distance_m,
                    src.room_pos,
                    &src.objects,
                    &src.wave_to_group_id,
                    &room_index_to_vec_idx,
                    default_src,
                    false,
                );
                let action_target_preview = pre_kind
                    .as_ref()
                    .ok()
                    .and_then(|k| resolve_action_target_room_index(wtr, k, &room_index_to_vec_idx).ok())
                    .flatten();
                wtr_targets_other_room =
                    action_target_preview.is_some_and(|ti| ti != pos_idx);
                let wtr_inline_unlock = wtr.unlocks_gate == Some(true)
                    && (group_id.is_none() || spawn_immediately || wtr_targets_other_room);
                let has_target_waves =
                    wtr.target_wave_index.is_some() || !wtr.target_wave_indices.is_empty();
                let has_explicit_or_esc = wtr.action.is_some()
                    || !wtr.extra_actions.is_empty()
                    || wtr.is_escalation == Some(true)
                    || wtr_inline_unlock
                    || has_target_waves;
                if (!spawn_immediately && group_id.is_some()) || has_explicit_or_esc {
                    push_translated_wiki_trigger(
                        wtr,
                        pre_kind.ok()?,
                        pos_idx,
                        (!spawn_immediately).then_some(group_id).flatten(),
                        wtr_inline_unlock,
                        true,
                        &room_scratches,
                        &room_index_to_vec_idx,
                        &mut triggers,
                        &mut auto_obj_ref_trigs,
                    )?;
                }
            }

            for (sp_idx, sp) in wwave.spawns.iter().enumerate() {
                if !src.materialized_spawns.contains(&(wwave.wave_index, sp_idx)) {
                    continue;
                }
                let Some(ref strig) = sp.trigger else {
                    continue;
                };
                let mut trig_kind = translate_wiki_trigger_kind(
                    strig,
                    wwave.distance_m,
                    src.room_pos,
                    &src.objects,
                    &src.wave_to_group_id,
                    &room_index_to_vec_idx,
                    group_id,
                    true,
                )
                .ok()?;
                let container_bound_objs = src
                    .spawn_object_indices
                    .get(&(wwave.wave_index, sp_idx));
                if let Some(obj_indices) = container_bound_objs {
                    bind_spawn_object_indices_if_omitted(
                        strig,
                        &mut trig_kind,
                        obj_indices,
                        &src.objects,
                        0,
                    );
                }
                push_translated_wiki_trigger(
                    strig,
                    trig_kind,
                    pos_idx,
                    None,
                    strig.unlocks_gate == Some(true),
                    container_bound_objs.is_none(),
                    &room_scratches,
                    &room_index_to_vec_idx,
                    &mut triggers,
                    &mut auto_obj_ref_trigs,
                )?;
            }

            if let Some(gid) = group_id {
                if !wtr_targets_other_room
                    && src.group_unlock_flags.get(&gid).copied().unwrap_or(false)
                {
                    let action = if pos_idx + 1 >= total_rooms {
                        DungeonTriggerAction::CompleteDungeon
                    } else {
                        DungeonTriggerAction::UnlockGate
                    };
                    triggers.push(DungeonTrigger {
                        trigger_id: (triggers.len() + 1) as u32,
                        kind: DungeonTriggerKind::OnKill {
                            target_npc_group: Some(gid),
                            target_ball_id: None,
                        },
                        action,
                        fired: false,
                        ..Default::default()
                    });
                }
            }
        }
        room_scratches[pos_idx].triggers = triggers;
    }

    // Post-pass across all rooms scoped by effective target room `t.target_room_index.unwrap_or(src_idx)`.
    let mut spawn_on_trigger_targets: Vec<(usize, usize)> = Vec::new();
    for (src_idx, sc) in room_scratches.iter().enumerate() {
        for trig in &sc.triggers {
            let eff_target_idx = trig.target_room_index.unwrap_or(src_idx);
            for act in trig.all_actions() {
                if let DungeonTriggerAction::SpawnObject { object_index } = act {
                    spawn_on_trigger_targets.push((eff_target_idx, *object_index));
                }
            }
        }
    }
    for (eff_target_idx, object_index) in spawn_on_trigger_targets {
        if let Some(target_sc) = room_scratches.get_mut(eff_target_idx) {
            if let Some(obj) = target_sc.objects.get_mut(object_index) {
                obj.spawn_on_trigger = true;
            }
        }
    }
    for (pos_idx, trig_idx, wtr) in &auto_obj_ref_trigs {
        if let Some(sc) = room_scratches.get_mut(*pos_idx) {
            let room_pos = sc.room_pos;
            let (objects, triggers) = (&sc.objects, &mut sc.triggers);
            if let Some(trig) = triggers.get_mut(*trig_idx) {
                rebind_auto_object_targets(wtr, &mut trig.kind, objects, room_pos);
            }
        }
    }

    for pos_idx in 0..total_rooms {
        let mut synthesized = Vec::new();
        for (g_idx, grp) in room_scratches[pos_idx].npc_groups.iter().enumerate() {
            if grp.spawn_immediately
                || room_has_targeted_action(&room_scratches, pos_idx, |a| {
                    matches!(a, DungeonTriggerAction::SpawnNpcGroup(gid) if *gid == grp.group_id)
                })
            {
                continue;
            }
            let prev_group = g_idx
                .checked_sub(1)
                .map(|pi| room_scratches[pos_idx].npc_groups[pi].group_id);
            synthesized.push(DungeonTrigger {
                trigger_id: (room_scratches[pos_idx].triggers.len() + synthesized.len() + 1) as u32,
                kind: DungeonTriggerKind::OnKill { target_npc_group: prev_group, target_ball_id: None },
                action: DungeonTriggerAction::SpawnNpcGroup(grp.group_id),
                ..Default::default()
            });
        }
        room_scratches[pos_idx].triggers.extend(synthesized);
    }

    for pos_idx in 0..total_rooms {
        let is_last_room = pos_idx + 1 >= total_rooms;
        let has_terminal_action = room_has_targeted_action(&room_scratches, pos_idx, |a| {
            matches!(a, DungeonTriggerAction::UnlockGate | DungeonTriggerAction::CompleteDungeon)
        });
        if !room_scratches[pos_idx].npc_groups.is_empty() && !has_terminal_action {
            let next_id = (room_scratches[pos_idx].triggers.len() + 1) as u32;
            room_scratches[pos_idx].triggers.push(DungeonTrigger {
                trigger_id: next_id,
                kind: DungeonTriggerKind::OnKill { target_npc_group: None, target_ball_id: None },
                action: if is_last_room { DungeonTriggerAction::CompleteDungeon } else { DungeonTriggerAction::UnlockGate },
                ..Default::default()
            });
        }
    }

    let unlock_targeted_rooms: Vec<bool> = (0..total_rooms)
        .map(|pos_idx| {
            room_has_targeted_action(&room_scratches, pos_idx, |a| {
                matches!(a, DungeonTriggerAction::UnlockGate)
            })
        })
        .collect();

    let mut rooms = Vec::with_capacity(total_rooms);
    for (pos_idx, mut scratch) in room_scratches.into_iter().enumerate() {
        if scratch.objects.is_empty() {
            scratch.objects.push(DungeonRuntimeObject {
                ball_id: 0,
                type_id: 23,
                name: format!("{} Beacon", scratch.room_name),
                position: [
                    scratch.room_pos[0] + 2_200.0,
                    scratch.room_pos[1],
                    scratch.room_pos[2],
                ],
                radius: 250.0,
                is_container: false,
                loot_items: Vec::new(),
                spawned: false,
                ..Default::default()
            });
        }
        let is_last_room = pos_idx + 1 >= total_rooms;
        let gated = pos_idx > 0 || has_entry_gate;
        let default_gate_locked =
            !is_last_room && (!scratch.npc_groups.is_empty() || unlock_targeted_rooms[pos_idx]);
        let gate_links = build_gate_links_for_room(
            entry,
            pos_idx,
            &room_positions,
            &room_index_to_vec_idx,
            dungeon_name,
            default_gate_locked,
        );
        let gate_locked = if !gate_links.is_empty() {
            gate_links.iter().all(|gl| gl.locked)
        } else {
            default_gate_locked
        };

        rooms.push(DungeonRuntimeRoom {
            room_index: scratch.room_index,
            room_id: scratch.room_id,
            room_name: scratch.room_name,
            position: scratch.room_pos,
            // Always keep Room i's incoming arrival coordinate; outgoing destinations belong strictly on gate_links.
            gate_destination_pos: [
                scratch.room_pos[0] - 10_000.0,
                scratch.room_pos[1],
                scratch.room_pos[2],
            ],
            gated,
            disallow_mwd: scratch.disallow_mwd,
            gate_ball_id: None,
            gate_locked,
            gate_links,
            objects: scratch.objects,
            npc_groups: scratch.npc_groups,
            triggers: scratch.triggers,
            spawned: false,
            cleared: false,
            ..Default::default()
        });
    }

    if rooms.is_empty() {
        return None;
    }

    let objectives = build_wiki_objectives(entry, dungeon_id);

    tracing::info!(
        source = "EVE University Wiki (CC BY-SA 4.0) + SDE stand-in resolver",
        entry_id = %entry.entry_id,
        dungeon_id = dungeon_id.as_u32(),
        rooms = rooms.len(),
        objectives = objectives.len(),
        concrete_rows = stats.concrete_rows,
        standin_rows = stats.standin_rows,
        skipped_rows = stats.skipped_rows,
        container_objects = stats.container_objects,
        "Instantiated authored wiki PvE dungeon rooms"
    );

    Some(WikiDungeonBuild {
        entry_id: entry.entry_id.clone(),
        source_label: "wiki_pve_v2",
        rooms,
        objectives,
        stats,
    })
}
