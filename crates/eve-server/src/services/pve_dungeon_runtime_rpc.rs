//! `pve_dungeon_runtime_rpc` — Acceleration Gate Pocket Traversal & Keeper/DungeonInstanceCacheMgr
//! RPC Handlers (`feat:pve.deadspace-pocket-rules`, `feat:pve.dungeon-system`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::notify_emitters_batch3_space::emit_dungeon_progression_notifications;
use crate::services::pve_dungeon_runtime::{
    materialize_dungeon_room_entities, on_dungeon_approach, unix_to_filetime_ticks,
    with_dungeon_runtime_store, DungeonNpcGroup, DungeonRuntimeInstance, DungeonRuntimeObject,
    DungeonRuntimeRoom, DungeonTrigger, DungeonTriggerAction, DungeonTriggerKind,
    DEFAULT_CONTAINER_TYPE_ID, GATE_ACTIVATION_RANGE_M,
};
use eve_destiny::MovementMode;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::SolarSystemId;
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

    let match_info = with_dungeon_runtime_store(ctx, |store| {
        for inst in store.instances.values() {
            if inst.completed || inst.despawned || inst.solar_system_id != sys_id {
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
                    ));
                }
                for (r_idx, r) in inst.rooms.iter().enumerate() {
                    if r.gate_ball_id == Some(gate_id) && r_idx + 1 < inst.rooms.len() {
                        let gate_pos = Some([r.position[0] + 5_000.0, r.position[1], r.position[2]]);
                        return Some((
                            inst.instance_id,
                            r_idx + 1,
                            gate_pos,
                            r.gate_locked,
                            gate_id,
                        ));
                    }
                }
            }
            if let Some(s) = arg_str {
                if s == format!("DNG-INST-{}", inst.instance_id) {
                    let next_idx = inst.current_room_index.min(inst.rooms.len().saturating_sub(1));
                    let cur_room = inst.rooms.get(inst.current_room_index.saturating_sub(1));
                    let gate_id = cur_room.and_then(|r| r.gate_ball_id).unwrap_or(0);
                    let gate_pos = cur_room.map(|r| [r.position[0] + 5_000.0, r.position[1], r.position[2]]);
                    let locked = cur_room.is_some_and(|r| r.gate_locked);
                    return Some((inst.instance_id, next_idx, gate_pos, locked, gate_id));
                }
            }
        }
        None
    });

    let (instance_id, target_room_idx, gate_pos_opt, gate_locked, gate_id) = match_info?;

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
            }));
        }
    }

    if gate_locked {
        return Some(Err(BridgeRpcError {
            code: "GateLocked".into(),
            message: "Acceleration gate is locked until room objectives are completed".into(),
        }));
    }

    // Check SDE dungeon difficulty hull restrictions if any
    let dungeon_id = with_dungeon_runtime_store(ctx, |store| {
        store.instances.get(&instance_id).map(|i| i.dungeon_id)
    })
    .unwrap_or(0);
    if let Some(dungeon) = ctx.sde.dungeons.get(&dungeon_id) {
        let ship_type = ctx.sde.types.get(&ship_ball.type_id);
        let ship_rig_size = ship_type
            .and_then(|t| t.default_attributes.get(&1547).copied())
            .unwrap_or(0.0);
        if dungeon.difficulty == Some(1) && (ship_rig_size > 1.0 || ship_ball.mass > 15_000_000.0) {
            return Some(Err(BridgeRpcError::user_error("ShipRestrictedByDungeonGate")));
        }
    }

    // Materialize target room entities
    let (obj_ids, npc_ids, next_gate_id) =
        match materialize_dungeon_room_entities(ctx, sys_id, instance_id, target_room_idx).await {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };

    // Update current_room_index and get landing destination
    let room_info = with_dungeon_runtime_store(ctx, |store| {
        let inst = store.instances.get_mut(&instance_id)?;
        inst.current_room_index = target_room_idx + 1;
        let room = inst.rooms.get(target_room_idx)?;
        Some((
            inst.dungeon_id,
            inst.category.clone(),
            inst.dungeon_name.clone(),
            room.room_id,
            room.room_index,
            room.gate_destination_pos,
            inst.rooms.len(),
            inst.created_unix,
            room.disallow_mwd.unwrap_or(inst.disallow_mwd),
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
        dest_pos,
        room_index as u8,
        total_rooms.max(1) as u8,
        false,
        expiry_ft,
    );

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

fn parse_rooms_from_pyrep(
    ctx: &ServerContext,
    dungeon_id: u32,
    dungeon_name: &str,
    base_pos: [f64; 3],
    has_entry_gate: bool,
    rooms_arg: Option<&PyRep>,
) -> Vec<DungeonRuntimeRoom> {
    if let Some(PyRep::List(room_list) | PyRep::Tuple(room_list)) = rooms_arg {
        let mut parsed = Vec::new();
        for (idx, r_rep) in room_list.iter().enumerate() {
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
                for (o_i, o_rep) in objs.iter().enumerate() {
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
                    let offset = match o_rep.dict_get("offset") {
                        Some(PyRep::List(v) | PyRep::Tuple(v)) if v.len() >= 3 => [
                            v[0].as_f64().unwrap_or(0.0),
                            v[1].as_f64().unwrap_or(0.0),
                            v[2].as_f64().unwrap_or(0.0),
                        ],
                        _ => [(o_i as f64 + 1.0) * 3_500.0, 0.0, 2_000.0],
                    };
                    let radius = o_rep
                        .dict_get("radius")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(300.0);
                    let is_container = o_rep
                        .dict_get("isContainer")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let mut loot_items = Vec::new();
                    if let Some(PyRep::List(loots) | PyRep::Tuple(loots)) = o_rep.dict_get("lootItems") {
                        for l_rep in loots {
                            if let PyRep::List(pair) | PyRep::Tuple(pair) = l_rep {
                                if pair.len() >= 2 {
                                    if let (Some(ltid), Some(lqty)) = (
                                        pair[0].as_i64().and_then(|v| u32::try_from(v).ok()),
                                        pair[1].as_i64().map(|v| v as i32),
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
                    });
                }
            }

            let mut npc_groups = Vec::new();
            if let Some(PyRep::List(grps) | PyRep::Tuple(grps)) = r_rep.dict_get("npcGroups") {
                for (g_i, g_rep) in grps.iter().enumerate() {
                    let group_id = g_rep
                        .dict_get("groupID")
                        .and_then(|v| v.as_i64())
                        .unwrap_or((g_i + 1) as i64) as u8;
                    let wave = g_rep
                        .dict_get("wave")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(1) as u8;
                    let spawn_immediately = g_rep
                        .dict_get("spawnImmediately")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(wave == 1);
                    let mut npc_specs = Vec::new();
                    if let Some(PyRep::List(npcs) | PyRep::Tuple(npcs)) = g_rep.dict_get("npcs") {
                        for n_rep in npcs {
                            if let PyRep::List(pair) | PyRep::Tuple(pair) = n_rep {
                                if pair.len() >= 2 {
                                    if let (Some(ntid), Some(ncnt)) = (
                                        pair[0].as_i64().and_then(|v| u32::try_from(v).ok()),
                                        pair[1].as_i64().map(|v| v.max(1) as usize),
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
                    });
                }
            }

            let mut triggers = Vec::new();
            if let Some(PyRep::List(trigs) | PyRep::Tuple(trigs)) = r_rep.dict_get("triggers") {
                for (t_i, t_rep) in trigs.iter().enumerate() {
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
                        .unwrap_or(2) as u8;
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
                                .map(|v| v as usize);
                            let radius_m = t_rep
                                .dict_get("radius")
                                .and_then(|v| v.as_f64())
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
                                .map(|v| v as usize);
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
                                .map(|v| v as u8);
                            DungeonTriggerKind::OnKill {
                                target_npc_group: target_grp,
                                target_ball_id: None,
                            }
                        }
                    };
                    triggers.push(DungeonTrigger {
                        trigger_id: (t_i + 1) as u32,
                        kind,
                        action,
                        fired: false,
                    });
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
            });
        }
        if !parsed.is_empty() {
            return parsed;
        }
    }

    // Default 2-room gated deadspace dungeon derived from SDE dungeon template
    let r1_pos = base_pos;
    let r2_pos = [base_pos[0] + 500_000.0, base_pos[1], base_pos[2]];
    vec![
        DungeonRuntimeRoom {
            room_index: 1,
            room_id: i64::from(dungeon_id) * 100 + 1,
            room_name: format!("{dungeon_name} - Room 1"),
            position: r1_pos,
            gate_destination_pos: [r1_pos[0] - 10_000.0, r1_pos[1], r1_pos[2]],
            gated: has_entry_gate,
            disallow_mwd: None,
            gate_ball_id: None,
            gate_locked: true,
            objects: vec![DungeonRuntimeObject {
                ball_id: 0,
                type_id: 23,
                name: "Deadspace Relay Beacon".into(),
                position: [r1_pos[0] + 2_500.0, r1_pos[1], r1_pos[2]],
                radius: 250.0,
                is_container: false,
                loot_items: Vec::new(),
                spawned: false,
            }],
            npc_groups: vec![DungeonNpcGroup {
                group_id: 1,
                wave: 1,
                npc_specs: vec![(10_043, 2)],
                npc_ball_ids: Vec::new(),
                spawn_immediately: true,
                spawned: false,
                cleared: false,
            }],
            triggers: vec![DungeonTrigger {
                trigger_id: 1,
                kind: DungeonTriggerKind::OnKill {
                    target_npc_group: Some(1),
                    target_ball_id: None,
                },
                action: DungeonTriggerAction::UnlockGate,
                fired: false,
            }],
            spawned: false,
            cleared: false,
        },
        DungeonRuntimeRoom {
            room_index: 2,
            room_id: i64::from(dungeon_id) * 100 + 2,
            room_name: format!("{dungeon_name} - Room 2"),
            position: r2_pos,
            gate_destination_pos: [r2_pos[0] - 10_000.0, r2_pos[1], r2_pos[2]],
            gated: true,
            disallow_mwd: None,
            gate_ball_id: None,
            gate_locked: false,
            objects: vec![DungeonRuntimeObject {
                ball_id: 0,
                type_id: 23,
                name: "Overseer Command Bunker".into(),
                position: [r2_pos[0] + 3_000.0, r2_pos[1], r2_pos[2]],
                radius: 400.0,
                is_container: false,
                loot_items: Vec::new(),
                spawned: false,
            }],
            npc_groups: vec![DungeonNpcGroup {
                group_id: 1,
                wave: 1,
                npc_specs: vec![(10_043, 1)],
                npc_ball_ids: Vec::new(),
                spawn_immediately: true,
                spawned: false,
                cleared: false,
            }],
            triggers: vec![DungeonTrigger {
                trigger_id: 1,
                kind: DungeonTriggerKind::OnKill {
                    target_npc_group: Some(1),
                    target_ball_id: None,
                },
                action: DungeonTriggerAction::CompleteDungeon,
                fired: false,
            }],
            spawned: false,
            cleared: false,
        },
    ]
}

fn instance_to_pyrep(inst: &DungeonRuntimeInstance) -> PyRep {
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
            ])
        })
        .collect();

    PyRep::keyval([
        ("instanceID", PyRep::Integer(inst.instance_id)),
        ("dungeonID", PyRep::Integer(i64::from(inst.dungeon_id))),
        ("dungeonName", PyRep::String(inst.dungeon_name.clone())),
        ("category", PyRep::String(inst.category.clone())),
        ("solarSystemID", PyRep::Integer(i64::from(inst.solar_system_id))),
        ("disallowMWD", PyRep::Bool(inst.disallow_mwd)),
        (
            "entryGateBallID",
            inst.entry_gate_ball_id
                .map(PyRep::Integer)
                .unwrap_or(PyRep::None),
        ),
        ("currentRoomIndex", PyRep::Integer(inst.current_room_index as i64)),
        ("completed", PyRep::Bool(inst.completed)),
        ("despawned", PyRep::Bool(inst.despawned)),
        ("rooms", PyRep::List(rooms_py)),
    ])
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
        .unwrap_or(30_000_142);
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
    let base_pos = match kwargs.get("position") {
        Some(PyRep::List(v) | PyRep::Tuple(v)) if v.len() >= 3 => [
            v[0].as_f64().unwrap_or(120_000_000.0),
            v[1].as_f64().unwrap_or(0.0),
            v[2].as_f64().unwrap_or(80_000_000.0),
        ],
        _ => [120_000_000.0, 0.0, 80_000_000.0],
    };
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
        dungeon_id,
        &dungeon_name,
        base_pos,
        has_entry_gate,
        kwargs.get("rooms").or_else(|| args.get(1)),
    );
    let now = ctx.now_unix();
    let inst_id = with_dungeon_runtime_store(ctx, |store| {
        let id = store.next_instance_id;
        store.next_instance_id += 1;
        let entry_gate_pos = has_entry_gate.then_some([
            base_pos[0] - 25_000.0,
            base_pos[1],
            base_pos[2],
        ]);
        store.instances.insert(
            id,
            DungeonRuntimeInstance {
                instance_id: id,
                dungeon_id,
                dungeon_name: dungeon_name.clone(),
                category: category.clone(),
                archetype: dungeon_name.clone(),
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
                created_unix: now,
            },
        );
        id
    });

    materialize_dungeon_room_entities(ctx, sys_id, inst_id, 0).await?;

    let rep = with_dungeon_runtime_store(ctx, |store| {
        store
            .instances
            .get(&inst_id)
            .map(instance_to_pyrep)
            .unwrap_or(PyRep::None)
    });
    Ok(rep)
}

pub fn get_dungeon_instance_state(ctx: &ServerContext, inst_id: i64) -> Result<PyRep, BridgeRpcError> {
    with_dungeon_runtime_store(ctx, |store| {
        store
            .instances
            .get(&inst_id)
            .map(instance_to_pyrep)
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
