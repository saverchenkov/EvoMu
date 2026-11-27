//! Track 2c-1 trigger hooks & gameplay-path orchestration for [`super::pve_dungeon_runtime`]:
//! - `OnCargoDelivered` (`on_dungeon_container_cargo_changed`, net delta accounting)
//! - `OnStructureDestroyed` (`on_dungeon_structure_destroyed_sync`, 100% synchronous / zero `block_on`)

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::pve_dungeon_runtime::actions::{
    finalize_terminal_in_snap, flush_trigger_effects, flush_trigger_effects_sync, merge_effects,
    DungeonRuntimeDeps, TriggerTxEffects,
};
use crate::services::pve_dungeon_runtime::triggers::{
    evaluate_room_triggers_in_inst, DungeonEvent,
};
use crate::services::pve_dungeon_runtime::{
    has_dungeon_runtime_store, instance_is_finished, room_is_entered, DEFAULT_CONTAINER_TYPE_ID,
};
use crate::services::pve_exploration_seeding::build_type_combat_state_from_sde;
use eve_db::service_domains::pve_sites::PendingDungeonEscalation;
use eve_db::WorldStateSnapshot;
use eve_destiny::CombatState;
use eve_sde::{SolarSystemId, TypeId};
use std::collections::HashMap;

pub(crate) fn build_destructible_object_combat_state(
    sde: &eve_sde::SdeRegistry,
    type_id: u32,
) -> CombatState {
    let tid = if type_id > 0 {
        type_id
    } else {
        DEFAULT_CONTAINER_TYPE_ID
    };
    let mut cs = build_type_combat_state_from_sde(sde, tid);
    if cs.structure_hp <= 0.0 {
        cs.structure_hp = 1_000.0;
    }
    if cs.max_structure_hp < cs.structure_hp {
        cs.max_structure_hp = cs.structure_hp;
    }
    if cs.shield_hp < 0.0 {
        cs.shield_hp = 0.0;
    }
    if cs.armor_hp < 0.0 {
        cs.armor_hp = 0.0;
    }
    cs.is_tethered = false;
    cs
}

pub fn is_destructible_dungeon_object_in_snap(
    snap: &WorldStateSnapshot,
    sys_id: SolarSystemId,
    ball_id: i64,
) -> bool {
    if ball_id <= 0 {
        return false;
    }
    snap.dungeon_runtime.instances.values().any(|inst| {
        !instance_is_finished(inst)
            && inst.solar_system_id == sys_id
            && inst.rooms.iter().any(|r| {
                r.objects
                    .iter()
                    .any(|o| o.ball_id == ball_id && o.spawned && !o.destroyed && o.destructible)
            })
    })
}

pub fn is_destructible_dungeon_object(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    ball_id: i64,
) -> bool {
    if ball_id <= 0 || !has_dungeon_runtime_store(ctx) {
        return false;
    }
    ctx.db
        .with_snapshot(|snap| is_destructible_dungeon_object_in_snap(snap, sys_id, ball_id))
}

pub fn is_dungeon_object_ball(ctx: &ServerContext, sys_id: SolarSystemId, ball_id: i64) -> bool {
    if ball_id <= 0 || !has_dungeon_runtime_store(ctx) {
        return false;
    }
    ctx.db.with_snapshot(|snap| {
        snap.dungeon_runtime.instances.values().any(|inst| {
            !instance_is_finished(inst)
                && inst.solar_system_id == sys_id
                && inst.rooms.iter().any(|r| {
                    r.objects.iter().any(|o| {
                        o.ball_id == ball_id
                            && o.spawned
                            && !o.destroyed
                            && (o.destructible
                                || o.is_container
                                || !ctx
                                    .sde
                                    .types
                                    .get(&o.type_id)
                                    .is_some_and(|t| t.category_id == 11))
                    })
                })
        })
    })
}

/// Tracks net cargo additions/withdrawals in dungeon containers for `OnCargoDelivered` (`R3-P1-2`).
/// Uses read-only snapshot pre-check so non-dungeon item moves perform zero DB write transactions.
pub async fn on_dungeon_container_cargo_changed(
    ctx: &ServerContext,
    char_id: i64,
    sys_id_opt: Option<SolarSystemId>,
    src_loc_id: i64,
    dst_loc_id: i64,
    item_type_id: TypeId,
    moved_qty: i64,
) {
    if moved_qty <= 0
        || src_loc_id == dst_loc_id
        || item_type_id == 0
        || !has_dungeon_runtime_store(ctx)
    {
        return;
    }
    let matched_ids: Vec<i64> = ctx.db.with_snapshot(|snap| {
        let mut ids = Vec::new();
        for inst in snap.dungeon_runtime.instances.values() {
            if instance_is_finished(inst)
                || sys_id_opt.is_some_and(|sid| inst.solar_system_id != sid)
            {
                continue;
            }
            let hits = inst.rooms.iter().any(|r| {
                room_is_entered(r)
                    && r.objects.iter().any(|o| {
                        o.is_container
                            && o.spawned
                            && !o.destroyed
                            && o.ball_id > 0
                            && (o.ball_id == src_loc_id || o.ball_id == dst_loc_id)
                    })
            });
            if hits {
                ids.push(inst.instance_id);
            }
        }
        ids
    });
    if matched_ids.is_empty() {
        return;
    }

    let now_unix = ctx.db.clock().now_unix_secs();
    let mut effects_by_sys: HashMap<SolarSystemId, TriggerTxEffects> = HashMap::new();
    ctx.db.with_mut_snapshot(|snap| {
        for inst_id in matched_ids {
            let Some(mut inst) = snap.dungeon_runtime.instances.remove(&inst_id) else {
                continue;
            };
            if instance_is_finished(&inst)
                || sys_id_opt.is_some_and(|sid| inst.solar_system_id != sid)
            {
                snap.dungeon_runtime.instances.insert(inst_id, inst);
                continue;
            }
            let inst_sys = inst.solar_system_id;
            let mut ie = TriggerTxEffects::default();
            for r_idx in 0..inst.rooms.len() {
                if ie.fail_reason.is_some() || ie.complete_now {
                    break;
                }
                if !room_is_entered(&inst.rooms[r_idx]) {
                    continue;
                }
                let has_src = src_loc_id > 0
                    && inst.rooms[r_idx].objects.iter().any(|o| {
                        o.is_container && o.spawned && !o.destroyed && o.ball_id == src_loc_id
                    });
                let has_dst = dst_loc_id > 0
                    && inst.rooms[r_idx].objects.iter().any(|o| {
                        o.is_container && o.spawned && !o.destroyed && o.ball_id == dst_loc_id
                    });
                if has_src {
                    let key = format!("{src_loc_id}:{item_type_id}");
                    let cur = inst.rooms[r_idx]
                        .cargo_delivered_net
                        .get(&key)
                        .copied()
                        .unwrap_or(0);
                    let next = cur.saturating_sub(moved_qty);
                    if next == 0 {
                        inst.rooms[r_idx].cargo_delivered_net.remove(&key);
                    } else {
                        inst.rooms[r_idx].cargo_delivered_net.insert(key, next);
                    }
                    let ev = DungeonEvent::CargoDelivered {
                        container_ball_id: src_loc_id,
                        item_type_id,
                        is_deposit: false,
                    };
                    let _ = evaluate_room_triggers_in_inst(
                        snap,
                        inst_sys,
                        &mut inst,
                        Some(&ctx.sde),
                        r_idx,
                        &ev,
                        char_id,
                        now_unix,
                        false,
                        false,
                        &mut ie,
                    );
                }
                if has_dst {
                    let key = format!("{dst_loc_id}:{item_type_id}");
                    let cur = inst.rooms[r_idx]
                        .cargo_delivered_net
                        .get(&key)
                        .copied()
                        .unwrap_or(0);
                    let next = cur.saturating_add(moved_qty);
                    if next == 0 {
                        inst.rooms[r_idx].cargo_delivered_net.remove(&key);
                    } else {
                        inst.rooms[r_idx].cargo_delivered_net.insert(key, next);
                    }
                    let ev = DungeonEvent::CargoDelivered {
                        container_ball_id: dst_loc_id,
                        item_type_id,
                        is_deposit: true,
                    };
                    let _ = evaluate_room_triggers_in_inst(
                        snap,
                        inst_sys,
                        &mut inst,
                        Some(&ctx.sde),
                        r_idx,
                        &ev,
                        char_id,
                        now_unix,
                        false,
                        false,
                        &mut ie,
                    );
                }
            }
            finalize_terminal_in_snap(snap, inst, char_id, now_unix, &mut ie);
            merge_effects(effects_by_sys.entry(inst_sys).or_default(), ie);
        }
    });
    let deps = DungeonRuntimeDeps::from_ctx(ctx);
    for (sid, eff) in effects_by_sys {
        flush_trigger_effects(deps, sid, eff).await;
    }
}

/// Synchronously handles destruction of any destructible dungeon object (`R3-P1-5`),
/// returning `true` if `ball_id` was an owned destructible dungeon structure/container.
/// Executes DB state update + trigger actions in one tx and flushes ballpark/notifications
/// strictly without `block_on` or nested Tokio runtimes.
pub fn on_dungeon_structure_destroyed_sync(
    ctx: &ServerContext,
    sys_handle: &crate::system_actor::SystemHandle,
    sys_id: SolarSystemId,
    ball_id: i64,
    killer_char_id: i64,
) -> bool {
    if ball_id <= 0 || !has_dungeon_runtime_store(ctx) {
        return false;
    }
    if !ctx
        .db
        .with_snapshot(|snap| is_destructible_dungeon_object_in_snap(snap, sys_id, ball_id))
    {
        return false;
    }

    let now_unix = ctx.db.clock().now_unix_secs();
    let matched_effects: Option<TriggerTxEffects> = ctx.db.with_mut_snapshot(|snap| {
        let candidate_ids: Vec<i64> = snap
            .dungeon_runtime
            .instances
            .values()
            .filter(|inst| {
                !instance_is_finished(inst)
                    && inst.solar_system_id == sys_id
                    && inst.rooms.iter().any(|r| {
                        r.objects.iter().any(|o| {
                            o.ball_id == ball_id && o.spawned && !o.destroyed && o.destructible
                        })
                    })
            })
            .map(|inst| inst.instance_id)
            .collect();
        if candidate_ids.is_empty() {
            return None;
        }
        let mut total_effects = TriggerTxEffects::default();
        let mut handled_any = false;
        for inst_id in candidate_ids {
            let Some(mut inst) = snap.dungeon_runtime.instances.remove(&inst_id) else {
                continue;
            };
            if instance_is_finished(&inst) || inst.solar_system_id != sys_id {
                snap.dungeon_runtime.instances.insert(inst_id, inst);
                continue;
            }
            let mut ie = TriggerTxEffects::default();
            for r_idx in 0..inst.rooms.len() {
                if ie.fail_reason.is_some() || ie.complete_now {
                    break;
                }
                let Some(obj_idx) = inst.rooms[r_idx].objects.iter().position(|o| {
                    o.ball_id == ball_id && o.spawned && !o.destroyed && o.destructible
                }) else {
                    continue;
                };
                handled_any = true;
                let (obj_tag, obj_type_id) = {
                    let obj = &mut inst.rooms[r_idx].objects[obj_idx];
                    obj.destroyed = true;
                    (obj.tag.clone(), obj.type_id)
                };
                snap.combat_crimewatch_state.record_destroyed_item_id(ball_id);
                snap.items
                    .retain(|iid, rec| *iid != ball_id && rec.location_id != ball_id);
                snap.item_positions.remove(&ball_id);
                snap.sp6_lifecycle.space_object_meta.remove(&ball_id);
                ie.despawn_other_balls.push(ball_id);

                if room_is_entered(&inst.rooms[r_idx]) {
                    let ev = DungeonEvent::StructureDestroyed {
                        target_ball_id: ball_id,
                        object_index: obj_idx,
                        object_tag: obj_tag.as_deref(),
                        object_type_id: obj_type_id,
                    };
                    let _ = evaluate_room_triggers_in_inst(
                        snap,
                        sys_id,
                        &mut inst,
                        Some(&ctx.sde),
                        r_idx,
                        &ev,
                        killer_char_id,
                        now_unix,
                        false,
                        false,
                        &mut ie,
                    );
                } else {
                    let tag_str = obj_tag.as_deref().unwrap_or("");
                    inst.rooms[r_idx].armed_timers.insert(
                        format!(
                            "__pre_struct_destroy:{r_idx}:{obj_idx}:{ball_id}:{obj_type_id}:{tag_str}"
                        ),
                        now_unix,
                    );
                }
            }
            finalize_terminal_in_snap(snap, inst, killer_char_id, now_unix, &mut ie);
            merge_effects(&mut total_effects, ie);
        }
        handled_any.then_some(total_effects)
    });

    let Some(mut effects) = matched_effects else {
        return false;
    };
    effects.cancel_dead_spawns();
    let escalations = std::mem::take(&mut effects.escalation_requests);
    let _ = sys_handle.remove_ball_sync(ball_id);
    flush_trigger_effects_sync(DungeonRuntimeDeps::from_ctx(ctx), sys_id, effects);
    if !escalations.is_empty() {
        ctx.db.with_mut_snapshot(|snap| {
            for (
                char_id,
                source_dungeon_id,
                target_dungeon_id,
                chance_bps,
                target_title,
                position,
                instance_id,
            ) in escalations
            {
                snap.dungeon_runtime
                    .pending_escalations
                    .push(PendingDungeonEscalation {
                        char_id,
                        solar_system_id: sys_id,
                        source_dungeon_id,
                        target_dungeon_id,
                        chance_bps,
                        target_title,
                        position,
                        instance_id,
                    });
            }
        });
    }
    true
}

#[cfg(test)]
#[path = "pve_dungeon_runtime_trigger_hooks_tests.rs"]
mod pve_dungeon_runtime_trigger_hooks_tests;
