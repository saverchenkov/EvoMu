//! `fitting_containers_ships` — Containers, ALSC, Item Locking, Encoded Items,
//! SMA/Fleet Hangar, Ship Eject/Launch/Config, Newbie Ship, Ship Tree/Masteries, FitShips & Simulation.

#![forbid(unsafe_code)]

use super::*;
// structureAssetSafety is implemented once, in `station_structure::structure_upwell_ops`.
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

const CONTAINER_SHIP_NOTE_ID: i64 = -8_400_000_000_000;
const CONTAINER_SHIP_NOTE_OWNER_ID: i64 = -84;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AlscLogEntry {
    pub timestamp_unix: u64, pub actor_id: i64, pub action: String,
    pub item_id: i64, pub item_type_id: u32, pub quantity: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct InsuranceContractRecord {
    pub ship_id: i64, pub ship_type_id: u32, pub owner_id: i64,
    pub level_name: String, pub level_fraction: f64, pub cost_isk: f64,
    pub payout_isk: f64, pub start_unix: u64, pub end_unix: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ShipAccessConfig {
    pub allow_fleet_smb: bool, pub allow_corp_smb: bool,
    pub allow_fleet_hangar: bool, pub allow_corp_hangar: bool,
    pub password: Option<String>,
}

impl Default for ShipAccessConfig {
    fn default() -> Self {
        Self { allow_fleet_smb: true, allow_corp_smb: true, allow_fleet_hangar: true, allow_corp_hangar: true, password: None }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct ContainerConfig {
    pub lock_by_default: bool,
    pub password_per_action: bool,
    pub config_bitmask: u32,
}

/// Resolves a fallback ship `type_id` (from the character's most recent killmail or `670` Capsule)
/// when an authenticated session queries `invbroker.GetInventoryFromId` / `GetSelfInvItem` for a
/// dynamically spawned ship entity (`200_000_000..1_000_000_000_000`) that was just destroyed in
/// combat (`OnSessionChanged` / `OnCapacityChange` teardown race, GH `#803`).
pub(crate) fn resolve_missing_dynamic_ship_stub_type_id(
    ctx: &ServerContext,
    char_id: i64,
    item_id: i64,
) -> Option<u32> {
    if !(200_000_000..1_000_000_000_000).contains(&item_id) || char_id <= 0 {
        return None;
    }
    ctx.db.with_snapshot(|snap| {
        if snap.items.contains_key(&item_id) {
            return None;
        }
        snap.killmails
            .values()
            .filter(|km| km.victim_character_id == char_id && km.ship_type_id > 0)
            .max_by_key(|km| (km.killed_unix, km.kill_id))
            .map(|km| km.ship_type_id)
            .or(Some(670))
    })
}

#[derive(Default, Clone, Serialize, Deserialize)]
pub(crate) struct ContainerShipRuntimeState {
    pub container_passwords: HashMap<i64, String>,
    #[serde(default)]
    pub container_config_passwords: HashMap<i64, String>,
    pub unlocked_containers: HashSet<(i64, i64)>,
    pub alsc_locked_items: HashSet<i64>,
    pub alsc_logs: HashMap<i64, Vec<AlscLogEntry>>,
    pub ship_configs: HashMap<i64, ShipAccessConfig>,
    pub custom_item_flags: HashMap<i64, u16>,
    pub insurance_contracts: HashMap<i64, InsuranceContractRecord>,
    #[serde(default)]
    pub ship_passwords: HashMap<i64, String>,
    #[serde(default)]
    pub container_configs: HashMap<i64, ContainerConfig>,
}

impl ContainerShipRuntimeState {
    pub fn get_container_password(&self, cid: i64, which: i64) -> Option<&str> {
        (if which == 2 { &self.container_config_passwords } else { &self.container_passwords }).get(&cid).map(String::as_str)
    }
    pub fn set_container_password(&mut self, cid: i64, which: i64, pw: String) {
        let map = if which == 2 { &mut self.container_config_passwords } else { &mut self.container_passwords };
        if pw.is_empty() { map.remove(&cid); } else { map.insert(cid, pw); }
    }
}

pub(crate) fn read_container_ship_state_snap(snap: &eve_db::WorldStateSnapshot) -> ContainerShipRuntimeState {
    snap.owner_notes.get(&CONTAINER_SHIP_NOTE_ID).and_then(|n| serde_json::from_str::<ContainerShipRuntimeState>(&n.note).ok()).unwrap_or_default()
}

pub(crate) fn read_container_ship_state(ctx: &ServerContext) -> ContainerShipRuntimeState {
    ctx.db.with_snapshot(read_container_ship_state_snap)
}

pub(crate) fn write_container_ship_state_snap<R>(snap: &mut eve_db::WorldStateSnapshot, f: impl FnOnce(&mut ContainerShipRuntimeState) -> R) -> R {
    let mut state = read_container_ship_state_snap(snap);
    let out = f(&mut state);
    if let Ok(json) = serde_json::to_string(&state) {
        let ts = snap.owner_notes.get(&CONTAINER_SHIP_NOTE_ID).map(|n| n.timestamp_unix).unwrap_or(0);
        snap.owner_notes.insert(CONTAINER_SHIP_NOTE_ID, eve_db::OwnerNoteRecord {
            note_id: CONTAINER_SHIP_NOTE_ID, character_id: CONTAINER_SHIP_NOTE_OWNER_ID, item_id: 0,
            label: "S:CONTAINER_SHIP_RUNTIME_STATE".to_string(), note: json, timestamp_unix: ts,
        });
    }
    out
}

pub(crate) fn save_container_ship_state_snap(snap: &mut eve_db::WorldStateSnapshot, now_unix: u64, state: &ContainerShipRuntimeState) {
    if let Ok(json) = serde_json::to_string(state) {
        snap.owner_notes.insert(CONTAINER_SHIP_NOTE_ID, eve_db::OwnerNoteRecord {
            note_id: CONTAINER_SHIP_NOTE_ID, character_id: CONTAINER_SHIP_NOTE_OWNER_ID, item_id: 0,
            label: "S:CONTAINER_SHIP_RUNTIME_STATE".to_string(), note: json, timestamp_unix: now_unix,
        });
    }
}

pub(crate) fn save_container_ship_state(ctx: &ServerContext, state: &ContainerShipRuntimeState) {
    let now_unix = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        save_container_ship_state_snap(snap, now_unix, state);
    });
}

pub(crate) fn write_container_ship_state<R>(
    ctx: &ServerContext,
    f: impl FnOnce(&mut ContainerShipRuntimeState) -> R,
) -> R {
    let now_unix = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let out = write_container_ship_state_snap(snap, f);
        if let Some(note) = snap.owner_notes.get_mut(&CONTAINER_SHIP_NOTE_ID) {
            note.timestamp_unix = now_unix;
        }
        out
    })
}

pub(crate) fn is_container_group(group_id: u32, type_id: TypeId) -> bool {
    type_id == 23
        || matches!(
            group_id,
            12 | 340
                | 448
                | 649
                | 1247
                | crate::services::mobile_deployables::GROUP_MOBILE_DEPOT
                | crate::services::mobile_deployables::GROUP_CRAB_BEACON
        )
}

/// Computes unit volume of an inventory item, respecting packaged hull/container volumes when `!item.singleton`.
pub(crate) fn effective_item_unit_volume(
    sde: &eve_sde::SdeRegistry,
    item: &eve_db::InventoryItemRecord,
) -> f64 {
    let Some(t) = sde.types.get(&item.type_id) else {
        return 0.0;
    };
    if item.singleton {
        return t.volume;
    }
    if t.category_id == 6 {
        // Packaged EVE ship volumes by group
        return match t.group_id {
            29 | 31 | 237 => 500.0, // Capsule, Shuttle, Corvette
            25 | 324 | 830 | 831 | 834 | 893 | 1283 | 1527 => 2_500.0, // Frigates
            463 | 543 => 3_750.0, // Mining Barges & Exhumers
            420 | 541 | 1305 | 1534 => 5_000.0, // Destroyers
            26 | 358 | 832 | 833 | 894 | 906 | 963 => 10_000.0, // Cruisers
            419 | 540 | 1201 => 15_000.0, // Battlecruisers
            28 | 380 | 1202 => 20_000.0, // Industrials / Haulers
            27 | 898 | 900 => 50_000.0, // Battleships
            _ if t.volume > 50_000.0 => (t.volume * 0.05).clamp(50_000.0, 1_000_000.0),
            _ => t.volume.min(2_500.0),
        };
    }
    if is_container_group(t.group_id, t.type_id) {
        return (t.volume * 0.1).min(5_000.0);
    }
    t.volume
}

/// Checks if `ship_id` in `sys_id` is within refitting range of an active Mobile Depot (`group_id == 1246`, 2,500m, owner-only, anchored)
/// or another ship providing a Ship Maintenance Bay fitting service (`attr 907` / `908` > 0, 3,000m).
pub async fn is_near_in_space_fitting_service(ctx: &ServerContext, sys_id: u32, ship_id: i64) -> bool {
    let char_id = ctx.db.with_snapshot(|snap| snap.items.get(&ship_id).map(|s| s.owner_id)).unwrap_or(0);
    let now_unix = ctx.now_unix();
    let deployables_state = crate::services::mobile_deployables::read_deployable_state(ctx);

    let providers: Vec<(i64, bool)> = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter_map(|i| {
                if i.item_id == ship_id || i.location_id != sys_id as i64 {
                    return None;
                }
                let t = ctx.sde.types.get(&i.type_id)?;
                let is_depot = t.group_id == 1246;
                let is_ship_fitting = t.default_attributes.get(&907).copied().unwrap_or(0.0) > 0.0
                    || t.default_attributes.get(&908).copied().unwrap_or(0.0) > 0.0;
                if is_depot {
                    // Mobile depot is usable ONLY by its owner and NOT while anchoring, unanchored, or in 48h reinforcement (`feat:modules.mobile-depot-deployment-and-refitting`)
                    if char_id != 0 && i.owner_id != char_id {
                        return None;
                    }
                    if let Some(rec) = deployables_state.deployables.get(&i.item_id) {
                        let is_anchored = !rec.is_unanchored
                            && (rec.anchor_completed || now_unix >= rec.anchor_start_unix.saturating_add(rec.anchor_duration_secs));
                        let is_reinforced = rec.reinforced_until_unix.is_some_and(|u| now_unix < u);
                        if !is_anchored || is_reinforced {
                            return None;
                        }
                    }
                    Some((i.item_id, true))
                } else if is_ship_fitting && can_character_access_ship_bay_with_password_snap(snap, char_id, i.item_id, i.owner_id, true, None) {
                    Some((i.item_id, false))
                } else {
                    None
                }
            })
            .collect()
    });
    if providers.is_empty() {
        return false;
    }
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let Ok(Some((_, ship_ball))) = sys_handle.get_ball_snapshot(ship_id).await else { return false };
    for (pid, is_depot) in providers {
        let p_pos = if let Ok(Some((_, p_ball))) = sys_handle.get_ball_snapshot(pid).await {
            Some(p_ball.position)
        } else if is_depot {
            ctx.db.with_snapshot(|snap| snap.item_positions.get(&pid).map(|p| glam::DVec3::from_array(*p)))
        } else {
            None
        };
        if let Some(pos) = p_pos {
            let max_range = if is_depot { 2_500.0 } else { 3_000.0 };
            if (ship_ball.position - pos).length() <= max_range {
                if is_depot {
                    crate::services::mobile_deployables::touch_mobile_depot_interaction(ctx, pid);
                }
                return true;
            }
        }
    }
    false
}

pub(crate) fn can_character_access_ship_bay_with_password_snap(
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
    host_ship_id: i64,
    host_owner_id: i64,
    is_sma: bool,
    _supplied_password: Option<&str>,
) -> bool {
    if char_id == host_owner_id {
        return true;
    }
    let st = read_container_ship_state_snap(snap);
    let cfg = st.ship_configs.get(&host_ship_id).cloned().unwrap_or_default();
    let c1 = snap.characters.get(&char_id).map(|c| c.corporation_id);
    let c2 = snap
        .characters
        .get(&host_owner_id)
        .map(|c| c.corporation_id)
        .or_else(|| {
            snap.corporations
                .contains_key(&host_owner_id)
                .then_some(host_owner_id)
        });
    let in_same_fleet = snap
        .fleets
        .values()
        .any(|fl| fl.members.contains(&char_id) && fl.members.contains(&host_owner_id));
    let same_corp = c1.is_some_and(|cid| cid > 0 && (cid == host_owner_id || c2 == Some(cid)));
    let base_allowed = if is_sma {
        (in_same_fleet && cfg.allow_fleet_smb) || (same_corp && cfg.allow_corp_smb)
    } else {
        (in_same_fleet && cfg.allow_fleet_hangar) || (same_corp && cfg.allow_corp_hangar)
    };
    if !base_allowed {
        return false;
    }
    let req_pw = cfg
        .password
        .as_deref()
        .filter(|s| !s.is_empty());
    if let Some(req) = req_pw {
        let already_unlocked = st.unlocked_containers.contains(&(char_id, host_ship_id))
            || st.unlocked_containers.contains(&(host_ship_id, char_id));
        if !already_unlocked && _supplied_password != Some(req) {
            return false;
        }
    }
    true
}

#[allow(dead_code)]
pub(crate) fn can_character_access_ship_bay_snap(
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
    host_ship_id: i64,
    host_owner_id: i64,
    is_sma: bool,
) -> bool {
    can_character_access_ship_bay_with_password_snap(
        snap,
        char_id,
        host_ship_id,
        host_owner_id,
        is_sma,
        None,
    )
}



pub(crate) fn resolve_corp_container_log_target(
    snap: &eve_db::WorldStateSnapshot,
    container_id: i64,
) -> Option<(i64, i32, i64, String)> {
    let c_item = snap.items.get(&container_id)?;
    let corp_id = if snap.corporations.contains_key(&c_item.owner_id) {
        c_item.owner_id
    } else if matches!(c_item.flag as u16, 116..=122 | 62) {
        if let Some(parent) = snap.items.get(&c_item.location_id) {
            if snap.corporations.contains_key(&parent.owner_id) { parent.owner_id } else { return None; }
        } else {
            return None;
        }
    } else {
        return None;
    };
    if corp_id == 0 {
        return None;
    }
    let flag_str = match c_item.flag {
        eve_db::InventoryFlag::CorpSAG1 => "CorpSAG1",
        eve_db::InventoryFlag::CorpSAG2 => "CorpSAG2",
        eve_db::InventoryFlag::CorpSAG3 => "CorpSAG3",
        eve_db::InventoryFlag::CorpSAG4 => "CorpSAG4",
        eve_db::InventoryFlag::CorpSAG5 => "CorpSAG5",
        eve_db::InventoryFlag::CorpSAG6 => "CorpSAG6",
        eve_db::InventoryFlag::CorpSAG7 => "CorpSAG7",
        eve_db::InventoryFlag::CorpDeliveries => "CorpDeliveries",
        eve_db::InventoryFlag::Locked => "Locked",
        eve_db::InventoryFlag::Unlocked => "Unlocked",
        _ => "Hangar",
    }.to_string();
    Some((corp_id, c_item.type_id as i32, c_item.location_id, flag_str))
}

pub(crate) fn record_container_add_log(
    ctx: &ServerContext,
    char_id: i64,
    item_id: i64,
    moved_tid: u32,
    moved_qty: i32,
    container_id: i64,
) {
    let now_unix = ctx.now_unix_secs();
    let is_alsc = ctx.db.with_snapshot(|snap| {
        snap.items.get(&container_id).map(|i| {
            let grp = ctx.sde.types.get(&i.type_id).map(|t| t.group_id).unwrap_or(0);
            matches!(grp, 340 | 448)
        }).unwrap_or(false)
    });
    if !is_alsc {
        return;
    }
    write_container_ship_state(ctx, |st| {
        if st.container_configs.get(&container_id).is_some_and(|c| c.lock_by_default) {
            st.alsc_locked_items.insert(item_id);
            st.alsc_logs.entry(container_id).or_default().push(AlscLogEntry {
                timestamp_unix: now_unix, actor_id: char_id, action: "ALSCActionLock".into(), item_id, item_type_id: moved_tid, quantity: moved_qty,
            });
        }
        st.alsc_logs.entry(container_id).or_default().push(AlscLogEntry {
            timestamp_unix: now_unix,
            actor_id: char_id,
            action: "ALSCActionAdd".into(),
            item_id,
            item_type_id: moved_tid,
            quantity: moved_qty,
        });
    });
    ctx.db.with_mut_snapshot(|snap| {
        if let Some((corp_id, c_tid, c_loc, loc_flag)) =
            resolve_corp_container_log_target(snap, container_id)
        {
            snap.corp_container_logs.entry(corp_id).or_default().push(
                eve_db::CorpContainerLogEntry {
                    logged_at_unix: now_unix,
                    container_id,
                    container_type_id: c_tid,
                    character_id: char_id,
                    location_id: c_loc,
                    location_flag: loc_flag,
                    action: "add".into(),
                    password_type: None,
                    type_id: Some(moved_tid as i32),
                    quantity: Some(moved_qty),
                    old_config_bitmask: None,
                    new_config_bitmask: None,
                },
            );
        }
    });
}

pub(crate) fn record_container_take_log(
    ctx: &ServerContext,
    char_id: i64,
    item_id: i64,
    moved_tid: u32,
    moved_qty: i32,
    container_id: i64,
) {
    let now_unix = ctx.now_unix_secs();
    let is_alsc = ctx.db.with_snapshot(|snap| {
        snap.items
            .get(&container_id)
            .map(|i| {
                let grp = ctx.sde.types.get(&i.type_id).map(|t| t.group_id).unwrap_or(0);
                matches!(grp, 340 | 448)
            })
            .unwrap_or(false)
    });
    if !is_alsc {
        return;
    }
    write_container_ship_state(ctx, |st| {
        st.alsc_logs.entry(container_id).or_default().push(AlscLogEntry {
            timestamp_unix: now_unix,
            actor_id: char_id,
            action: "ALSCActionTake".into(),
            item_id,
            item_type_id: moved_tid,
            quantity: moved_qty,
        });
    });
    ctx.db.with_mut_snapshot(|snap| {
        if let Some((corp_id, c_tid, c_loc, loc_flag)) =
            resolve_corp_container_log_target(snap, container_id)
        {
            snap.corp_container_logs.entry(corp_id).or_default().push(
                eve_db::CorpContainerLogEntry {
                    logged_at_unix: now_unix,
                    container_id,
                    container_type_id: c_tid,
                    character_id: char_id,
                    location_id: c_loc,
                    location_flag: loc_flag,
                    action: "take".into(),
                    password_type: None,
                    type_id: Some(moved_tid as i32),
                    quantity: Some(moved_qty),
                    old_config_bitmask: None,
                    new_config_bitmask: None,
                },
            );
        }
    });
}

pub(crate) fn record_container_rename_log(ctx: &ServerContext, char_id: i64, container_id: i64, tid: u32) {
    let now_unix = ctx.now_unix_secs();
    write_container_ship_state(ctx, |st| {
        st.alsc_logs.entry(container_id).or_default().push(AlscLogEntry {
            timestamp_unix: now_unix, actor_id: char_id, action: "ALSCActionRename".into(), item_id: container_id, item_type_id: tid, quantity: 1,
        });
    });
}

pub(crate) fn container_has_active_audit_log_snap(snap: &eve_db::WorldStateSnapshot, container_id: i64, now_unix: u64) -> bool {
    read_container_ship_state_snap(snap).alsc_logs.get(&container_id).map_or(false, |logs| logs.iter().any(|e| now_unix.saturating_sub(e.timestamp_unix) < 21 * 86_400))
}

/// Hard-refuses moves/takes/strips involving `Impounded` (72), `Locked` (63), contract/job-locked,
/// `AssetSafety` (36), `AssetSafetyWrap` (`type_id == 60`), or active `ext.asset_safety_wraps` items
/// or any ancestor container in one of those states (`#549`).
pub(crate) fn check_item_lock_impound_asset_safety_in_snap(
    snap: &eve_db::WorldStateSnapshot,
    item: &eve_db::InventoryItemRecord,
) -> Result<(), BridgeRpcError> {
    let ext = crate::services::station_structure::structure_upwell_ops::load_ext_state(snap);
    let in_active_wrap = |iid: i64, loc_id: i64| -> bool {
        ext.asset_safety_wraps.values().any(|w| {
            !w.retrieval_fee_paid
                && (w.wrap_id == iid || w.wrap_id == loc_id || w.item_ids.contains(&iid))
        })
    };
    let check_single = |rec: &eve_db::InventoryItemRecord| -> Result<(), BridgeRpcError> {
        let raw_flag = rec.flag as u16;
        if rec.flag == InventoryFlag::Impounded || raw_flag == 72 {
            return Err(BridgeRpcError {
                code: "ImpoundedItem".into(),
                message: format!("Item {} is impounded and cannot be moved until impound fees are paid", rec.item_id),
            });
        }
        if rec.flag as u16 == 36 || rec.type_id == 60 || in_active_wrap(rec.item_id, rec.location_id) {
            return Err(BridgeRpcError {
                code: "AssetSafetyLocked".into(),
                message: format!("Item {} is sealed in Asset Safety until retrieval fees are paid", rec.item_id),
            });
        }
        if rec.flag == InventoryFlag::Locked
            || raw_flag == 63
            || snap.is_item_locked_in_contract_or_job(rec.item_id)
        {
            return Err(BridgeRpcError {
                code: "ItemLocked".into(),
                message: format!("Item {} is locked by contract, job, or corporation lockdown", rec.item_id),
            });
        }
        Ok(())
    };

    check_single(item)?;
    let mut cur_loc = item.location_id;
    for _ in 0..8 {
        let Some(anc) = snap.items.get(&cur_loc) else {
            break;
        };
        check_single(anc)?;
        cur_loc = anc.location_id;
    }
    Ok(())
}

/// Validates container rules, ALSC locks/passwords, SMA (`flag 90`), and Fleet Hangar (`flag 155`) on item move.
pub(crate) fn validate_container_and_bay_move(
    ctx: &ServerContext,
    char_id: i64,
    item_id: i64,
    new_loc: i64,
    flag_u16: u16,
    kwargs: &HashMap<String, PyRep>,
    requested_qty: Option<i64>,
) -> Result<(), BridgeRpcError> {
    if item_id == new_loc {
        return Err(BridgeRpcError { code: "UserError".into(), message: "CannotStoreShipInItself".into() });
    }
    if flag_u16 == 72 {
        return Err(BridgeRpcError {
            code: "ImpoundedItem".into(),
            message: "Cannot move items into Impounded flag".into(),
        });
    }
    if flag_u16 == 36 {
        return Err(BridgeRpcError {
            code: "AssetSafetyLocked".into(),
            message: "Cannot move items into Asset Safety flag".into(),
        });
    }
    if flag_u16 == 63 {
        return Err(BridgeRpcError {
            code: "ItemLocked".into(),
            message: "Cannot move items into Locked flag".into(),
        });
    }
    ctx.db.with_snapshot(|snap| {
        if let Some(moving_item) = snap.items.get(&item_id) {
            check_item_lock_impound_asset_safety_in_snap(snap, moving_item)?;
        }
        if let Some(dest_item) = snap.items.get(&new_loc) {
            check_item_lock_impound_asset_safety_in_snap(snap, dest_item)?;
        }
        Ok(())
    })?;
    let alsc_locked = read_container_ship_state(ctx)
        .alsc_locked_items
        .contains(&item_id);
    if alsc_locked {
        return Err(BridgeRpcError {
            code: "ALSCItemLocked".into(),
            message: format!("Item {item_id} is locked in an Audit Log Secure Container"),
        });
    }

    // Validate source container password and corporate container take role when moving out of a container
    let src_cont_info = ctx.db.with_snapshot(|snap| {
        let moving_item = snap.items.get(&item_id)?;
        let src_loc = moving_item.location_id;
        if src_loc == new_loc {
            return None;
        }
        let src_cont = snap.items.get(&src_loc)?;
        let src_grp = ctx.sde.types.get(&src_cont.type_id).map(|t| t.group_id).unwrap_or(0);
        if !is_container_group(src_grp, src_cont.type_id) {
            return None;
        }
        let char_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
        let cont_corp = if snap.corporations.contains_key(&src_cont.owner_id) {
            src_cont.owner_id
        } else if (116..=122).contains(&(src_cont.flag as i64)) && char_corp > 0 && moving_item.owner_id == char_corp {
            char_corp
        } else {
            0
        };
        Some((src_loc, src_cont.flag as i64, src_cont.location_id, cont_corp, char_corp, src_grp, src_cont.owner_id))
    });
    if let Some((src_loc, src_cont_flag, src_cont_station, cont_corp, char_corp, src_grp, src_cont_owner)) = src_cont_info {
        if src_grp == crate::services::mobile_deployables::GROUP_CRAB_BEACON {
            crate::services::pve_crab_beacons::check_crab_loot_access(ctx, src_loc, char_id, false)?;
        }
        if src_grp == crate::services::mobile_deployables::GROUP_MOBILE_DEPOT {
            if src_cont_owner != char_id {
                return Err(BridgeRpcError { code: "OwnershipViolation".into(), message: "Only the owner may take items from this Mobile Depot".into() });
            }
        }
        let mut st = read_container_ship_state(ctx);
        if let Some(req_pw) = st.container_passwords.get(&src_loc).cloned() {
            if !req_pw.is_empty()
                && !st.unlocked_containers.contains(&(src_loc, char_id))
                && !st.unlocked_containers.contains(&(char_id, src_loc))
            {
                let provided = kwargs.get("password").and_then(|v| v.as_str()).unwrap_or("");
                if provided != req_pw {
                    return Err(BridgeRpcError {
                        code: "ALSCPasswordNeededToOpen".into(),
                        message: format!("Password required to take items from secure container {src_loc}"),
                    });
                }
                st.unlocked_containers.insert((src_loc, char_id));
                save_container_ship_state(ctx, &st);
            }
        }
        if src_grp == crate::services::mobile_deployables::GROUP_MOBILE_DEPOT {
            crate::services::mobile_deployables::touch_mobile_depot_interaction(ctx, src_loc);
        }
        if cont_corp > 0 && cont_corp == char_corp {
            let (is_ceo, roles) = crate::services::inventory_bound::get_effective_corp_roles_at_location(
                ctx,
                cont_corp,
                char_id,
                Some(src_cont_station),
            );
            let div_idx = if (116..=122).contains(&src_cont_flag) {
                (src_cont_flag - 116) as u32
            } else {
                0
            };
            let req_container_take_role =
                eve_db::corp_roles::ROLE_CONTAINER_CAN_TAKE[(div_idx as usize).min(6)];
            if !is_ceo && (roles & (eve_db::corp_roles::ROLE_DIRECTOR | req_container_take_role)) == 0 {
                return Err(BridgeRpcError {
                    code: "CorpRoleDenied".into(),
                    message: format!(
                        "Corporation container take role denied for division {}",
                        div_idx + 1
                    ),
                });
            }
        }
    }

    let snap_check = ctx.db.with_snapshot(|snap| {
        let Some(moving_item) = snap.items.get(&item_id) else {
            return Ok(None);
        };
        if moving_item.flag == InventoryFlag::Locked || snap.is_item_locked_in_contract_or_job(item_id) {
            return Err(BridgeRpcError { code: "ItemLocked".into(), message: format!("Item {item_id} is locked by contract or job") });
        }
        let moving_type = ctx.sde.types.get(&moving_item.type_id);
        let moving_grp = moving_type.map(|t| t.group_id).unwrap_or(0);
        let moving_cat = moving_type.map(|t| t.category_id).unwrap_or(0);
        let move_qty = requested_qty.unwrap_or(moving_item.quantity as i64).max(1) as f64;
        let extra_charge_vol: f64 = if !matches!(flag_u16, 11..=34 | 92..=94 | 125..=132)
            && moving_cat != 8
            && (11..=34).contains(&(moving_item.flag as u16))
        {
            snap.items
                .values()
                .filter(|i| {
                    i.location_id == moving_item.location_id
                        && i.flag == moving_item.flag
                        && i.item_id != item_id
                        && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.category_id == 8)
                })
                .map(|i| effective_item_unit_volume(&ctx.sde, i) * (i.quantity.max(1) as f64))
                .sum()
        } else {
            0.0
        };
        let moving_total_vol = effective_item_unit_volume(&ctx.sde, moving_item) * move_qty + extra_charge_vol;

        let Some(dest_item) = snap.items.get(&new_loc) else { return Ok(None); };
        let Some(dest_type) = ctx.sde.types.get(&dest_item.type_id) else { return Ok(None); };

        // Station Container (`group 448`, unpacked volume >= 500,000 m3) cannot be moved into a ship when assembled
        if moving_grp == 448 && moving_item.singleton && dest_type.category_id == 6 {
            return Err(BridgeRpcError {
                code: "CapacityExceeded".into(),
                message: "Assembled Station Containers cannot be loaded into a ship".into(),
            });
        }
        if matches!(dest_type.group_id, 513 | 902) && dest_type.category_id == 6 && moving_item.singleton && is_container_group(moving_grp, moving_item.type_id) && moving_grp != 649 {
            return Err(BridgeRpcError {
                code: "OnlyFreightContainersAllowedInFreighters".into(),
                message: "Freight containers are the only containers allowed assembled in freighter holds".into(),
            });
        }

        // Moving into a Container (`group 12 | 340 | 448 | 649`, jetcan `23`, or Mobile Depot `1246`)
        if is_container_group(dest_type.group_id, dest_type.type_id) {
            if dest_type.group_id == crate::services::mobile_deployables::GROUP_CRAB_BEACON {
                return Err(BridgeRpcError::user_error("CannotPutItemsInCrabBeacon"));
            }
            if dest_type.group_id == 1247 {
                return Err(BridgeRpcError { code: "CannotAddToSiphon".into(), message: "Items cannot be placed into a Mobile Siphon Unit".into() });
            }
            if dest_type.group_id == crate::services::mobile_deployables::GROUP_MOBILE_DEPOT {
                if dest_item.owner_id != char_id {
                    return Err(BridgeRpcError { code: "OwnershipViolation".into(), message: "Only the owner may store items in this Mobile Depot".into() });
                }
            } else if !dest_item.singleton {
                return Err(BridgeRpcError {
                    code: "ContainerNotAssembled".into(),
                    message: format!("Target container {new_loc} must be assembled before storing items"),
                });
            }
            if is_container_group(moving_grp, moving_item.type_id) {
                return Err(BridgeRpcError {
                    code: "CannotPutContainerInContainer".into(),
                    message: "Cannot place a container inside another container".into(),
                });
            }
            let cap = dest_item
                .dynamic_attributes
                .get(&eve_sde::attr_ids::CAPACITY)
                .copied()
                .filter(|&c| c > 0.0)
                .unwrap_or_else(|| {
                    if dest_type.capacity > 0.0 {
                        dest_type.capacity
                    } else {
                        dest_type
                            .default_attributes
                            .get(&eve_sde::attr_ids::CAPACITY)
                            .copied()
                            .unwrap_or(0.0)
                    }
                });
            if cap <= 0.0 {
                return Err(BridgeRpcError {
                    code: "CapacityExceeded".into(),
                    message: format!("Container {} has no storage capacity", dest_type.name),
                });
            }
            let used: f64 = snap
                .items
                .values()
                .filter(|i| i.location_id == new_loc && i.item_id != item_id)
                .map(|i| effective_item_unit_volume(&ctx.sde, i) * (i.quantity as f64))
                .sum();
            if used + moving_total_vol > cap + 1e-4 {
                return Err(BridgeRpcError {
                    code: "CapacityExceeded".into(),
                    message: format!(
                        "Container capacity of {cap} m³ exceeded (used {used:.1} m³, adding {moving_total_vol:.1} m³)"
                    ),
                });
            }
            return Ok(Some((dest_type.group_id, moving_item.type_id, moving_item.quantity)));
        }

        // Ship Maintenance Bay (`flag 90`)
        if flag_u16 == 90 && dest_type.category_id == 6 {
            let sma_cap = dest_type
                .default_attributes
                .get(&908)
                .copied()
                .unwrap_or(0.0);
            if sma_cap <= 0.0 {
                return Err(BridgeRpcError {
                    code: "NoShipMaintenanceBay".into(),
                    message: format!("Ship {} does not have a Ship Maintenance Bay", dest_type.name),
                });
            }
            if moving_cat != 6 {
                return Err(BridgeRpcError {
                    code: "CannotStoreCargoInSMA".into(),
                    message: "Only ships can be stored in a Ship Maintenance Bay".into(),
                });
            }
            if !moving_item.singleton {
                return Err(BridgeRpcError {
                    code: "OnlyAssembledShipsInSMA".into(),
                    message: "Only assembled ships can be stored in a Ship Maintenance Bay".into(),
                });
            }
            // Reject storing ship if it contains non-charge cargo (`category_id != 8` in cargo/holds)
            let has_non_charge_cargo = snap.items.values().any(|child| {
                if child.location_id != item_id || child.quantity <= 0 {
                    return false;
                }
                let cf = child.flag as u16;
                if matches!(cf, 11..=34 | 87 | 92..=94 | 125..=132 | 158) {
                    return false;
                }
                let c_cat = ctx.sde.types.get(&child.type_id).map(|t| t.category_id).unwrap_or(0);
                c_cat != 8
            });
            if has_non_charge_cargo {
                return Err(BridgeRpcError {
                    code: "ShipHasNonChargeCargo".into(),
                    message: "Cannot store ship in Ship Maintenance Bay while it carries cargo other than charges/ammo".into(),
                });
            }
            let used_sma: f64 = snap
                .items
                .values()
                // Stored hulls carry `invFlagShipHangar` (90) or `SpecializedShipHold` (138, set by `ship.StoreVessel`).
                .filter(|i| i.location_id == new_loc && matches!(i.flag as u16, 90 | 138) && i.item_id != item_id)
                .map(|i| effective_item_unit_volume(&ctx.sde, i))
                .sum();
            if used_sma + moving_total_vol > sma_cap + 1e-4 {
                return Err(BridgeRpcError {
                    code: "CapacityExceeded".into(),
                    message: format!("Ship Maintenance Bay capacity {sma_cap} m³ exceeded"),
                });
            }
            let supplied_pw = kwargs.get("password").and_then(|v| v.as_str());
            if !can_character_access_ship_bay_with_password_snap(snap, char_id, new_loc, dest_item.owner_id, true, supplied_pw) {
                return Err(BridgeRpcError {
                    code: "SMAAccessDenied".into(),
                    message: "Ship Maintenance Bay access denied by ship configuration or password".into(),
                });
            }
        }

        // Fleet Hangar (`flag 155`)
        if flag_u16 == 155 && dest_type.category_id == 6 {
            let fh_cap = dest_type
                .default_attributes
                .get(&912)
                .copied()
                .unwrap_or(0.0);
            if fh_cap <= 0.0 {
                return Err(BridgeRpcError {
                    code: "NoFleetHangar".into(),
                    message: format!("Ship {} does not have a Fleet Hangar", dest_type.name),
                });
            }
            let used_fh: f64 = snap
                .items
                .values()
                .filter(|i| i.location_id == new_loc && (i.flag as u16) == 155 && i.item_id != item_id)
                .map(|i| effective_item_unit_volume(&ctx.sde, i) * (i.quantity as f64))
                .sum();
            if used_fh + moving_total_vol > fh_cap + 1e-4 {
                return Err(BridgeRpcError {
                    code: "CapacityExceeded".into(),
                    message: format!("Fleet Hangar capacity {fh_cap} m³ exceeded"),
                });
            }
            let supplied_pw = kwargs.get("password").and_then(|v| v.as_str());
            if !can_character_access_ship_bay_with_password_snap(snap, char_id, new_loc, dest_item.owner_id, false, supplied_pw) {
                return Err(BridgeRpcError {
                    code: "FleetHangarAccessDenied".into(),
                    message: "Fleet Hangar access denied by ship configuration or password".into(),
                });
            }
        }

        Ok(None)
    })?;

    if let Some(supplied_pw) = kwargs.get("password").and_then(|v| v.as_str()) {
        write_container_ship_state(ctx, |st| {
            if let Some(cfg) = st.ship_configs.get(&new_loc) {
                if cfg.password.as_deref() == Some(supplied_pw) {
                    st.unlocked_containers.insert((char_id, new_loc));
                }
            }
        });
    }

    if let Some((grp, _, _)) = snap_check {
        let mut st = read_container_ship_state(ctx);
        let cfg = st.container_configs.get(&new_loc).cloned().unwrap_or_default();
        let need_check = cfg.password_per_action || !st.unlocked_containers.contains(&(new_loc, char_id));
        if let Some(req_pw) = st.container_passwords.get(&new_loc).cloned() {
            if !req_pw.is_empty() && need_check {
                let provided = kwargs.get("password").and_then(|v| v.as_str()).unwrap_or("");
                let now_unix = ctx.now_unix_secs();
                let tid = ctx.db.with_snapshot(|s| s.items.get(&new_loc).map(|i| i.type_id)).unwrap_or(0);
                if provided != req_pw {
                    st.alsc_logs.entry(new_loc).or_default().push(AlscLogEntry { timestamp_unix: now_unix, actor_id: char_id, action: "ALSCActionPasswordFailure".into(), item_id: new_loc, item_type_id: tid, quantity: 1 });
                    save_container_ship_state(ctx, &st);
                    return Err(BridgeRpcError { code: "ALSCPasswordNeededToOpen".into(), message: format!("Password required to access secure container {new_loc}") });
                }
                st.alsc_logs.entry(new_loc).or_default().push(AlscLogEntry { timestamp_unix: now_unix, actor_id: char_id, action: "ALSCActionPasswordSuccess".into(), item_id: new_loc, item_type_id: tid, quantity: 1 });
                st.unlocked_containers.insert((new_loc, char_id));
                save_container_ship_state(ctx, &st);
            }
        }
        if grp == crate::services::mobile_deployables::GROUP_MOBILE_DEPOT {
            crate::services::mobile_deployables::touch_mobile_depot_interaction(ctx, new_loc);
        }
    }

    Ok(())
}

pub async fn validate_single_item_can_be_opened(
    ctx: &ServerContext,
    char_id: i64,
    item_id: i64,
) -> Result<(), BridgeRpcError> {
    if item_id <= 0 {
        return Err(BridgeRpcError { code: "InvalidArgument".into(), message: "Valid itemID required".into() });
    }
    let (item_type_id, owner_id, location_id, singleton) = ctx.db.with_snapshot(|snap| {
        let it = snap.items.get(&item_id).ok_or_else(|| BridgeRpcError {
            code: "ItemNotFound".into(),
            message: format!("Container item {item_id} not found"),
        })?;
        Ok::<_, BridgeRpcError>((it.type_id, it.owner_id, it.location_id, it.singleton))
    })?;
    if !singleton {
        return Err(BridgeRpcError {
            code: "CantOpenPackagedItem".into(),
            message: format!("Item {item_id} is packaged and cannot be opened. Assemble it first."),
        });
    }
    let grp = ctx.sde.types.get(&item_type_id).map(|t| t.group_id).unwrap_or(0);
    let cat = ctx.sde.types.get(&item_type_id).map(|t| t.category_id).unwrap_or(0);
    if !(is_container_group(grp, item_type_id) || grp == 186 || cat == 6 || cat == 2 || item_type_id == 23 || grp == 1246 || grp == 1250) {
        return Err(BridgeRpcError {
            code: "NotAContainer".into(),
            message: format!("Item {item_id} (type {item_type_id}) is not a container"),
        });
    }
    if grp == crate::services::mobile_deployables::GROUP_CRAB_BEACON {
        crate::services::pve_crab_beacons::check_crab_loot_access(ctx, item_id, char_id, false)?;
    } else if (grp == 1246 || grp == 1250) && owner_id != char_id {
        return Err(BridgeRpcError {
            code: "OwnershipViolation".into(),
            message: "Only the owner may access this structure's inventory".into(),
        });
    }
    if grp == 1246 {
        crate::services::mobile_deployables::touch_mobile_depot_interaction(ctx, item_id);
    }
    crate::services::character_skill_ops::verify_in_space_container_proximity(ctx, char_id, location_id, item_id).await?;
    let st = read_container_ship_state(ctx);
    if st.container_passwords.get(&item_id).is_some_and(|p| !p.is_empty())
        && !st.unlocked_containers.contains(&(item_id, char_id))
    {
        return Err(BridgeRpcError {
            code: "ContainerIsLocked".into(),
            message: format!("Container {item_id} is password-locked. Enter password to unlock."),
        });
    }
    Ok(())
}

fn handle_alsc_lock_unlock(
    ctx: &ServerContext,
    session: &SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let is_lock = method.contains("ALSCLock");
    let rpc = crate::router::RpcArgs::new(args, kwargs);
    let container_id = rpc
        .opt_positive_i64(0, "containerID")?
        .or_else(|| bound_entry.map(|b| b.object_id))
        .unwrap_or(0);
    if container_id <= 0 {
        return Err(BridgeRpcError { code: "InvalidContainer".into(), message: "Valid containerID required for ALSC lock/unlock".into() });
    }
    let item_ids: Vec<i64> = match args.get(1).or_else(|| args.first()) {
        Some(PyRep::List(v) | PyRep::Tuple(v)) => v.iter().filter_map(|x| x.as_i64()).collect(),
        Some(PyRep::Integer(id)) if *id != container_id => vec![*id],
        _ => kwargs.get("itemIDs").and_then(|v| match v { PyRep::List(l) | PyRep::Tuple(l) => Some(l.iter().filter_map(|x| x.as_i64()).collect()), _ => None }).unwrap_or_default(),
    };
    let now_unix = ctx.now_unix_secs();
    let corp_for_promote = ctx
        .db
        .with_snapshot(|snap| snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0));
    if corp_for_promote > 0 {
        crate::services::fleet_social::fleet_social_corps::promote_due_pending_corp_grants(
            ctx,
            corp_for_promote,
        );
    }
    ctx.db.with_snapshot(|snap| {
        let cont = snap.items.get(&container_id).ok_or_else(|| BridgeRpcError { code: "ContainerNotFound".into(), message: format!("Container {container_id} not found") })?;
        let corp_id = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
        let ch_loc = snap.characters.get(&char_id).and_then(|c| c.station_id.or(c.structure_id).or(Some(c.solar_system_id as i64))).unwrap_or(0);
        let ch_ship = snap.characters.get(&char_id).map(|c| c.active_ship_id).unwrap_or(0);

        let (mut root_loc, mut root_flag) = (cont.location_id, cont.flag);
        for _ in 0..8 {
            if (60_000_000..=69_999_999).contains(&root_loc)
                || root_loc >= 1_000_000_000_000
                || snap.structures.contains_key(&root_loc)
            {
                break;
            }
            let Some(parent) = snap.items.get(&root_loc) else {
                break;
            };
            if parent.type_id == 27 {
                root_loc = parent.location_id;
                break;
            }
            root_loc = parent.location_id;
            root_flag = parent.flag;
        }
        if cont.location_id != ch_loc && cont.location_id != ch_ship && root_loc != ch_loc {
            return Err(BridgeRpcError { code: "ColocationRequired".into(), message: format!("Must be co-located with container {container_id} to lock/unlock items") });
        }
        let is_authorized = if cont.owner_id == char_id {
            true
        } else if corp_id > 0 && cont.owner_id == corp_id {
            let (is_ceo, role_mask) =
                crate::services::inventory_bound::get_effective_corp_roles_at_location_in_snap_at(
                    snap,
                    corp_id,
                    char_id,
                    Some(root_loc),
                    now_unix,
                );
            let div_cont_take = if (116..=122).contains(&(root_flag as i64)) {
                eve_db::corp_roles::ROLE_CONTAINER_CAN_TAKE[(root_flag as usize) - 116]
            } else {
                0
            };
            is_ceo
                || eve_db::corp_roles::has_role(role_mask, eve_db::corp_roles::ROLE_DIRECTOR)
                || eve_db::corp_roles::has_role(role_mask, eve_db::corp_roles::ROLE_EQUIPMENT_CONFIG)
                || (div_cont_take != 0 && eve_db::corp_roles::has_role(role_mask, div_cont_take))
        } else {
            false
        };
        if !is_authorized {
            return Err(BridgeRpcError { code: "ALSCAccessDenied".into(), message: format!("Not authorized to lock/unlock items in container {container_id}") });
        }
        for &iid in &item_ids {
            let it = snap.items.get(&iid).ok_or_else(|| BridgeRpcError { code: "ItemNotFound".into(), message: format!("Item {iid} not found") })?;
            if it.location_id != container_id {
                return Err(BridgeRpcError { code: "ItemNotInContainer".into(), message: format!("Item {iid} is not located inside container {container_id}") });
            }
        }
        Ok(())
    })?;
    let mut st = read_container_ship_state(ctx);
    let req_pw = st.container_passwords.get(&container_id).cloned().unwrap_or_default();
    if !req_pw.is_empty() {
        let cfg = st.container_configs.get(&container_id).cloned().unwrap_or_default();
        let need_check = cfg.password_per_action || !st.unlocked_containers.contains(&(container_id, char_id));
        if need_check {
            let provided = rpc.opt_str(2, "password")?.unwrap_or("");
            let tid = ctx.db.with_snapshot(|s| s.items.get(&container_id).map(|i| i.type_id)).unwrap_or(0);
            if provided != req_pw {
                st.alsc_logs.entry(container_id).or_default().push(AlscLogEntry { timestamp_unix: now_unix, actor_id: char_id, action: "ALSCActionPasswordFailure".into(), item_id: container_id, item_type_id: tid, quantity: 1 });
                save_container_ship_state(ctx, &st);
                return Err(BridgeRpcError { code: if is_lock { "ALSCPasswordNeededToLock".into() } else { "ALSCPasswordNeededToUnlock".into() }, message: "Container password required".into() });
            }
            st.alsc_logs.entry(container_id).or_default().push(AlscLogEntry { timestamp_unix: now_unix, actor_id: char_id, action: "ALSCActionPasswordSuccess".into(), item_id: container_id, item_type_id: tid, quantity: 1 });
            st.unlocked_containers.insert((container_id, char_id));
        }
    }
    for &iid in &item_ids {
        if is_lock { st.alsc_locked_items.insert(iid); } else { st.alsc_locked_items.remove(&iid); }
        let (tid, qty, owner_id, loc_id) = ctx.db.with_snapshot(|s| s.items.get(&iid).map(|i| (i.type_id, i.quantity, i.owner_id, i.location_id))).unwrap_or((0, 1, char_id, container_id));
        st.alsc_logs.entry(container_id).or_default().push(AlscLogEntry {
            timestamp_unix: now_unix, actor_id: char_id, action: if is_lock { "ALSCActionLock".into() } else { "ALSCActionUnlock".into() }, item_id: iid, item_type_id: tid, quantity: qty,
        });
        ctx.db.with_mut_snapshot(|snap| {
            if let Some((corp_id, c_tid, c_loc, loc_flag)) = resolve_corp_container_log_target(snap, container_id) {
                snap.corp_container_logs.entry(corp_id).or_default().push(eve_db::CorpContainerLogEntry {
                    logged_at_unix: now_unix, container_id, container_type_id: c_tid, character_id: char_id,
                    location_id: c_loc, location_flag: loc_flag, action: if is_lock { "lock".into() } else { "unlock".into() },
                    password_type: None, type_id: Some(tid as i32), quantity: Some(qty), old_config_bitmask: None, new_config_bitmask: None,
                });
            }
        });
        ctx.notify(crate::router::NotificationTarget::Character(char_id), "OnLockedItemChange", "charid", vec![PyRep::Integer(iid), PyRep::Integer(owner_id), PyRep::Integer(loc_id)]);
    }
    save_container_ship_state(ctx, &st);
    Ok(PyRep::keyval([("containerID", PyRep::Integer(container_id)), ("locked", PyRep::Bool(is_lock)), ("count", PyRep::Integer(item_ids.len() as i64))]))
}

async fn handle_validate_item_can_be_opened(
    ctx: &ServerContext, session: &SessionState, bound_entry: Option<&BoundObjectEntry>,
    args: &[PyRep], kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
    let item_id = match crate::router::RpcArgs::new(args, kwargs).opt_positive_i64(0, "itemID") {
        Ok(v) => v.or_else(|| bound_entry.map(|b| b.object_id)).unwrap_or(0),
        Err(e) => return Some(Err(e)),
    };
    match validate_single_item_can_be_opened(ctx, char_id, item_id).await {
        Ok(()) => Some(Ok(PyRep::Bool(true))),
        Err(e) => Some(Err(e)),
    }
}

pub(crate) async fn handle_containers_ships_and_fitting(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("invbroker", "ValidateItemCanBeOpened") => {
            handle_validate_item_can_be_opened(ctx, session, bound_entry, args, kwargs).await
        }

        ("invbroker", "ValidateItemListCanBeOpened") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let item_ids: Vec<i64> = match args.first().or_else(|| kwargs.get("itemIDs")) {
                Some(PyRep::List(l) | PyRep::Tuple(l)) => l.iter().filter_map(|x| x.as_i64()).collect(),
                Some(PyRep::Integer(id)) => vec![*id],
                _ => Vec::new(),
            };
            if item_ids.is_empty() {
                return Some(Err(BridgeRpcError { code: "InvalidArgument".into(), message: "ValidateItemListCanBeOpened requires a list of itemIDs".into() }));
            }
            for &id in &item_ids {
                if let Err(e) = validate_single_item_can_be_opened(ctx, char_id, id).await {
                    return Some(Err(e));
                }
            }
            Some(Ok(PyRep::Bool(true)))
        }

        ("invbroker", "AssembleCargoContainer") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(item_id) = args.first().and_then(|a| a.as_i64()).or_else(|| bound_entry.map(|b| b.object_id)) else {
                return Some(Err(BridgeRpcError { code: "InvalidArgument".into(), message: "AssembleCargoContainer requires container itemID".into() }));
            };
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(item) = snap.items.get(&item_id).cloned() else {
                    return Err(BridgeRpcError { code: "ItemNotFound".into(), message: format!("Container {item_id} not found") });
                };
                let corp_id = session.corporation_id.unwrap_or(0);
                if item.owner_id != char_id {
                    let is_ceo = snap.corporations.get(&corp_id).map(|c| c.ceo_id == char_id).unwrap_or(false);
                    if corp_id == 0 || item.owner_id != corp_id {
                        return Err(BridgeRpcError { code: "OwnershipViolation".into(), message: format!("Container {item_id} is not owned by {char_id}") });
                    }
                    if session.corp_role == 0 && !is_ceo {
                        return Err(BridgeRpcError { code: "CorpRoleRequired".into(), message: "Corporation role required to assemble corp container".into() });
                    }
                }
                let caller_loc = session.station_id.or(session.structure_id).or_else(|| session.solar_system_id.map(i64::from));
                if Some(item.location_id) != caller_loc && session.ship_id != Some(item.location_id) {
                    return Err(BridgeRpcError { code: "ColocationRequired".into(), message: "Character must be colocated with container to assemble it".into() });
                }
                let grp = ctx.sde.types.get(&item.type_id).map(|t| t.group_id).unwrap_or(0);
                if !is_container_group(grp, item.type_id) {
                    return Err(BridgeRpcError { code: "NotAContainer".into(), message: format!("Item {item_id} (group {grp}) is not a cargo container") });
                }
                if !item.singleton && item.quantity > 1 {
                    if let Some(orig) = snap.items.get_mut(&item_id) {
                        orig.quantity -= 1;
                        orig.version += 1;
                    }
                    snap.next_entity_id += 1;
                    let new_id = snap.next_entity_id;
                    snap.items.insert(new_id, eve_db::InventoryItemRecord {
                        item_id: new_id, type_id: item.type_id, owner_id: item.owner_id, location_id: item.location_id,
                        flag: item.flag, quantity: 1, singleton: true, version: 1, ..Default::default()
                    });
                    Ok((new_id, item.type_id, 1_u64))
                } else {
                    let it = snap.items.get_mut(&item_id).unwrap();
                    it.singleton = true;
                    it.version += 1;
                    Ok((item_id, it.type_id, it.version))
                }
            });
            match res {
                Ok((cid, tid, ver)) => {
                    let now_unix = ctx.now_unix_secs();
                    write_container_ship_state(ctx, |st| {
                        st.alsc_logs.entry(cid).or_default().push(AlscLogEntry {
                            timestamp_unix: now_unix, actor_id: char_id, action: "ALSCActionAssemble".into(), item_id: cid, item_type_id: tid, quantity: 1,
                        });
                    });
                    ctx.db.with_mut_snapshot(|snap| {
                        if let Some((corp_id, _, loc_id, loc_flag)) = resolve_corp_container_log_target(snap, cid) {
                            snap.corp_container_logs.entry(corp_id).or_default().push(eve_db::CorpContainerLogEntry {
                                logged_at_unix: now_unix, container_id: cid, container_type_id: tid as i32, character_id: char_id,
                                location_id: loc_id, location_flag: loc_flag, action: "assemble".into(), password_type: None,
                                type_id: None, quantity: None, old_config_bitmask: None, new_config_bitmask: None,
                            });
                        }
                    });
                    Some(Ok(PyRep::keyval([("itemID", PyRep::Integer(cid)), ("typeID", PyRep::Integer(tid as i64)), ("singleton", PyRep::Bool(true)), ("version", PyRep::Integer(ver as i64))])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("invbroker", "SetPassword") | ("invbroker", "ALSCConfigSet") | ("invbroker", "ConfigureALSC") => {
            Some(crate::services::notify_emitters_fleet_combat::handle_set_password(ctx, session, bound_entry, method, args, kwargs).await)
        }

        ("invbroker", "ALSCConfigGet") => {
            let (_, _char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let rpc = crate::router::RpcArgs::new(args, kwargs);
            let container_id = match rpc.opt_positive_i64(0, "containerID") {
                Ok(v) => v.or_else(|| bound_entry.map(|b| b.object_id)).unwrap_or(0),
                Err(e) => return Some(Err(e)),
            };
            let st = read_container_ship_state(ctx);
            let has_pw = st.get_container_password(container_id, 1).is_some() || st.get_container_password(container_id, 2).is_some();
            let cfg = st.container_configs.get(&container_id).cloned().unwrap_or_default();
            Some(Ok(PyRep::keyval([
                ("containerID", PyRep::Integer(container_id)), ("passwordRequired", PyRep::Bool(has_pw)),
                ("hasPassword", PyRep::Bool(has_pw)), ("lockByDefault", PyRep::Bool(cfg.lock_by_default)),
                ("passwordPerAction", PyRep::Bool(cfg.password_per_action)), ("configBitmask", PyRep::Integer(cfg.config_bitmask as i64)),
            ])))
        }

        ("invbroker", "ALSCLockItems") => {
            Some(handle_alsc_lock_unlock(ctx, session, bound_entry, method, args, kwargs))
        }
        ("invbroker", "ALSCLock") => {
            Some(handle_alsc_lock_unlock(ctx, session, bound_entry, method, args, kwargs))
        }
        ("invbroker", "ALSCUnlockItems") => {
            Some(handle_alsc_lock_unlock(ctx, session, bound_entry, method, args, kwargs))
        }
        ("invbroker", "ALSCUnlock") => {
            Some(handle_alsc_lock_unlock(ctx, session, bound_entry, method, args, kwargs))
        }

        ("invbroker", "ALSCLogGet") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let rpc = crate::router::RpcArgs::new(args, kwargs);
            let container_id = match rpc.opt_positive_i64(0, "containerID") {
                Ok(v) => v.or_else(|| bound_entry.map(|b| b.object_id)).unwrap_or(0),
                Err(e) => return Some(Err(e)),
            };
            let auth_ok = ctx.db.with_snapshot(|snap| {
                if let Some(cont) = snap.items.get(&container_id) {
                    let corp_id = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
                    // Owner, corp-owned container member, or a corp-mate of the owning pilot (who
                    // must still pass the ALSC password check below); outsiders are denied outright.
                    let owner_corp = snap.characters.get(&cont.owner_id).map(|c| c.corporation_id).unwrap_or(0);
                    cont.owner_id == char_id || (corp_id > 0 && (cont.owner_id == corp_id || owner_corp == corp_id))
                } else {
                    true
                }
            });
            if !auth_ok {
                return Some(Err(BridgeRpcError { code: "ALSCAccessDenied".into(), message: format!("Not authorized to view audit log for container {container_id}") }));
            }
            let mut st = read_container_ship_state(ctx);
            let cfg = st.container_configs.get(&container_id).cloned().unwrap_or_default();
            let need_check = cfg.password_per_action || !st.unlocked_containers.contains(&(container_id, char_id));
            if let Some(req_pw) = st.container_passwords.get(&container_id).cloned() {
                if !req_pw.is_empty() && need_check {
                    let provided = kwargs.get("password").and_then(|v| v.as_str()).or_else(|| args.get(1).and_then(|a| a.as_str())).unwrap_or("");
                    let now_unix = ctx.now_unix_secs();
                    let tid = ctx.db.with_snapshot(|s| s.items.get(&container_id).map(|i| i.type_id)).unwrap_or(0);
                    if provided != req_pw {
                        st.alsc_logs.entry(container_id).or_default().push(AlscLogEntry { timestamp_unix: now_unix, actor_id: char_id, action: "ALSCActionPasswordFailure".into(), item_id: container_id, item_type_id: tid, quantity: 1 });
                        save_container_ship_state(ctx, &st);
                        return Some(Err(BridgeRpcError { code: "ALSCPasswordNeededToViewAuditLog".into(), message: "Container password required to view audit log".into() }));
                    }
                    st.alsc_logs.entry(container_id).or_default().push(AlscLogEntry { timestamp_unix: now_unix, actor_id: char_id, action: "ALSCActionPasswordSuccess".into(), item_id: container_id, item_type_id: tid, quantity: 1 });
                    st.unlocked_containers.insert((container_id, char_id));
                    save_container_ship_state(ctx, &st);
                }
            }
            let entries: Vec<PyRep> = st.alsc_logs.get(&container_id).cloned().unwrap_or_default().into_iter()
                .map(|e| PyRep::keyval([("timestampUnix", PyRep::Integer(e.timestamp_unix as i64)), ("actorID", PyRep::Integer(e.actor_id)), ("action", PyRep::String(e.action)), ("itemID", PyRep::Integer(e.item_id)), ("typeID", PyRep::Integer(e.item_type_id as i64)), ("quantity", PyRep::Integer(e.quantity as i64))])).collect();
            Some(Ok(PyRep::keyval([("containerID", PyRep::Integer(container_id)), ("logCount", PyRep::Integer(entries.len() as i64)), ("entries", PyRep::List(entries))])))
        }

        ("invbroker", "GetDamageForCrystals") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let entries: Vec<(PyRep, PyRep)> = ctx.db.with_snapshot(|snap| {
                snap.items
                    .values()
                    .filter(|i| {
                        i.owner_id == char_id
                            && ctx
                                .sde
                                .types
                                .get(&i.type_id)
                                // SDE `crystalsGetDamaged` (786): frequency/mining crystals that wear out.
                                .is_some_and(|t| t.default_attributes.get(&786).is_some_and(|&v| v > 0.0))
                    })
                    .map(|i| {
                        let dmg = i
                            .dynamic_attributes
                            .get(&attr_ids::DAMAGE)
                            .copied()
                            .or_else(|| {
                                i.dynamic_attributes
                                    .get(&attr_ids::HP_STRUCT)
                                    .map(|&hp| (1.0 - hp).clamp(0.0, 1.0))
                            })
                            .unwrap_or(0.0);
                        (PyRep::Integer(i.item_id), PyRep::Real(dmg))
                    })
                    .collect()
            });
            Some(Ok(PyRep::Dict(entries)))
        }

        ("invbroker", "GetAllItems") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let search_filter = args
                .first()
                .and_then(|a| a.as_str())
                .or_else(|| kwargs.get("search").and_then(|v| v.as_str()))
                .map(|s| s.to_lowercase());
            let loc_filter = kwargs.get("locationID").and_then(|v| v.as_i64());
            let items = ctx.db.get_items_for_character(char_id);
            let mut rows = Vec::new();
            let mut total_volume = 0.0;
            let mut total_value_isk = 0.0;
            let mut locations = HashSet::new();
            for item in items {
                if let Some(lf) = loc_filter {
                    if item.location_id != lf {
                        continue;
                    }
                }
                let type_info = ctx.sde.types.get(&item.type_id);
                if let Some(ref q) = search_filter {
                    let name_matches = type_info
                        .map(|t| t.name.to_lowercase().contains(q))
                        .unwrap_or(false);
                    if !name_matches {
                        continue;
                    }
                }
                locations.insert(item.location_id);
                let unit_vol = effective_item_unit_volume(&ctx.sde, &item);
                total_volume += unit_vol * (item.quantity as f64);
                total_value_isk += type_info.map(|t| t.base_price).unwrap_or(100.0) * (item.quantity as f64);
                rows.push(item_to_packed_cells(ctx, &item));
            }
            let count = rows.len() as i64;
            Some(Ok(PyRep::keyval([
                ("totalItems", PyRep::Integer(count)),
                ("locationCount", PyRep::Integer(locations.len() as i64)),
                ("totalVolumeM3", PyRep::Real(total_volume)),
                ("estimatedNetWorthISK", PyRep::Real(total_value_isk)),
                ("items", DBRowDescriptor::inv_item_header().build_crowset(rows)),
            ])))
        }

        ("invbroker", "IsMyItemAtDockableLocation") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let item_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let dock_id = session.station_id.or(session.structure_id);
            let at_dock = ctx.db.with_snapshot(|snap| {
                snap.items
                    .get(&item_id)
                    .is_some_and(|i| i.owner_id == char_id && Some(i.location_id) == dock_id)
            });
            Some(Ok(PyRep::Bool(at_dock)))
        }

        ("invbroker", "BreakPlasticWrap") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let wrap_id = args
                .first()
                .and_then(|a| a.as_i64())
                .or_else(|| bound_entry.map(|b| b.object_id))
                .unwrap_or(0);
            let sess_dock = session.station_id.or(session.structure_id);
            let sess_ship = session.ship_id.filter(|&id| id > 0);
            let now_unix = ctx.now_unix();
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(wrap) = snap.items.get(&wrap_id).cloned() else {
                    return Err(BridgeRpcError { code: "ItemNotFound".into(), message: format!("Plastic Wrap {wrap_id} not found") });
                };
                if wrap.type_id != 3468 {
                    return Err(BridgeRpcError { code: "InvalidPlasticWrap".into(), message: format!("Item {wrap_id} is not a Plastic Wrap") });
                }
                let ch_opt = snap.characters.get(&char_id);
                let eff_dock = sess_dock.or_else(|| if sess_ship.is_none() { ch_opt.and_then(|c| c.station_id.or(c.structure_id)) } else { None });
                let eff_ship = sess_ship.or_else(|| ch_opt.map(|c| c.active_ship_id).filter(|&id| id > 0));
                let colocated = if let Some(dock_id) = eff_dock {
                    wrap.location_id == dock_id
                        || (eff_ship == Some(wrap.location_id)
                            && snap.items.get(&wrap.location_id).is_some_and(|sh| sh.location_id == dock_id))
                } else {
                    eff_ship == Some(wrap.location_id)
                };
                if !colocated {
                    return Err(BridgeRpcError {
                        code: "ItemNotAtStation".into(),
                        message: format!("Plastic Wrap {wrap_id} is at {} which is not co-located with character {char_id}", wrap.location_id),
                    });
                }
                let has_courier = snap.contracts.values().any(|c| {
                    c.contract_type == "Courier"
                        && c.status == "InProgress"
                        && c.item_ids.iter().any(|iid| snap.items.get(iid).is_some_and(|it| it.location_id == wrap_id))
                });
                if has_courier {
                    let (cid, issuer_id, extracted) =
                        crate::services::contracts_service::break_courier_plastic_wrap(snap, char_id, wrap_id, now_unix)?;
                    let changed: Vec<_> = extracted.iter().filter_map(|iid| snap.items.get(iid).cloned()).collect();
                    return Ok((Some((cid, issuer_id)), wrap.location_id, wrap.flag, changed));
                }
                if wrap.owner_id != char_id {
                    return Err(BridgeRpcError { code: "NotItemOwner".into(), message: format!("Character {char_id} does not own Plastic Wrap {wrap_id}") });
                }
                let parent_loc = wrap.location_id;
                let parent_flag = wrap.flag;
                let mut changed = Vec::new();
                for child in snap.items.values_mut() {
                    if child.location_id == wrap_id {
                        child.location_id = parent_loc;
                        child.flag = parent_flag;
                        child.version += 1;
                        changed.push(child.clone());
                    }
                }
                snap.items.remove(&wrap_id);
                Ok((None, parent_loc, parent_flag, changed))
            });
            match res {
                Ok((failed_courier, prev_loc, prev_flag, changed)) => {
                    if let Some((cid, issuer_id)) = failed_courier {
                        crate::services::contracts_service::notify_contract(ctx, issuer_id, "OnContractFailed", cid);
                    }
                    for item in &changed {
                        let (group_id, category_id) = ctx
                            .sde
                            .types
                            .get(&item.type_id)
                            .map(|t| (t.group_id, t.category_id))
                            .unwrap_or((0, 0));
                        ctx.notify(
                            crate::router::NotificationTarget::Character(char_id),
                            "OnItemChange",
                            "charid",
                            vec![
                                PyRep::keyval([
                                    ("itemID", PyRep::Integer(item.item_id)),
                                    ("typeID", PyRep::Integer(item.type_id as i64)),
                                    ("ownerID", PyRep::Integer(item.owner_id)),
                                    ("locationID", PyRep::Integer(item.location_id)),
                                    ("flagID", PyRep::Integer(item.flag as i64)),
                                    (
                                        "quantity",
                                        PyRep::Integer(if item.singleton {
                                            -1
                                        } else {
                                            item.quantity as i64
                                        }),
                                    ),
                                    ("groupID", PyRep::Integer(i64::from(group_id))),
                                    ("categoryID", PyRep::Integer(i64::from(category_id))),
                                    ("singleton", PyRep::Bool(item.singleton)),
                                ]),
                                PyRep::Dict(vec![
                                    (PyRep::Integer(3), PyRep::Integer(wrap_id)),
                                    (PyRep::Integer(4), PyRep::Integer(prev_flag as i64)),
                                ]),
                            ],
                        );
                    }
                    let _ = prev_loc;
                    Some(Ok(PyRep::None))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("invbroker", "ClearCharges") => {
            Some(super::fitting_containers_ships_ops::handle_clear_charges(ctx, session, bound_entry, args).await)
        }

        ("invbroker", "DestroyFitting") => {
            return super::fitting_dogma_im::handle_dogma_im(
                ctx,
                session,
                None,
                "dogmaIM",
                "DestroyFitting",
                args,
                kwargs,
            )
            .await;
        }

        _ => super::fitting_containers_ships_ops::handle_ext_tail(ctx, session, bound_entry, service, method, args, kwargs).await,
    }
}

/// Strictly validates item ownership, ship ownership (`category_id == 6`), and co-location
/// (docked at the same fitting-capable station/structure, or actively piloting the ship near an
/// in-space fitting service without a weapons timer) before unfitting or destroying a ship module/rig.
pub(crate) async fn validate_ship_module_unfit_auth(
    ctx: &ServerContext,
    session: &SessionState,
    char_id: i64,
    src_item: &eve_db::InventoryItemRecord,
    new_loc: i64,
) -> Result<(), BridgeRpcError> {
    let in_space_ship = ctx.db.with_snapshot(|snap| -> Result<Option<(u32, i64)>, BridgeRpcError> {
        if src_item.owner_id != char_id {
            return Err(BridgeRpcError {
                code: "ItemOwnershipViolation".into(),
                message: format!("Item {} is not owned by {char_id}", src_item.item_id),
            });
        }
        if src_item.flag == InventoryFlag::Locked
            || snap.is_item_locked_in_contract_or_job(src_item.item_id)
        {
            return Err(BridgeRpcError {
                code: "ItemLocked".into(),
                message: format!("Item {} is locked and cannot be unfitted", src_item.item_id),
            });
        }
        let Some(ship_item) = snap.items.get(&src_item.location_id) else {
            return Err(BridgeRpcError {
                code: "ShipNotFound".into(),
                message: format!("Source ship {} not found", src_item.location_id),
            });
        };
        let ship_cat = ctx.sde.types.get(&ship_item.type_id).map(|t| t.category_id).unwrap_or(0);
        if ship_item.owner_id != char_id || ship_cat != 6 {
            return Err(BridgeRpcError {
                code: "ShipOwnershipViolation".into(),
                message: format!("Source {} is not a ship owned by {char_id}", src_item.location_id),
            });
        }
        let (char_dock, char_sys, active_ship_id) = snap
            .characters
            .get(&char_id)
            .map(|c| (c.station_id.or(c.structure_id).unwrap_or(0), c.solar_system_id as i64, c.active_ship_id))
            .unwrap_or((0, 0, 0));
        let ch_dock = if let Some(d) = session.station_id.or(session.structure_id) {
            d
        } else if session.solar_system_id.is_some() {
            0
        } else {
            char_dock
        };
        let ch_sys = if char_sys > 0 { char_sys } else { session.solar_system_id.map(|s| s as i64).unwrap_or(0) };
        let resolves_to_loc = |start_loc: i64, target_loc: i64| -> bool {
            let mut cur = start_loc;
            for _ in 0..4 {
                if cur == target_loc {
                    return true;
                }
                let Some(node) = snap.items.get(&cur) else { return false };
                cur = node.location_id;
            }
            false
        };
        if ch_dock > 0 {
            crate::services::structure_acl_defense_rules::validate_docked_ship_fitting_facility_in_snap(ctx, snap, ch_dock)?;
            if !resolves_to_loc(ship_item.location_id, ch_dock) {
                return Err(BridgeRpcError { code: "InvalidFittingTarget".into(), message: format!("Ship {} is not docked at current location {ch_dock}", ship_item.item_id) });
            }
            if new_loc != ship_item.item_id && !resolves_to_loc(new_loc, ch_dock) {
                return Err(BridgeRpcError { code: "InvalidItemLocation".into(), message: format!("Destination {new_loc} is not at current dock {ch_dock}") });
            }
            Ok(None)
        } else {
            let caller_ship = session.ship_id.filter(|&id| id > 0).unwrap_or(active_ship_id);
            if ship_item.item_id != caller_ship || (ch_sys > 0 && ship_item.location_id != ch_sys) {
                return Err(BridgeRpcError { code: "InvalidFittingTarget".into(), message: format!("Pilot {char_id} must be aboard ship {} in system {ch_sys} to unfit in space", ship_item.item_id) });
            }
            Ok(Some((session.solar_system_id.unwrap_or(ch_sys.max(0) as u32), caller_ship)))
        }
    })?;
    if let Some((sys_id, ship_id)) = in_space_ship {
        let now_unix = ctx.now_unix_secs();
        if ctx.crimewatch.read().unwrap_or_else(|e| e.into_inner()).states.get(&char_id).is_some_and(|cw| cw.is_weapon_timer_active(now_unix)) {
            return Err(BridgeRpcError { code: "CannotRefitWithWeaponsTimer".into(), message: "Cannot refit or unfit modules in space while weapons timer is active".into() });
        }
        if sys_id == 0 || !super::is_near_in_space_fitting_service(ctx, sys_id, ship_id).await {
            return Err(BridgeRpcError { code: "CannotUnfitInSpace".into(), message: "Unfitting modules requires being docked at a station/structure or within range of a Mobile Depot or capital SMA".into() });
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "fitting_containers_ships_edges2_tests.rs"]
mod fitting_containers_ships_edges2_tests;
