//! Protocol-agnostic character skill mutation and queue reconciliation domain services (`#1193`).

#![forbid(unsafe_code)]

use crate::domain::DomainError;
use crate::formulas::sp_for_skill_level;
use crate::router::{NotificationTarget, ServerContext};
use crate::services::character_skills::{
    alpha_skill_cap_for_character, check_skill_type_prerequisites, effective_character_attributes,
    validate_published_skill_type,
};
use crate::services::jump_clone_names::{
    emit_server_skills_changed, emit_server_skills_removed, skill_queue_entry_duration_with_attrs,
    skill_queue_rowset,
};
use eve_db::{SkillHistoryEntry, TrainedSkillRecord, WorldRepository, WorldStateSnapshot};
use eve_sde::{attr_ids, TypeId};
use std::collections::{BTreeSet, HashMap};

/// Snapshot skill update request passed to [`apply_character_skill_update_in_snap`].
#[derive(Clone, Copy, Debug)]
pub struct CharacterSkillUpdate {
    pub skill_type_id: TypeId,
    pub new_level: u8,
    pub new_skill_points: f64,
    pub remove_when_zero: bool,
    pub free_sp_delta: f64,
    pub history_event_type_id: Option<i64>,
    pub now_ft: i64,
    pub now_unix: u64,
    pub allow_rewind_start_sp: bool,
}

/// Result metadata returned by [`apply_character_skill_update_in_snap`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CharacterSkillUpdateOutcome {
    pub active_ship_id: i64,
    pub total_skill_points: f64,
    pub remaining_free_skill_points: f64,
    pub was_removed: bool,
    pub queue_changed: bool,
}

/// Committed result returned by [`set_character_skill_level`].
#[derive(Clone, Debug, PartialEq)]
pub struct SetCharacterSkillResult {
    pub target_char_id: i64,
    pub skill_type_id: TypeId,
    pub skill_name: String,
    pub level: u8,
    pub skill_points: f64,
    pub character_total_sp: f64,
}

/// Committed result returned by [`apply_free_skill_points_to_skill`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ApplyFreeSkillPointsResult {
    pub new_skill_sp: f64,
    pub remaining_free_sp: f64,
}

fn rpc_err_to_domain(err: eve_proto::BridgeRpcError) -> DomainError {
    match err.code.as_str() {
        "InvalidSkillType" | "InvalidSkillPointAmount" => {
            DomainError::invalid_input(err.code, err.message)
        }
        "CharacterNotFound" | "SkillNotFound" => DomainError::not_found(err.code, err.message),
        _ => DomainError::precondition_failed(err.code, err.message),
    }
}

/// Reconciles `snap.skill_queues` for `char_id` after `skill_type_id` is updated to
/// `(new_level, new_skill_points)` or removed from `snap.skills`.
///
/// Drops completed, superseded, uninjected, or prerequisite-broken queue entries, recomputes
/// active head progress/durations, removes the map key when emptied, and returns `true` when the
/// persisted queue changed.
#[allow(clippy::too_many_arguments)]
pub fn reconcile_skill_queue_in_snap(
    ctx: &ServerContext,
    snap: &mut WorldStateSnapshot,
    char_id: i64,
    clone_grade: u32,
    skill_type_id: TypeId,
    new_level: u8,
    new_skill_points: f64,
    now_unix: u64,
    allow_rewind_start_sp: bool,
) -> bool {
    if !snap
        .skill_queues
        .get(&char_id)
        .is_some_and(|q| !q.is_empty())
    {
        if snap.skill_queues.contains_key(&char_id) {
            snap.skill_queues.remove(&char_id);
        }
        return false;
    }

    let train_mult = ctx.skill_training_multiplier();
    let eff_attrs = if (train_mult - 1.0).abs() > 1e-9 {
        Some(effective_character_attributes(&ctx.sde, snap, char_id))
    } else {
        None
    };

    let mut valid_next_lvl: HashMap<TypeId, u8> = snap
        .skills
        .get(&char_id)
        .map(|m| m.iter().map(|(&k, v)| (k, v.level)).collect())
        .unwrap_or_default();
    let paused = snap.paused_skill_queues.contains(&char_id);

    let (changed, remove_empty_queue) = {
        let Some(queue) = snap.skill_queues.get_mut(&char_id) else {
            return false;
        };
        let prev_queue = queue.clone();
        let prev_head = prev_queue.first().map(|e| (e.type_id, e.target_level));
        queue.retain_mut(|q| {
            if q.type_id == skill_type_id && !valid_next_lvl.contains_key(&skill_type_id) {
                return false;
            }
            if let Some(st) = ctx.sde.types.get(&q.type_id) {
                for (req_attr, req_lvl_attr) in eve_dogma::REQUIRED_SKILL_ATTR_IDS
                    .into_iter()
                    .zip(eve_dogma::REQUIRED_SKILL_LEVEL_ATTR_IDS)
                {
                    if let Some(&req_tid_f) = st.default_attributes.get(&req_attr) {
                        let req_tid = req_tid_f as u32;
                        if req_tid > 0 {
                            let req_lvl = st
                                .default_attributes
                                .get(&req_lvl_attr)
                                .copied()
                                .unwrap_or(1.0)
                                .max(1.0) as u8;
                            let have_lvl = valid_next_lvl.get(&req_tid).copied().unwrap_or(0);
                            if have_lvl < req_lvl {
                                return false;
                            }
                        }
                    }
                }
            }
            if q.type_id == skill_type_id {
                let expected_prev = valid_next_lvl.get(&skill_type_id).copied().unwrap_or(0);
                if q.target_level <= new_level
                    || q.target_level != expected_prev + 1
                    || new_skill_points + 0.5 >= q.end_sp
                {
                    return false;
                }
                if q.target_level == new_level + 1 {
                    let new_start_sp = new_skill_points.min(q.end_sp);
                    let should_update_start = if allow_rewind_start_sp {
                        (q.start_sp - new_start_sp).abs() > 1e-6
                    } else {
                        new_start_sp > q.start_sp
                    };
                    if should_update_start {
                        let old_needed = (q.end_sp - q.start_sp).max(1.0);
                        q.start_sp = new_start_sp;
                        q.duration_seconds = if let Some(attrs) = eff_attrs {
                            skill_queue_entry_duration_with_attrs(
                                ctx,
                                q.type_id,
                                q.start_sp,
                                q.end_sp,
                                clone_grade,
                                attrs,
                                q.duration_seconds,
                            )
                        } else {
                            q.duration_seconds * (((q.end_sp - q.start_sp).max(0.0)) / old_needed)
                        };
                        if allow_rewind_start_sp && q.started_at_unix.is_some() {
                            q.started_at_unix = Some(now_unix);
                        }
                    }
                }
            }
            valid_next_lvl.insert(q.type_id, q.target_level);
            true
        });

        let new_head = queue.first().map(|e| (e.type_id, e.target_level));
        if new_head != prev_head {
            if let Some(head) = queue.first_mut() {
                head.started_at_unix = if paused { None } else { Some(now_unix) };
            }
        } else if !paused {
            if let Some(head) = queue.first_mut() {
                head.started_at_unix.get_or_insert(now_unix);
            }
        }
        (*queue != prev_queue, queue.is_empty())
    };
    if remove_empty_queue {
        snap.skill_queues.remove(&char_id);
    }
    changed
}

/// Applies a validated skill mutation inside `snap`: updates `snap.skills`, keeps
/// `CharacterRecord.skill_points` / `free_skill_points` consistent, appends optional
/// `snap.skill_history`, and reconciles `snap.skill_queues`.
pub fn apply_character_skill_update_in_snap(
    ctx: &ServerContext,
    snap: &mut WorldStateSnapshot,
    char_id: i64,
    update: CharacterSkillUpdate,
) -> Result<CharacterSkillUpdateOutcome, DomainError> {
    let (clone_grade, active_ship_id) = snap
        .characters
        .get(&char_id)
        .map(|ch| (ch.clone_grade, ch.active_ship_id))
        .ok_or_else(|| {
            DomainError::not_found("CharacterNotFound", format!("Character {char_id} not found"))
        })?;

    let (prev_sp, was_removed) = {
        let skills_map = snap.skills.entry(char_id).or_default();
        let prev_sp = skills_map
            .get(&update.skill_type_id)
            .map(|s| s.skill_points)
            .unwrap_or(0.0);
        let was_removed = if update.remove_when_zero && update.new_level == 0 {
            skills_map.remove(&update.skill_type_id).is_some()
        } else {
            skills_map.insert(
                update.skill_type_id,
                TrainedSkillRecord {
                    type_id: update.skill_type_id,
                    level: update.new_level,
                    skill_points: update.new_skill_points,
                },
            );
            false
        };
        (prev_sp, was_removed)
    };

    let sp_diff = update.new_skill_points - prev_sp;
    let (total_skill_points, remaining_free_skill_points) = {
        let ch = snap.characters.get_mut(&char_id).ok_or_else(|| {
            DomainError::not_found("CharacterNotFound", format!("Character {char_id} not found"))
        })?;
        if sp_diff != 0.0 {
            ch.skill_points = (ch.skill_points + sp_diff).max(0.0);
        }
        if update.free_sp_delta != 0.0 {
            ch.free_skill_points = (ch.free_skill_points + update.free_sp_delta).max(0.0);
        }
        (ch.skill_points, ch.free_skill_points)
    };

    if let Some(event_type_id) = update.history_event_type_id {
        snap.skill_history
            .entry(char_id)
            .or_default()
            .push(SkillHistoryEntry {
                log_date_ft: update.now_ft,
                event_type_id,
                skill_type_id: update.skill_type_id,
                skill_level: update.new_level,
                absolute_points: update.new_skill_points,
            });
    }

    let queue_changed = reconcile_skill_queue_in_snap(
        ctx,
        snap,
        char_id,
        clone_grade,
        update.skill_type_id,
        update.new_level,
        update.new_skill_points,
        update.now_unix,
        update.allow_rewind_start_sp,
    );

    Ok(CharacterSkillUpdateOutcome {
        active_ship_id,
        total_skill_points,
        remaining_free_skill_points,
        was_removed,
        queue_changed,
    })
}

/// Sets or clears (`level == 0`) a character skill atomically via `WorldRepository::tx` and
/// broadcasts fit/skill/brain/queue updates.
pub fn set_character_skill_level(
    ctx: &ServerContext,
    target_char_id: i64,
    skill_type_id: TypeId,
    level: u8,
) -> Result<SetCharacterSkillResult, DomainError> {
    if level > 5 {
        return Err(DomainError::invalid_input(
            "InvalidSlashArgument",
            "Level for /setskill must be between 0 and 5",
        ));
    }
    let Some(skill_type) = ctx.sde.types.get(&skill_type_id) else {
        return Err(DomainError::not_found(
            "SkillNotFound",
            format!("Skill type ID {skill_type_id} not found in SDE"),
        ));
    };
    if skill_type.category_id != 16 {
        return Err(DomainError::invalid_input(
            "InvalidSkillType",
            format!("Type {skill_type_id} is not a skill (category 16)"),
        ));
    }
    let rank = skill_type
        .default_attributes
        .get(&attr_ids::SKILL_TIME_CONSTANT)
        .copied()
        .unwrap_or(1.0);
    let skill_name = skill_type.name.clone();
    let target_sp = sp_for_skill_level(rank, level);
    let now_ft = ctx.now_blueos_filetime_ticks();
    let now_unix = ctx.now_unix();

    let outcome = ctx.db.tx("set_character_skill_level", |wtx| {
        apply_character_skill_update_in_snap(
            ctx,
            wtx.snap,
            target_char_id,
            CharacterSkillUpdate {
                skill_type_id,
                new_level: level,
                new_skill_points: target_sp,
                remove_when_zero: true,
                free_sp_delta: 0.0,
                history_event_type_id: None,
                now_ft,
                now_unix,
                allow_rewind_start_sp: true,
            },
        )
    })?;

    ctx.sync_ship_ballpark_stats(target_char_id, outcome.active_ship_id);
    if level == 0 {
        if outcome.was_removed {
            // Pass post-removal level 0 so `emit_server_skills_removed` emits `-1.0` SP and
            // V24 `SkillsSvc._UpdateMySkillsAndReturnLevelsTrained` deletes the cached skill.
            emit_server_skills_removed(ctx, target_char_id, &[(skill_type_id, 0)]);
        }
    } else {
        emit_server_skills_changed(ctx, target_char_id, &[skill_type_id], Some("GMGiveSkill"));
    }
    ctx.push_brain_update(target_char_id);
    if outcome.queue_changed {
        let _ = ctx.notify_new_skill_queue_saved(
            target_char_id,
            skill_queue_rowset(ctx, target_char_id),
        );
    }

    Ok(SetCharacterSkillResult {
        target_char_id,
        skill_type_id,
        skill_name,
        level,
        skill_points: target_sp,
        character_total_sp: outcome.total_skill_points,
    })
}

/// Applies unallocated free skill points to one injected skill atomically via
/// `WorldRepository::tx`, enforcing prerequisites, Alpha clone caps, queue reconciliation, and
/// fit/skill/brain/free-SP updates.
pub fn apply_free_skill_points_to_skill(
    ctx: &ServerContext,
    char_id: i64,
    session_ship_id: Option<i64>,
    skill_type_id: TypeId,
    points_to_apply: f64,
) -> Result<ApplyFreeSkillPointsResult, DomainError> {
    if !points_to_apply.is_finite() || points_to_apply <= 0.0 {
        return Err(DomainError::invalid_input(
            "InvalidSkillPointAmount",
            "Requested free skill points to apply must be finite and positive",
        ));
    }
    let skill_type = validate_published_skill_type(&ctx.sde, skill_type_id).map_err(rpc_err_to_domain)?;
    let rank = skill_type
        .default_attributes
        .get(&attr_ids::SKILL_TIME_CONSTANT)
        .copied()
        .unwrap_or(1.0);
    let now_ft = ctx.now_blueos_filetime_ticks();
    let now_unix = ctx.now_unix();

    let (new_skill_sp, outcome) = ctx.db.tx("apply_free_skill_points", |wtx| {
        let Some(ch_snap) = wtx.snap.characters.get(&char_id).cloned() else {
            return Err(DomainError::not_found(
                "CharacterNotFound",
                format!("Character {char_id} not found"),
            ));
        };
        if ch_snap.free_skill_points <= 0.0 {
            return Err(DomainError::precondition_failed(
                "InsufficientFreeSP",
                "No unallocated free skill points available to apply",
            ));
        }
        let (existing_skill, max_lvl, room_in_skill, alpha_total_room) = {
            let skills_map = wtx.snap.skills.entry(char_id).or_default();
            let Some(existing_skill) = skills_map.get(&skill_type_id).cloned() else {
                return Err(DomainError::precondition_failed(
                    "SkillNotInjected",
                    format!(
                        "Skill {} ({skill_type_id}) must be injected before applying free skill points",
                        skill_type.name
                    ),
                ));
            };
            check_skill_type_prerequisites(skill_type, |req_skill_id| {
                skills_map
                    .get(&req_skill_id)
                    .map(|s| s.level)
                    .unwrap_or(0)
            })
            .map_err(rpc_err_to_domain)?;
            let max_lvl = if ch_snap.clone_grade <= 1 {
                alpha_skill_cap_for_character(&ctx.sde, ch_snap.race_id, skill_type_id)
            } else {
                5
            };
            let max_skill_sp = sp_for_skill_level(rank, max_lvl);
            let room_in_skill = (max_skill_sp - existing_skill.skill_points).max(0.0);
            if room_in_skill <= 0.0 {
                return Err(DomainError::precondition_failed(
                    "SkillAlreadyAtMaxSP",
                    format!(
                        "Skill {} ({skill_type_id}) is already at maximum trainable SP ({max_skill_sp})",
                        skill_type.name
                    ),
                ));
            }
            let alpha_total_room = if ch_snap.clone_grade <= 1 {
                let r = (5_000_000.0 - ch_snap.skill_points).max(0.0);
                if r <= 0.0 {
                    return Err(DomainError::precondition_failed(
                        "AlphaCloneSpCapExceeded",
                        "Alpha clones cannot apply free skill points beyond 5,000,000 total SP",
                    ));
                }
                r
            } else {
                f64::INFINITY
            };
            (existing_skill, max_lvl, room_in_skill, alpha_total_room)
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

        let outcome = apply_character_skill_update_in_snap(
            ctx,
            wtx.snap,
            char_id,
            CharacterSkillUpdate {
                skill_type_id,
                new_level: achieved_lvl,
                new_skill_points: new_skill_sp,
                remove_when_zero: false,
                free_sp_delta: -applied,
                history_event_type_id: Some(307),
                now_ft,
                now_unix,
                allow_rewind_start_sp: false,
            },
        )?;
        Ok((new_skill_sp, outcome))
    })?;

    if let Some(sid) = session_ship_id {
        ctx.invalidate_and_push_fit(sid);
    }
    emit_server_skills_changed(ctx, char_id, &[skill_type_id], None);
    ctx.push_brain_update(char_id);
/// Result metadata returned by [`reset_character_skills_to_race_starter`].
#[derive(Clone, Debug, PartialEq)]
pub struct ResetSkillsToStarterOutcome {
    pub char_id: i64,
    pub race_id: u32,
    pub active_ship_id: i64,
    pub starter_skill_count: usize,
    pub removed_skill_count: usize,
    pub total_skill_points: f64,
    pub free_skill_points: f64,
    pub changed_skill_type_ids: Vec<TypeId>,
}

/// Resets `char_id`'s trained skills, active/paused skill queues, skill history, and SP totals
/// back to the character's SDE racial starter profile via [`WorldRepository::starter_skills_for_race`]
/// inside one durable DB transaction, emitting skill/queue/free-SP/brain/fit notifications (`#1193`).
pub fn reset_character_skills_to_race_starter(
    ctx: &ServerContext,
    char_id: i64,
) -> Result<ResetSkillsToStarterOutcome, DomainError> {
    const SKILL_EVENT_CHAR_CREATION: i64 = 33;

    let now_ft = ctx.now_blueos_filetime_ticks();
    let outcome = ctx.db.tx("reset_character_skills_to_race_starter", |wtx| {
        let Some(ch) = wtx.snap.characters.get_mut(&char_id) else {
            return Err(DomainError::not_found(
                "CharacterNotFound",
                format!("Character {char_id} not found"),
            ));
        };
        let race_id = ch.race_id;
        let active_ship_id = ch.active_ship_id;
        let old_skill_ids: Vec<TypeId> = wtx
            .snap
            .skills
            .get(&char_id)
            .map(|m| m.keys().copied().collect())
            .unwrap_or_default();

        let (starter_skills_map, total_sp) =
            WorldRepository::starter_skills_for_race(race_id, &ctx.sde);

        let mut sorted_starter: Vec<TrainedSkillRecord> =
            starter_skills_map.values().cloned().collect();
        sorted_starter.sort_by_key(|s| s.type_id);
        let starter_history: Vec<SkillHistoryEntry> = sorted_starter
            .into_iter()
            .map(|s| SkillHistoryEntry {
                log_date_ft: now_ft,
                event_type_id: SKILL_EVENT_CHAR_CREATION,
                skill_type_id: s.type_id,
                skill_level: s.level,
                absolute_points: s.skill_points,
            })
            .collect();

        let mut changed_tids: BTreeSet<TypeId> = old_skill_ids.iter().copied().collect();
        changed_tids.extend(starter_skills_map.keys().copied());
        let removed_count = old_skill_ids
            .iter()
            .filter(|tid| !starter_skills_map.contains_key(tid))
            .count();
        let starter_count = starter_skills_map.len();

        ch.skill_points = total_sp;
        ch.free_skill_points = 0.0;
        if starter_skills_map.is_empty() {
            wtx.snap.skills.remove(&char_id);
        } else {
            wtx.snap.skills.insert(char_id, starter_skills_map);
        }
        wtx.snap.skill_queues.remove(&char_id);
        wtx.snap.paused_skill_queues.remove(&char_id);
        if starter_history.is_empty() {
            wtx.snap.skill_history.remove(&char_id);
        } else {
            wtx.snap.skill_history.insert(char_id, starter_history);
        }

        Ok(ResetSkillsToStarterOutcome {
            char_id,
            race_id,
            active_ship_id,
            starter_skill_count: starter_count,
            removed_skill_count: removed_count,
            total_skill_points: total_sp,
            free_skill_points: 0.0,
            changed_skill_type_ids: changed_tids.into_iter().collect(),
        })
    })?;

    emit_server_skills_changed(
        ctx,
        char_id,
        &outcome.changed_skill_type_ids,
        Some("SkillReset"),
    );
    let _ = ctx.notify_new_skill_queue_saved(char_id, skill_queue_rowset(ctx, char_id));
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        crate::router::notify_registry::OnFreeSkillPointsChanged {
            new_free_skill_points: 0.0,
        },
    );
    ctx.push_brain_update(char_id);
    if outcome.active_ship_id > 0 {
        ctx.invalidate_and_push_fit(outcome.active_ship_id);
    }

    Ok(outcome)
}

    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        crate::router::notify_registry::OnFreeSkillPointsChanged {
            new_free_skill_points: outcome.remaining_free_skill_points,
        },
    );

    Ok(ApplyFreeSkillPointsResult {
        new_skill_sp,
        remaining_free_sp: outcome.remaining_free_skill_points,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_character_skills_to_race_starter_restores_starter_profile_and_clears_queues() {
        let ctx = ServerContext::new();
        let cid = 90_000_001_i64;
        let extra_skill_id = ctx
            .sde
            .types
            .values()
            .find(|t| t.category_id == 16 && t.published && t.type_id != 3402)
            .map(|t| t.type_id)
            .unwrap_or(3300);

        let race_id = ctx.db.with_snapshot(|snap| snap.characters[&cid].race_id);
        let (expected_starter, expected_sp) =
            WorldRepository::starter_skills_for_race(race_id, &ctx.sde);
        assert!(
            !expected_starter.is_empty(),
            "demo race must have SDE starter skills"
        );

        ctx.db.with_mut_snapshot(|snap| {
            let skills = snap.skills.entry(cid).or_default();
            skills.insert(
                extra_skill_id,
                TrainedSkillRecord {
                    type_id: extra_skill_id,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
            snap.skill_queues.insert(
                cid,
                vec![eve_db::SkillQueueEntry {
                    type_id: extra_skill_id,
                    target_level: 5,
                    start_sp: 0.0,
                    end_sp: 256_000.0,
                    duration_seconds: 1_200.0,
                    started_at_unix: Some(1_000),
                }],
            );
            snap.paused_skill_queues.insert(cid, Vec::new());
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.free_skill_points = 50_000.0;
            }
        });
        let _ = ctx.drain_notifications_for_char(cid);

        let out = reset_character_skills_to_race_starter(&ctx, cid)
            .expect("reset_character_skills_to_race_starter ok");
        assert_eq!(out.char_id, cid);
        assert_eq!(out.race_id, race_id);
        assert_eq!(out.starter_skill_count, expected_starter.len());
        assert_eq!(out.free_skill_points, 0.0);
        assert!((out.total_skill_points - expected_sp).abs() < 1e-6);

        ctx.db.with_snapshot(|snap| {
            let trained = snap.skills.get(&cid).expect("starter skills present");
            assert_eq!(trained.len(), expected_starter.len());
            for (tid, exp_rec) in &expected_starter {
                let got = trained.get(tid).expect("starter skill retained");
                assert_eq!(got.level, exp_rec.level);
            }
            assert!(!snap.skill_queues.contains_key(&cid));
            assert!(!snap.paused_skill_queues.contains_key(&cid));
            assert_eq!(snap.characters[&cid].free_skill_points, 0.0);
        });

        let notes = ctx.drain_notifications_for_char(cid);
        assert!(notes.iter().any(|n| n.event == "OnServerSkillsChanged"));
        assert!(notes.iter().any(|n| n.event == "OnFreeSkillPointsChanged"));
    }

    #[test]
    fn reset_character_skills_to_race_starter_rejects_missing_character() {
        let ctx = ServerContext::new();
        let err = reset_character_skills_to_race_starter(&ctx, 999_888_777)
            .expect_err("missing character must fail");
        assert_eq!(err.code, "CharacterNotFound");
    }
}
