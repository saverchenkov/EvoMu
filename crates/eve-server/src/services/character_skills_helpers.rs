//! Tail dispatch helpers extracted from `character_skills` to keep module size under control.

#![forbid(unsafe_code)]

use super::{
    alpha_skill_cap_for_character, catch_up_skill_queue, effective_character_attributes,
    lookup_effective_attr_from_tuple,
};
use crate::formulas::{skill_injector_sp_yield, skill_training_time_seconds, sp_for_skill_level};
use crate::router::{NotificationTarget, ServerContext};
use crate::services::implant_plugging::{
    is_daily_alpha_injector, is_non_diminishing_injector, load_alpha_injector_cooldowns,
    non_diminishing_sp_yield, ALPHA_INJECTOR_COOLDOWN_TICKS, TYPE_LARGE_SKILL_INJECTOR,
    TYPE_SMALL_SKILL_INJECTOR,
};
use eve_db::{
    InventoryItemRecord, SkillHistoryEntry, SkillQueueEntry, TrainedSkillRecord, WorldStateSnapshot,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{attr_ids, TypeId};
use std::collections::{BTreeMap, HashMap};

fn err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError {
        code: code.into(),
        message: message.into(),
    }
}

pub(crate) fn handle_skill_tail(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("skillMgr2", "GetImplants") => Some(handle_get_implants(ctx, session)),
        ("skillMgr2", "GetBoosters") => Some(handle_get_boosters(ctx, session)),
        ("skillMgr2", "GetSkillChangesForISIS") => {
            Some(handle_get_skill_changes_for_isis(ctx, session))
        }
        ("skillMgr2", "ApplyFreeSkillPointsToQueue") => {
            Some(handle_apply_free_skill_points_to_queue(ctx, session))
        }
        ("skillMgr2", "ApplyFreeSkillPointsToSkills") => {
            Some(handle_apply_free_skill_points_to_skills(ctx, session, args))
        }
        ("skillMgr2", "GetFreeSkillPointsAppliedToQueue") => {
            Some(handle_get_free_skill_points_applied_to_queue(ctx, session))
        }
        ("skillMgr2", "GetFreeSkillPointsAppliedToSkills") => {
            Some(handle_get_free_skill_points_applied_to_skills(ctx, session, args))
        }
        ("skillMgr2", "CharStartTrainingSkill") => {
            Some(handle_char_start_training_skill(ctx, session, args))
        }
        ("skillMgr2", "CheckInjectionConstraints") => {
            Some(handle_check_injection_constraints(ctx, session, args))
        }
        ("skillMgr2", "CombineSkillInjector") => {
            Some(handle_combine_skill_injector(ctx, session, args))
        }
        ("skillMgr2", "SplitSkillInjector") => {
            Some(handle_split_skill_injector(ctx, session, args))
        }
        ("skillMgr2", "GetDiminishedSpFromInjectors") => {
            Some(handle_get_diminished_sp_from_injectors(ctx, session, args))
        }
        ("skillMgr2", "GetEstimatedSkillPoints") => {
            Some(handle_get_estimated_skill_points(ctx, session, args))
        }
        ("skillMgr2", "GetSkillPoints") => Some(handle_get_skill_points(ctx, session, args)),
        ("skillMgr2", "GetSkillLevel") => Some(handle_get_skill_level(ctx, session, args)),
        ("skillMgr2", "GetSkillBundleInfo") => Some(handle_get_skill_bundle_info(ctx, session, args)),
        ("dogmaIM", "InjectImplant") => Some(handle_inject_implant(ctx, session, args)),
        ("dogmaIM", "DestroyImplant") => Some(handle_destroy_implant(ctx, session, args)),
        ("dogmaIM", "UseBooster") => Some(handle_use_booster(ctx, session, args)),
        _ => crate::services::implant_plugging::handle_skill_notifications_and_injections(
            ctx, session, service, method,
        )
        .or_else(|| {
            super::character_clones_implants::handle_clones_and_sheet(
                ctx, session, service, method, args, _kwargs,
            )
        }),
    }
}

fn handle_get_implants(ctx: &ServerContext, session: &SessionState) -> Result<PyRep, BridgeRpcError> {
    crate::services::implant_plugging::get_implants(ctx, session)
}

fn handle_get_boosters(ctx: &ServerContext, session: &SessionState) -> Result<PyRep, BridgeRpcError> {
    crate::services::implant_plugging::get_boosters(ctx, session)
}

fn handle_get_skill_changes_for_isis(
    ctx: &ServerContext,
    session: &SessionState,
) -> Result<PyRep, BridgeRpcError> {
    super::super::character_skill_ops::handle_get_skill_changes_for_isis(ctx, session)
}

fn handle_inject_implant(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    crate::services::implant_plugging::inject_implant(ctx, session, args)
}

fn handle_destroy_implant(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    crate::services::implant_plugging::destroy_implant(ctx, session, args)
}

fn handle_use_booster(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    crate::services::implant_plugging::use_booster(ctx, session, args)
}

fn skill_rank(ctx: &ServerContext, skill_type_id: TypeId) -> f64 {
    ctx.sde
        .types
        .get(&skill_type_id)
        .and_then(|t| t.default_attributes.get(&attr_ids::SKILL_TIME_CONSTANT).copied())
        .unwrap_or(1.0)
}

fn check_skill_prerequisites(
    ctx: &ServerContext,
    skills_map: &HashMap<TypeId, TrainedSkillRecord>,
    skill_type_id: TypeId,
) -> Result<(), BridgeRpcError> {
    let Some(skill_type) = ctx.sde.types.get(&skill_type_id) else {
        return Err(err(
            "InvalidSkillType",
            format!("Skill type {skill_type_id} not found in SDE"),
        ));
    };
    if skill_type.category_id != 16 {
        return Err(err(
            "InvalidSkillType",
            format!("Type {skill_type_id} is not a skill (category 16)"),
        ));
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
                let trained_lvl = skills_map.get(&req_skill_id).map(|s| s.level).unwrap_or(0);
                if trained_lvl < req_lvl {
                    return Err(err(
                        "SkillPrerequisiteNotMet",
                        format!(
                            "Skill {} (type {skill_type_id}) requires prerequisite skill {req_skill_id} at level {req_lvl}",
                            skill_type.name
                        ),
                    ));
                }
            }
        }
    }
    Ok(())
}

fn parse_skills_and_levels_arg(arg: Option<&PyRep>) -> Result<Vec<(TypeId, u8)>, BridgeRpcError> {
    let mut out = Vec::new();
    match arg {
        Some(PyRep::Dict(entries)) => {
            for (k, v) in entries {
                let tid = k
                    .as_i64()
                    .filter(|&id| id > 0)
                    .ok_or_else(|| err("InvalidSkillType", "Skill typeID must be a positive integer"))?
                    as TypeId;
                let lvl = v
                    .as_i64()
                    .filter(|&l| (1..=5).contains(&l))
                    .ok_or_else(|| err("InvalidSkillLevel", "Skill level must be between 1 and 5"))?
                    as u8;
                out.push((tid, lvl));
            }
            out.sort_by_key(|&(tid, lvl)| (lvl, tid));
        }
        Some(PyRep::List(items) | PyRep::Tuple(items)) => {
            for item in items {
                let (PyRep::List(pair) | PyRep::Tuple(pair)) = item else {
                    return Err(err(
                        "InvalidArgument",
                        "Each skill entry must be a (skillTypeID, level) pair",
                    ));
                };
                let tid = pair
                    .first()
                    .and_then(|v| v.as_i64())
                    .filter(|&id| id > 0)
                    .ok_or_else(|| err("InvalidSkillType", "Skill typeID must be a positive integer"))?
                    as TypeId;
                let lvl = pair
                    .get(1)
                    .and_then(|v| v.as_i64())
                    .filter(|&l| (1..=5).contains(&l))
                    .ok_or_else(|| err("InvalidSkillLevel", "Skill level must be between 1 and 5"))?
                    as u8;
                out.push((tid, lvl));
            }
        }
        _ => {
            return Err(err(
                "InvalidArgument",
                "Expected a mapping or list of (skillTypeID, level) pairs",
            ));
        }
    }
    Ok(out)
}

fn handle_get_free_skill_points_applied_to_queue(
    ctx: &ServerContext,
    session: &SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    catch_up_skill_queue(ctx, char_id);
    let allocations = ctx.db.with_snapshot(|snap| {
        let Some(ch) = snap.characters.get(&char_id) else {
            return Vec::new();
        };
        let mut budget = ch.free_skill_points.max(0.0);
        if ch.clone_grade <= 1 {
            budget = budget.min((5_000_000.0 - ch.skill_points).max(0.0));
        }
        if budget <= 0.0 {
            return Vec::new();
        }
        let Some(queue) = snap.skill_queues.get(&char_id) else {
            return Vec::new();
        };
        let mut per_skill: BTreeMap<TypeId, f64> = BTreeMap::new();
        for q in queue {
            if budget <= 0.0 {
                break;
            }
            if ch.clone_grade <= 1
                && q.target_level > alpha_skill_cap_for_character(&ctx.sde, ch.race_id, q.type_id)
            {
                continue;
            }
            let needed = (q.end_sp - q.start_sp).max(0.0);
            if needed <= 0.0 {
                continue;
            }
            let applied = budget.min(needed);
            budget -= applied;
            *per_skill.entry(q.type_id).or_default() += applied;
        }
        per_skill
            .into_iter()
            .filter_map(|(tid, pts)| {
                let rounded = pts.round() as i64;
                (rounded > 0).then_some((PyRep::Integer(tid as i64), PyRep::Integer(rounded)))
            })
            .collect::<Vec<(PyRep, PyRep)>>()
    });
    Ok(PyRep::Dict(allocations))
}

fn handle_apply_free_skill_points_to_queue(
    ctx: &ServerContext,
    session: &SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    catch_up_skill_queue(ctx, char_id);
    let now_ft = ctx.now_blueos_filetime_ticks();
    let now_unix = ctx.now_unix();
    let (changed_skills, rem_free) = ctx.db.try_with_mut_snapshot(|snap| {
        let Some(ch_snap) = snap.characters.get(&char_id).cloned() else {
            return Err(err("CharacterNotFound", format!("Character {char_id} not found")));
        };
        if ch_snap.free_skill_points <= 0.0 {
            return Err(err(
                "InsufficientFreeSP",
                "No unallocated free skill points available to apply",
            ));
        }
        let mut remaining_budget = ch_snap.free_skill_points;
        if ch_snap.clone_grade <= 1 {
            let alpha_room = (5_000_000.0 - ch_snap.skill_points).max(0.0);
            if alpha_room <= 0.0 {
                return Err(err(
                    "AlphaCloneSpCapExceeded",
                    "Alpha clones cannot apply free skill points beyond 5,000,000 total SP",
                ));
            }
            remaining_budget = remaining_budget.min(alpha_room);
        }

        let queue = snap.skill_queues.entry(char_id).or_default().clone();
        let mut remaining_queue = Vec::new();
        let mut history_to_append = Vec::new();
        let mut changed_tids = Vec::new();
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
                if !changed_tids.contains(&q.type_id) {
                    changed_tids.push(q.type_id);
                }
                history_to_append.push(SkillHistoryEntry {
                    log_date_ft: now_ft,
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
                let rank = skill_rank(ctx, q.type_id);
                let current_lvl = skills_map.get(&q.type_id).map(|s| s.level).unwrap_or(0);
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
                if !changed_tids.contains(&q.type_id) {
                    changed_tids.push(q.type_id);
                }
                history_to_append.push(SkillHistoryEntry {
                    log_date_ft: now_ft,
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
        if !snap.paused_skill_queues.contains(&char_id) {
            if let Some(head) = remaining_queue.first_mut() {
                head.started_at_unix.get_or_insert(now_unix);
            }
        }
        snap.skill_queues.insert(char_id, remaining_queue);
        if !history_to_append.is_empty() {
            snap.skill_history.entry(char_id).or_default().extend(history_to_append);
        }
        let rem_free = if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.free_skill_points = (ch.free_skill_points - total_applied).max(0.0);
            ch.skill_points += total_applied;
            ch.free_skill_points
        } else {
            0.0
        };
        Ok((changed_tids, rem_free))
    })?;

    if let Some(sid) = session.ship_id {
        ctx.invalidate_ship_fitting(sid);
    }
    if !changed_skills.is_empty() {
        crate::services::jump_clone_names::emit_server_skills_changed(
            ctx,
            char_id,
            &changed_skills,
            None,
        );
    }
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnFreeSkillPointsChanged",
        "charid",
        vec![PyRep::Real(rem_free)],
    );
    Ok(PyRep::None)
}

fn handle_get_free_skill_points_applied_to_skills(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    if args.is_empty() {
        return Ok(PyRep::Dict(Vec::new()));
    }
    let targets = parse_skills_and_levels_arg(args.first())?;
    catch_up_skill_queue(ctx, char_id);

    let allocations = ctx.db.with_snapshot(|snap| {
        let Some(ch) = snap.characters.get(&char_id) else {
            return Vec::new();
        };
        let mut budget = ch.free_skill_points.max(0.0);
        if ch.clone_grade <= 1 {
            budget = budget.min((5_000_000.0 - ch.skill_points).max(0.0));
        }
        if budget <= 0.0 {
            return Vec::new();
        }
        let skills_map = snap.skills.get(&char_id);
        let mut simulated_sp: HashMap<TypeId, f64> = HashMap::new();
        let mut per_skill: BTreeMap<TypeId, f64> = BTreeMap::new();

        for (tid, target_lvl) in targets {
            if budget <= 0.0 {
                break;
            }
            let Some(existing) = skills_map.and_then(|m| m.get(&tid)) else {
                continue;
            };
            let max_lvl = if ch.clone_grade <= 1 {
                alpha_skill_cap_for_character(&ctx.sde, ch.race_id, tid)
            } else {
                5
            };
            let eff_target = target_lvl.min(max_lvl);
            let rank = skill_rank(ctx, tid);
            let cur_sp = simulated_sp
                .get(&tid)
                .copied()
                .unwrap_or(existing.skill_points);
            let target_sp = sp_for_skill_level(rank, eff_target);
            let needed = (target_sp - cur_sp).max(0.0);
            if needed <= 0.0 {
                continue;
            }
            let applied = budget.min(needed);
            budget -= applied;
            simulated_sp.insert(tid, cur_sp + applied);
            *per_skill.entry(tid).or_default() += applied;
        }
        per_skill
            .into_iter()
            .filter_map(|(tid, pts)| {
                let rounded = pts.round() as i64;
                (rounded > 0).then_some((PyRep::Integer(tid as i64), PyRep::Integer(rounded)))
            })
            .collect::<Vec<(PyRep, PyRep)>>()
    });
    Ok(PyRep::Dict(allocations))
}

fn handle_apply_free_skill_points_to_skills(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let targets = parse_skills_and_levels_arg(args.first())?;
    if targets.is_empty() {
        return Err(err("InvalidArgument", "No target skills specified"));
    }
    catch_up_skill_queue(ctx, char_id);
    let now_ft = ctx.now_blueos_filetime_ticks();
    let now_unix = ctx.now_unix();

    let (changed_skills, rem_free) = ctx.db.try_with_mut_snapshot(|snap| {
        let Some(ch_snap) = snap.characters.get(&char_id).cloned() else {
            return Err(err("CharacterNotFound", format!("Character {char_id} not found")));
        };
        if ch_snap.free_skill_points <= 0.0 {
            return Err(err(
                "InsufficientFreeSP",
                "No unallocated free skill points available to apply",
            ));
        }
        let mut remaining_budget = ch_snap.free_skill_points;
        if ch_snap.clone_grade <= 1 {
            let alpha_room = (5_000_000.0 - ch_snap.skill_points).max(0.0);
            if alpha_room <= 0.0 {
                return Err(err(
                    "AlphaCloneSpCapExceeded",
                    "Alpha clones cannot apply free skill points beyond 5,000,000 total SP",
                ));
            }
            remaining_budget = remaining_budget.min(alpha_room);
        }

        let mut total_applied = 0.0;
        let mut changed_tids = Vec::new();
        for (tid, target_lvl) in targets {
            if remaining_budget <= 0.0 {
                break;
            }
            {
                let skills_ro = snap.skills.entry(char_id).or_default();
                if !skills_ro.contains_key(&tid) {
                    return Err(err(
                        "CannotApplyFreePointsDoNotHaveSkill",
                        format!("Skill {tid} must be injected before applying free skill points"),
                    ));
                }
                check_skill_prerequisites(ctx, skills_ro, tid)?;
            }
            let max_lvl = if ch_snap.clone_grade <= 1 {
                alpha_skill_cap_for_character(&ctx.sde, ch_snap.race_id, tid)
            } else {
                5
            };
            let eff_target = target_lvl.min(max_lvl);
            let rank = skill_rank(ctx, tid);
            let existing = snap.skills[&char_id][&tid].clone();
            let target_sp = sp_for_skill_level(rank, eff_target);
            let needed = (target_sp - existing.skill_points).max(0.0);
            if needed <= 0.0 {
                continue;
            }
            let applied = remaining_budget.min(needed);
            if applied <= 0.0 {
                continue;
            }
            remaining_budget -= applied;
            total_applied += applied;
            let new_skill_sp = existing.skill_points + applied;
            let mut achieved_lvl = existing.level;
            for lvl in (existing.level + 1)..=max_lvl {
                if new_skill_sp + 0.5 >= sp_for_skill_level(rank, lvl) {
                    achieved_lvl = lvl;
                } else {
                    break;
                }
            }
            snap.skills.entry(char_id).or_default().insert(
                tid,
                TrainedSkillRecord {
                    type_id: tid,
                    level: achieved_lvl,
                    skill_points: new_skill_sp,
                },
            );
            if let Some(queue) = snap.skill_queues.get_mut(&char_id) {
                queue.retain_mut(|q| {
                    if q.type_id != tid {
                        return true;
                    }
                    if new_skill_sp + 0.5 >= q.end_sp {
                        false
                    } else {
                        if new_skill_sp > q.start_sp {
                            let old_needed = (q.end_sp - q.start_sp).max(1.0);
                            q.start_sp = new_skill_sp;
                            q.duration_seconds *= ((q.end_sp - q.start_sp).max(0.0)) / old_needed;
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
                    event_type_id: 307,
                    skill_type_id: tid,
                    skill_level: achieved_lvl,
                    absolute_points: new_skill_sp,
                });
            if !changed_tids.contains(&tid) {
                changed_tids.push(tid);
            }
        }
        if !snap.paused_skill_queues.contains(&char_id) {
            if let Some(head) = snap.skill_queues.get_mut(&char_id).and_then(|q| q.first_mut()) {
                head.started_at_unix.get_or_insert(now_unix);
            }
        }
        let rem_free = if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.free_skill_points = (ch.free_skill_points - total_applied).max(0.0);
            ch.skill_points += total_applied;
            ch.free_skill_points
        } else {
            0.0
        };
        Ok((changed_tids, rem_free))
    })?;

    if let Some(sid) = session.ship_id {
        ctx.invalidate_ship_fitting(sid);
    }
    if !changed_skills.is_empty() {
        crate::services::jump_clone_names::emit_server_skills_changed(
            ctx,
            char_id,
            &changed_skills,
            None,
        );
    }
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnFreeSkillPointsChanged",
        "charid",
        vec![PyRep::Real(rem_free)],
    );
    Ok(PyRep::None)
}

fn handle_char_start_training_skill(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let raw_id = args
        .first()
        .and_then(|a| a.as_i64())
        .filter(|&v| v > 0)
        .ok_or_else(|| err("InvalidArgument", "CharStartTrainingSkill requires a skill itemID or typeID"))?;

    catch_up_skill_queue(ctx, char_id);
    let now_unix = ctx.now_unix();

    let (paused_other_chars, _) = ctx.db.try_with_mut_snapshot(|snap| {
        let Some(ch) = snap.characters.get(&char_id).cloned() else {
            return Err(err("CharacterNotFound", format!("Character {char_id} not found")));
        };
        let skill_tid: TypeId = if let Some(item) = snap
            .items
            .get(&raw_id)
            .filter(|it| it.owner_id == char_id)
        {
            item.type_id
        } else if let Ok(tid) = u32::try_from(raw_id) {
            tid
        } else {
            return Err(err("SkillNotInjected", "Invalid skill identifier"));
        };

        let skills_map = snap.skills.get(&char_id).cloned().unwrap_or_default();
        check_skill_prerequisites(ctx, &skills_map, skill_tid)?;
        let Some(existing) = skills_map.get(&skill_tid) else {
            return Err(err(
                "SkillNotInjected",
                format!("Skill {skill_tid} is not injected"),
            ));
        };
        let max_lvl = if ch.clone_grade <= 1 {
            alpha_skill_cap_for_character(&ctx.sde, ch.race_id, skill_tid)
        } else {
            5
        };
        if existing.level >= max_lvl {
            return Err(err(
                if ch.clone_grade <= 1 && existing.level < 5 {
                    "AlphaSkillCapExceeded"
                } else {
                    "SkillAlreadyAtMaxLevel"
                },
                format!("Skill {skill_tid} is already at maximum trainable level ({max_lvl})"),
            ));
        }
        let target_level = existing.level + 1;
        let rank = skill_rank(ctx, skill_tid);
        let eff_attrs = effective_character_attributes(&ctx.sde, snap, char_id);
        let skill_type = &ctx.sde.types[&skill_tid];
        let prim_id = skill_type
            .default_attributes
            .get(&attr_ids::PRIMARY_ATTRIBUTE)
            .copied()
            .unwrap_or(164.0) as u16;
        let sec_id = skill_type
            .default_attributes
            .get(&attr_ids::SECONDARY_ATTRIBUTE)
            .copied()
            .unwrap_or(167.0) as u16;
        let prim_val = lookup_effective_attr_from_tuple(eff_attrs, u32::from(prim_id));
        let sec_val = lookup_effective_attr_from_tuple(eff_attrs, u32::from(sec_id));
        let clone_mult = if ch.clone_grade >= 2 { 1.0 } else { 0.5 };
        let start_sp = existing.skill_points;
        let end_sp = sp_for_skill_level(rank, target_level);
        let duration_seconds =
            skill_training_time_seconds(start_sp, end_sp, prim_val, sec_val, clone_mult);

        let mut queue = snap.skill_queues.remove(&char_id).unwrap_or_default();
        queue.retain(|q| !(q.type_id == skill_tid && q.target_level == target_level));
        if let Some(prev_head) = queue.first_mut() {
            prev_head.started_at_unix = None;
        }
        queue.insert(
            0,
            SkillQueueEntry {
                type_id: skill_tid,
                target_level,
                started_at_unix: Some(now_unix),
                duration_seconds,
                start_sp,
                end_sp,
            },
        );
        snap.skill_queues.insert(char_id, queue);
        snap.paused_skill_queues.remove(&char_id);

        let mct_slots: Vec<u64> = snap
            .cluster_and_runtime
            .mct_slots
            .get(&ch.user_id)
            .cloned()
            .or_else(|| {
                crate::services::account_services::load_store(snap)
                    .mct_slots
                    .get(&ch.user_id)
                    .cloned()
            })
            .unwrap_or_default();
        let active_mct_count = mct_slots.into_iter().filter(|&exp| exp > now_unix).count();
        let mut paused_others = Vec::new();
        if active_mct_count == 0 {
            let other_cids: Vec<i64> = snap
                .characters
                .values()
                .filter(|c| c.user_id == ch.user_id && c.character_id != char_id)
                .map(|c| c.character_id)
                .collect();
            for ocid in other_cids {
                if !snap.paused_skill_queues.contains(&ocid) {
                    snap.paused_skill_queues.insert(ocid);
                    paused_others.push(ocid);
                }
            }
        }
        Ok((paused_others, skill_tid))
    })?;

    for ocid in paused_other_chars {
        let _ = ctx.notify_skill_queue_paused_server(ocid);
    }
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnSkillQueuePausedServer",
        "charid",
        vec![PyRep::Bool(false)],
    );
    Ok(PyRep::None)
}

fn validate_injector_for_char(
    ctx: &ServerContext,
    snap: &WorldStateSnapshot,
    char_id: i64,
    current_dock: Option<i64>,
    current_ship: Option<i64>,
    item_id: i64,
    quantity: i32,
) -> Result<InventoryItemRecord, BridgeRpcError> {
    let Some(item) = snap.items.get(&item_id).cloned() else {
        return Err(err(
            "InjectorNotFound",
            format!("Skill injector item {item_id} not found"),
        ));
    };
    if !snap.is_item_consumable_by(&item, char_id, current_dock, current_ship) {
        return Err(err(
            "InjectorNotAccessible",
            "Skill injector is not owned or accessible by character",
        ));
    }
    if item.quantity < quantity || item.singleton {
        return Err(err(
            "InsufficientInjectors",
            format!(
                "Requested {quantity} skill injectors but only {} available",
                item.quantity
            ),
        ));
    }
    let is_valid_type = item.type_id == TYPE_LARGE_SKILL_INJECTOR
        || item.type_id == TYPE_SMALL_SKILL_INJECTOR
        || is_daily_alpha_injector(item.type_id)
        || is_non_diminishing_injector(&ctx.sde, item.type_id);
    if !is_valid_type {
        return Err(err(
            "InvalidSkillInjector",
            format!("Item type {} is not a valid skill injector", item.type_id),
        ));
    }
    Ok(item)
}

fn handle_check_injection_constraints(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let item_id = args
        .first()
        .and_then(|a| a.as_i64())
        .filter(|&id| id > 0)
        .ok_or_else(|| err("InvalidArgument", "CheckInjectionConstraints requires itemID"))?;
    let quantity = args.get(1).and_then(|a| a.as_i64()).unwrap_or(1);
    if quantity <= 0 {
        return Err(err(
            "InvalidSkillPointAmount",
            "Injector quantity must be positive",
        ));
    }
    let current_dock = session.station_id.or(session.structure_id);
    let current_ship = session.ship_id;
    let now_ft = ctx.now_blueos_filetime_ticks();

    ctx.db.with_snapshot(|snap| {
        let Some(ch) = snap.characters.get(&char_id) else {
            return Err(err("CharacterNotFound", format!("Character {char_id} not found")));
        };
        let qty_i32 = i32::try_from(quantity)
            .map_err(|_| err("InvalidSkillPointAmount", "Quantity out of range"))?;
        let item = validate_injector_for_char(
            ctx,
            snap,
            char_id,
            current_dock,
            current_ship,
            item_id,
            qty_i32,
        )?;
        if is_daily_alpha_injector(item.type_id) {
            if ch.clone_grade > 1 {
                return Err(err(
                    "AlphaCloneRequired",
                    "Daily Alpha Injectors can only be used by Alpha clone characters",
                ));
            }
            if quantity != 1 {
                return Err(err(
                    "DailyAlphaInjectorCooldownActive",
                    "Only one Daily Alpha Injector may be used per 24 hours",
                ));
            }
            let cooldowns = load_alpha_injector_cooldowns(snap);
            if let Some(&last_ft) = cooldowns.get(&char_id) {
                if now_ft < last_ft + ALPHA_INJECTOR_COOLDOWN_TICKS {
                    return Err(err(
                        "DailyAlphaInjectorCooldownActive",
                        "Only one Daily Alpha Injector may be used per 24 hours",
                    ));
                }
            }
        }
        Ok(PyRep::None)
    })
}

fn handle_split_skill_injector(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let item_id = args
        .first()
        .and_then(|a| a.as_i64())
        .filter(|&id| id > 0)
        .ok_or_else(|| err("InvalidArgument", "SplitSkillInjector requires itemID"))?;
    let quantity = args.get(1).and_then(|a| a.as_i64()).unwrap_or(1);
    let qty_i32 = i32::try_from(quantity)
        .ok()
        .filter(|&q| q > 0)
        .ok_or_else(|| err("InvalidSkillPointAmount", "Split quantity must be positive"))?;
    let small_to_create = qty_i32
        .checked_mul(5)
        .ok_or_else(|| err("InvalidSkillPointAmount", "Split quantity overflow"))?;
    let current_dock = session.station_id.or(session.structure_id);
    let current_ship = session.ship_id;

    ctx.db.try_with_mut_snapshot(|snap| {
        let item = validate_injector_for_char(
            ctx,
            snap,
            char_id,
            current_dock,
            current_ship,
            item_id,
            qty_i32,
        )?;
        if item.type_id != TYPE_LARGE_SKILL_INJECTOR {
            return Err(err(
                "InvalidSkillInjector",
                "Only Large Skill Injectors can be split into Small Skill Injectors",
            ));
        }
        let loc_id = item.location_id;
        let flag = item.flag;
        snap.consume_exact_item_quantity(
            item_id,
            qty_i32,
            eve_db::ConsumeExactOptions {
                active_ship_id: current_ship,
                ..eve_db::ConsumeExactOptions::for_owner_exact(Some(char_id), char_id)
            },
        )
        .map_err(|e| err("ItemLocked", e.to_string()))?;
        if let Some(existing_small) = snap.items.values_mut().find(|it| {
            it.owner_id == char_id
                && it.location_id == loc_id
                && it.flag == flag
                && !it.singleton
                && it.type_id == TYPE_SMALL_SKILL_INJECTOR
        }) {
            existing_small.quantity += small_to_create;
            existing_small.version += 1;
        } else {
            snap.next_entity_id += 1;
            let new_id = snap.next_entity_id;
            snap.items.insert(
                new_id,
                InventoryItemRecord {
                    item_id: new_id,
                    type_id: TYPE_SMALL_SKILL_INJECTOR,
                    owner_id: char_id,
                    location_id: loc_id,
                    flag,
                    quantity: small_to_create,
                    singleton: false,
                    ..Default::default()
                },
            );
        }
        Ok(PyRep::None)
    })
}

fn handle_combine_skill_injector(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let item_id = args
        .first()
        .and_then(|a| a.as_i64())
        .filter(|&id| id > 0)
        .ok_or_else(|| err("InvalidArgument", "CombineSkillInjector requires itemID"))?;
    let quantity = args.get(1).and_then(|a| a.as_i64()).unwrap_or(5);
    if quantity < 5 {
        return Err(err(
            "CombineSkillInjectorTooFewInjectors",
            "At least 5 Small Skill Injectors are required to combine into a Large Skill Injector",
        ));
    }
    let qty_i32 = i32::try_from(quantity)
        .map_err(|_| err("InvalidSkillPointAmount", "Quantity out of range"))?;
    let large_to_create = qty_i32 / 5;
    let small_to_consume = large_to_create * 5;
    let current_dock = session.station_id.or(session.structure_id);
    let current_ship = session.ship_id;

    ctx.db.try_with_mut_snapshot(|snap| {
        let item = validate_injector_for_char(
            ctx,
            snap,
            char_id,
            current_dock,
            current_ship,
            item_id,
            small_to_consume,
        )?;
        if item.type_id != TYPE_SMALL_SKILL_INJECTOR {
            return Err(err(
                "InvalidSkillInjector",
                "Only Small Skill Injectors can be combined into Large Skill Injectors",
            ));
        }
        let loc_id = item.location_id;
        let flag = item.flag;
        snap.consume_exact_item_quantity(
            item_id,
            small_to_consume,
            eve_db::ConsumeExactOptions {
                active_ship_id: current_ship,
                ..eve_db::ConsumeExactOptions::for_owner_exact(Some(char_id), char_id)
            },
        )
        .map_err(|e| err("ItemLocked", e.to_string()))?;
        if let Some(existing_large) = snap.items.values_mut().find(|it| {
            it.owner_id == char_id
                && it.location_id == loc_id
                && it.flag == flag
                && !it.singleton
                && it.type_id == TYPE_LARGE_SKILL_INJECTOR
        }) {
            existing_large.quantity += large_to_create;
            existing_large.version += 1;
        } else {
            snap.next_entity_id += 1;
            let new_id = snap.next_entity_id;
            snap.items.insert(
                new_id,
                InventoryItemRecord {
                    item_id: new_id,
                    type_id: TYPE_LARGE_SKILL_INJECTOR,
                    owner_id: char_id,
                    location_id: loc_id,
                    flag,
                    quantity: large_to_create,
                    singleton: false,
                    ..Default::default()
                },
            );
        }
        Ok(PyRep::None)
    })
}

fn handle_get_diminished_sp_from_injectors(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    catch_up_skill_queue(ctx, char_id);
    let type_id = args
        .first()
        .and_then(|a| a.as_i64())
        .filter(|&v| v > 0)
        .map(|v| v as TypeId)
        .unwrap_or(TYPE_LARGE_SKILL_INJECTOR);
    let quantity = args.get(1).and_then(|a| a.as_i64()).unwrap_or(1).max(0);
    let non_dim_remaining = args.get(2).and_then(|a| a.as_i64()).unwrap_or(0).max(0);

    let char_total_sp = ctx.db.with_snapshot(|snap| {
        snap.characters
            .get(&char_id)
            .map(|c| c.skill_points + c.free_skill_points)
            .unwrap_or(0.0)
    });

    if quantity == 0 {
        return Ok(PyRep::Integer(0));
    }

    let total_yield = if let Some(fixed_sp) = non_diminishing_sp_yield(&ctx.sde, type_id) {
        fixed_sp * (quantity as f64)
    } else {
        let is_small = type_id == TYPE_SMALL_SKILL_INJECTOR;
        let mut running_sp = char_total_sp;
        let mut gained = 0.0;
        let mut rem_non_dim = non_dim_remaining;
        for _ in 0..quantity {
            let base = if rem_non_dim > 0 {
                rem_non_dim -= 1;
                500_000.0
            } else {
                skill_injector_sp_yield(running_sp)
            };
            let per_inj = if is_small { base / 5.0 } else { base };
            gained += per_inj;
            running_sp += per_inj;
        }
        gained
    };

    Ok(PyRep::Integer(total_yield.round() as i64))
}

fn total_trained_sp_in_snap(snap: &WorldStateSnapshot, char_id: i64) -> f64 {
    if let Some(skills) = snap.skills.get(&char_id).filter(|m| !m.is_empty()) {
        let sum: f64 = skills.values().map(|s| s.skill_points).sum();
        return sum.max(
            snap.characters
                .get(&char_id)
                .map(|c| c.skill_points)
                .unwrap_or(0.0),
        );
    }
    snap.characters
        .get(&char_id)
        .map(|c| c.skill_points)
        .unwrap_or(0.0)
}

fn handle_get_estimated_skill_points(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    catch_up_skill_queue(ctx, char_id);
    let tid = args
        .first()
        .and_then(|a| a.as_i64())
        .filter(|&v| v > 0)
        .map(|v| v as TypeId);
    let sp = ctx.db.with_snapshot(|snap| match tid {
        Some(skill_tid) => snap
            .skills
            .get(&char_id)
            .and_then(|m| m.get(&skill_tid))
            .map(|s| s.skill_points)
            .unwrap_or(0.0),
        None => total_trained_sp_in_snap(snap, char_id),
    });
    Ok(PyRep::Real(sp))
}

fn handle_get_skill_points(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    catch_up_skill_queue(ctx, char_id);
    let tid = args
        .first()
        .and_then(|a| a.as_i64())
        .filter(|&v| v > 0)
        .map(|v| v as TypeId);
    let sp = ctx.db.with_snapshot(|snap| match tid {
        Some(skill_tid) => snap
            .skills
            .get(&char_id)
            .and_then(|m| m.get(&skill_tid))
            .map(|s| s.skill_points)
            .unwrap_or(0.0),
        None => total_trained_sp_in_snap(snap, char_id),
    });
    Ok(PyRep::Real(sp))
}

fn handle_get_skill_level(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    catch_up_skill_queue(ctx, char_id);
    let tid = args
        .first()
        .and_then(|a| a.as_i64())
        .filter(|&v| v > 0)
        .map(|v| v as TypeId);
    let lvl = ctx.db.with_snapshot(|snap| {
        tid.and_then(|skill_tid| snap.skills.get(&char_id).and_then(|m| m.get(&skill_tid)))
            .map(|s| s.level as i64)
            .unwrap_or(0)
    });
    Ok(PyRep::Integer(lvl))
}

fn skill_bundle_catalog(ctx: &ServerContext) -> &'static HashMap<i64, String> {
    static BUNDLE_INFO: std::sync::OnceLock<HashMap<i64, String>> = std::sync::OnceLock::new();
    BUNDLE_INFO.get_or_init(|| {
        let mut map = HashMap::new();
        if let Ok(cfsd) = ctx.sde.load_cfsd_nested("res:/staticdata/crates.fsdbinary") {
            for (_k, v) in cfsd {
                let Some(bundle_id) = v.get("skillBundleID").and_then(|b| b.as_i64()) else {
                    continue;
                };
                let label = v
                    .get("typeID")
                    .and_then(|t| t.as_u64())
                    .and_then(|tid| ctx.sde.types.get(&(tid as TypeId)))
                    .map(|t| t.name.clone())
                    .unwrap_or_else(|| format!("Skill Bundle #{bundle_id}"));
                map.entry(bundle_id).or_insert(label);
            }
        }
        map
    })
}

fn handle_get_skill_bundle_info(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, _char_id) = ServerContext::require_character(session)?;
    let bundle_id = args
        .first()
        .and_then(|a| a.as_i64())
        .filter(|&b| b > 0)
        .ok_or_else(|| err("InvalidSkillBundle", "Valid skillBundleID is required"))?;
    let name = skill_bundle_catalog(ctx)
        .get(&bundle_id)
        .cloned()
        .ok_or_else(|| {
            err(
                "InvalidSkillBundle",
                format!("Skill bundle {bundle_id} not found"),
            )
        })?;
    // `SkillBundleLootItem.__init__` unpacks `(name, icon)` where `icon = None` falls back to `ICON_PATH`.
    Ok(PyRep::Tuple(vec![PyRep::String(name), PyRep::None]))
}

#[cfg(test)]
#[path = "character_skills_helpers_edges_tests.rs"]
mod character_skills_helpers_edges_tests;
