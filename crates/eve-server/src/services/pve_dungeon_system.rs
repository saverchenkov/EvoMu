//! `pve_dungeon_system` — Dungeon instance system runtime (feat:pve.dungeon-system).
//!
//! Spawns multi-room dungeon instances with celestial objects, acceleration gates,
//! NPC groups, and triggers (on kill, approach, loot), and despawns all room entities
//! when completed.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DungeonTriggerKind {
    OnKill,
    OnApproach,
    OnLoot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DungeonTriggerAction {
    SpawnWave(u8),
    UnlockGate,
    CompleteDungeon,
}

#[derive(Debug, Clone)]
pub struct LiveDungeonTrigger {
    pub trigger_id: u32,
    pub kind: DungeonTriggerKind,
    pub target_id: i64,
    pub radius: f64,
    pub action: DungeonTriggerAction,
    pub fired: bool,
}

#[derive(Debug, Clone)]
pub struct LiveDungeonRoom {
    pub room_id: u32,
    pub name: String,
    pub position: [f64; 3],
    pub object_ids: Vec<i64>,
    pub npc_ball_ids: Vec<i64>,
    pub triggers: Vec<LiveDungeonTrigger>,
    pub gate_unlocked: bool,
    pub cleared: bool,
}

#[derive(Debug, Clone)]
pub struct LiveDungeonInstance {
    pub instance_id: i64,
    pub dungeon_id: u32,
    pub dungeon_name: String,
    pub solar_system_id: u32,
    pub rooms: Vec<LiveDungeonRoom>,
    pub current_room_idx: usize,
    pub completed: bool,
    pub created_unix: u64,
}

static DUNGEON_STATE: LazyLock<Mutex<DungeonRuntimeState>> =
    LazyLock::new(|| Mutex::new(DungeonRuntimeState::default()));

#[derive(Default)]
struct DungeonRuntimeState {
    next_instance_id: i64,
    instances: HashMap<i64, LiveDungeonInstance>,
}

fn with_dungeon_state<R>(f: impl FnOnce(&mut DungeonRuntimeState) -> R) -> R {
    let mut guard = DUNGEON_STATE.lock().unwrap();
    f(&mut guard)
}

pub fn get_dungeon_instance(instance_id: i64) -> Option<LiveDungeonInstance> {
    with_dungeon_state(|st| st.instances.get(&instance_id).cloned())
}

/// Spawns a complete dungeon instance with rooms, objects, NPC groups, and triggers.
pub async fn spawn_dungeon_instance(
    ctx: &ServerContext,
    dungeon_id: u32,
    solar_system_id: u32,
    origin_pos: [f64; 3],
) -> Result<i64, BridgeRpcError> {
    let d_name = ctx
        .sde
        .dungeons
        .get(&dungeon_id)
        .map(|d| d.name.clone())
        .unwrap_or_else(|| format!("Dungeon {dungeon_id}"));

    let instance_id = with_dungeon_state(|st| {
        st.next_instance_id += 1;
        800_000_000 + st.next_instance_id
    });

    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
    let now = ctx.now_unix();

    // 2-room dungeon layout: Room 1 at origin, Room 2 offset by 100,000 km
    let r1_pos = origin_pos;
    let r2_pos = [
        origin_pos[0] + 100_000_000.0,
        origin_pos[1],
        origin_pos[2],
    ];

    // Room 1 objects: acceleration gate + loot container + beacon
    let gate1_id = instance_id * 10 + 1;
    let can1_id = instance_id * 10 + 2;
    let beacon1_id = instance_id * 10 + 3;

    let _ = sys_handle
        .spawn_ball(Ball {
            id: gate1_id,
            type_id: 17831, // Acceleration Gate
            position: DVec3::new(r1_pos[0] + 1_000.0, r1_pos[1], r1_pos[2]),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 0.0,
            mass: 50_000_000.0,
            radius: 200.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    let _ = sys_handle
        .spawn_ball(Ball {
            id: can1_id,
            type_id: 23, // Cargo Container
            position: DVec3::new(r1_pos[0] + 3_000.0, r1_pos[1], r1_pos[2]),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 0.0,
            mass: 1_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    let _ = sys_handle
        .spawn_ball(Ball {
            id: beacon1_id,
            type_id: 17982, // Warp In Beacon
            position: DVec3::new(r1_pos[0], r1_pos[1], r1_pos[2]),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 0.0,
            mass: 10_000.0,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    // Room 1 initial NPC group: 2 pirate frigates
    let npc1_id = instance_id * 100 + 1;
    let npc2_id = instance_id * 100 + 2;

    for (nid, offset_x) in [(npc1_id, 8_000.0), (npc2_id, 10_000.0)] {
        let _ = sys_handle
            .spawn_ball(Ball {
                id: nid,
                type_id: 10515, // Pirate Frigate
                position: DVec3::new(r1_pos[0] + offset_x, r1_pos[1] + 2_000.0, r1_pos[2]),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 2.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
    }

    // Room 1 triggers:
    // 1. OnKill trigger: destroying room 1 NPCs unlocks acceleration gate
    let t_kill = LiveDungeonTrigger {
        trigger_id: 1,
        kind: DungeonTriggerKind::OnKill,
        target_id: 0, // all room hostiles
        radius: 0.0,
        action: DungeonTriggerAction::UnlockGate,
        fired: false,
    };

    // 2. OnApproach trigger: approaching container within 2,500m spawns reinforcement wave
    let t_approach = LiveDungeonTrigger {
        trigger_id: 2,
        kind: DungeonTriggerKind::OnApproach,
        target_id: can1_id,
        radius: 2_500.0,
        action: DungeonTriggerAction::SpawnWave(2),
        fired: false,
    };

    let room1 = LiveDungeonRoom {
        room_id: 1,
        name: format!("{d_name} - Entry Pocket"),
        position: r1_pos,
        object_ids: vec![gate1_id, can1_id, beacon1_id],
        npc_ball_ids: vec![npc1_id, npc2_id],
        triggers: vec![t_kill, t_approach],
        gate_unlocked: false,
        cleared: false,
    };

    // Room 2: Inner Sanctum with commander ship and final objective container
    let can2_id = instance_id * 10 + 4;
    let beacon2_id = instance_id * 10 + 5;
    let npc_boss_id = instance_id * 100 + 3;

    let _ = sys_handle
        .spawn_ball(Ball {
            id: can2_id,
            type_id: 23,
            position: DVec3::new(r2_pos[0] + 2_000.0, r2_pos[1], r2_pos[2]),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 0.0,
            mass: 1_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    let _ = sys_handle
        .spawn_ball(Ball {
            id: beacon2_id,
            type_id: 17982,
            position: DVec3::new(r2_pos[0], r2_pos[1], r2_pos[2]),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 0.0,
            mass: 10_000.0,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    let _ = sys_handle
        .spawn_ball(Ball {
            id: npc_boss_id,
            type_id: 20914, // Pirate Commander Cruiser
            position: DVec3::new(r2_pos[0] + 5_000.0, r2_pos[1], r2_pos[2]),
            velocity: DVec3::ZERO,
            max_velocity: 250.0,
            inertia: 3.0,
            mass: 10_000_000.0,
            radius: 120.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    // Room 2 OnLoot trigger: looting can2 completes the dungeon
    let t_loot = LiveDungeonTrigger {
        trigger_id: 3,
        kind: DungeonTriggerKind::OnLoot,
        target_id: can2_id,
        radius: 0.0,
        action: DungeonTriggerAction::CompleteDungeon,
        fired: false,
    };

    let room2 = LiveDungeonRoom {
        room_id: 2,
        name: format!("{d_name} - Sanctum"),
        position: r2_pos,
        object_ids: vec![can2_id, beacon2_id],
        npc_ball_ids: vec![npc_boss_id],
        triggers: vec![t_loot],
        gate_unlocked: false,
        cleared: false,
    };

    let inst = LiveDungeonInstance {
        instance_id,
        dungeon_id,
        dungeon_name: d_name,
        solar_system_id,
        rooms: vec![room1, room2],
        current_room_idx: 0,
        completed: false,
        created_unix: now,
    };

    with_dungeon_state(|st| {
        st.instances.insert(instance_id, inst);
    });

    Ok(instance_id)
}

/// Evaluates kill triggers in the dungeon instance.
pub async fn trigger_on_kill(
    ctx: &ServerContext,
    instance_id: i64,
    killed_ball_id: i64,
) -> Result<Option<DungeonTriggerAction>, BridgeRpcError> {
    let (sys_id, action_opt) = with_dungeon_state(|st| {
        let Some(inst) = st.instances.get_mut(&instance_id) else {
            return (0, None);
        };
        let room = &mut inst.rooms[inst.current_room_idx];
        room.npc_ball_ids.retain(|&id| id != killed_ball_id);
        let all_dead = room.npc_ball_ids.is_empty();

        let mut triggered = None;
        if all_dead {
            room.cleared = true;
            for t in &mut room.triggers {
                if t.kind == DungeonTriggerKind::OnKill && !t.fired {
                    t.fired = true;
                    if t.action == DungeonTriggerAction::UnlockGate {
                        room.gate_unlocked = true;
                    }
                    triggered = Some(t.action.clone());
                    break;
                }
            }
        }
        (inst.solar_system_id, triggered)
    });

    if sys_id > 0 {
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let _ = sys_handle.remove_ball(killed_ball_id).await;
    }

    Ok(action_opt)
}

/// Evaluates approach triggers based on pilot's current position.
pub async fn trigger_on_approach(
    ctx: &ServerContext,
    instance_id: i64,
    pilot_pos: [f64; 3],
) -> Result<Option<DungeonTriggerAction>, BridgeRpcError> {
    let mut to_spawn = None;
    let (sys_id, action_opt) = with_dungeon_state(|st| {
        let Some(inst) = st.instances.get_mut(&instance_id) else {
            return (0, None);
        };
        let room = &mut inst.rooms[inst.current_room_idx];
        let p_pos = DVec3::new(pilot_pos[0], pilot_pos[1], pilot_pos[2]);

        for t in &mut room.triggers {
            if t.kind == DungeonTriggerKind::OnApproach && !t.fired {
                // Approximate distance to room center + target
                let t_pos = DVec3::new(
                    room.position[0] + 3_000.0,
                    room.position[1],
                    room.position[2],
                );
                if p_pos.distance(t_pos) <= t.radius {
                    t.fired = true;
                    if let DungeonTriggerAction::SpawnWave(wave) = t.action {
                        to_spawn = Some((wave, room.position));
                    }
                    return (inst.solar_system_id, Some(t.action.clone()));
                }
            }
        }
        (inst.solar_system_id, None)
    });

    if let Some((_, r_pos)) = to_spawn {
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let amb_id = instance_id * 100 + 4;
        let _ = sys_handle
            .spawn_ball(Ball {
                id: amb_id,
                type_id: 10515,
                position: DVec3::new(r_pos[0] + 4_000.0, r_pos[1], r_pos[2]),
                velocity: DVec3::ZERO,
                max_velocity: 320.0,
                inertia: 2.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
        with_dungeon_state(|st| {
            if let Some(inst) = st.instances.get_mut(&instance_id) {
                inst.rooms[inst.current_room_idx].npc_ball_ids.push(amb_id);
                inst.rooms[inst.current_room_idx].cleared = false;
                inst.rooms[inst.current_room_idx].gate_unlocked = false;
            }
        });
    }

    Ok(action_opt)
}

/// Evaluates loot triggers when a container is opened or looted.
pub async fn trigger_on_loot(
    _ctx: &ServerContext,
    instance_id: i64,
    container_id: i64,
) -> Result<Option<DungeonTriggerAction>, BridgeRpcError> {
    let action_opt = with_dungeon_state(|st| {
        let Some(inst) = st.instances.get_mut(&instance_id) else {
            return None;
        };
        let room = &mut inst.rooms[inst.current_room_idx];
        for t in &mut room.triggers {
            if t.kind == DungeonTriggerKind::OnLoot && !t.fired && t.target_id == container_id {
                t.fired = true;
                if t.action == DungeonTriggerAction::CompleteDungeon {
                    inst.completed = true;
                }
                return Some(t.action.clone());
            }
        }
        None
    });

    Ok(action_opt)
}

/// Enforces ship hull classification restrictions from SDE dogma (`ATTR_RIG_SIZE` 1547) and dungeon metadata.
pub fn validate_acceleration_gate_ship(
    ctx: &ServerContext,
    dungeon_id: u32,
    ship_type_id: u32,
) -> Result<(), BridgeRpcError> {
    let ship_type = ctx.sde.types.get(&ship_type_id);
    let rig_size = ship_type
        .and_then(|t| t.default_attributes.get(&1547).copied())
        .unwrap_or(0.0);
    let mass = ship_type.map(|t| t.mass).unwrap_or(0.0);

    // Capital hulls (rigSize 4.0 or mass > 500M kg) cannot use standard acceleration gates
    if rig_size >= 4.0 || mass > 500_000_000.0 {
        return Err(BridgeRpcError {
            code: "ShipTooLargeForGate".into(),
            message: "Capital ships cannot activate this acceleration gate".into(),
        });
    }

    if let Some(dungeon) = ctx.sde.dungeons.get(&dungeon_id) {
        match dungeon.difficulty {
            Some(1) => {
                if rig_size > 1.0 || mass > 15_000_000.0 {
                    return Err(BridgeRpcError {
                        code: "ShipRestrictedByDungeonGate".into(),
                        message: format!(
                            "Dungeon '{}' (ID {dungeon_id}, difficulty 1) restricts gate to Frigate/Destroyer hulls",
                            dungeon.name
                        ),
                    });
                }
            }
            Some(2) | Some(3) => {
                if rig_size >= 3.0 || mass >= 50_000_000.0 {
                    return Err(BridgeRpcError {
                        code: "ShipRestrictedByDungeonGate".into(),
                        message: format!(
                            "Dungeon '{}' (ID {dungeon_id}) restricts gate against Battleship hulls",
                            dungeon.name
                        ),
                    });
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Advances dungeon to the next room when the acceleration gate is activated.
pub fn advance_dungeon_room(instance_id: i64) -> Result<u32, BridgeRpcError> {
    advance_dungeon_room_with_ship(None, None, instance_id)
}

/// Advances dungeon to the next room, optionally validating ship hull restrictions against SDE dogma.
pub fn advance_dungeon_room_with_ship(
    ctx_opt: Option<&ServerContext>,
    ship_type_id: Option<u32>,
    instance_id: i64,
) -> Result<u32, BridgeRpcError> {
    with_dungeon_state(|st| {
        let Some(inst) = st.instances.get_mut(&instance_id) else {
            return Err(BridgeRpcError {
                code: "DungeonNotFound".into(),
                message: format!("Dungeon instance {instance_id} not found"),
            });
        };
        let room = &inst.rooms[inst.current_room_idx];
        if !room.gate_unlocked && !room.cleared {
            return Err(BridgeRpcError {
                code: "RoomNotCleared".into(),
                message: "Room gate is locked: destroy remaining hostiles or complete triggers"
                    .into(),
            });
        }
        if let (Some(ctx), Some(stid)) = (ctx_opt, ship_type_id) {
            validate_acceleration_gate_ship(ctx, inst.dungeon_id, stid)?;
        }
        if inst.current_room_idx + 1 < inst.rooms.len() {
            inst.current_room_idx += 1;
            Ok(inst.rooms[inst.current_room_idx].room_id)
        } else {
            inst.completed = true;
            Ok(inst.rooms[inst.current_room_idx].room_id)
        }
    })
}

/// Despawns all objects, gates, and NPC balls when dungeon is completed.
pub async fn despawn_dungeon_instance(
    ctx: &ServerContext,
    instance_id: i64,
) -> Result<usize, BridgeRpcError> {
    let (sys_id, balls_to_remove) = with_dungeon_state(|st| {
        let Some(mut inst) = st.instances.remove(&instance_id) else {
            return (0, Vec::new());
        };
        inst.completed = true;
        let mut balls = Vec::new();
        for r in &inst.rooms {
            balls.extend_from_slice(&r.object_ids);
            balls.extend_from_slice(&r.npc_ball_ids);
        }
        (inst.solar_system_id, balls)
    });

    if sys_id > 0 {
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        for bid in &balls_to_remove {
            let _ = sys_handle.remove_ball(*bid).await;
        }
    }

    Ok(balls_to_remove.len())
}

pub async fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if service != "keeper" && service != "dungeon" {
        return None;
    }

    match (service, method) {
        ("keeper", "PlayDungeon") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "PlayDungeon requires a solar system in session".into(),
                }));
            };
            let did = args
                .first()
                .and_then(|a| a.as_i64())
                .and_then(|v| u32::try_from(v).ok())
                .unwrap_or(111);

            let pos = [10_000_000.0, 5_000_000.0, -2_000_000.0];
            match spawn_dungeon_instance(ctx, did, sys_id, pos).await {
                Ok(inst_id) => Some(Ok(PyRep::keyval([
                    ("instanceID", PyRep::Integer(inst_id)),
                    ("dungeonID", PyRep::Integer(did as i64)),
                    ("solarSystemID", PyRep::Integer(sys_id as i64)),
                    ("currentRoom", PyRep::Integer(1)),
                    ("totalRooms", PyRep::Integer(2)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        _ => None,
    }
}
