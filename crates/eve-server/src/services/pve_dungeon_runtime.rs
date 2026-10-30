//! `pve_dungeon_runtime` — Multi-room deadspace dungeons, acceleration gate pocket traversal,
//! data-driven MWD restrictions, and room object/NPC/trigger lifecycle (`feat:pve.deadspace-pocket-rules`, `feat:pve.dungeon-system`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::notify_emitters_batch3_space::{
    emit_dungeon_progression_notifications, emit_exiting_dungeon_on_warp_if_present,
};
use crate::services::pve_exploration::with_pve_exploration_reg;
use crate::services::pve_exploration_seeding::build_type_combat_state_from_sde;
use crate::services::pve_special_sites::make_site_npc_ball;
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::SolarSystemId;
use glam::DVec3;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

const DUNGEON_RUNTIME_BLOB_KEY: &str = "dungeon_runtime_store";
const POCKET_RADIUS_M: f64 = 200_000.0;
pub(crate) const GATE_ACTIVATION_RANGE_M: f64 = 2_500.0;
const ACCELERATION_GATE_TYPE_ID: u32 = 17831;
pub(crate) const DEFAULT_CONTAINER_TYPE_ID: u32 = 23;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum DungeonTriggerKind {
    OnKill {
        target_npc_group: Option<u8>,
        target_ball_id: Option<i64>,
    },
    OnApproach {
        target_ball_id: Option<i64>,
        target_object_index: Option<usize>,
        anchor_pos: [f64; 3],
        radius_m: f64,
    },
    OnLoot {
        container_ball_id: Option<i64>,
        target_object_index: Option<usize>,
        item_type_id: Option<u32>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum DungeonTriggerAction {
    UnlockGate,
    SpawnNpcGroup(u8),
    CompleteDungeon,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DungeonTrigger {
    pub trigger_id: u32,
    pub kind: DungeonTriggerKind,
    pub action: DungeonTriggerAction,
    #[serde(default)]
    pub fired: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DungeonRuntimeObject {
    #[serde(default)]
    pub ball_id: i64,
    pub type_id: u32,
    pub name: String,
    pub position: [f64; 3],
    pub radius: f64,
    #[serde(default)]
    pub is_container: bool,
    #[serde(default)]
    pub loot_items: Vec<(u32, i32)>,
    #[serde(default)]
    pub spawned: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DungeonNpcGroup {
    pub group_id: u8,
    pub wave: u8,
    pub npc_specs: Vec<(u32, usize)>,
    #[serde(default)]
    pub npc_ball_ids: Vec<i64>,
    #[serde(default)]
    pub spawn_immediately: bool,
    #[serde(default)]
    pub spawned: bool,
    #[serde(default)]
    pub cleared: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DungeonRuntimeRoom {
    pub room_index: usize,
    pub room_id: i64,
    pub room_name: String,
    pub position: [f64; 3],
    pub gate_destination_pos: [f64; 3],
    pub gated: bool,
    #[serde(default)]
    pub disallow_mwd: Option<bool>,
    #[serde(default)]
    pub gate_ball_id: Option<i64>,
    #[serde(default)]
    pub gate_locked: bool,
    #[serde(default)]
    pub objects: Vec<DungeonRuntimeObject>,
    #[serde(default)]
    pub npc_groups: Vec<DungeonNpcGroup>,
    #[serde(default)]
    pub triggers: Vec<DungeonTrigger>,
    #[serde(default)]
    pub spawned: bool,
    #[serde(default)]
    pub cleared: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DungeonRuntimeInstance {
    pub instance_id: i64,
    pub dungeon_id: u32,
    pub dungeon_name: String,
    pub category: String,
    pub archetype: String,
    pub solar_system_id: SolarSystemId,
    pub owner_char_id: i64,
    pub base_position: [f64; 3],
    #[serde(default)]
    pub disallow_mwd: bool,
    #[serde(default)]
    pub entry_gate_ball_id: Option<i64>,
    #[serde(default)]
    pub entry_gate_pos: Option<[f64; 3]>,
    #[serde(default)]
    pub entry_gate_locked: bool,
    #[serde(default)]
    pub current_room_index: usize,
    #[serde(default)]
    pub rooms: Vec<DungeonRuntimeRoom>,
    #[serde(default)]
    pub completed: bool,
    #[serde(default)]
    pub despawned: bool,
    pub created_unix: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DungeonRuntimeStore {
    #[serde(default)]
    pub instances: BTreeMap<i64, DungeonRuntimeInstance>,
    #[serde(default = "default_next_instance_id")]
    pub next_instance_id: i64,
}

fn default_next_instance_id() -> i64 {
    950_001
}

impl Default for DungeonRuntimeStore {
    fn default() -> Self {
        Self {
            instances: BTreeMap::new(),
            next_instance_id: default_next_instance_id(),
        }
    }
}

pub(crate) fn has_dungeon_runtime_store_db(db: &eve_db::WorldRepository) -> bool {
    db.with_snapshot(|snap| snap.service_state_blobs.contains_key(DUNGEON_RUNTIME_BLOB_KEY))
}

pub(crate) fn has_dungeon_runtime_store(ctx: &ServerContext) -> bool {
    has_dungeon_runtime_store_db(&ctx.db)
}

pub fn with_dungeon_runtime_store_db<R>(
    db: &eve_db::WorldRepository,
    f: impl FnOnce(&mut DungeonRuntimeStore) -> R,
) -> R {
    db.with_mut_snapshot(|snap| {
        let orig_opt = snap.service_state_blobs.get(DUNGEON_RUNTIME_BLOB_KEY).cloned();
        let mut store: DungeonRuntimeStore = orig_opt
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        let out = f(&mut store);
        if (!store.instances.is_empty() || orig_opt.is_some())
            && serde_json::to_string(&store)
                .is_ok_and(|json| orig_opt.as_deref() != Some(json.as_str()) && {
                    snap.service_state_blobs
                        .insert(DUNGEON_RUNTIME_BLOB_KEY.to_string(), json);
                    true
                })
        {}
        out
    })
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

fn is_pocket_mwd_disallowed(
    ctx: &ServerContext,
    inst: &DungeonRuntimeInstance,
    room: &DungeonRuntimeRoom,
) -> bool {
    if let Some(flag) = room.disallow_mwd {
        return flag;
    }
    if inst.disallow_mwd {
        return true;
    }
    if crate::services::pve_dungeon_editor::is_dungeon_or_room_mwd_disallowed(
        ctx,
        inst.dungeon_id as i64,
        Some(room.room_id),
    ) {
        return true;
    }
    ctx.sde
        .dungeons
        .get(&inst.dungeon_id)
        .is_some_and(|d| d.archetype_id == Some(51))
}

/// Checks whether activating a MicroWarpdrive is permitted at `ship_pos` in `sys_id`.
/// Refuses MWD activation (`ModuleNotAllowedInDeadspace`) when the pocket's dungeon or room data
/// disallows MicroWarpdrives.
pub fn check_deadspace_mwd_allowed(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    ship_pos: [f64; 3],
) -> Result<(), BridgeRpcError> {
    let runtime_disallowed = has_dungeon_runtime_store(ctx)
        && with_dungeon_runtime_store(ctx, |store| {
            store.instances.values().any(|inst| {
                !inst.completed
                    && !inst.despawned
                    && inst.solar_system_id == sys_id
                    && inst.rooms.iter().any(|r| {
                        dist3(ship_pos, r.position) <= POCKET_RADIUS_M
                            && is_pocket_mwd_disallowed(ctx, inst, r)
                    })
            })
        });
    if runtime_disallowed {
        return Err(BridgeRpcError::user_error("ModuleNotAllowedInDeadspace"));
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

    let site_disallowed = with_pve_exploration_reg(ctx, |reg| {
        reg.sites_by_system.get(&sys_id).is_some_and(|sites| {
            sites.iter().any(|s| {
                !s.cleared
                    && dist3(ship_pos, s.position) <= POCKET_RADIUS_M
                    && (crate::services::pve_dungeon_editor::is_dungeon_or_room_mwd_disallowed(
                        ctx,
                        s.dungeon_id as i64,
                        None,
                    ) || ctx
                        .sde
                        .dungeons
                        .get(&s.dungeon_id)
                        .is_some_and(|d| d.archetype_id == Some(51)))
            })
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
        && with_dungeon_runtime_store(ctx, |store| {
            for inst in store.instances.values() {
                if inst.completed || inst.despawned || inst.solar_system_id != sys_id {
                    continue;
                }
                let src_room_idx = inst
                    .rooms
                    .iter()
                    .find(|r| dist3(ship_pos, r.position) <= POCKET_RADIUS_M)
                    .map(|r| r.room_index);
                for r in &inst.rooms {
                    let targets_room = dist3(anchor_pos, r.position) <= POCKET_RADIUS_M
                        || dist3(dest_pos, r.position) <= POCKET_RADIUS_M;
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
    if blocked {
        return Err(BridgeRpcError::user_error("NaturalPhenomenaDisruptingWarp"));
    }

    let blocked_site = with_pve_exploration_reg(ctx, |reg| {
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
        let pos_opt = with_pve_exploration_reg(ctx, |reg| {
            let esc = reg.escalations_by_char.get(&char_id).and_then(|list| {
                list.iter()
                    .find(|e| e.instance_id == target_id && e.destination_solar_system_id == sys_id)
            })?;
            let site_id = format!("EXP-{}-{}", esc.instance_id, esc.step_index);
            reg.sites_by_system
                .get(&sys_id)
                .and_then(|sites| sites.iter().find(|s| s.site_id == site_id && !s.cleared))
                .map(|s| DVec3::from_array(s.position))
        });
        if pos_opt.is_some() {
            return Ok(pos_opt);
        }
        let bm_pos = ctx.db.with_snapshot(|snap| {
            snap.bookmarks
                .values()
                .find(|b| {
                    b.owner_id == char_id
                        && b.solar_system_id == sys_id
                        && b.item_id == Some(target_id)
                })
                .map(|b| DVec3::new(b.x, b.y, b.z))
        });
        return Ok(bm_pos);
    }

    if matches!(stuff_type, "dungeon" | "location") {
        let res = with_dungeon_runtime_store(ctx, |store| {
            for inst in store.instances.values() {
                if inst.completed || inst.despawned || inst.solar_system_id != sys_id {
                    continue;
                }
                let matches_inst = inst.instance_id == target_id
                    || i64::from(inst.dungeon_id) == target_id
                    || target_str_opt == Some(format!("DNG-INST-{}", inst.instance_id).as_str());
                if matches_inst {
                    if let Some(entry_pos) = inst.entry_gate_pos {
                        return Ok(Some(DVec3::from_array(entry_pos)));
                    }
                    if let Some(first_room) = inst.rooms.first() {
                        if first_room.gated {
                            return Err(BridgeRpcError::user_error("NaturalPhenomenaDisruptingWarp"));
                        }
                        return Ok(Some(DVec3::from_array(first_room.position)));
                    }
                }
                if let Some(room) = inst.rooms.iter().find(|r| r.room_id == target_id) {
                    if room.gated {
                        return Err(BridgeRpcError::user_error("NaturalPhenomenaDisruptingWarp"));
                    }
                    return Ok(Some(DVec3::from_array(room.position)));
                }
            }
            Ok(None)
        })?;
        if res.is_some() {
            return Ok(res);
        }

        let site_pos = with_pve_exploration_reg(ctx, |reg| {
            reg.sites_by_system.get(&sys_id).and_then(|sites| {
                sites
                    .iter()
                    .find(|s| {
                        !s.cleared
                            && (i64::from(s.dungeon_id) == target_id
                                || target_str_opt == Some(s.site_id.as_str()))
                    })
                    .map(|s| DVec3::from_array(s.position))
            })
        });
        if site_pos.is_some() {
            return Ok(site_pos);
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
    let runtime_did = with_dungeon_runtime_store(ctx, |store| {
        store.instances.values().find_map(|inst| {
            if inst.completed || inst.despawned || inst.solar_system_id != sys_id {
                return None;
            }
            let near_entry = inst
                .entry_gate_pos
                .is_some_and(|ep| dist3(pos, ep) <= 150_000.0);
            let near_room = inst
                .rooms
                .iter()
                .any(|r| dist3(pos, r.position) <= 150_000.0);
            (near_entry || near_room).then_some(inst.dungeon_id)
        })
    });
    if runtime_did.is_some() {
        return runtime_did;
    }

    with_pve_exploration_reg(ctx, |reg| {
        reg.sites_by_system.get(&sys_id).and_then(|sites| {
            sites
                .iter()
                .find(|s| !s.cleared && dist3(pos, s.position) <= 150_000.0)
                .map(|s| s.dungeon_id)
        })
    })
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

/// Called upon warp landing (`OnWarpFinished`) to materialize room entities and emit
/// `OnEnteringDungeonRoom` + `OnDistributionDungeonEntered` / `OnEscalatingPathDungeonEntered`.
pub async fn on_warp_landed_at_dungeon_pocket_if_present(
    ctx: &ServerContext,
    char_id: i64,
    sys_id: SolarSystemId,
    landed_pos: [f64; 3],
) {
    // Check if landing in a DungeonRuntimeInstance room
    let runtime_entry = with_dungeon_runtime_store(ctx, |store| {
        store.instances.values().find_map(|inst| {
            if inst.completed || inst.despawned || inst.solar_system_id != sys_id {
                return None;
            }
            let room = inst
                .rooms
                .iter()
                .find(|r| dist3(landed_pos, r.position) <= 150_000.0)?;
            Some((
                inst.instance_id,
                inst.dungeon_id,
                inst.category.clone(),
                inst.dungeon_name.clone(),
                room.room_index,
                room.position,
                inst.rooms.len(),
                inst.created_unix,
            ))
        })
    });

    if let Some((inst_id, dungeon_id, category, dungeon_name, room_idx, room_pos, total_rooms, created_unix)) =
        runtime_entry
    {
        if matches!(category.as_str(), "Expedition" | "Escalation") {
            with_pve_exploration_reg(ctx, |reg| {
                if let Some(list) = reg.escalations_by_char.get_mut(&char_id) {
                    for esc in list.iter_mut() {
                        if esc.destination_solar_system_id == sys_id
                            && esc.expedition_dungeon_id == dungeon_id
                            && !esc.dungeons_entered.contains(&dungeon_id)
                        {
                            esc.dungeons_entered.push(dungeon_id);
                        }
                    }
                }
            });
        }
        let _ = materialize_dungeon_room_entities(ctx, sys_id, inst_id, room_idx.saturating_sub(1)).await;
        let expiry_ft = unix_to_filetime_ticks(created_unix.saturating_add(86_400));
        emit_dungeon_progression_notifications(
            ctx,
            char_id,
            sys_id,
            &category,
            &dungeon_name,
            i64::from(dungeon_id),
            room_pos,
            room_idx as u8,
            total_rooms.max(1) as u8,
            false,
            expiry_ft,
        );
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
        if let Some((did, _, _, _, _, _, _, _, exp_inst_opt)) = &found {
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

    if let Some((did, category, dungeon_name, site_pos, cur_wave, total_waves, created_unix, lifetime_sec, _)) =
        site_entry
    {
        let life = if lifetime_sec == 0 { 86_400 } else { lifetime_sec };
        let expiry_ft = unix_to_filetime_ticks(created_unix.saturating_add(life));
        emit_dungeon_progression_notifications(
            ctx,
            char_id,
            sys_id,
            &category,
            &dungeon_name,
            i64::from(did),
            site_pos,
            cur_wave,
            total_waves,
            false,
            expiry_ft,
        );
    }
}

/// Materializes a room's objects, acceleration gate, and immediate NPC groups into the system ballpark.
pub(crate) async fn materialize_dungeon_room_entities(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    instance_id: i64,
    room_vec_idx: usize,
) -> Result<(Vec<i64>, Vec<i64>, Option<i64>), BridgeRpcError> {
    enum SpawnPlan {
        Object {
            ball_id: i64,
            type_id: u32,
            pos: [f64; 3],
            radius: f64,
        },
        Gate {
            ball_id: i64,
            pos: [f64; 3],
        },
        Npc {
            ball_id: i64,
            type_id: u32,
            pos: [f64; 3],
        },
    }

    let (plans, obj_ids, npc_ids, gate_id_opt) = ctx.db.with_mut_snapshot(|snap| {
        let mut store: DungeonRuntimeStore = snap
            .service_state_blobs
            .get(DUNGEON_RUNTIME_BLOB_KEY)
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();

        let Some(inst) = store.instances.get_mut(&instance_id) else {
            return Err(BridgeRpcError::user_error("DungeonInstanceNotFound"));
        };
        if inst.completed || inst.despawned {
            return Err(BridgeRpcError::user_error("DungeonAlreadyCompleted"));
        }
        let total_rooms = inst.rooms.len();
        let entry_gate_pos_opt = inst.entry_gate_pos;
        let existing_entry_gate = inst.entry_gate_ball_id;
        let owner_char_id = inst.owner_char_id;

        let mut plans = Vec::new();
        let mut obj_ids = Vec::new();
        let mut npc_ids = Vec::new();

        if room_vec_idx == 0 {
            if let Some(ep) = entry_gate_pos_opt {
                let eg_id = existing_entry_gate.unwrap_or_else(|| {
                    snap.next_entity_id += 1;
                    snap.next_entity_id
                });
                inst.entry_gate_ball_id = Some(eg_id);
                plans.push(SpawnPlan::Gate {
                    ball_id: eg_id,
                    pos: ep,
                });
            }
        }

        let Some(room) = inst.rooms.get_mut(room_vec_idx) else {
            return Err(BridgeRpcError::user_error("DungeonRoomNotFound"));
        };

        for obj in &mut room.objects {
            if obj.ball_id <= 0 {
                snap.next_entity_id += 1;
                obj.ball_id = snap.next_entity_id;
            }
            obj_ids.push(obj.ball_id);
            if !obj.spawned {
                obj.spawned = true;
                plans.push(SpawnPlan::Object {
                    ball_id: obj.ball_id,
                    type_id: obj.type_id,
                    pos: obj.position,
                    radius: obj.radius,
                });
                if obj.is_container && !snap.items.contains_key(&obj.ball_id) {
                    let cont_owner = if owner_char_id > 0 { owner_char_id } else { 500_010 };
                    snap.items.insert(
                        obj.ball_id,
                        InventoryItemRecord {
                            item_id: obj.ball_id,
                            type_id: if obj.type_id > 0 { obj.type_id } else { DEFAULT_CONTAINER_TYPE_ID },
                            owner_id: cont_owner,
                            location_id: i64::from(sys_id),
                            flag: InventoryFlag::Hangar,
                            quantity: 1,
                            singleton: true,
                            version: 1,
                            dynamic_attributes: HashMap::new(),
                            source_mutaplasmid_id: None,
                            insured_payout_isk: 0.0,
                            is_blueprint_copy: false,
                            blueprint_runs_remaining: None,
                        },
                    );
                    for &(loot_tid, loot_qty) in &obj.loot_items {
                        snap.next_entity_id += 1;
                        let lid = snap.next_entity_id;
                        snap.items.insert(
                            lid,
                            InventoryItemRecord {
                                item_id: lid,
                                type_id: loot_tid,
                                owner_id: cont_owner,
                                location_id: obj.ball_id,
                                flag: InventoryFlag::Cargo,
                                quantity: loot_qty.max(1),
                                singleton: false,
                                version: 1,
                                dynamic_attributes: HashMap::new(),
                                source_mutaplasmid_id: None,
                                insured_payout_isk: 0.0,
                                is_blueprint_copy: false,
                                blueprint_runs_remaining: None,
                            },
                        );
                    }
                }
            }
        }

        if room_vec_idx + 1 < total_rooms {
            let gid = room.gate_ball_id.unwrap_or_else(|| {
                snap.next_entity_id += 1;
                snap.next_entity_id
            });
            let first_spawn = room.gate_ball_id.is_none() || !room.spawned;
            room.gate_ball_id = Some(gid);
            if first_spawn {
                let gate_pos = [
                    room.position[0] + 5_000.0,
                    room.position[1],
                    room.position[2],
                ];
                plans.push(SpawnPlan::Gate {
                    ball_id: gid,
                    pos: gate_pos,
                });
            }
        }

        for grp in &mut room.npc_groups {
            if grp.spawn_immediately && !grp.spawned && !grp.cleared {
                grp.spawned = true;
                let mut offset_idx = 0usize;
                for &(npc_tid, count) in &grp.npc_specs {
                    for _ in 0..count {
                        snap.next_entity_id += 1;
                        let nid = snap.next_entity_id;
                        let angle = (offset_idx as f64) * 0.85;
                        let npc_pos = [
                            room.position[0] + angle.cos() * 8_000.0,
                            room.position[1],
                            room.position[2] + angle.sin() * 8_000.0,
                        ];
                        offset_idx += 1;
                        grp.npc_ball_ids.push(nid);
                        plans.push(SpawnPlan::Npc {
                            ball_id: nid,
                            type_id: npc_tid,
                            pos: npc_pos,
                        });
                    }
                }
            }
            npc_ids.extend(grp.npc_ball_ids.iter().copied());
        }

        room.spawned = true;
        let gate_id_opt = room.gate_ball_id;

        if let Ok(json) = serde_json::to_string(&store) {
            snap.service_state_blobs
                .insert(DUNGEON_RUNTIME_BLOB_KEY.to_string(), json);
        }
        Ok((plans, obj_ids, npc_ids, gate_id_opt))
    })?;

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for plan in plans {
        match plan {
            SpawnPlan::Object {
                ball_id,
                type_id,
                pos,
                radius,
            } => {
                let mut ball = make_site_npc_ball(ball_id, type_id, DVec3::from_array(pos));
                ball.radius = radius.max(100.0);
                let _ = sys_handle.spawn_ball(ball).await;
            }
            SpawnPlan::Gate { ball_id, pos } => {
                let mut ball =
                    make_site_npc_ball(ball_id, ACCELERATION_GATE_TYPE_ID, DVec3::from_array(pos));
                ball.radius = 1_500.0;
                let _ = sys_handle.spawn_ball(ball).await;
            }
            SpawnPlan::Npc {
                ball_id,
                type_id,
                pos,
            } => {
                let ball = make_site_npc_ball(ball_id, type_id, DVec3::from_array(pos));
                let cs = build_type_combat_state_from_sde(&ctx.sde, type_id);
                let _ = sys_handle.spawn_ball_with_combat(ball, cs).await;
            }
        }
    }

    Ok((obj_ids, npc_ids, gate_id_opt))
}

/// Despawns all remaining dungeon objects, gates, and NPCs when a dungeon completes and emits `OnDungeonCompleted`.
pub async fn despawn_completed_dungeon(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    instance_id: i64,
    char_id: i64,
) {
    despawn_completed_dungeon_core(
        &ctx.systems,
        &ctx.db,
        Some(&ctx.pending_notifications),
        Some(&ctx.notify_tx),
        sys_id,
        instance_id,
        char_id,
    )
    .await;
}

#[allow(clippy::too_many_arguments)]
pub async fn despawn_completed_dungeon_core(
    systems: &std::sync::Arc<crate::system_actor::SystemRegistry>,
    db: &std::sync::Arc<eve_db::WorldRepository>,
    pending_notifications: Option<
        &std::sync::RwLock<
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
    let cleanup = with_dungeon_runtime_store_db(db, |store| {
        let Some(inst) = store.instances.get_mut(&instance_id) else {
            return None;
        };
        if inst.despawned {
            return None;
        }
        let mut balls_to_remove = Vec::new();
        if let Some(eg) = inst.entry_gate_ball_id.take() {
            balls_to_remove.push(eg);
        }
        for r in &mut inst.rooms {
            if let Some(g) = r.gate_ball_id.take() {
                balls_to_remove.push(g);
            }
            for obj in &mut r.objects {
                if obj.spawned && obj.ball_id > 0 {
                    balls_to_remove.push(obj.ball_id);
                    obj.spawned = false;
                }
            }
            for grp in &mut r.npc_groups {
                balls_to_remove.extend(grp.npc_ball_ids.drain(..));
                grp.cleared = true;
            }
            r.cleared = true;
        }
        inst.completed = true;
        inst.despawned = true;
        let recipient = if char_id > 0 { char_id } else { inst.owner_char_id };
        Some((inst.dungeon_id, recipient, balls_to_remove))
    });

    if let Some((dungeon_id, recipient_char_id, balls)) = cleanup {
        let sys_handle = systems.get_or_spawn(sys_id);
        for bid in balls {
            let _ = sys_handle.remove_ball(bid).await;
        }
        let now_unix = db.clock().now_unix_secs();
        let mut cleared_sig_ids = Vec::new();
        crate::services::pve_exploration::with_pve_exploration_reg_db(db, now_unix, |reg| {
            if let Some(sites) = reg.sites_by_system.get_mut(&sys_id) {
                for s in sites.iter_mut() {
                    if !s.cleared && s.dungeon_id == dungeon_id && s.site_category == "Deadspace" {
                        s.cleared = true;
                        cleared_sig_ids.push(s.site_id.clone());
                    }
                }
            }
        });
        if !cleared_sig_ids.is_empty() {
            let mut collapsed_now = Vec::new();
            db.with_mut_snapshot(|snap| {
                if let Some(sigs) = snap.cosmic_signatures.get_mut(&sys_id) {
                    for sig in sigs.iter_mut() {
                        if !sig.collapsed && cleared_sig_ids.contains(&sig.signature_id) {
                            sig.collapsed = true;
                            collapsed_now.push(sig.signature_id.clone());
                        }
                    }
                }
            });
            if let (Some(pend), Some(tx)) = (pending_notifications, notify_tx) {
                for sig_id in &collapsed_now {
                    crate::services::fleet_social::fleet_social_scanning::emit_signal_tracker_site_removed_with_db(
                        db, pend, tx, sig_id,
                    );
                }
            }
        }
        if recipient_char_id > 0 {
            if let (Some(pend), Some(tx)) = (pending_notifications, notify_tx) {
                let d_args = vec![PyRep::Integer(i64::from(dungeon_id))];
                let (d_pkt, d_mpkt) = ServerContext::build_macho_notification_packet(
                    recipient_char_id,
                    None,
                    "OnDungeonCompleted",
                    "charid",
                    d_args.clone(),
                );
                let notif = crate::router::cache_notifications::PendingMachoNotification {
                    recipient_char_id,
                    event: "OnDungeonCompleted".to_string(),
                    id_type: "charid".to_string(),
                    args: d_args,
                    packet: d_pkt,
                    marshaled_packet: d_mpkt,
                    created_filetime: db.clock().now_blueos_filetime_ticks(),
                };
                let mut queues = pend.write().unwrap_or_else(|e| e.into_inner());
                let q = queues.entry(recipient_char_id).or_default();
                if q.len() >= 256 {
                    q.remove(0);
                }
                q.push(notif.clone());
                let _ = tx.send((recipient_char_id, notif));
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn apply_dungeon_trigger_actions_core(
    systems: &std::sync::Arc<crate::system_actor::SystemRegistry>,
    db: &std::sync::Arc<eve_db::WorldRepository>,
    sde: &eve_sde::SdeRegistry,
    pending_notifications: Option<
        &std::sync::RwLock<
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
    room_vec_idx: usize,
    actions: Vec<DungeonTriggerAction>,
    char_id: i64,
) -> usize {
    let mut fired_count = 0usize;
    let mut groups_to_spawn = Vec::new();
    let mut complete_now = false;

    with_dungeon_runtime_store_db(db, |store| {
        let Some(inst) = store.instances.get_mut(&instance_id) else {
            return;
        };
        if inst.completed || inst.despawned {
            return;
        }
        let Some(room) = inst.rooms.get_mut(room_vec_idx) else {
            return;
        };
        for act in actions {
            fired_count += 1;
            match act {
                DungeonTriggerAction::UnlockGate => {
                    room.gate_locked = false;
                }
                DungeonTriggerAction::SpawnNpcGroup(grp_id) => {
                    groups_to_spawn.push(grp_id);
                }
                DungeonTriggerAction::CompleteDungeon => {
                    room.cleared = true;
                    complete_now = true;
                }
            }
        }
    });

    if !groups_to_spawn.is_empty() {
        let spawn_list = db.with_mut_snapshot(|snap| {
            let mut store: DungeonRuntimeStore = snap
                .service_state_blobs
                .get(DUNGEON_RUNTIME_BLOB_KEY)
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or_default();
            let mut to_spawn = Vec::new();
            if let Some(inst) = store.instances.get_mut(&instance_id) {
                if let Some(room) = inst.rooms.get_mut(room_vec_idx) {
                    for grp_id in &groups_to_spawn {
                        if let Some(grp) = room
                            .npc_groups
                            .iter_mut()
                            .find(|g| g.group_id == *grp_id && !g.spawned)
                        {
                            grp.spawned = true;
                            let mut offset_idx = 0usize;
                            for &(npc_tid, count) in &grp.npc_specs {
                                for _ in 0..count {
                                    snap.next_entity_id += 1;
                                    let nid = snap.next_entity_id;
                                    let angle = (offset_idx as f64) * 0.9 + 0.4;
                                    let npc_pos = [
                                        room.position[0] + angle.cos() * 9_500.0,
                                        room.position[1],
                                        room.position[2] + angle.sin() * 9_500.0,
                                    ];
                                    offset_idx += 1;
                                    grp.npc_ball_ids.push(nid);
                                    to_spawn.push((nid, npc_tid, npc_pos));
                                }
                            }
                        }
                    }
                }
            }
            if let Ok(json) = serde_json::to_string(&store) {
                snap.service_state_blobs
                    .insert(DUNGEON_RUNTIME_BLOB_KEY.to_string(), json);
            }
            to_spawn
        });

        let sys_handle = systems.get_or_spawn(sys_id);
        for (nid, npc_tid, npc_pos) in spawn_list {
            let ball = make_site_npc_ball(nid, npc_tid, DVec3::from_array(npc_pos));
            let cs = build_type_combat_state_from_sde(sde, npc_tid);
            let _ = sys_handle.spawn_ball_with_combat(ball, cs).await;
        }
    }

    if complete_now {
        despawn_completed_dungeon_core(
            systems,
            db,
            pending_notifications,
            notify_tx,
            sys_id,
            instance_id,
            char_id,
        )
        .await;
    }

    fired_count
}

pub(crate) async fn apply_dungeon_trigger_actions(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    instance_id: i64,
    room_vec_idx: usize,
    actions: Vec<DungeonTriggerAction>,
    char_id: i64,
) -> usize {
    apply_dungeon_trigger_actions_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        Some(&ctx.pending_notifications),
        Some(&ctx.notify_tx),
        sys_id,
        instance_id,
        room_vec_idx,
        actions,
        char_id,
    )
    .await
}

/// Evaluates `OnKill` triggers and room/dungeon completion when an NPC ball is destroyed in `sys_id`.
pub async fn on_dungeon_npc_killed(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    npc_ball_id: i64,
    killer_char_id: i64,
) {
    on_dungeon_npc_killed_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        Some(&ctx.pending_notifications),
        Some(&ctx.notify_tx),
        sys_id,
        npc_ball_id,
        killer_char_id,
    )
    .await;
}

#[allow(clippy::too_many_arguments)]
pub async fn on_dungeon_npc_killed_core(
    systems: &std::sync::Arc<crate::system_actor::SystemRegistry>,
    db: &std::sync::Arc<eve_db::WorldRepository>,
    sde: &eve_sde::SdeRegistry,
    pending_notifications: Option<
        &std::sync::RwLock<
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
    npc_ball_id: i64,
    killer_char_id: i64,
) {
    if !has_dungeon_runtime_store_db(db) {
        return;
    }
    let mut pending_actions = Vec::new();
    let mut auto_complete_insts = Vec::new();

    with_dungeon_runtime_store_db(db, |store| {
        for inst in store.instances.values_mut() {
            if inst.completed || inst.despawned || inst.solar_system_id != sys_id {
                continue;
            }
            let total_rooms = inst.rooms.len();
            for (r_idx, room) in inst.rooms.iter_mut().enumerate() {
                let mut matched_group_ids = Vec::new();
                let mut any_removed = false;
                for grp in &mut room.npc_groups {
                    if let Some(pos) = grp.npc_ball_ids.iter().position(|&id| id == npc_ball_id) {
                        grp.npc_ball_ids.remove(pos);
                        any_removed = true;
                        if grp.npc_ball_ids.is_empty() {
                            grp.cleared = true;
                            matched_group_ids.push(grp.group_id);
                        }
                    }
                }
                if !any_removed {
                    continue;
                }
                let all_spawned_dead = room
                    .npc_groups
                    .iter()
                    .filter(|g| g.spawned)
                    .all(|g| g.npc_ball_ids.is_empty());

                let mut room_actions = Vec::new();
                for trig in &mut room.triggers {
                    if trig.fired {
                        continue;
                    }
                    if let DungeonTriggerKind::OnKill {
                        target_npc_group,
                        target_ball_id,
                    } = &trig.kind
                    {
                        let matches = if let Some(tbid) = target_ball_id {
                            *tbid == npc_ball_id
                        } else if let Some(tgrp) = target_npc_group {
                            matched_group_ids.contains(tgrp)
                        } else {
                            all_spawned_dead
                        };
                        if matches {
                            trig.fired = true;
                            room_actions.push(trig.action.clone());
                        }
                    }
                }
                let spawns_more_npcs = room_actions
                    .iter()
                    .any(|a| matches!(a, DungeonTriggerAction::SpawnNpcGroup(_)));
                let explicit_complete = room_actions
                    .iter()
                    .any(|a| matches!(a, DungeonTriggerAction::CompleteDungeon));
                if !room_actions.is_empty() {
                    pending_actions.push((inst.instance_id, r_idx, room_actions));
                }

                let any_unspawned_groups = room.npc_groups.iter().any(|g| !g.spawned);
                let any_unfired_triggers = room.triggers.iter().any(|t| !t.fired);
                if all_spawned_dead && !spawns_more_npcs && !any_unspawned_groups && !any_unfired_triggers {
                    room.cleared = true;
                    room.gate_locked = false;
                    if r_idx + 1 == total_rooms && !explicit_complete {
                        auto_complete_insts.push(inst.instance_id);
                    }
                }
            }
        }
    });

    for (inst_id, r_idx, actions) in pending_actions {
        let _ = apply_dungeon_trigger_actions_core(
            systems,
            db,
            sde,
            pending_notifications,
            notify_tx,
            sys_id,
            inst_id,
            r_idx,
            actions,
            killer_char_id,
        )
        .await;
    }

    for inst_id in auto_complete_insts {
        let still_clear = with_dungeon_runtime_store_db(db, |store| {
            store.instances.get(&inst_id).is_some_and(|inst| {
                !inst.completed
                    && !inst.despawned
                    && inst.rooms.last().is_some_and(|r| {
                        r.npc_groups.iter().all(|g| g.spawned && g.npc_ball_ids.is_empty())
                            && r.triggers.iter().all(|t| t.fired)
                    })
            })
        });
        if still_clear {
            despawn_completed_dungeon_core(
                systems,
                db,
                pending_notifications,
                notify_tx,
                sys_id,
                inst_id,
                killer_char_id,
            )
            .await;
        }
    }
}

pub use crate::services::pve_dungeon_runtime_rpc::{
    check_dungeon_triggers, get_dungeon_instance_state, on_dungeon_approach,
    on_dungeon_item_looted, spawn_dungeon_instance, spawn_sde_dungeon_runtime_instance,
    try_activate_dungeon_runtime_gate,
};
