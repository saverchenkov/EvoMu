//! `pve_dungeon_runtime` — Multi-room deadspace dungeons, acceleration gate pocket traversal,
//! data-driven MWD restrictions, and room object/NPC/trigger lifecycle (`feat:pve.deadspace-pocket-rules`, `feat:pve.dungeon-system`).

#![forbid(unsafe_code)]

#[path = "pve_dungeon_runtime_actions.rs"]
pub(crate) mod actions;
#[path = "pve_dungeon_runtime_ops.rs"]
mod ops;

use crate::router::ServerContext;
use crate::services::notify_emitters_space_dungeons::{
    emit_dungeon_progression_notifications, emit_exiting_dungeon_on_warp_if_present,
};
use crate::services::pve_exploration::{read_pve_exploration_reg, with_pve_exploration_reg};
use crate::services::pve_exploration_seeding::build_type_combat_state_from_sde;
use crate::services::pve_special_sites::make_site_npc_ball;
use eve_db::lock_rank::AnyRwLock;
use eve_db::{InventoryFlag, InventoryItemRecord, WorldStateSnapshot};
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::SolarSystemId;
use glam::DVec3;
use std::collections::{HashMap, HashSet};

const POCKET_RADIUS_M: f64 = 200_000.0;
pub(crate) const GATE_ACTIVATION_RANGE_M: f64 = 2_500.0;
pub(crate) const ACCELERATION_GATE_TYPE_ID: u32 = 17831;
pub(crate) const DEFAULT_CONTAINER_TYPE_ID: u32 = 23;
pub(crate) const DUNGEON_COMPLETION_LOOT_GRACE_SECS: u64 = 1_800;
pub(crate) const COMPLETED_INSTANCE_RETENTION_SECS: u64 = 3_600;

pub use eve_db::{
    DungeonGateLink, DungeonInstanceId, DungeonNpcGroup, DungeonNpcSpawnEntry, DungeonObjective,
    DungeonObjectiveStatus, DungeonRuntimeInstance, DungeonRuntimeObject, DungeonRuntimeRoom,
    DungeonRuntimeStore, DungeonSpawnedNpcMeta, DungeonTemplateId, DungeonTrigger,
    DungeonTriggerAction, DungeonTriggerKind, EscalationInstanceId, ExplorationSiteId,
};

pub(crate) fn has_dungeon_runtime_store_db(db: &eve_db::WorldRepository) -> bool {
    db.with_snapshot(|snap| !snap.dungeon_runtime.instances.is_empty())
}

pub(crate) fn has_dungeon_runtime_store(ctx: &ServerContext) -> bool {
    has_dungeon_runtime_store_db(&ctx.db)
}

pub fn read_dungeon_runtime_store_db<R>(
    db: &eve_db::WorldRepository,
    f: impl FnOnce(&DungeonRuntimeStore) -> R,
) -> R {
    db.with_snapshot(|snap| f(&snap.dungeon_runtime))
}

pub fn read_dungeon_runtime_store<R>(
    ctx: &ServerContext,
    f: impl FnOnce(&DungeonRuntimeStore) -> R,
) -> R {
    read_dungeon_runtime_store_db(&ctx.db, f)
}

pub fn with_dungeon_runtime_store_db<R>(
    db: &eve_db::WorldRepository,
    f: impl FnOnce(&mut DungeonRuntimeStore) -> R,
) -> R {
    db.with_mut_snapshot(|snap| f(&mut snap.dungeon_runtime))
}

pub fn with_dungeon_runtime_store<R>(
    ctx: &ServerContext,
    f: impl FnOnce(&mut DungeonRuntimeStore) -> R,
) -> R {
    with_dungeon_runtime_store_db(&ctx.db, f)
}

pub(crate) fn dist3(a: [f64; 3], b: [f64; 3]) -> f64 {
    DVec3::from_array(a).distance(DVec3::from_array(b))
}

pub(crate) fn unix_to_filetime_ticks(u: u64) -> i64 {
    ((u as i64) + 11_644_473_600) * 10_000_000
}

#[inline]
pub(crate) fn instance_is_finished(inst: &DungeonRuntimeInstance) -> bool {
    inst.completed || inst.failed || inst.despawned
}

#[inline]
pub(crate) fn room_is_entered(room: &DungeonRuntimeRoom) -> bool {
    room.spawned && room.entered
}

pub(crate) fn record_dungeon_actor(inst: &mut DungeonRuntimeInstance, char_id: i64) {
    if char_id > 0 { inst.last_actor_char_id = char_id; }
}

pub(crate) fn record_room_actor(inst: &mut DungeonRuntimeInstance, room_vec_idx: usize, char_id: i64) {
    if char_id > 0 {
        inst.last_actor_char_id = char_id;
        if let Some(room) = inst.rooms.get_mut(room_vec_idx) { room.last_actor_char_id = char_id; }
    }
}

pub(crate) fn resolve_dungeon_recipient(inst: &DungeonRuntimeInstance, actor_char_id: i64) -> i64 {
    if actor_char_id > 0 { actor_char_id } else if inst.owner_char_id > 0 { inst.owner_char_id } else { inst.last_actor_char_id }
}

pub(crate) fn resolve_room_recipient(
    inst: &DungeonRuntimeInstance, room_vec_idx: Option<usize>, actor_char_id: i64,
) -> i64 {
    if actor_char_id > 0 { return actor_char_id; }
    if let Some(cid) = room_vec_idx.and_then(|i| inst.rooms.get(i)).map(|r| r.last_actor_char_id).filter(|&id| id > 0) {
        return cid;
    }
    resolve_dungeon_recipient(inst, 0)
}

pub(crate) fn wake_dungeon_system_handle(
    systems: &crate::system_actor::SystemRegistry, sys_id: SolarSystemId,
) -> crate::system_actor::SystemHandle {
    systems.get_or_spawn(sys_id)
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RoomGateSpec {
    pub(crate) gate_ball_id: Option<i64>,
    pub(crate) gate_position: [f64; 3],
    pub(crate) target_room_vec_idx: usize,
    pub(crate) destination_pos: Option<[f64; 3]>,
    pub(crate) locked: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RuntimeGateMatch {
    pub(crate) instance_id: i64,
    pub(crate) source_room_vec_idx: Option<usize>,
    pub(crate) target_room_vec_idx: usize,
    pub(crate) gate_position: Option<[f64; 3]>,
    pub(crate) locked: bool,
    pub(crate) gate_ball_id: i64,
    pub(crate) dest_override: Option<[f64; 3]>,
}

/// Canonical per-room exit gate descriptor (`I4` / `OWN-R5-03`), supporting branching `gate_links`
/// and linear `r_idx + 1` progression without inventing phantom gates in final rooms.
pub(crate) fn collect_room_gate_specs(inst: &DungeonRuntimeInstance, room_vec_idx: usize) -> Vec<RoomGateSpec> {
    let Some(room) = inst.rooms.get(room_vec_idx) else { return Vec::new() };
    if !room.gate_links.is_empty() {
        return room.gate_links.iter().filter(|gl| gl.target_room_index < inst.rooms.len()).map(|gl| RoomGateSpec {
            gate_ball_id: gl.gate_ball_id, gate_position: gl.gate_position, target_room_vec_idx: gl.target_room_index,
            destination_pos: (gl.destination_pos != [0.0, 0.0, 0.0]).then_some(gl.destination_pos), locked: gl.locked,
        }).collect();
    }
    if room_vec_idx + 1 < inst.rooms.len() {
        return vec![RoomGateSpec {
            gate_ball_id: room.gate_ball_id,
            gate_position: [room.position[0] + 5_000.0, room.position[1], room.position[2]],
            target_room_vec_idx: room_vec_idx + 1,
            destination_pos: None,
            locked: room.gate_locked,
        }];
    }
    Vec::new()
}

fn gate_match_from_spec(instance_id: i64, src_idx: usize, spec: RoomGateSpec, fallback_id: i64) -> RuntimeGateMatch {
    RuntimeGateMatch {
        instance_id, source_room_vec_idx: Some(src_idx), target_room_vec_idx: spec.target_room_vec_idx,
        gate_position: Some(spec.gate_position), locked: spec.locked,
        gate_ball_id: spec.gate_ball_id.unwrap_or(fallback_id), dest_override: spec.destination_pos,
    }
}

/// Single authoritative gate lookup used by both `pve_exploration::ActivateAccelerationGate`
/// dispatch and `try_activate_dungeon_runtime_gate` (`I4`, `OWN-R5-03`).
pub(crate) fn resolve_runtime_gate_in_snap(
    snap: &WorldStateSnapshot, sys_id: SolarSystemId, arg_int: Option<i64>, arg_str: Option<&str>,
) -> Option<RuntimeGateMatch> {
    let arg_str_trimmed = arg_str.map(str::trim).filter(|s| !s.is_empty());
    for inst in snap.dungeon_runtime.instances.values() {
        if instance_is_finished(inst) || inst.solar_system_id != sys_id || inst.category == "Flashpoint" { continue; }
        if let Some(gate_id) = arg_int {
            if inst.entry_gate_ball_id == Some(gate_id) && !inst.rooms.is_empty() {
                return Some(RuntimeGateMatch {
                    instance_id: inst.instance_id, source_room_vec_idx: None, target_room_vec_idx: 0,
                    gate_position: inst.entry_gate_pos, locked: inst.entry_gate_locked, gate_ball_id: gate_id, dest_override: None,
                });
            }
            for r_idx in 0..inst.rooms.len() {
                if let Some(spec) = collect_room_gate_specs(inst, r_idx).into_iter().find(|s| s.gate_ball_id == Some(gate_id)) {
                    return Some(gate_match_from_spec(inst.instance_id, r_idx, spec, gate_id));
                }
            }
        }
        if let Some(s) = arg_str_trimmed {
            if s == format!("DNG-INST-{}", inst.instance_id) {
                // `OWN-R5-03`: before entering (or outside 1-indexed Room 1), `DNG-INST-<id>` resolves entry gate.
                let before_entry = inst.current_room_index == 0
                    && (inst.entry_gate_pos.is_some() || inst.entry_gate_ball_id.is_some())
                    && inst.rooms.first().is_some_and(|r| !r.entered || r.room_index > 0);
                if before_entry {
                    return Some(RuntimeGateMatch {
                        instance_id: inst.instance_id, source_room_vec_idx: None, target_room_vec_idx: 0,
                        gate_position: inst.entry_gate_pos, locked: inst.entry_gate_locked,
                        gate_ball_id: inst.entry_gate_ball_id.unwrap_or(0), dest_override: None,
                    });
                }
                let cur_idx = if inst.current_room_index == 0 {
                    inst.rooms.first().filter(|r| r.room_index == 0 && (r.gate_ball_id.is_some() || !r.gate_links.is_empty() || (r.entered && inst.rooms.len() > 1))).map(|_| 0usize)
                } else {
                    inst.room_vec_index_by_room_index(inst.current_room_index)
                        .or_else(|| inst.current_room_index.checked_sub(1).filter(|&i| i < inst.rooms.len()))
                };
                if let Some(spec) = cur_idx.and_then(|r_idx| collect_room_gate_specs(inst, r_idx).into_iter().next().map(|sp| (r_idx, sp))) {
                    return Some(gate_match_from_spec(inst.instance_id, spec.0, spec.1, 0));
                }
            }
        }
    }
    None
}

/// Returns true if `gate_ball_arg` or `gate_str_arg` maps to a runtime dungeon gate or Homefront outer gate (`I4`).
pub fn has_runtime_gate_target(
    ctx: &ServerContext, sys_id: SolarSystemId, gate_ball_arg: Option<i64>, gate_str_arg: Option<&str>,
) -> bool {
    ctx.db.with_snapshot(|snap| {
        resolve_runtime_gate_in_snap(snap, sys_id, gate_ball_arg, gate_str_arg).is_some()
            || gate_ball_arg.is_some_and(|gate_id| {
                snap.dungeon_runtime.homefront_runtimes.values().any(|r| {
                    r.solar_system_id == sys_id && r.outer_gate_ball_id == gate_id && !r.completed && !r.failed
                })
            })
    })
}

/// Evaluates and fires any armed delayed dungeon triggers whose `delay_secs` timer has elapsed.
pub async fn tick_due_delayed_dungeon_triggers(ctx: &ServerContext) -> usize {
    actions::tick_due_delayed_dungeon_triggers_impl(actions::DungeonRuntimeDeps::from_ctx(ctx)).await
}

pub(crate) fn prune_stale_despawned_instances(store: &mut DungeonRuntimeStore, now_unix: u64) {
    store.instances.retain(|_, inst| {
        if !inst.despawned {
            return true;
        }
        let anchor = inst
            .despawn_after_unix
            .or(inst.completed_unix)
            .unwrap_or(inst.created_unix);
        now_unix < anchor.saturating_add(COMPLETED_INSTANCE_RETENTION_SECS)
    });
}

fn is_pocket_mwd_disallowed(
    ctx: &ServerContext,
    inst: &DungeonRuntimeInstance,
    room: &DungeonRuntimeRoom,
) -> bool {
    if let Some(flag) = room.disallow_mwd {
        return flag;
    }
    if let Some(flag) = crate::services::pve_dungeon_editor::get_dungeon_or_room_mwd_override(
        ctx,
        inst.dungeon_id as i64,
        Some(room.room_id),
    ) {
        return flag;
    }
    if inst.disallow_mwd {
        return true;
    }
    ctx.sde
        .dungeons
        .get(&inst.dungeon_id)
        .is_some_and(|d| matches!(d.archetype_id, Some(3 | 4 | 6 | 8 | 24 | 51)))
}

/// Returns `true` if `pos` is within `radius_m` of `room`'s primary position, default
/// `gate_destination_pos`, or any incoming `gate_links.destination_pos` targeting `r_idx`.
pub(crate) fn room_contains_pocket_pos(
    inst: &DungeonRuntimeInstance,
    r_idx: usize,
    room: &DungeonRuntimeRoom,
    pos: [f64; 3],
    radius_m: f64,
) -> bool {
    if dist3(pos, room.position) <= radius_m {
        return true;
    }
    if room.gate_destination_pos != [0.0, 0.0, 0.0]
        && !inst
            .rooms
            .iter()
            .enumerate()
            .any(|(oi, o)| oi != r_idx && dist3(room.gate_destination_pos, o.position) <= radius_m)
        && dist3(pos, room.gate_destination_pos) <= radius_m
    {
        return true;
    }
    inst.rooms.iter().any(|src| {
        src.gate_links.iter().any(|gl| {
            gl.target_room_index == r_idx
                && gl.destination_pos != [0.0, 0.0, 0.0]
                && dist3(pos, gl.destination_pos) <= radius_m
        })
    })
}

/// Returns `true` if `ship_pos` lies inside an active k-space `DungeonRuntimeInstance` pocket.
pub fn is_ship_in_deadspace_pocket(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    ship_pos: [f64; 3],
) -> bool {
    if !has_dungeon_runtime_store(ctx) {
        return false;
    }
    read_dungeon_runtime_store(ctx, |store| {
        store.instances.values().any(|inst| {
            !instance_is_finished(inst)
                && inst.solar_system_id == sys_id
                && inst
                    .rooms
                    .iter()
                    .enumerate()
                    .any(|(r_idx, r)| room_contains_pocket_pos(inst, r_idx, r, ship_pos, POCKET_RADIUS_M))
        })
    })
}

/// Checks whether activating a MicroWarpdrive is permitted at `ship_pos` in `sys_id`.
/// Refuses MWD activation (`ModuleNotAllowedInDeadspace`) when the pocket's dungeon or room data
/// disallows MicroWarpdrives.
pub fn check_deadspace_mwd_allowed(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    ship_pos: [f64; 3],
) -> Result<(), BridgeRpcError> {
    if has_dungeon_runtime_store(ctx) {
        let matched_room = read_dungeon_runtime_store(ctx, |store| {
            store.instances.values().find_map(|inst| {
                if instance_is_finished(inst) || inst.solar_system_id != sys_id {
                    return None;
                }
                inst.rooms
                    .iter()
                    .enumerate()
                    .find(|(r_idx, r)| {
                        room_contains_pocket_pos(inst, *r_idx, r, ship_pos, POCKET_RADIUS_M)
                    })
                    .map(|(_, r)| (inst.clone(), r.clone()))
            })
        });
        if let Some((inst, r)) = matched_room {
            return if is_pocket_mwd_disallowed(ctx, &inst, &r) {
                Err(BridgeRpcError::user_error("ModuleNotAllowedInDeadspace"))
            } else {
                Ok(())
            };
        }
    }

    let editor_disallowed = crate::services::pve_dungeon_editor::with_dungeon_editor_store(ctx, |ed| {
        ed.active_editors.values().any(|st| {
            st.solar_system_id == Some(sys_id)
                && dist3(ship_pos, [0.0, 0.0, 0.0]) <= POCKET_RADIUS_M
                && st.dungeon_id.is_some_and(|did| {
                    ed.dungeon_disallow_mwd.get(&did).copied().unwrap_or(false)
                        || st.room_id.is_some_and(|rid| {
                            ed.rooms_by_dungeon
                                .get(&did)
                                .and_then(|rs| rs.iter().find(|r| r.room_id == rid))
                                .is_some_and(|r| r.disallow_mwd)
                        })
                })
        })
    });
    if editor_disallowed {
        return Err(BridgeRpcError::user_error("ModuleNotAllowedInDeadspace"));
    }

    let nearby_site_dungeon_ids: Vec<u32> = read_pve_exploration_reg(ctx, |reg| {
        reg.sites_by_system
            .get(&sys_id)
            .map(|sites| {
                sites
                    .iter()
                    .filter(|s| !s.cleared && dist3(ship_pos, s.position) <= POCKET_RADIUS_M)
                    .map(|s| s.dungeon_id)
                    .collect()
            })
            .unwrap_or_default()
    });
    let site_disallowed = nearby_site_dungeon_ids.into_iter().any(|did| {
        crate::services::pve_dungeon_editor::get_dungeon_or_room_mwd_override(
            ctx,
            did as i64,
            None,
        )
        .unwrap_or_else(|| {
            ctx.sde
                .dungeons
                .get(&did)
                .is_some_and(|d| d.archetype_id == Some(51))
        })
    });
    if site_disallowed {
        return Err(BridgeRpcError::user_error("ModuleNotAllowedInDeadspace"));
    }

    Ok(())
}

/// Checks deadspace warp rules:
/// - Warping directly into gated rooms (`room.gated == true`, e.g. Room 2+ or behind an entry gate) is impossible.
/// - Warping directly between two distinct pockets of the same deadspace complex is impossible.
pub fn check_deadspace_warp_allowed(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    ship_pos: [f64; 3],
    anchor_pos: [f64; 3],
    dest_pos: [f64; 3],
) -> Result<(), BridgeRpcError> {
    let blocked = has_dungeon_runtime_store(ctx)
        && read_dungeon_runtime_store(ctx, |store| {
            for inst in store.instances.values() {
                if instance_is_finished(inst) || inst.solar_system_id != sys_id {
                    continue;
                }
                let src_room_idx = inst
                    .rooms
                    .iter()
                    .enumerate()
                    .find(|(r_idx, r)| {
                        room_contains_pocket_pos(inst, *r_idx, r, ship_pos, POCKET_RADIUS_M)
                    })
                    .map(|(_, r)| r.room_index);
                for (r_idx, r) in inst.rooms.iter().enumerate() {
                    let targets_room =
                        room_contains_pocket_pos(inst, r_idx, r, anchor_pos, POCKET_RADIUS_M)
                            || room_contains_pocket_pos(inst, r_idx, r, dest_pos, POCKET_RADIUS_M);
                    if !targets_room {
                        continue;
                    }
                    if r.gated {
                        return true;
                    }
                    if let Some(src_idx) = src_room_idx {
                        if src_idx != r.room_index {
                            return true;
                        }
                    }
                }
            }
            false
        });
    if blocked
        || crate::services::insurgency_complex_ops::is_warp_into_outpost_raid_pocket_blocked(
            ctx, sys_id, ship_pos, anchor_pos, dest_pos,
        )
    {
        return Err(BridgeRpcError::user_error("NaturalPhenomenaDisruptingWarp"));
    }

    let blocked_site = read_pve_exploration_reg(ctx, |reg| {
        reg.sites_by_system.get(&sys_id).is_some_and(|sites| {
            sites.iter().any(|s| {
                !s.cleared
                    && s.total_rooms > 1
                    && s.current_room > 1
                    && {
                        let gated_pos = [
                            s.position[0] + ((s.current_room - 1) as f64) * 500_000.0,
                            s.position[1],
                            s.position[2],
                        ];
                        dist3(anchor_pos, gated_pos) <= POCKET_RADIUS_M
                            || dist3(dest_pos, gated_pos) <= POCKET_RADIUS_M
                    }
            })
        })
    });
    if blocked_site {
        return Err(BridgeRpcError::user_error("NaturalPhenomenaDisruptingWarp"));
    }

    Ok(())
}

fn is_unowned_runtime_dungeon_accessible(
    ctx: &ServerContext,
    char_id: i64,
    sys_id: SolarSystemId,
    inst_id: i64,
    dungeon_id: u32,
) -> bool {
    let probe_site_ids: Vec<String> =
        crate::services::pve_exploration::read_pve_exploration_reg(ctx, |reg| {
            let Some(sites) = reg.sites_by_system.get(&sys_id) else {
                return Vec::new();
            };
            let has_exact_inst = sites
                .iter()
                .any(|s| !s.cleared && s.expedition_instance_id == Some(inst_id));
            sites
                .iter()
                .filter(|s| {
                    !s.cleared
                        && s.probe_required
                        && if has_exact_inst {
                            s.expedition_instance_id == Some(inst_id)
                        } else {
                            s.dungeon_id == dungeon_id
                        }
                })
                .map(|s| s.site_id.clone())
                .collect()
        });
    if probe_site_ids.is_empty() {
        return true;
    }
    let sys_sigs = ctx.db.with_snapshot(|snap| {
        snap.cosmic_signatures
            .get(&sys_id)
            .cloned()
            .unwrap_or_default()
    });
    sys_sigs.iter().any(|sig| {
        !sig.collapsed
            && probe_site_ids.iter().any(|sid| sid == &sig.signature_id)
            && ctx.effective_signature_signal_strength(char_id, sys_id, sig) >= 1.0
    })
}

fn natural_phenomena_warp_err() -> BridgeRpcError {
    BridgeRpcError::with_client_error(
        "NaturalPhenomenaDisruptingWarp",
        "Natural phenomena are disrupting warp",
        eve_proto::ClientError::CustomInfo("Natural phenomena are disrupting warp".into()),
    )
}

/// Resolves warp-in coordinates for `"dungeon"` / `"location"` / `"epinstance"` stuff types.
pub fn resolve_deadspace_or_escalation_warp_target(
    ctx: &ServerContext,
    char_id: i64,
    sys_id: SolarSystemId,
    stuff_type: &str,
    target_id: i64,
    target_str_opt: Option<&str>,
) -> Result<Option<DVec3>, BridgeRpcError> {
    if stuff_type == "epinstance" {
        let esc_target_id = EscalationInstanceId::new(target_id);
        let (pos_opt, matched_entry, now_unix) =
            crate::services::pve_exploration::read_pve_exploration_reg(ctx, |reg| {
                let now_unix = crate::services::pve_exploration::now_sec(reg);
                let matches: Vec<_> = reg
                    .escalations_by_char
                    .get(&char_id)
                    .map(|list| {
                        list.iter()
                            .filter(|e| EscalationInstanceId::new(e.instance_id) == esc_target_id)
                            .collect()
                    })
                    .unwrap_or_default();
                if matches.is_empty() {
                    return (None, false, now_unix);
                }
                let pos = matches.into_iter().find_map(|esc| {
                    if esc.destination_solar_system_id != sys_id {
                        return None;
                    }
                    if esc.expires_at_sec > 0 && esc.expires_at_sec <= now_unix {
                        return None;
                    }
                    let esc_key = format!("EXP-{}-{}", esc.instance_id, esc.step_index);
                    let key_ref = crate::services::pve_exploration::ExplorationSiteKeyRef(&esc_key);
                    reg.sites_by_system.get(&sys_id).and_then(|sites| {
                        crate::services::pve_exploration::find_site_by_canonical_key_in_reg(
                            reg, sites, key_ref, now_unix,
                        )
                        .filter(|s| {
                            !s.cleared
                                && !crate::services::pve_exploration::is_site_expired_in_reg(
                                    reg, s, now_unix,
                                )
                        })
                        .map(|s| DVec3::from_array(s.position))
                    })
                });
                (pos, true, now_unix)
            });
        if pos_opt.is_some() {
            return Ok(pos_opt);
        }
        if matched_entry {
            return Ok(None);
        }
        let bm_pos = ctx.db.with_snapshot(|snap| {
            snap.bookmarks
                .values()
                .find(|b| {
                    b.owner_id == char_id
                        && b.solar_system_id == sys_id
                        && b.item_id == Some(esc_target_id.as_i64())
                        && !crate::services::fleet_social::is_bookmark_expired_in_snap(
                            snap,
                            b.bookmark_id,
                            now_unix,
                        )
                        && !b.label.starts_with("Escalation:")
                })
                .map(|b| DVec3::new(b.x, b.y, b.z))
        });
        return Ok(bm_pos);
    }

    if matches!(stuff_type, "dungeon" | "location") {
        let explicit_inst_id = target_str_opt
            .and_then(|s| s.strip_prefix("DNG-INST-"))
            .and_then(|rest| rest.parse::<i64>().ok())
            .filter(|&v| v > 0)
            .map(DungeonInstanceId::new);
        let candidate_inst_id = (target_id > 0).then_some(DungeonInstanceId::new(target_id));
        let candidate_template_id =
            u32::try_from(target_id).ok().filter(|&v| v > 0).map(DungeonTemplateId::new);

        let inst_outcome = |inst: &DungeonRuntimeInstance| -> Result<DVec3, BridgeRpcError> {
            if let Some(entry_pos) = inst.entry_gate_pos {
                return Ok(DVec3::from_array(entry_pos));
            }
            if let Some(first_room) = inst.rooms.first() {
                if first_room.gated {
                    return Err(natural_phenomena_warp_err());
                }
                return Ok(DVec3::from_array(first_room.position));
            }
            Ok(DVec3::from_array(inst.base_position))
        };

        let inst_candidates: Vec<(i64, u32, i64, String, Result<DVec3, BridgeRpcError>)> = ctx
            .db
            .with_snapshot(|snap| {
                let mut candidates = Vec::new();
                let mut seen_ids = std::collections::HashSet::new();
                let is_eligible = |inst: &DungeonRuntimeInstance| {
                    !instance_is_finished(inst)
                        && inst.solar_system_id == sys_id
                        && (inst.owner_char_id <= 0 || inst.owner_char_id == char_id)
                };

                // Priority 1: Explicit `DungeonInstanceId` (`DNG-INST-<id>` or numeric instance_id)
                for req_id in [explicit_inst_id, candidate_inst_id].into_iter().flatten() {
                    if let Some(inst) = snap.dungeon_runtime.get_instance(req_id) {
                        if is_eligible(inst) && seen_ids.insert(inst.instance_id) {
                            candidates.push((
                                inst.instance_id,
                                inst.dungeon_id,
                                inst.owner_char_id,
                                inst.category.clone(),
                                inst_outcome(inst),
                            ));
                        }
                    }
                }

                // Priority 2: Room ID match (only when `DNG-INST-` string wasn't specified)
                if explicit_inst_id.is_none() && target_id > 0 {
                    for inst in snap.dungeon_runtime.instances.values() {
                        if !is_eligible(inst) || seen_ids.contains(&inst.instance_id) {
                            continue;
                        }
                        if let Some(room) = inst.rooms.iter().find(|r| r.room_id == target_id) {
                            seen_ids.insert(inst.instance_id);
                            let outcome = if room.gated {
                                Err(natural_phenomena_warp_err())
                            } else {
                                Ok(DVec3::from_array(room.position))
                            };
                            candidates.push((
                                inst.instance_id,
                                inst.dungeon_id,
                                inst.owner_char_id,
                                inst.category.clone(),
                                outcome,
                            ));
                        }
                    }
                }

                // Priority 3: `DungeonTemplateId` (`dungeon_id`) match
                if explicit_inst_id.is_none() {
                    if let Some(tpl_id) = candidate_template_id {
                        for inst in snap.dungeon_runtime.instances.values() {
                            if !is_eligible(inst) || seen_ids.contains(&inst.instance_id) {
                                continue;
                            }
                            if inst.typed_template_id() == tpl_id {
                                seen_ids.insert(inst.instance_id);
                                candidates.push((
                                    inst.instance_id,
                                    inst.dungeon_id,
                                    inst.owner_char_id,
                                    inst.category.clone(),
                                    inst_outcome(inst),
                                ));
                            }
                        }
                    }
                }
                candidates
            });
        for (inst_id, dungeon_id, owner_char_id, category, outcome) in inst_candidates {
            // Public/unowned runtime instances seeded for probeable DSG-* sites must stay
            // unwarpable until the character resolves their cosmic signature to >= 100%.
            if owner_char_id == 0
                && !is_unowned_runtime_dungeon_accessible(ctx, char_id, sys_id, inst_id, dungeon_id)
            {
                continue;
            }
            if owner_char_id > 0 && matches!(category.as_str(), "Expedition" | "Escalation") {
                let active_esc =
                    crate::services::pve_exploration::read_pve_exploration_reg(ctx, |reg| {
                        let now_unix = crate::services::pve_exploration::now_sec(reg);
                        reg.escalations_by_char.get(&owner_char_id).is_some_and(|list| {
                            list.iter().any(|e| {
                                e.destination_solar_system_id == sys_id
                                    && e.expedition_dungeon_id == dungeon_id
                                    && (e.expires_at_sec == 0 || e.expires_at_sec > now_unix)
                            })
                        })
                    });
                if !active_esc {
                    continue;
                }
            }
            return outcome.map(Some);
        }

        let candidate_sites: Vec<(String, bool, DVec3)> =
            crate::services::pve_exploration::read_pve_exploration_reg(ctx, |reg| {
                let now_unix = crate::services::pve_exploration::now_sec(reg);
                let owned_exp_ids: std::collections::HashSet<i64> = reg
                    .escalations_by_char
                    .get(&char_id)
                    .map(|list| {
                        list.iter()
                            .filter(|e| e.expires_at_sec == 0 || e.expires_at_sec > now_unix)
                            .map(|e| e.instance_id)
                            .collect()
                    })
                    .unwrap_or_default();
                let target_site_id = (target_id > 0).then_some(ExplorationSiteId::new(target_id));
                reg.sites_by_system
                    .get(&sys_id)
                    .map(|sites| {
                        sites
                            .iter()
                            .filter(|s| {
                                if s.cleared
                                    || crate::services::pve_exploration::is_site_expired_in_reg(
                                        reg, s, now_unix,
                                    )
                                {
                                    return false;
                                }
                                let matches_key =
                                    target_str_opt.is_some_and(|k| s.site_id == k);
                                let matches_site_id = target_site_id
                                    .is_some_and(|sid| reg.exploration_site_id(s) == sid);
                                let matches_tpl = candidate_template_id
                                    .is_some_and(|did| DungeonTemplateId::new(s.dungeon_id) == did);
                                if !(matches_key || matches_site_id || matches_tpl) {
                                    return false;
                                }
                                if s.site_category == "Expedition" {
                                    match s.expedition_instance_id {
                                        Some(exp_id) if owned_exp_ids.contains(&exp_id) => {}
                                        _ => return false,
                                    }
                                }
                                true
                            })
                            .map(|s| (s.site_id.clone(), s.probe_required, DVec3::from_array(s.position)))
                            .collect()
                    })
                    .unwrap_or_default()
            });
        let site_pos = candidate_sites.into_iter().find_map(|(site_id, probe_required, pos)| {
            if !probe_required {
                return Some(pos);
            }
            let scanned = ctx.db.with_snapshot(|snap| {
                snap.cosmic_signatures
                    .get(&sys_id)
                    .into_iter()
                    .flatten()
                    .any(|sig| {
                        sig.signature_id == site_id
                            && !sig.collapsed
                            && ctx.effective_signature_signal_strength(char_id, sys_id, sig) >= 1.0
                    })
            });
            scanned.then_some(pos)
        });
        if site_pos.is_some() {
            return Ok(site_pos);
        }
        if let Some(pos) = ops::resolve_fw_or_special_warp_target(
            ctx,
            sys_id,
            target_id,
            target_str_opt,
        ) {
            return Ok(Some(pos));
        }
    }

    Ok(None)
}

/// Finds the active dungeon ID near `pos` in `sys_id` (both `DungeonRuntimeStore` and `PveExplorationRegistry`).
pub fn find_active_dungeon_id_at_position(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    pos: [f64; 3],
) -> Option<u32> {
    ops::find_active_dungeon_id_at_position(ctx, sys_id, pos)
}

/// Called on warp initiation (`CmdWarpToStuff`) to emit `OnExitingDungeon` when leaving an active dungeon pocket.
pub fn on_warp_start_leave_dungeon_if_needed(
    ctx: &ServerContext,
    char_id: i64,
    sys_id: SolarSystemId,
    origin_pos: [f64; 3],
    dest_pos: [f64; 3],
) {
    let origin_did = find_active_dungeon_id_at_position(ctx, sys_id, origin_pos);
    let dest_did = find_active_dungeon_id_at_position(ctx, sys_id, dest_pos);
    if origin_did.is_some() && origin_did != dest_did {
        emit_exiting_dungeon_on_warp_if_present(ctx, char_id, sys_id, origin_pos);
    }
}

fn resolve_site_entry_at_pos(
    ctx: &ServerContext,
    char_id: i64,
    sys_id: SolarSystemId,
    landed_pos: [f64; 3],
) -> Option<(String, Option<u32>, Option<String>, String)> {
    if let Some(site) = read_pve_exploration_reg(ctx, |reg| {
        reg.sites_by_system.get(&sys_id).and_then(|sites| {
            sites
                .iter()
                .find(|s| !s.cleared && dist3(landed_pos, s.position) <= 150_000.0)
                .map(|s| {
                    (
                        s.site_id.clone(),
                        Some(s.dungeon_id),
                        Some(s.dungeon_name.clone()),
                        s.site_category.clone(),
                    )
                })
        })
    }) {
        return Some(site);
    }
    ctx.db.with_snapshot(|snap| {
        if let Some(anoms) = snap.cosmic_anomalies.get(&sys_id) {
            for a in anoms {
                if !a.cleared && dist3(landed_pos, a.position) <= 150_000.0 {
                    return Some((
                        a.anomaly_id.clone(),
                        None,
                        Some(a.name.clone()),
                        a.anomaly_type.clone(),
                    ));
                }
            }
        }
        if let Some(sigs) = snap.cosmic_signatures.get(&sys_id) {
            for s in sigs {
                if s.collapsed {
                    continue;
                }
                let sig_pos = crate::services::space_destiny::space_pochven_wormholes::pochven_wormhole_ball_position(
                    &ctx.sde, s,
                )
                .to_array();
                if dist3(landed_pos, sig_pos) <= 150_000.0
                    && ctx.effective_signature_signal_strength(char_id, sys_id, s) >= 1.0
                {
                    return Some((
                        s.signature_id.clone(),
                        None,
                        Some(s.type_name.clone()),
                        s.sig_group.clone(),
                    ));
                }
            }
        }
        None
    })
}

fn dispatch_site_entry_career_goals(
    ctx: &ServerContext,
    char_id: i64,
    sys_id: SolarSystemId,
    site_id: &str,
    dungeon_id: Option<u32>,
    dungeon_name: Option<&str>,
    site_category: &str,
) {
    crate::quasar_gateway::isolate_side_effect("pve_dungeon_runtime.warp_arrival.career_site_entry", || {
        crate::services::quasar_career_goals::record_site_entry_career_goals(
            ctx,
            char_id,
            sys_id,
            site_id,
            dungeon_id,
            dungeon_name,
            site_category,
        );
    });
}

/// Called upon warp landing (`OnWarpFinished`) to materialize room entities and emit
/// `OnEnteringDungeonRoom` + `OnDistributionDungeonEntered` / `OnEscalatingPathDungeonEntered`.
pub async fn on_warp_landed_at_dungeon_pocket_if_present(
    ctx: &ServerContext,
    char_id: i64,
    sys_id: SolarSystemId,
    landed_pos: [f64; 3],
) {
    if crate::services::pve_homefront_sites::on_homefront_warp_landed(
        ctx,
        char_id,
        sys_id,
        DVec3::from_array(landed_pos),
    )
    .await
    {
        return;
    }

    // Check if landing in a DungeonRuntimeInstance room
    let runtime_entry = read_dungeon_runtime_store(ctx, |store| {
        store.instances.values().find_map(|inst| {
            if instance_is_finished(inst)
                || inst.solar_system_id != sys_id
                || inst.category == "Flashpoint"
            {
                return None;
            }
            let (room_vec_idx, room) = inst
                .rooms
                .iter()
                .enumerate()
                .find(|(r_idx, r)| {
                    room_contains_pocket_pos(inst, *r_idx, r, landed_pos, 150_000.0)
                        || (*r_idx == 0
                            && inst
                                .entry_gate_pos
                                .is_some_and(|ep| dist3(landed_pos, ep) <= 150_000.0))
                })?;
            let behind_unentered_gate = !room.entered
                && (room.gated
                    || (room_vec_idx == 0
                        && (inst.entry_gate_pos.is_some()
                            || inst.entry_gate_ball_id.is_some())));
            Some((
                inst.instance_id,
                inst.dungeon_id,
                inst.category.clone(),
                inst.dungeon_name.clone(),
                room.room_index,
                room_vec_idx,
                room.room_id,
                room.position,
                inst.rooms.len(),
                inst.created_unix,
                behind_unentered_gate,
            ))
        })
    });

    if let Some((
        inst_id,
        dungeon_id,
        category,
        dungeon_name,
        room_idx,
        room_vec_idx,
        room_id,
        room_pos,
        total_rooms,
        created_unix,
        behind_unentered_gate,
    )) = runtime_entry
    {
        if room_vec_idx > 0 && behind_unentered_gate {
            return;
        }
        let mut esc_instance_id = None;
        if matches!(category.as_str(), "Expedition" | "Escalation") {
            with_pve_exploration_reg(ctx, |reg| {
                if let Some(list) = reg.escalations_by_char.get_mut(&char_id) {
                    for esc in list.iter_mut() {
                        if esc.destination_solar_system_id == sys_id
                            && esc.expedition_dungeon_id == dungeon_id
                        {
                            esc_instance_id = Some(esc.instance_id);
                            if !esc.dungeons_entered.contains(&dungeon_id) {
                                esc.dungeons_entered.push(dungeon_id);
                            }
                        }
                    }
                }
            });
        }
        if materialize_dungeon_room_entities(ctx, sys_id, inst_id, room_vec_idx)
            .await
            .is_ok()
            && !behind_unentered_gate
        {
            on_dungeon_room_entered(ctx, sys_id, inst_id, room_vec_idx, char_id).await;
        }
        let expiry_ft = unix_to_filetime_ticks(created_unix.saturating_add(86_400));
        emit_dungeon_progression_notifications(
            ctx,
            char_id,
            sys_id,
            &category,
            &dungeon_name,
            i64::from(dungeon_id),
            esc_instance_id.unwrap_or(inst_id),
            room_id,
            room_pos,
            room_idx as u8,
            total_rooms.max(1) as u8,
            false,
            expiry_ft,
        );
        if !matches!(category.as_str(), "Mission" | "Homefront") {
            let (site_id, did, dname, cat) = match resolve_site_entry_at_pos(ctx, char_id, sys_id, landed_pos) {
                Some((sid, did_opt, name_opt, sig_cat)) => {
                    let effective_cat = if category == "Deadspace" { sig_cat } else { category.clone() };
                    (sid, Some(dungeon_id).or(did_opt), Some(dungeon_name.clone()).or(name_opt), effective_cat)
                }
                None => (format!("DNG-{inst_id}"), Some(dungeon_id), Some(dungeon_name.clone()), category.clone()),
            };
            dispatch_site_entry_career_goals(ctx, char_id, sys_id, &site_id, did, dname.as_deref(), &cat);
        }
        return;
    }

    // Check if landing in a PveExplorationRegistry site pocket
    let site_entry = with_pve_exploration_reg(ctx, |reg| {
        let found = reg.sites_by_system.get(&sys_id).and_then(|sites| {
            sites
                .iter()
                .find(|s| !s.cleared && dist3(landed_pos, s.position) <= 150_000.0)
                .map(|s| {
                    (
                        s.site_id.clone(),
                        s.dungeon_id,
                        s.site_category.clone(),
                        s.dungeon_name.clone(),
                        s.position,
                        s.current_wave,
                        s.total_waves,
                        s.created_unix,
                        s.lifetime_seconds,
                        s.expedition_instance_id,
                    )
                })
        });
        if let Some((_, did, _, _, _, _, _, _, _, exp_inst_opt)) = &found {
            if let Some(list) = reg.escalations_by_char.get_mut(&char_id) {
                for esc in list.iter_mut() {
                    if (exp_inst_opt == &Some(esc.instance_id)
                        || (esc.destination_solar_system_id == sys_id && esc.expedition_dungeon_id == *did))
                        && !esc.dungeons_entered.contains(did)
                    {
                        esc.dungeons_entered.push(*did);
                    }
                }
            }
        }
        found
    });

    if let Some((site_id, did, category, dungeon_name, site_pos, cur_wave, total_waves, created_unix, lifetime_sec, exp_inst_opt)) =
        site_entry
    {
        let life = if lifetime_sec == 0 { 86_400 } else { lifetime_sec };
        let expiry_ft = unix_to_filetime_ticks(created_unix.saturating_add(life));
        let fallback_room_id = i64::from(did.max(1)) * 100 + i64::from(cur_wave);
        emit_dungeon_progression_notifications(
            ctx,
            char_id,
            sys_id,
            &category,
            &dungeon_name,
            i64::from(did),
            exp_inst_opt.unwrap_or(fallback_room_id),
            fallback_room_id,
            site_pos,
            cur_wave,
            total_waves,
            false,
            expiry_ft,
        );
        dispatch_site_entry_career_goals(
            ctx,
            char_id,
            sys_id,
            &site_id,
            Some(did),
            Some(&dungeon_name),
            &category,
        );
        return;
    }

    if let Some((site_id, did_opt, dname_opt, cat)) = resolve_site_entry_at_pos(ctx, char_id, sys_id, landed_pos) {
        dispatch_site_entry_career_goals(
            ctx,
            char_id,
            sys_id,
            &site_id,
            did_opt,
            dname_opt.as_deref(),
            &cat,
        );
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct MaterializedDungeonRoomEntities {
    pub object_ids: Vec<i64>,
    pub npc_ids: Vec<i64>,
    pub next_gate_id: Option<i64>,
}

#[derive(Clone, Debug)]
pub(crate) struct NpcSpawnOrder {
    pub(crate) ball_id: i64,
    pub(crate) type_id: u32,
    pub(crate) group_id: u8,
    pub(crate) pos: [f64; 3],
    pub(crate) custom_name: Option<String>,
    pub(crate) destructible: bool,
}

pub(crate) fn npc_spawn_position(
    room_pos: [f64; 3],
    grp_idx: usize,
    spawn_index: usize,
    offset_m: Option<[i64; 3]>,
) -> [f64; 3] {
    if let Some([x, y, z]) = offset_m {
        [
            room_pos[0] + x as f64,
            room_pos[1] + y as f64,
            room_pos[2] + z as f64,
        ]
    } else {
        let angle = ((grp_idx as f64) * 1.1) + ((spawn_index as f64) * 0.9);
        let dist = 8_000.0 + ((spawn_index as f64) * 1_500.0);
        [
            room_pos[0] + angle.cos() * dist,
            room_pos[1],
            room_pos[2] + angle.sin() * dist,
        ]
    }
}

pub(crate) fn plan_npc_group_spawns_in_snap(
    snap: &mut WorldStateSnapshot,
    room: &mut DungeonRuntimeRoom,
    grp_idx: usize,
    now_unix: u64,
    out_orders: &mut Vec<NpcSpawnOrder>,
) {
    let entries = room.npc_groups[grp_idx].effective_entries();
    if entries.is_empty() {
        let g = &mut room.npc_groups[grp_idx];
        g.spawned = true;
        g.cleared = true;
        return;
    }
    let total_npcs: usize = entries.iter().map(|e| e.count.max(1)).sum();
    let alloc_ids: Vec<i64> = (0..total_npcs).map(|_| snap.alloc_entity_id()).collect();
    let room_pos = room.position;
    let mut spawned_any = false;
    {
        let grp = &mut room.npc_groups[grp_idx];
        grp.spawned = true;
        grp.cleared = false;
        grp.spawn_count = grp.spawn_count.saturating_add(1);
        grp.spawn_started_unix = Some(now_unix);

        let base_spawn_idx = usize::try_from(grp.kills_recorded)
            .unwrap_or(0)
            .saturating_add(grp.npc_ball_ids.len());
        let mut id_iter = alloc_ids.into_iter();
        let mut spawn_idx = 0usize;
        for entry in entries {
            let count = entry.count.max(1);
            for sub_idx in 0..count {
                let Some(npc_id) = id_iter.next() else {
                    break;
                };
                let offset_m = entry.relative_offsets_m.get(sub_idx).copied();
                let cur_spawn_idx = base_spawn_idx + spawn_idx;
                let pos = npc_spawn_position(room_pos, grp_idx, cur_spawn_idx, offset_m);
                let destructible = entry.destructible.unwrap_or(true);
                spawn_idx += 1;
                spawned_any = true;
                grp.npc_ball_ids.push(npc_id);
                grp.npc_ball_meta.insert(
                    npc_id,
                    DungeonSpawnedNpcMeta {
                        type_id: entry.type_id,
                        offset_m,
                        custom_loot: entry.custom_loot,
                        destructible,
                        spawn_index: cur_spawn_idx,
                    },
                );
                if let Some(ref tag) = entry.tag {
                    grp.npc_ball_tags.insert(npc_id, tag.clone());
                }
                if let Some(ref name) = entry.custom_name {
                    grp.npc_ball_names.insert(npc_id, name.clone());
                }
                if let Some(bounty) = entry.bounty_isk_override {
                    grp.npc_ball_bounties.insert(npc_id, bounty);
                }
                out_orders.push(NpcSpawnOrder {
                    ball_id: npc_id,
                    type_id: entry.type_id,
                    group_id: grp.group_id,
                    pos,
                    custom_name: entry.custom_name.clone(),
                    destructible,
                });
            }
        }
    }
    if spawned_any {
        room.cleared = false;
    }
}

pub(crate) async fn spawn_object_ball(
    sys_handle: &crate::system_actor::SystemHandle,
    sde: &eve_sde::SdeRegistry,
    ball_id: i64,
    type_id: u32,
    pos: [f64; 3],
    radius: f64,
    destructible: bool,
) {
    let tid = if type_id > 0 {
        type_id
    } else {
        DEFAULT_CONTAINER_TYPE_ID
    };
    let mut ball = make_site_npc_ball(ball_id, tid, DVec3::from_array(pos));
    ball.radius = radius.max(100.0);
    if destructible {
        let cs = build_type_combat_state_from_sde(sde, tid);
        let _ = sys_handle.spawn_ball_with_combat(ball, cs).await;
    } else {
        let _ = sys_handle.spawn_ball(ball).await;
    }
}

pub(crate) async fn spawn_gate_ball(
    sys_handle: &crate::system_actor::SystemHandle,
    ball_id: i64,
    pos: [f64; 3],
    gate_type_id: Option<u32>,
) {
    let tid = gate_type_id
        .filter(|&id| id > 0)
        .unwrap_or(ACCELERATION_GATE_TYPE_ID);
    let mut ball = make_site_npc_ball(ball_id, tid, DVec3::from_array(pos));
    ball.radius = 1_500.0;
    let _ = sys_handle.spawn_ball(ball).await;
}

pub(crate) fn insert_container_and_loot_in_snap(
    snap: &mut WorldStateSnapshot,
    sys_id: SolarSystemId,
    container_ball_id: i64,
    container_type_id: u32,
    position: [f64; 3],
    loot_items: &[(u32, i32)],
    owner_char_id: i64,
) {
    let cont_owner = if owner_char_id > 0 {
        owner_char_id
    } else {
        500_010
    };
    let tid = if container_type_id > 0 {
        container_type_id
    } else {
        DEFAULT_CONTAINER_TYPE_ID
    };
    snap.item_positions.insert(container_ball_id, position);
    snap.items
        .entry(container_ball_id)
        .or_insert_with(|| InventoryItemRecord {
            item_id: container_ball_id,
            type_id: tid,
            owner_id: cont_owner,
            location_id: i64::from(sys_id),
            flag: InventoryFlag::Hangar,
            quantity: 1,
            singleton: true,
            version: 1,
            ..Default::default()
        });
    for &(loot_tid, loot_qty) in loot_items {
        let lid = snap.alloc_entity_id();
        snap.items.insert(
            lid,
            InventoryItemRecord {
                item_id: lid,
                type_id: loot_tid,
                owner_id: cont_owner,
                location_id: container_ball_id,
                flag: InventoryFlag::Cargo,
                quantity: loot_qty.max(1),
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    }
}

pub(crate) fn cleanup_instance_containers_in_snap(
    snap: &mut WorldStateSnapshot,
    ball_ids: &[i64],
) {
    if ball_ids.is_empty() {
        return;
    }
    let id_set: HashSet<i64> = ball_ids.iter().copied().collect();
    snap.items
        .retain(|item_id, rec| !id_set.contains(item_id) && !id_set.contains(&rec.location_id));
    for bid in &id_set {
        snap.item_positions.remove(bid);
        snap.sp6_lifecycle.space_object_meta.remove(bid);
    }
}

pub(crate) fn has_unlooted_containers_in_snap(
    snap: &WorldStateSnapshot,
    inst: &DungeonRuntimeInstance,
) -> bool {
    let live_containers: HashSet<i64> = inst
        .rooms
        .iter()
        .flat_map(|r| r.objects.iter())
        .filter(|o| o.is_container && o.spawned && !o.destroyed && o.ball_id > 0)
        .map(|o| o.ball_id)
        .collect();
    if live_containers.is_empty() {
        return false;
    }
    snap.items
        .values()
        .any(|it| it.quantity > 0 && live_containers.contains(&it.location_id))
}

pub(crate) fn collect_all_room_gate_ids(room: &DungeonRuntimeRoom) -> Vec<i64> {
    let mut out = Vec::new();
    if let Some(gid) = room.gate_ball_id {
        out.push(gid);
    }
    for link in &room.gate_links {
        if let Some(gid) = link.gate_ball_id {
            if !out.contains(&gid) {
                out.push(gid);
            }
        }
    }
    out
}

/// Materializes a room's objects, acceleration gate, and immediate NPC groups into the system ballpark.
/// Leaves `room.entered = false` (`I1`) until `on_dungeon_room_entered` marks pilot arrival.
pub(crate) async fn materialize_dungeon_room_entities(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    instance_id: i64,
    room_vec_idx: usize,
) -> Result<MaterializedDungeonRoomEntities, BridgeRpcError> {
    let now_unix = ctx.now_unix();
    let (gate_plans, obj_plans, npc_plans, obj_ids, npc_ids, gate_id_opt, already_spawned) =
        ctx.db.try_with_mut_snapshot(|snap| {
            let mut inst = snap
                .dungeon_runtime
                .instances
                .remove(&instance_id)
                .ok_or_else(|| BridgeRpcError::user_error("DungeonInstanceNotFound"))?;
            if instance_is_finished(&inst) {
                snap.dungeon_runtime.instances.insert(instance_id, inst);
                return Err(BridgeRpcError::user_error("DungeonAlreadyCompleted"));
            }
            if inst.rooms.get(room_vec_idx).is_none() {
                snap.dungeon_runtime.instances.insert(instance_id, inst);
                return Err(BridgeRpcError::user_error("DungeonRoomNotFound"));
            }
            let was_room_spawned = inst.rooms[room_vec_idx].spawned;
            let total_rooms = inst.rooms.len();
            let owner_char_id = inst.owner_char_id;

            let mut gate_plans: Vec<(i64, [f64; 3], Option<u32>)> = Vec::new();
            let mut obj_plans: Vec<(i64, u32, [f64; 3], f64, bool)> = Vec::new();
            let mut npc_plans: Vec<NpcSpawnOrder> = Vec::new();
            let mut obj_ids: Vec<i64> = Vec::new();
            let mut npc_ids: Vec<i64> = Vec::new();

            if room_vec_idx == 0 {
                if let Some(ep) = inst.entry_gate_pos {
                    let first_eg = inst.entry_gate_ball_id.is_none() || !was_room_spawned;
                    let eg_id = inst
                        .entry_gate_ball_id
                        .unwrap_or_else(|| snap.alloc_entity_id());
                    inst.entry_gate_ball_id = Some(eg_id);
                    if first_eg {
                        gate_plans.push((eg_id, ep, None));
                    }
                }
            }

            let obj_len = inst.rooms[room_vec_idx].objects.len();
            for oi in 0..obj_len {
                let (skip_obj, needs_cont, bid, tid, pos, radius, destructible, loot_items) = {
                    let obj = &mut inst.rooms[room_vec_idx].objects[oi];
                    if obj.destroyed || (obj.spawn_on_trigger && !obj.spawned) {
                        (true, false, obj.ball_id, 0, [0.0; 3], 0.0, false, Vec::new())
                    } else {
                        if obj.ball_id <= 0 {
                            obj.ball_id = snap.alloc_entity_id();
                        }
                        let newly_spawned = !obj.spawned;
                        if newly_spawned {
                            obj.spawned = true;
                        }
                        (
                            false,
                            newly_spawned && obj.is_container,
                            obj.ball_id,
                            obj.type_id,
                            obj.position,
                            obj.radius,
                            obj.destructible,
                            if newly_spawned && obj.is_container {
                                obj.loot_items.clone()
                            } else {
                                Vec::new()
                            },
                        )
                    }
                };
                if skip_obj {
                    continue;
                }
                obj_ids.push(bid);
                if needs_cont && !snap.items.contains_key(&bid) {
                    insert_container_and_loot_in_snap(
                        snap,
                        sys_id,
                        bid,
                        tid,
                        pos,
                        &loot_items,
                        owner_char_id,
                    );
                }
                if !was_room_spawned || needs_cont {
                    obj_plans.push((bid, tid, pos, radius, destructible));
                }
            }

            let has_gate_links = !inst.rooms[room_vec_idx].gate_links.is_empty();
            if has_gate_links {
                let gl_len = inst.rooms[room_vec_idx].gate_links.len();
                for gi in 0..gl_len {
                    let need_id = inst.rooms[room_vec_idx].gate_links[gi]
                        .gate_ball_id
                        .is_none();
                    let allocated = need_id.then(|| snap.alloc_entity_id());
                    let room = &mut inst.rooms[room_vec_idx];
                    let gl = &mut room.gate_links[gi];
                    if let Some(new_gid) = allocated {
                        gl.gate_ball_id = Some(new_gid);
                    }
                    let newly_spawned = !gl.spawned;
                    if newly_spawned {
                        gl.spawned = true;
                    }
                    if room.gate_ball_id.is_none() {
                        room.gate_ball_id = gl.gate_ball_id;
                    }
                    if newly_spawned || !was_room_spawned {
                        if let Some(gid) = gl.gate_ball_id {
                            gate_plans.push((gid, gl.gate_position, gl.gate_type_id));
                        }
                    }
                }
            } else if room_vec_idx + 1 < total_rooms {
                let need_id = inst.rooms[room_vec_idx].gate_ball_id.is_none();
                let allocated = need_id.then(|| snap.alloc_entity_id());
                let room = &mut inst.rooms[room_vec_idx];
                if let Some(new_gid) = allocated {
                    room.gate_ball_id = Some(new_gid);
                }
                if let Some(gid) = room.gate_ball_id {
                    if allocated.is_some() || !was_room_spawned {
                        let gate_pos = [
                            room.position[0] + 5_000.0,
                            room.position[1],
                            room.position[2],
                        ];
                        gate_plans.push((gid, gate_pos, None));
                    }
                }
            }

            let grp_len = inst.rooms[room_vec_idx].npc_groups.len();
            for g_idx in 0..grp_len {
                let should_spawn = {
                    let g = &inst.rooms[room_vec_idx].npc_groups[g_idx];
                    g.spawn_immediately && !g.spawned && !g.cleared
                };
                if should_spawn {
                    plan_npc_group_spawns_in_snap(
                        snap,
                        &mut inst.rooms[room_vec_idx],
                        g_idx,
                        now_unix,
                        &mut npc_plans,
                    );
                }
                npc_ids.extend(inst.rooms[room_vec_idx].npc_groups[g_idx].npc_ball_ids.iter().copied());
            }

            let room = &mut inst.rooms[room_vec_idx];
            room.spawned = true;
            let gate_id_opt = room
                .gate_ball_id
                .or_else(|| room.gate_links.first().and_then(|gl| gl.gate_ball_id));

            snap.dungeon_runtime.instances.insert(instance_id, inst);

            Ok((
                gate_plans,
                obj_plans,
                npc_plans,
                obj_ids,
                npc_ids,
                gate_id_opt,
                was_room_spawned,
            ))
        })?;

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for (ball_id, pos, gate_type_id) in gate_plans {
        spawn_gate_ball(&sys_handle, ball_id, pos, gate_type_id).await;
    }
    for (ball_id, type_id, pos, radius, destructible) in obj_plans {
        spawn_object_ball(&sys_handle, &ctx.sde, ball_id, type_id, pos, radius, destructible).await;
    }
    if !npc_plans.is_empty() {
        actions::spawn_dungeon_npc_orders(
            actions::DungeonRuntimeDeps::from_ctx(ctx),
            sys_id,
            &sys_handle,
            npc_plans,
        )
        .await;
    }
    if already_spawned {
        let _ = reconcile_and_rematerialize_dungeon_instance(ctx, sys_id, instance_id).await;
    }

    Ok(MaterializedDungeonRoomEntities {
        object_ids: obj_ids,
        npc_ids,
        next_gate_id: gate_id_opt,
    })
}

/// Marks a dungeon room as entered (`I1`), records pilot attribution (`last_actor_char_id`),
/// and retroactively evaluates any `OnKill`/room-clear progression for NPCs destroyed prior to entry.
pub async fn on_dungeon_room_entered(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    instance_id: i64,
    room_vec_idx: usize,
    char_id: i64,
) {
    actions::on_dungeon_room_entered_impl(
        actions::DungeonRuntimeDeps::from_ctx(ctx),
        sys_id,
        instance_id,
        room_vec_idx,
        char_id,
    )
    .await;
}

/// Reconciles persisted `spawned` state with the live ballpark after restart or unload
/// and re-materializes missing gates, containers/objects (at persisted `snap.item_positions`),
/// and live NPCs without duplicating loot (`PVE-210`).
pub async fn reconcile_and_rematerialize_dungeon_instance(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    instance_id: i64,
) -> usize {
    let active_in_sys = read_dungeon_runtime_store(ctx, |store| {
        store.instances.get(&instance_id).is_some_and(|inst| {
            !inst.despawned && inst.solar_system_id == sys_id && inst.category != "Flashpoint"
        })
    });
    if !active_in_sys {
        return 0;
    }

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let view = sys_handle.view();

    let (gates_to_spawn, objects_to_spawn, npcs_to_spawn) = ctx.db.with_snapshot(|snap| {
        let Some(inst) = snap.dungeon_runtime.instances.get(&instance_id) else {
            return (Vec::new(), Vec::new(), Vec::new());
        };
        if inst.despawned || inst.solar_system_id != sys_id || inst.category == "Flashpoint" {
            return (Vec::new(), Vec::new(), Vec::new());
        }
        let mut gates: Vec<(i64, [f64; 3], Option<u32>)> = Vec::new();
        let mut objs = Vec::new();
        let mut npcs = Vec::new();

        if !instance_is_finished(inst) {
            if let (Some(eg_id), Some(eg_pos)) = (inst.entry_gate_ball_id, inst.entry_gate_pos) {
                if !view.contains_ball(eg_id) {
                    gates.push((eg_id, eg_pos, None));
                }
            }
        }

        for r in &inst.rooms {
            if !r.spawned {
                continue;
            }
            if !instance_is_finished(inst) {
                if !r.gate_links.is_empty() {
                    for gl in &r.gate_links {
                        if gl.spawned {
                            if let Some(gid) = gl.gate_ball_id {
                                if !view.contains_ball(gid) {
                                    gates.push((gid, gl.gate_position, gl.gate_type_id));
                                }
                            }
                        }
                    }
                } else if let Some(gid) = r.gate_ball_id {
                    if !view.contains_ball(gid) {
                        gates.push((gid, [r.position[0] + 5_000.0, r.position[1], r.position[2]], None));
                    }
                }
            }

            for o in &r.objects {
                let pos = snap.item_positions.get(&o.ball_id).copied().unwrap_or(o.position);
                let needs_ball = o.spawned
                    && !o.destroyed
                    && o.ball_id > 0
                    && (!instance_is_finished(inst) || o.is_container)
                    && !view.contains_ball(o.ball_id);
                if needs_ball {
                    objs.push((o.ball_id, o.type_id, pos, o.radius, o.destructible));
                }
            }

            if !instance_is_finished(inst) {
                for (g_idx, g) in r.npc_groups.iter().enumerate() {
                    if !g.spawned || g.cleared {
                        continue;
                    }
                    let entries = g.effective_entries();
                    let expanded: Vec<(&DungeonNpcSpawnEntry, usize)> = entries
                        .iter()
                        .flat_map(|e| (0..e.count.max(1)).map(move |sub_i| (e, sub_i)))
                        .collect();
                    for (idx, &npc_id) in g.npc_ball_ids.iter().enumerate() {
                        if npc_id <= 0 || view.contains_ball(npc_id) {
                            continue;
                        }
                        let meta_opt = g.npc_ball_meta.get(&npc_id);
                        let pair = meta_opt
                            .is_none()
                            .then(|| expanded.get(idx).or_else(|| expanded.first()).copied())
                            .flatten();
                        let type_id = meta_opt
                            .map(|m| m.type_id)
                            .or_else(|| pair.map(|(e, _)| e.type_id))
                            .unwrap_or(16_981);
                        let (spawn_idx, offset_m, destructible) = if let Some(m) = meta_opt {
                            (m.spawn_index, m.offset_m, m.destructible)
                        } else {
                            (
                                idx,
                                pair.and_then(|(e, sub_i)| e.relative_offsets_m.get(sub_i).copied()),
                                pair.is_none_or(|(e, _)| e.destructible.unwrap_or(true)),
                            )
                        };
                        let pos = npc_spawn_position(r.position, g_idx, spawn_idx, offset_m);
                        npcs.push(NpcSpawnOrder {
                            ball_id: npc_id,
                            type_id,
                            group_id: g.group_id,
                            pos,
                            custom_name: g
                                .npc_ball_names
                                .get(&npc_id)
                                .cloned()
                                .or_else(|| pair.and_then(|(e, _)| e.custom_name.clone())),
                            destructible,
                        });
                    }
                }
            }
        }
        (gates, objs, npcs)
    });
    drop(view);

    let count = gates_to_spawn.len() + objects_to_spawn.len() + npcs_to_spawn.len();
    for (gid, gpos, gate_type_id) in gates_to_spawn {
        spawn_gate_ball(&sys_handle, gid, gpos, gate_type_id).await;
    }
    for (bid, tid, pos, radius, destructible) in objects_to_spawn {
        spawn_object_ball(&sys_handle, &ctx.sde, bid, tid, pos, radius, destructible).await;
    }
    if !npcs_to_spawn.is_empty() {
        actions::spawn_dungeon_npc_orders(
            actions::DungeonRuntimeDeps::from_ctx(ctx),
            sys_id,
            &sys_handle,
            npcs_to_spawn,
        )
        .await;
    }
    count
}

/// Despawns all remaining dungeon objects, gates, and NPCs when a dungeon completes and emits `OnDungeonCompleted`.
pub async fn despawn_completed_dungeon(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    instance_id: i64,
    char_id: i64,
) {
    actions::complete_and_flush_instance(
        actions::DungeonRuntimeDeps::from_ctx(ctx),
        sys_id,
        instance_id,
        char_id,
        true,
    )
    .await;
}

#[allow(clippy::too_many_arguments)]
pub async fn despawn_completed_dungeon_core(
    systems: &std::sync::Arc<crate::system_actor::SystemRegistry>,
    db: &std::sync::Arc<eve_db::WorldRepository>,
    pending_notifications: Option<
        &dyn AnyRwLock<
            HashMap<i64, Vec<crate::router::cache_notifications::PendingMachoNotification>>,
        >,
    >,
    notify_tx: Option<
        &tokio::sync::broadcast::Sender<(
            i64,
            crate::router::cache_notifications::PendingMachoNotification,
        )>,
    >,
    sys_id: SolarSystemId,
    instance_id: i64,
    char_id: i64,
) {
    complete_dungeon_instance_core(
        systems,
        db,
        pending_notifications,
        notify_tx,
        sys_id,
        instance_id,
        char_id,
        true,
    )
    .await;
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn complete_dungeon_instance_core(
    systems: &std::sync::Arc<crate::system_actor::SystemRegistry>,
    db: &std::sync::Arc<eve_db::WorldRepository>,
    pending_notifications: Option<
        &dyn AnyRwLock<
            HashMap<i64, Vec<crate::router::cache_notifications::PendingMachoNotification>>,
        >,
    >,
    notify_tx: Option<
        &tokio::sync::broadcast::Sender<(
            i64,
            crate::router::cache_notifications::PendingMachoNotification,
        )>,
    >,
    sys_id: SolarSystemId,
    instance_id: i64,
    char_id: i64,
    force_full_despawn: bool,
) {
    actions::complete_and_flush_instance(
        actions::DungeonRuntimeDeps::from_parts(
            systems,
            db,
            None,
            pending_notifications,
            notify_tx,
        ),
        sys_id,
        instance_id,
        char_id,
        force_full_despawn,
    )
    .await;
}

/// Finalizes any completed or failed dungeon instances whose `loot_grace_until_unix` has elapsed.
pub async fn expire_dungeon_loot_grace(ctx: &ServerContext, sys_id: SolarSystemId) -> usize {
    let now_unix = ctx.db.clock().now_unix_secs();
    let expired_ids: Vec<i64> = read_dungeon_runtime_store(ctx, |store| {
        store
            .instances
            .values()
            .filter(|inst| {
                inst.solar_system_id == sys_id
                    && (inst.completed || inst.failed)
                    && !inst.despawned
                    && inst
                        .loot_grace_until_unix
                        .or(inst.despawn_after_unix)
                        .is_some_and(|due| now_unix >= due)
            })
            .map(|inst| inst.instance_id)
            .collect()
    });
    for &inst_id in &expired_ids {
        actions::complete_and_flush_instance(
            actions::DungeonRuntimeDeps::from_ctx(ctx),
            sys_id,
            inst_id,
            0,
            true,
        )
        .await;
    }
    expired_ids.len()
}

#[cfg(test)]
pub(crate) async fn apply_dungeon_trigger_actions(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    instance_id: i64,
    room_vec_idx: usize,
    actions: Vec<(u32, DungeonTriggerAction)>,
    char_id: i64,
) -> usize {
    actions::apply_dungeon_trigger_actions_impl(
        actions::DungeonRuntimeDeps::from_ctx(ctx),
        sys_id,
        instance_id,
        room_vec_idx,
        actions,
        char_id,
    )
    .await
}

/// Validates hull group and mass restrictions for a `DungeonRuntimeInstance` acceleration gate.
pub fn check_dungeon_runtime_gate_ship_allowed(sde: &eve_sde::SdeRegistry, dungeon_id: u32, ship_type_id: eve_sde::TypeId, ship_mass: f64) -> Result<(), BridgeRpcError> {
    let st = sde.types.get(&ship_type_id);
    let gid = st.map(|t| t.group_id).unwrap_or(0);
    if matches!(gid, 29 | 30 | 485 | 513 | 547 | 659 | 883 | 902 | 941 | 1538) {
        return Err(BridgeRpcError::user_error("ShipRestrictedByDungeonGate"));
    }
    let rig = st.and_then(|t| t.default_attributes.get(&1547).copied()).unwrap_or(0.0);
    let mass = st.map_or(ship_mass, |t| t.mass.max(ship_mass));
    let diff = sde.dungeons.get(&dungeon_id).and_then(|d| d.difficulty);
    let disallowed = match diff {
        Some(1) => rig > 1.0 || mass > 15_000_000.0 || (gid != 0 && !matches!(gid, 25 | 28 | 237 | 324 | 380 | 420 | 541 | 830 | 831 | 834 | 893 | 1283 | 1305 | 1527)),
        Some(2..=3) => rig > 2.0 || mass > 50_000_000.0 || (gid != 0 && !matches!(gid, 25 | 26 | 28 | 237 | 324 | 358 | 380 | 420 | 541 | 830 | 831 | 832 | 833 | 834 | 893 | 894 | 906 | 963 | 1283 | 1305 | 1527 | 1972)),
        Some(4..=5) => rig > 3.0 || mass > 300_000_000.0,
        _ => rig > 3.0 || mass > 500_000_000.0,
    };
    if disallowed { return Err(BridgeRpcError::user_error("ShipRestrictedByDungeonGate")); }
    Ok(())
}

/// Evaluates `OnKill` triggers and room/dungeon completion when an NPC ball is destroyed in `sys_id`.
pub async fn on_dungeon_npc_killed(ctx: &ServerContext, sys_id: SolarSystemId, npc_ball_id: i64, killer_char_id: i64) {
    actions::on_dungeon_npc_killed_impl(
        actions::DungeonRuntimeDeps::from_ctx(ctx),
        sys_id,
        npc_ball_id,
        killer_char_id,
    )
    .await;
}

#[allow(clippy::too_many_arguments)]
pub async fn on_dungeon_npc_killed_core(
    systems: &std::sync::Arc<crate::system_actor::SystemRegistry>, db: &std::sync::Arc<eve_db::WorldRepository>, sde: &eve_sde::SdeRegistry,
    pending_notifications: Option<&dyn AnyRwLock<HashMap<i64, Vec<crate::router::cache_notifications::PendingMachoNotification>>>>,
    notify_tx: Option<&tokio::sync::broadcast::Sender<(i64, crate::router::cache_notifications::PendingMachoNotification)>>,
    sys_id: SolarSystemId, npc_ball_id: i64, killer_char_id: i64,
) {
    actions::on_dungeon_npc_killed_impl(
        actions::DungeonRuntimeDeps::from_parts(
            systems,
            db,
            Some(sde),
            pending_notifications,
            notify_tx,
        ),
        sys_id,
        npc_ball_id,
        killer_char_id,
    )
    .await;
}

pub use crate::services::pve_dungeon_runtime_rpc::{
    check_dungeon_triggers, get_dungeon_instance_state, on_dungeon_approach,
    spawn_dungeon_instance, spawn_sde_dungeon_runtime_instance,
};
pub(crate) use ops::{
    expire_all_due_dungeon_loot_grace, expire_due_dungeon_loot_grace_sync,
};

/// Sweeps due dungeon loot-grace expiry and armed delayed triggers during cluster ticks.
pub(crate) async fn tick_background_dungeon_runtime(ctx: &ServerContext) {
    let _ = expire_all_due_dungeon_loot_grace(ctx).await;
    let _ = tick_due_delayed_dungeon_triggers(ctx).await;
}

/// Dispatches Homefront Operations acceleration gates first, then falls back to SDE/runtime dungeons.
pub async fn try_activate_dungeon_runtime_gate(
    ctx: &ServerContext,
    session: &mut eve_machonet::SessionState,
    sys_id: SolarSystemId,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    if let [arg0, ..] = args {
        if let Some(gate_id) = arg0.as_i64() {
            if let Some(res) = crate::services::pve_homefront_sites::activate_homefront_gate_by_ball_id(
                ctx, session, sys_id, gate_id,
            )
            .await
            {
                return Some(res);
            }
        }
    }
    crate::services::pve_dungeon_runtime_rpc::try_activate_dungeon_runtime_gate(
        ctx, session, sys_id, args,
    )
    .await
}

/// Forwards space-container loot / Raid delivery events to Homefront Operations and standard dungeon triggers.
pub async fn on_dungeon_item_looted(
    ctx: &ServerContext,
    char_id: i64,
    sys_id_opt: Option<SolarSystemId>,
    container_ball_id: i64,
    item_type_id: eve_sde::TypeId,
) {
    crate::services::pve_homefront_sites::on_homefront_inventory_event(
        ctx,
        char_id,
        sys_id_opt,
        container_ball_id,
        item_type_id,
    )
    .await;
    crate::services::pve_dungeon_runtime_rpc::on_dungeon_item_looted(
        ctx,
        char_id,
        sys_id_opt,
        container_ball_id,
        item_type_id,
    )
    .await;
}

#[cfg(test)]
#[path = "pve_dungeon_runtime_tests.rs"]
mod pve_dungeon_runtime_tests;

