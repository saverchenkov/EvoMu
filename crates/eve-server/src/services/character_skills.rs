//! `character_skills` — Domain 2: Skill Queue Engine, Skill Books, & Attributes (`§3.2`, `CH-1`, `CH-2`).

#![forbid(unsafe_code)]

use crate::formulas::{skill_training_time_seconds, sp_for_skill_level};
use crate::router::ServerContext;
use eve_db::{SkillHistoryEntry, SkillQueueEntry, TrainedSkillRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{attr_ids, TypeId};
use std::collections::HashMap;

#[path = "character_clones_implants.rs"]
pub mod character_clones_implants;
#[path = "character_contacts.rs"]
pub mod character_contacts;
#[path = "character_gm_tools.rs"]
pub mod character_gm_tools;

pub use character_clones_implants::*;
pub use character_contacts::*;
pub use character_gm_tools::*;

pub mod character_contacts_gm {
    pub use super::character_contacts::*;
    pub use super::character_gm_tools::*;
}

pub use super::jump_clone_names::{
    alpha_skill_cap_for_character, effective_character_attributes, lookup_effective_attr_from_tuple,
};
pub(crate) use super::jump_clone_names::{
    is_valid_doll_or_portrait_payload, serialize_doll_pyrep, skill_queue_rowset,
};

/// Offline / login skill queue catch-up (`feat:skills.skill-catch-up`):
/// computes elapsed time since `started_at_unix` on the head of `char_id`'s queue,
/// completes any finished skill levels in order, and advances partial SP on the current training skill.
pub fn catch_up_skill_queue(ctx: &ServerContext, char_id: i64) {
    let now_unix = ctx.now_unix();
    let now_ft = ctx.now_blueos_filetime_ticks();
    let sde = ctx.sde.clone();

    let (ship_to_invalidate, completed_events, paused_by_lapse) =
        ctx.db.with_mut_snapshot(|snap| {
            let Some(ch) = snap.characters.get(&char_id).cloned() else {
                return (None, Vec::new(), false);
            };
            let exp_opt =
                crate::services::account_services::subscription_expiry_for_user(snap, ch.user_id);
            let omega_lapsed_at = exp_opt.filter(|&until| until <= now_unix && ch.clone_grade >= 2);

            if snap.paused_skill_queues.contains(&char_id) {
                if omega_lapsed_at.is_some() {
                    for c in snap
                        .characters
                        .values_mut()
                        .filter(|c| c.user_id == ch.user_id)
                    {
                        if c.clone_grade != 1 {
                            c.clone_grade = 1;
                            c.version += 1;
                        }
                        snap.paused_skill_queues.insert(c.character_id);
                    }
                }
                return (None, Vec::new(), false);
            }
            let Some(started_unix) = snap
                .skill_queues
                .get(&char_id)
                .and_then(|q| q.first())
                .and_then(|q| q.started_at_unix)
            else {
                if omega_lapsed_at.is_some() {
                    for c in snap
                        .characters
                        .values_mut()
                        .filter(|c| c.user_id == ch.user_id)
                    {
                        if c.clone_grade != 1 {
                            c.clone_grade = 1;
                            c.version += 1;
                        }
                        snap.paused_skill_queues.insert(c.character_id);
                    }
                }
                return (None, Vec::new(), omega_lapsed_at.is_some());
            };
            let effective_end_unix = omega_lapsed_at.unwrap_or(now_unix);
            if effective_end_unix <= started_unix && omega_lapsed_at.is_none() {
                return (None, Vec::new(), false);
            }
            let elapsed_secs = effective_end_unix.saturating_sub(started_unix) as f64;
            let is_alpha = omega_lapsed_at.is_none() && ch.clone_grade <= 1;
            let current_char_sp = ch.skill_points + ch.free_skill_points;
            if is_alpha && current_char_sp >= 5_000_000.0 {
                if let Some(q) = snap.skill_queues.get_mut(&char_id) {
                    if let Some(head) = q.first_mut() {
                        head.started_at_unix = Some(now_unix);
                    }
                }
                snap.paused_skill_queues.insert(char_id);
                return (None, Vec::new(), false);
            }

            let eff_attrs = effective_character_attributes(&sde, snap, char_id);
            let queue = snap.skill_queues.get(&char_id).cloned().unwrap_or_default();
            let mut rem_secs = elapsed_secs;
            let mut remaining_queue = Vec::new();
            let mut total_sp_gained = 0.0;
            let mut completed = Vec::new();
            let mut history_to_append = Vec::new();
            let mut hit_alpha_cap = false;
            let skills_map = snap.skills.entry(char_id).or_default();

            for mut q in queue {
                if rem_secs <= 0.0 {
                    remaining_queue.push(q);
                    continue;
                }
                if is_alpha && (current_char_sp + total_sp_gained) >= 5_000_000.0 {
                    rem_secs = 0.0;
                    hit_alpha_cap = true;
                    remaining_queue.push(q);
                    continue;
                }
                if is_alpha {
                    let alpha_cap = alpha_skill_cap_for_character(&sde, ch.race_id, q.type_id);
                    if q.target_level > alpha_cap {
                        rem_secs = 0.0;
                        hit_alpha_cap = true;
                        remaining_queue.push(q);
                        continue;
                    }
                }

                let skill_type = sde.types.get(&q.type_id);
                let p_attr_id = skill_type
                    .and_then(|t| t.default_attributes.get(&attr_ids::PRIMARY_ATTRIBUTE))
                    .copied()
                    .map(|v| v as u32)
                    .unwrap_or(attr_ids::PERCEPTION as u32);
                let s_attr_id = skill_type
                    .and_then(|t| t.default_attributes.get(&attr_ids::SECONDARY_ATTRIBUTE))
                    .copied()
                    .map(|v| v as u32)
                    .unwrap_or(attr_ids::WILLPOWER as u32);
                let p_val = lookup_effective_attr_from_tuple(eff_attrs, p_attr_id);
                let s_val = lookup_effective_attr_from_tuple(eff_attrs, s_attr_id);
                let clone_mult = if !is_alpha { 1.0 } else { 0.5 };
                let sp_min = crate::formulas::sp_per_minute(p_val, s_val, clone_mult).max(1e-6);
                let sp_sec = sp_min / 60.0;

                let needed_sp = (q.end_sp - q.start_sp).max(0.0);
                let time_for_needed = (needed_sp / sp_sec).max(0.0);
                let max_allowed_sp = if is_alpha {
                    (5_000_000.0 - (current_char_sp + total_sp_gained)).max(0.0)
                } else {
                    f64::INFINITY
                };

                if rem_secs >= time_for_needed && needed_sp <= max_allowed_sp {
                    rem_secs = (rem_secs - time_for_needed).max(0.0);
                    total_sp_gained += needed_sp;
                    skills_map.insert(
                        q.type_id,
                        TrainedSkillRecord {
                            type_id: q.type_id,
                            level: q.target_level,
                            skill_points: q.end_sp,
                        },
                    );
                    completed.push((q.type_id, q.target_level));
                    history_to_append.push(SkillHistoryEntry {
                        log_date_ft: now_ft,
                        event_type_id: 37,
                        skill_type_id: q.type_id,
                        skill_level: q.target_level,
                        absolute_points: q.end_sp,
                    });
                } else {
                    let mut sp_advance = (rem_secs * sp_sec).min(needed_sp);
                    if sp_advance > max_allowed_sp {
                        sp_advance = max_allowed_sp;
                    }
                    rem_secs = 0.0;
                    q.start_sp += sp_advance;
                    let rem_sp = (q.end_sp - q.start_sp).max(0.0);
                    q.duration_seconds = rem_sp / sp_sec;
                    q.started_at_unix = Some(effective_end_unix);
                    total_sp_gained += sp_advance;

                    let current_lvl = skills_map.get(&q.type_id).map(|s| s.level).unwrap_or(0);
                    let mut achieved_lvl = current_lvl;
                    if let Some(rank) = skill_type.and_then(|t| {
                        t.default_attributes
                            .get(&attr_ids::SKILL_TIME_CONSTANT)
                            .copied()
                    }) {
                        for lvl in (current_lvl + 1)..=q.target_level {
                            if q.start_sp + 0.5 >= sp_for_skill_level(rank, lvl) {
                                achieved_lvl = lvl;
                            } else {
                                break;
                            }
                        }
                    }
                    if achieved_lvl > current_lvl {
                        completed.push((q.type_id, achieved_lvl));
                        history_to_append.push(SkillHistoryEntry {
                            log_date_ft: now_ft,
                            event_type_id: 37,
                            skill_type_id: q.type_id,
                            skill_level: achieved_lvl,
                            absolute_points: q.start_sp,
                        });
                    }
                    skills_map.insert(
                        q.type_id,
                        TrainedSkillRecord {
                            type_id: q.type_id,
                            level: achieved_lvl,
                            skill_points: q.start_sp,
                        },
                    );
                    remaining_queue.push(q);
                }
            }
            if let Some(head) = remaining_queue.first_mut() {
                head.started_at_unix = Some(effective_end_unix);
            }
            snap.skill_queues.insert(char_id, remaining_queue);
            if !history_to_append.is_empty() {
                snap.skill_history
                    .entry(char_id)
                    .or_default()
                    .extend(history_to_append);
            }
            let mut ship_inv = None;
            if total_sp_gained > 0.0 {
                if let Some(ch_mut) = snap.characters.get_mut(&char_id) {
                    ch_mut.skill_points += total_sp_gained;
                    ship_inv = Some(ch_mut.active_ship_id);
                }
            }
            if omega_lapsed_at.is_some() {
                for c in snap
                    .characters
                    .values_mut()
                    .filter(|c| c.user_id == ch.user_id)
                {
                    if c.clone_grade != 1 {
                        c.clone_grade = 1;
                        c.version += 1;
                    }
                    snap.paused_skill_queues.insert(c.character_id);
                }
            } else if hit_alpha_cap {
                snap.paused_skill_queues.insert(char_id);
            }
            (
                ship_inv,
                completed,
                omega_lapsed_at.is_some() || hit_alpha_cap,
            )
        });

    if paused_by_lapse {
        let _ = ctx.notify_skill_queue_paused_server(char_id);
    }
    for (skill_type_id, lvl) in completed_events {
        let _ = ctx.notify_skill_trained(char_id, 0, skill_type_id, lvl);
        super::jump_clone_names::emit_server_skills_changed(
            ctx,
            char_id,
            &[skill_type_id],
            Some("SkillTrained"),
        );
    }
    if let Some(ship_id) = ship_to_invalidate {
        ctx.invalidate_ship_fitting(ship_id);
    }
}

pub fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("skillMgr2", "GetSkills") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            catch_up_skill_queue(ctx, char_id);
            let rows = ctx.db.with_snapshot(|snap| {
                let (clone_grade, race_id) = snap
                    .characters
                    .get(&char_id)
                    .map(|c| (c.clone_grade, c.race_id))
                    .unwrap_or((2, 1));
                snap.skills
                    .get(&char_id)
                    .map(|m| {
                        m.values()
                            .map(|s| {
                                let active_lvl = if clone_grade <= 1 {
                                    s.level.min(alpha_skill_cap_for_character(
                                        &ctx.sde, race_id, s.type_id,
                                    ))
                                } else {
                                    s.level
                                };
                                vec![
                                    PyRep::Integer(s.type_id as i64),
                                    PyRep::Integer(s.level as i64),
                                    PyRep::Real(s.skill_points),
                                    PyRep::Integer(active_lvl as i64),
                                ]
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            });
            Some(Ok(PyRep::rowset(
                vec!["typeID", "skillLevel", "skillPoints", "activeSkillLevel"],
                rows,
            )))
        }

        ("skillMgr2", "GetSkillQueue") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            catch_up_skill_queue(ctx, char_id);
            Some(Ok(skill_queue_rowset(ctx, char_id)))
        }

        ("skillMgr2", "GetSkillQueueAndFreePoints") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            catch_up_skill_queue(ctx, char_id);
            let free_sp = ctx.db.with_snapshot(|snap| {
                snap.characters
                    .get(&char_id)
                    .map(|c| c.free_skill_points)
                    .unwrap_or(0.0)
            });
            Some(Ok(PyRep::Tuple(vec![
                skill_queue_rowset(ctx, char_id),
                PyRep::Real(free_sp),
            ])))
        }

        ("skillMgr2", "GetEndOfTraining") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            catch_up_skill_queue(ctx, char_id);
            let end_ft = ctx.db.with_snapshot(|snap| {
                if snap.paused_skill_queues.contains(&char_id) {
                    return None;
                }
                let q = snap.skill_queues.get(&char_id).filter(|q| !q.is_empty())?;
                let head = q.first()?;
                let base_unix = head.started_at_unix.unwrap_or_else(|| ctx.now_unix());
                let elapsed = ctx.now_unix().saturating_sub(base_unix) as f64;
                let head_rem = (head.duration_seconds - elapsed).max(0.0);
                let tail_secs: f64 = q.iter().skip(1).map(|e| e.duration_seconds.max(0.0)).sum();
                let base_ft = (base_unix as i64 * 10_000_000) + 116_444_736_000_000_000;
                let end_ticks = base_ft + ((elapsed + head_rem + tail_secs) * 10_000_000.0) as i64;
                Some(end_ticks)
            });
            Some(Ok(end_ft.map(PyRep::Integer).unwrap_or(PyRep::None)))
        }

        ("skillMgr2", "SaveSkillQueue") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let is_add_to_end = false;

            let mut skills_to_queue: Vec<(TypeId, u8)> = Vec::new();
            match args.first() {
                Some(PyRep::List(items) | PyRep::Tuple(items)) => {
                    for it in items {
                        match it {
                            PyRep::Tuple(pair) | PyRep::List(pair) => {
                                if let Some(tid) = pair.first().and_then(|a| a.as_i64()) {
                                    skills_to_queue.push((
                                        tid as TypeId,
                                        pair.get(1)
                                            .and_then(|a| a.as_i64())
                                            .unwrap_or(1)
                                            .clamp(1, 5)
                                            as u8,
                                    ));
                                }
                            }
                            single_val => {
                                if let Some(tid) = single_val.as_i64() {
                                    skills_to_queue.push((
                                        tid as TypeId,
                                        items
                                            .get(1)
                                            .and_then(|a| a.as_i64())
                                            .unwrap_or(1)
                                            .clamp(1, 5)
                                            as u8,
                                    ));
                                    break;
                                }
                            }
                        }
                    }
                }
                _ => {
                    if let Some(tid) = args.first().and_then(|a| a.as_i64()) {
                        skills_to_queue.push((
                            tid as TypeId,
                            args.get(1)
                                .and_then(|a| a.as_i64())
                                .unwrap_or(1)
                                .clamp(1, 5) as u8,
                        ));
                    }
                }
            }

            if skills_to_queue.is_empty() {
                if !is_add_to_end {
                    let now_ft = ctx.now_blueos_filetime_ticks();
                    ctx.db.with_mut_snapshot(|snap| {
                        if let Some(old_q) = snap.skill_queues.insert(char_id, Vec::new()) {
                            let hist = snap.skill_history.entry(char_id).or_default();
                            for q in old_q {
                                hist.push(SkillHistoryEntry {
                                    log_date_ft: now_ft,
                                    event_type_id: 38,
                                    skill_type_id: q.type_id,
                                    skill_level: q.target_level,
                                    absolute_points: q.start_sp,
                                });
                            }
                        }
                        snap.paused_skill_queues.insert(char_id);
                    });
                    let _ = ctx.notify_skill_queue_paused_server(char_id);
                    return Some(Ok(PyRep::List(vec![])));
                } else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSkillType".into(),
                        message: "AddToEndOfSkillQueue requires an explicit skill typeID".into(),
                    }));
                }
            }

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(ch) = snap.characters.get(&char_id).cloned() else {
                    return Err(BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {char_id} not found"),
                    });
                };
                let now = ctx.now_unix();
                if ch.user_id > 0 {
                    let other_training = snap.characters.values().filter(|o| {
                        o.user_id == ch.user_id && o.character_id != char_id
                            && snap.skill_queues.get(&o.character_id).is_some_and(|q| q.first().and_then(|e| e.started_at_unix).is_some())
                            && !snap.paused_skill_queues.contains(&o.character_id)
                    }).count();
                    let mct_slots: Vec<u64> = snap.service_state_blobs.get(&format!("mct_slots:{}", ch.user_id))
                        .and_then(|b| serde_json::from_str(b).ok())
                        .or_else(|| crate::services::account_services::load_store(snap).mct_slots.get(&ch.user_id).cloned())
                        .unwrap_or_default();
                    let active_mct_count = mct_slots.into_iter().filter(|&exp| exp > now).count();
                    if other_training + 1 > 1 + active_mct_count {
                        return Err(BridgeRpcError {
                            code: "UserError".into(),
                            message: "CantTrainMultipleCharactersWithoutMCT".into(),
                        });
                    }
                }
                if !is_add_to_end {
                    let now_ft = ctx.now_blueos_filetime_ticks();
                    if let Some(old_q) = snap.skill_queues.insert(char_id, Vec::new()) {
                        let hist = snap.skill_history.entry(char_id).or_default();
                        for q in old_q {
                            hist.push(SkillHistoryEntry {
                                log_date_ft: now_ft,
                                event_type_id: 38,
                                skill_type_id: q.type_id,
                                skill_level: q.target_level,
                                absolute_points: q.start_sp,
                            });
                        }
                    }
                    snap.paused_skill_queues.remove(&char_id);
                }

                let mut last_ret = (0 as TypeId, 1 as u8, 0.0, 0.0);

                for (skill_type_id, target_level) in skills_to_queue {
                    let Some(skill_type) = ctx.sde.types.get(&skill_type_id) else {
                        return Err(BridgeRpcError {
                            code: "InvalidSkillType".into(),
                            message: format!("Skill type {skill_type_id} not found in SDE"),
                        });
                    };
                    if skill_type.category_id != 16 {
                        return Err(BridgeRpcError {
                            code: "InvalidSkillType".into(),
                            message: format!("Type {skill_type_id} is not a skill (category 16)"),
                        });
                    }

                    let rank = skill_type
                        .default_attributes
                        .get(&attr_ids::SKILL_TIME_CONSTANT)
                        .copied()
                        .unwrap_or(1.0);
                    let end_sp = sp_for_skill_level(rank, target_level);

                    // Enforce skill prerequisites (all 6 dogma slots)
                    for (skill_attr, level_attr) in eve_dogma::REQUIRED_SKILL_ATTR_IDS
                        .into_iter()
                        .zip(eve_dogma::REQUIRED_SKILL_LEVEL_ATTR_IDS)
                    {
                        if let Some(&req_skill_f) = skill_type.default_attributes.get(&skill_attr) {
                            let req_skill_id = req_skill_f as TypeId;
                            if req_skill_id > 0 {
                                let req_lvl = skill_type
                                    .default_attributes
                                    .get(&level_attr)
                                    .copied()
                                    .unwrap_or(1.0)
                                    .max(1.0) as u8;
                                let trained_lvl = snap
                                    .skills
                                    .get(&char_id)
                                    .and_then(|m| m.get(&req_skill_id))
                                    .map(|s| s.level)
                                    .unwrap_or(0);
                                let queued_lvl = snap
                                    .skill_queues
                                    .get(&char_id)
                                    .and_then(|q| {
                                        q.iter()
                                            .filter(|e| e.type_id == req_skill_id)
                                            .map(|e| e.target_level)
                                            .max()
                                    })
                                    .unwrap_or(0);
                                if trained_lvl.max(queued_lvl) < req_lvl {
                                    return Err(BridgeRpcError {
                                        code: "SkillPrerequisiteNotMet".into(),
                                        message: format!(
                                            "Skill {} (type {skill_type_id}) requires prerequisite skill {req_skill_id} at level {req_lvl}",
                                            skill_type.name
                                        ),
                                    });
                                }
                            }
                        }
                    }

                    let current_lvl = snap
                        .skills
                        .get(&char_id)
                        .and_then(|m| m.get(&skill_type_id))
                        .map(|s| s.level)
                        .unwrap_or(0);
                    let (max_queued_lvl, max_queued_sp) = snap
                        .skill_queues
                        .get(&char_id)
                        .and_then(|q| {
                            q.iter()
                                .filter(|e| e.type_id == skill_type_id)
                                .max_by_key(|e| e.target_level)
                                .map(|e| (e.target_level, Some(e.end_sp)))
                        })
                        .unwrap_or((0, None));
                    let effective_base_lvl = current_lvl.max(max_queued_lvl);
                    if target_level <= effective_base_lvl {
                        return Err(BridgeRpcError {
                            code: "SkillAlreadyTrained".into(),
                            message: format!(
                                "Skill {} (type {skill_type_id}) is already trained or queued to level {effective_base_lvl}",
                                skill_type.name
                            ),
                        });
                    }

                    let start_sp = max_queued_sp.unwrap_or_else(|| {
                        snap.skills
                            .get(&char_id)
                            .and_then(|m| m.get(&skill_type_id))
                            .map(|s| s.skill_points)
                            .unwrap_or(0.0)
                    });
                    let sp_delta = (end_sp - start_sp).max(0.0);
                    let queued_sp: f64 = snap
                        .skill_queues
                        .get(&char_id)
                        .map(|q| q.iter().map(|e| (e.end_sp - e.start_sp).max(0.0)).sum())
                        .unwrap_or(0.0);

                    // Enforce per-skill Alpha Clone level cap from clonegrades.fsdbinary and 5,000,000 SP total cap (grade 1)
                    if ch.clone_grade <= 1 {
                        let alpha_cap =
                            alpha_skill_cap_for_character(&ctx.sde, ch.race_id, skill_type_id);
                        if target_level > alpha_cap {
                            return Err(BridgeRpcError {
                                code: "AlphaCloneSkillLevelCapExceeded".into(),
                                message: format!(
                                    "Alpha clones cannot train {} (type {skill_type_id}) to level {target_level} (Alpha cap is level {alpha_cap})",
                                    skill_type.name
                                ),
                            });
                        }
                        if ch.skill_points + ch.free_skill_points + queued_sp + sp_delta > 5_000_000.0
                        {
                            return Err(BridgeRpcError {
                                code: "AlphaCloneSpCapExceeded".into(),
                                message:
                                    "Alpha clones cannot queue skills beyond 5,000,000 SP".into(),
                            });
                        }
                    }

                    let primary_attr_id = skill_type.default_attributes.get(&eve_sde::attr_ids::PRIMARY_ATTRIBUTE).copied().map(|v| v as u32).unwrap_or(eve_sde::attr_ids::PERCEPTION as u32);
                    let secondary_attr_id = skill_type.default_attributes.get(&eve_sde::attr_ids::SECONDARY_ATTRIBUTE).copied().map(|v| v as u32).unwrap_or(eve_sde::attr_ids::WILLPOWER as u32);
                    let eff_attrs = effective_character_attributes(&ctx.sde, snap, char_id);
                    let primary_attr = lookup_effective_attr_from_tuple(eff_attrs, primary_attr_id);
                    let secondary_attr = lookup_effective_attr_from_tuple(eff_attrs, secondary_attr_id);
                    let clone_mult = if ch.clone_grade == 2 { 1.0 } else { 0.5 };
                    let duration_sec = skill_training_time_seconds(start_sp, end_sp, primary_attr, secondary_attr, clone_mult);
                    let is_first = snap.skill_queues.get(&char_id).map(|q| q.is_empty()).unwrap_or(true);
                    let started_at = if is_first { Some(now) } else { None };
                    snap.skill_queues.entry(char_id).or_default().push(SkillQueueEntry {
                        type_id: skill_type_id, target_level, start_sp, end_sp, duration_seconds: duration_sec, started_at_unix: started_at,
                    });
                    let now_ft = ctx.now_blueos_filetime_ticks();
                    snap.skill_history.entry(char_id).or_default().push(SkillHistoryEntry {
                        log_date_ft: now_ft, event_type_id: 53, skill_type_id, skill_level: target_level, absolute_points: start_sp,
                    });
                    last_ret = (skill_type_id, target_level, end_sp, duration_sec);
                }
                Ok(last_ret)
            });

            match res {
                Ok((tid, lvl, esp, dur)) => {
                    let _ =
                        ctx.notify_new_skill_queue_saved(char_id, skill_queue_rowset(ctx, char_id));
                    Some(Ok(PyRep::keyval([
                        ("typeID", PyRep::Integer(tid as i64)),
                        ("targetLevel", PyRep::Integer(lvl as i64)),
                        ("endSP", PyRep::Real(esp)),
                        ("durationSeconds", PyRep::Real(dur)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("skillMgr2", "ApplyFreeSkillPoints") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let now_ft = ctx.now_blueos_filetime_ticks();

            // V24 2-argument form: ApplyFreeSkillPoints(skill_type_id, points_to_apply)
            if args.len() >= 2 {
                let Some(skill_type_id) = args
                    .first()
                    .and_then(|a| a.as_i64())
                    .filter(|&id| id > 0)
                    .map(|id| id as TypeId)
                else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSkillType".into(),
                        message: "ApplyFreeSkillPoints requires a positive skillTypeID".into(),
                    }));
                };
                let Some(points_to_apply) = args
                    .get(1)
                    .and_then(|a| a.as_f64())
                    .filter(|&v| v.is_finite() && v > 0.0)
                else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSkillPointAmount".into(),
                        message: "Requested free skill points to apply must be finite and positive"
                            .into(),
                    }));
                };
                let Some(skill_type) = ctx.sde.types.get(&skill_type_id) else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSkillType".into(),
                        message: format!("Skill type {skill_type_id} not found in SDE"),
                    }));
                };
                if skill_type.category_id != 16 {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSkillType".into(),
                        message: format!("Type {skill_type_id} is not a skill (category 16)"),
                    }));
                }
                let rank = skill_type
                    .default_attributes
                    .get(&attr_ids::SKILL_TIME_CONSTANT)
                    .copied()
                    .unwrap_or(1.0);
                let res = ctx.db.try_with_mut_snapshot(|snap| {
                    let Some(ch_snap) = snap.characters.get(&char_id).cloned() else {
                        return Err(BridgeRpcError {
                            code: "CharacterNotFound".into(),
                            message: format!("Character {char_id} not found"),
                        });
                    };
                    if ch_snap.free_skill_points <= 0.0 {
                        return Err(BridgeRpcError {
                            code: "InsufficientFreeSP".into(),
                            message: "No unallocated free skill points available to apply".into(),
                        });
                    }
                    let skills_map = snap.skills.entry(char_id).or_default();
                    let Some(existing_skill) = skills_map.get(&skill_type_id).cloned() else {
                        return Err(BridgeRpcError {
                            code: "SkillNotInjected".into(),
                            message: format!(
                                "Skill {} ({skill_type_id}) must be injected before applying free skill points",
                                skill_type.name
                            ),
                        });
                    };
                    for (skill_attr, level_attr) in eve_dogma::REQUIRED_SKILL_ATTR_IDS
                        .into_iter()
                        .zip(eve_dogma::REQUIRED_SKILL_LEVEL_ATTR_IDS)
                    {
                        if let Some(&req_skill_f) = skill_type.default_attributes.get(&skill_attr) {
                            let req_skill_id = req_skill_f as TypeId;
                            if req_skill_id > 0 {
                                let req_lvl = skill_type
                                    .default_attributes
                                    .get(&level_attr)
                                    .copied()
                                    .unwrap_or(1.0)
                                    .max(1.0) as u8;
                                let trained_lvl = skills_map
                                    .get(&req_skill_id)
                                    .map(|s| s.level)
                                    .unwrap_or(0);
                                if trained_lvl < req_lvl {
                                    return Err(BridgeRpcError {
                                        code: "SkillPrerequisiteNotMet".into(),
                                        message: format!(
                                            "Skill {} (type {skill_type_id}) requires prerequisite skill {req_skill_id} at level {req_lvl}",
                                            skill_type.name
                                        ),
                                    });
                                }
                            }
                        }
                    }
                    let max_lvl = if ch_snap.clone_grade <= 1 {
                        alpha_skill_cap_for_character(&ctx.sde, ch_snap.race_id, skill_type_id)
                    } else {
                        5
                    };
                    let max_skill_sp = sp_for_skill_level(rank, max_lvl);
                    let room_in_skill = (max_skill_sp - existing_skill.skill_points).max(0.0);
                    if room_in_skill <= 0.0 {
                        return Err(BridgeRpcError {
                            code: "SkillAlreadyAtMaxSP".into(),
                            message: format!(
                                "Skill {} ({skill_type_id}) is already at maximum trainable SP ({max_skill_sp})",
                                skill_type.name
                            ),
                        });
                    }
                    let alpha_total_room = if ch_snap.clone_grade <= 1 {
                        let r = (5_000_000.0 - ch_snap.skill_points).max(0.0);
                        if r <= 0.0 {
                            return Err(BridgeRpcError {
                                code: "AlphaCloneSpCapExceeded".into(),
                                message: "Alpha clones cannot apply free skill points beyond 5,000,000 total SP".into(),
                            });
                        }
                        r
                    } else {
                        f64::INFINITY
                    };
                    let applied = points_to_apply
                        .min(ch_snap.free_skill_points)
                        .min(room_in_skill)
                        .min(alpha_total_room);
                    let new_skill_sp = existing_skill.skill_points + applied;
                    let mut achieved_lvl = existing_skill.level;
                    for lvl in (existing_skill.level + 1)..=max_lvl {
                        if new_skill_sp + 0.5 >= sp_for_skill_level(rank, lvl) {
                            achieved_lvl = lvl;
                        } else {
                            break;
                        }
                    }
                    skills_map.insert(
                        skill_type_id,
                        TrainedSkillRecord {
                            type_id: skill_type_id,
                            level: achieved_lvl,
                            skill_points: new_skill_sp,
                        },
                    );
                    if let Some(queue) = snap.skill_queues.get_mut(&char_id) {
                        queue.retain_mut(|q| {
                            if q.type_id != skill_type_id {
                                return true;
                            }
                            if new_skill_sp + 0.5 >= q.end_sp {
                                false
                            } else {
                                if new_skill_sp > q.start_sp {
                                    let old_needed = (q.end_sp - q.start_sp).max(1.0);
                                    q.start_sp = new_skill_sp;
                                    q.duration_seconds *=
                                        ((q.end_sp - q.start_sp).max(0.0)) / old_needed;
                                }
                                true
                            }
                        });
                    }
                    snap.skill_history
                        .entry(char_id)
                        .or_default()
                        .push(SkillHistoryEntry {
                            log_date_ft: now_ft,
                            // 307 = SkillEventSkillPointsApplied (evemu_Crucible EVE_Skills.h:35)
                            event_type_id: 307,
                            skill_type_id,
                            skill_level: achieved_lvl,
                            absolute_points: new_skill_sp,
                        });
                    let remaining_free = if let Some(ch_mut) = snap.characters.get_mut(&char_id) {
                        ch_mut.free_skill_points = (ch_mut.free_skill_points - applied).max(0.0);
                        ch_mut.skill_points += applied;
                        ch_mut.free_skill_points
                    } else {
                        0.0
                    };
                    Ok((new_skill_sp, remaining_free))
                });
                return match res {
                    Ok((new_skill_sp, remaining_free)) => {
                        if let Some(sid) = session.ship_id {
                            ctx.invalidate_ship_fitting(sid);
                        }
                        super::jump_clone_names::emit_server_skills_changed(
                            ctx,
                            char_id,
                            &[skill_type_id],
                            None,
                        );
                        ctx.notify(
                            crate::router::NotificationTarget::Character(char_id),
                            "OnFreeSkillPointsChanged",
                            "charid",
                            vec![PyRep::Real(remaining_free)],
                        );
                        Some(Ok(PyRep::Real(new_skill_sp)))
                    }
                    Err(e) => Some(Err(e)),
                };
            }

            let requested_sp_opt = args.first().and_then(|a| a.as_f64());
            if let Some(req_sp) = requested_sp_opt {
                if !req_sp.is_finite() || req_sp <= 0.0 {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSkillPointAmount".into(),
                        message: "Requested free skill points to apply must be finite and positive"
                            .into(),
                    }));
                }
            }
            let now_ft = ctx.now_blueos_filetime_ticks();
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(ch_snap) = snap.characters.get(&char_id).cloned() else {
                    return Err(BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {char_id} not found"),
                    });
                };
                let available_free_sp = ch_snap.free_skill_points;
                if available_free_sp <= 0.0 {
                    return Err(BridgeRpcError {
                        code: "InsufficientFreeSP".into(),
                        message: "No unallocated free skill points available to apply".into(),
                    });
                }

                let mut remaining_budget = requested_sp_opt
                    .map(|req| available_free_sp.min(req))
                    .unwrap_or(available_free_sp);
                if ch_snap.clone_grade <= 1 {
                    let alpha_room = (5_000_000.0 - ch_snap.skill_points).max(0.0);
                    if alpha_room <= 0.0 {
                        return Err(BridgeRpcError {
                            code: "AlphaCloneSpCapExceeded".into(),
                            message: "Alpha clones cannot apply free skill points beyond 5,000,000 total SP".into(),
                        });
                    }
                    remaining_budget = remaining_budget.min(alpha_room);
                }
                let queue = snap.skill_queues.entry(char_id).or_default().clone();
                let mut remaining_queue = Vec::new();
                let mut history_to_append = Vec::new();
                let skills_map = snap.skills.entry(char_id).or_default();
                let mut total_applied = 0.0;

                for mut q in queue {
                    if ch_snap.clone_grade <= 1
                        && q.target_level > alpha_skill_cap_for_character(&ctx.sde, ch_snap.race_id, q.type_id)
                    {
                        remaining_queue.push(q);
                        continue;
                    }
                    let needed = (q.end_sp - q.start_sp).max(0.0);
                    if remaining_budget >= needed && needed > 0.0 {
                        remaining_budget -= needed;
                        total_applied += needed;
                        skills_map.insert(
                            q.type_id,
                            TrainedSkillRecord {
                                type_id: q.type_id,
                                level: q.target_level,
                                skill_points: q.end_sp,
                            },
                        );
                        history_to_append.push(SkillHistoryEntry {
                            log_date_ft: now_ft,
                            // 307 = SkillEventSkillPointsApplied (evemu_Crucible EVE_Skills.h:35)
                            event_type_id: 307,
                            skill_type_id: q.type_id,
                            skill_level: q.target_level,
                            absolute_points: q.end_sp,
                        });
                    } else if remaining_budget > 0.0 && needed > 0.0 {
                        let applied = remaining_budget;
                        remaining_budget = 0.0;
                        total_applied += applied;
                        q.start_sp += applied;
                        q.duration_seconds *= ((q.end_sp - q.start_sp).max(0.0)) / needed;
                        let current_lvl = skills_map.get(&q.type_id).map(|s| s.level).unwrap_or(0);
                        let rank = ctx
                            .sde
                            .types
                            .get(&q.type_id)
                            .and_then(|t| {
                                t.default_attributes
                                    .get(&attr_ids::SKILL_TIME_CONSTANT)
                                    .copied()
                            })
                            .unwrap_or(1.0);
                        let mut achieved_lvl = current_lvl;
                        for lvl in (current_lvl + 1)..=q.target_level {
                            if q.start_sp + 0.5 >= sp_for_skill_level(rank, lvl) {
                                achieved_lvl = lvl;
                            } else {
                                break;
                            }
                        }
                        skills_map.insert(
                            q.type_id,
                            TrainedSkillRecord {
                                type_id: q.type_id,
                                level: achieved_lvl,
                                skill_points: q.start_sp,
                            },
                        );
                        history_to_append.push(SkillHistoryEntry {
                            log_date_ft: now_ft,
                            // 307 = SkillEventSkillPointsApplied (evemu_Crucible EVE_Skills.h:35)
                            event_type_id: 307,
                            skill_type_id: q.type_id,
                            skill_level: achieved_lvl,
                            absolute_points: q.start_sp,
                        });
                        remaining_queue.push(q);
                    } else {
                        remaining_queue.push(q);
                    }
                }
                snap.skill_queues.insert(char_id, remaining_queue);
                if !history_to_append.is_empty() {
                    snap.skill_history
                        .entry(char_id)
                        .or_default()
                        .extend(history_to_append);
                }

                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.free_skill_points = (ch.free_skill_points - total_applied).max(0.0);
                    ch.skill_points += total_applied;
                    return Ok((ch.skill_points, ch.free_skill_points));
                }
                Ok((0.0, 0.0))
            });
            match res {
                Ok((sp, rem_free)) => {
                    if let Some(sid) = session.ship_id {
                        ctx.invalidate_ship_fitting(sid);
                    }
                    ctx.notify(
                        crate::router::NotificationTarget::Character(char_id),
                        "OnFreeSkillPointsChanged",
                        "charid",
                        vec![PyRep::Real(rem_free)],
                    );
                    Some(Ok(PyRep::Real(sp)))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("skillMgr2", "InjectSkillpoints") => Some(
            crate::services::implant_plugging::inject_skillpoints(ctx, session, args),
        ),

        ("skillMgr2", "AbortTraining") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            ctx.db.with_mut_snapshot(|snap| {
                snap.paused_skill_queues.insert(char_id);
            });
            let _ = ctx.notify_skill_queue_paused_server(char_id);
            Some(Ok(PyRep::Bool(true)))
        }

        ("skillMgr2", "GetSkillHistory") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let rows = ctx.db.with_snapshot(|snap| {
                let mut out: Vec<Vec<PyRep>> = Vec::new();
                if let Some(events) = snap.skill_history.get(&char_id) {
                    for ev in events {
                        out.push(vec![
                            PyRep::Integer(ev.log_date_ft),
                            PyRep::Integer(ev.event_type_id),
                            PyRep::Integer(ev.skill_type_id as i64),
                            PyRep::Integer(ev.skill_level as i64),
                            PyRep::Real(ev.absolute_points),
                        ]);
                    }
                }
                out.sort_by_key(|r| r.first().and_then(|v| v.as_i64()).unwrap_or(0));
                out
            });
            Some(Ok(PyRep::rowset(
                vec![
                    "logDate",
                    "eventTypeID",
                    "skillTypeID",
                    "skillLevel",
                    "absolutePoints",
                ],
                rows,
            )))
        }

        ("skillMgr2", "InjectSkillIntoBrain") | ("dogmaIM", "InjectSkillIntoBrain") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let current_dock = session.station_id.or(session.structure_id);
            let current_ship = session.ship_id;
            let raw_id = args
                .first()
                .and_then(|a| {
                    a.as_i64().or_else(|| match a {
                        PyRep::List(v) | PyRep::Tuple(v) => v.first().and_then(|x| x.as_i64()),
                        _ => None,
                    })
                })
                .unwrap_or(0);
            if raw_id <= 0 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "InjectSkillIntoBrain requires itemID or skill typeID".into(),
                }));
            }
            let now_ft = ctx.now_blueos_filetime_ticks();

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let skill_type_id = if let Some(item) = snap.items.get(&raw_id) {
                    item.type_id
                } else if ctx.sde.types.contains_key(&(raw_id as u32)) {
                    raw_id as u32
                } else if let Some(inv_item) = snap.items.values().find(|i| {
                    i.type_id == raw_id as u32
                        && i.owner_id == char_id
                        && snap.is_item_consumable_by(i, char_id, current_dock, current_ship)
                }) {
                    inv_item.type_id
                } else {
                    return Err(BridgeRpcError {
                        code: "ItemNotFound".into(),
                        message: format!("Skillbook item or skill type {raw_id} not found"),
                    });
                };

                let Some(skill_type) = ctx.sde.types.get(&skill_type_id) else {
                    return Err(BridgeRpcError {
                        code: "SkillNotFound".into(),
                        message: format!("Skill type {skill_type_id} not found in SDE"),
                    });
                };
                if skill_type.category_id != 16 {
                    return Err(BridgeRpcError {
                        code: "InvalidSkillType".into(),
                        message: format!("Type {skill_type_id} is not a skill (category 16)"),
                    });
                }

                if let Some(skills_map) = snap.skills.get(&char_id) {
                    if skills_map.contains_key(&skill_type_id) {
                        return Err(BridgeRpcError {
                            code: "SkillAlreadyInjected".into(),
                            message: format!("Skill {} is already known to character", skill_type.name),
                        });
                    }
                }

                for (skill_attr, level_attr) in eve_dogma::REQUIRED_SKILL_ATTR_IDS
                    .into_iter()
                    .zip(eve_dogma::REQUIRED_SKILL_LEVEL_ATTR_IDS)
                {
                    if let Some(&req_skill_f) = skill_type.default_attributes.get(&skill_attr) {
                        let req_skill_id = req_skill_f as TypeId;
                        if req_skill_id > 0 {
                            let req_lvl = skill_type
                                .default_attributes
                                .get(&level_attr)
                                .copied()
                                .unwrap_or(1.0)
                                .max(1.0) as u8;
                            let trained_lvl = snap
                                .skills
                                .get(&char_id)
                                .and_then(|m| m.get(&req_skill_id))
                                .map(|s| s.level)
                                .unwrap_or(0);
                            if trained_lvl < req_lvl {
                                return Err(BridgeRpcError {
                                    code: "SkillPrerequisiteNotMet".into(),
                                    message: format!(
                                        "Skill {} (type {skill_type_id}) requires prerequisite skill {req_skill_id} at level {req_lvl}",
                                        skill_type.name
                                    ),
                                });
                            }
                        }
                    }
                }

                // R-SKB-01: Injection strictly requires an owned, reachable skillbook item to consume
                let book_item = if let Some(item) = snap.items.get(&raw_id) {
                    if item.owner_id == char_id && snap.is_item_consumable_by(item, char_id, current_dock, current_ship) {
                        Some(item.item_id)
                    } else {
                        None
                    }
                } else {
                    snap.items.values().find(|i| {
                        i.type_id == skill_type_id
                            && i.owner_id == char_id
                            && snap.is_item_consumable_by(i, char_id, current_dock, current_ship)
                    }).map(|i| i.item_id)
                };

                let Some(item_id) = book_item else {
                    return Err(BridgeRpcError {
                        code: "ItemNotFound".into(),
                        message: format!("Skillbook item for skill {} not found in inventory", skill_type.name),
                    });
                };

                if let Some(item) = snap.items.get_mut(&item_id) {
                    if item.quantity > 1 {
                        item.quantity -= 1;
                        item.version += 1;
                    } else {
                        snap.items.remove(&item_id);
                    }
                }

                let skills_map = snap.skills.entry(char_id).or_default();

                skills_map.insert(
                    skill_type_id,
                    TrainedSkillRecord {
                        type_id: skill_type_id,
                        level: 0,
                        skill_points: 0.0,
                    },
                );
                snap.skill_history
                    .entry(char_id)
                    .or_default()
                    .push(SkillHistoryEntry {
                        log_date_ft: now_ft,
                        // 36 = SkillEventSkillInjected (CCP V24 / evemu)
                        event_type_id: 36,
                        skill_type_id,
                        skill_level: 0,
                        absolute_points: 0.0,
                    });

                Ok(skill_type_id)
            });

            match res {
                Ok(tid) => {
                    super::jump_clone_names::emit_server_skills_changed(
                        ctx,
                        char_id,
                        &[tid],
                        Some("SkillInjected"),
                    );
                    Some(Ok(PyRep::keyval([
                        ("typeID", PyRep::Integer(tid as i64)),
                        ("injected", PyRep::Bool(true)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("skillMgr2", "ExtractSkills") => {
            Some(super::jump_clone_names::extract_skills(ctx, session, args))
        }

        ("skillMgr2", "GetRespecInfo") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let res = ctx.db.with_snapshot(|snap| {
                snap.characters.get(&char_id).map(|c| {
                    (
                        c.neural.bonus_remaps,
                        c.neural.last_remap_unix,
                        c.neural.remap_cooldown_until_unix,
                    )
                })
            });
            let Some((bonus_remaps, last_remap, next_timed)) = res else {
                return Some(Err(BridgeRpcError {
                    code: "CharacterNotFound".into(),
                    message: format!("Character {char_id} not found"),
                }));
            };
            Some(Ok(PyRep::keyval([
                ("freeRespecs", PyRep::Integer(bonus_remaps as i64)),
                ("bonusRemaps", PyRep::Integer(bonus_remaps as i64)),
                (
                    "lastRespecDate",
                    last_remap
                        .map(|u| PyRep::Integer(u as i64))
                        .unwrap_or(PyRep::None),
                ),
                (
                    "nextTimedRespec",
                    next_timed
                        .map(|u| PyRep::Integer(u as i64))
                        .unwrap_or(PyRep::None),
                ),
            ])))
        }

        ("skillMgr2", "GetImplants") => Some(crate::services::implant_plugging::get_implants(
            ctx, session,
        )),
        ("skillMgr2", "GetBoosters") => Some(crate::services::implant_plugging::get_boosters(
            ctx, session,
        )),
        ("dogmaIM", "InjectImplant") => Some(crate::services::implant_plugging::inject_implant(
            ctx, session, args,
        )),
        ("dogmaIM", "DestroyImplant") => Some(crate::services::implant_plugging::destroy_implant(
            ctx, session, args,
        )),
        ("dogmaIM", "UseBooster") => Some(crate::services::implant_plugging::use_booster(
            ctx, session, args,
        )),

        _ => crate::services::implant_plugging::handle_skill_notifications_and_injections(
            ctx, session, service, method,
        )
        .or_else(|| {
            character_clones_implants::handle_clones_and_sheet(
                ctx, session, service, method, args, _kwargs,
            )
        }),
    }
}
