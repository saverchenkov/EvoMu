//! `pve_dungeon_runtime_rpc` — Acceleration Gate Pocket Traversal & Keeper/DungeonInstanceCacheMgr
//! RPC Handlers (`feat:pve.deadspace-pocket-rules`, `feat:pve.dungeon-system`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::notify_emitters_space_dungeons::emit_dungeon_progression_notifications;
use crate::services::pve_dungeon_runtime::{
    apply_dungeon_trigger_actions, complete_dungeon_instance_core, dist3,
    has_unlooted_containers_in_snap, instance_is_finished, materialize_dungeon_room_entities,
    on_dungeon_room_entered, prune_stale_despawned_instances, read_dungeon_runtime_store,
    record_dungeon_actor, room_is_entered, unix_to_filetime_ticks, with_dungeon_runtime_store,
    DungeonGateLink, DungeonInstanceId, DungeonNpcGroup, DungeonObjectiveStatus,
    DungeonRuntimeInstance, DungeonRuntimeObject, DungeonRuntimeRoom, DungeonTemplateId,
    DungeonTrigger, DungeonTriggerAction, DungeonTriggerKind, MaterializedDungeonRoomEntities,
    DEFAULT_CONTAINER_TYPE_ID, GATE_ACTIVATION_RANGE_M,
};
use eve_db::lock_rank::AnyRwLock;
use eve_destiny::MovementMode;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{SolarSystemId, TypeId};
use glam::DVec3;
use std::collections::HashMap;

/// Checks whether `args[0]` targets a `DungeonRuntimeInstance` acceleration gate and executes pocket transition.
pub async fn try_activate_dungeon_runtime_gate(
    ctx: &ServerContext,
    session: &mut SessionState,
    sys_id: SolarSystemId,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let arg_int = args.first().and_then(|a| a.as_i64());
    let arg_str = args.first().and_then(|a| a.as_str());

    let match_info = ctx.db.with_snapshot(|snap| {
        for inst in snap.dungeon_runtime.instances.values() {
            if instance_is_finished(inst) || inst.solar_system_id != sys_id {
                continue;
            }
            if let Some(gate_id) = arg_int {
                if inst.entry_gate_ball_id == Some(gate_id) {
                    return Some((
                        inst.instance_id,
                        0usize,
                        inst.entry_gate_pos,
                        inst.entry_gate_locked,
                        gate_id,
                        None,
                    ));
                }
                for (r_idx, r) in inst.rooms.iter().enumerate() {
                    if !r.gate_links.is_empty() {
                        if let Some(gl) = r.gate_links.iter().find(|gl| gl.gate_ball_id == Some(gate_id)) {
                            if gl.target_room_index < inst.rooms.len() {
                                return Some((
                                    inst.instance_id,
                                    gl.target_room_index,
                                    Some(gl.gate_position),
                                    gl.locked,
                                    gate_id,
                                    Some(gl.destination_pos),
                                ));
                            }
                        }
                    } else if r.gate_ball_id == Some(gate_id) && r_idx + 1 < inst.rooms.len() {
                        let gate_pos = Some([r.position[0] + 5_000.0, r.position[1], r.position[2]]);
                        return Some((
                            inst.instance_id,
                            r_idx + 1,
                            gate_pos,
                            r.gate_locked,
                            gate_id,
                            None,
                        ));
                    }
                }
            }
            if let Some(s) = arg_str {
                if s == format!("DNG-INST-{}", inst.instance_id) {
                    if inst.current_room_index == 0
                        && (inst.entry_gate_pos.is_some() || inst.entry_gate_ball_id.is_some())
                    {
                        return Some((
                            inst.instance_id,
                            0usize,
                            inst.entry_gate_pos,
                            inst.entry_gate_locked,
                            inst.entry_gate_ball_id.unwrap_or(0),
                            None,
                        ));
                    }
                    let cur_room = inst
                        .current_room_index
                        .checked_sub(1)
                        .and_then(|idx| inst.rooms.get(idx));
                    if let Some(gl) = cur_room.and_then(|r| {
                        r.gate_links
                            .iter()
                            .find(|gl| gl.target_room_index < inst.rooms.len())
                    }) {
                        return Some((
                            inst.instance_id,
                            gl.target_room_index,
                            Some(gl.gate_position),
                            gl.locked,
                            gl.gate_ball_id.unwrap_or(0),
                            Some(gl.destination_pos),
                        ));
                    }
                    let next_idx = inst.current_room_index.min(inst.rooms.len().saturating_sub(1));
                    let gate_id = cur_room.and_then(|r| r.gate_ball_id).unwrap_or(0);
                    let gate_pos =
                        cur_room.map(|r| [r.position[0] + 5_000.0, r.position[1], r.position[2]]);
                    let locked = cur_room.is_some_and(|r| r.gate_locked);
                    return Some((inst.instance_id, next_idx, gate_pos, locked, gate_id, None));
                }
            }
        }
        None
    });

    let (instance_id, target_room_idx, gate_pos_opt, gate_locked, gate_id, dest_override) = match_info?;

    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Some(Err(BridgeRpcError::user_error("NotInSpace")));
    }
    let Some(ship_id) = session.ship_id else {
        return Some(Err(BridgeRpcError::user_error("NoActiveShip")));
    };

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let Some((_, ship_ball)) = sys_handle.get_ball_snapshot(ship_id).await.ok().flatten() else {
        return Some(Err(BridgeRpcError::user_error("ShipNotInSpace")));
    };
    if let Err(e) = ctx
        .validate_ship_movement(
            char_id,
            ship_id,
            crate::services::space_nav_movement_guard::MovementIntent::AccelerationGate {
                sys_id,
                kind: crate::services::space_nav_movement_guard::AccelerationGateKind::Dungeon,
            },
        )
        .await
    {
        return Some(Err(e));
    }

    // Verify proximity <= 2,500m to the acceleration gate
    let gate_pos = if gate_id > 0 {
        sys_handle
            .get_ball_snapshot(gate_id)
            .await
            .ok()
            .flatten()
            .map(|(_, b)| b.position)
            .or_else(|| gate_pos_opt.map(DVec3::from_array))
    } else {
        gate_pos_opt.map(DVec3::from_array)
    };
    if let Some(gp) = gate_pos {
        let dist = ship_ball.position.distance(gp);
        if dist > GATE_ACTIVATION_RANGE_M {
            return Some(Err(BridgeRpcError {
                code: "OutsideGateActivationRange".into(),
                message: format!("Ship is {dist:.1} m from gate (must be <= 2500 m)"),
                client_error: None,
            }));
        }
    }

    if gate_locked {
        return Some(Err(BridgeRpcError {
            code: "GateLocked".into(),
            message: "Acceleration gate is locked until room objectives are completed".into(),
            client_error: None,
        }));
    }

    let (dungeon_id, item_mass) = ctx.db.with_snapshot(|snap| {
        let did = snap
            .dungeon_runtime
            .instances
            .get(&instance_id)
            .map(|i| i.dungeon_id)
            .unwrap_or(0);
        let mass = snap
            .items
            .get(&ship_id)
            .and_then(|it| ctx.sde.types.get(&it.type_id))
            .map(|t| t.mass)
            .unwrap_or(ship_ball.mass);
        (did, mass)
    });
    let effective_mass = item_mass.max(ship_ball.mass);
    if let Err(e) = crate::services::pve_dungeon_runtime::check_dungeon_runtime_gate_ship_allowed(
        &ctx.sde,
        dungeon_id,
        ship_ball.type_id,
        effective_mass,
    ) {
        return Some(Err(e));
    }

    // Materialize target room entities
    let MaterializedDungeonRoomEntities {
        object_ids: obj_ids,
        npc_ids,
        next_gate_id,
    } = match materialize_dungeon_room_entities(ctx, sys_id, instance_id, target_room_idx).await {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };

    // Update current_room_index and get landing destination
    let room_info = with_dungeon_runtime_store(ctx, |store| {
        let inst = store.instances.get_mut(&instance_id)?;
        if instance_is_finished(inst) {
            return None;
        }
        record_dungeon_actor(inst, char_id);
        let room = inst.rooms.get(target_room_idx)?.clone();
        inst.current_room_index = if room.room_index > 0 {
            room.room_index
        } else {
            target_room_idx + 1
        };
        let room_obj_id = room.room_id * 10 + 1;
        inst.active_objective_id = room_obj_id;
        Some((
            inst.dungeon_id,
            inst.category.clone(),
            inst.dungeon_name.clone(),
            room.room_id,
            room.room_index,
            dest_override.unwrap_or(room.gate_destination_pos),
            inst.rooms.len(),
            inst.created_unix,
            room.disallow_mwd.unwrap_or(inst.disallow_mwd),
            room_obj_id,
            inst.last_completed_task_id,
        ))
    });
    let Some((
        dungeon_id,
        category,
        dungeon_name,
        room_id,
        room_index,
        dest_pos,
        total_rooms,
        created_unix,
        room_disallow_mwd,
        room_obj_id,
        last_completed_task_id,
    )) = room_info
    else {
        return Some(Err(BridgeRpcError::user_error("DungeonRoomNotFound")));
    };

    // Move ship ball to the acceleration gate's landing destination in the new pocket
    let dest_vec = DVec3::from_array(dest_pos);
    let _ = sys_handle.set_ball_position(ship_id, dest_vec).await;
    let _ = sys_handle.set_movement_mode(ship_id, MovementMode::Stop).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_positions.insert(ship_id, dest_pos);
    });

    let expiry_ft = unix_to_filetime_ticks(created_unix.saturating_add(86_400));
    emit_dungeon_progression_notifications(
        ctx,
        char_id,
        sys_id,
        &category,
        &dungeon_name,
        i64::from(dungeon_id),
        instance_id,
        room_id,
        dest_pos,
        room_index as u8,
        total_rooms.max(1) as u8,
        false,
        expiry_ft,
    );
    if target_room_idx > 0 {
        if let Some(last_tid) = last_completed_task_id {
            crate::services::notify_emitters_ess_bounties::emit_progression_advanced(
                ctx,
                char_id,
                room_obj_id,
                last_tid,
            );
        }
    }
    on_dungeon_room_entered(ctx, sys_id, instance_id, target_room_idx, char_id).await;

    Some(Ok(PyRep::keyval([
        ("success", PyRep::Bool(true)),
        ("instanceID", PyRep::Integer(instance_id)),
        ("gateID", PyRep::Integer(gate_id)),
        ("dungeonID", PyRep::Integer(i64::from(dungeon_id))),
        ("roomID", PyRep::Integer(room_id)),
        ("roomIndex", PyRep::Integer(room_index as i64)),
        ("totalRooms", PyRep::Integer(total_rooms as i64)),
        ("disallowMWD", PyRep::Bool(room_disallow_mwd)),
        ("destinationX", PyRep::Real(dest_pos[0])),
        ("destinationY", PyRep::Real(dest_pos[1])),
        ("destinationZ", PyRep::Real(dest_pos[2])),
        (
            "nextGateBallID",
            next_gate_id.map(PyRep::Integer).unwrap_or(PyRep::None),
        ),
        (
            "objectBallIDs",
            PyRep::List(obj_ids.into_iter().map(PyRep::Integer).collect()),
        ),
        (
            "npcBallIDs",
            PyRep::List(npc_ids.into_iter().map(PyRep::Integer).collect()),
        ),
    ])))
}

const MAX_DUNGEON_COORD_M: f64 = 1.0e15;

fn finite_f64(rep: &PyRep) -> Option<f64> {
    rep.as_f64().filter(|v| v.is_finite())
}

fn parse_finite_vec3(rep: Option<&PyRep>, default: [f64; 3]) -> [f64; 3] {
    match rep {
        Some(PyRep::List(v) | PyRep::Tuple(v)) if v.len() >= 3 => [
            finite_f64(&v[0])
                .unwrap_or(default[0])
                .clamp(-MAX_DUNGEON_COORD_M, MAX_DUNGEON_COORD_M),
            finite_f64(&v[1])
                .unwrap_or(default[1])
                .clamp(-MAX_DUNGEON_COORD_M, MAX_DUNGEON_COORD_M),
            finite_f64(&v[2])
                .unwrap_or(default[2])
                .clamp(-MAX_DUNGEON_COORD_M, MAX_DUNGEON_COORD_M),
        ],
        _ => default,
    }
}

fn parse_rooms_from_pyrep(
    ctx: &ServerContext,
    sys_id: u32,
    dungeon_id: u32,
    dungeon_name: &str,
    base_pos: [f64; 3],
    has_entry_gate: bool,
    rooms_arg: Option<&PyRep>,
) -> Vec<DungeonRuntimeRoom> {
    if let Some(PyRep::List(room_list) | PyRep::Tuple(room_list)) = rooms_arg {
        let mut parsed = Vec::new();
        for (idx, r_rep) in room_list.iter().take(16).enumerate() {
            let r_idx = idx + 1;
            let room_pos = [
                base_pos[0] + (idx as f64) * 500_000.0,
                base_pos[1],
                base_pos[2],
            ];
            let gate_dest = [room_pos[0] - 10_000.0, room_pos[1], room_pos[2]];
            let r_name = r_rep
                .dict_get("roomName")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("{dungeon_name} - Room {r_idx}"));
            let r_disallow_mwd = r_rep.dict_get("disallowMWD").and_then(|v| v.as_bool());
            let gate_locked = r_rep
                .dict_get("gateLocked")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let mut objects = Vec::new();
            if let Some(PyRep::List(objs) | PyRep::Tuple(objs)) = r_rep.dict_get("objects") {
                for (o_i, o_rep) in objs.iter().take(64).enumerate() {
                    let tid = o_rep
                        .dict_get("typeID")
                        .and_then(|v| v.as_i64())
                        .and_then(|v| u32::try_from(v).ok())
                        .unwrap_or(DEFAULT_CONTAINER_TYPE_ID);
                    let oname = o_rep
                        .dict_get("name")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .or_else(|| ctx.sde.types.get(&tid).map(|t| t.name.clone()))
                        .unwrap_or_else(|| format!("Dungeon Object {}", o_i + 1));
                    let offset = parse_finite_vec3(
                        o_rep.dict_get("offset"),
                        [(o_i as f64 + 1.0) * 3_500.0, 0.0, 2_000.0],
                    );
                    let radius = o_rep
                        .dict_get("radius")
                        .and_then(finite_f64)
                        .map(|v| v.clamp(0.0, 1_000_000.0))
                        .unwrap_or(300.0);
                    let is_container = o_rep
                        .dict_get("isContainer")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let mut loot_items = Vec::new();
                    if let Some(PyRep::List(loots) | PyRep::Tuple(loots)) = o_rep.dict_get("lootItems") {
                        for l_rep in loots.iter().take(64) {
                            if let PyRep::List(pair) | PyRep::Tuple(pair) = l_rep {
                                if pair.len() >= 2 {
                                    if let (Some(ltid), Some(lqty)) = (
                                        pair[0].as_i64().and_then(|v| u32::try_from(v).ok()),
                                        pair[1]
                                            .as_i64()
                                            .and_then(|v| i32::try_from(v).ok())
                                            .filter(|&q| q > 0),
                                    ) {
                                        loot_items.push((ltid, lqty));
                                    }
                                }
                            }
                        }
                    }
                    objects.push(DungeonRuntimeObject {
                        ball_id: 0,
                        type_id: tid,
                        name: oname,
                        position: [
                            room_pos[0] + offset[0],
                            room_pos[1] + offset[1],
                            room_pos[2] + offset[2],
                        ],
                        radius,
                        is_container,
                        loot_items,
                        spawned: false,
                        destructible: o_rep
                            .dict_get("destructible")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false),
                        ..Default::default()
                    });
                }
            }

            let mut npc_groups = Vec::new();
            if let Some(PyRep::List(grps) | PyRep::Tuple(grps)) = r_rep.dict_get("npcGroups") {
                for (g_i, g_rep) in grps.iter().take(32).enumerate() {
                    let group_id = g_rep
                        .dict_get("groupID")
                        .and_then(|v| v.as_i64())
                        .and_then(|v| u8::try_from(v).ok())
                        .unwrap_or_else(|| u8::try_from(g_i + 1).unwrap_or(u8::MAX));
                    let wave = g_rep
                        .dict_get("wave")
                        .and_then(|v| v.as_i64())
                        .and_then(|v| u8::try_from(v).ok())
                        .unwrap_or(1);
                    let spawn_immediately = g_rep
                        .dict_get("spawnImmediately")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(wave == 1);
                    let mut npc_specs = Vec::new();
                    if let Some(PyRep::List(npcs) | PyRep::Tuple(npcs)) = g_rep.dict_get("npcs") {
                        for n_rep in npcs.iter().take(32) {
                            if let PyRep::List(pair) | PyRep::Tuple(pair) = n_rep {
                                if pair.len() >= 2 {
                                    if let (Some(ntid), Some(ncnt)) = (
                                        pair[0].as_i64().and_then(|v| u32::try_from(v).ok()),
                                        pair[1]
                                            .as_i64()
                                            .and_then(|v| usize::try_from(v).ok())
                                            .map(|c| c.clamp(1, 500)),
                                    ) {
                                        npc_specs.push((ntid, ncnt));
                                    }
                                }
                            }
                        }
                    }
                    if npc_specs.is_empty() {
                        npc_specs.push((10_043, 1));
                    }
                    npc_groups.push(DungeonNpcGroup {
                        group_id,
                        wave,
                        npc_specs,
                        npc_ball_ids: Vec::new(),
                        spawn_immediately,
                        spawned: false,
                        cleared: false,
                        ..Default::default()
                    });
                }
            }

            let mut triggers = Vec::new();
            if let Some(PyRep::List(trigs) | PyRep::Tuple(trigs)) = r_rep.dict_get("triggers") {
                for (t_i, t_rep) in trigs.iter().take(64).enumerate() {
                    let kind_str = t_rep
                        .dict_get("kind")
                        .and_then(|v| v.as_str())
                        .unwrap_or("kill")
                        .to_ascii_lowercase();
                    let action_str = t_rep
                        .dict_get("action")
                        .and_then(|v| v.as_str())
                        .unwrap_or("UnlockGate");
                    let action_grp = t_rep
                        .dict_get("actionGroup")
                        .and_then(|v| v.as_i64())
                        .and_then(|v| u8::try_from(v).ok())
                        .unwrap_or(2);
                    let action = match action_str {
                        "SpawnNpcGroup" => DungeonTriggerAction::SpawnNpcGroup(action_grp),
                        "CompleteDungeon" => DungeonTriggerAction::CompleteDungeon,
                        _ => DungeonTriggerAction::UnlockGate,
                    };
                    let kind = match kind_str.as_str() {
                        "approach" => {
                            let target_obj_idx = t_rep
                                .dict_get("targetObjectIndex")
                                .and_then(|v| v.as_i64())
                                .and_then(|v| usize::try_from(v).ok());
                            let radius_m = t_rep
                                .dict_get("radius")
                                .and_then(finite_f64)
                                .filter(|&v| v >= 0.0)
                                .map(|v| v.clamp(0.0, 1_000_000.0))
                                .unwrap_or(5_000.0);
                            let anchor = target_obj_idx
                                .and_then(|oi| objects.get(oi))
                                .map(|o| o.position)
                                .unwrap_or(room_pos);
                            DungeonTriggerKind::OnApproach {
                                target_ball_id: None,
                                target_object_index: target_obj_idx,
                                anchor_pos: anchor,
                                radius_m,
                            }
                        }
                        "loot" => {
                            let target_obj_idx = t_rep
                                .dict_get("targetObjectIndex")
                                .and_then(|v| v.as_i64())
                                .and_then(|v| usize::try_from(v).ok());
                            let item_tid = t_rep
                                .dict_get("itemTypeID")
                                .and_then(|v| v.as_i64())
                                .and_then(|v| u32::try_from(v).ok());
                            DungeonTriggerKind::OnLoot {
                                container_ball_id: None,
                                target_object_index: target_obj_idx,
                                item_type_id: item_tid,
                            }
                        }
                        _ => {
                            let target_grp = t_rep
                                .dict_get("targetGroup")
                                .and_then(|v| v.as_i64())
                                .and_then(|v| u8::try_from(v).ok());
                            DungeonTriggerKind::OnKill {
                                target_npc_group: target_grp,
                                target_ball_id: None,
                            }
                        }
                    };
                    triggers.push(DungeonTrigger {
                        trigger_id: u32::try_from(t_i + 1).unwrap_or(u32::MAX),
                        kind,
                        action,
                        fired: false,
                    });
                }
            }

            let mut gate_links = Vec::new();
            if let Some(PyRep::List(links) | PyRep::Tuple(links)) = r_rep.dict_get("gateLinks") {
                for l_rep in links.iter().take(16) {
                    if let Some(target_room_index) = l_rep
                        .dict_get("targetRoomIndex")
                        .and_then(|v| v.as_i64())
                        .and_then(|v| usize::try_from(v).ok())
                    {
                        let gpos = parse_finite_vec3(
                            l_rep.dict_get("gatePosition"),
                            [room_pos[0] + 5_000.0, room_pos[1], room_pos[2]],
                        );
                        let dpos = parse_finite_vec3(
                            l_rep.dict_get("destinationPos"),
                            [
                                base_pos[0] + (target_room_index as f64) * 500_000.0 - 10_000.0,
                                base_pos[1],
                                base_pos[2],
                            ],
                        );
                        gate_links.push(DungeonGateLink {
                            target_room_index,
                            gate_position: gpos,
                            destination_pos: dpos,
                            locked: l_rep
                                .dict_get("locked")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(gate_locked),
                            label: l_rep
                                .dict_get("label")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            ..Default::default()
                        });
                    }
                }
            }

            parsed.push(DungeonRuntimeRoom {
                room_index: r_idx,
                room_id: i64::from(dungeon_id) * 100 + (r_idx as i64),
                room_name: r_name,
                position: room_pos,
                gate_destination_pos: gate_dest,
                gated: r_idx > 1 || has_entry_gate,
                disallow_mwd: r_disallow_mwd,
                gate_ball_id: None,
                gate_locked,
                objects,
                npc_groups,
                triggers,
                spawned: false,
                cleared: false,
                gate_links,
                ..Default::default()
            });
        }
        if !parsed.is_empty() {
            let room_count = parsed.len();
            for room in &mut parsed {
                room.gate_links.retain(|gl| gl.target_room_index < room_count);
            }
            return parsed;
        }
    }

    resolve_rooms_with_content_fallback(ctx, sys_id, dungeon_id, dungeon_name, base_pos, has_entry_gate)
}

/// Resolves dungeon rooms from loaded wiki PvE runtime content first (`pve_dungeon_content`),
/// falling back cleanly to `build_sde_deadspace_rooms` when no authored wiki entry exists.
pub(crate) fn resolve_rooms_with_content_fallback(
    ctx: &ServerContext,
    sys_id: u32,
    dungeon_id: u32,
    dungeon_name: &str,
    base_pos: [f64; 3],
    has_entry_gate: bool,
) -> Vec<DungeonRuntimeRoom> {
    if let Some(built) = crate::services::pve_dungeon_content::build_wiki_deadspace_rooms(
        ctx,
        sys_id,
        DungeonTemplateId::new(dungeon_id),
        dungeon_name,
        base_pos,
        has_entry_gate,
    ) {
        return built.rooms;
    }
    tracing::debug!(
        source = "SDE synthetic fallback (no authored wiki entry matched)",
        dungeon_id,
        dungeon_name = %dungeon_name,
        "Using synthetic SDE 2-room deadspace dungeon template"
    );
    build_sde_deadspace_rooms(ctx, sys_id, dungeon_id, dungeon_name, base_pos, has_entry_gate)
}

/// Resolves faction Deadspace NPC ship `TypeId`s (`(frigate_tid, cruiser_tid, commander_tid)`) from
/// `sde.npc_spawn_groups` (`62..=86`), matching `sde.dungeons[dungeon_id].faction_id` or regional pirate faction.
fn resolve_dungeon_faction_npc_types(
    sde: &eve_sde::SdeRegistry,
    sys_id: u32,
    dungeon_id: u32,
) -> (u32, u32, u32) {
    let faction_id = sde
        .dungeons
        .get(&dungeon_id)
        .and_then(|d| d.faction_id)
        .unwrap_or_else(|| {
            match crate::services::pve_exploration_seeding::pirate_faction_for_system(sde, sys_id) {
                crate::services::pve_exploration_seeding::PirateFaction::AngelCartel => 500_011,
                crate::services::pve_exploration_seeding::PirateFaction::BloodRaiders => 500_012,
                crate::services::pve_exploration_seeding::PirateFaction::Sansha => 500_019,
                crate::services::pve_exploration_seeding::PirateFaction::Serpentis => 500_020,
                crate::services::pve_exploration_seeding::PirateFaction::Guristas => 500_010,
            }
        });
    let (frig_sg, cruiser_sg, bs_sg) = match faction_id {
        500_011 => (66u32, 64u32, 63u32), // Angel Cartel
        500_012 => (71, 69, 68),          // Blood Raiders
        500_019 => (81, 79, 78),          // Sansha's Nation
        500_020 => (86, 84, 83),          // Serpentis
        _ => (76, 74, 73),                // Guristas
    };
    let pick = |sg_id: u32, fallback: u32| -> u32 {
        sde.npc_spawn_groups
            .get(&sg_id)
            .and_then(|g| g.npc_type_ids.first().copied())
            .unwrap_or(fallback)
    };
    let difficulty = sde
        .dungeons
        .get(&dungeon_id)
        .and_then(|d| d.difficulty)
        .unwrap_or(3);
    let frig_tid = pick(frig_sg, 16_981);
    let cruiser_tid = pick(cruiser_sg, 16_980);
    let boss_tid = if difficulty >= 4 {
        pick(bs_sg, cruiser_tid)
    } else {
        cruiser_tid
    };
    (frig_tid, cruiser_tid, boss_tid)
}

/// Appends SDE-authored asteroid rocks (`sde.dungeon_asteroids`) and gas clouds (`sde.dungeon_gas_types`)
/// for `dungeon_id` when present in `evomu_sde.sqlite`, preserving index `0` beacon/container objects.
// DERIVED: Per-object spatial ring offsets around room anchor positions and the per-entry ball cap (<= 4)
// are derived placement rules; `type_id`s, `room_id` split, and `asteroid_count > 0` bounds come from SDE.
fn append_sde_dungeon_environmental_objects(
    sde: &eve_sde::SdeRegistry,
    dungeon_id: u32,
    r1_pos: [f64; 3],
    r2_pos: [f64; 3],
    r1_objects: &mut Vec<DungeonRuntimeObject>,
    r2_objects: &mut Vec<DungeonRuntimeObject>,
) {
    if let Some(entries) = sde.dungeon_asteroids.get(&dungeon_id).filter(|v| !v.is_empty()) {
        let mut sde_rooms: Vec<u32> = entries.iter().map(|e| e.room_id).collect();
        sde_rooms.sort_unstable();
        sde_rooms.dedup();
        let first_sde_room = sde_rooms[0];
        let mut r1_idx = 0usize;
        let mut r2_idx = 0usize;
        for entry in entries {
            let in_r1 = sde_rooms.len() == 1 || entry.room_id == first_sde_room;
            let (target, anchor, idx_ref) = if in_r1 {
                (&mut *r1_objects, r1_pos, &mut r1_idx)
            } else {
                (&mut *r2_objects, r2_pos, &mut r2_idx)
            };
            let base_name = sde
                .types
                .get(&entry.type_id)
                .map(|t| t.name.clone())
                .unwrap_or_else(|| format!("Asteroid {}", entry.type_id));
            let count = entry.asteroid_count.clamp(1, 4);
            for _ in 0..count {
                let angle = (*idx_ref as f64) * 0.7;
                let dist = 3_200.0 + ((*idx_ref % 4) as f64) * 600.0;
                *idx_ref += 1;
                target.push(DungeonRuntimeObject {
                    ball_id: 0,
                    type_id: entry.type_id,
                    name: base_name.clone(),
                    position: [
                        anchor[0] + angle.cos() * dist,
                        anchor[1],
                        anchor[2] + angle.sin() * dist,
                    ],
                    radius: 350.0,
                    is_container: false,
                    loot_items: Vec::new(),
                    spawned: false,
                    ..Default::default()
                });
            }
        }
    }

    if let Some(gas_types) = sde.dungeon_gas_types.get(&dungeon_id).filter(|v| !v.is_empty()) {
        for (idx, &gas_tid) in gas_types.iter().enumerate() {
            let angle = (idx as f64) * 1.3;
            let dist = 4_500.0 + (idx as f64) * 900.0;
            let name = sde
                .types
                .get(&gas_tid)
                .map(|t| t.name.clone())
                .unwrap_or_else(|| format!("Gas Cloud {gas_tid}"));
            r1_objects.push(DungeonRuntimeObject {
                ball_id: 0,
                type_id: gas_tid,
                name,
                position: [
                    r1_pos[0] + angle.cos() * dist,
                    r1_pos[1],
                    r1_pos[2] + angle.sin() * dist,
                ],
                radius: 2_500.0,
                is_container: false,
                loot_items: Vec::new(),
                spawned: false,
                ..Default::default()
            });
        }
    }
}

// DERIVED: Server-side 2-room Deadspace pocket & trigger topology synthesized from `sde.dungeons`
// (`name`, `archetype_id`, `faction_id`, `difficulty`), `sde_dungeon_asteroid_counts`,
// `sde_dungeon_gas_types`, and `sde.npc_spawn_groups` (`62..=86`), since CCP Tranquility FSD
// omits server-authored room trigger graphs and wave scripts.
pub(crate) fn build_sde_deadspace_rooms(
    ctx: &ServerContext,
    sys_id: u32,
    dungeon_id: u32,
    dungeon_name: &str,
    base_pos: [f64; 3],
    has_entry_gate: bool,
) -> Vec<DungeonRuntimeRoom> {
    let (frig_tid, cruiser_tid, boss_tid) =
        resolve_dungeon_faction_npc_types(&ctx.sde, sys_id, dungeon_id);
    let (loot_tid, _) = if sys_id > 0 {
        crate::services::pve_exploration_seeding::pirate_commander_loot_for_system(&ctx.sde, sys_id)
    } else {
        (19_419, "Overseer's Personal Effects")
    };
    let r1_mwd_override = crate::services::pve_dungeon_editor::get_dungeon_or_room_mwd_override(
        ctx,
        i64::from(dungeon_id),
        Some(i64::from(dungeon_id) * 100 + 1),
    );
    let r2_mwd_override = crate::services::pve_dungeon_editor::get_dungeon_or_room_mwd_override(
        ctx,
        i64::from(dungeon_id),
        Some(i64::from(dungeon_id) * 100 + 2),
    );
    let r1_pos = if has_entry_gate {
        [base_pos[0] + 600_000.0, base_pos[1], base_pos[2]]
    } else {
        base_pos
    };
    let r2_pos = [r1_pos[0] + 600_000.0, r1_pos[1], r1_pos[2]];
    let relay_pos = [r1_pos[0] + 2_200.0, r1_pos[1], r1_pos[2]];
    let strongbox_pos = [r2_pos[0] + 1_500.0, r2_pos[1], r2_pos[2]];
    let mut r1_objects = vec![DungeonRuntimeObject {
        ball_id: 0,
        type_id: 23,
        name: "Deadspace Relay Beacon".into(),
        position: relay_pos,
        radius: 250.0,
        is_container: false,
        loot_items: Vec::new(),
        spawned: false,
        ..Default::default()
    }];
    let mut r2_objects = vec![DungeonRuntimeObject {
        ball_id: 0,
        type_id: 23,
        name: "Overseer Strongbox".into(),
        position: strongbox_pos,
        radius: 400.0,
        is_container: true,
        loot_items: vec![(loot_tid, 1)],
        spawned: false,
        ..Default::default()
    }];
    append_sde_dungeon_environmental_objects(
        &ctx.sde,
        dungeon_id,
        r1_pos,
        r2_pos,
        &mut r1_objects,
        &mut r2_objects,
    );
    vec![
        DungeonRuntimeRoom {
            room_index: 1,
            room_id: i64::from(dungeon_id) * 100 + 1,
            room_name: format!("{dungeon_name} - Room 1"),
            position: r1_pos,
            gate_destination_pos: [r1_pos[0] - 10_000.0, r1_pos[1], r1_pos[2]],
            gated: has_entry_gate,
            disallow_mwd: r1_mwd_override,
            gate_ball_id: None,
            gate_locked: true,
            objects: r1_objects,
            npc_groups: vec![
                DungeonNpcGroup {
                    group_id: 1,
                    wave: 1,
                    npc_specs: vec![(frig_tid, 2)],
                    npc_ball_ids: Vec::new(),
                    spawn_immediately: true,
                    spawned: false,
                    cleared: false,
                    ..Default::default()
                },
                DungeonNpcGroup {
                    group_id: 2,
                    wave: 2,
                    npc_specs: vec![(cruiser_tid, 1)],
                    npc_ball_ids: Vec::new(),
                    spawn_immediately: false,
                    spawned: false,
                    cleared: false,
                    ..Default::default()
                },
            ],
            triggers: vec![
                DungeonTrigger {
                    trigger_id: 1,
                    kind: DungeonTriggerKind::OnApproach {
                        target_ball_id: None,
                        target_object_index: Some(0),
                        anchor_pos: relay_pos,
                        radius_m: 3_500.0,
                    },
                    action: DungeonTriggerAction::SpawnNpcGroup(2),
                    fired: false,
                },
                DungeonTrigger {
                    trigger_id: 2,
                    kind: DungeonTriggerKind::OnKill {
                        target_npc_group: Some(1),
                        target_ball_id: None,
                    },
                    action: DungeonTriggerAction::UnlockGate,
                    fired: false,
                },
            ],
            spawned: false,
            cleared: false,
            ..Default::default()
        },
        DungeonRuntimeRoom {
            room_index: 2,
            room_id: i64::from(dungeon_id) * 100 + 2,
            room_name: format!("{dungeon_name} - Room 2"),
            position: r2_pos,
            gate_destination_pos: [r2_pos[0] - 10_000.0, r2_pos[1], r2_pos[2]],
            gated: true,
            disallow_mwd: r2_mwd_override,
            gate_ball_id: None,
            gate_locked: false,
            objects: r2_objects,
            npc_groups: vec![
                DungeonNpcGroup {
                    group_id: 1,
                    wave: 1,
                    npc_specs: vec![(boss_tid, 1)],
                    npc_ball_ids: Vec::new(),
                    spawn_immediately: true,
                    spawned: false,
                    cleared: false,
                    ..Default::default()
                },
                DungeonNpcGroup {
                    group_id: 2,
                    wave: 2,
                    npc_specs: vec![(frig_tid, 1)],
                    npc_ball_ids: Vec::new(),
                    spawn_immediately: false,
                    spawned: false,
                    cleared: false,
                    ..Default::default()
                },
            ],
            triggers: vec![
                DungeonTrigger {
                    trigger_id: 1,
                    kind: DungeonTriggerKind::OnLoot {
                        container_ball_id: None,
                        target_object_index: Some(0),
                        item_type_id: None,
                    },
                    action: DungeonTriggerAction::SpawnNpcGroup(2),
                    fired: false,
                },
                DungeonTrigger {
                    trigger_id: 2,
                    kind: DungeonTriggerKind::OnKill {
                        target_npc_group: Some(1),
                        target_ball_id: None,
                    },
                    action: DungeonTriggerAction::CompleteDungeon,
                    fired: false,
                },
            ],
            spawned: false,
            cleared: false,
            ..Default::default()
        },
    ]
}

fn sde_dungeon_archetype_label(sde_dungeon: Option<&eve_sde::DungeonTemplate>) -> String {
    sde_dungeon
        .and_then(|d| d.archetype_id)
        .map(|id| format!("archetype:{id}"))
        .unwrap_or_else(|| "Deadspace".to_string())
}

fn instance_to_pyrep(
    inst: &DungeonRuntimeInstance,
    sde: Option<&eve_sde::SdeRegistry>,
) -> PyRep {
    let sde_dungeon = sde.and_then(|s| s.dungeons.get(&inst.dungeon_id));
    let rooms_py: Vec<PyRep> = inst
        .rooms
        .iter()
        .map(|r| {
            PyRep::keyval([
                ("roomID", PyRep::Integer(r.room_id)),
                ("roomIndex", PyRep::Integer(r.room_index as i64)),
                ("roomName", PyRep::String(r.room_name.clone())),
                ("gated", PyRep::Bool(r.gated)),
                ("gateLocked", PyRep::Bool(r.gate_locked)),
                ("spawned", PyRep::Bool(r.spawned)),
                ("entered", PyRep::Bool(r.entered)),
                ("cleared", PyRep::Bool(r.cleared)),
                (
                    "disallowMWD",
                    PyRep::Bool(r.disallow_mwd.unwrap_or(inst.disallow_mwd)),
                ),
                (
                    "gateBallID",
                    r.gate_ball_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                ),
                (
                    "position",
                    PyRep::Tuple(vec![
                        PyRep::Real(r.position[0]),
                        PyRep::Real(r.position[1]),
                        PyRep::Real(r.position[2]),
                    ]),
                ),
                (
                    "gateDestinationPos",
                    PyRep::Tuple(vec![
                        PyRep::Real(r.gate_destination_pos[0]),
                        PyRep::Real(r.gate_destination_pos[1]),
                        PyRep::Real(r.gate_destination_pos[2]),
                    ]),
                ),
                (
                    "objectBallIDs",
                    PyRep::List(
                        r.objects
                            .iter()
                            .filter(|o| o.ball_id > 0)
                            .map(|o| PyRep::Integer(o.ball_id))
                            .collect(),
                    ),
                ),
                (
                    "objectTypeIDs",
                    PyRep::List(
                        r.objects
                            .iter()
                            .filter(|o| o.ball_id > 0)
                            .map(|o| PyRep::Integer(i64::from(o.type_id)))
                            .collect(),
                    ),
                ),
                (
                    "npcBallIDs",
                    PyRep::List(
                        r.npc_groups
                            .iter()
                            .flat_map(|g| g.npc_ball_ids.iter().copied())
                            .map(PyRep::Integer)
                            .collect(),
                    ),
                ),
                (
                    "triggersFired",
                    PyRep::Integer(r.triggers.iter().filter(|t| t.fired).count() as i64),
                ),
                (
                    "gateLinks",
                    PyRep::List(
                        r.gate_links
                            .iter()
                            .map(|gl| {
                                PyRep::keyval([
                                    (
                                        "gateBallID",
                                        gl.gate_ball_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                                    ),
                                    (
                                        "targetRoomIndex",
                                        PyRep::Integer(gl.target_room_index as i64),
                                    ),
                                    ("locked", PyRep::Bool(gl.locked)),
                                    ("label", PyRep::String(gl.label.clone())),
                                ])
                            })
                            .collect(),
                    ),
                ),
            ])
        })
        .collect();

    let objectives_py: Vec<PyRep> = inst
        .objectives
        .iter()
        .map(|o| {
            let status_str = match o.status {
                DungeonObjectiveStatus::Active => "active",
                DungeonObjectiveStatus::Completed => "completed",
                DungeonObjectiveStatus::Failed => "failed",
            };
            PyRep::keyval([
                ("objectiveID", PyRep::Integer(o.objective_id)),
                ("key", PyRep::String(o.key.clone())),
                ("title", PyRep::String(o.title.clone())),
                ("kind", PyRep::String(o.kind.clone())),
                ("status", PyRep::String(status_str.to_string())),
            ])
        })
        .collect();

    PyRep::keyval([
        ("instanceID", PyRep::Integer(inst.instance_id)),
        ("dungeonID", PyRep::Integer(i64::from(inst.dungeon_id))),
        ("dungeonName", PyRep::String(inst.dungeon_name.clone())),
        ("category", PyRep::String(inst.category.clone())),
        ("archetype", PyRep::String(inst.archetype.clone())),
        (
            "archetypeID",
            sde_dungeon
                .and_then(|d| d.archetype_id)
                .map(|v| PyRep::Integer(i64::from(v)))
                .unwrap_or(PyRep::None),
        ),
        (
            "factionID",
            sde_dungeon
                .and_then(|d| d.faction_id)
                .map(|v| PyRep::Integer(i64::from(v)))
                .unwrap_or(PyRep::None),
        ),
        (
            "difficulty",
            sde_dungeon
                .and_then(|d| d.difficulty)
                .map(|v| PyRep::Integer(i64::from(v)))
                .unwrap_or(PyRep::None),
        ),
        ("solarSystemID", PyRep::Integer(i64::from(inst.solar_system_id))),
        ("disallowMWD", PyRep::Bool(inst.disallow_mwd)),
        (
            "entryGateBallID",
            inst.entry_gate_ball_id
                .map(PyRep::Integer)
                .unwrap_or(PyRep::None),
        ),
        ("currentRoomIndex", PyRep::Integer(inst.current_room_index as i64)),
        ("activeObjectiveID", PyRep::Integer(inst.active_objective_id)),
        (
            "lastCompletedTaskID",
            inst.last_completed_task_id
                .map(PyRep::Integer)
                .unwrap_or(PyRep::None),
        ),
        (
            "completedTaskIDs",
            PyRep::List(inst.completed_task_ids.iter().copied().map(PyRep::Integer).collect()),
        ),
        ("completed", PyRep::Bool(inst.completed)),
        ("failed", PyRep::Bool(inst.failed)),
        ("despawned", PyRep::Bool(inst.despawned)),
        (
            "lootGraceUntilUnix",
            inst.loot_grace_until_unix
                .map(|v| PyRep::Integer(v as i64))
                .unwrap_or(PyRep::None),
        ),
        (
            "despawnAfterUnix",
            inst.despawn_after_unix
                .map(|v| PyRep::Integer(v as i64))
                .unwrap_or(PyRep::None),
        ),
        ("objectives", PyRep::List(objectives_py)),
        ("rooms", PyRep::List(rooms_py)),
    ])
}

pub async fn spawn_sde_dungeon_runtime_instance(
    ctx: &ServerContext,
    owner_char_id: i64,
    sys_id: u32,
    dungeon_id: u32,
    category: &str,
    base_pos: [f64; 3],
    has_entry_gate: bool,
) -> Result<i64, BridgeRpcError> {
    let existing = read_dungeon_runtime_store(ctx, |store| {
        store.instances.values().find_map(|inst| {
            (!instance_is_finished(inst)
                && inst.solar_system_id == sys_id
                && inst.dungeon_id == dungeon_id
                && inst.base_position == base_pos)
                .then_some(inst.instance_id)
        })
    });
    if let Some(inst_id) = existing {
        return Ok(inst_id);
    }

    let sde_dungeon = ctx.sde.dungeons.get(&dungeon_id);
    let dungeon_name = sde_dungeon
        .map(|d| d.name.trim().to_string())
        .unwrap_or_else(|| format!("Deadspace Dungeon {dungeon_id}"));
    let editor_disallow = crate::services::pve_dungeon_editor::is_dungeon_or_room_mwd_disallowed(
        ctx,
        i64::from(dungeon_id),
        None,
    );
    let sde_disallow = sde_dungeon
        .is_some_and(|d| matches!(d.archetype_id, Some(3 | 4 | 6 | 8 | 24 | 51)));
    let disallow_mwd = editor_disallow || sde_disallow;
    let rooms = resolve_rooms_with_content_fallback(
        ctx,
        sys_id,
        dungeon_id,
        &dungeon_name,
        base_pos,
        has_entry_gate,
    );
    let now = ctx.now_unix();
    let init_obj_id = rooms.first().map(|r| r.room_id * 10 + 1).unwrap_or(0);
    let inst_id = ctx.db.with_mut_snapshot(|snap| {
        let id = DungeonInstanceId::new(snap.alloc_entity_id()).as_i64();
        prune_stale_despawned_instances(&mut snap.dungeon_runtime, now);
        let entry_gate_pos = has_entry_gate.then_some(base_pos);
        snap.dungeon_runtime.insert_instance(DungeonRuntimeInstance {
            instance_id: id,
            dungeon_id,
            dungeon_name: dungeon_name.clone(),
            category: category.to_string(),
            archetype: sde_dungeon_archetype_label(sde_dungeon),
            solar_system_id: sys_id,
            owner_char_id,
            base_position: base_pos,
            disallow_mwd,
            entry_gate_ball_id: None,
            entry_gate_pos,
            entry_gate_locked: false,
            current_room_index: if has_entry_gate { 0 } else { 1 },
            rooms,
            completed: false,
            despawned: false,
            active_objective_id: init_obj_id,
            last_completed_task_id: None,
            completed_task_ids: Vec::new(),
            created_unix: now,
            ..Default::default()
        });
        id
    });
    materialize_dungeon_room_entities(ctx, sys_id, inst_id, 0).await?;
    if !has_entry_gate && !matches!(category, "Expedition" | "Escalation") {
        on_dungeon_room_entered(ctx, sys_id, inst_id, 0, owner_char_id).await;
    }
    Ok(inst_id)
}

pub async fn spawn_dungeon_instance(
    ctx: &ServerContext,
    char_id: i64,
    default_sys_id: Option<u32>,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let sys_id = kwargs
        .get("solarSystemID")
        .and_then(|v| v.as_i64())
        .and_then(|v| u32::try_from(v).ok())
        .or(default_sys_id)
        .ok_or_else(|| BridgeRpcError::user_error("MustBeInSpace"))?;
    let dungeon_id = kwargs
        .get("dungeonID")
        .and_then(|v| v.as_i64())
        .or_else(|| args.first().and_then(|a| a.as_i64()))
        .and_then(|v| u32::try_from(v).ok())
        .unwrap_or(2001);
    let sde_dungeon = ctx.sde.dungeons.get(&dungeon_id);
    let dungeon_name = kwargs
        .get("dungeonName")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| sde_dungeon.map(|d| d.name.clone()))
        .unwrap_or_else(|| format!("Deadspace Dungeon {dungeon_id}"));
    let category = kwargs
        .get("category")
        .and_then(|v| v.as_str())
        .unwrap_or("Deadspace")
        .to_string();
    let base_pos = parse_finite_vec3(kwargs.get("position"), [120_000_000.0, 0.0, 80_000_000.0]);
    let has_entry_gate = kwargs
        .get("hasEntryGate")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let editor_disallow = crate::services::pve_dungeon_editor::is_dungeon_or_room_mwd_disallowed(
        ctx,
        i64::from(dungeon_id),
        None,
    );
    let sde_disallow = sde_dungeon.is_some_and(|d| d.archetype_id == Some(51));
    let disallow_mwd = kwargs
        .get("disallowMWD")
        .and_then(|v| v.as_bool())
        .unwrap_or(editor_disallow || sde_disallow);

    let rooms = parse_rooms_from_pyrep(
        ctx,
        sys_id,
        dungeon_id,
        &dungeon_name,
        base_pos,
        has_entry_gate,
        kwargs.get("rooms").or_else(|| args.get(1)),
    );
    let now = ctx.now_unix();
    let init_obj_id = rooms.first().map(|r| r.room_id * 10 + 1).unwrap_or(0);
    let inst_id = ctx.db.with_mut_snapshot(|snap| {
        let id = DungeonInstanceId::new(snap.alloc_entity_id()).as_i64();
        prune_stale_despawned_instances(&mut snap.dungeon_runtime, now);
        let entry_gate_pos = has_entry_gate.then_some([
            base_pos[0] - 25_000.0,
            base_pos[1],
            base_pos[2],
        ]);
        snap.dungeon_runtime.insert_instance(DungeonRuntimeInstance {
            instance_id: id,
            dungeon_id,
            dungeon_name: dungeon_name.clone(),
            category: category.clone(),
            archetype: sde_dungeon_archetype_label(sde_dungeon),
            solar_system_id: sys_id,
            owner_char_id: char_id,
            base_position: base_pos,
            disallow_mwd,
            entry_gate_ball_id: None,
            entry_gate_pos,
            entry_gate_locked: false,
            current_room_index: if has_entry_gate { 0 } else { 1 },
            rooms,
            completed: false,
            despawned: false,
            active_objective_id: init_obj_id,
            last_completed_task_id: None,
            completed_task_ids: Vec::new(),
            created_unix: now,
            ..Default::default()
        });
        id
    });

    materialize_dungeon_room_entities(ctx, sys_id, inst_id, 0).await?;
    if !has_entry_gate {
        on_dungeon_room_entered(ctx, sys_id, inst_id, 0, char_id).await;
    }

    let rep = read_dungeon_runtime_store(ctx, |store| {
        store
            .get_instance(DungeonInstanceId::new(inst_id))
            .map(|inst| instance_to_pyrep(inst, Some(&ctx.sde)))
            .unwrap_or(PyRep::None)
    });
    Ok(rep)
}

pub fn get_dungeon_instance_state(ctx: &ServerContext, inst_id: i64) -> Result<PyRep, BridgeRpcError> {
    read_dungeon_runtime_store(ctx, |store| {
        store
            .get_instance(DungeonInstanceId::new(inst_id))
            .map(|inst| instance_to_pyrep(inst, Some(&ctx.sde)))
            .ok_or_else(|| BridgeRpcError::user_error("DungeonInstanceNotFound"))
    })
}

pub async fn check_dungeon_triggers(
    ctx: &ServerContext,
    char_id: i64,
    sys_id: u32,
    ship_id: i64,
) -> usize {
    on_dungeon_approach(ctx, char_id, sys_id, ship_id, None).await
}

fn matches_approach_trigger_parts(
    objects: &[DungeonRuntimeObject],
    trig: &DungeonTrigger,
    ship_pos: [f64; 3],
    target_id_opt: Option<i64>,
) -> bool {
    if trig.fired {
        return false;
    }
    let DungeonTriggerKind::OnApproach {
        target_ball_id,
        target_object_index,
        anchor_pos,
        radius_m,
    } = &trig.kind
    else {
        return false;
    };
    let resolved_target_ball = (*target_ball_id).or_else(|| {
        target_object_index
            .and_then(|idx| objects.get(idx))
            .map(|o| o.ball_id)
    });
    let obj_pos = resolved_target_ball
        .and_then(|bid| objects.iter().find(|o| o.ball_id == bid))
        .map(|o| o.position)
        .unwrap_or(*anchor_pos);
    let effective_radius = radius_m.max(2_500.0);
    let within_radius = dist3(ship_pos, obj_pos) <= effective_radius;
    let explicit_target_close = target_id_opt.is_some()
        && target_id_opt == resolved_target_ball
        && dist3(ship_pos, obj_pos) <= effective_radius.max(5_000.0);
    within_radius || explicit_target_close
}

fn matches_loot_trigger_parts(
    objects: &[DungeonRuntimeObject],
    trig: &DungeonTrigger,
    container_ball_id: i64,
    item_type_id: TypeId,
) -> bool {
    if trig.fired {
        return false;
    }
    let DungeonTriggerKind::OnLoot {
        container_ball_id: trig_cont,
        target_object_index,
        item_type_id: trig_tid,
    } = &trig.kind
    else {
        return false;
    };
    let resolved_cont = (*trig_cont).or_else(|| {
        target_object_index
            .and_then(|idx| objects.get(idx))
            .map(|o| o.ball_id)
    });
    let cont_ok = resolved_cont.is_none_or(|cid| cid == container_ball_id);
    let tid_ok = trig_tid.is_none_or(|tid| tid == item_type_id);
    let room_has_cont = objects.iter().any(|o| o.ball_id == container_ball_id);
    cont_ok && tid_ok && (resolved_cont.is_some() || room_has_cont)
}

/// Evaluates `OnApproach` triggers when a pilot issues an approach/orbit/goto/stop command or updates state.
/// Performs read-only snapshot & trigger-distance checks first and ONLY opens a write transaction
/// (`with_dungeon_runtime_store`) when at least one trigger actually transitions (`PVE-212`).
pub async fn on_dungeon_approach(
    ctx: &ServerContext,
    char_id: i64,
    sys_id: SolarSystemId,
    ship_id: i64,
    target_id_opt: Option<i64>,
) -> usize {
    let has_pending_approach = read_dungeon_runtime_store(ctx, |store| {
        store.instances.values().any(|inst| {
            !instance_is_finished(inst)
                && inst.solar_system_id == sys_id
                && inst.rooms.iter().any(|room| {
                    room.spawned
                        && room_is_entered(room)
                        && room.triggers.iter().any(|trig| {
                            !trig.fired
                                && matches!(trig.kind, DungeonTriggerKind::OnApproach { .. })
                        })
                })
        })
    });
    if !has_pending_approach {
        return 0;
    }

    let Some(sys_handle) = ctx.systems.get_handle(sys_id) else {
        return 0;
    };
    let Some((_, ship_ball)) = sys_handle.get_ball_snapshot(ship_id).await.ok().flatten() else {
        return 0;
    };
    let ship_pos = ship_ball.position.to_array();

    let approach_ready = read_dungeon_runtime_store(ctx, |store| {
        store.instances.values().any(|inst| {
            !instance_is_finished(inst)
                && inst.solar_system_id == sys_id
                && inst.rooms.iter().any(|room| {
                    room.spawned
                        && room_is_entered(room)
                        && room.triggers.iter().any(|trig| {
                            matches_approach_trigger_parts(
                                &room.objects,
                                trig,
                                ship_pos,
                                target_id_opt,
                            )
                        })
                })
        })
    });
    if !approach_ready {
        return 0;
    }

    let mut pending_actions = Vec::new();
    with_dungeon_runtime_store(ctx, |store| {
        for inst in store.instances.values_mut() {
            if instance_is_finished(inst) || inst.solar_system_id != sys_id {
                continue;
            }
            let mut any_fired = false;
            for (r_idx, room) in inst.rooms.iter_mut().enumerate() {
                if !room.spawned || !room_is_entered(room) {
                    continue;
                }
                let room_objs = &room.objects;
                let mut room_actions = Vec::new();
                for trig in &mut room.triggers {
                    if matches_approach_trigger_parts(room_objs, trig, ship_pos, target_id_opt) {
                        trig.fired = true;
                        any_fired = true;
                        room_actions.push((trig.trigger_id, trig.action.clone()));
                    }
                }
                if !room_actions.is_empty() {
                    pending_actions.push((inst.instance_id, r_idx, room_actions));
                }
            }
            if any_fired {
                record_dungeon_actor(inst, char_id);
            }
        }
    });

    let mut total_fired = 0usize;
    for (inst_id, r_idx, actions) in pending_actions {
        total_fired +=
            apply_dungeon_trigger_actions(ctx, sys_id, inst_id, r_idx, actions, char_id).await;
    }
    total_fired
}

/// Evaluates `OnLoot` triggers when an item is looted from a container/wreck in space,
/// and finalizes post-completion loot-grace despawn once the last container is emptied (`PVE-207`).
pub async fn on_dungeon_item_looted(
    ctx: &ServerContext,
    char_id: i64,
    sys_id_opt: Option<SolarSystemId>,
    container_ball_id: i64,
    item_type_id: TypeId,
) {
    let (has_matching_loot, loot_grace_finished) = ctx.db.with_snapshot(|snap| {
        let mut trigger_match = false;
        let mut grace_done = Vec::new();
        for inst in snap.dungeon_runtime.instances.values() {
            if sys_id_opt.is_some_and(|sid| inst.solar_system_id != sid) {
                continue;
            }
            if !instance_is_finished(inst) {
                if inst.rooms.iter().any(|room| {
                    room.spawned
                        && room_is_entered(room)
                        && room.triggers.iter().any(|trig| {
                            matches_loot_trigger_parts(
                                &room.objects,
                                trig,
                                container_ball_id,
                                item_type_id,
                            )
                        })
                }) {
                    trigger_match = true;
                }
            } else if (inst.completed || inst.failed)
                && !inst.despawned
                && inst.loot_grace_until_unix.is_some()
                && inst.rooms.iter().any(|r| {
                    r.objects
                        .iter()
                        .any(|o| o.is_container && o.spawned && o.ball_id == container_ball_id)
                })
                && !has_unlooted_containers_in_snap(snap, inst)
            {
                grace_done.push((inst.solar_system_id, inst.instance_id));
            }
        }
        (trigger_match, grace_done)
    });
    if has_matching_loot {
        let mut pending_actions = Vec::new();

        with_dungeon_runtime_store(ctx, |store| {
            for inst in store.instances.values_mut() {
                if instance_is_finished(inst) {
                    continue;
                }
                if let Some(sys_id) = sys_id_opt {
                    if inst.solar_system_id != sys_id {
                        continue;
                    }
                }
                let mut any_fired = false;
                for (r_idx, room) in inst.rooms.iter_mut().enumerate() {
                    if !room.spawned || !room_is_entered(room) {
                        continue;
                    }
                    let room_objs = &room.objects;
                    let mut room_actions = Vec::new();
                    for trig in &mut room.triggers {
                        if matches_loot_trigger_parts(
                            room_objs,
                            trig,
                            container_ball_id,
                            item_type_id,
                        ) {
                            trig.fired = true;
                            any_fired = true;
                            room_actions.push((trig.trigger_id, trig.action.clone()));
                        }
                    }
                    if !room_actions.is_empty() {
                        pending_actions.push((
                            inst.solar_system_id,
                            inst.instance_id,
                            r_idx,
                            room_actions,
                        ));
                    }
                }
                if any_fired {
                    record_dungeon_actor(inst, char_id);
                }
            }
        });

        for (sys_id, inst_id, r_idx, actions) in pending_actions {
            let _ =
                apply_dungeon_trigger_actions(ctx, sys_id, inst_id, r_idx, actions, char_id).await;
        }
    }

    for (sys_id, inst_id) in loot_grace_finished {
        complete_dungeon_instance_core(
            &ctx.systems,
            &ctx.db,
            Some(&ctx.pending_notifications),
            Some(&ctx.notify_tx),
            sys_id,
            inst_id,
            char_id,
            true,
        )
        .await;
    }
}

pub fn record_dungeon_progression_events(
    inst: &mut DungeonRuntimeInstance,
    events: &[(i64, i64)],
) {
    for &(task_id, next_obj_id) in events {
        if !inst.completed_task_ids.contains(&task_id) {
            inst.completed_task_ids.push(task_id);
        }
        inst.last_completed_task_id = Some(task_id);
        inst.active_objective_id = next_obj_id;
    }
}

pub fn dispatch_dungeon_notify_actions(
    db: &eve_db::WorldRepository,
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
    char_id: i64,
    progression_events: &[(i64, i64)],
) {
    for &(task_id, objective_id) in progression_events {
        crate::services::notify_emitters_ess_bounties::emit_progression_task_completed_with_db(
            db,
            pending_notifications,
            notify_tx,
            char_id,
            task_id,
        );
        crate::services::notify_emitters_ess_bounties::emit_progression_advanced_with_db(
            db,
            pending_notifications,
            notify_tx,
            char_id,
            objective_id,
            task_id,
        );
    }
}

fn room_contains_ship_pos(
    inst: &DungeonRuntimeInstance,
    r_idx: usize,
    room: &DungeonRuntimeRoom,
    ship_pos: DVec3,
) -> bool {
    super::pve_dungeon_runtime::room_contains_pocket_pos(
        inst,
        r_idx,
        room,
        [ship_pos.x, ship_pos.y, ship_pos.z],
        250_000.0,
    )
}

fn resolve_current_pocket_room_at_pos(
    inst: &DungeonRuntimeInstance,
    ship_pos: DVec3,
) -> Option<&DungeonRuntimeRoom> {
    if inst.current_room_index == 0 {
        return None;
    }
    inst.rooms
        .iter()
        .enumerate()
        .find(|(r_idx, r)| {
            r.room_index == inst.current_room_index && room_contains_ship_pos(inst, *r_idx, r, ship_pos)
        })
        .map(|(_, r)| r)
        .or_else(|| {
            let idx = inst.current_room_index - 1;
            inst.rooms
                .get(idx)
                .filter(|r| room_contains_ship_pos(inst, idx, r, ship_pos))
        })
}

/// Returns `true` if `ship_pos` in `sys_id` is inside an active deadspace dungeon pocket whose
/// dungeon or room data disallows Microwarpdrive activation (`feat:pve.deadspace-pocket-rules`).
pub fn is_ship_in_mwd_disallowed_dungeon_pocket(
    ctx: &ServerContext,
    sys_id: u32,
    ship_pos: DVec3,
) -> bool {
    read_dungeon_runtime_store(ctx, |store| {
        for inst in store.instances.values() {
            if instance_is_finished(inst)
                || inst.solar_system_id != sys_id
                || inst.current_room_index == 0
            {
                continue;
            }
            if let Some(room) = resolve_current_pocket_room_at_pos(inst, ship_pos) {
                return room.disallow_mwd.unwrap_or(inst.disallow_mwd);
            }
        }
        false
    })
}

/// Refuses direct `CmdWarpToStuff` into a gated deadspace dungeon room that has not yet been
/// unlocked/entered via its acceleration gate (`feat:pve.deadspace-pocket-rules`).
pub fn check_warp_target_blocked_by_gated_dungeon_room(
    ctx: &ServerContext,
    sys_id: u32,
    target_pos: DVec3,
    target_ball_id: Option<i64>,
) -> Result<(), BridgeRpcError> {
    let blocked = read_dungeon_runtime_store(ctx, |store| {
        for inst in store.instances.values() {
            if instance_is_finished(inst) || inst.solar_system_id != sys_id {
                continue;
            }
            for (r_idx, room) in inst.rooms.iter().enumerate() {
                let is_gated = r_idx > 0 || inst.entry_gate_ball_id.is_some();
                if !is_gated {
                    continue;
                }
                let matches_ball = target_ball_id.is_some_and(|tid| {
                    room.objects.iter().any(|o| o.ball_id == tid)
                        || room.gate_ball_id == Some(tid)
                        || room.gate_links.iter().any(|gl| gl.gate_ball_id == Some(tid))
                });
                let matches_pos = super::pve_dungeon_runtime::room_contains_pocket_pos(
                    inst,
                    r_idx,
                    room,
                    [target_pos.x, target_pos.y, target_pos.z],
                    150_000.0,
                );
                if matches_ball || matches_pos {
                    return true;
                }
            }
        }
        false
    });

    if blocked {
        return Err(BridgeRpcError {
            code: "WarpBlockedByDeadspacePocket".into(),
            message: "Cannot warp directly into a gated deadspace pocket; use the acceleration gate".into(),
            client_error: None,
        });
    }
    Ok(())
}

/// Checks if `ship_id` in `sys_id` is inside a deadspace pocket that disallows MWD activation.
pub async fn check_deadspace_mwd_disallowed(
    ctx: &ServerContext,
    sys: &crate::system_actor::SystemHandle,
    sys_id: u32,
    ship_id: i64,
) -> Result<(), BridgeRpcError> {
    let ship_pos = sys
        .get_ball_snapshot(ship_id)
        .await
        .ok()
        .flatten()
        .map(|(_, b)| b.position)
        .unwrap_or(DVec3::ZERO);
    if is_ship_in_mwd_disallowed_dungeon_pocket(ctx, sys_id, ship_pos) {
        let has_explicit_room_flag = read_dungeon_runtime_store(ctx, |st| {
            st.instances.values().any(|i| {
                !instance_is_finished(i)
                    && i.solar_system_id == sys_id
                    && resolve_current_pocket_room_at_pos(i, ship_pos)
                        .is_some_and(|r| r.disallow_mwd == Some(true))
            })
        });
        if !has_explicit_room_flag {
            return crate::services::pve_dungeon_runtime::check_deadspace_mwd_allowed(ctx, sys_id, ship_pos.to_array());
        }
        return Err(BridgeRpcError {
            code: "MicroWarpdriveDisallowedInDeadspace".into(),
            message: "Microwarpdrive activation is disallowed in this deadspace pocket".into(),
            client_error: None,
        });
    }
    Ok(())
}

pub fn register_sde_operation_dungeon_instance(
    ctx: &ServerContext,
    owner_char_id: i64,
    sys_id: u32,
    dungeon_id: u32,
) -> i64 {
    let base_pos = [0.0, 0.0, 0.0];
    let existing = read_dungeon_runtime_store(ctx, |store| {
        store.instances.values().find_map(|inst| {
            (!instance_is_finished(inst)
                && inst.solar_system_id == sys_id
                && inst.dungeon_id == dungeon_id)
                .then_some(inst.instance_id)
        })
    });
    if let Some(inst_id) = existing {
        return inst_id;
    }
    let sde_dungeon = ctx.sde.dungeons.get(&dungeon_id);
    let dungeon_name = sde_dungeon
        .map(|d| d.name.trim().to_string())
        .unwrap_or_else(|| format!("Operation Dungeon {dungeon_id}"));
    let disallow_mwd = sde_dungeon.is_some_and(|d| matches!(d.archetype_id, Some(3 | 4 | 6 | 8 | 24 | 51)));
    let mut rooms = resolve_rooms_with_content_fallback(
        ctx,
        sys_id,
        dungeon_id,
        &dungeon_name,
        base_pos,
        false,
    );
    let now = ctx.now_unix();
    if let Some(r0) = rooms.first_mut() {
        r0.entered = true;
        r0.entered_unix = Some(now);
    }
    let total_rooms = rooms.len().max(1) as u8;
    let room_id = rooms.first().map(|r| r.room_id).unwrap_or(i64::from(dungeon_id) * 100 + 1);
    let inst_id = ctx.db.with_mut_snapshot(|snap| {
        let id = DungeonInstanceId::new(snap.alloc_entity_id()).as_i64();
        prune_stale_despawned_instances(&mut snap.dungeon_runtime, now);
        snap.dungeon_runtime.insert_instance(DungeonRuntimeInstance {
            instance_id: id,
            dungeon_id,
            dungeon_name: dungeon_name.clone(),
            category: "Operation".to_string(),
            archetype: sde_dungeon_archetype_label(sde_dungeon),
            solar_system_id: sys_id,
            owner_char_id,
            base_position: base_pos,
            disallow_mwd,
            entry_gate_ball_id: None,
            entry_gate_pos: None,
            entry_gate_locked: false,
            current_room_index: 1,
            rooms,
            completed: false,
            despawned: false,
            created_unix: now,
            active_objective_id: 0,
            completed_task_ids: Vec::new(),
            last_completed_task_id: None,
            ..Default::default()
        });
        id
    });
    let expiry_ft = unix_to_filetime_ticks(now.saturating_add(86_400));
    emit_dungeon_progression_notifications(
        ctx, owner_char_id, sys_id, "Operation", &dungeon_name, i64::from(dungeon_id),
        inst_id, room_id, base_pos, 1, total_rooms, false, expiry_ft,
    );
    inst_id
}

#[cfg(test)]
#[path = "pve_dungeon_runtime_rpc_tests.rs"]
mod pve_dungeon_runtime_rpc_tests;

