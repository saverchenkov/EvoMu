//! `pve_dungeon_editor` — GM Dungeon & Level Editor persistence, room/object CRUD, and ballpark synchronization (`feat:admin.gm-dungeon-editor`, `rpc:dungeon.DEGetRooms`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::SolarSystemId;
use glam::DVec3;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

const DUNGEON_EDITOR_BLOB_KEY: &str = "dungeon_editor_store";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DungeonRoomObject {
    pub object_id: i64,
    pub room_id: i64,
    pub type_id: u32,
    pub quantity: i64,
    pub radius: f64,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub yaw: f64,
    pub pitch: f64,
    pub roll: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DungeonEditorRoom {
    pub room_id: i64,
    pub room_name: String,
    pub dungeon_id: i64,
    pub objects: Vec<DungeonRoomObject>,
    #[serde(default)]
    pub disallow_mwd: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GmActiveEditorState {
    pub dungeon_id: Option<i64>,
    pub room_id: Option<i64>,
    pub godmode: bool,
    pub selected_object_ids: Vec<i64>,
    pub spawned_ball_ids: Vec<i64>,
    pub solar_system_id: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DungeonEditorStore {
    pub rooms_by_dungeon: BTreeMap<i64, Vec<DungeonEditorRoom>>,
    #[serde(default)]
    pub dungeon_disallow_mwd: BTreeMap<i64, bool>,
    pub active_editors: BTreeMap<i64, GmActiveEditorState>,
    pub next_custom_room_id: i64,
    pub next_custom_object_id: i64,
}

impl Default for DungeonEditorStore {
    fn default() -> Self {
        Self {
            rooms_by_dungeon: BTreeMap::new(),
            dungeon_disallow_mwd: BTreeMap::new(),
            active_editors: BTreeMap::new(),
            next_custom_room_id: 9_000_001,
            next_custom_object_id: 1_965_000_001,
        }
    }
}

pub fn set_dungeon_disallow_mwd(ctx: &ServerContext, dungeon_id: i64, disallow: bool) {
    with_dungeon_editor_store(ctx, |store| {
        store.dungeon_disallow_mwd.insert(dungeon_id, disallow);
    });
}

pub fn set_room_disallow_mwd(ctx: &ServerContext, room_id: i64, disallow: bool) -> bool {
    with_dungeon_editor_store(ctx, |store| {
        for rooms in store.rooms_by_dungeon.values_mut() {
            if let Some(r) = rooms.iter_mut().find(|r| r.room_id == room_id) {
                r.disallow_mwd = disallow;
                return true;
            }
        }
        false
    })
}

pub fn is_dungeon_or_room_mwd_disallowed(
    ctx: &ServerContext,
    dungeon_id: i64,
    room_id_opt: Option<i64>,
) -> bool {
    with_dungeon_editor_store(ctx, |store| {
        if store.dungeon_disallow_mwd.get(&dungeon_id).copied().unwrap_or(false) {
            return true;
        }
        if let Some(rid) = room_id_opt {
            if let Some(rooms) = store.rooms_by_dungeon.get(&dungeon_id) {
                if rooms.iter().any(|r| r.room_id == rid && r.disallow_mwd) {
                    return true;
                }
            }
        }
        false
    })
}

/// Seeds default rooms strictly from SDE `dungeon_asteroids` and `dungeon_gas_types` tables.
/// Dungeons without SDE asteroid or gas room entries start with an empty room list until rooms
/// are added via `EditRoom`.
pub fn build_default_rooms_for_sde_dungeon(
    ctx: &ServerContext,
    d: &eve_sde::DungeonTemplate,
) -> Vec<DungeonEditorRoom> {
    let did = d.dungeon_id;
    if let Some(entries) = ctx.sde.dungeon_asteroids.get(&did).filter(|e| !e.is_empty()) {
        let mut by_room: BTreeMap<u32, Vec<&eve_sde::DungeonAsteroidEntry>> = BTreeMap::new();
        for e in entries {
            by_room.entry(e.room_id).or_default().push(e);
        }
        // DERIVED: Room names and deterministic grid offsets derived from SDE dungeon_asteroids entries.
        return by_room
            .into_iter()
            .enumerate()
            .map(|(r_idx, (room_id, room_entries))| {
                let objects = room_entries
                    .into_iter()
                    .enumerate()
                    .map(|(o_idx, entry)| DungeonRoomObject {
                        object_id: 1_960_000_000 + (room_id as i64) * 100 + (o_idx as i64) + 1,
                        room_id: room_id as i64,
                        type_id: entry.type_id,
                        quantity: entry.asteroid_count.max(1) as i64,
                        radius: 2_500.0,
                        x: (o_idx as f64) * 5_000.0,
                        y: 0.0,
                        z: (r_idx as f64) * 25_000.0,
                        yaw: 0.0,
                        pitch: 0.0,
                        roll: 0.0,
                    })
                    .collect();
                DungeonEditorRoom {
                    room_id: room_id as i64,
                    room_name: format!("{} - Room {}", d.name, r_idx + 1),
                    dungeon_id: did as i64,
                    objects,
                    disallow_mwd: false,
                }
            })
            .collect();
    }

    if let Some(gas_types) = ctx.sde.dungeon_gas_types.get(&did).filter(|g| !g.is_empty()) {
        // DERIVED: Single gas room and object offsets derived from SDE dungeon_gas_types entries.
        let room_id = (did as i64) * 100 + 1;
        let objects = gas_types
            .iter()
            .enumerate()
            .map(|(o_idx, &type_id)| DungeonRoomObject {
                object_id: 1_960_000_000 + room_id * 100 + (o_idx as i64) + 1,
                room_id,
                type_id,
                quantity: 1,
                radius: 2_000.0,
                x: (o_idx as f64) * 5_000.0,
                y: 0.0,
                z: 0.0,
                yaw: 0.0,
                pitch: 0.0,
                roll: 0.0,
            })
            .collect();
        return vec![DungeonEditorRoom {
            room_id,
            room_name: d.name.clone(),
            dungeon_id: did as i64,
            objects,
            disallow_mwd: false,
        }];
    }

    Vec::new()
}

pub fn with_dungeon_editor_store<R>(
    ctx: &ServerContext,
    f: impl FnOnce(&mut DungeonEditorStore) -> R,
) -> R {
    ctx.db.with_mut_snapshot(|snap| {
        let mut store: DungeonEditorStore = snap
            .service_state_blobs
            .get(DUNGEON_EDITOR_BLOB_KEY)
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        let out = f(&mut store);
        if let Ok(json) = serde_json::to_string(&store) {
            snap.service_state_blobs
                .insert(DUNGEON_EDITOR_BLOB_KEY.to_string(), json);
        }
        out
    })
}

pub fn ensure_dungeon_rooms_seeded(
    ctx: &ServerContext,
    d: &eve_sde::DungeonTemplate,
) -> Vec<DungeonEditorRoom> {
    let default_rooms = build_default_rooms_for_sde_dungeon(ctx, d);
    with_dungeon_editor_store(ctx, |store| {
        store
            .rooms_by_dungeon
            .entry(d.dungeon_id as i64)
            .or_insert(default_rooms)
            .clone()
    })
}

fn ensure_all_mentioned_dungeon_seeded(ctx: &ServerContext, dungeon_id: i64) -> Option<Vec<DungeonEditorRoom>> {
    let did = u32::try_from(dungeon_id).ok()?;
    let d = ctx.sde.dungeons.get(&did)?;
    Some(ensure_dungeon_rooms_seeded(ctx, d))
}

fn find_room_dungeon_id(ctx: &ServerContext, room_id: i64) -> Option<i64> {
    let from_store = with_dungeon_editor_store(ctx, |store| {
        store
            .rooms_by_dungeon
            .iter()
            .find_map(|(&did, rooms)| rooms.iter().any(|r| r.room_id == room_id).then_some(did))
    });
    if from_store.is_some() {
        return from_store;
    }
    for (&did, entries) in &ctx.sde.dungeon_asteroids {
        if entries.iter().any(|e| e.room_id as i64 == room_id) {
            if let Some(d) = ctx.sde.dungeons.get(&did) {
                let _ = ensure_dungeon_rooms_seeded(ctx, d);
                return Some(did as i64);
            }
        }
    }
    if room_id > 100 {
        let candidate_did = (room_id / 100) as u32;
        if let Some(d) = ctx.sde.dungeons.get(&candidate_did) {
            let rooms = ensure_dungeon_rooms_seeded(ctx, d);
            if rooms.iter().any(|r| r.room_id == room_id) {
                return Some(candidate_did as i64);
            }
        }
    }
    None
}

fn room_object_to_pyrep(ctx: &ServerContext, dungeon_id: i64, obj: &DungeonRoomObject) -> PyRep {
    let (tname, gid) = ctx
        .sde
        .types
        .get(&obj.type_id)
        .map(|t| (t.name.clone(), t.group_id as i64))
        .unwrap_or_else(|| (format!("Type {}", obj.type_id), 0));
    PyRep::keyval([
        ("objectID", PyRep::Integer(obj.object_id)),
        ("roomID", PyRep::Integer(obj.room_id)),
        ("dungeonID", PyRep::Integer(dungeon_id)),
        ("typeID", PyRep::Integer(obj.type_id as i64)),
        ("typeName", PyRep::String(tname)),
        ("groupID", PyRep::Integer(gid)),
        ("x", PyRep::Real(obj.x)),
        ("y", PyRep::Real(obj.y)),
        ("z", PyRep::Real(obj.z)),
        ("yaw", PyRep::Real(obj.yaw)),
        ("pitch", PyRep::Real(obj.pitch)),
        ("roll", PyRep::Real(obj.roll)),
        ("radius", PyRep::Real(obj.radius)),
        ("quantity", PyRep::Integer(obj.quantity)),
    ])
}

async fn sync_editor_room_balls(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    old_balls: &[i64],
    room: &DungeonEditorRoom,
) -> Vec<i64> {
    let handle = ctx.systems.get_or_spawn(sys_id);
    for &bid in old_balls {
        handle.try_remove_ball(bid);
    }
    let mut spawned = Vec::with_capacity(room.objects.len());
    for obj in &room.objects {
        let mut ball = crate::services::pve_special_sites::make_site_npc_ball(
            obj.object_id,
            obj.type_id,
            DVec3::new(obj.x, obj.y, obj.z),
        );
        ball.radius = obj.radius.max(50.0);
        let _ = handle.spawn_ball(ball).await;
        spawned.push(obj.object_id);
    }
    spawned
}

fn kw_or_arg_i64(args: &[PyRep], kwargs: &HashMap<String, PyRep>, idx: usize, keys: &[&str]) -> Option<i64> {
    for k in keys {
        if let Some(v) = kwargs.get(*k).and_then(|v| v.as_i64()) {
            return Some(v);
        }
    }
    args.get(idx).and_then(|a| a.as_i64())
}

fn kw_or_arg_f64(args: &[PyRep], kwargs: &HashMap<String, PyRep>, idx: usize, keys: &[&str]) -> Option<f64> {
    for k in keys {
        if let Some(v) = kwargs.get(*k).and_then(|v| v.as_f64()) {
            return Some(v);
        }
    }
    args.get(idx).and_then(|a| a.as_f64())
}

fn kw_or_arg_str(args: &[PyRep], kwargs: &HashMap<String, PyRep>, idx: usize, keys: &[&str]) -> Option<String> {
    for k in keys {
        if let Some(s) = kwargs.get(*k).and_then(|v| v.as_str()) {
            return Some(s.to_string());
        }
    }
    args.get(idx).and_then(|a| a.as_str()).map(|s| s.to_string())
}

pub async fn handle_gm_dungeon_editor_op(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    if !session.has_gm_role() {
        return Err(BridgeRpcError {
            code: "InsufficientRole".into(),
            message: format!("{method} requires GM/Content role"),
        });
    }
    let sys_id_opt = session.solar_system_id;

    match method {
        "Bind" => {
            let st = with_dungeon_editor_store(ctx, |store| {
                let entry = store.active_editors.entry(char_id).or_default();
                if let Some(sys_id) = sys_id_opt {
                    entry.solar_system_id = Some(sys_id);
                }
                entry.clone()
            });
            Ok(PyRep::keyval([
                (
                    "solarSystemID",
                    st.solar_system_id
                        .or(sys_id_opt)
                        .map(|s| PyRep::Integer(s as i64))
                        .unwrap_or(PyRep::None),
                ),
                ("dungeonID", st.dungeon_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
                ("roomID", st.room_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
            ]))
        }
        "GetCurrentlyEditedRoomID" | "GetCurrentRoomID" | "GetEditingRoomID" => {
            let room_id = with_dungeon_editor_store(ctx, |store| {
                store.active_editors.get(&char_id).and_then(|e| e.room_id)
            });
            Ok(room_id.map(PyRep::Integer).unwrap_or(PyRep::None))
        }
        "GetEditingDungeonID" => {
            let dungeon_id = with_dungeon_editor_store(ctx, |store| {
                store.active_editors.get(&char_id).and_then(|e| e.dungeon_id)
            });
            Ok(dungeon_id.map(PyRep::Integer).unwrap_or(PyRep::None))
        }
        "EditDungeon" | "PlayDungeon" => {
            let sys_id = sys_id_opt.ok_or_else(|| BridgeRpcError::user_error("MustBeInSpace"))?;
            let dungeon_id = kw_or_arg_i64(args, kwargs, 0, &["dungeonID", "dungeonVID"])
                .filter(|&id| id > 0)
                .ok_or_else(|| BridgeRpcError::user_error("DungeonIDRequired"))?;
            let rooms = ensure_all_mentioned_dungeon_seeded(ctx, dungeon_id)
                .ok_or_else(|| BridgeRpcError::user_error("DungeonNotFound"))?;
            let requested_room_id = kw_or_arg_i64(args, kwargs, 1, &["roomID"]);
            let room = if let Some(rid) = requested_room_id {
                rooms
                    .into_iter()
                    .find(|r| r.room_id == rid)
                    .ok_or_else(|| BridgeRpcError::user_error("RoomNotFound"))?
            } else {
                rooms
                    .into_iter()
                    .next()
                    .ok_or_else(|| BridgeRpcError::user_error("RoomNotFound"))?
            };
            let godmode = kwargs
                .get("godmode")
                .and_then(|v| v.as_bool().or_else(|| v.as_i64().map(|n| n != 0)))
                .unwrap_or(method == "EditDungeon");
            let old_balls = with_dungeon_editor_store(ctx, |store| {
                store
                    .active_editors
                    .get(&char_id)
                    .map(|e| e.spawned_ball_ids.clone())
                    .unwrap_or_default()
            });
            let spawned = sync_editor_room_balls(ctx, sys_id, &old_balls, &room).await;
            if let Some(ship_id) = session.ship_id {
                let first_z = room.objects.first().map(|o| o.z).unwrap_or(0.0);
                let _ = ctx
                    .systems
                    .get_or_spawn(sys_id)
                    .set_ball_position(ship_id, DVec3::new(0.0, 0.0, first_z - 2_500.0))
                    .await;
            }
            with_dungeon_editor_store(ctx, |store| {
                store.active_editors.insert(
                    char_id,
                    GmActiveEditorState {
                        dungeon_id: Some(dungeon_id),
                        room_id: Some(room.room_id),
                        godmode,
                        selected_object_ids: Vec::new(),
                        spawned_ball_ids: spawned.clone(),
                        solar_system_id: Some(sys_id),
                    },
                );
            });
            Ok(PyRep::keyval([
                ("dungeonID", PyRep::Integer(dungeon_id)),
                ("roomID", PyRep::Integer(room.room_id)),
                ("roomName", PyRep::String(room.room_name)),
                ("godmode", PyRep::Bool(godmode)),
                ("spawnedObjectIDs", PyRep::List(spawned.into_iter().map(PyRep::Integer).collect())),
            ]))
        }
        "GotoRoom" => {
            let sys_id = sys_id_opt.ok_or_else(|| BridgeRpcError::user_error("MustBeInSpace"))?;
            let room_id = kw_or_arg_i64(args, kwargs, 0, &["roomID"])
                .ok_or_else(|| BridgeRpcError::user_error("RoomIDRequired"))?;
            let dungeon_id = find_room_dungeon_id(ctx, room_id)
                .ok_or_else(|| BridgeRpcError::user_error("RoomNotFound"))?;
            let (room, old_balls) = with_dungeon_editor_store(ctx, |store| {
                let r = store
                    .rooms_by_dungeon
                    .get(&dungeon_id)
                    .and_then(|rs| rs.iter().find(|r| r.room_id == room_id))
                    .cloned();
                let old = store
                    .active_editors
                    .get(&char_id)
                    .map(|e| e.spawned_ball_ids.clone())
                    .unwrap_or_default();
                (r, old)
            });
            let room = room.ok_or_else(|| BridgeRpcError::user_error("RoomNotFound"))?;
            let spawned = sync_editor_room_balls(ctx, sys_id, &old_balls, &room).await;
            with_dungeon_editor_store(ctx, |store| {
                let entry = store.active_editors.entry(char_id).or_default();
                entry.dungeon_id = Some(dungeon_id);
                entry.room_id = Some(room_id);
                entry.spawned_ball_ids = spawned.clone();
                entry.solar_system_id = Some(sys_id);
            });
            Ok(PyRep::keyval([
                ("dungeonID", PyRep::Integer(dungeon_id)),
                ("roomID", PyRep::Integer(room_id)),
                ("roomName", PyRep::String(room.room_name)),
                ("spawnedObjectIDs", PyRep::List(spawned.into_iter().map(PyRep::Integer).collect())),
            ]))
        }
        "ObjectSelection" => {
            let mut ids = Vec::new();
            if let Some(PyRep::List(items) | PyRep::Tuple(items)) = kwargs.get("objects").or_else(|| args.first()) {
                for item in items {
                    if let Some(id) = item.as_i64() {
                        ids.push(id);
                    }
                }
            } else if let Some(id) = args.first().and_then(|a| a.as_i64()) {
                ids.push(id);
            }
            with_dungeon_editor_store(ctx, |store| {
                store.active_editors.entry(char_id).or_default().selected_object_ids = ids.clone();
            });
            Ok(PyRep::List(ids.into_iter().map(PyRep::Integer).collect()))
        }
        "Reset" | "ResetDungeon" => {
            let arg_did = kw_or_arg_i64(args, kwargs, 0, &["dungeonID"]);
            let old = with_dungeon_editor_store(ctx, |store| {
                let old_ed = store.active_editors.remove(&char_id);
                if let Some(did) = arg_did.or_else(|| old_ed.as_ref().and_then(|e| e.dungeon_id)) {
                    store.rooms_by_dungeon.remove(&did);
                }
                old_ed
            });
            if let Some(st) = old {
                if let Some(sys_id) = st.solar_system_id.or(sys_id_opt) {
                    let handle = ctx.systems.get_or_spawn(sys_id);
                    for bid in st.spawned_ball_ids {
                        handle.try_remove_ball(bid);
                    }
                }
            }
            Ok(PyRep::Bool(true))
        }
        "SaveAllChanges" => Ok(PyRep::Bool(true)),
        "IsObjectLocked" => Ok(PyRep::Tuple(vec![PyRep::Bool(false), PyRep::String(String::new())])),
        "EditRoom" => {
            let dungeon_id = kw_or_arg_i64(args, kwargs, 0, &["dungeonID"])
                .ok_or_else(|| BridgeRpcError::user_error("DungeonIDRequired"))?;
            let _ = ensure_all_mentioned_dungeon_seeded(ctx, dungeon_id)
                .ok_or_else(|| BridgeRpcError::user_error("DungeonNotFound"))?;
            let room_name = kw_or_arg_str(args, kwargs, 1, &["roomName", "name"])
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "New Room".to_string());
            let disallow_mwd = kwargs
                .get("disallowMWD")
                .and_then(|v| v.as_bool().or_else(|| v.as_i64().map(|n| n != 0)))
                .unwrap_or(false);
            let new_room = with_dungeon_editor_store(ctx, |store| {
                let rid = store.next_custom_room_id;
                store.next_custom_room_id += 1;
                let room = DungeonEditorRoom {
                    room_id: rid,
                    room_name: room_name.clone(),
                    dungeon_id,
                    objects: Vec::new(),
                    disallow_mwd,
                };
                store.rooms_by_dungeon.entry(dungeon_id).or_default().push(room.clone());
                room
            });
            Ok(PyRep::keyval([
                ("roomID", PyRep::Integer(new_room.room_id)),
                ("roomName", PyRep::String(new_room.room_name)),
                ("dungeonID", PyRep::Integer(new_room.dungeon_id)),
                ("disallowMWD", PyRep::Bool(new_room.disallow_mwd)),
            ]))
        }
        "SaveRoom" => {
            let room_id = kw_or_arg_i64(args, kwargs, 0, &["roomID"])
                .ok_or_else(|| BridgeRpcError::user_error("RoomIDRequired"))?;
            let new_name = kw_or_arg_str(args, kwargs, 1, &["roomName", "newName", "name"])
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| BridgeRpcError::user_error("RoomNameRequired"))?;
            let disallow_mwd_opt = kwargs
                .get("disallowMWD")
                .and_then(|v| v.as_bool().or_else(|| v.as_i64().map(|n| n != 0)));
            let dungeon_id = find_room_dungeon_id(ctx, room_id)
                .ok_or_else(|| BridgeRpcError::user_error("RoomNotFound"))?;
            let updated = with_dungeon_editor_store(ctx, |store| {
                let rooms = store.rooms_by_dungeon.get_mut(&dungeon_id)?;
                let r = rooms.iter_mut().find(|r| r.room_id == room_id)?;
                r.room_name = new_name.clone();
                if let Some(flag) = disallow_mwd_opt {
                    r.disallow_mwd = flag;
                }
                Some(r.clone())
            })
            .ok_or_else(|| BridgeRpcError::user_error("RoomNotFound"))?;
            Ok(PyRep::keyval([
                ("roomID", PyRep::Integer(updated.room_id)),
                ("roomName", PyRep::String(updated.room_name)),
                ("dungeonID", PyRep::Integer(updated.dungeon_id)),
                ("disallowMWD", PyRep::Bool(updated.disallow_mwd)),
            ]))
        }
        "DeleteGroup" => {
            let room_id = kw_or_arg_i64(args, kwargs, 0, &["roomID", "groupID"])
                .ok_or_else(|| BridgeRpcError::user_error("RoomIDRequired"))?;
            let dungeon_id = find_room_dungeon_id(ctx, room_id)
                .ok_or_else(|| BridgeRpcError::user_error("RoomNotFound"))?;
            let (removed_objs, active_sys) = with_dungeon_editor_store(ctx, |store| {
                let rooms = store.rooms_by_dungeon.get_mut(&dungeon_id)?;
                let idx = rooms.iter().position(|r| r.room_id == room_id)?;
                let objs = rooms.remove(idx).objects;
                let active_sys = store
                    .active_editors
                    .get(&char_id)
                    .filter(|ed| ed.room_id == Some(room_id))
                    .and_then(|ed| ed.solar_system_id.or(sys_id_opt));
                Some((objs, active_sys))
            })
            .ok_or_else(|| BridgeRpcError::user_error("RoomNotFound"))?;
            if let Some(sys_id) = active_sys {
                let handle = ctx.systems.get_or_spawn(sys_id);
                for obj in removed_objs {
                    handle.try_remove_ball(obj.object_id);
                }
            }
            Ok(PyRep::Bool(true))
        }
        "GetDunObjects" => {
            let room_id = kw_or_arg_i64(args, kwargs, 0, &["roomID"])
                .or_else(|| {
                    with_dungeon_editor_store(ctx, |store| {
                        store.active_editors.get(&char_id).and_then(|e| e.room_id)
                    })
                })
                .ok_or_else(|| BridgeRpcError::user_error("RoomIDRequired"))?;
            let dungeon_id = find_room_dungeon_id(ctx, room_id)
                .ok_or_else(|| BridgeRpcError::user_error("RoomNotFound"))?;
            let objs = with_dungeon_editor_store(ctx, |store| {
                store
                    .rooms_by_dungeon
                    .get(&dungeon_id)
                    .and_then(|rs| rs.iter().find(|r| r.room_id == room_id))
                    .map(|r| r.objects.clone())
            })
            .ok_or_else(|| BridgeRpcError::user_error("RoomNotFound"))?;
            let list = objs
                .iter()
                .map(|o| room_object_to_pyrep(ctx, dungeon_id, o))
                .collect();
            Ok(PyRep::List(list))
        }
        "DropIntoDungeonRoom" => {
            let room_id = kw_or_arg_i64(args, kwargs, 0, &["roomID"])
                .or_else(|| {
                    with_dungeon_editor_store(ctx, |store| {
                        store.active_editors.get(&char_id).and_then(|e| e.room_id)
                    })
                })
                .ok_or_else(|| BridgeRpcError::user_error("RoomIDRequired"))?;
            let type_id = kw_or_arg_i64(args, kwargs, 1, &["typeID"])
                .and_then(|v| u32::try_from(v).ok())
                .filter(|tid| ctx.sde.types.contains_key(tid))
                .ok_or_else(|| BridgeRpcError::user_error("InvalidTypeID"))?;
            let x = kw_or_arg_f64(args, kwargs, 2, &["x"]).unwrap_or(0.0);
            let y = kw_or_arg_f64(args, kwargs, 3, &["y"]).unwrap_or(0.0);
            let z = kw_or_arg_f64(args, kwargs, 4, &["z"]).unwrap_or(0.0);
            let yaw = kw_or_arg_f64(args, kwargs, 5, &["yaw"]).unwrap_or(0.0);
            let pitch = kw_or_arg_f64(args, kwargs, 6, &["pitch"]).unwrap_or(0.0);
            let roll = kw_or_arg_f64(args, kwargs, 7, &["roll"]).unwrap_or(0.0);
            let radius = kw_or_arg_f64(args, kwargs, 8, &["radius"])
                .filter(|&r| r > 0.0)
                .unwrap_or(1_500.0);
            let quantity = kw_or_arg_i64(args, kwargs, 9, &["quantity"])
                .filter(|&q| q > 0)
                .unwrap_or(1);
            let dungeon_id = find_room_dungeon_id(ctx, room_id)
                .ok_or_else(|| BridgeRpcError::user_error("RoomNotFound"))?;
            let (new_obj, active_sys) = with_dungeon_editor_store(ctx, |store| {
                let oid = store.next_custom_object_id;
                store.next_custom_object_id += 1;
                let obj = DungeonRoomObject {
                    object_id: oid,
                    room_id,
                    type_id,
                    quantity,
                    radius,
                    x,
                    y,
                    z,
                    yaw,
                    pitch,
                    roll,
                };
                let rooms = store.rooms_by_dungeon.get_mut(&dungeon_id)?;
                let r = rooms.iter_mut().find(|r| r.room_id == room_id)?;
                r.objects.push(obj.clone());
                let mut active_sys = None;
                if let Some(ed) = store.active_editors.get_mut(&char_id) {
                    if ed.room_id == Some(room_id) {
                        ed.spawned_ball_ids.push(oid);
                        active_sys = ed.solar_system_id.or(sys_id_opt);
                    }
                }
                Some((obj, active_sys))
            })
            .ok_or_else(|| BridgeRpcError::user_error("RoomNotFound"))?;

            if let Some(sys_id) = active_sys {
                let handle = ctx.systems.get_or_spawn(sys_id);
                let mut ball = crate::services::pve_special_sites::make_site_npc_ball(
                    new_obj.object_id,
                    new_obj.type_id,
                    DVec3::new(new_obj.x, new_obj.y, new_obj.z),
                );
                ball.radius = new_obj.radius.max(50.0);
                let _ = handle.spawn_ball(ball).await;
            }
            Ok(room_object_to_pyrep(ctx, dungeon_id, &new_obj))
        }
        "SetObjectPosition" => {
            let object_id = kw_or_arg_i64(args, kwargs, 0, &["objectID"])
                .ok_or_else(|| BridgeRpcError::user_error("ObjectIDRequired"))?;
            let x = kw_or_arg_f64(args, kwargs, 1, &["x"])
                .ok_or_else(|| BridgeRpcError::user_error("CoordinatesRequired"))?;
            let y = kw_or_arg_f64(args, kwargs, 2, &["y"])
                .ok_or_else(|| BridgeRpcError::user_error("CoordinatesRequired"))?;
            let z = kw_or_arg_f64(args, kwargs, 3, &["z"])
                .ok_or_else(|| BridgeRpcError::user_error("CoordinatesRequired"))?;
            let (did, updated, active_sys) = with_dungeon_editor_store(ctx, |store| {
                let active_ed = store.active_editors.get(&char_id).cloned();
                for (&did, rooms) in store.rooms_by_dungeon.iter_mut() {
                    for r in rooms.iter_mut() {
                        if let Some(obj) = r.objects.iter_mut().find(|o| o.object_id == object_id) {
                            obj.x = x;
                            obj.y = y;
                            obj.z = z;
                            let active_sys = active_ed
                                .filter(|ed| ed.room_id == Some(r.room_id))
                                .and_then(|ed| ed.solar_system_id.or(sys_id_opt));
                            return Some((did, obj.clone(), active_sys));
                        }
                    }
                }
                None
            })
            .ok_or_else(|| BridgeRpcError::user_error("ObjectNotFound"))?;
            if let Some(sys_id) = active_sys {
                let _ = ctx
                    .systems
                    .get_or_spawn(sys_id)
                    .set_ball_position(object_id, DVec3::new(x, y, z))
                    .await;
            }
            Ok(room_object_to_pyrep(ctx, did, &updated))
        }
        "GetObjectPosition" => {
            let object_id = kw_or_arg_i64(args, kwargs, 0, &["objectID"])
                .ok_or_else(|| BridgeRpcError::user_error("ObjectIDRequired"))?;
            let pos = with_dungeon_editor_store(ctx, |store| {
                for rooms in store.rooms_by_dungeon.values() {
                    for r in rooms {
                        if let Some(obj) = r.objects.iter().find(|o| o.object_id == object_id) {
                            return Some((obj.x, obj.y, obj.z));
                        }
                    }
                }
                None
            })
            .ok_or_else(|| BridgeRpcError::user_error("ObjectNotFound"))?;
            Ok(PyRep::Tuple(vec![PyRep::Real(pos.0), PyRep::Real(pos.1), PyRep::Real(pos.2)]))
        }
        "SetObjectRotation" => {
            let object_id = kw_or_arg_i64(args, kwargs, 0, &["objectID"])
                .ok_or_else(|| BridgeRpcError::user_error("ObjectIDRequired"))?;
            let yaw = kw_or_arg_f64(args, kwargs, 1, &["yaw"]).unwrap_or(0.0);
            let pitch = kw_or_arg_f64(args, kwargs, 2, &["pitch"]).unwrap_or(0.0);
            let roll = kw_or_arg_f64(args, kwargs, 3, &["roll"]).unwrap_or(0.0);
            let (did, updated) = with_dungeon_editor_store(ctx, |store| {
                for (&did, rooms) in store.rooms_by_dungeon.iter_mut() {
                    for r in rooms.iter_mut() {
                        if let Some(obj) = r.objects.iter_mut().find(|o| o.object_id == object_id) {
                            obj.yaw = yaw;
                            obj.pitch = pitch;
                            obj.roll = roll;
                            return Some((did, obj.clone()));
                        }
                    }
                }
                None
            })
            .ok_or_else(|| BridgeRpcError::user_error("ObjectNotFound"))?;
            Ok(room_object_to_pyrep(ctx, did, &updated))
        }
        "GetObjectRotation" => {
            let object_id = kw_or_arg_i64(args, kwargs, 0, &["objectID"])
                .ok_or_else(|| BridgeRpcError::user_error("ObjectIDRequired"))?;
            let rot = with_dungeon_editor_store(ctx, |store| {
                for rooms in store.rooms_by_dungeon.values() {
                    for r in rooms {
                        if let Some(obj) = r.objects.iter().find(|o| o.object_id == object_id) {
                            return Some((obj.yaw, obj.pitch, obj.roll));
                        }
                    }
                }
                None
            })
            .ok_or_else(|| BridgeRpcError::user_error("ObjectNotFound"))?;
            Ok(PyRep::Tuple(vec![PyRep::Real(rot.0), PyRep::Real(rot.1), PyRep::Real(rot.2)]))
        }
        "SetObjectRadius" => {
            let object_id = kw_or_arg_i64(args, kwargs, 0, &["objectID"])
                .ok_or_else(|| BridgeRpcError::user_error("ObjectIDRequired"))?;
            let radius = kw_or_arg_f64(args, kwargs, 1, &["radius"])
                .filter(|&r| r > 0.0)
                .ok_or_else(|| BridgeRpcError::user_error("InvalidRadius"))?;
            let (did, updated, active_sys) = with_dungeon_editor_store(ctx, |store| {
                let active_ed = store.active_editors.get(&char_id).cloned();
                for (&did, rooms) in store.rooms_by_dungeon.iter_mut() {
                    for r in rooms.iter_mut() {
                        if let Some(obj) = r.objects.iter_mut().find(|o| o.object_id == object_id) {
                            obj.radius = radius;
                            let active_sys = active_ed
                                .filter(|ed| ed.room_id == Some(r.room_id))
                                .and_then(|ed| ed.solar_system_id.or(sys_id_opt));
                            return Some((did, obj.clone(), active_sys));
                        }
                    }
                }
                None
            })
            .ok_or_else(|| BridgeRpcError::user_error("ObjectNotFound"))?;
            if let Some(sys_id) = active_sys {
                let handle = ctx.systems.get_or_spawn(sys_id);
                handle.try_remove_ball(object_id);
                let mut ball = crate::services::pve_special_sites::make_site_npc_ball(
                    updated.object_id,
                    updated.type_id,
                    DVec3::new(updated.x, updated.y, updated.z),
                );
                ball.radius = updated.radius.max(50.0);
                let _ = handle.spawn_ball(ball).await;
            }
            Ok(room_object_to_pyrep(ctx, did, &updated))
        }
        "GetObjectRadius" => {
            let object_id = kw_or_arg_i64(args, kwargs, 0, &["objectID"])
                .ok_or_else(|| BridgeRpcError::user_error("ObjectIDRequired"))?;
            let radius = with_dungeon_editor_store(ctx, |store| {
                for rooms in store.rooms_by_dungeon.values() {
                    for r in rooms {
                        if let Some(obj) = r.objects.iter().find(|o| o.object_id == object_id) {
                            return Some(obj.radius);
                        }
                    }
                }
                None
            })
            .ok_or_else(|| BridgeRpcError::user_error("ObjectNotFound"))?;
            Ok(PyRep::Real(radius))
        }
        "SetObjectQuantity" => {
            let object_id = kw_or_arg_i64(args, kwargs, 0, &["objectID"])
                .ok_or_else(|| BridgeRpcError::user_error("ObjectIDRequired"))?;
            let quantity = kw_or_arg_i64(args, kwargs, 1, &["quantity", "value"])
                .filter(|&q| q > 0)
                .unwrap_or(1);
            let (did, updated) = with_dungeon_editor_store(ctx, |store| {
                for (&did, rooms) in store.rooms_by_dungeon.iter_mut() {
                    for r in rooms.iter_mut() {
                        if let Some(obj) = r.objects.iter_mut().find(|o| o.object_id == object_id) {
                            obj.quantity = quantity;
                            return Some((did, obj.clone()));
                        }
                    }
                }
                None
            })
            .ok_or_else(|| BridgeRpcError::user_error("ObjectNotFound"))?;
            Ok(room_object_to_pyrep(ctx, did, &updated))
        }
        "GetObjectQuantity" => {
            let object_id = kw_or_arg_i64(args, kwargs, 0, &["objectID"])
                .ok_or_else(|| BridgeRpcError::user_error("ObjectIDRequired"))?;
            let qty = with_dungeon_editor_store(ctx, |store| {
                for rooms in store.rooms_by_dungeon.values() {
                    for r in rooms {
                        if let Some(obj) = r.objects.iter().find(|o| o.object_id == object_id) {
                            return Some(obj.quantity);
                        }
                    }
                }
                None
            })
            .ok_or_else(|| BridgeRpcError::user_error("ObjectNotFound"))?;
            Ok(PyRep::Integer(qty))
        }
        "DeleteSelected" | "DeleteGroupObjects" => {
            let mut explicit_ids = Vec::new();
            if let Some(PyRep::List(items) | PyRep::Tuple(items)) = kwargs.get("objectIDs").or_else(|| args.first()) {
                for it in items {
                    if let Some(id) = it.as_i64() {
                        explicit_ids.push(id);
                    }
                }
            } else if let Some(id) = kw_or_arg_i64(args, kwargs, 0, &["objectID"]) {
                explicit_ids.push(id);
            }
            let (removed_ids, active_sys) = with_dungeon_editor_store(ctx, |store| {
                let target_ids = if !explicit_ids.is_empty() {
                    explicit_ids
                } else {
                    store
                        .active_editors
                        .get(&char_id)
                        .map(|ed| ed.selected_object_ids.clone())
                        .unwrap_or_default()
                };
                if target_ids.is_empty() {
                    return (Vec::new(), None);
                }
                let mut removed = Vec::new();
                for rooms in store.rooms_by_dungeon.values_mut() {
                    for r in rooms.iter_mut() {
                        r.objects.retain(|o| {
                            if target_ids.contains(&o.object_id) {
                                removed.push(o.object_id);
                                false
                            } else {
                                true
                            }
                        });
                    }
                }
                let mut active_sys = None;
                if let Some(ed) = store.active_editors.get_mut(&char_id) {
                    ed.spawned_ball_ids.retain(|id| !removed.contains(id));
                    ed.selected_object_ids.retain(|id| !removed.contains(id));
                    active_sys = ed.solar_system_id.or(sys_id_opt);
                }
                (removed, active_sys)
            });
            if removed_ids.is_empty() {
                return Err(BridgeRpcError::user_error("ObjectNotFound"));
            }
            if let Some(sys_id) = active_sys {
                let handle = ctx.systems.get_or_spawn(sys_id);
                for oid in removed_ids {
                    handle.try_remove_ball(oid);
                }
            }
            Ok(PyRep::Bool(true))
        }
        _ => Err(BridgeRpcError::user_error("UnimplementedRPC")),
    }
}
