//! Unified 10-variant trigger evaluation, occurrence latching (`I3`), read-pure
//! preflight (`PVE-212`), and 1 Hz subwarp/timer/damage ticks for [`super::pve_dungeon_runtime`] (`#1495` Track 2a-3).

#![forbid(unsafe_code)]

use super::actions::{
    complete_and_flush_instance, finalize_terminal_in_snap, finish_instance_entities_in_snap,
    finish_room_clear_checks_in_inst, flush_trigger_effects, flush_trigger_effects_sync,
    merge_effects, run_trigger_batch_in_inst, DungeonRuntimeDeps, TriggerBatch, TriggerTxEffects,
};
use super::{
    dist3, has_dungeon_runtime_store_db, has_unlooted_containers_in_snap, instance_is_finished,
    record_room_actor, resolve_room_recipient, room_contains_pocket_pos, room_is_entered,
    POCKET_RADIUS_M,
};
use crate::services::combat_crimewatch::with_combat_runtime_with_locks;
use eve_db::service_domains::pve_sites::{
    DungeonRuntimeInstance, DungeonRuntimeRoom, DungeonTriggerAction, DungeonTriggerKind,
};
use eve_db::WorldStateSnapshot;
use eve_sde::{SolarSystemId, TypeId};
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Clone, Debug)]
pub(crate) enum DungeonEvent<'a> {
    RoomEntered {
        room_vec_idx: usize,
    },
    NpcKilled {
        npc_ball_id: i64,
        group_id: Option<u8>,
        npc_tag: Option<&'a str>,
        npc_type_id: u32,
        retroactive_room_check: bool,
    },
    NpcDamaged {
        npc_ball_id: i64,
        group_id: Option<u8>,
        npc_tag: Option<&'a str>,
        total_pct: u8,
        shield_pct: Option<u8>,
        hull_pct: u8,
        shield_or_armor_depleted: bool,
        damaged_at_all: bool,
    },
    Approach {
        ship_id: i64,
        char_id: i64,
        ship_pos: [f64; 3],
        target_id_opt: Option<i64>,
    },
    Loot {
        container_ball_id: i64,
        item_type_id: TypeId,
    },
    TimerTick,
}

enum LeafOccurrence {
    Irrelevant,
    Inactive { subject: String, sticky: bool },
    Active { subject: String, discrete_event: bool },
}

fn kind_matches_leaf(kind: &DungeonTriggerKind, pred: fn(&DungeonTriggerKind) -> bool) -> bool {
    if pred(kind) {
        return true;
    }
    match kind {
        DungeonTriggerKind::CompoundAnd { clauses } | DungeonTriggerKind::CompoundOr { clauses } => {
            clauses.iter().any(|c| kind_matches_leaf(c, pred))
        }
        _ => false,
    }
}

fn kind_mentions_approach(k: &DungeonTriggerKind) -> bool {
    kind_matches_leaf(k, |x| matches!(x, DungeonTriggerKind::OnApproach { .. }))
}
fn kind_mentions_loot(k: &DungeonTriggerKind) -> bool {
    kind_matches_leaf(k, |x| matches!(x, DungeonTriggerKind::OnLoot { .. }))
}
fn kind_needs_damage_poll(k: &DungeonTriggerKind) -> bool {
    kind_matches_leaf(k, |x| matches!(x, DungeonTriggerKind::OnNpcDamaged { .. }))
}
fn kind_needs_timer_poll(k: &DungeonTriggerKind) -> bool {
    kind_matches_leaf(k, |x| matches!(x, DungeonTriggerKind::OnTimer { .. }))
}

#[allow(clippy::too_many_arguments)]
fn collect_newly_latched_due_named_timers(
    kind: &DungeonTriggerKind,
    prev_armed: &BTreeMap<String, u64>,
    curr_armed: &BTreeMap<String, u64>,
    r_idx: usize,
    t_idx: usize,
    path: &str,
    now_unix: u64,
    out_expired: &mut Vec<String>,
) {
    match kind {
        DungeonTriggerKind::OnTimer { timer_id: Some(ref tid), .. } => {
            if !tid.is_empty() && !tid.starts_with("__") {
                let latch_key = format!("__latch:{r_idx}:{t_idx}:{path}:timer:{tid}");
                if !prev_armed.contains_key(&latch_key)
                    && curr_armed.contains_key(&latch_key)
                    && curr_armed.get(tid).is_some_and(|&due| now_unix >= due)
                    && !out_expired.contains(tid)
                {
                    out_expired.push(tid.clone());
                }
            }
        }
        DungeonTriggerKind::CompoundAnd { clauses } | DungeonTriggerKind::CompoundOr { clauses } => {
            for (ci, c) in clauses.iter().enumerate() {
                let sub = format!("{path}.{ci}");
                collect_newly_latched_due_named_timers(
                    c, prev_armed, curr_armed, r_idx, t_idx, &sub, now_unix, out_expired,
                );
            }
        }
        _ => {}
    }
}

/// Resets repeatable trigger latches ONLY for clauses whose occurrence/timer actually fired (`OWN-R5-04`, `BF-R4-01`).
/// Never consumes named `timer_id` entries here: `collect_newly_latched_due_named_timers` handles
/// named-timer expiration strictly when a leaf transitions from unlatched to latched on `TimerTick`.
fn reset_repeatable_trigger_state(
    kind: &DungeonTriggerKind,
    armed_timers: &mut BTreeMap<String, u64>,
    room_entered_unix: Option<u64>,
    r_idx: usize,
    t_idx: usize,
    path: &str,
    now_unix: u64,
) {
    if path == "root" {
        let and_pfx = format!("__and:{r_idx}:{t_idx}:");
        let tag_cnt_pfx = format!("__kill_tagged_cnt:{r_idx}:{t_idx}:");
        let grp_cnt_pfx = format!("__kill_cnt:{r_idx}:{t_idx}:");
        armed_timers.retain(|k, _| {
            !k.starts_with(&and_pfx) && !k.starts_with(&tag_cnt_pfx) && !k.starts_with(&grp_cnt_pfx)
        });
    }
    match kind {
        DungeonTriggerKind::OnTimer { delay_secs, timer_id, after_group_cleared } => {
            if let Some(tid) = timer_id {
                if tid.is_empty() || tid.starts_with("__") {
                    return;
                }
                armed_timers.remove(&format!("__latch:{r_idx}:{t_idx}:{path}:timer:{tid}"));
            } else if after_group_cleared.is_none() {
                let unnamed_latch = format!("__latch:{r_idx}:{t_idx}:{path}:unnamed_timer");
                if armed_timers.contains_key(&unnamed_latch) {
                    let rep_key = format!("__rep_timer:{r_idx}:{t_idx}:{path}");
                    let due = armed_timers
                        .get(&rep_key)
                        .copied()
                        .or_else(|| room_entered_unix.map(|e| e.saturating_add(*delay_secs)));
                    if due.is_some_and(|d| now_unix >= d) {
                        armed_timers.insert(rep_key, now_unix.saturating_add((*delay_secs).max(1)));
                        armed_timers.remove(&unnamed_latch);
                    }
                }
            }
        }
        DungeonTriggerKind::CompoundAnd { clauses } | DungeonTriggerKind::CompoundOr { clauses } => {
            for (ci, clause) in clauses.iter().enumerate() {
                let sub = format!("{path}.{ci}");
                reset_repeatable_trigger_state(
                    clause,
                    armed_timers,
                    room_entered_unix,
                    r_idx,
                    t_idx,
                    &sub,
                    now_unix,
                );
            }
        }
        _ => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn eval_leaf_outcome(
    clause: &DungeonTriggerKind,
    inst: &DungeonRuntimeInstance,
    room: &DungeonRuntimeRoom,
    armed_timers: &mut BTreeMap<String, u64>,
    room_vec_idx: usize,
    trig_idx: usize,
    path: &str,
    event: &DungeonEvent<'_>,
    now_unix: u64,
) -> LeafOccurrence {
    match clause {
        DungeonTriggerKind::OnKill { target_npc_group, target_ball_id } => match event {
            DungeonEvent::NpcKilled { npc_ball_id, group_id: ev_gid, retroactive_room_check, .. } => {
                if let Some(tid) = target_ball_id {
                    let hit = *tid == *npc_ball_id
                        || (*retroactive_room_check && room.npc_groups.iter().any(|g| g.dead_npc_ball_ids.contains(tid)));
                    if hit {
                        LeafOccurrence::Active { subject: format!("ball:{tid}"), discrete_event: false }
                    } else {
                        LeafOccurrence::Irrelevant
                    }
                } else if let Some(gid) = target_npc_group {
                    if *ev_gid != Some(*gid) && !*retroactive_room_check {
                        return LeafOccurrence::Irrelevant;
                    }
                    let cleared = room.npc_groups.iter().any(|g| {
                        g.group_id == *gid && g.spawned && g.cleared && g.npc_ball_ids.is_empty()
                    });
                    let subject = format!("grp:{gid}");
                    if cleared {
                        LeafOccurrence::Active { subject, discrete_event: false }
                    } else {
                        LeafOccurrence::Inactive { subject, sticky: false }
                    }
                } else {
                    let all_dead = room.npc_groups.iter().filter(|g| g.spawned).all(|g| g.npc_ball_ids.is_empty());
                    let all_spawned = !room.npc_groups.is_empty() && room.npc_groups.iter().all(|g| g.spawned);
                    if all_dead && all_spawned {
                        LeafOccurrence::Active { subject: "room".into(), discrete_event: false }
                    } else {
                        LeafOccurrence::Inactive { subject: "room".into(), sticky: false }
                    }
                }
            }
            DungeonEvent::TimerTick => {
                if let Some(gid) = target_npc_group {
                    if room.npc_groups.iter().any(|g| g.group_id == *gid && !g.cleared) {
                        LeafOccurrence::Inactive { subject: format!("grp:{gid}"), sticky: false }
                    } else {
                        LeafOccurrence::Irrelevant
                    }
                } else if target_ball_id.is_none()
                    && !room.cleared
                    && !room.npc_groups.is_empty()
                    && !room.npc_groups.iter().all(|g| g.spawned && g.cleared && g.npc_ball_ids.is_empty())
                {
                    LeafOccurrence::Inactive { subject: "room".into(), sticky: false }
                } else {
                    LeafOccurrence::Irrelevant
                }
            }
            _ => LeafOccurrence::Irrelevant,
        },
        DungeonTriggerKind::OnKillTagged { target_tag, required_kills, type_ids } => match event {
            DungeonEvent::NpcKilled { npc_ball_id, npc_tag, npc_type_id, .. } if *npc_ball_id > 0 => {
                let tag_ok = target_tag.is_empty() || npc_tag.is_some_and(|t| t.eq_ignore_ascii_case(target_tag));
                let type_ok = type_ids.is_empty() || type_ids.contains(npc_type_id);
                if !(tag_ok && type_ok) {
                    return LeafOccurrence::Irrelevant;
                }
                let seen_key = format!("__kill_seen:{room_vec_idx}:{trig_idx}:{path}:{npc_ball_id}");
                if armed_timers.contains_key(&seen_key) {
                    return LeafOccurrence::Irrelevant;
                }
                armed_timers.insert(seen_key, now_unix);
                let cnt_key = format!("__kill_tagged_cnt:{room_vec_idx}:{trig_idx}:{path}");
                let cnt = {
                    let entry = armed_timers.entry(cnt_key).or_insert(0);
                    *entry = entry.saturating_add(1);
                    *entry
                };
                let need = u64::from((*required_kills).unwrap_or(1).max(1));
                if cnt >= need {
                    LeafOccurrence::Active { subject: format!("tag:{npc_ball_id}"), discrete_event: true }
                } else {
                    LeafOccurrence::Inactive { subject: "tag".into(), sticky: false }
                }
            }
            _ => LeafOccurrence::Irrelevant,
        },
        DungeonTriggerKind::OnKillCount { target_npc_group, required_kills } => match event {
            DungeonEvent::NpcKilled { npc_ball_id, group_id: ev_gid, .. } if *npc_ball_id > 0 => {
                if !target_npc_group.is_none_or(|tg| Some(tg) == *ev_gid) {
                    return LeafOccurrence::Irrelevant;
                }
                let seen_key = format!("__kill_seen:{room_vec_idx}:{trig_idx}:{path}:{npc_ball_id}");
                if armed_timers.contains_key(&seen_key) {
                    return LeafOccurrence::Irrelevant;
                }
                armed_timers.insert(seen_key, now_unix);
                let cnt_key = format!("__kill_cnt:{room_vec_idx}:{trig_idx}:{path}");
                let cnt = {
                    let entry = armed_timers.entry(cnt_key).or_insert(0);
                    *entry = entry.saturating_add(1);
                    *entry
                };
                if cnt >= u64::from((*required_kills).max(1)) {
                    LeafOccurrence::Active { subject: format!("cnt:{npc_ball_id}"), discrete_event: true }
                } else {
                    LeafOccurrence::Inactive { subject: "cnt".into(), sticky: false }
                }
            }
            _ => LeafOccurrence::Irrelevant,
        },
        DungeonTriggerKind::OnApproach { target_ball_id, target_object_index, anchor_pos, radius_m } => {
            let DungeonEvent::Approach { ship_id, char_id, ship_pos, target_id_opt } = event else {
                return LeafOccurrence::Irrelevant;
            };
            let subject = if *ship_id > 0 {
                format!("ship:{ship_id}")
            } else if *char_id > 0 {
                format!("char:{char_id}")
            } else {
                return LeafOccurrence::Irrelevant;
            };
            let (resolved_target_ball, pos_arr) = if let Some(bid) = *target_ball_id {
                if bid <= 0 {
                    return LeafOccurrence::Irrelevant;
                }
                if let Some(obj) = room.objects.iter().find(|o| o.ball_id == bid) {
                    if !obj.spawned || obj.destroyed {
                        return LeafOccurrence::Irrelevant;
                    }
                    (Some(bid), obj.position)
                } else {
                    (Some(bid), *anchor_pos)
                }
            } else if let Some(idx) = *target_object_index {
                let Some(obj) = room.objects.get(idx) else {
                    return LeafOccurrence::Irrelevant;
                };
                if !obj.spawned || obj.destroyed || obj.ball_id <= 0 {
                    return LeafOccurrence::Irrelevant;
                }
                (Some(obj.ball_id), obj.position)
            } else {
                (None, *anchor_pos)
            };
            let effective_radius = radius_m.max(2_500.0);
            let dist = dist3(*ship_pos, pos_arr);
            let in_pocket_or_near = room_contains_pocket_pos(inst, room_vec_idx, room, *ship_pos, POCKET_RADIUS_M)
                || dist <= effective_radius.max(5_000.0);
            let latch_key = format!("__latch:{room_vec_idx}:{trig_idx}:{path}:{subject}");
            if !in_pocket_or_near && !armed_timers.contains_key(&latch_key) {
                return LeafOccurrence::Irrelevant;
            }
            let explicit_target_close = target_id_opt.is_some()
                && *target_id_opt == resolved_target_ball
                && dist <= effective_radius.max(5_000.0);
            if dist <= effective_radius || explicit_target_close {
                LeafOccurrence::Active { subject, discrete_event: false }
            } else {
                LeafOccurrence::Inactive { subject, sticky: false }
            }
        }
        DungeonTriggerKind::OnLoot { container_ball_id, target_object_index, item_type_id } => {
            let DungeonEvent::Loot { container_ball_id: cont_id, item_type_id: looted_tid } = event else {
                return LeafOccurrence::Irrelevant;
            };
            if *cont_id <= 0 {
                return LeafOccurrence::Irrelevant;
            }
            let resolved_cont = if let Some(cid) = *container_ball_id {
                if cid <= 0 || room.objects.iter().any(|o| o.ball_id == cid && (!o.spawned || o.destroyed)) {
                    return LeafOccurrence::Irrelevant;
                }
                Some(cid)
            } else if let Some(idx) = *target_object_index {
                let Some(obj) = room.objects.get(idx) else {
                    return LeafOccurrence::Irrelevant;
                };
                if !obj.spawned || obj.destroyed || obj.ball_id <= 0 {
                    return LeafOccurrence::Irrelevant;
                }
                Some(obj.ball_id)
            } else {
                None
            };
            let cont_ok = resolved_cont.is_none_or(|cid| cid == *cont_id);
            let tid_ok = item_type_id.is_none_or(|et| et == *looted_tid);
            let room_has_cont = room.objects.iter().any(|o| o.ball_id == *cont_id && o.spawned && !o.destroyed);
            if cont_ok && tid_ok && (resolved_cont.is_some() || room_has_cont) {
                LeafOccurrence::Active { subject: format!("loot:{cont_id}:{looted_tid}"), discrete_event: true }
            } else {
                LeafOccurrence::Irrelevant
            }
        }
        DungeonTriggerKind::OnTimer { delay_secs, timer_id, after_group_cleared } => {
            if !matches!(event, DungeonEvent::TimerTick) {
                return LeafOccurrence::Irrelevant;
            }
            if let Some(tid) = timer_id {
                if tid.is_empty() || tid.starts_with("__") {
                    return LeafOccurrence::Irrelevant;
                }
                let subject = format!("timer:{tid}");
                if armed_timers.get(tid).is_some_and(|&due| now_unix >= due) {
                    LeafOccurrence::Active { subject, discrete_event: false }
                } else {
                    LeafOccurrence::Inactive { subject, sticky: false }
                }
            } else if let Some(gid) = after_group_cleared {
                let subject = format!("after_grp:{gid}");
                let Some(entered_unix) = room.entered_unix else {
                    return LeafOccurrence::Irrelevant;
                };
                let cleared = room.npc_groups.iter().any(|g| {
                    g.group_id == *gid && g.spawned && g.cleared && g.npc_ball_ids.is_empty()
                });
                if !cleared {
                    LeafOccurrence::Inactive { subject, sticky: false }
                } else {
                    let cleared_at = armed_timers
                        .get(&format!("__group_cleared:{gid}"))
                        .or_else(|| armed_timers.get(&format!("group_cleared:{gid}")))
                        .copied()
                        .unwrap_or(entered_unix)
                        .max(entered_unix);
                    if now_unix >= cleared_at.saturating_add(*delay_secs) {
                        LeafOccurrence::Active { subject, discrete_event: false }
                    } else {
                        LeafOccurrence::Inactive { subject, sticky: false }
                    }
                }
            } else {
                let Some(entered_unix) = room.entered_unix else {
                    return LeafOccurrence::Irrelevant;
                };
                let rep_key = format!("__rep_timer:{room_vec_idx}:{trig_idx}:{path}");
                let due = armed_timers
                    .get(&rep_key)
                    .copied()
                    .unwrap_or_else(|| entered_unix.saturating_add(*delay_secs));
                if now_unix >= due {
                    LeafOccurrence::Active { subject: "unnamed_timer".into(), discrete_event: false }
                } else {
                    LeafOccurrence::Inactive { subject: "unnamed_timer".into(), sticky: false }
                }
            }
        }
        DungeonTriggerKind::OnRoomEntered { target_room_index } => match event {
            DungeonEvent::RoomEntered { room_vec_idx: ev_ri } => {
                if target_room_index.unwrap_or(room_vec_idx) == *ev_ri {
                    LeafOccurrence::Active { subject: format!("enter:{ev_ri}"), discrete_event: true }
                } else {
                    LeafOccurrence::Irrelevant
                }
            }
            _ => LeafOccurrence::Irrelevant,
        },
        DungeonTriggerKind::OnNpcDamaged {
            target_npc_group,
            target_tag,
            target_ball_id,
            hull_or_shield_below_pct,
            on_first_attack,
        } => {
            let DungeonEvent::NpcDamaged {
                npc_ball_id,
                group_id,
                npc_tag,
                total_pct,
                shield_pct,
                hull_pct,
                shield_or_armor_depleted,
                damaged_at_all,
            } = event
            else {
                return LeafOccurrence::Irrelevant;
            };
            let id_ok = target_ball_id.is_none_or(|bid| bid == *npc_ball_id);
            let grp_ok = target_npc_group.is_none_or(|gid| Some(gid) == *group_id);
            let tag_ok = target_tag.as_deref().is_none_or(|t| npc_tag.is_some_and(|nt| nt.eq_ignore_ascii_case(t)));
            if !(id_ok && grp_ok && tag_ok) {
                return LeafOccurrence::Irrelevant;
            }
            let cond = if *on_first_attack {
                *damaged_at_all
            } else {
                *damaged_at_all
                    && (*total_pct <= *hull_or_shield_below_pct
                        || shield_pct.is_some_and(|sp| sp <= *hull_or_shield_below_pct)
                        || (*shield_or_armor_depleted && *hull_pct <= *hull_or_shield_below_pct))
            };
            let subject = format!("npc:{npc_ball_id}");
            if cond {
                LeafOccurrence::Active { subject, discrete_event: false }
            } else {
                LeafOccurrence::Inactive { subject, sticky: *on_first_attack }
            }
        }
        DungeonTriggerKind::CompoundAnd { .. } | DungeonTriggerKind::CompoundOr { .. } => {
            LeafOccurrence::Irrelevant
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn evaluate_leaf_clause_with_latch(
    clause: &DungeonTriggerKind,
    inst: &DungeonRuntimeInstance,
    room: &DungeonRuntimeRoom,
    armed_timers: &mut BTreeMap<String, u64>,
    room_vec_idx: usize,
    trig_idx: usize,
    path: &str,
    event: &DungeonEvent<'_>,
    now_unix: u64,
) -> bool {
    match eval_leaf_outcome(clause, inst, room, armed_timers, room_vec_idx, trig_idx, path, event, now_unix) {
        LeafOccurrence::Irrelevant => false,
        LeafOccurrence::Inactive { subject, sticky } => {
            if !sticky {
                armed_timers.remove(&format!("__latch:{room_vec_idx}:{trig_idx}:{path}:{subject}"));
            }
            false
        }
        LeafOccurrence::Active { subject, discrete_event } => {
            if discrete_event {
                true
            } else {
                let latch_key = format!("__latch:{room_vec_idx}:{trig_idx}:{path}:{subject}");
                if armed_timers.contains_key(&latch_key) {
                    false
                } else {
                    armed_timers.insert(latch_key, now_unix);
                    true
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn evaluate_trigger_with_occurrence_latch(
    kind: &DungeonTriggerKind,
    inst: &DungeonRuntimeInstance,
    room: &DungeonRuntimeRoom,
    armed_timers: &mut BTreeMap<String, u64>,
    room_vec_idx: usize,
    trig_idx: usize,
    path: &str,
    event: &DungeonEvent<'_>,
    now_unix: u64,
    already_satisfied: bool,
) -> bool {
    match kind {
        DungeonTriggerKind::CompoundAnd { clauses } => {
            if clauses.is_empty() {
                return false;
            }
            let mut all_ok = true;
            for (ci, clause) in clauses.iter().enumerate() {
                let sub_path = format!("{path}.{ci}");
                let key = format!("__and:{room_vec_idx}:{trig_idx}:{sub_path}");
                let already = armed_timers.contains_key(&key);
                let ok_now = evaluate_trigger_with_occurrence_latch(
                    clause,
                    inst,
                    room,
                    armed_timers,
                    room_vec_idx,
                    trig_idx,
                    &sub_path,
                    event,
                    now_unix,
                    already_satisfied || already,
                );
                if !already_satisfied && !already {
                    if ok_now {
                        armed_timers.insert(key, now_unix);
                    } else {
                        all_ok = false;
                    }
                }
            }
            all_ok
        }
        DungeonTriggerKind::CompoundOr { clauses } => {
            let mut any_ok = false;
            for (ci, c) in clauses.iter().enumerate() {
                let sub_path = format!("{path}.{ci}");
                if evaluate_trigger_with_occurrence_latch(
                    c,
                    inst,
                    room,
                    armed_timers,
                    room_vec_idx,
                    trig_idx,
                    &sub_path,
                    event,
                    now_unix,
                    already_satisfied,
                ) {
                    any_ok = true;
                }
            }
            any_ok
        }
        leaf => {
            if already_satisfied
                && matches!(
                    leaf,
                    DungeonTriggerKind::OnTimer { .. }
                        | DungeonTriggerKind::OnKillTagged { .. }
                        | DungeonTriggerKind::OnKillCount { .. }
                        | DungeonTriggerKind::OnRoomEntered { .. }
                        | DungeonTriggerKind::OnLoot { .. }
                )
            {
                return true;
            }
            evaluate_leaf_clause_with_latch(
                leaf, inst, room, armed_timers, room_vec_idx, trig_idx, path, event, now_unix,
            )
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn evaluate_room_triggers_in_inst(
    snap: &mut WorldStateSnapshot,
    sys_id: SolarSystemId,
    inst: &mut DungeonRuntimeInstance,
    sde_opt: Option<&eve_sde::SdeRegistry>,
    r_idx: usize,
    event: &DungeonEvent<'_>,
    actor_char_id: i64,
    now_unix: u64,
    allow_empty_groups_clear: bool,
    run_clear_check_even_if_empty: bool,
    effects: &mut TriggerTxEffects,
) -> usize {
    if r_idx >= inst.rooms.len() || !room_is_entered(&inst.rooms[r_idx]) {
        return 0;
    }
    let is_timer_tick = matches!(event, DungeonEvent::TimerTick);
    let mut batch: TriggerBatch = Vec::new();
    let mut expired_named_timers: Vec<String> = Vec::new();
    let mut armed_count = 0usize;
    let mut any_hit = false;
    let trig_len = inst.rooms[r_idx].triggers.len();
    for t_idx in 0..trig_len {
        let (fired, delay, repeatable, armed_at, armed_by, trig_id, target_room_index, kind) = {
            let t = &inst.rooms[r_idx].triggers[t_idx];
            (
                t.fired,
                t.delay_secs,
                t.repeatable,
                t.armed_at_unix,
                t.armed_by_char_id,
                t.trigger_id,
                t.target_room_index,
                t.kind.clone(),
            )
        };
        if fired {
            continue;
        }
        if let Some(t0) = armed_at {
            if now_unix >= t0.saturating_add(delay) {
                let t = &mut inst.rooms[r_idx].triggers[t_idx];
                t.armed_at_unix = None;
                t.armed_by_char_id = None;
                let acts: Vec<DungeonTriggerAction> = t.all_actions().cloned().collect();
                if !repeatable {
                    t.fired = true;
                } else {
                    let entered_unix = inst.rooms[r_idx].entered_unix;
                    reset_repeatable_trigger_state(
                        &kind,
                        &mut inst.rooms[r_idx].armed_timers,
                        entered_unix,
                        r_idx,
                        t_idx,
                        "root",
                        now_unix,
                    );
                }
                let eff_trig_actor = armed_by.filter(|&c| c > 0).unwrap_or(actor_char_id);
                any_hit = true;
                batch.push((trig_id, target_room_index, repeatable, eff_trig_actor, acts));
            }
            continue;
        }
        let prev_armed = is_timer_tick.then(|| inst.rooms[r_idx].armed_timers.clone());
        let mut scratch = prev_armed.as_ref().cloned().unwrap_or_else(|| inst.rooms[r_idx].armed_timers.clone());
        let matched = evaluate_trigger_with_occurrence_latch(
            &kind, inst, &inst.rooms[r_idx], &mut scratch, r_idx, t_idx, "root", event, now_unix, false,
        );
        if let Some(ref prev) = prev_armed {
            collect_newly_latched_due_named_timers(
                &kind,
                prev,
                &scratch,
                r_idx,
                t_idx,
                "root",
                now_unix,
                &mut expired_named_timers,
            );
        }
        inst.rooms[r_idx].armed_timers = scratch;
        if !matched {
            continue;
        }
        any_hit = true;
        if delay > 0 {
            let t = &mut inst.rooms[r_idx].triggers[t_idx];
            t.armed_at_unix = Some(now_unix);
            t.armed_by_char_id = (actor_char_id > 0).then_some(actor_char_id);
            armed_count += 1;
            continue;
        }
        let acts: Vec<DungeonTriggerAction> = inst.rooms[r_idx].triggers[t_idx].all_actions().cloned().collect();
        if repeatable {
            let entered_unix = inst.rooms[r_idx].entered_unix;
            reset_repeatable_trigger_state(
                &kind,
                &mut inst.rooms[r_idx].armed_timers,
                entered_unix,
                r_idx,
                t_idx,
                "root",
                now_unix,
            );
        } else {
            inst.rooms[r_idx].triggers[t_idx].fired = true;
        }
        batch.push((trig_id, target_room_index, repeatable, actor_char_id, acts));
    }
    // OWN-R5-04 / BF-03: Clear shared expired named timers ONLY AFTER every trigger in this room has evaluated on TimerTick.
    if is_timer_tick {
        for tid in expired_named_timers {
            if inst.rooms[r_idx].armed_timers.get(&tid).is_some_and(|&due| now_unix >= due) {
                inst.rooms[r_idx].armed_timers.remove(&tid);
            }
        }
    }
    if any_hit {
        record_room_actor(inst, r_idx, actor_char_id);
    }
    let had_actions = !batch.is_empty();
    let executed = if had_actions {
        run_trigger_batch_in_inst(snap, sys_id, inst, sde_opt, r_idx, actor_char_id, now_unix, batch, effects).0
    } else {
        0
    };
    if had_actions || run_clear_check_even_if_empty {
        finish_room_clear_checks_in_inst(inst, r_idx, actor_char_id, had_actions, allow_empty_groups_clear, effects);
    }
    armed_count + executed
}

fn trigger_needs_eval_dryrun(
    kind: &DungeonTriggerKind,
    inst: &DungeonRuntimeInstance,
    room: &DungeonRuntimeRoom,
    r_idx: usize,
    t_idx: usize,
    event: &DungeonEvent<'_>,
    now_unix: u64,
) -> bool {
    let mut scratch = room.armed_timers.clone();
    let matched = evaluate_trigger_with_occurrence_latch(
        kind, inst, room, &mut scratch, r_idx, t_idx, "root", event, now_unix, false,
    );
    matched || scratch != room.armed_timers
}

pub(crate) async fn on_dungeon_npc_killed_impl(
    deps: DungeonRuntimeDeps<'_>,
    sys_id: SolarSystemId,
    npc_ball_id: i64,
    killer_char_id: i64,
) {
    if npc_ball_id <= 0 || !has_dungeon_runtime_store_db(deps.db) {
        return;
    }
    let (candidate_ids, missing_meta) = deps.db.with_snapshot(|snap| {
        let mut ids = Vec::new();
        let mut need_ce = false;
        for inst in snap.dungeon_runtime.instances.values() {
            if instance_is_finished(inst) || inst.solar_system_id != sys_id {
                continue;
            }
            for r in &inst.rooms {
                let has_mission_kill = r.armed_timers.keys().any(|k| {
                    k.starts_with("__mission_kill:")
                        && k.split(':')
                            .nth(2)
                            .and_then(|s| s.parse::<i64>().ok())
                            == Some(npc_ball_id)
                });
                if has_mission_kill {
                    ids.push(inst.instance_id);
                }
                for g in &r.npc_groups {
                    if g.npc_ball_ids.contains(&npc_ball_id) {
                        ids.push(inst.instance_id);
                        if !g.npc_ball_meta.contains_key(&npc_ball_id) {
                            need_ce = true;
                        }
                    }
                }
            }
        }
        ids.sort_unstable();
        ids.dedup();
        (ids, need_ce)
    });
    if candidate_ids.is_empty() {
        return;
    }
    let killed_ent_type_id = if missing_meta {
        deps.combat_ephemeral.and_then(|ce| {
            with_combat_runtime_with_locks(deps.db, ce, |rt| rt.npc_entities.get(&npc_ball_id).map(|e| e.type_id))
        })
    } else {
        None
    };

    let now_unix = deps.db.clock().now_unix_secs();
    let mut effects = TriggerTxEffects::default();
    deps.db.with_mut_snapshot(|snap| {
        for inst_id in candidate_ids {
            let Some(mut inst) = snap.dungeon_runtime.instances.remove(&inst_id) else { continue };
            if instance_is_finished(&inst) || inst.solar_system_id != sys_id {
                snap.dungeon_runtime.instances.insert(inst_id, inst);
                continue;
            }
            let mut ie = TriggerTxEffects::default();
            for r_idx in 0..inst.rooms.len() {
                if ie.fail_reason.is_some() || ie.complete_now {
                    break;
                }
                let mut hit_ev: Option<(Option<u8>, Option<String>, u32)> = None;
                let entered = {
                    let room = &mut inst.rooms[r_idx];
                    for grp in &mut room.npc_groups {
                        if let Some(pos) = grp.npc_ball_ids.iter().position(|&id| id == npc_ball_id) {
                            let meta = grp.npc_ball_meta.remove(&npc_ball_id);
                            let tag = grp.npc_ball_tags.remove(&npc_ball_id);
                            grp.npc_ball_names.remove(&npc_ball_id);
                            grp.npc_ball_bounties.remove(&npc_ball_id);
                            let entries = grp.effective_entries();
                            let tid = meta
                                .as_ref()
                                .map(|m| m.type_id)
                                .or(killed_ent_type_id)
                                .or_else(|| {
                                    entries
                                        .iter()
                                        .flat_map(|e| std::iter::repeat(e.type_id).take(e.count.max(1)))
                                        .nth(pos)
                                })
                                .or_else(|| grp.npc_specs.first().map(|&(t, _)| t))
                                .unwrap_or(0);
                            grp.npc_ball_ids.remove(pos);
                            if !grp.dead_npc_ball_ids.contains(&npc_ball_id) {
                                grp.dead_npc_ball_ids.push(npc_ball_id);
                            }
                            grp.kills_recorded = grp.kills_recorded.saturating_add(1);
                            if grp.npc_ball_ids.is_empty() && grp.spawned {
                                grp.cleared = true;
                                room.armed_timers.entry(format!("__group_cleared:{}", grp.group_id)).or_insert(now_unix);
                            }
                            hit_ev = Some((Some(grp.group_id), tag, tid));
                            break;
                        }
                    }
                    if hit_ev.is_none() {
                        let mk_prefix = format!("__mission_kill:{r_idx}:{npc_ball_id}:");
                        if let Some(mk_key) = room.armed_timers.keys().find(|k| k.starts_with(&mk_prefix)).cloned() {
                            room.armed_timers.remove(&mk_key);
                            let parts: Vec<&str> = mk_key.splitn(6, ':').collect();
                            if parts.len() == 6 {
                                let gid_opt = parts[3].parse::<u8>().ok();
                                let tid = parts[4].parse::<u32>().unwrap_or(0);
                                let tag_opt = (!parts[5].is_empty()).then(|| parts[5].to_string());
                                hit_ev = Some((gid_opt, tag_opt, tid));
                            }
                        }
                    }
                    room_is_entered(room)
                };
                let Some((gid_opt, tag_opt, tid)) = hit_ev else { continue };
                if !entered {
                    let gid_str = gid_opt.map_or_else(|| "none".to_string(), |g| g.to_string());
                    let tag_str = tag_opt.unwrap_or_default();
                    inst.rooms[r_idx].armed_timers.insert(
                        format!("__pre_kill:{r_idx}:{npc_ball_id}:{gid_str}:{tid}:{tag_str}"),
                        now_unix,
                    );
                    continue;
                }
                record_room_actor(&mut inst, r_idx, killer_char_id);
                let ev = DungeonEvent::NpcKilled {
                    npc_ball_id,
                    group_id: gid_opt,
                    npc_tag: tag_opt.as_deref(),
                    npc_type_id: tid,
                    retroactive_room_check: false,
                };
                let _ = evaluate_room_triggers_in_inst(
                    snap, sys_id, &mut inst, deps.sde, r_idx, &ev, killer_char_id, now_unix, true, true, &mut ie,
                );
            }
            finalize_terminal_in_snap(snap, inst, killer_char_id, now_unix, &mut ie);
            merge_effects(&mut effects, ie);
        }
    });
    flush_trigger_effects(deps, sys_id, effects).await;
}

fn prepare_on_dungeon_room_entered_in_snap(
    deps: DungeonRuntimeDeps<'_>,
    sys_id: SolarSystemId,
    instance_id: i64,
    room_vec_idx: usize,
    char_id: i64,
) -> Option<TriggerTxEffects> {
    let now_unix = deps.db.clock().now_unix_secs();
    let needs = deps.db.with_snapshot(|snap| {
        snap.dungeon_runtime.instances.get(&instance_id).is_some_and(|i| {
            !instance_is_finished(i)
                && i.solar_system_id == sys_id
                && i.rooms.get(room_vec_idx).is_some_and(|r| {
                    !r.entered || (char_id > 0 && (i.last_actor_char_id != char_id || r.last_actor_char_id != char_id))
                })
        })
    });
    if !needs {
        return None;
    }
    let mut effects = TriggerTxEffects::default();
    deps.db.with_mut_snapshot(|snap| {
        let Some(mut inst) = snap.dungeon_runtime.instances.remove(&instance_id) else { return };
        if instance_is_finished(&inst) || inst.solar_system_id != sys_id {
            snap.dungeon_runtime.instances.insert(instance_id, inst);
            return;
        }
        let Some((was_ent, has_prior_kills, pre_kills)) = inst.rooms.get_mut(room_vec_idx).map(|r| {
            let was = r.entered;
            r.entered = true;
            r.entered_unix.get_or_insert(now_unix);
            let prior = r.npc_groups.iter().any(|g| g.kills_recorded > 0 || !g.dead_npc_ball_ids.is_empty());
            let prefix = format!("__pre_kill:{room_vec_idx}:");
            let mut recorded = Vec::new();
            let keys: Vec<String> = r.armed_timers.keys().filter(|k| k.starts_with(&prefix)).cloned().collect();
            for k in keys {
                r.armed_timers.remove(&k);
                let parts: Vec<&str> = k.splitn(6, ':').collect();
                if parts.len() == 6 {
                    let gid_opt = if parts[3] == "none" {
                        Ok(None)
                    } else {
                        parts[3].parse::<u8>().map(Some)
                    };
                    if let (Ok(bid), Ok(gid), Ok(tid)) = (parts[2].parse::<i64>(), gid_opt, parts[4].parse::<u32>()) {
                        recorded.push((bid, gid, (!parts[5].is_empty()).then(|| parts[5].to_string()), tid));
                    }
                }
            }
            recorded.sort_by_key(|e| e.0);
            (was, !was && prior, recorded)
        }) else {
            snap.dungeon_runtime.instances.insert(instance_id, inst);
            return;
        };
        record_room_actor(&mut inst, room_vec_idx, char_id);
        if !was_ent {
            let enter_ev = DungeonEvent::RoomEntered { room_vec_idx };
            let total_rooms = inst.rooms.len();
            for eval_idx in 0..total_rooms {
                if effects.fail_reason.is_some() || effects.complete_now {
                    break;
                }
                let _ = evaluate_room_triggers_in_inst(
                    snap, sys_id, &mut inst, deps.sde, eval_idx, &enter_ev, char_id, now_unix, false, false, &mut effects,
                );
            }
            if !pre_kills.is_empty() {
                for (bid, gid_opt, tag_opt, tid) in pre_kills {
                    if effects.fail_reason.is_some() || effects.complete_now {
                        break;
                    }
                    let ev = DungeonEvent::NpcKilled {
                        npc_ball_id: bid,
                        group_id: gid_opt,
                        npc_tag: tag_opt.as_deref(),
                        npc_type_id: tid,
                        retroactive_room_check: true,
                    };
                    let _ = evaluate_room_triggers_in_inst(
                        snap, sys_id, &mut inst, deps.sde, room_vec_idx, &ev, char_id, now_unix, true, true, &mut effects,
                    );
                }
            } else if has_prior_kills && effects.fail_reason.is_none() && !effects.complete_now {
                let ev = DungeonEvent::NpcKilled {
                    npc_ball_id: 0,
                    group_id: None,
                    npc_tag: None,
                    npc_type_id: 0,
                    retroactive_room_check: true,
                };
                let _ = evaluate_room_triggers_in_inst(
                    snap, sys_id, &mut inst, deps.sde, room_vec_idx, &ev, char_id, now_unix, true, true, &mut effects,
                );
            }
        }
        finalize_terminal_in_snap(snap, inst, char_id, now_unix, &mut effects);
    });
    Some(effects)
}

pub(crate) async fn on_dungeon_room_entered_impl(
    deps: DungeonRuntimeDeps<'_>,
    sys_id: SolarSystemId,
    instance_id: i64,
    room_vec_idx: usize,
    char_id: i64,
) {
    if let Some(effects) =
        prepare_on_dungeon_room_entered_in_snap(deps, sys_id, instance_id, room_vec_idx, char_id)
    {
        flush_trigger_effects(deps, sys_id, effects).await;
    }
}

pub(crate) fn on_dungeon_room_entered_sync_impl(
    deps: DungeonRuntimeDeps<'_>,
    sys_id: SolarSystemId,
    instance_id: i64,
    room_vec_idx: usize,
    char_id: i64,
) {
    if let Some(effects) =
        prepare_on_dungeon_room_entered_in_snap(deps, sys_id, instance_id, room_vec_idx, char_id)
    {
        flush_trigger_effects_sync(deps, sys_id, effects);
    }
}

/// Read-pure `OnApproach` evaluation (`OWN-R5-01`, `PVE-212`); keys latches per ship/char and opens a
/// write tx ONLY when at least one trigger fires or an existing latch for this ship actually clears.
pub(crate) async fn on_dungeon_approach_impl(
    deps: DungeonRuntimeDeps<'_>,
    char_id: i64,
    sys_id: SolarSystemId,
    ship_id: i64,
    target_id_opt: Option<i64>,
) -> usize {
    if (ship_id <= 0 && char_id <= 0) || !has_dungeon_runtime_store_db(deps.db) {
        return 0;
    }
    let has_cands = deps.db.with_snapshot(|snap| {
        snap.dungeon_runtime.instances.values().any(|inst| {
            !instance_is_finished(inst)
                && inst.solar_system_id == sys_id
                && inst.rooms.iter().any(|r| {
                    room_is_entered(r)
                        && r.triggers
                            .iter()
                            .any(|t| !t.fired && t.armed_at_unix.is_none() && kind_mentions_approach(&t.kind))
                })
        })
    });
    if !has_cands {
        return 0;
    }
    let Some(sys_handle) = deps.systems.get_handle(sys_id) else { return 0 };
    let Some((_, ship_ball)) = sys_handle.get_ball_snapshot(ship_id).await.ok().flatten() else {
        return 0;
    };
    let ship_pos = ship_ball.position.to_array();
    let now_unix = deps.db.clock().now_unix_secs();

    let prematched: Vec<(i64, Vec<usize>)> = deps.db.with_snapshot(|snap| {
        let ev = DungeonEvent::Approach { ship_id, char_id, ship_pos, target_id_opt };
        snap.dungeon_runtime
            .instances
            .values()
            .filter(|i| !instance_is_finished(i) && i.solar_system_id == sys_id)
            .filter_map(|inst| {
                let r_indices: Vec<usize> = inst
                    .rooms
                    .iter()
                    .enumerate()
                    .filter_map(|(ri, r)| {
                        let any = room_is_entered(r)
                            && r.triggers.iter().enumerate().any(|(ti, t)| {
                                !t.fired
                                    && t.armed_at_unix.is_none()
                                    && kind_mentions_approach(&t.kind)
                                    && trigger_needs_eval_dryrun(&t.kind, inst, r, ri, ti, &ev, now_unix)
                            });
                        any.then_some(ri)
                    })
                    .collect();
                (!r_indices.is_empty()).then_some((inst.instance_id, r_indices))
            })
            .collect()
    });
    if prematched.is_empty() {
        return 0;
    }

    let mut total_fired = 0usize;
    let mut effects = TriggerTxEffects::default();
    deps.db.with_mut_snapshot(|snap| {
        let ev = DungeonEvent::Approach { ship_id, char_id, ship_pos, target_id_opt };
        for (inst_id, r_indices) in prematched {
            let Some(mut inst) = snap.dungeon_runtime.instances.remove(&inst_id) else { continue };
            if instance_is_finished(&inst) || inst.solar_system_id != sys_id {
                snap.dungeon_runtime.instances.insert(inst_id, inst);
                continue;
            }
            let mut ie = TriggerTxEffects::default();
            for ri in r_indices {
                if ie.fail_reason.is_some() || ie.complete_now {
                    break;
                }
                total_fired += evaluate_room_triggers_in_inst(
                    snap, sys_id, &mut inst, deps.sde, ri, &ev, char_id, now_unix, false, false, &mut ie,
                );
            }
            finalize_terminal_in_snap(snap, inst, char_id, now_unix, &mut ie);
            merge_effects(&mut effects, ie);
        }
    });
    flush_trigger_effects(deps, sys_id, effects).await;
    total_fired
}

pub(crate) async fn on_dungeon_item_looted_impl(
    deps: DungeonRuntimeDeps<'_>,
    char_id: i64,
    sys_id_opt: Option<SolarSystemId>,
    container_ball_id: i64,
    item_type_id: TypeId,
) {
    if container_ball_id <= 0 || !has_dungeon_runtime_store_db(deps.db) {
        return;
    }
    let now_unix = deps.db.clock().now_unix_secs();
    let (prematched, grace_done) = deps.db.with_snapshot(|snap| {
        let ev = DungeonEvent::Loot { container_ball_id, item_type_id };
        let mut trigger_hits: Vec<(i64, Vec<usize>)> = Vec::new();
        let mut grace = Vec::new();
        for inst in snap.dungeon_runtime.instances.values() {
            if sys_id_opt.is_some_and(|sid| inst.solar_system_id != sid) {
                continue;
            }
            if !instance_is_finished(inst) {
                let r_indices: Vec<usize> = inst
                    .rooms
                    .iter()
                    .enumerate()
                    .filter_map(|(ri, r)| {
                        let any = room_is_entered(r)
                            && r.triggers.iter().enumerate().any(|(ti, t)| {
                                !t.fired
                                    && t.armed_at_unix.is_none()
                                    && kind_mentions_loot(&t.kind)
                                    && trigger_needs_eval_dryrun(&t.kind, inst, r, ri, ti, &ev, now_unix)
                            });
                        any.then_some(ri)
                    })
                    .collect();
                if !r_indices.is_empty() {
                    trigger_hits.push((inst.instance_id, r_indices));
                }
            } else if (inst.completed || inst.failed)
                && !inst.despawned
                && inst.loot_grace_until_unix.or(inst.despawn_after_unix).is_some()
                && inst.rooms.iter().any(|r| {
                    r.objects.iter().any(|o| o.is_container && o.spawned && !o.destroyed && o.ball_id == container_ball_id)
                })
                && !has_unlooted_containers_in_snap(snap, inst)
            {
                grace.push((inst.solar_system_id, inst.instance_id));
            }
        }
        (trigger_hits, grace)
    });
    if !prematched.is_empty() {
        let mut effects_by_sys: HashMap<SolarSystemId, TriggerTxEffects> = HashMap::new();
        deps.db.with_mut_snapshot(|snap| {
            let ev = DungeonEvent::Loot { container_ball_id, item_type_id };
            for (inst_id, r_indices) in prematched {
                let Some(mut inst) = snap.dungeon_runtime.instances.remove(&inst_id) else { continue };
                if instance_is_finished(&inst) || sys_id_opt.is_some_and(|sid| inst.solar_system_id != sid) {
                    snap.dungeon_runtime.instances.insert(inst_id, inst);
                    continue;
                }
                let (inst_sys, mut ie) = (inst.solar_system_id, TriggerTxEffects::default());
                for ri in r_indices {
                    if ie.fail_reason.is_some() || ie.complete_now {
                        break;
                    }
                    let _ = evaluate_room_triggers_in_inst(
                        snap, inst_sys, &mut inst, deps.sde, ri, &ev, char_id, now_unix, false, false, &mut ie,
                    );
                }
                finalize_terminal_in_snap(snap, inst, char_id, now_unix, &mut ie);
                merge_effects(effects_by_sys.entry(inst_sys).or_default(), ie);
            }
        });
        for (sid, eff) in effects_by_sys {
            flush_trigger_effects(deps, sid, eff).await;
        }
    }
    for (sid, inst_id) in grace_done {
        complete_and_flush_instance(deps, sid, inst_id, char_id, true).await;
    }
}

pub(crate) async fn tick_due_delayed_dungeon_triggers_impl(deps: DungeonRuntimeDeps<'_>) -> usize {
    if !has_dungeon_runtime_store_db(deps.db) {
        return 0;
    }
    let now_unix = deps.db.clock().now_unix_secs();
    let due_ids: Vec<i64> = deps.db.with_snapshot(|snap| {
        snap.dungeon_runtime
            .instances
            .values()
            .filter(|i| {
                !instance_is_finished(i)
                    && i.rooms.iter().any(|r| {
                        r.triggers.iter().any(|t| {
                            !t.fired && t.armed_at_unix.is_some_and(|t0| now_unix >= t0.saturating_add(t.delay_secs))
                        })
                    })
            })
            .map(|i| i.instance_id)
            .collect()
    });
    if due_ids.is_empty() {
        return 0;
    }
    let mut fired_total = 0usize;
    let mut effects_by_sys: HashMap<SolarSystemId, TriggerTxEffects> = HashMap::new();
    deps.db.with_mut_snapshot(|snap| {
        for inst_id in due_ids {
            let Some(mut inst) = snap.dungeon_runtime.instances.remove(&inst_id) else { continue };
            if instance_is_finished(&inst) {
                snap.dungeon_runtime.instances.insert(inst_id, inst);
                continue;
            }
            let (inst_sys, mut ie, mut last_actor) = (inst.solar_system_id, TriggerTxEffects::default(), 0i64);
            for r_idx in 0..inst.rooms.len() {
                if ie.fail_reason.is_some() || ie.complete_now {
                    break;
                }
                let room_fallback_actor = resolve_room_recipient(&inst, Some(r_idx), 0);
                let mut batch: TriggerBatch = Vec::new();
                let trig_len = inst.rooms[r_idx].triggers.len();
                for t_idx in 0..trig_len {
                    let due = {
                        let t = &inst.rooms[r_idx].triggers[t_idx];
                        !t.fired && t.armed_at_unix.is_some_and(|t0| now_unix >= t0.saturating_add(t.delay_secs))
                    };
                    if !due {
                        continue;
                    }
                    let (trig_id, tri, repeatable, armed_by, kind, acts) = {
                        let t = &mut inst.rooms[r_idx].triggers[t_idx];
                        let ab = t.armed_by_char_id.unwrap_or(0);
                        t.armed_at_unix = None;
                        t.armed_by_char_id = None;
                        if !t.repeatable {
                            t.fired = true;
                        }
                        (t.trigger_id, t.target_room_index, t.repeatable, ab, t.kind.clone(), t.all_actions().cloned().collect::<Vec<_>>())
                    };
                    if repeatable {
                        let entered_unix = inst.rooms[r_idx].entered_unix;
                        // BF-03 / BF-R4-01: reset latches and bump unnamed __rep_timer only if its leaf actually latched.
                        reset_repeatable_trigger_state(
                            &kind,
                            &mut inst.rooms[r_idx].armed_timers,
                            entered_unix,
                            r_idx,
                            t_idx,
                            "root",
                            now_unix,
                        );
                    }
                    let actor = if armed_by > 0 { armed_by } else { room_fallback_actor };
                    if actor > 0 {
                        last_actor = actor;
                    }
                    batch.push((trig_id, tri, repeatable, actor, acts));
                }
                if !batch.is_empty() {
                    record_room_actor(&mut inst, r_idx, last_actor);
                    fired_total += run_trigger_batch_in_inst(
                        snap, inst_sys, &mut inst, deps.sde, r_idx, last_actor, now_unix, batch, &mut ie,
                    ).0;
                    finish_room_clear_checks_in_inst(&mut inst, r_idx, last_actor, true, false, &mut ie);
                }
            }
            finalize_terminal_in_snap(snap, inst, last_actor, now_unix, &mut ie);
            merge_effects(effects_by_sys.entry(inst_sys).or_default(), ie);
        }
    });
    for (sid, eff) in effects_by_sys {
        flush_trigger_effects(deps, sid, eff).await;
    }
    fired_total
}

/// 1 Hz system-step handler for subwarp `OnApproach`, `OnTimer`, `OnNpcDamaged`, delay-armed triggers,
/// and expired loot grace (`OWN-R5-04`, `OWN-R5-05`, `R5 P2-1` strictly read-pure when idle/waiting).
pub(crate) async fn tick_dungeon_runtime_system_step_impl(deps: DungeonRuntimeDeps<'_>, sys_id: SolarSystemId) {
    if !has_dungeon_runtime_store_db(deps.db) {
        return;
    }
    let has_sys_inst = deps.db.with_snapshot(|snap| {
        snap.dungeon_runtime
            .instances
            .values()
            .any(|i| !i.despawned && i.solar_system_id == sys_id)
    });
    if !has_sys_inst {
        return;
    }
    let now_unix = deps.db.clock().now_unix_secs();
    let view_opt = deps.systems.get_handle(sys_id).map(|sh| sh.view());

    let (grace_expired, approach_hits, dmg_hits, timer_iids, room_actors) = deps.db.with_snapshot(|snap| {
        let mut grace = Vec::new();
        let mut app = Vec::new();
        let mut dmg = Vec::new();
        let mut timers = HashSet::new();
        let mut actors: HashMap<(i64, usize), i64> = HashMap::new();

        let mut in_space_ships: Vec<(i64, i64, [f64; 3])> = Vec::new();
        let mut seen_ships = HashSet::new();
        if let Some(view) = view_opt.as_deref() {
            for ch in snap.characters.values().filter(|c| c.solar_system_id == sys_id && c.active_ship_id > 0) {
                if let Some(ball) = view.balls.get(&ch.active_ship_id) {
                    if seen_ships.insert(ch.active_ship_id) {
                        in_space_ships.push((ch.active_ship_id, ch.character_id, ball.position.to_array()));
                    }
                }
            }
            for (&bid, meta) in &view.ball_slim_meta {
                if meta.category_id == 6 && seen_ships.insert(bid) {
                    if let (Some(cid), Some(ball)) = (meta.char_id.filter(|&c| c > 0), view.balls.get(&bid)) {
                        in_space_ships.push((bid, cid, ball.position.to_array()));
                    }
                }
            }
        }

        for inst in snap.dungeon_runtime.instances.values() {
            if inst.despawned || inst.solar_system_id != sys_id {
                continue;
            }
            if (inst.completed || inst.failed)
                && inst.loot_grace_until_unix.or(inst.despawn_after_unix).is_some_and(|due| now_unix >= due)
            {
                grace.push((inst.instance_id, inst.failed));
                continue;
            }
            if instance_is_finished(inst) {
                continue;
            }
            for (ri, r) in inst.rooms.iter().enumerate() {
                if !room_is_entered(r) {
                    continue;
                }
                let base_actor = resolve_room_recipient(inst, Some(ri), 0);
                let pocket_actor = if base_actor > 0 {
                    base_actor
                } else {
                    in_space_ships
                        .iter()
                        .find(|&&(_, cid, pos)| cid > 0 && room_contains_pocket_pos(inst, ri, r, pos, POCKET_RADIUS_M))
                        .map_or(0, |&(_, cid, _)| cid)
                };
                if pocket_actor > 0 {
                    actors.insert((inst.instance_id, ri), pocket_actor);
                }

                let mut want_dmg = false;
                let mut want_app = false;
                let mut need_timer_tx = false;
                for (ti, t) in r.triggers.iter().enumerate() {
                    if t.fired {
                        continue;
                    }
                    if let Some(t0) = t.armed_at_unix {
                        if now_unix >= t0.saturating_add(t.delay_secs) {
                            need_timer_tx = true;
                        }
                        continue;
                    }
                    if kind_needs_damage_poll(&t.kind) {
                        want_dmg = true;
                    }
                    if kind_mentions_approach(&t.kind) {
                        want_app = true;
                    }
                    if (kind_needs_timer_poll(&t.kind)
                        || r.armed_timers.keys().any(|k| k.starts_with("__latch:") && (k.ends_with(":room") || k.contains(":grp:"))))
                        && trigger_needs_eval_dryrun(&t.kind, inst, r, ri, ti, &DungeonEvent::TimerTick, now_unix)
                    {
                        need_timer_tx = true;
                    }
                }
                if need_timer_tx {
                    timers.insert(inst.instance_id);
                }
                if want_app {
                    for &(ship_id, cid, ship_pos) in &in_space_ships {
                        let ev = DungeonEvent::Approach { ship_id, char_id: cid, ship_pos, target_id_opt: None };
                        if r.triggers.iter().enumerate().any(|(ti, t)| {
                            !t.fired && t.armed_at_unix.is_none() && kind_mentions_approach(&t.kind)
                                && trigger_needs_eval_dryrun(&t.kind, inst, r, ri, ti, &ev, now_unix)
                        }) {
                            app.push((inst.instance_id, ri, ship_id, cid, ship_pos));
                        }
                    }
                }
                if want_dmg {
                    if let Some(view) = view_opt.as_deref() {
                        for g in &r.npc_groups {
                            for &bid in &g.npc_ball_ids {
                                let Some(cs) = view.combat_states.get(&bid) else { continue };
                                let total_max = (cs.max_shield_hp + cs.max_armor_hp + cs.max_structure_hp).max(1.0);
                                let total_cur = (cs.shield_hp + cs.armor_hp + cs.structure_hp).max(0.0);
                                let total_pct = ((total_cur / total_max) * 100.0).clamp(0.0, 100.0).round() as u8;
                                let shield_pct = (cs.max_shield_hp > 0.0).then(|| {
                                    ((cs.shield_hp / cs.max_shield_hp) * 100.0).clamp(0.0, 100.0).round() as u8
                                });
                                let hull_pct = ((cs.structure_hp / cs.max_structure_hp.max(1.0)) * 100.0).clamp(0.0, 100.0).round() as u8;
                                let depleted_sa = cs.shield_hp + cs.armor_hp <= 0.5;
                                let damaged = total_cur + 0.5 < total_max;
                                let ev = DungeonEvent::NpcDamaged {
                                    npc_ball_id: bid,
                                    group_id: Some(g.group_id),
                                    npc_tag: g.npc_ball_tags.get(&bid).map(String::as_str),
                                    total_pct,
                                    shield_pct,
                                    hull_pct,
                                    shield_or_armor_depleted: depleted_sa,
                                    damaged_at_all: damaged,
                                };
                                let hit = r.triggers.iter().enumerate().any(|(ti, t)| {
                                    !t.fired && t.armed_at_unix.is_none() && kind_needs_damage_poll(&t.kind)
                                        && trigger_needs_eval_dryrun(&t.kind, inst, r, ri, ti, &ev, now_unix)
                                });
                                if hit {
                                    dmg.push((inst.instance_id, ri, bid, g.group_id, g.npc_ball_tags.get(&bid).cloned(), total_pct, shield_pct, hull_pct, depleted_sa, damaged));
                                }
                            }
                        }
                    }
                }
            }
        }
        let mut timer_vec: Vec<i64> = timers.into_iter().collect();
        timer_vec.sort_unstable();
        (grace, app, dmg, timer_vec, actors)
    });
    drop(view_opt);

    if grace_expired.is_empty() && approach_hits.is_empty() && dmg_hits.is_empty() && timer_iids.is_empty() {
        return;
    }

    let mut effects = TriggerTxEffects::default();
    deps.db.with_mut_snapshot(|snap| {
        for (iid, was_failed) in grace_expired {
            finish_instance_entities_in_snap(snap, iid, 0, now_unix, was_failed, true, &mut effects);
        }
        for (iid, ri, ship_id, cid, ship_pos) in approach_hits {
            let Some(mut inst) = snap.dungeon_runtime.instances.remove(&iid) else { continue };
            if instance_is_finished(&inst) || inst.solar_system_id != sys_id {
                snap.dungeon_runtime.instances.insert(iid, inst);
                continue;
            }
            let mut ie = TriggerTxEffects::default();
            let ev = DungeonEvent::Approach { ship_id, char_id: cid, ship_pos, target_id_opt: None };
            let _ = evaluate_room_triggers_in_inst(snap, sys_id, &mut inst, deps.sde, ri, &ev, cid, now_unix, false, false, &mut ie);
            finalize_terminal_in_snap(snap, inst, cid, now_unix, &mut ie);
            merge_effects(&mut effects, ie);
        }
        for (iid, ri, bid, gid, tag_opt, total_pct, shield_pct, hull_pct, depleted_sa, damaged) in dmg_hits {
            let Some(mut inst) = snap.dungeon_runtime.instances.remove(&iid) else { continue };
            if instance_is_finished(&inst) || inst.solar_system_id != sys_id {
                snap.dungeon_runtime.instances.insert(iid, inst);
                continue;
            }
            let actor = room_actors.get(&(iid, ri)).copied().unwrap_or(0);
            let mut ie = TriggerTxEffects::default();
            let ev = DungeonEvent::NpcDamaged {
                npc_ball_id: bid,
                group_id: Some(gid),
                npc_tag: tag_opt.as_deref(),
                total_pct,
                shield_pct,
                hull_pct,
                shield_or_armor_depleted: depleted_sa,
                damaged_at_all: damaged,
            };
            let _ = evaluate_room_triggers_in_inst(snap, sys_id, &mut inst, deps.sde, ri, &ev, actor, now_unix, false, false, &mut ie);
            finalize_terminal_in_snap(snap, inst, actor, now_unix, &mut ie);
            merge_effects(&mut effects, ie);
        }
        for iid in timer_iids {
            let Some(mut inst) = snap.dungeon_runtime.instances.remove(&iid) else { continue };
            if instance_is_finished(&inst) || inst.solar_system_id != sys_id {
                snap.dungeon_runtime.instances.insert(iid, inst);
                continue;
            }
            let mut ie = TriggerTxEffects::default();
            let mut last_actor = 0i64;
            for ri in 0..inst.rooms.len() {
                if ie.fail_reason.is_some() || ie.complete_now {
                    break;
                }
                let actor = room_actors.get(&(iid, ri)).copied().unwrap_or(0);
                if actor > 0 {
                    last_actor = actor;
                }
                let _ = evaluate_room_triggers_in_inst(
                    snap, sys_id, &mut inst, deps.sde, ri, &DungeonEvent::TimerTick, actor, now_unix, false, false, &mut ie,
                );
            }
            finalize_terminal_in_snap(snap, inst, last_actor, now_unix, &mut ie);
            merge_effects(&mut effects, ie);
        }
    });
    flush_trigger_effects(deps, sys_id, effects).await;
}

/// Clears repeatable room/group kill latches whenever `plan_npc_group_spawns_in_snap` spawns/respawns `spawned_gid`.
pub(crate) fn clear_latches_for_spawned_group(room: &mut DungeonRuntimeRoom, spawned_gid: u8) {
    room.armed_timers.remove(&format!("__group_cleared:{spawned_gid}"));
    room.armed_timers.remove(&format!("group_cleared:{spawned_gid}"));
    let grp_suffix = format!(":grp:{spawned_gid}");
    let after_suffix = format!(":after_grp:{spawned_gid}");
    room.armed_timers.retain(|k, _| {
        !(k.starts_with("__latch:")
            && (k.ends_with(":room") || k.ends_with(&grp_suffix) || k.ends_with(&after_suffix)))
    });
}

#[cfg(test)]
#[path = "pve_dungeon_runtime_t2_tests.rs"]
mod pve_dungeon_runtime_t2_tests;
